//! `haunt mcp`: an MCP server (JSON-RPC over stdio) that lets AI agents such as Claude Code
//! and Codex run commands and edit files on the servers the user shared with them.
//!
//! It reads the same config, keys and saved passwords as the app and keeps one connection
//! per server for as long as the agent runs. Replies are made for a model to read: plain
//! text, terminal escapes removed, long output cut to its head and tail, servers that
//! answered the same merged into one block.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::{Config, Server};
use crate::tr;
use crate::session::{ConnectSpec, ExecEvent, PromptKind, SftpSession, Session, Status};

/// Protocol versions we speak; the newest is offered when the client asks for another.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(40);
/// Output lines per server, by default and at most.
const LINES: usize = 200;
const MAX_LINES: usize = 5000;
/// Longer lines (minified JSON, base64) are cut to this many characters.
const LINE_CHARS: usize = 400;
/// Raw output kept per server: the first `HEAD` bytes and the last `TAIL`.
const HEAD: usize = 256 << 10;
const TAIL: usize = 4 << 20;
/// Files bigger than this are not read whole (`run` with sed/tail does it better).
const MAX_FILE: u64 = 8 << 20;

const INSTRUCTIONS: &str = "The user's own SSH servers, saved in the haunt SSH client. \
Use `run` for shell commands (on one server or many at once) and read/edit/write for files: \
no quoting or heredocs needed. Relative paths and ~ start in the home directory.";

pub fn main() -> i32 {
    // Messages are for the model: English, whatever the app's language.
    crate::i18n::set("en");
    let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("haunt mcp: {e}");
            return 1;
        }
    };
    let hub = Arc::new(Hub { rt: rt.handle().clone(), ctx: egui::Context::default(), conns: Mutex::new(HashMap::new()) });
    rt.block_on(serve(hub));
    rt.shutdown_timeout(Duration::from_secs(1));
    0
}

async fn serve(hub: Arc<Hub>) {
    let out = Arc::new(Mutex::new(tokio::io::stdout()));
    let calls: Arc<std::sync::Mutex<HashMap<String, tokio::task::AbortHandle>>> = Default::default();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&out, json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})).await;
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            // A notification. The only one that matters: the agent gave up on a call.
            if method == "notifications/cancelled"
                && let Some(call) = calls.lock().unwrap().remove(&params["requestId"].to_string())
            {
                call.abort();
            }
            continue;
        };
        let result = match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "haunt", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                // Calls run side by side: a long command must not hold up the rest.
                let (hub, out, calls2) = (hub.clone(), out.clone(), calls.clone());
                let key = id.to_string();
                let task = tokio::spawn(async move {
                    let name = params["name"].as_str().unwrap_or_default().to_string();
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    let (text, error) = match hub.call(&name, &args).await {
                        Ok(text) => (text, false),
                        Err(text) => (text, true),
                    };
                    calls2.lock().unwrap().remove(&id.to_string());
                    let result = json!({"content": [{"type": "text", "text": text}], "isError": error});
                    send(&out, json!({"jsonrpc": "2.0", "id": id, "result": result})).await;
                });
                calls.lock().unwrap().insert(key, task.abort_handle());
                continue;
            }
            _ => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
        };
        let reply = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        };
        send(&out, reply).await;
    }
    // The agent went away. Dropping the connections ends whatever still runs.
    for call in calls.lock().unwrap().drain() {
        call.1.abort();
    }
    hub.conns.lock().await.clear();
}

async fn send(out: &Mutex<tokio::io::Stdout>, msg: Value) {
    let mut line = msg.to_string();
    line.push('\n');
    let mut out = out.lock().await;
    let _ = out.write_all(line.as_bytes()).await;
    let _ = out.flush().await;
}

