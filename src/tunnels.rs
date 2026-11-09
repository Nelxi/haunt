//! Port forwards over the tab's own SSH connection: `ssh -L`, `-R` and `-D`.
//!
//! The rules belong to the saved server and come up with every connection, including after a
//! reconnect. A SOCKS rule is a proxy like Ghostly's local one: the same port speaks SOCKS5,
//! SOCKS4a and HTTP, and the traffic leaves from the server.
//!
//! The SSH handle lives in the session task, so tunnel tasks ask it for channels through the
//! session's input queue; the session opens them concurrently and the terminal never waits.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use eframe::egui;
use parking_lot::Mutex;
use russh::client::Msg;
use russh::{Channel, ChannelStream};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

mod editor;
pub use editor::{Editor, Look};

use crate::config::{Tunnel, TunnelKind};
use crate::session::{Input, Shared};
use crate::tr;

/// Live counters of one rule.
#[derive(Default)]
pub struct Stats {
    pub conns: AtomicU32,
    pub up: AtomicU64,
    pub down: AtomicU64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Disabled, or not filled in yet.
    Off,
    /// Enabled; starts when the tab is connected.
    Waiting,
    Starting,
    Active,
    Failed(String),
}

#[derive(Clone)]
pub struct Live {
    pub state: State,
    pub stats: Arc<Stats>,
}

/// Where connections the server forwards to us (`-R`) go.
pub struct RemoteTarget {
    pub host: String,
    pub port: u16,
    pub stats: Arc<Stats>,
}

pub type OpenReply = oneshot::Sender<Result<Channel<Msg>, String>>;

fn set_state(shared: &Mutex<Shared>, ctx: &egui::Context, id: Uuid, state: State) {
    let mut sh = shared.lock();
    let live = sh.tunnels.entry(id).or_insert_with(|| Live { state: State::Off, stats: Arc::default() });
    if live.state != state {
        live.state = state;
        ctx.request_repaint();
    }
}

fn stats_of(shared: &Mutex<Shared>, id: Uuid) -> Arc<Stats> {
    shared.lock().tunnels.entry(id).or_insert_with(|| Live { state: State::Off, stats: Arc::default() }).stats.clone()
}

/// What a rule should show while the tab is offline.
pub fn idle_state(rule: &Tunnel) -> State {
    if rule.enabled && rule.is_valid() { State::Waiting } else { State::Off }
}

/// Marks every rule as waiting/off (used before the first connection and between reconnects).
pub fn mark_idle(shared: &Mutex<Shared>, ctx: &egui::Context, rules: &[Tunnel]) {
    for r in rules {
        set_state(shared, ctx, r.id, idle_state(r));
    }
    let ids: Vec<Uuid> = rules.iter().map(|r| r.id).collect();
    shared.lock().tunnels.retain(|id, _| ids.contains(id));
}

/// The running rules of one connection. Dropping it stops them all.
pub struct Runner {
    tx: mpsc::UnboundedSender<Input>,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    running: HashMap<Uuid, (Tunnel, JoinHandle<()>)>,
}

impl Runner {
    pub fn new(tx: mpsc::UnboundedSender<Input>, shared: Arc<Mutex<Shared>>, ctx: egui::Context) -> Self {
        Self { tx, shared, ctx, running: HashMap::new() }
    }

