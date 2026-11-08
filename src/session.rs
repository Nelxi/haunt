//! One SSH session: the async network task plus the state the UI reads.
//!
//! Threading model: the terminal grid (`Term`) sits behind a mutex. The tokio task parses
//! incoming bytes into it and asks egui for a repaint; the UI thread locks it only while
//! drawing a frame. Keystrokes go the other way through an unbounded channel.
//!
//! Lock order: the task may lock `shared` while holding `term` (terminal events such as a
//! title change go through the listener). The UI must therefore never lock `term` while
//! holding `shared`.

use std::path::PathBuf;
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
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::config::{self, Auth, Server};
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
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
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
}

impl ConnectSpec {
    pub fn from_server(s: &Server, keepalive_secs: u64) -> Self {
        Self {
            server_id: Some(s.id),
            host: s.host.trim().to_string(),
            port: s.port,
            user: s.user.trim().to_string(),
            auth: s.auth.clone(),
            keepalive: Duration::from_secs(keepalive_secs.max(5)),
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
}

impl Task {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Input>) {
        let target = format!("{}@{}:{}", self.spec.user, self.spec.host, self.spec.port);
        self.out.info(&format!("connecting to {target} …"));
        let reason = match self.session(&mut rx).await {
            Ok(reason) => {
                self.out.info(&format!("\r\n{reason}"));
                reason
            }
            Err(e) => {
                let msg = format!("{e:#}");
                self.out.error(&msg);
                msg
            }
        };
        let mut sh = self.shared.lock();
        sh.status = Status::Closed(reason);
        sh.prompt = None;
        drop(sh);
        self.ctx.request_repaint();
    }

    async fn session(&mut self, rx: &mut mpsc::UnboundedReceiver<Input>) -> Result<String> {
        let spec = self.spec.clone();
        if spec.host.is_empty() {
            bail!("no host set");
        }

        let stream = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect((spec.host.as_str(), spec.port)),
        )
        .await
        .map_err(|_| anyhow!("connection timed out after 10 s"))?
        .with_context(|| format!("cannot reach {}:{}", spec.host, spec.port))?;
        stream.set_nodelay(true)?;

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
            .context("SSH handshake failed")?;

        self.authenticate(&mut handle).await?;

        let channel = handle.channel_open_session().await.context("cannot open a session channel")?;
        let size = self.shared.lock().size;
        channel
            .request_pty(false, "xterm-256color", size.cols as u32, size.rows as u32, 0, 0, &[])
            .await
            .context("PTY request failed")?;
        // Most servers drop env requests (AcceptEnv); harmless when they do.
        let _ = channel.set_env(false, "COLORTERM", "truecolor").await;
        channel.request_shell(false).await.context("shell request failed")?;

        self.shared.lock().status = Status::Connected;
        self.ctx.request_repaint();

        let mut channel = channel;
        let mut exit_status: Option<u32> = None;
        loop {
            tokio::select! {
                input = rx.recv() => match input {
                    Some(Input::Data(bytes)) => channel.data(&bytes[..]).await?,
                    Some(Input::Resize(s)) => {
                        let _ = channel.window_change(s.cols as u32, s.rows as u32, 0, 0).await;
                    }
                    Some(Input::Close) | None => {
                        let _ = channel.eof().await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                        return Ok("session closed".into());
                    }
                },
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => self.out.feed(&data),
                    Some(ChannelMsg::ExtendedData { data, .. }) => self.out.feed(&data),
                    Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                    Some(ChannelMsg::Close) | None => break,
                    _ => {}
                },
            }
        }
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        Ok(match exit_status {
            Some(0) | None => "connection closed".into(),
            Some(code) => format!("connection closed (exit status {code})"),
        })
    }

    async fn authenticate(&mut self, handle: &mut client::Handle<HostKeyCheck>) -> Result<()> {
        let spec = self.spec.clone();
        match &spec.auth {
            Auth::Key { path } => {
                let key = self.load_key(path).await?;
                let hash = handle.best_supported_rsa_hash().await?.flatten();
                let res = handle
                    .authenticate_publickey(&spec.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await?;
                if !res.success() {
                    bail!("the server rejected key {} for user {}", path.display(), spec.user);
                }
                Ok(())
            }
            Auth::Password => {
                let mut saved = spec.server_id.and_then(secrets::get);
                for attempt in 0..3 {
                    let from_store = saved.is_some();
                    let (password, remember) = match saved.take() {
                        Some(p) => (p, false),
                        None => {
                            let msg = if attempt == 0 {
                                format!("Password for {}@{}", spec.user, spec.host)
                            } else {
                                "Wrong password, try again".to_string()
                            };
                            self.ask(PromptKind::Password, "Password", &msg)
                                .await
                                .ok_or_else(|| anyhow!("authentication cancelled"))?
                        }
                    };
                    let res = handle.authenticate_password(&spec.user, &password).await?;
                    if res.success() {
                        if remember {
                            if let Some(id) = spec.server_id {
                                if let Err(e) = secrets::set(id, &password) {
                                    self.out.error(&format!("could not save the password: {e}"));
                                }
                            }
                        }
                        return Ok(());
                    }
                    if from_store {
                        self.out.info("saved password was rejected");
                    }
                }
                bail!("authentication failed")
            }
        }
    }

    /// Loads a private key, asking for the passphrase if the key needs one.
    async fn load_key(&mut self, path: &PathBuf) -> Result<ssh_key::PrivateKey> {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("cannot read key file {}", path.display()))?;
        let first_err = match russh::keys::decode_secret_key(&text, None) {
            Ok(k) => return Ok(k),
            Err(e) => e,
        };
        // Encrypted key (or unreadable). Try the saved passphrase, then ask.
        let mut saved = self.spec.server_id.and_then(secrets::get);
        for attempt in 0..3 {
            let (pass, remember) = match saved.take() {
                Some(p) => (p, false),
                None => {
                    let msg = if attempt == 0 {
                        format!("Passphrase for {}\n({first_err})", path.display())
                    } else {
                        "Wrong passphrase, try again".to_string()
                    };
                    match self.ask(PromptKind::Passphrase, "Key passphrase", &msg).await {
                        Some(r) => r,
                        None => bail!("cannot load key {}: {first_err}", path.display()),
                    }
                }
            };
            if let Ok(k) = russh::keys::decode_secret_key(&text, Some(&pass)) {
                if remember {
                    if let Some(id) = self.spec.server_id {
                        let _ = secrets::set(id, &pass);
                    }
                }
                return Ok(k);
            }
        }
        bail!("cannot decrypt key {}", path.display())
    }

    async fn ask(&self, kind: PromptKind, title: &str, message: &str) -> Option<(String, bool)> {
        ask(&self.shared, &self.ctx, kind, title, message).await
    }
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
            bail!("the server presented a host certificate; certificate host keys are not supported yet");
        };
        let ours = config::known_hosts_path();
        let fingerprint = key.fingerprint(HashAlg::Sha256);

        let changed = |line: usize, file: &str| {
            anyhow!(
                "HOST KEY CHANGED for {}:{} (recorded in {file}, line {line}).\n\
                 New key: {} {fingerprint}\n\
                 This can mean someone is intercepting the connection. If the server was \
                 reinstalled, remove the old line from that file and connect again.",
                self.host,
                self.port,
                key.algorithm(),
            )
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

        let message = format!(
            "First connection to {}:{}.\n\nHost key fingerprint ({}):\n{fingerprint}\n\n\
             Make sure it matches the server before trusting it.",
            self.host,
            self.port,
            key.algorithm(),
        );
        match ask(&self.shared, &self.ctx, PromptKind::HostKey, "Unknown host", &message).await {
            Some(_) => {
                if let Some(path) = &ours {
                    if let Err(e) = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, path) {
                        log::warn!("could not save host key: {e}");
                    }
                }
                Ok(true)
            }
            None => bail!("host key not trusted, connection aborted"),
        }
    }
}