/// Tool schemas. Descriptions are short on purpose: they sit in the agent's context all the time.
fn tools() -> Value {
    let on = json!({"type": "string", "description": "Server name, group name, comma-separated names, or \"all\""});
    let server = json!({"type": "string", "description": "Server name"});
    let path = json!({"type": "string"});
    json!([
        {
            "name": "servers",
            "description": "List the servers you may use.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true},
        },
        {
            "name": "run",
            "description": "Run a shell command on one or more servers in parallel. No TTY, no stdin. Long output keeps its head and tail; servers with identical results are merged.",
            "inputSchema": {"type": "object", "properties": {
                "on": on,
                "command": {"type": "string"},
                "timeout": {"type": "integer", "description": "Seconds, default 120"},
                "lines": {"type": "integer", "description": "Max output lines per server, default 200"},
            }, "required": ["on", "command"]},
        },
        {
            "name": "read",
            "description": "Read a text file.",
            "inputSchema": {"type": "object", "properties": {
                "server": server,
                "path": path,
                "offset": {"type": "integer", "description": "First line, from 1"},
                "limit": {"type": "integer", "description": "Lines, default 400"},
            }, "required": ["server", "path"]},
            "annotations": {"readOnlyHint": true},
        },
        {
            "name": "write",
            "description": "Create or overwrite a file.",
            "inputSchema": {"type": "object", "properties": {
                "server": server, "path": path, "content": {"type": "string"},
            }, "required": ["server", "path", "content"]},
        },
        {
            "name": "edit",
            "description": "Replace exact text in a file. `old` must occur once unless all=true.",
            "inputSchema": {"type": "object", "properties": {
                "server": server, "path": path,
                "old": {"type": "string"}, "new": {"type": "string"},
                "all": {"type": "boolean"},
            }, "required": ["server", "path", "old", "new"]},
        },
        {
            "name": "copy",
            "description": "Copy a file between this PC and a server.",
            "inputSchema": {"type": "object", "properties": {
                "server": server,
                "local": {"type": "string", "description": "Path on this PC"},
                "remote": {"type": "string"},
                "direction": {"type": "string", "enum": ["upload", "download"]},
            }, "required": ["server", "local", "remote", "direction"]},
        },
    ])
}

struct Hub {
    rt: tokio::runtime::Handle,
    ctx: egui::Context,
    conns: Mutex<HashMap<Uuid, Conn>>,
}

struct Conn {
    session: Session,
    /// The settings it was opened with: a server edited in the app gets a fresh connection.
    spec: String,
    /// An SFTP channel and the connection count it belongs to.
    sftp: Option<(u64, Arc<SftpSession>)>,
}

type Res<T> = Result<T, String>;

