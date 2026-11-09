//! One SSH session: the async network task plus the state the UI reads.
//!
//! Threading model: the terminal grid (`Term`) sits behind a mutex. The tokio task parses
//! incoming bytes into it and asks egui for a repaint; the UI thread locks it only while
//! drawing a frame. Keystrokes go the other way through an unbounded channel.
//!
//! Lock order: the task may lock `shared` while holding `term` (terminal events such as a
//! title change go through the listener). The UI must therefore never lock `term` while
//! holding `shared`.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use anyhow::{Context as _, Result, anyhow, bail};
use eframe::egui;
use parking_lot::Mutex;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, ssh_key};
use russh::{ChannelMsg, Disconnect, client};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::tr;

use crate::config::{self, Auth, Proxy, Server};
use crate::secrets;

/// Grid size handed to alacritty_terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

pub enum Input {
    Data(Vec<u8>),
    Resize(GridSize),
    /// Skip the wait before the next reconnect attempt.
    RetryNow,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
    /// The connection dropped; the next attempt starts at this moment.
    Reconnecting(std::time::Instant),
    Closed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// Unknown host key: the answer is "accept" or nothing.
    HostKey,
    Password,
    Passphrase,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    pub message: String,
    /// `Some((text, remember))` to continue, `None` (or dropping the sender) to cancel.
    pub reply: oneshot::Sender<Option<(String, bool)>>,
}

pub struct Shared {
    pub status: Status,
    pub prompt: Option<Prompt>,
    /// Title set by the remote shell through OSC 0/2.
    pub title: Option<String>,
    pub size: GridSize,
    /// Sender into the currently running task; replaced on reconnect.
    tx: mpsc::UnboundedSender<Input>,
}