    /// Starts, stops and restarts rules so that exactly the enabled, complete ones run.
    /// Unchanged rules keep their open connections. Returns the `-R` ports to cancel on the
    /// server; the caller does that before anything else so a restarted rule can bind again.
    #[must_use]
    pub fn sync(&mut self, rules: &[Tunnel]) -> Vec<(String, u16)> {
        let wanted: HashMap<Uuid, &Tunnel> = rules.iter().filter(|r| r.enabled && r.is_valid()).map(|r| (r.id, r)).collect();
        let mut cancel = Vec::new();
        let shared = self.shared.clone();
        self.running.retain(|id, (rule, task)| {
            let keep = wanted.get(id).is_some_and(|w| *w == rule);
            if !keep {
                task.abort();
                if rule.kind == TunnelKind::Remote {
                    shared.lock().remote.remove(&u32::from(rule.port));
                    cancel.push((bind_addr(rule.lan).to_string(), rule.port));
                }
            }
            keep
        });
        for r in rules {
            if !wanted.contains_key(&r.id) {
                set_state(&self.shared, &self.ctx, r.id, State::Off);
            }
        }
        for (id, rule) in wanted {
            if self.running.contains_key(&id) {
                continue;
            }
            set_state(&self.shared, &self.ctx, id, State::Starting);
            let task = match rule.kind {
                TunnelKind::Local | TunnelKind::Socks => tokio::spawn(listen(rule.clone(), self.tx.clone(), self.shared.clone(), self.ctx.clone())),
                TunnelKind::Remote => {
                    let target = RemoteTarget { host: rule.to_host.trim().to_string(), port: rule.to_port, stats: stats_of(&self.shared, id) };
                    self.shared.lock().remote.insert(u32::from(rule.port), target);
                    tokio::spawn(remote(rule.clone(), self.tx.clone(), self.shared.clone(), self.ctx.clone()))
                }
            };
            self.running.insert(id, (rule.clone(), task));
        }
        let ids: Vec<Uuid> = rules.iter().map(|r| r.id).collect();
        self.shared.lock().tunnels.retain(|id, _| ids.contains(id));
        cancel
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        for (rule, task) in self.running.values() {
            task.abort();
            set_state(&self.shared, &self.ctx, rule.id, idle_state(rule));
        }
        self.shared.lock().remote.clear();
    }
}

fn bind_addr(lan: bool) -> &'static str {
    if lan { "0.0.0.0" } else { "127.0.0.1" }
}

fn bind_error(e: &std::io::Error, port: u16) -> String {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => tr!("port {port} is busy: another program or another haunt tab uses it", port = port),
        // Windows reserves port ranges (Hyper-V, WSL) and answers "access denied" for them.
        std::io::ErrorKind::PermissionDenied => tr!("Windows does not let programs use port {port} (it is reserved)", port = port),
        _ => format!("{port}: {e}"),
    }
}

/// Asks the session for a channel to `host:port`, as seen from the server.
async fn open_direct(tx: &mpsc::UnboundedSender<Input>, host: String, port: u16, from: SocketAddr) -> Result<Channel<Msg>, String> {
    let (reply, rx) = oneshot::channel();
    let gone = || tr!("not connected").to_string();
    tx.send(Input::OpenDirect { host, port, from_host: from.ip().to_string(), from_port: from.port(), reply }).map_err(|_| gone())?;
    rx.await.map_err(|_| gone())?
}