impl Hub {
    async fn call(&self, name: &str, args: &Value) -> Res<String> {
        // Read on every call: servers shared or edited in the app count at once.
        let (cfg, err) = Config::load();
        if let Some(e) = err {
            return Err(format!("cannot read the haunt config: {e}"));
        }
        let text = |key: &str| -> Res<String> {
            args[key].as_str().map(str::to_string).ok_or_else(|| format!("missing `{key}`"))
        };
        match name {
            "servers" => Ok(list(&cfg)),
            "run" => {
                let targets = pick(&cfg, &text("on")?)?;
                let command = text("command")?;
                let timeout = Duration::from_secs(args["timeout"].as_u64().unwrap_or(120).clamp(1, 3600));
                let lines = args["lines"].as_u64().map_or(LINES, |n| (n as usize).clamp(1, MAX_LINES));
                let runs = targets.iter().map(|s| self.run(&cfg, s, &command, timeout));
                let results = futures_util::future::join_all(runs).await;
                let names: Vec<&str> = targets.iter().map(|s| s.display_name()).collect();
                for (s, r) in targets.iter().zip(&results) {
                    log(s, "run", &command, &r.end.to_string());
                }
                Ok(report(&names, &results, lines))
            }
            "read" => {
                let server = one(&cfg, &text("server")?)?;
                let path = text("path")?;
                let sftp = self.sftp(&cfg, &server).await?;
                let meta = sftp.metadata(remote(&path)).await.map_err(sftp_err)?;
                if meta.file_type().is_dir() {
                    return Err(format!("{path} is a folder"));
                }
                let size = meta.size.unwrap_or(0);
                if size > MAX_FILE {
                    return Err(format!("{path} is {}; read parts of it with `run` (sed -n, tail, grep)", crate::files::human_size(size)));
                }
                let data = sftp.read(remote(&path)).await.map_err(sftp_err)?;
                if data.iter().take(8192).any(|b| *b == 0) {
                    return Ok(format!("[binary file, {}]", crate::files::human_size(data.len() as u64)));
                }
                let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
                let limit = args["limit"].as_u64().map_or(400, |n| n.max(1) as usize);
                Ok(slice(&String::from_utf8_lossy(&data), offset, limit))
            }
            "write" => {
                let server = one(&cfg, &text("server")?)?;
                let (path, content) = (text("path")?, text("content")?);
                let sftp = self.sftp(&cfg, &server).await?;
                put(&sftp, &path, content.as_bytes()).await?;
                log(&server, "write", &path, &format!("{} bytes", content.len()));
                Ok(format!("wrote {}", crate::files::human_size(content.len() as u64)))
            }
            "edit" => {
                let server = one(&cfg, &text("server")?)?;
                let (path, old, new) = (text("path")?, text("old")?, text("new")?);
                if old.is_empty() {
                    return Err("`old` is empty; use `write` to create a file".into());
                }
                let sftp = self.sftp(&cfg, &server).await?;
                let data = sftp.read(remote(&path)).await.map_err(sftp_err)?;
                let body = String::from_utf8(data).map_err(|_| format!("{path} is not UTF-8 text"))?;
                let (body, n) = replace(&body, &old, &new, args["all"].as_bool().unwrap_or(false))?;
                put(&sftp, &path, body.as_bytes()).await?;
                log(&server, "edit", &path, &format!("{n} replaced"));
                Ok(if n == 1 { "replaced 1".into() } else { format!("replaced {n}") })
            }
            "copy" => {
                let server = one(&cfg, &text("server")?)?;
                let (local, rpath) = (text("local")?, text("remote")?);
                let up = match args["direction"].as_str() {
                    Some("upload") => true,
                    Some("download") => false,
                    _ => return Err("`direction` is upload or download".into()),
                };
                let sftp = self.sftp(&cfg, &server).await?;
                let started = Instant::now();
                let bytes = if up { upload(&sftp, &local, &rpath).await? } else { download(&sftp, &rpath, &local).await? };
                let what = format!("{} {local} {} {rpath}", if up { "up" } else { "down" }, if up { "->" } else { "<-" });
                log(&server, "copy", &what, &format!("{bytes} bytes"));
                Ok(format!(
                    "{} {} in {:.1}s",
                    if up { "uploaded" } else { "downloaded" },
                    crate::files::human_size(bytes),
                    started.elapsed().as_secs_f32()
                ))
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }

    /// Makes sure the server has a working connection, then runs `f` on it (under the lock).
    async fn with_conn<T>(&self, cfg: &Config, server: &Server, f: impl Fn(&mut Conn) -> Option<T>) -> Res<T> {
        let mut spec = ConnectSpec::from_server(server, &cfg.servers, cfg.settings.keepalive_secs);
        spec.shell = false;
        // Tunnels belong to the app's tabs; the agent's connection shouldn't grab their ports.
        spec.tunnels.clear();
        let key = format!("{spec:?}");
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut started = false;
        loop {
            {
                let mut conns = self.conns.lock().await;
                let conn = conns.entry(server.id).or_insert_with(|| {
                    started = true;
                    let session = Session::start(0, server.display_name().into(), spec.clone(), Default::default(), &self.rt, &self.ctx);
                    Conn { session, spec: key.clone(), sftp: None }
                });
                if conn.spec != key {
                    conns.remove(&server.id);
                    continue;
                }
                let refused = conn.session.shared.lock().prompt.take();
                if let Some(p) = refused {
                    let _ = p.reply.send(None);
                    let name = server.display_name();
                    conns.remove(&server.id);
                    return Err(match p.kind {
                        PromptKind::HostKey => format!("{name}: unknown host key; connect to it once in the haunt app and accept the key"),
                        PromptKind::Password => format!("{name}: needs a password that isn't saved; connect once in the haunt app and tick Remember"),
                        PromptKind::Passphrase => format!("{name}: the key needs a passphrase that isn't saved; connect once in the haunt app and tick Remember"),
                    });
                }
                match conn.session.status() {
                    Status::Connected => {
                        if let Some(v) = f(conn) {
                            return Ok(v);
                        }
                    }
                    Status::Closed(reason) if started => {
                        conns.remove(&server.id);
                        return Err(format!("{}: {reason}", server.display_name()));
                    }
                    Status::Closed(_) => {
                        // Dropped since the last call: try again once.
                        started = true;
                        conn.sftp = None;
                        conn.session.reconnect(&self.rt, &self.ctx);
                    }
                    _ => {}
                }
            }
            if Instant::now() > deadline {
                return Err(format!("{}: no connection after {} s", server.display_name(), CONNECT_TIMEOUT.as_secs()));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn run(&self, cfg: &Config, server: &Server, command: &str, timeout: Duration) -> Ran {
        let started = Instant::now();
        let mut ran = Ran { head: Vec::new(), tail: Vec::new(), skipped: 0, end: End::Lost, took: Duration::ZERO };
        let mut rx = match self.with_conn(cfg, server, |c| c.session.exec(command.to_string())).await {
            Ok(rx) => rx,
            Err(e) => {
                ran.end = End::Failed(e);
                return ran;
            }
        };
        let stop = tokio::time::sleep(timeout);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ExecEvent::Out(b)) => ran.push(&b),
                    Some(ExecEvent::Exit { code, signal }) => {
                        ran.end = End::Exit(code, signal);
                        break;
                    }
                    Some(ExecEvent::Failed(e)) => {
                        ran.end = End::Failed(e);
                        break;
                    }
                    None => break,
                },
                () = &mut stop => {
                    ran.end = End::Timeout(timeout.as_secs());
                    break;
                }
            }
        }
        // Dropping the receiver stops a command that is still going.
        drop(rx);
        ran.took = started.elapsed();
        ran
    }

    async fn sftp(&self, cfg: &Config, server: &Server) -> Res<Arc<SftpSession>> {
        let (have, rx) = self
            .with_conn(cfg, server, |c| {
                let connects = c.session.connects();
                Some(match &c.sftp {
                    Some((n, s)) if *n == connects => (Some(s.clone()), None),
                    _ => (None, Some((connects, c.session.open_sftp()))),
                })
            })
            .await?;
        if let Some(s) = have {
            return Ok(s);
        }
        let (connects, rx) = rx.expect("one of the two");
        let rx = rx.ok_or_else(|| format!("{}: connection lost", server.display_name()))?;
        let sftp = match tokio::time::timeout(CONNECT_TIMEOUT, rx).await {
            Ok(Ok(Ok(s))) => Arc::new(s),
            Ok(Ok(Err(e))) => return Err(format!("{}: {e}", server.display_name())),
            _ => return Err(format!("{}: SFTP did not start", server.display_name())),
        };
        if let Some(c) = self.conns.lock().await.get_mut(&server.id) {
            c.sftp = Some((connects, sftp.clone()));
        }
        Ok(sftp)
    }
}

/// What one server did with a command.
struct Ran {
    head: Vec<u8>,
    tail: Vec<u8>,
    /// Bytes dropped between head and tail.
    skipped: u64,
    end: End,
    took: Duration,
}

impl Ran {
    fn push(&mut self, mut b: &[u8]) {
        if self.head.len() < HEAD {
            let n = (HEAD - self.head.len()).min(b.len());
            self.head.extend_from_slice(&b[..n]);
            b = &b[n..];
        }
        self.tail.extend_from_slice(b);
        if self.tail.len() > TAIL + (TAIL >> 2) {
            let cut = self.tail.len() - TAIL;
            self.tail.drain(..cut);
            self.skipped += cut as u64;
        }
    }