/// Receives events from the terminal parser (runs on the network task).
#[derive(Clone)]
pub struct Listener {
    shared: Arc<Mutex<Shared>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        match event {
            // Replies to terminal queries (cursor position, device attributes, ...).
            TermEvent::PtyWrite(s) => {
                let _ = self.shared.lock().tx.send(Input::Data(s.into_bytes()));
            }
            TermEvent::Title(t) => self.shared.lock().title = Some(t),
            TermEvent::ResetTitle => self.shared.lock().title = None,
            // Clipboard writes from the remote side (OSC 52) are ignored on purpose:
            // a server should not be able to silently replace the local clipboard.
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectSpec {
    pub server_id: Option<Uuid>,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    pub keepalive: Duration,
    /// What sits between us and the host (resolved from the server's [`Proxy`]).
    pub via: Option<Box<Via>>,
    /// Reconnect by itself when an established connection drops.
    pub reconnect: bool,
}

#[derive(Clone, Debug)]
pub enum Via {
    Socks5 { host: String, port: u16, user: String },
    Http { host: String, port: u16, user: String },
    /// SSH into this server first, then open a direct-tcpip channel to the host.
    Jump(ConnectSpec),
    /// The proxy cannot be used (deleted jump server, loop); the message says why.
    Broken(String),
}

impl ConnectSpec {
    /// `all` is the saved server list, needed to resolve jump hosts (which may chain).
    pub fn from_server(s: &Server, all: &[Server], keepalive_secs: u64) -> Self {
        Self::resolve(s, all, keepalive_secs, &mut Vec::new())
    }

    fn resolve(s: &Server, all: &[Server], keepalive_secs: u64, seen: &mut Vec<Uuid>) -> Self {
        seen.push(s.id);
        let via = match &s.proxy {
            Proxy::None => None,
            Proxy::Socks5 { host, port, user } => {
                Some(Via::Socks5 { host: host.trim().into(), port: *port, user: user.trim().into() })
            }
            Proxy::Http { host, port, user } => {
                Some(Via::Http { host: host.trim().into(), port: *port, user: user.trim().into() })
            }
            Proxy::Jump { server } => Some(if seen.contains(server) {
                Via::Broken(tr!("jump host loop at {name}", name = s.display_name()))
            } else if let Some(j) = all.iter().find(|x| x.id == *server) {
                Via::Jump(Self::resolve(j, all, keepalive_secs, seen))
            } else {
                Via::Broken(tr!("the jump server of {name} was deleted", name = s.display_name()))
            }),
        };
        Self {
            server_id: Some(s.id),
            host: s.host.trim().to_string(),
            port: s.port,
            user: s.user.trim().to_string(),
            auth: s.auth.clone(),
            keepalive: Duration::from_secs(keepalive_secs.max(5)),
            via: via.map(Box::new),
            reconnect: false,
        }
    }

    /// `user@host:port`, preceded by the route when there is one.
    pub fn route(&self) -> String {
        let me = format!("{}@{}:{}", self.user, self.host, self.port);
        match self.via.as_deref() {
            None | Some(Via::Broken(_)) => me,
            Some(Via::Socks5 { host, port, .. }) => format!("socks5 {host}:{port} → {me}"),
            Some(Via::Http { host, port, .. }) => format!("http {host}:{port} → {me}"),
            Some(Via::Jump(j)) => format!("{} → {me}", j.route()),
        }
    }
}

pub struct Session {
    pub id: u64,
    pub label: String,
    pub spec: ConnectSpec,
    pub term: Arc<Mutex<Term<Listener>>>,
    pub shared: Arc<Mutex<Shared>>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Set when the tab becomes active so the terminal grabs keyboard focus.
    pub want_focus: bool,
    pub view: crate::termview::ViewState,
}

impl Session {
    pub fn start(
        id: u64,
        label: String,
        spec: ConnectSpec,
        term_config: TermConfig,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let size = GridSize { cols: 100, rows: 30 };
        let shared = Arc::new(Mutex::new(Shared {
            status: Status::Connecting,
            prompt: None,
            title: None,
            size,
            tx,
        }));
        let term = Term::new(term_config, &size, Listener { shared: shared.clone() });
        let mut s = Self {
            id,
            label,
            spec,
            term: Arc::new(Mutex::new(term)),
            shared,
            task: None,
            want_focus: true,
            view: Default::default(),
        };
        s.spawn(rx, rt, ctx);
        s
    }

    fn spawn(&mut self, rx: mpsc::UnboundedReceiver<Input>, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
        let task = Task {
            spec: self.spec.clone(),
            out: Output { parser: Processor::new(), term: self.term.clone(), ctx: ctx.clone() },
            shared: self.shared.clone(),
            ctx: ctx.clone(),
            secrets: HashMap::new(),
            authenticating: false,
        };
        self.task = Some(rt.spawn(task.run(rx)));
    }

    /// Reconnect in place: same tab, same scrollback.
    pub fn reconnect(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
        self.stop();
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut sh = self.shared.lock();
            sh.tx = tx;
            sh.status = Status::Connecting;
            sh.prompt = None;
        }
        self.want_focus = true;
        self.spawn(rx, rt, ctx);
    }

    pub fn send(&self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        let sh = self.shared.lock();
        if sh.status == Status::Connected {
            let _ = sh.tx.send(Input::Data(bytes));
        }
    }

    pub fn status(&self) -> Status {
        self.shared.lock().status.clone()
    }

    /// While waiting to reconnect: try right away instead.
    pub fn retry_now(&self) {
        let _ = self.shared.lock().tx.send(Input::RetryNow);
    }

    /// Resize the local grid and tell the server. Cheap no-op if the size is unchanged.
    pub fn resize(&self, size: GridSize) {
        {
            let mut sh = self.shared.lock();
            if sh.size == size {
                return;
            }
            sh.size = size;
            let _ = sh.tx.send(Input::Resize(size));
        }
        self.term.lock().resize(size);
    }

    fn stop(&mut self) {
        let _ = self.shared.lock().tx.send(Input::Close);
        if let Some(task) = self.task.take() {
            // Give a connected session the chance to send a clean disconnect; anything
            // still connecting or waiting on a prompt is simply aborted.
            let graceful = self.shared.lock().status == Status::Connected;
            if !graceful {
                task.abort();
            }
        }
        let mut sh = self.shared.lock();
        sh.prompt = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Feeds bytes into the terminal and schedules a repaint.
struct Output {
    parser: Processor<StdSyncHandler>,
    term: Arc<Mutex<Term<Listener>>>,
    ctx: egui::Context,
}

impl Output {
    fn feed(&mut self, bytes: &[u8]) {
        {
            let mut term = self.term.lock();
            self.parser.advance(&mut *term, bytes);
        }
        self.ctx.request_repaint();
    }

    fn info(&mut self, text: &str) {
        self.feed(format!("\x1b[2m{text}\x1b[0m\r\n").as_bytes());
    }

    fn error(&mut self, text: &str) {
        let text = text.replace('\n', "\r\n");
        self.feed(format!("\r\n\x1b[1;31m✖ {text}\x1b[0m\r\n").as_bytes());
    }
}

struct Task {
    spec: ConnectSpec,
    out: Output,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    /// Passwords and passphrases that worked in this tab, so an automatic reconnect does
    /// not ask again. Lives only as long as the tab.
    secrets: HashMap<String, String>,
    /// Set once the transport is up and login starts: failures after this point are
    /// about credentials, and retrying them blindly would only lock the account.
    authenticating: bool,
}

/// How a session that got connected came to an end.
enum End {
    /// The tab was closed.
    Closed,
    /// The remote shell ended (exit, logout, the server closed the channel).
    Exited(String),
    /// The connection itself died: network, keepalive timeout, reset.
    Lost(String),
}

/// An error that must not be retried automatically (host key problems).
#[derive(Debug)]
struct Fatal(String);

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

fn is_fatal(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Fatal>())
}

/// Seconds to wait before each reconnect attempt; the last one repeats.
const BACKOFF: [u64; 7] = [1, 2, 4, 8, 15, 30, 60];

/// A human reason for a dropped connection, from the error the SSH session ended with.
fn lost_reason(e: &anyhow::Error, keepalive: Duration) -> String {
    match e.downcast_ref::<russh::Error>() {
        Some(russh::Error::KeepaliveTimeout) => {
            tr!("connection lost: the server stopped answering ({n} s without a reply)", n = keepalive.as_secs() * 3)
        }
        Some(russh::Error::HUP) | Some(russh::Error::Disconnect) => tr!("connection lost: the server closed it").into(),
        Some(russh::Error::IO(io))
            if matches!(
                io.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            tr!("connection lost: the network connection was cut").into()
        }
        Some(russh::Error::IO(io)) => tr!("connection lost: {e}", e = io),
        _ => tr!("connection lost: {e}", e = format!("{e:#}")),
    }
}

impl Task {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Input>) {
        let mut was_connected = false;
        let mut failures = 0usize;
        loop {
            let route = self.spec.route();
            self.out.info(&if was_connected { tr!("reconnecting to {route} …", route = route) } else { tr!("connecting to {route} …", route = route) });
            self.authenticating = false;
            let (reason, retry) = match self.session(&mut rx, &mut was_connected).await {
                Ok(End::Closed) => {
                    self.finish(tr!("session closed").into());
                    return;
                }
                Ok(End::Exited(reason)) => {
                    self.out.info(&format!("\r\n{reason}"));
                    (reason, false)
                }
                Ok(End::Lost(reason)) => {
                    self.out.error(&reason);
                    failures = 0;
                    (reason, self.spec.reconnect)
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    self.out.error(&msg);
                    // Only keep trying for a session that worked before, and only while the
                    // problem is getting there (network, proxy), not logging in.
                    let retry = was_connected && self.spec.reconnect && !self.authenticating && !is_fatal(&e);
                    (msg, retry)
                }
            };
            if !retry || !self.wait_to_reconnect(&mut rx, &mut failures).await {
                self.finish(reason);
                return;
            }
        }
    }

    fn finish(&mut self, reason: String) {
        let mut sh = self.shared.lock();
        sh.status = Status::Closed(reason);
        sh.prompt = None;
        drop(sh);
        self.ctx.request_repaint();
    }

    /// Waits out the backoff. False when the tab was closed meanwhile.
    async fn wait_to_reconnect(&mut self, rx: &mut mpsc::UnboundedReceiver<Input>, failures: &mut usize) -> bool {
        let secs = BACKOFF[(*failures).min(BACKOFF.len() - 1)];
        *failures += 1;
        let at = std::time::Instant::now() + Duration::from_secs(secs);
        {
            let mut sh = self.shared.lock();
            sh.status = Status::Reconnecting(at);
            sh.prompt = None;
        }
        self.out.info(&tr!("reconnecting in {n} s  ·  Enter to retry now", n = secs));
        let deadline = tokio::time::Instant::from_std(at);
        loop {
            // Tick once a second so the countdown on screen stays current.
            self.ctx.request_repaint();
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                input = rx.recv() => match input {
                    Some(Input::RetryNow) => break,
                    Some(Input::Close) | None => return false,
                    // Typing goes nowhere while offline; the size is re-sent on connect.
                    Some(Input::Data(_)) | Some(Input::Resize(_)) => {}
                },
            }
        }
        self.shared.lock().status = Status::Connecting;
        self.ctx.request_repaint();
        true
    }