/// `-L` and `-D`: a listener on this PC.
async fn listen(rule: Tunnel, tx: mpsc::UnboundedSender<Input>, shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let listener = match TcpListener::bind((bind_addr(rule.lan), rule.port)).await {
        Ok(l) => l,
        Err(e) => {
            set_state(&shared, &ctx, rule.id, State::Failed(bind_error(&e, rule.port)));
            return;
        }
    };
    set_state(&shared, &ctx, rule.id, State::Active);
    let stats = stats_of(&shared, rule.id);
    // Owned here so that stopping the rule closes its connections too.
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((tcp, from)) = accepted else { continue };
                let _ = tcp.set_nodelay(true);
                let (tx, stats, ctx, rule) = (tx.clone(), stats.clone(), ctx.clone(), rule.clone());
                conns.spawn(async move {
                    stats.conns.fetch_add(1, Relaxed);
                    ctx.request_repaint();
                    match rule.kind {
                        TunnelKind::Socks => {
                            let _ = proxy(tcp, from, &tx, &stats).await;
                        }
                        _ => {
                            if let Ok(ch) = open_direct(&tx, rule.to_host.trim().to_string(), rule.to_port, from).await {
                                pump(tcp, ch.into_stream(), &stats).await;
                            }
                        }
                    }
                    stats.conns.fetch_sub(1, Relaxed);
                    ctx.request_repaint();
                });
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// `-R`: asks the server to listen; its connections arrive through the client handler
/// ([`accept_forwarded`]). The target was registered by [`Runner::sync`].
async fn remote(rule: Tunnel, tx: mpsc::UnboundedSender<Input>, shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let (reply, rx) = oneshot::channel();
    if tx.send(Input::RemoteForward { bind: bind_addr(rule.lan).to_string(), port: rule.port, reply }).is_err() {
        return;
    }
    let state = match rx.await {
        Ok(Ok(())) => State::Active,
        Ok(Err(e)) => State::Failed(tr!("the server refused to listen on port {port}: {e}", port = rule.port, e = e)),
        Err(_) => return,
    };
    set_state(&shared, &ctx, rule.id, state);
}

/// A connection the server forwarded to us for a `-R` rule.
pub fn accept_forwarded(shared: &Mutex<Shared>, ctx: &egui::Context, port: u32, channel: Channel<Msg>) -> bool {
    let target = shared.lock().remote.get(&port).map(|t| (t.host.clone(), t.port, t.stats.clone()));
    let Some((host, to_port, stats)) = target else { return false };
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let host = if host.is_empty() { "127.0.0.1".to_string() } else { host };
        let Ok(tcp) = TcpStream::connect((host.as_str(), to_port)).await else {
            let _ = channel.close().await;
            return;
        };
        let _ = tcp.set_nodelay(true);
        stats.conns.fetch_add(1, Relaxed);
        ctx.request_repaint();
        pump(tcp, channel.into_stream(), &stats).await;
        stats.conns.fetch_sub(1, Relaxed);
        ctx.request_repaint();
    });
    true
}

// ---------------------------------------------------------------- copying

async fn copy_counted<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(r: &mut R, w: &mut W, counter: &AtomicU64) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if w.write_all(&buf[..n]).await.is_err() {
            break;
        }
        counter.fetch_add(n as u64, Relaxed);
    }
    let _ = w.shutdown().await;
}

/// Copies both ways until both sides are done. `local` is the side on this PC, so "up" is
/// what goes to the server.
async fn pump<L, R>(local: L, remote: R, stats: &Stats)
where
    L: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    let (mut lr, mut lw) = tokio::io::split(local);
    let (mut rr, mut rw) = tokio::io::split(remote);
    tokio::join!(copy_counted(&mut lr, &mut rw, &stats.up), copy_counted(&mut rr, &mut lw, &stats.down));
}

// ---------------------------------------------------------------- the proxy (-D)

/// One client of a SOCKS rule: SOCKS5, SOCKS4/4a or an HTTP proxy request, told apart by the
/// first byte.
async fn proxy(mut tcp: TcpStream, from: SocketAddr, tx: &mpsc::UnboundedSender<Input>, stats: &Stats) -> std::io::Result<()> {
    let mut first = [0u8; 1];
    tcp.read_exact(&mut first).await?;
    match first[0] {
        5 => socks5(tcp, from, tx, stats).await,
        4 => socks4(tcp, from, tx, stats).await,
        b'A'..=b'Z' => http(tcp, first[0], from, tx, stats).await,
        _ => Ok(()),
    }
}

async fn socks5(mut tcp: TcpStream, from: SocketAddr, tx: &mpsc::UnboundedSender<Input>, stats: &Stats) -> std::io::Result<()> {
    let n = tcp.read_u8().await? as usize;
    let mut methods = vec![0u8; n];
    tcp.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        // We only speak "no authentication".
        tcp.write_all(&[5, 0xff]).await?;
        return Ok(());
    }
    tcp.write_all(&[5, 0]).await?;
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await?;
    let host = match head[3] {
        1 => {
            let mut a = [0u8; 4];
            tcp.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let len = tcp.read_u8().await? as usize;
            let mut d = vec![0u8; len];
            tcp.read_exact(&mut d).await?;
            String::from_utf8_lossy(&d).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            tcp.read_exact(&mut a).await?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => {
            tcp.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Ok(());
        }
    };
    let port = tcp.read_u16().await?;
    if head[1] != 1 {
        // Only CONNECT; no BIND or UDP.
        tcp.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    }
    match open_direct(tx, host, port, from).await {
        Ok(ch) => {
            tcp.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            pump(tcp, ch.into_stream(), stats).await;
        }
        Err(_) => tcp.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?,
    }
    Ok(())
}