    fn text(&self) -> String {
        let mut raw = self.head.clone();
        if self.skipped > 0 {
            raw.extend_from_slice(format!("\n… [{} skipped] …\n", crate::files::human_size(self.skipped)).as_bytes());
        }
        raw.extend_from_slice(&self.tail);
        crate::multi::clean(&raw)
    }
}

#[derive(Clone, PartialEq)]
enum End {
    Exit(Option<u32>, Option<String>),
    Failed(String),
    Timeout(u64),
    Lost,
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            End::Exit(Some(c), _) => write!(f, "exit {c}"),
            End::Exit(None, Some(sig)) => write!(f, "killed by SIG{sig}"),
            End::Exit(None, None) => f.write_str("ended without a status"),
            End::Failed(e) => write!(f, "error: {e}"),
            End::Timeout(s) => write!(f, "timed out after {s}s and was stopped (pass a bigger `timeout`)"),
            End::Lost => f.write_str("connection lost"),
        }
    }
}

/// The reply to `run`: one server reads like a terminal; several are grouped by result.
fn report(names: &[&str], results: &[Ran], lines: usize) -> String {
    let shaped: Vec<String> = results.iter().map(|r| shape(&r.text(), lines)).collect();
    if let [r] = results {
        let mut out = shaped[0].clone();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("[{} · {:.1}s]", r.end, r.took.as_secs_f32()));
        return out;
    }
    let mut groups: Vec<(Vec<&str>, &End, &str)> = Vec::new();
    for ((name, r), text) in names.iter().zip(results).zip(&shaped) {
        match groups.iter_mut().find(|g| *g.1 == r.end && g.2 == text) {
            Some(g) => g.0.push(name),
            None => groups.push((vec![name], &r.end, text)),
        }
    }
    let mut out = String::new();
    for (names, end, text) in groups {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("## {}: {end}\n", names.join(", ")));
        if !text.is_empty() {
            out.push_str(text);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Cuts long lines, and long output to its first quarter and the rest from the end.
fn shape(text: &str, max: usize) -> String {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let cut = |l: &str| -> String {
        let l = l.trim_end_matches('\r');
        match l.char_indices().nth(LINE_CHARS) {
            Some((i, _)) => format!("{} …[+{} chars]", &l[..i], l[i..].chars().count()),
            None => l.to_string(),
        }
    };
    if lines.len() <= max {
        return lines.iter().map(|l| cut(l)).collect::<Vec<_>>().join("\n");
    }
    let head = max / 4;
    let tail = max - head;
    let mut out: Vec<String> = lines[..head].iter().map(|l| cut(l)).collect();
    out.push(format!("… [{} lines skipped] …", lines.len() - head - tail));
    out.extend(lines[lines.len() - tail..].iter().map(|l| cut(l)));
    out.join("\n")
}

/// Lines `offset..offset+limit` of a file (from 1), with a note when it is not the whole file.
fn slice(text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if offset == 1 && total <= limit {
        return text.to_string();
    }
    if offset > total {
        return format!("[the file has {total} lines]");
    }
    let end = (offset - 1 + limit).min(total);
    let mut out = lines[offset - 1..end].join("\n");
    out.push_str(&format!("\n[lines {offset}-{end} of {total}]"));
    out
}

fn replace(body: &str, old: &str, new: &str, all: bool) -> Res<(String, usize)> {
    let n = body.matches(old).count();
    // Files with Windows line ends: let the model's \n match them.
    if n == 0 && body.contains("\r\n") && !old.contains('\r') {
        let crlf = |s: &str| s.replace('\n', "\r\n");
        return replace(body, &crlf(old), &crlf(new), all);
    }
    match n {
        0 => Err("`old` not found; read the file and copy the text exactly".into()),
        1 => Ok((body.replacen(old, new, 1), 1)),
        n if all => Ok((body.replace(old, new), n)),
        n => Err(format!("`old` occurs {n} times; add surrounding lines to make it unique, or set all=true")),
    }
}

/// SFTP paths start in the home directory, so `~/x` is just `x`.
fn remote(path: &str) -> String {
    match path {
        "~" => ".".into(),
        p => p.strip_prefix("~/").unwrap_or(p).to_string(),
    }
}

async fn put(sftp: &SftpSession, path: &str, data: &[u8]) -> Res<()> {
    let mut f = sftp.create(remote(path)).await.map_err(sftp_err)?;
    f.write_all(data).await.map_err(|e| e.to_string())?;
    f.shutdown().await.map_err(|e| e.to_string())
}

async fn upload(sftp: &SftpSession, local: &str, path: &str) -> Res<u64> {
    let mut input = tokio::fs::File::open(local).await.map_err(|e| format!("{local}: {e}"))?;
    if input.metadata().await.is_ok_and(|m| m.is_dir()) {
        return Err(format!("{local} is a folder; copy takes files (pack folders with tar first)"));
    }
    let mut out = sftp.create(remote(path)).await.map_err(sftp_err)?;
    let n = tokio::io::copy(&mut input, &mut out).await.map_err(|e| e.to_string())?;
    out.shutdown().await.map_err(|e| e.to_string())?;
    Ok(n)
}

async fn download(sftp: &SftpSession, path: &str, local: &str) -> Res<u64> {
    let mut input = sftp.open(remote(path)).await.map_err(sftp_err)?;
    let mut out = tokio::fs::File::create(local).await.map_err(|e| format!("{local}: {e}"))?;
    let n = tokio::io::copy(&mut input, &mut out).await.map_err(|e| e.to_string())?;
    out.flush().await.map_err(|e| e.to_string())?;
    Ok(n)
}

fn sftp_err(e: russh_sftp::client::error::Error) -> String {
    crate::files::sftp_err(e)
}

/// The servers the agent may use, one per line.
fn list(cfg: &Config) -> String {
    let shared: Vec<&Server> = cfg.servers.iter().filter(|s| s.ai).collect();
    if shared.is_empty() {
        return "No servers are shared with AI agents. The user turns them on in haunt: Settings → AI agents.".into();
    }
    shared
        .iter()
        .map(|s| {
            let group = if s.group.trim().is_empty() { String::new() } else { format!("  [{}]", s.group.trim()) };
            format!("{}  {}@{}:{}{group}", s.display_name(), s.user.trim(), s.host.trim(), s.port)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Exactly one server.
fn one(cfg: &Config, name: &str) -> Res<Server> {
    match pick(cfg, name)?.as_slice() {
        [s] => Ok(s.clone()),
        many => Err(format!("`server` must name one server, \"{name}\" means {}", many.len())),
    }
}

/// Servers by name, group, host, a comma-separated list of those, or "all".
fn pick(cfg: &Config, on: &str) -> Res<Vec<Server>> {
    let shared: Vec<&Server> = cfg.servers.iter().filter(|s| s.ai).collect();
    let names = || shared.iter().map(|s| s.display_name()).collect::<Vec<_>>().join(", ");
    if shared.is_empty() {
        return Err(list(cfg));
    }
    let mut out: Vec<Server> = Vec::new();
    for part in on.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let found: Vec<&Server> = if part.eq_ignore_ascii_case("all") || part == "*" {
            shared.clone()
        } else if let Some(s) = cfg.servers.iter().find(|s| s.display_name().eq_ignore_ascii_case(part)) {
            if !s.ai {
                return Err(format!("{} is not shared with AI agents (the user can allow it in haunt: Settings → AI agents)", s.display_name()));
            }
            vec![s]
        } else {
            let group: Vec<&Server> = shared.iter().copied().filter(|s| s.group.trim().eq_ignore_ascii_case(part)).collect();
            if !group.is_empty() {
                group
            } else {
                let host: Vec<&Server> = shared.iter().copied().filter(|s| s.host.trim().eq_ignore_ascii_case(part)).collect();
                if host.is_empty() {
                    return Err(format!("no server \"{part}\"; yours: {}", names()));
                }
                host
            }
        };
        for s in found {
            if !out.iter().any(|o| o.id == s.id) {
                out.push(s.clone());
            }
        }
    }
    if out.is_empty() {
        return Err(format!("say which servers; yours: {}", names()));
    }
    Ok(out)
}

/// Every action goes to `mcp.log` next to the config, so the user can see what agents did.
fn log(server: &Server, tool: &str, what: &str, result: &str) {
    let Some(path) = log_path() else { return };
    // Keep the file from growing without end: start over past 4 MB.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 << 20) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let what: String = what.replace(['\n', '\r'], " ⏎ ").chars().take(500).collect();
    let line = format!("{}\t{}\t{tool}\t{what}\t{result}\n", stamp(), server.display_name());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Local time, to the minute.
fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::platform::format_local(secs as i64)
}

/// Where the log of agent actions goes.
pub fn log_path() -> Option<std::path::PathBuf> {
    crate::config::config_dir().map(|d| d.join("mcp.log"))
}

#[derive(Clone, Copy)]
pub enum Agent {
    Claude,
    Codex,
}

/// The result of adding haunt to an agent, for the app to show.
pub struct Setup {
    pub text: String,
    pub error: bool,
    /// A command for the user to run by hand, copied to the clipboard.
    pub copy: Option<String>,
}

/// Adds `haunt mcp` to the agent's MCP servers (for the Windows user, all projects).
/// Blocks for a few seconds: run it off the UI thread.
pub fn install(agent: Agent) -> Setup {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Setup { text: e.to_string(), error: true, copy: None },
    };
    match agent {
        Agent::Claude => install_claude(&exe),
        Agent::Codex => match install_codex(&exe) {
            Ok(path) => Setup { text: tr!("Added to Codex ({path}). Restart Codex to use it.", path = path.display()), error: false, copy: None },
            Err(e) => Setup { text: e, error: true, copy: None },
        },
    }
}

fn install_claude(exe: &std::path::Path) -> Setup {
    let by_hand = format!("claude mcp add --scope user haunt -- \"{}\" mcp", exe.display());
    let Some(claude) = find_program("claude") else {
        return Setup { text: tr!("Claude Code was not found. The command to add haunt is copied: run it in a terminal.").into(), error: true, copy: Some(by_hand) };
    };
    let run = |args: &[&std::ffi::OsStr]| {
        let mut c = std::process::Command::new(&claude);
        c.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // No console window flashing up.
            c.creation_flags(0x0800_0000);
        }
        c.output()
    };
    let os = std::ffi::OsStr::new;
    // Re-adding replaces an older entry (the app may have moved).
    let _ = run(&[os("mcp"), os("remove"), os("--scope"), os("user"), os("haunt")]);
    match run(&[os("mcp"), os("add"), os("--scope"), os("user"), os("haunt"), os("--"), exe.as_os_str(), os("mcp")]) {
        Ok(o) if o.status.success() => Setup { text: tr!("Added to Claude Code. Restart it, then /mcp shows haunt.").into(), error: false, copy: None },
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let err = if err.is_empty() { String::from_utf8_lossy(&o.stdout).trim().to_string() } else { err };
            Setup { text: tr!("Claude Code said: {e}. The command is copied: run it in a terminal.", e = err), error: true, copy: Some(by_hand) }
        }
        Err(e) => Setup { text: tr!("Claude Code said: {e}. The command is copied: run it in a terminal.", e = e), error: true, copy: Some(by_hand) },
    }
}