    async fn session(&mut self, rx: &mut mpsc::UnboundedReceiver<Input>, was_connected: &mut bool) -> Result<End> {
        let spec = self.spec.clone();
        if spec.host.is_empty() {
            bail!("{}", tr!("no host set"));
        }

        // Jump hosts stay alive as long as the session: its traffic runs through them.
        let mut hops = Vec::new();
        let mut handle = self.open(&spec, &mut hops).await?;

        let channel = handle.channel_open_session().await.context(tr!("cannot open a session channel"))?;
        let size = self.shared.lock().size;
        channel
            .request_pty(false, "xterm-256color", size.cols as u32, size.rows as u32, 0, 0, &[])
            .await
            .context(tr!("PTY request failed"))?;
        // Most servers drop env requests (AcceptEnv); harmless when they do.
        let _ = channel.set_env(false, "COLORTERM", "truecolor").await;
        channel.request_shell(false).await.context(tr!("shell request failed"))?;

        // The grid may have been resized while we were offline.
        let now = self.shared.lock().size;
        if now != size {
            let _ = channel.window_change(now.cols as u32, now.rows as u32, 0, 0).await;
        }
        self.shared.lock().status = Status::Connected;
        *was_connected = true;
        self.ctx.request_repaint();

        let mut channel = channel;
        let mut exit_status: Option<u32> = None;
        // True when the server ended the channel (the shell exited); false when the whole
        // connection vanished under us.
        let mut ended_by_server = false;
        loop {
            tokio::select! {
                input = rx.recv() => match input {
                    Some(Input::Data(bytes)) => channel.data(&bytes[..]).await?,
                    Some(Input::Resize(s)) => {
                        let _ = channel.window_change(s.cols as u32, s.rows as u32, 0, 0).await;
                    }
                    Some(Input::RetryNow) => {}
                    Some(Input::Close) | None => {
                        let _ = channel.eof().await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                        close_hops(hops).await;
                        return Ok(End::Closed);
                    }
                },
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => self.out.feed(&data),
                    Some(ChannelMsg::ExtendedData { data, .. }) => self.out.feed(&data),
                    Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                    Some(ChannelMsg::Close) => {
                        ended_by_server = true;
                        break;
                    }
                    None => break,
                    _ => {}
                },
            }
        }