async fn read_cstr(tcp: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let b = tcp.read_u8().await?;
        if b == 0 || out.len() > 255 {
            return Ok(out);
        }
        out.push(b);
    }
}

async fn socks4(mut tcp: TcpStream, from: SocketAddr, tx: &mpsc::UnboundedSender<Input>, stats: &Stats) -> std::io::Result<()> {
    let cmd = tcp.read_u8().await?;
    let port = tcp.read_u16().await?;
    let mut ip = [0u8; 4];
    tcp.read_exact(&mut ip).await?;
    read_cstr(&mut tcp).await?; // user id
    // SOCKS4a: 0.0.0.x means "the host name follows".
    let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
        String::from_utf8_lossy(&read_cstr(&mut tcp).await?).into_owned()
    } else {
        std::net::Ipv4Addr::from(ip).to_string()
    };
    if cmd != 1 {
        tcp.write_all(&[0, 0x5b, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    }
    match open_direct(tx, host, port, from).await {
        Ok(ch) => {
            tcp.write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0]).await?;
            pump(tcp, ch.into_stream(), stats).await;
        }
        Err(_) => tcp.write_all(&[0, 0x5b, 0, 0, 0, 0, 0, 0]).await?,
    }
    Ok(())
}

/// "host:port" or "[v6]:port" with a default port.
fn split_host_port(s: &str, default: u16) -> Option<(String, u16)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default,
        };
        return Some((h.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((s.to_string(), default)),
    }
}