/// A program on PATH: an .exe if there is one, else a .cmd shim (npm installs).
fn find_program(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &["exe", "cmd", "bat"] } else { &[""] };
    for ext in exts {
        for dir in std::env::split_paths(&path) {
            let p = if ext.is_empty() { dir.join(name) } else { dir.join(format!("{name}.{ext}")) };
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // The native installer puts it here without always touching PATH.
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let p = std::path::PathBuf::from(home).join(".local").join("bin").join(if cfg!(windows) { "claude.exe" } else { "claude" });
    p.is_file().then_some(p)
}

fn install_codex(exe: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let dir = match std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).ok_or("no home folder")?;
            std::path::PathBuf::from(home).join(".codex")
        }
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("config.toml");
    let old = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let new = codex_config(&old, exe);
    if let Err(e) = new.parse::<toml::Table>() {
        return Err(tr!("{path} is not valid TOML, left as it was: {e}", path = path.display(), e = e));
    }
    std::fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// `config` without its old `[mcp_servers.haunt]` tables, plus a fresh one.
fn codex_config(config: &str, exe: &std::path::Path) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in config.lines() {
        let head = line.trim();
        if head.starts_with('[') {
            let name = head.trim_start_matches('[').trim_end_matches(']').trim();
            skipping = name == "mcp_servers.haunt" || name.starts_with("mcp_servers.haunt.");
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    // A literal string: Windows backslashes stay as they are.
    out.push_str(&format!("[mcp_servers.haunt]\ncommand = '{}'\nargs = [\"mcp\"]\n", exe.display()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(text: &str, end: End) -> Ran {
        Ran { head: text.as_bytes().to_vec(), tail: Vec::new(), skipped: 0, end, took: Duration::from_millis(300) }
    }

    #[test]
    fn merges_servers_that_answered_the_same() {
        let r = [ran("ok\n", End::Exit(Some(0), None)), ran("boom\n", End::Exit(Some(1), None)), ran("ok\n", End::Exit(Some(0), None))];
        assert_eq!(report(&["a", "b", "c"], &r, 200), "## a, c: exit 0\nok\n\n## b: exit 1\nboom");
        assert_eq!(report(&["a"], &r[..1], 200), "ok\n[exit 0 · 0.3s]");
        assert_eq!(report(&["a"], &[ran("", End::Exit(Some(0), None))], 200), "[exit 0 · 0.3s]");
    }

    #[test]
    fn keeps_head_and_tail() {
        let text: String = (1..=1000).map(|i| format!("{i}\n")).collect();
        let s = shape(&text, 8);
        assert_eq!(s, "1\n2\n… [992 lines skipped] …\n995\n996\n997\n998\n999\n1000");
        let long = "x".repeat(LINE_CHARS + 5);
        assert_eq!(shape(&long, 10), format!("{} …[+5 chars]", "x".repeat(LINE_CHARS)));
    }

    #[test]
    fn keeps_raw_output_bounded() {
        let mut r = ran("", End::Lost);
        r.head.clear();
        for _ in 0..40 {
            r.push(&vec![b'a'; 256 << 10]);
        }
        assert_eq!(r.head.len(), HEAD);
        assert!(r.tail.len() <= TAIL + (TAIL >> 2));
        assert_eq!(r.head.len() as u64 + r.tail.len() as u64 + r.skipped, 40 * (256 << 10));
    }

    #[test]
    fn edits_exactly() {
        assert_eq!(replace("a b a", "b", "c", false).unwrap(), ("a c a".into(), 1));
        assert!(replace("a b a", "a", "c", false).unwrap_err().contains("2 times"));
        assert_eq!(replace("a b a", "a", "c", true).unwrap(), ("c b c".into(), 2));
        assert!(replace("x", "y", "z", false).is_err());
        assert_eq!(replace("one\r\ntwo\r\n", "one\ntwo", "1\n2", false).unwrap(), ("1\r\n2\r\n".into(), 1));
    }

    #[test]
    fn slices_files() {
        assert_eq!(slice("a\nb\nc\n", 1, 10), "a\nb\nc\n");
        assert_eq!(slice("a\nb\nc\n", 2, 1), "b\n[lines 2-2 of 3]");
        assert_eq!(slice("a\n", 5, 1), "[the file has 1 lines]");
    }

    #[test]
    fn picks_servers() {
        let mk = |name: &str, group: &str, ai: bool| Server { name: name.into(), group: group.into(), host: format!("{name}.example"), ai, ..Server::new() };
        let cfg = Config { servers: vec![mk("web1", "prod", true), mk("web2", "prod", true), mk("db", "", false)], ..Default::default() };
        let names = |on: &str| pick(&cfg, on).map(|v| v.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
        assert_eq!(names("all").unwrap(), ["web1", "web2"]);
        assert_eq!(names("PROD").unwrap(), ["web1", "web2"]);
        assert_eq!(names("web2, web1, web2").unwrap(), ["web2", "web1"]);
        assert_eq!(names("web1.example").unwrap(), ["web1"]);
        assert!(names("db").unwrap_err().contains("not shared"));
        assert!(names("nope").unwrap_err().contains("web1, web2"));
        assert!(one(&cfg, "prod").is_err());
    }

    #[test]
    fn adds_itself_to_codex() {
        let exe = std::path::Path::new(r"C:\Program Files\haunt\haunt.exe");
        let fresh = codex_config("", exe);
        assert_eq!(fresh, "[mcp_servers.haunt]\ncommand = 'C:\\Program Files\\haunt\\haunt.exe'\nargs = [\"mcp\"]\n");
        let t: toml::Table = fresh.parse().unwrap();
        assert_eq!(t["mcp_servers"]["haunt"]["command"].as_str(), Some(r"C:\Program Files\haunt\haunt.exe"));
        // An older entry and its env table go; everything else stays.
        let old = "model = \"o3\"\n\n[mcp_servers.haunt]\ncommand = 'old'\n\n[mcp_servers.haunt.env]\nX = '1'\n\n[mcp_servers.other]\ncommand = 'x'\n";
        let new = codex_config(old, exe);
        assert_eq!(new.matches("[mcp_servers.haunt]").count(), 1);
        assert!(!new.contains("'old'") && !new.contains("X = ") && new.contains("[mcp_servers.other]") && new.starts_with("model"));
        let t: toml::Table = new.parse().unwrap();
        assert_eq!(t["mcp_servers"]["haunt"]["args"][0].as_str(), Some("mcp"));
    }

    /// The whole server against a real one (the first in the config, shared or not):
    /// `HAUNT_CONFIG_DIR=<dir> cargo test live_mcp -- --ignored`.
    #[test]
    #[ignore]
    fn live_mcp() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let hub = Hub { rt: rt.handle().clone(), ctx: egui::Context::default(), conns: Mutex::new(HashMap::new()) };
        let (mut cfg, err) = Config::load();
        assert!(err.is_none(), "{err:?}");
        cfg.servers.truncate(1);
        cfg.servers[0].ai = true;
        let s = cfg.servers[0].clone();
        rt.block_on(async {
            let r = hub.run(&cfg, &s, "echo hi; echo oops >&2; exit 4", Duration::from_secs(20)).await;
            assert!(r.end == End::Exit(Some(4), None), "{}", r.end);
            let mut lines: Vec<String> = r.text().lines().map(String::from).collect();
            lines.sort();
            assert_eq!(lines, ["hi", "oops"]);

            let r = hub.run(&cfg, &s, "sleep 30", Duration::from_secs(1)).await;
            assert!(r.end == End::Timeout(1), "{}", r.end);
            let r = hub.run(&cfg, &s, "sleep 0.5; pgrep -x sleep -a | grep -c 'sleep 30' || true", Duration::from_secs(20)).await;
            assert_eq!(r.text().trim(), "0", "the timed out command still runs");

            let sftp = hub.sftp(&cfg, &s).await.unwrap();
            let path = "~/.haunt-mcp-test.txt";
            put(&sftp, path, b"alpha\r\nbeta\r\n").await.unwrap();
            let body = String::from_utf8(sftp.read(remote(path)).await.unwrap()).unwrap();
            let (body, n) = replace(&body, "alpha\nbeta", "gamma", false).unwrap();
            assert_eq!((body.as_str(), n), ("gamma\r\n", 1));
            put(&sftp, path, body.as_bytes()).await.unwrap();
            let r = hub.run(&cfg, &s, "cat ~/.haunt-mcp-test.txt; rm ~/.haunt-mcp-test.txt", Duration::from_secs(20)).await;
            assert_eq!(r.text(), "gamma\n");
            // The same connection served all of it.
            assert_eq!(hub.conns.lock().await.len(), 1);
        });
    }
}