        if ended_by_server || exit_status.is_some() {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
            close_hops(hops).await;
            return Ok(End::Exited(match exit_status {
                Some(0) | None => tr!("session ended").into(),
                Some(code) => tr!("session ended (exit status {code})", code = code),
            }));
        }
        // The channel vanished without a goodbye: the SSH session itself is over. Its
        // result says why (keepalive timeout, reset, ...).
        let reason = match tokio::time::timeout(Duration::from_secs(2), &mut handle).await {
            Ok(Err(e)) => lost_reason(&e, spec.keepalive),
            _ => tr!("connection lost").into(),
        };
        close_hops(hops).await;
        Ok(End::Lost(reason))
    }

    /// Reaches `spec`'s host (through its proxy chain), does the handshake and logs in.
    /// Handles of the jump hosts on the way are pushed to `hops`.
    fn open<'a>(
        &'a mut self,
        spec: &'a ConnectSpec,
        hops: &'a mut Vec<client::Handle<HostKeyCheck>>,
    ) -> Pin<Box<dyn Future<Output = Result<client::Handle<HostKeyCheck>>> + Send + 'a>> {
        Box::pin(async move {
            if spec.host.is_empty() {
                bail!("{}", tr!("no host set"));
            }
            let stream: Box<dyn Io> = match spec.via.as_deref() {
                None => Box::new(tcp(&spec.host, spec.port).await?),
                Some(Via::Broken(why)) => bail!("{why}"),
                Some(Via::Socks5 { host, port, user }) => {
                    self.out.info(&tr!("via SOCKS5 proxy {addr}", addr = format!("{host}:{port}")));
                    let pass = self.proxy_password(spec, user, host, *port).await?;
                    let mut s = tcp(host, *port).await?;
                    socks5(&mut s, &spec.host, spec.port, user, pass.as_ref().map(|p| p.0.as_str()))
                        .await
                        .with_context(|| tr!("SOCKS5 proxy {addr}", addr = format!("{host}:{port}")))?;
                    self.remember_proxy(spec, user, host, *port, pass);
                    Box::new(s)
                }
                Some(Via::Http { host, port, user }) => {
                    self.out.info(&tr!("via HTTP proxy {addr}", addr = format!("{host}:{port}")));
                    let pass = self.proxy_password(spec, user, host, *port).await?;
                    let mut s = tcp(host, *port).await?;
                    http_connect(&mut s, &spec.host, spec.port, user, pass.as_ref().map(|p| p.0.as_str()))
                        .await
                        .with_context(|| tr!("HTTP proxy {addr}", addr = format!("{host}:{port}")))?;
                    self.remember_proxy(spec, user, host, *port, pass);
                    Box::new(s)
                }
                Some(Via::Jump(j)) => {
                    let jh = self.open(j, hops).await?;
                    self.out.info(&tr!("through {jump} → {target}", jump = j.host, target = format!("{}:{}", spec.host, spec.port)));
                    let ch = jh
                        .channel_open_direct_tcpip(spec.host.clone(), spec.port as u32, "127.0.0.1", 0)
                        .await
                        .with_context(|| tr!("{jump} refused to forward to {target}", jump = j.host, target = format!("{}:{}", spec.host, spec.port)))?;
                    hops.push(jh);
                    Box::new(ch.into_stream())
                }
            };

            let config = Arc::new(client::Config {
                keepalive_interval: Some(spec.keepalive),
                keepalive_max: 3,
                inactivity_timeout: None,
                nodelay: true,
                ..Default::default()
            });
            let handler = HostKeyCheck {
                host: spec.host.clone(),
                port: spec.port,
                shared: self.shared.clone(),
                ctx: self.ctx.clone(),
            };
            let mut handle = client::connect_stream(config, stream, handler)
                .await
                .with_context(|| tr!("SSH handshake with {host} failed", host = spec.host))?;
            self.authenticating = true;
            self.authenticate(&mut handle, spec).await?;
            self.authenticating = false;
            Ok(handle)
        })
    }

    /// The proxy password: none without a user, else the saved one, else asked for.
    async fn proxy_password(&self, spec: &ConnectSpec, user: &str, host: &str, port: u16) -> Result<Option<(String, bool)>> {
        if user.is_empty() {
            return Ok(None);
        }
        if let Some(p) = self.secrets.get(&format!("proxy {user}@{host}:{port}")) {
            return Ok(Some((p.clone(), false)));
        }
        if let Some(p) = spec.server_id.and_then(secrets::get_proxy) {
            return Ok(Some((p, false)));
        }
        let msg = tr!("Proxy password for {target}", target = format!("{user}@{host}:{port}"));
        let r = self
            .ask(PromptKind::Password, tr!("Proxy password"), &msg)
            .await
            .ok_or_else(|| anyhow!("{}", tr!("authentication cancelled")))?;
        Ok(Some(r))
    }

    fn remember_proxy(&mut self, spec: &ConnectSpec, user: &str, host: &str, port: u16, pass: Option<(String, bool)>) {
        if let Some((p, _)) = &pass {
            self.secrets.insert(format!("proxy {user}@{host}:{port}"), p.clone());
        }
        if let (Some((p, true)), Some(id)) = (pass, spec.server_id)
            && let Err(e) = secrets::set_proxy(id, &p) {
                self.out.error(&tr!("could not save the proxy password: {e}", e = e));
            }
    }

    async fn authenticate(&mut self, handle: &mut client::Handle<HostKeyCheck>, spec: &ConnectSpec) -> Result<()> {
        let spec = spec.clone();
        match &spec.auth {
            Auth::Key { path } => {
                let key = self.load_key(path, spec.server_id).await?;
                let hash = handle.best_supported_rsa_hash().await?.flatten();
                let res = handle
                    .authenticate_publickey(&spec.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await?;
                if !res.success() {
                    bail!("{}", tr!("the server rejected key {path} for user {user}", path = path.display(), user = spec.user));
                }
                Ok(())
            }
            Auth::Password => {
                let cache_key = format!("password {}@{}:{}", spec.user, spec.host, spec.port);
                let mut saved = self.secrets.get(&cache_key).cloned().or_else(|| spec.server_id.and_then(secrets::get));
                for attempt in 0..3 {
                    let from_store = saved.is_some();
                    let (password, remember) = match saved.take() {
                        Some(p) => (p, false),
                        None => {
                            let msg = if attempt == 0 {
                                tr!("Password for {target}", target = format!("{}@{}", spec.user, spec.host))
                            } else {
                                tr!("Wrong password, try again").to_string()
                            };
                            self.ask(PromptKind::Password, tr!("Password"), &msg)
                                .await
                                .ok_or_else(|| anyhow!("{}", tr!("authentication cancelled")))?
                        }
                    };
                    let res = handle.authenticate_password(&spec.user, &password).await?;
                    if res.success() {
                        self.secrets.insert(cache_key, password.clone());
                        if remember
                            && let Some(id) = spec.server_id
                                && let Err(e) = secrets::set(id, &password) {
                                    self.out.error(&tr!("could not save the password: {e}", e = e));
                                }
                        return Ok(());
                    }
                    if from_store {
                        self.out.info(tr!("saved password was rejected"));
                    }
                }
                bail!("{}", tr!("authentication failed"))
            }
        }
    }

    /// Loads a private key, asking for the passphrase if the key needs one.
    async fn load_key(&mut self, path: &PathBuf, server_id: Option<Uuid>) -> Result<ssh_key::PrivateKey> {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| tr!("cannot read key file {path}", path = path.display()))?;
        let first_err = match russh::keys::decode_secret_key(&text, None) {
            Ok(k) => return Ok(k),
            Err(e) => e,
        };
        // Encrypted key (or unreadable). Try the passphrase from earlier in this tab, the
        // saved one, then ask.
        let cache_key = format!("passphrase {}", path.display());
        let mut saved = self.secrets.get(&cache_key).cloned().or_else(|| server_id.and_then(secrets::get));
        for attempt in 0..3 {
            let (pass, remember) = match saved.take() {
                Some(p) => (p, false),
                None => {
                    let msg = if attempt == 0 {
                        tr!("Passphrase for {path}\n({e})", path = path.display(), e = first_err)
                    } else {
                        tr!("Wrong passphrase, try again").to_string()
                    };
                    match self.ask(PromptKind::Passphrase, tr!("Key passphrase"), &msg).await {
                        Some(r) => r,
                        None => bail!("{}", tr!("cannot load key {path}: {e}", path = path.display(), e = first_err)),
                    }
                }
            };
            if let Ok(k) = russh::keys::decode_secret_key(&text, Some(&pass)) {
                self.secrets.insert(cache_key, pass.clone());
                if remember
                    && let Some(id) = server_id {
                        let _ = secrets::set(id, &pass);
                    }
                return Ok(k);
            }
        }
        bail!("{}", tr!("cannot decrypt key {path}", path = path.display()))
    }

    async fn ask(&self, kind: PromptKind, title: &str, message: &str) -> Option<(String, bool)> {
        ask(&self.shared, &self.ctx, kind, title, message).await
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}