/// An HTTP proxy request: CONNECT for HTTPS, or a plain request with an absolute URL.
async fn http(mut tcp: TcpStream, first: u8, from: SocketAddr, tx: &mpsc::UnboundedSender<Input>, stats: &Stats) -> std::io::Result<()> {
    let mut buf = vec![first];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 32 * 1024 {
            tcp.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n").await?;
            return Ok(());
        }
        let mut chunk = [0u8; 4096];
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let rest = buf[end..].to_vec();
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split(' ');
    let (method, target, version) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or("HTTP/1.1"));
    let bad = async |tcp: &mut TcpStream| tcp.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n").await;

    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_host_port(target, 443) else { return bad(&mut tcp).await };
        return match open_direct(tx, host, port, from).await {
            Ok(ch) => {
                tcp.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
                let mut stream = ch.into_stream();
                if !rest.is_empty() {
                    stream.write_all(&rest).await?;
                }
                pump(tcp, stream, stats).await;
                Ok(())
            }
            Err(_) => tcp.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n").await,
        };
    }

    // "GET http://host:port/path HTTP/1.1" → "GET /path HTTP/1.1" to host:port.
    let Some(after) = target.strip_prefix("http://") else { return bad(&mut tcp).await };
    let (authority, path) = match after.find('/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, "/"),
    };
    let Some((host, port)) = split_host_port(authority, 80) else { return bad(&mut tcp).await };
    let mut out = format!("{method} {path} {version}\r\n");
    for l in lines.filter(|l| !l.is_empty()) {
        let name = l.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        // One request per connection: the next one may be for another host.
        if name.starts_with("proxy-") || name == "connection" || name == "keep-alive" {
            continue;
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    match open_direct(tx, host, port, from).await {
        Ok(ch) => {
            let mut stream: ChannelStream<Msg> = ch.into_stream();
            stream.write_all(out.as_bytes()).await?;
            stream.write_all(&rest).await?;
            pump(tcp, stream, stats).await;
            Ok(())
        }
        Err(_) => tcp.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n").await,
    }
}

/// This PC's address in the local network (for showing a LAN-shared tunnel). No packet is sent:
/// connecting a UDP socket only picks the outgoing interface.
pub fn lan_ip() -> Option<String> {
    use std::sync::OnceLock;
    static IP: OnceLock<Option<String>> = OnceLock::new();
    IP.get_or_init(|| {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("192.0.2.1:9").ok()?;
        Some(s.local_addr().ok()?.ip().to_string())
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_forms() {
        assert_eq!(split_host_port("example.com:8080", 80), Some(("example.com".into(), 8080)));
        assert_eq!(split_host_port("example.com", 443), Some(("example.com".into(), 443)));
        assert_eq!(split_host_port("[::1]:22", 80), Some(("::1".into(), 22)));
        assert_eq!(split_host_port("[::1]", 80), Some(("::1".into(), 80)));
        assert_eq!(split_host_port("host:x", 80), None);
    }

    /// Real forwards through a real server (the first one in the config), all checked from
    /// this PC. Run with `HAUNT_CONFIG_DIR=<dir> cargo test live_forwards -- --ignored`.
    ///
    /// -L and -D reach the server's own sshd and must read its banner. The -R rule is
    /// reached through a second -L to the server's loopback, so the bytes go
    /// PC → server → PC → local echo listener and back.
    #[test]
    #[ignore]
    fn live_forwards() {
        use std::time::{Duration, Instant};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let rt = tokio::runtime::Runtime::new().unwrap();
        let ctx = egui::Context::default();
        let (cfg, err) = crate::config::Config::load();
        assert!(err.is_none(), "{err:?}");
        let server = cfg.servers.first().expect("a server in the config").clone();
        let ssh_port = server.port;

        // The local service the -R rule leads to.
        let echo = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        rt.spawn(async move {
            while let Ok((mut c, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = c.split();
                    let _ = w.write_all(b"echo:").await;
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });

        let rule = |kind, port, to_host: &str, to_port| Tunnel { port, to_host: to_host.into(), to_port, ..Tunnel::new(kind) };
        let rules = vec![
            rule(TunnelKind::Local, 47101, "127.0.0.1", ssh_port),
            rule(TunnelKind::Socks, 47102, "", 0),
            rule(TunnelKind::Remote, 47103, "127.0.0.1", echo_port),
            rule(TunnelKind::Local, 47104, "127.0.0.1", 47103),
        ];
        let mut spec = crate::session::ConnectSpec::from_server(&server, &cfg.servers, 15);
        spec.tunnels = rules.clone();
        let mut s = crate::session::Session::start(1, "live".into(), spec, Default::default(), rt.handle(), &ctx);

        let deadline = Instant::now() + Duration::from_secs(25);
        while !rules.iter().all(|r| s.tunnel_live(r.id).is_some_and(|l| l.state == State::Active)) {
            let states: Vec<_> = rules.iter().map(|r| s.tunnel_live(r.id).map(|l| l.state)).collect();
            assert!(Instant::now() < deadline, "not active: {states:?}, status {:?}", s.status());
            assert!(!states.iter().any(|st| matches!(st, Some(State::Failed(_)))), "{states:?}");
            std::thread::sleep(Duration::from_millis(100));
        }

        rt.block_on(async {
            let banner = async |mut c: TcpStream| {
                let mut b = [0u8; 8];
                tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut b)).await.expect("banner timeout").unwrap();
                String::from_utf8_lossy(&b).into_owned()
            };
            // -L
            let c = TcpStream::connect("127.0.0.1:47101").await.unwrap();
            assert_eq!(banner(c).await, "SSH-2.0-");

            // -D, SOCKS5 with a host name.
            let mut c = TcpStream::connect("127.0.0.1:47102").await.unwrap();
            c.write_all(&[5, 1, 0]).await.unwrap();
            let mut r = [0u8; 2];
            c.read_exact(&mut r).await.unwrap();
            assert_eq!(r, [5, 0]);
            let mut req = vec![5, 1, 0, 3, 9];
            req.extend_from_slice(b"localhost");
            req.extend_from_slice(&ssh_port.to_be_bytes());
            c.write_all(&req).await.unwrap();
            let mut r = [0u8; 10];
            c.read_exact(&mut r).await.unwrap();
            assert_eq!(r[1], 0, "SOCKS5 reply {r:?}");
            assert_eq!(banner(c).await, "SSH-2.0-");

            // -D, SOCKS4a.
            let mut c = TcpStream::connect("127.0.0.1:47102").await.unwrap();
            let mut req = vec![4, 1];
            req.extend_from_slice(&ssh_port.to_be_bytes());
            req.extend_from_slice(&[0, 0, 0, 1, 0]);
            req.extend_from_slice(b"127.0.0.1\0");
            c.write_all(&req).await.unwrap();
            let mut r = [0u8; 8];
            c.read_exact(&mut r).await.unwrap();
            assert_eq!(r[1], 0x5a, "SOCKS4 reply {r:?}");
            assert_eq!(banner(c).await, "SSH-2.0-");

            // -D, HTTP CONNECT.
            let mut c = TcpStream::connect("127.0.0.1:47102").await.unwrap();
            c.write_all(format!("CONNECT 127.0.0.1:{ssh_port} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(c.read_u8().await.unwrap());
            }
            assert!(head.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&head));
            assert_eq!(banner(c).await, "SSH-2.0-");

            // -D, a target that refuses: a clean SOCKS error, not a hang.
            let mut c = TcpStream::connect("127.0.0.1:47102").await.unwrap();
            c.write_all(&[5, 1, 0]).await.unwrap();
            let mut r = [0u8; 2];
            c.read_exact(&mut r).await.unwrap();
            c.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0, 1]).await.unwrap();
            let mut r = [0u8; 10];
            tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut r)).await.unwrap().unwrap();
            assert_eq!(r[1], 5, "SOCKS5 reply {r:?}");

            // -R, through the server and back.
            let mut c = TcpStream::connect("127.0.0.1:47104").await.unwrap();
            c.write_all(b"ping").await.unwrap();
            let mut b = [0u8; 9];
            tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut b)).await.expect("-R timeout").unwrap();
            assert_eq!(&b, b"echo:ping");
        });

        let st = s.tunnel_live(rules[0].id).unwrap().stats;
        assert!(st.down.load(Relaxed) >= 8, "counted {}", st.down.load(Relaxed));

        // Moving a rule restarts it on the new port; the old one stops listening.
        let mut moved = rules.clone();
        moved[0].port = 47105;
        s.set_tunnels(moved.clone(), &ctx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while rt.block_on(TcpStream::connect("127.0.0.1:47101")).is_ok() || rt.block_on(TcpStream::connect("127.0.0.1:47105")).is_err() {
            assert!(Instant::now() < deadline, "rule did not move");
            std::thread::sleep(Duration::from_millis(100));
        }
        // Editing the -R rule restarts it on the same server port in one go: the old forward
        // has to be cancelled before the new one asks for that port.
        moved[2].to_host = "localhost".into();
        s.set_tunnels(moved.clone(), &ctx);
        std::thread::sleep(Duration::from_millis(300));
        let deadline = Instant::now() + Duration::from_secs(10);
        while s.tunnel_live(moved[2].id).is_none_or(|l| l.state != State::Active) {
            let st = s.tunnel_live(moved[2].id).map(|l| l.state);
            assert!(Instant::now() < deadline && !matches!(st, Some(State::Failed(_))), "-R restart: {st:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
        rt.block_on(async {
            let mut c = TcpStream::connect("127.0.0.1:47104").await.unwrap();
            c.write_all(b"pong").await.unwrap();
            let mut b = [0u8; 9];
            tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut b)).await.expect("-R after restart").unwrap();
            assert_eq!(&b, b"echo:pong");
        });
        drop(s);
        rt.shutdown_timeout(Duration::from_secs(2));
    }
}