async fn close_hops(hops: Vec<client::Handle<HostKeyCheck>>) {
    for h in hops.into_iter().rev() {
        let _ = h.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}

async fn tcp(host: &str, port: u16) -> Result<TcpStream> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host, port)))
        .await
        .map_err(|_| anyhow!("{}", tr!("connection to {addr} timed out after 10 s", addr = format!("{host}:{port}"))))?
        .with_context(|| tr!("cannot reach {addr}", addr = format!("{host}:{port}")))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// SOCKS5 CONNECT (RFC 1928), with username/password auth (RFC 1929) when a user is set.
/// The host name goes to the proxy as is, so the proxy resolves it (no local DNS leak).
async fn socks5<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    host: &str,
    port: u16,
    user: &str,
    pass: Option<&str>,
) -> Result<()> {
    let with_auth = !user.is_empty();
    s.write_all(if with_auth { &[5, 2, 0, 2][..] } else { &[5, 1, 0][..] }).await?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.context(tr!("no reply"))?;
    if r[0] != 5 {
        bail!("{}", tr!("not a SOCKS5 proxy"));
    }
    match r[1] {
        0 => {}
        2 if with_auth => {
            let pass = pass.unwrap_or("");
            if user.len() > 255 || pass.len() > 255 {
                bail!("{}", tr!("user or password longer than 255 bytes"));
            }
            let mut m = vec![1, user.len() as u8];
            m.extend_from_slice(user.as_bytes());
            m.push(pass.len() as u8);
            m.extend_from_slice(pass.as_bytes());
            s.write_all(&m).await?;
            s.read_exact(&mut r).await?;
            if r[1] != 0 {
                bail!("{}", tr!("wrong proxy user or password"));
            }
        }
        0xff if !with_auth => bail!("{}", tr!("the proxy wants a user and password")),
        _ => bail!("{}", tr!("the proxy accepts none of our auth methods")),
    }
    if host.len() > 255 {
        bail!("{}", tr!("host name too long"));
    }
    let mut m = vec![5, 1, 0, 3, host.len() as u8];
    m.extend_from_slice(host.as_bytes());
    m.extend_from_slice(&port.to_be_bytes());
    s.write_all(&m).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0 {
        bail!(
            "{}",
            match head[1] {
                1 => tr!("general failure"),
                2 => tr!("connection not allowed by ruleset"),
                3 => tr!("network unreachable"),
                4 => tr!("host unreachable"),
                5 => tr!("connection refused"),
                6 => tr!("TTL expired"),
                7 => tr!("command not supported"),
                8 => tr!("address type not supported"),
                _ => tr!("unknown error"),
            }
        );
    }
    let skip = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            l[0] as usize + 2
        }
        _ => bail!("{}", tr!("bad reply")),
    };
    let mut rest = vec![0u8; skip];
    s.read_exact(&mut rest).await?;
    Ok(())
}

/// HTTP CONNECT tunnel, Basic auth when a user is set.
async fn http_connect<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    host: &str,
    port: u16,
    user: &str,
    pass: Option<&str>,
) -> Result<()> {
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: haunt\r\n");
    if !user.is_empty() {
        let cred = base64(format!("{user}:{}", pass.unwrap_or("")).as_bytes());
        req.push_str(&format!("Proxy-Authorization: Basic {cred}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;
    // Byte by byte: the SSH banner may follow the headers at once and must stay in the stream.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            bail!("{}", tr!("reply headers too long"));
        }
        s.read_exact(&mut b).await.context(tr!("the proxy closed the connection"))?;
        head.push(b[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let status = head.lines().next().unwrap_or("").trim();
    match status.split_whitespace().nth(1).unwrap_or("") {
        "200" => Ok(()),
        "407" => bail!("{}", tr!("proxy authentication required ({status})", status = status)),
        _ => bail!("{status}"),
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

async fn ask(
    shared: &Mutex<Shared>,
    ctx: &egui::Context,
    kind: PromptKind,
    title: &str,
    message: &str,
) -> Option<(String, bool)> {
    let (tx, rx) = oneshot::channel();
    shared.lock().prompt = Some(Prompt { kind, title: title.into(), message: message.into(), reply: tx });
    ctx.request_repaint();
    rx.await.ok().flatten()
}

/// Verifies the server's host key against our known_hosts file and the user's
/// `~/.ssh/known_hosts`. Unknown keys need explicit confirmation; a changed key is refused.
struct HostKeyCheck {
    host: String,
    port: u16,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
}

impl client::Handler for HostKeyCheck {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            bail!("{}", tr!("the server presented a host certificate; certificate host keys are not supported yet"));
        };
        let ours = config::known_hosts_path();
        let fingerprint = key.fingerprint(HashAlg::Sha256);

        let changed = |line: usize, file: &str| {
            anyhow::Error::new(Fatal(tr!(
                "HOST KEY CHANGED for {addr} (recorded in {file}, line {line}).\nNew key: {key}\nThis can mean someone is intercepting the connection. If the server was reinstalled, remove the old line from that file and connect again.",
                addr = format!("{}:{}", self.host, self.port),
                file = file,
                line = line,
                key = format!("{} {fingerprint}", key.algorithm()),
            )))
        };

        if let Some(path) = &ours {
            match russh::keys::check_known_hosts_path(&self.host, self.port, key, path) {
                Ok(true) => return Ok(true),
                Err(russh::keys::Error::KeyChanged { line }) => {
                    return Err(changed(line, &path.display().to_string()));
                }
                _ => {}
            }
        }
        match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => return Ok(true),
            Err(russh::keys::Error::KeyChanged { line }) => return Err(changed(line, "~/.ssh/known_hosts")),
            _ => {}
        }

        let message = tr!(
            "First connection to {addr}.\n\nHost key fingerprint ({algorithm}):\n{fingerprint}\n\nMake sure it matches the server before trusting it.",
            addr = format!("{}:{}", self.host, self.port),
            algorithm = key.algorithm(),
            fingerprint = fingerprint,
        );
        match ask(&self.shared, &self.ctx, PromptKind::HostKey, tr!("Unknown host"), &message).await {
            Some(_) => {
                if let Some(path) = &ours
                    && let Err(e) = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, path) {
                        log::warn!("could not save host key: {e}");
                    }
                Ok(true)
            }
            None => Err(Fatal(tr!("host key not trusted, connection aborted").into()).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    fn srv(name: &str) -> Server {
        let mut s = Server::new();
        s.name = name.into();
        s.host = format!("{name}.example");
        s
    }

    #[test]
    fn jump_chain_and_loop() {
        let mut a = srv("a");
        let mut b = srv("b");
        let c = srv("c");
        a.proxy = Proxy::Jump { server: b.id };
        b.proxy = Proxy::Jump { server: c.id };
        let all = vec![a.clone(), b.clone(), c.clone()];
        let spec = ConnectSpec::from_server(&a, &all, 30);
        assert_eq!(spec.route(), "root@c.example:22 → root@b.example:22 → root@a.example:22");

        b.proxy = Proxy::Jump { server: a.id };
        let all = vec![a.clone(), b.clone()];
        let spec = ConnectSpec::from_server(&a, &all, 30);
        let Some(Via::Jump(j)) = spec.via.as_deref() else { panic!("no jump") };
        assert!(matches!(j.via.as_deref(), Some(Via::Broken(_))));
    }

    #[tokio::test]
    async fn socks5_handshake_with_auth() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let fake = tokio::spawn(async move {
            let mut b = [0u8; 4];
            server.read_exact(&mut b).await.unwrap();
            assert_eq!(b, [5, 2, 0, 2]);
            server.write_all(&[5, 2]).await.unwrap();
            let mut a = [0u8; 5];
            server.read_exact(&mut a).await.unwrap();
            assert_eq!(a, [1, 1, b'u', 1, b'p']);
            server.write_all(&[1, 0]).await.unwrap();
            let mut c = [0u8; 5 + 6 + 2];
            server.read_exact(&mut c).await.unwrap();
            assert_eq!(&c[5..11], b"host.x");
            assert_eq!(&c[11..], &22u16.to_be_bytes());
            server.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });
        socks5(&mut client, "host.x", 22, "u", Some("p")).await.unwrap();
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_keeps_banner() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 512];
            let n = server.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("CONNECT h:22 HTTP/1.1\r\n"));
            server.write_all(b"HTTP/1.1 200 Connection established\r\n\r\nSSH-2.0-x\r\n").await.unwrap();
        });
        http_connect(&mut client, "h", 22, "", None).await.unwrap();
        let mut banner = [0u8; 9];
        client.read_exact(&mut banner).await.unwrap();
        assert_eq!(&banner, b"SSH-2.0-x");
    }

    #[tokio::test]
    async fn http_connect_reports_407() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 512];
            let _ = server.read(&mut buf).await.unwrap();
            server.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await.unwrap();
        });
        let e = http_connect(&mut client, "h", 22, "", None).await.unwrap_err();
        assert!(e.to_string().contains("407"));
    }

    #[test]
    fn dropped_connections_get_a_plain_reason() {
        let ka = Duration::from_secs(30);
        let r = lost_reason(&anyhow::Error::from(russh::Error::KeepaliveTimeout), ka);
        assert!(r.contains("stopped answering") && r.contains("90 s"), "{r}");
        let eof = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "early eof");
        let r = lost_reason(&anyhow::Error::from(russh::Error::IO(eof)), ka);
        assert!(r.contains("was cut"), "{r}");
        let r = lost_reason(&anyhow::Error::from(russh::Error::HUP), ka);
        assert!(r.contains("closed it"), "{r}");
    }

    #[test]
    fn host_key_errors_are_never_retried() {
        let e = anyhow::Error::new(Fatal("HOST KEY CHANGED".into())).context("SSH handshake with h failed");
        assert!(is_fatal(&e));
        assert!(!is_fatal(&anyhow!("cannot reach h:22")));
    }
}
