//! The file manager: FileZilla's two panes inside a tab. This PC on the left, the server on
//! the right, a transfer queue below.
//!
//! The server side is SFTP over a second channel of the tab's own SSH connection: no second
//! login, and it goes through the same proxies and jump hosts. After a reconnect the channel
//! is opened again by itself.
//!
//! Nothing here blocks the UI thread. Listings, transfers and file operations run on the tokio
//! runtime and report back through an inbox that the next frame picks up.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant, UNIX_EPOCH};

use eframe::egui::{self, Align2, Color32, CornerRadius, CursorIcon, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, UiBuilder, pos2, vec2};
use parking_lot::Mutex;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use crate::anim;
use crate::fonts::{self, icon};
use crate::fx;
use crate::platform;
use crate::session::{Session, SftpSession, Status};
use crate::theme::{Theme, mix};
use crate::tr;
use crate::widgets;

const ROW_H: f32 = 30.0;
const CHUNK: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Side {
    Local,
    Remote,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Local => Side::Remote,
            Side::Remote => Side::Local,
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    dir: bool,
    link: bool,
    hidden: bool,
    size: u64,
    modified: Option<i64>,
    perms: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Modified,
}

// ---------------------------------------------------------------- paths

/// Local paths are native; the empty path is the list of drives (Windows).
fn local_join(path: &str, name: &str) -> String {
    if path.is_empty() {
        return format!("{name}\\");
    }
    Path::new(path).join(name).to_string_lossy().into_owned()
}

fn local_parent(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => Some(p.to_string_lossy().into_owned()),
        // "C:\" has no parent: show the drives.
        _ if cfg!(windows) => Some(String::new()),
        _ => None,
    }
}

fn remote_join(path: &str, name: &str) -> String {
    if path.ends_with('/') { format!("{path}{name}") } else { format!("{path}/{name}") }
}

fn remote_parent(path: &str) -> Option<String> {
    let t = path.trim_end_matches('/');
    if t.is_empty() {
        return None;
    }
    match t.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(t[..i].to_string()),
        None => None,
    }
}

fn last_component(path: &str) -> Option<String> {
    let t = path.trim_end_matches(['/', '\\']);
    let name = t.rsplit(['/', '\\']).next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// A name the user typed for a new file or folder.
fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n != "." && n != ".." && !n.contains(['/', '\\'])
}

// ---------------------------------------------------------------- errors

pub(crate) fn io_err(e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => tr!("no such file or folder").into(),
        std::io::ErrorKind::PermissionDenied => tr!("permission denied").into(),
        _ => e.to_string(),
    }
}

pub(crate) fn sftp_err(e: SftpError) -> String {
    match e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => tr!("no such file or folder").into(),
            StatusCode::PermissionDenied => tr!("permission denied").into(),
            _ if !s.error_message.is_empty() => s.error_message,
            c => c.to_string(),
        },
        SftpError::Timeout => tr!("the server did not answer in time").into(),
        e => e.to_string(),
    }
}

fn cancelled() -> String {
    tr!("cancelled").into()
}

// ---------------------------------------------------------------- formatting

/// "4,2 MB" (the decimal mark follows the interface language).
pub fn human_size(n: u64) -> String {
    if n < 1024 {
        return tr!("{n} B", n = n);
    }
    let units = [tr!("KB"), tr!("MB"), tr!("GB"), tr!("TB")];
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    let num = if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
    let num = if crate::i18n::current().decimal_comma() { num.replace('.', ",") } else { num };
    format!("{num} {}", units[i])
}

fn rights(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn duration_short(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s < 60 {
        tr!("{n} s", n = s)
    } else if s < 3600 {
        tr!("{m} min {s} s", m = s / 60, s = s % 60)
    } else {
        tr!("{h} h {m} min", h = s / 3600, m = s % 3600 / 60)
    }
}

/// Shortens `text` to `max_w`, cutting the end (or the start, for paths) with an ellipsis.
fn fit_text(p: &egui::Painter, text: &str, font: &egui::FontId, max_w: f32, keep_end: bool) -> String {
    let width = |s: &str| p.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    let cut = |k: usize| -> String {
        if keep_end {
            format!("…{}", chars[chars.len() - k..].iter().collect::<String>())
        } else {
            format!("{}…", chars[..k].iter().collect::<String>())
        }
    };
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max_w { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

// ---------------------------------------------------------------- listing

fn system_time(t: std::time::SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

#[cfg(windows)]
fn hidden_local(name: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0 || name.starts_with('.')
}

#[cfg(not(windows))]
fn hidden_local(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

fn list_local(path: &str) -> Result<Vec<Entry>, String> {
    if path.is_empty() {
        // The drives, like "This PC".
        return Ok((b'A'..=b'Z')
            .map(|c| format!("{}:", c as char))
            .filter(|d| Path::new(&format!("{d}\\")).exists())
            .map(|name| Entry { name, dir: true, link: false, hidden: false, size: 0, modified: None, perms: None })
            .collect());
    }
    let mut out = Vec::new();
    for e in std::fs::read_dir(path).map_err(io_err)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(lmeta) = e.metadata() else { continue };
        let link = lmeta.file_type().is_symlink();
        let meta = if link { std::fs::metadata(e.path()).unwrap_or(lmeta) } else { lmeta };
        let dir = meta.is_dir();
        out.push(Entry {
            hidden: hidden_local(&name, &meta),
            name,
            dir,
            link,
            size: if dir { 0 } else { meta.len() },
            modified: meta.modified().ok().and_then(system_time),
            perms: None,
        });
    }
    Ok(out)
}

async fn list_remote(sftp: &SftpSession, path: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for e in sftp.read_dir(path).await.map_err(sftp_err)? {
        let name = e.file_name();
        let m = e.metadata();
        let link = m.file_type().is_symlink();
        let (mut dir, mut size) = (m.file_type().is_dir(), m.size.unwrap_or(0));
        if link && let Ok(target) = sftp.metadata(remote_join(path, &name)).await {
            dir = target.file_type().is_dir();
            size = target.size.unwrap_or(0);
        }
        out.push(Entry {
            hidden: name.starts_with('.'),
            name,
            dir,
            link,
            size: if dir { 0 } else { size },
            modified: m.mtime.map(i64::from),
            perms: m.permissions,
        });
    }
    Ok(out)
}

/// One file or folder inside a folder being copied, relative to it ("a/b.txt").
struct Item {
    rel: String,
    dir: bool,
    size: u64,
}

/// Everything under `root`, parents before their contents. Links to folders are skipped
/// (they can loop); links to files are copied as files.
fn walk_local(root: &Path) -> Result<Vec<Item>, String> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(&rel) };
        let mut entries: Vec<_> = std::fs::read_dir(&dir).map_err(io_err)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                if let Ok(m) = std::fs::metadata(e.path())
                    && m.is_file()
                {
                    out.push(Item { rel: r, dir: false, size: m.len() });
                }
            } else if ft.is_dir() {
                out.push(Item { rel: r.clone(), dir: true, size: 0 });
                stack.push(r);
            } else {
                out.push(Item { rel: r, dir: false, size: e.metadata().map(|m| m.len()).unwrap_or(0) });
            }
        }
    }
    Ok(out)
}

/// The remote twin of [`walk_local`]. With `follow`, links to files count as files (for
/// downloads); without it links are listed as plain entries (for deleting the link itself).
async fn walk_remote(sftp: &SftpSession, root: &str, follow: bool) -> Result<Vec<Item>, String> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        let dir = if rel.is_empty() { root.to_string() } else { remote_join(root, &rel) };
        let mut entries: Vec<_> = sftp.read_dir(&dir).await.map_err(sftp_err)?.collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let m = e.metadata();
            let ft = m.file_type();
            if ft.is_symlink() {
                if !follow {
                    out.push(Item { rel: r, dir: false, size: 0 });
                } else if let Ok(t) = sftp.metadata(remote_join(&dir, &name)).await
                    && !t.file_type().is_dir()
                {
                    out.push(Item { rel: r, dir: false, size: t.size.unwrap_or(0) });
                }
            } else if ft.is_dir() {
                out.push(Item { rel: r.clone(), dir: true, size: 0 });
                stack.push(r);
            } else {
                out.push(Item { rel: r, dir: false, size: m.size.unwrap_or(0) });
            }
        }
    }
    Ok(out)
}

fn local_path_of(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel.split('/'));
    p
}

// ---------------------------------------------------------------- transfers

#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Up,
    Down,
}

#[derive(Clone, PartialEq)]
enum JobState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Default)]
struct Progress {
    total: AtomicU64,
    done: AtomicU64,
    cancel: AtomicBool,
}

struct Job {
    id: u64,
    way: Way,
    name: String,
    src: String,
    dst: String,
    dir: bool,
    progress: Arc<Progress>,
    state: JobState,
    started: Option<Instant>,
    ended: Option<Instant>,
    /// Smoothed bytes per second, updated by the UI.
    speed: f64,
    sample: Option<(Instant, u64)>,
}

#[derive(Clone)]
struct Plan {
    way: Way,
    name: String,
    src: String,
    dst: String,
    dir: bool,
}

async fn ensure_remote_dir(sftp: &SftpSession, path: &str) -> Result<(), String> {
    match sftp.metadata(path).await {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(tr!("{name} exists and is not a folder", name = path)),
        Err(_) => sftp.create_dir(path).await.map_err(sftp_err),
    }
}

async fn upload_file(sftp: &SftpSession, src: &Path, dst: &str, p: &Progress) -> Result<(), String> {
    let mut input = tokio::fs::File::open(src).await.map_err(io_err)?;
    let mut out = sftp.create(dst).await.map_err(sftp_err)?;
    let mut buf = vec![0u8; CHUNK];
    let copied: Result<(), String> = async {
        loop {
            if p.cancel.load(Relaxed) {
                return Err(cancelled());
            }
            let n = input.read(&mut buf).await.map_err(io_err)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await.map_err(io_err)?;
            p.done.fetch_add(n as u64, Relaxed);
        }
        out.shutdown().await.map_err(io_err)
    }
    .await;
    if copied.is_err() {
        // A half-written file is worse than none.
        drop(out);
        let _ = sftp.remove_file(dst).await;
    }
    copied
}

async fn download_file(sftp: &SftpSession, src: &str, dst: &Path, p: &Progress) -> Result<(), String> {
    let mut input = sftp.open(src).await.map_err(sftp_err)?;
    let mut out = tokio::fs::File::create(dst).await.map_err(io_err)?;
    let mut buf = vec![0u8; CHUNK];
    let copied: Result<(), String> = async {
        loop {
            if p.cancel.load(Relaxed) {
                return Err(cancelled());
            }
            let n = input.read(&mut buf).await.map_err(io_err)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await.map_err(io_err)?;
            p.done.fetch_add(n as u64, Relaxed);
        }
        out.flush().await.map_err(io_err)
    }
    .await;
    if copied.is_err() {
        drop(out);
        let _ = tokio::fs::remove_file(dst).await;
    }
    copied
}

async fn upload(sftp: &SftpSession, src: &str, dst: &str, dir: bool, p: &Progress) -> Result<(), String> {
    let root = PathBuf::from(src);
    if !dir {
        let size = tokio::fs::metadata(&root).await.map_err(io_err)?.len();
        p.total.store(size, Relaxed);
        return upload_file(sftp, &root, dst, p).await;
    }
    let walk_root = root.clone();
    let items = tokio::task::spawn_blocking(move || walk_local(&walk_root)).await.map_err(|e| e.to_string())??;
    p.total.store(items.iter().map(|i| i.size).sum(), Relaxed);
    ensure_remote_dir(sftp, dst).await?;
    for it in items {
        if p.cancel.load(Relaxed) {
            return Err(cancelled());
        }
        let target = remote_join(dst, &it.rel);
        if it.dir {
            ensure_remote_dir(sftp, &target).await?;
        } else {
            upload_file(sftp, &local_path_of(&root, &it.rel), &target, p).await?;
        }
    }
    Ok(())
}

async fn download(sftp: &SftpSession, src: &str, dst: &str, dir: bool, p: &Progress) -> Result<(), String> {
    let root = PathBuf::from(dst);
    if !dir {
        let size = sftp.metadata(src).await.map_err(sftp_err)?.size.unwrap_or(0);
        p.total.store(size, Relaxed);
        return download_file(sftp, src, &root, p).await;
    }
    let items = walk_remote(sftp, src, true).await?;
    p.total.store(items.iter().map(|i| i.size).sum(), Relaxed);
    tokio::fs::create_dir_all(&root).await.map_err(io_err)?;
    for it in items {
        if p.cancel.load(Relaxed) {
            return Err(cancelled());
        }
        let target = local_path_of(&root, &it.rel);
        if it.dir {
            tokio::fs::create_dir_all(&target).await.map_err(io_err)?;
        } else {
            download_file(sftp, &remote_join(src, &it.rel), &target, p).await?;
        }
    }
    Ok(())
}

async fn remove_remote(sftp: &SftpSession, path: &str, dir: bool) -> Result<(), String> {
    if !dir {
        return sftp.remove_file(path).await.map_err(sftp_err);
    }
    let items = walk_remote(sftp, path, false).await?;
    for it in items.iter().filter(|i| !i.dir) {
        sftp.remove_file(remote_join(path, &it.rel)).await.map_err(sftp_err)?;
    }
    // Deepest folders first.
    for it in items.iter().rev().filter(|i| i.dir) {
        sftp.remove_dir(remote_join(path, &it.rel)).await.map_err(sftp_err)?;
    }
    sftp.remove_dir(path).await.map_err(sftp_err)
}

// ---------------------------------------------------------------- model

struct Pane {
    side: Side,
    path: String,
    entries: Vec<Entry>,
    /// Indices into `entries` in display order (sorted, hidden files filtered out).
    view: Vec<usize>,
    selected: BTreeSet<String>,
    cursor: Option<String>,
    anchor: Option<String>,
    loading: bool,
    listed: bool,
    edit: Option<String>,
    edit_focus: bool,
    sort: SortKey,
    asc: bool,
    show_hidden: bool,
    /// Id of the newest listing request; older answers are dropped.
    request: u64,
    /// Scroll the cursor into view next frame.
    reveal: bool,
    scroll_id: Option<Id>,
}

impl Pane {
    fn new(side: Side, path: String, show_hidden: bool) -> Self {
        Self {
            side,
            path,
            entries: Vec::new(),
            view: Vec::new(),
            selected: BTreeSet::new(),
            cursor: None,
            anchor: None,
            loading: false,
            listed: false,
            edit: None,
            edit_focus: false,
            sort: SortKey::Name,
            asc: true,
            show_hidden,
            request: 0,
            reveal: false,
            scroll_id: None,
        }
    }

    fn join(&self, name: &str) -> String {
        match self.side {
            Side::Local => local_join(&self.path, name),
            Side::Remote => remote_join(&self.path, name),
        }
    }

    fn parent(&self) -> Option<String> {
        match self.side {
            Side::Local => local_parent(&self.path),
            Side::Remote => remote_parent(&self.path),
        }
    }

    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    fn rebuild_view(&mut self) {
        let mut v: Vec<usize> = (0..self.entries.len()).filter(|&i| self.show_hidden || !self.entries[i].hidden).collect();
        let e = &self.entries;
        let (key, asc) = (self.sort, self.asc);
        v.sort_by(|&a, &b| {
            let (a, b) = (&e[a], &e[b]);
            // Folders always first, whatever the order.
            b.dir.cmp(&a.dir).then_with(|| {
                let o = match key {
                    SortKey::Name => natural_cmp(&a.name, &b.name),
                    SortKey::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
                    SortKey::Modified => a.modified.cmp(&b.modified).then_with(|| natural_cmp(&a.name, &b.name)),
                };
                if asc { o } else { o.reverse() }
            })
        });
        self.view = v;
        // Hidden files that just disappeared must not stay selected.
        let visible: BTreeSet<&str> = self.view.iter().map(|&i| self.entries[i].name.as_str()).collect();
        self.selected.retain(|n| visible.contains(n.as_str()));
        if self.cursor.as_deref().is_some_and(|c| !visible.contains(c)) {
            self.cursor = None;
        }
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.view.iter().position(|&i| self.entries[i].name == name)
    }

    /// The selection in display order, or the cursor row when nothing is selected.
    fn targets(&self) -> Vec<String> {
        let sel: Vec<String> =
            self.view.iter().map(|&i| &self.entries[i].name).filter(|n| self.selected.contains(*n)).cloned().collect();
        if sel.is_empty() { self.cursor.iter().cloned().collect() } else { sel }
    }

    fn select_only(&mut self, name: &str) {
        self.selected.clear();
        self.selected.insert(name.to_string());
        self.cursor = Some(name.to_string());
        self.anchor = Some(name.to_string());
    }

    fn select_range(&mut self, to: &str) {
        let from = self.anchor.clone().or_else(|| self.cursor.clone()).unwrap_or_else(|| to.to_string());
        let (Some(a), Some(b)) = (self.position(&from), self.position(to)) else {
            self.select_only(to);
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        self.selected = self.view[lo..=hi].iter().map(|&i| self.entries[i].name.clone()).collect();
        self.cursor = Some(to.to_string());
    }
}

/// "file2" before "file10"; case-insensitive.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

enum Msg {
    Listed { side: Side, request: u64, path: String, result: Result<Vec<Entry>, String>, select: Option<String> },
    Home(Result<String, String>),
    /// A file operation finished: show the error, if any, and refresh that side.
    Changed { side: Side, error: Option<String>, select: Option<String> },
}

enum Conn {
    Waiting,
    Opening(oneshot::Receiver<Result<SftpSession, String>>),
    Ready,
    Failed(String),
}

enum Dialog {
    Rename { side: Side, old: String, name: String, focus: bool },
    NewFolder { side: Side, name: String, focus: bool },
    Delete { side: Side, names: Vec<String> },
    Replace { plan: Vec<Plan>, existing: Vec<String> },
}

enum Act {
    Focus(Side),
    Open(Side, String),
    Navigate(Side, String),
    Up(Side),
    Refresh(Side),
    Transfer(Side, Vec<String>),
    UploadPaths(Vec<PathBuf>),
    AskDelete(Side, Vec<String>),
    AskRename(Side, String),
    AskNewFolder(Side),
    ToggleHidden(Side),
    Sort(Side, SortKey),
    CopyPath(Side, String),
    OpenLocal(String),
    Cancel(u64),
    ClearFinished,
}

/// What is being dragged between the panes.
struct DragFiles {
    side: Side,
    names: Vec<String>,
}

pub struct Look<'a> {
    pub theme: &'a Theme,
    pub pane_alpha: f32,
    pub radius: f32,
    pub gaps: f32,
    pub server: &'a str,
}

pub struct Files {
    id: u64,
    local: Pane,
    remote: Pane,
    focus: Side,
    conn: Conn,
    /// `Session::connects` the current SFTP channel belongs to.
    conn_gen: u64,
    sftp: Arc<Mutex<Option<Arc<SftpSession>>>>,
    inbox: Arc<Mutex<Vec<Msg>>>,
    jobs: Arc<Mutex<Vec<Job>>>,
    worker: Arc<AtomicBool>,
    next_job: u64,
    dialog: Option<Dialog>,
    dialog_at: f64,
    notice: Option<(String, f64)>,
    started: bool,
    /// Debug builds: HAUNT_PREVIEW_UPLOAD=<path> uploads that once the server side is listed.
    #[cfg(debug_assertions)]
    preview_upload: Option<PathBuf>,
}

impl Files {
    pub fn new(id: u64) -> Self {
        let home = directories::UserDirs::new().map(|u| u.home_dir().to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            id,
            local: Pane::new(Side::Local, home, false),
            // Dotfiles matter on a server (.env, .bashrc); they are shown there by default.
            remote: Pane::new(Side::Remote, String::new(), true),
            focus: Side::Remote,
            conn: Conn::Waiting,
            conn_gen: 0,
            sftp: Arc::new(Mutex::new(None)),
            inbox: Arc::new(Mutex::new(Vec::new())),
            jobs: Arc::new(Mutex::new(Vec::new())),
            worker: Arc::new(AtomicBool::new(false)),
            next_job: 1,
            dialog: None,
            dialog_at: 0.0,
            notice: None,
            started: false,
            #[cfg(debug_assertions)]
            preview_upload: std::env::var_os("HAUNT_PREVIEW_UPLOAD").map(PathBuf::from),
        }
    }

    /// True while a dialog of the file manager is open (the app treats it as modal).
    pub fn modal_open(&self) -> bool {
        self.dialog.is_some()
    }

    fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Local => &self.local,
            Side::Remote => &self.remote,
        }
    }

    fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    fn current_sftp(&self) -> Option<Arc<SftpSession>> {
        self.sftp.lock().clone()
    }

    fn post(inbox: &Arc<Mutex<Vec<Msg>>>, ctx: &egui::Context, msg: Msg) {
        inbox.lock().push(msg);
        ctx.request_repaint();
    }

    fn say(&mut self, text: impl Into<String>, now: f64) {
        self.notice = Some((text.into(), now));
    }

    // ------------------------------------------------------------ background work

    /// Lists `path` on `side`. The pane keeps showing its current folder until the new one
    /// arrives, so a failing folder (no permission) leaves you where you were.
    fn list(&mut self, side: Side, path: String, select: Option<String>, rt: &Handle, ctx: &egui::Context) {
        let sftp = self.current_sftp();
        let (inbox, ctx) = (self.inbox.clone(), ctx.clone());
        let pane = self.pane_mut(side);
        pane.request += 1;
        pane.loading = true;
        let request = pane.request;
        match side {
            Side::Local => {
                rt.spawn_blocking(move || {
                    let result = list_local(&path);
                    Self::post(&inbox, &ctx, Msg::Listed { side, request, path, result, select });
                });
            }
            Side::Remote => {
                let Some(sftp) = sftp else {
                    pane.loading = false;
                    return;
                };
                rt.spawn(async move {
                    let result = list_remote(&sftp, &path).await;
                    Self::post(&inbox, &ctx, Msg::Listed { side, request, path, result, select });
                });
            }
        }
    }

    fn refresh(&mut self, side: Side, select: Option<String>, rt: &Handle, ctx: &egui::Context) {
        let pane = self.pane(side);
        if side == Side::Remote && pane.path.is_empty() {
            return;
        }
        let select = select.or_else(|| pane.cursor.clone());
        self.list(side, pane.path.clone(), select, rt, ctx);
    }

    /// Connection upkeep and the inbox.
    fn pump(&mut self, s: &Session, rt: &Handle, ctx: &egui::Context, now: f64) {
        let connected = s.status() == Status::Connected;
        let generation = s.connects();
        if connected && generation != self.conn_gen {
            // A new connection (first one, or after a reconnect): the old channel is gone.
            self.conn_gen = generation;
            *self.sftp.lock() = None;
            self.conn = match s.open_sftp() {
                Some(rx) => Conn::Opening(rx),
                None => Conn::Waiting,
            };
        }
        if !connected && matches!(self.conn, Conn::Ready) {
            self.conn = Conn::Waiting;
        }
        if let Conn::Opening(rx) = &mut self.conn {
            match rx.try_recv() {
                Ok(Ok(sftp)) => {
                    let sftp = Arc::new(sftp);
                    *self.sftp.lock() = Some(sftp.clone());
                    self.conn = Conn::Ready;
                    #[cfg(debug_assertions)]
                    if self.remote.path.is_empty()
                        && let Ok(p) = std::env::var("HAUNT_PREVIEW_REMOTE")
                    {
                        self.remote.path = p;
                    }
                    if self.remote.path.is_empty() {
                        let (inbox, ctx2) = (self.inbox.clone(), ctx.clone());
                        rt.spawn(async move {
                            let home = sftp.canonicalize(".").await.map_err(sftp_err);
                            Self::post(&inbox, &ctx2, Msg::Home(home));
                        });
                    } else {
                        self.refresh(Side::Remote, None, rt, ctx);
                    }
                }
                Ok(Err(e)) => self.conn = Conn::Failed(e),
                Err(oneshot::error::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(80)),
                Err(oneshot::error::TryRecvError::Closed) => self.conn = Conn::Waiting,
            }
        }

        let msgs = std::mem::take(&mut *self.inbox.lock());
        for m in msgs {
            match m {
                Msg::Listed { side, request, path, result, select } => {
                    let pane = self.pane_mut(side);
                    if request != pane.request {
                        continue;
                    }
                    pane.loading = false;
                    match result {
                        Ok(entries) => {
                            let moved = pane.path != path || !pane.listed;
                            pane.path = path;
                            pane.entries = entries;
                            pane.listed = true;
                            if moved {
                                pane.selected.clear();
                                pane.cursor = None;
                                pane.anchor = None;
                            }
                            pane.rebuild_view();
                            if let Some(sel) = select
                                && pane.position(&sel).is_some()
                            {
                                pane.cursor = Some(sel.clone());
                                pane.anchor = Some(sel);
                                pane.reveal = true;
                            }
                        }
                        Err(e) => {
                            let what = if path.is_empty() { tr!("This PC").to_string() } else { path };
                            self.say(tr!("Cannot open {path}: {e}", path = what, e = e), now);
                        }
                    }
                }
                Msg::Home(Ok(home)) => self.list(Side::Remote, home, None, rt, ctx),
                Msg::Home(Err(e)) => {
                    // No home (chrooted or odd server): start at the root.
                    self.say(e, now);
                    self.list(Side::Remote, "/".into(), None, rt, ctx);
                }
                Msg::Changed { side, error, select } => {
                    if let Some(e) = error {
                        self.say(e, now);
                    }
                    self.refresh(side, select, rt, ctx);
                }
            }
        }
    }

    /// Starts the transfer worker unless it is already running.
    fn kick(&self, rt: &Handle, ctx: &egui::Context) {
        if self.worker.swap(true, Relaxed) {
            return;
        }
        let (jobs, worker, inbox, sftp, ctx) = (self.jobs.clone(), self.worker.clone(), self.inbox.clone(), self.sftp.clone(), ctx.clone());
        rt.spawn(async move {
            loop {
                loop {
                    let next = {
                        let mut js = jobs.lock();
                        js.iter_mut().find(|j| j.state == JobState::Queued).map(|j| {
                            j.state = JobState::Running;
                            j.started = Some(Instant::now());
                            (j.id, j.way, j.src.clone(), j.dst.clone(), j.dir, j.progress.clone())
                        })
                    };
                    let Some((id, way, src, dst, dir, progress)) = next else { break };
                    ctx.request_repaint();
                    let current = sftp.lock().clone();
                    let result = match current {
                        None => Err(tr!("not connected").to_string()),
                        Some(sftp) => match way {
                            Way::Up => upload(&sftp, &src, &dst, dir, &progress).await,
                            Way::Down => download(&sftp, &src, &dst, dir, &progress).await,
                        },
                    };
                    let state = match result {
                        _ if progress.cancel.load(Relaxed) => JobState::Cancelled,
                        Ok(()) => JobState::Done,
                        Err(e) => JobState::Failed(e),
                    };
                    if let Some(j) = jobs.lock().iter_mut().find(|j| j.id == id) {
                        j.state = state;
                        j.ended = Some(Instant::now());
                    }
                    let side = if way == Way::Up { Side::Remote } else { Side::Local };
                    let select = last_component(&dst);
                    Self::post(&inbox, &ctx, Msg::Changed { side, error: None, select });
                }
                worker.store(false, Relaxed);
                // A job queued between the last look and now would be stranded otherwise.
                let more = jobs.lock().iter().any(|j| j.state == JobState::Queued);
                if !more || worker.swap(true, Relaxed) {
                    break;
                }
            }
        });
    }

    fn enqueue(&mut self, plan: Vec<Plan>, rt: &Handle, ctx: &egui::Context) {
        if plan.is_empty() {
            return;
        }
        {
            let mut jobs = self.jobs.lock();
            for p in plan {
                jobs.push(Job {
                    id: self.next_job,
                    way: p.way,
                    name: p.name,
                    src: p.src,
                    dst: p.dst,
                    dir: p.dir,
                    progress: Arc::new(Progress::default()),
                    state: JobState::Queued,
                    started: None,
                    ended: None,
                    speed: 0.0,
                    sample: None,
                });
                self.next_job += 1;
            }
        }
        self.kick(rt, ctx);
    }

    /// Plans copying `names` from `from` into the other pane's folder, asking first when
    /// some of them already exist there.
    fn transfer(&mut self, from: Side, names: Vec<String>, rt: &Handle, ctx: &egui::Context, now: f64) {
        if names.is_empty() {
            return;
        }
        if !matches!(self.conn, Conn::Ready) {
            self.say(tr!("The server side is not ready yet"), now);
            return;
        }
        let (src, dst) = (self.pane(from), self.pane(from.other()));
        if src.path.is_empty() || dst.path.is_empty() {
            self.say(tr!("Open a folder on this PC first"), now);
            return;
        }
        let way = if from == Side::Local { Way::Up } else { Way::Down };
        let plan: Vec<Plan> = names
            .iter()
            .filter_map(|n| src.entry(n))
            .map(|e| Plan { way, name: e.name.clone(), src: src.join(&e.name), dst: dst.join(&e.name), dir: e.dir })
            .collect();
        self.plan_or_ask(plan, rt, ctx, now);
    }

    fn plan_or_ask(&mut self, plan: Vec<Plan>, rt: &Handle, ctx: &egui::Context, now: f64) {
        let Some(first) = plan.first() else { return };
        let dst = self.pane(if first.way == Way::Up { Side::Remote } else { Side::Local });
        let existing: Vec<String> = plan.iter().filter(|p| dst.entry(&p.name).is_some()).map(|p| p.name.clone()).collect();
        if existing.is_empty() {
            self.enqueue(plan, rt, ctx);
        } else {
            self.dialog = Some(Dialog::Replace { plan, existing });
            self.dialog_at = now;
        }
    }

    fn upload_paths(&mut self, paths: Vec<PathBuf>, rt: &Handle, ctx: &egui::Context, now: f64) {
        if !matches!(self.conn, Conn::Ready) || self.remote.path.is_empty() {
            self.say(tr!("The server side is not ready yet"), now);
            return;
        }
        let plan = paths
            .into_iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                Some(Plan { way: Way::Up, dst: self.remote.join(&name), dir: p.is_dir(), src: p.to_string_lossy().into_owned(), name })
            })
            .collect();
        self.plan_or_ask(plan, rt, ctx, now);
    }

    fn open(&mut self, side: Side, name: &str, rt: &Handle, ctx: &egui::Context, now: f64) {
        let pane = self.pane(side);
        let Some(e) = pane.entry(name) else { return };
        if e.dir {
            let path = pane.join(name);
            self.list(side, path, None, rt, ctx);
        } else {
            self.transfer(side, vec![name.to_string()], rt, ctx, now);
        }
    }

    fn up(&mut self, side: Side, rt: &Handle, ctx: &egui::Context) {
        let pane = self.pane(side);
        if let Some(parent) = pane.parent() {
            // Land on the folder we came from.
            let from = last_component(&pane.path);
            self.list(side, parent, from, rt, ctx);
        }
    }

    fn rename(&mut self, side: Side, old: String, new: String, rt: &Handle, ctx: &egui::Context) {
        let pane = self.pane(side);
        let (from, to) = (pane.join(&old), pane.join(new.trim()));
        let select = Some(new.trim().to_string());
        let (inbox, ctx) = (self.inbox.clone(), ctx.clone());
        match side {
            Side::Local => {
                rt.spawn_blocking(move || {
                    let error = std::fs::rename(&from, &to).err().map(io_err);
                    Self::post(&inbox, &ctx, Msg::Changed { side, error, select });
                });
            }
            Side::Remote => {
                let Some(sftp) = self.current_sftp() else { return };
                rt.spawn(async move {
                    let error = sftp.rename(from, to).await.err().map(sftp_err);
                    Self::post(&inbox, &ctx, Msg::Changed { side, error, select });
                });
            }
        }
    }

    fn new_folder(&mut self, side: Side, name: String, rt: &Handle, ctx: &egui::Context) {
        let path = self.pane(side).join(name.trim());
        let select = Some(name.trim().to_string());
        let (inbox, ctx) = (self.inbox.clone(), ctx.clone());
        match side {
            Side::Local => {
                rt.spawn_blocking(move || {
                    let error = std::fs::create_dir(&path).err().map(io_err);
                    Self::post(&inbox, &ctx, Msg::Changed { side, error, select });
                });
            }
            Side::Remote => {
                let Some(sftp) = self.current_sftp() else { return };
                rt.spawn(async move {
                    let error = sftp.create_dir(path).await.err().map(sftp_err);
                    Self::post(&inbox, &ctx, Msg::Changed { side, error, select });
                });
            }
        }
    }

    fn delete(&mut self, side: Side, names: Vec<String>, rt: &Handle, ctx: &egui::Context) {
        let pane = self.pane(side);
        let targets: Vec<(String, bool)> = names.iter().filter_map(|n| pane.entry(n)).map(|e| (pane.join(&e.name), e.dir && !e.link)).collect();
        let (inbox, ctx) = (self.inbox.clone(), ctx.clone());
        match side {
            Side::Local => {
                rt.spawn_blocking(move || {
                    let paths: Vec<PathBuf> = targets.into_iter().map(|(p, _)| PathBuf::from(p)).collect();
                    let error = platform::to_recycle_bin(&paths).err();
                    Self::post(&inbox, &ctx, Msg::Changed { side, error, select: None });
                });
            }
            Side::Remote => {
                let Some(sftp) = self.current_sftp() else { return };
                rt.spawn(async move {
                    let mut error = None;
                    for (path, dir) in targets {
                        if let Err(e) = remove_remote(&sftp, &path, dir).await {
                            error = Some(e);
                            break;
                        }
                    }
                    Self::post(&inbox, &ctx, Msg::Changed { side, error, select: None });
                });
            }
        }
    }

    fn apply(&mut self, act: Act, rt: &Handle, ctx: &egui::Context, now: f64) {
        match act {
            Act::Focus(side) => self.focus = side,
            Act::Open(side, name) => self.open(side, &name, rt, ctx, now),
            Act::Navigate(side, path) => {
                let path = path.trim().to_string();
                if side == Side::Remote && path.is_empty() {
                    return;
                }
                self.list(side, path, None, rt, ctx);
            }
            Act::Up(side) => self.up(side, rt, ctx),
            Act::Refresh(side) => self.refresh(side, None, rt, ctx),
            Act::Transfer(side, names) => self.transfer(side, names, rt, ctx, now),
            Act::UploadPaths(paths) => self.upload_paths(paths, rt, ctx, now),
            Act::AskDelete(side, names) if !names.is_empty() && !self.pane(side).path.is_empty() => {
                self.dialog = Some(Dialog::Delete { side, names });
                self.dialog_at = now;
            }
            Act::AskRename(side, old) if !self.pane(side).path.is_empty() => {
                self.dialog = Some(Dialog::Rename { side, name: old.clone(), old, focus: true });
                self.dialog_at = now;
            }
            Act::AskNewFolder(side) if !self.pane(side).path.is_empty() && self.pane(side).listed => {
                self.dialog = Some(Dialog::NewFolder { side, name: String::new(), focus: true });
                self.dialog_at = now;
            }
            Act::AskDelete(..) | Act::AskRename(..) | Act::AskNewFolder(_) => {}
            Act::ToggleHidden(side) => {
                let pane = self.pane_mut(side);
                pane.show_hidden = !pane.show_hidden;
                pane.rebuild_view();
            }
            Act::Sort(side, key) => {
                let pane = self.pane_mut(side);
                if pane.sort == key {
                    pane.asc = !pane.asc;
                } else {
                    pane.sort = key;
                    // Newest and biggest first is what one usually looks for.
                    pane.asc = key == SortKey::Name;
                }
                pane.rebuild_view();
            }
            Act::CopyPath(side, name) => {
                let path = self.pane(side).join(&name);
                ctx.copy_text(path);
                self.say(tr!("Path copied"), now);
            }
            Act::OpenLocal(name) => {
                let path = self.local.join(&name);
                platform::open_path(Path::new(&path));
            }
            Act::Cancel(id) => {
                if let Some(j) = self.jobs.lock().iter_mut().find(|j| j.id == id) {
                    j.progress.cancel.store(true, Relaxed);
                    if j.state == JobState::Queued {
                        j.state = JobState::Cancelled;
                        j.ended = Some(Instant::now());
                    }
                }
            }
            Act::ClearFinished => self.jobs.lock().retain(|j| matches!(j.state, JobState::Queued | JobState::Running)),
        }
    }

    // ------------------------------------------------------------ UI

    pub fn show(&mut self, ui: &mut egui::Ui, s: &Session, look: &Look<'_>, rt: &Handle, input_enabled: bool) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        if !self.started {
            self.started = true;
            let path = self.local.path.clone();
            self.list(Side::Local, path, None, rt, &ctx);
        }
        self.pump(s, rt, &ctx, now);
        #[cfg(debug_assertions)]
        if self.remote.listed
            && let Some(p) = self.preview_upload.take()
        {
            self.upload_paths(vec![p], rt, &ctx, now);
        }

        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let gap = look.gaps.clamp(6.0, 24.0);
        let jobs_h = self.transfers_height();
        let panes_bottom = if jobs_h > 0.0 { rect.max.y - jobs_h - gap } else { rect.max.y };
        let half = ((rect.width() - gap) / 2.0).floor();
        let lrect = Rect::from_min_max(rect.min, pos2(rect.min.x + half, panes_bottom));
        let rrect = Rect::from_min_max(pos2(lrect.max.x + gap, rect.min.y), pos2(rect.max.x, panes_bottom));

        let mut acts = Vec::new();
        let overlay = self.remote_overlay(s);
        self.pane_ui(ui, lrect, Side::Local, look, None, &mut acts);
        self.pane_ui(ui, rrect, Side::Remote, look, overlay.as_deref(), &mut acts);
        if jobs_h > 0.0 {
            self.transfers_ui(ui, Rect::from_min_max(pos2(rect.min.x, rect.max.y - jobs_h), rect.max), look, &mut acts);
        }

        if input_enabled && self.dialog.is_none() {
            // Files dropped from Explorer go to the server's folder.
            let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
            if !dropped.is_empty() {
                acts.push(Act::UploadPaths(dropped));
            }
            self.keys(&ctx, &mut acts);
        }
        for a in acts {
            self.apply(a, rt, &ctx, now);
        }
        self.notice_ui(ui, rect, look.theme, now);
        self.dialog_ui(&ctx, look.theme, rt, now);

        let busy = self.local.loading || self.remote.loading || self.jobs.lock().iter().any(|j| matches!(j.state, JobState::Running | JobState::Queued));
        if busy {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn remote_overlay(&self, s: &Session) -> Option<String> {
        match s.status() {
            Status::Connected => {}
            Status::Connecting => return Some(tr!("connecting…").into()),
            Status::Reconnecting(_) => return Some(tr!("connection lost, reconnecting…").into()),
            Status::Closed(_) => return Some(tr!("not connected").into()),
        }
        match &self.conn {
            Conn::Waiting | Conn::Opening(_) => Some(tr!("opening SFTP…").into()),
            Conn::Failed(e) => Some(tr!("SFTP is not available: {e}", e = e)),
            Conn::Ready if !self.remote.listed => Some(tr!("loading…").into()),
            Conn::Ready => None,
        }
    }

    fn keys(&mut self, ctx: &egui::Context, acts: &mut Vec<Act>) {
        if ctx.egui_wants_keyboard_input() || self.local.edit.is_some() || self.remote.edit.is_some() {
            return;
        }
        let side = self.focus;
        let n = Modifiers::NONE;
        let (up, down, s_up, s_down, home, end, enter, back, tab, del, f2, f5, f7, all) = ctx.input_mut(|i| {
            (
                i.consume_key(n, Key::ArrowUp),
                i.consume_key(n, Key::ArrowDown),
                i.consume_key(Modifiers::SHIFT, Key::ArrowUp),
                i.consume_key(Modifiers::SHIFT, Key::ArrowDown),
                i.consume_key(n, Key::Home),
                i.consume_key(n, Key::End),
                i.consume_key(n, Key::Enter),
                i.consume_key(n, Key::Backspace),
                i.consume_key(n, Key::Tab),
                i.consume_key(n, Key::Delete),
                i.consume_key(n, Key::F2),
                i.consume_key(n, Key::F5),
                i.consume_key(n, Key::F7),
                i.consume_key(Modifiers::COMMAND, Key::A),
            )
        });
        if tab {
            acts.push(Act::Focus(side.other()));
            return;
        }
        let pane = self.pane_mut(side);
        let len = pane.view.len();
        let at = pane.cursor.as_deref().and_then(|c| pane.position(c));
        let step = |d: isize| -> Option<usize> {
            if len == 0 {
                return None;
            }
            Some(match at {
                None => 0,
                Some(i) => (i as isize + d).clamp(0, len as isize - 1) as usize,
            })
        };
        let target = if up || s_up {
            step(-1)
        } else if down || s_down {
            step(1)
        } else if home {
            (len > 0).then_some(0)
        } else if end {
            len.checked_sub(1)
        } else {
            None
        };
        if let Some(i) = target {
            let name = pane.entries[pane.view[i]].name.clone();
            if s_up || s_down { pane.select_range(&name) } else { pane.select_only(&name) }
            pane.reveal = true;
        }
        if all {
            pane.selected = pane.view.iter().map(|&i| pane.entries[i].name.clone()).collect();
        }
        if enter {
            let targets = pane.targets();
            match targets.as_slice() {
                [one] if pane.entry(one).is_some_and(|e| e.dir) => acts.push(Act::Open(side, one.clone())),
                [] => {}
                _ => acts.push(Act::Transfer(side, targets)),
            }
        }
        if back {
            acts.push(Act::Up(side));
        }
        if del {
            acts.push(Act::AskDelete(side, pane.targets()));
        }
        if f2 && let Some(c) = pane.cursor.clone() {
            acts.push(Act::AskRename(side, c));
        }
        if f5 {
            acts.push(Act::Refresh(side));
        }
        if f7 {
            acts.push(Act::AskNewFolder(side));
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, rect: Rect, side: Side, look: &Look<'_>, overlay: Option<&str>, acts: &mut Vec<Act>) {
        let t = look.theme;
        let ctx = ui.ctx().clone();
        let base = Id::unique(("files-pane", self.id, side));
        let focused = self.focus == side;
        let ready = side == Side::Local || overlay.is_none();
        let explorer_hover = side == Side::Remote && ctx.input(|i| !i.raw.hovered_files.is_empty());

        // The plate, as the terminal draws it.
        let painter = ui.painter_at(rect.expand(24.0));
        let ft = anim::toggle(&ctx, base.with("focus"), focused, 0.25);
        fx::shadow(&painter, rect, look.radius, 0.7);
        painter.rect_filled(rect, CornerRadius::same(look.radius as u8), t.bg.gamma_multiply(look.pane_alpha));
        let top = fx::mix_alpha(Color32::from_white_alpha(16), t.accent.gamma_multiply(0.75), ft);
        let bottom = fx::mix_alpha(Color32::from_white_alpha(9), t.accent.gamma_multiply(0.12), ft);
        fx::vgradient_stroke(&painter, rect, look.radius, 1.0, top, bottom);

        // Background: focus on click, drop target for rows dragged from the other pane.
        let bg = ui.interact(rect, base.with("bg"), Sense::click());
        if bg.clicked() || bg.secondary_clicked() {
            acts.push(Act::Focus(side));
            self.pane_mut(side).selected.clear();
        }
        let incoming = bg.dnd_hover_payload::<DragFiles>().filter(|d| d.side != side);
        if let Some(d) = bg.dnd_release_payload::<DragFiles>()
            && d.side != side
        {
            acts.push(Act::Transfer(d.side, d.names.clone()));
        }

        let inner = rect.shrink2(vec2(14.0, 12.0));
        let p = ui.painter_at(rect);

        // Title row: where we are, and the folder tools on the right.
        let title_y = inner.min.y + 14.0;
        let (title_icon, title) = match side {
            Side::Local => (icon::DESKTOP, tr!("This PC").to_string()),
            Side::Remote => (icon::SERVER, look.server.to_string()),
        };
        fx::glyph(&p, pos2(inner.min.x + 8.0, title_y), title_icon, fonts::mono(13.0), mix(t.subtext, t.accent, ft));
        let mut bx = inner.max.x;
        let mut tool = |ui: &mut egui::Ui, ic: &str, tip: &str, enabled: bool| -> bool {
            bx -= 28.0;
            let r = Rect::from_min_size(pos2(bx, title_y - 14.0), vec2(28.0, 28.0));
            bx -= 2.0;
            let resp = ui
                .scope_builder(UiBuilder::new().max_rect(r), |ui| {
                    if !enabled {
                        ui.disable();
                    }
                    widgets::icon_button(ui, t, ic, r.size(), false)
                })
                .inner
                .on_hover_text(tip);
            enabled && resp.clicked()
        };
        let pane = self.pane(side);
        let has_parent = pane.parent().is_some() && ready;
        let can_edit = ready && !pane.path.is_empty() && pane.listed;
        let hidden_icon = if pane.show_hidden { icon::EYE } else { icon::EYE_OFF };
        let hidden_tip = if pane.show_hidden { tr!("Hide hidden files") } else { tr!("Show hidden files") };
        if tool(ui, hidden_icon, hidden_tip, true) {
            acts.push(Act::ToggleHidden(side));
        }
        if tool(ui, icon::FOLDER_PLUS, tr!("New folder  (F7)"), can_edit) {
            acts.push(Act::AskNewFolder(side));
        }
        if tool(ui, icon::REFRESH, tr!("Refresh  (F5)"), ready) {
            acts.push(Act::Refresh(side));
        }
        if tool(ui, icon::UP, tr!("Up one level  (Backspace)"), has_parent) {
            acts.push(Act::Up(side));
        }
        let title_font = fonts::bold(13.5);
        let title_w = bx - (inner.min.x + 22.0) - 8.0;
        let title = fit_text(&p, &title, &title_font, title_w, false);
        crate::veil::text_auto(&p, pos2(inner.min.x + 22.0, title_y + fx::text_dy(&p, &title_font)), Align2::LEFT_CENTER, &title, title_font, t.text, t.bg);

        // Path bar: click to type a path.
        let path_r = Rect::from_min_size(pos2(inner.min.x, inner.min.y + 36.0), vec2(inner.width(), 32.0));
        let path_font = fonts::mono(12.5);
        let editing = self.pane(side).edit.is_some();
        p.rect(
            path_r,
            CornerRadius::same(10),
            Color32::from_black_alpha(if editing { 90 } else { 55 }),
            Stroke::new(1.0, if editing { t.accent.gamma_multiply(0.6) } else { Color32::from_white_alpha(10) }),
            StrokeKind::Inside,
        );
        if editing {
            let pane = self.pane_mut(side);
            let edit = pane.edit.as_mut().expect("editing");
            let te = egui::TextEdit::singleline(edit)
                .frame(egui::Frame::NONE)
                .font(path_font.clone())
                .text_color(t.text)
                .align(Align2::LEFT_CENTER)
                .desired_width(path_r.width() - 24.0);
            let r = ui.put(path_r.shrink2(vec2(12.0, 0.0)), te);
            if pane.edit_focus {
                r.request_focus();
                pane.edit_focus = false;
            }
            if r.lost_focus() {
                let go = ctx.input(|i| i.key_pressed(Key::Enter));
                let target = pane.edit.take().unwrap_or_default();
                if go {
                    acts.push(Act::Navigate(side, target));
                }
            }
        } else {
            let pr = ui.interact(path_r, base.with("path"), Sense::click()).on_hover_cursor(CursorIcon::Text);
            let pane = self.pane(side);
            let shown = if pane.path.is_empty() && side == Side::Local { tr!("This PC").to_string() } else { pane.path.clone() };
            let shown = fit_text(&p, &shown, &path_font, path_r.width() - 24.0, true);
            let c = if pr.hovered() { t.text } else { mix(t.subtext, t.text, 0.5) };
            fx::text(&p, pos2(path_r.min.x + 12.0, path_r.center().y), Align2::LEFT_CENTER, &shown, path_font, c);
            if pr.clicked() && ready {
                let pane = self.pane_mut(side);
                pane.edit = Some(pane.path.clone());
                pane.edit_focus = true;
                acts.push(Act::Focus(side));
            }
        }

        // Columns.
        let w = inner.width();
        let size_w = 80.0;
        let date_w = if w > 360.0 { 122.0 } else { 0.0 };
        let perm_w = if side == Side::Remote && w > 560.0 { 92.0 } else { 0.0 };
        let name_w = w - size_w - date_w - perm_w;
        let head_y = path_r.max.y + 10.0;
        let head = Rect::from_min_size(pos2(inner.min.x, head_y), vec2(w, 22.0));
        let col_font = fonts::medium(11.5);
        let (sort, asc) = (self.pane(side).sort, self.pane(side).asc);
        let mut col = |ui: &mut egui::Ui, x0: f32, width: f32, label: &str, key: Option<SortKey>, right: bool| {
            if width <= 0.0 {
                return;
            }
            let r = Rect::from_min_size(pos2(x0, head.min.y), vec2(width, head.height()));
            let active = key == Some(sort);
            let hovered = key.is_some() && ui.rect_contains_pointer(r);
            let color = if active || hovered { t.text } else { t.subtext };
            let pad = 10.0;
            let tx = if right { r.max.x - pad } else { r.min.x + pad };
            let align = if right { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
            let tr_ = fx::text(&p, pos2(tx, r.center().y), align, label, col_font.clone(), color);
            if active {
                let cx = if right { tr_.min.x - 8.0 } else { tr_.max.x + 8.0 };
                fx::glyph(&p, pos2(cx, r.center().y), if asc { icon::CARET_UP } else { icon::CARET_DOWN }, fonts::mono(10.0), t.accent);
            }
            if let Some(key) = key
                && ui.interact(r, base.with(("col", label)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).clicked()
            {
                acts.push(Act::Sort(side, key));
            }
        };
        let x = inner.min.x;
        col(ui, x + 22.0, name_w - 22.0, tr!("Name"), Some(SortKey::Name), false);
        col(ui, x + name_w, size_w, tr!("Size"), Some(SortKey::Size), true);
        col(ui, x + name_w + size_w, date_w, tr!("Modified"), Some(SortKey::Modified), false);
        col(ui, x + name_w + size_w + date_w, perm_w, tr!("Rights"), None, false);
        p.hline(inner.x_range(), head.max.y + 4.0, Stroke::new(1.0, Color32::from_white_alpha(8)));

        // Footer: counts.
        let foot = Rect::from_min_max(pos2(inner.min.x, inner.max.y - 18.0), inner.max);
        let list = Rect::from_min_max(pos2(inner.min.x - 4.0, head.max.y + 8.0), pos2(inner.max.x + 4.0, foot.min.y - 6.0));
        {
            let pane = self.pane(side);
            if ready && pane.listed {
                let n = pane.view.len();
                let text = if pane.selected.is_empty() {
                    tr!("{n} {n:item|items}", n = n)
                } else {
                    let bytes: u64 = pane.selected.iter().filter_map(|s| pane.entry(s)).map(|e| e.size).sum();
                    let k = pane.selected.len();
                    tr!("{k} of {n} selected · {size}", k = k, n = n, size = human_size(bytes))
                };
                fx::text(&p, pos2(foot.min.x + 4.0, foot.center().y), Align2::LEFT_CENTER, text, fonts::ui(11.5), t.subtext.gamma_multiply(0.8));
            }
            if pane.loading {
                fx::text(&p, pos2(foot.max.x - 4.0, foot.center().y), Align2::RIGHT_CENTER, tr!("loading…"), fonts::ui(11.5), t.subtext.gamma_multiply(0.8));
            }
        }

        if let Some(msg) = overlay {
            let font = fonts::ui(13.0);
            let galley = p.layout(msg.to_string(), font, t.subtext, (list.width() - 40.0).max(80.0));
            let pos = list.center() - galley.size() / 2.0;
            p.galley(pos, galley, t.subtext);
        } else {
            self.rows_ui(ui, list, side, base, look, name_w, size_w, date_w, perm_w, acts);
        }

        // Drop target highlight.
        let drop_hint = if incoming.is_some() || explorer_hover {
            Some(if side == Side::Remote { tr!("Drop to upload here") } else { tr!("Drop to download here") })
        } else {
            None
        };
        if let Some(hint) = drop_hint {
            let fp = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, base.with("drop")));
            fp.rect(rect.shrink(1.0), CornerRadius::same(look.radius as u8), t.accent.gamma_multiply(0.08), Stroke::new(1.5, t.accent.gamma_multiply(0.8)), StrokeKind::Inside);
            let chip_font = fonts::medium(13.0);
            let g = fp.layout_no_wrap(hint.to_string(), chip_font.clone(), t.text);
            let chip = Rect::from_center_size(rect.center(), vec2(g.size().x + 64.0, 40.0));
            fx::plate(&fp, chip, 20.0, t.surface, t.accent, 1.0);
            let ic = if side == Side::Remote { icon::UPLOAD } else { icon::DOWNLOAD };
            fx::glyph(&fp, pos2(chip.min.x + 22.0, chip.center().y), ic, fonts::mono(13.0), t.accent);
            fx::text(&fp, pos2(chip.min.x + 40.0, chip.center().y), Align2::LEFT_CENTER, hint, chip_font, t.text);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rows_ui(
        &mut self,
        ui: &mut egui::Ui,
        list: Rect,
        side: Side,
        base: Id,
        look: &Look<'_>,
        name_w: f32,
        size_w: f32,
        date_w: f32,
        perm_w: f32,
        acts: &mut Vec<Act>,
    ) {
        let t = look.theme;
        let ctx = ui.ctx().clone();
        let focused = self.focus == side;
        let pane = self.pane_mut(side);
        let parent_row = pane.parent().is_some();
        let offset = usize::from(parent_row);
        let n = pane.view.len() + offset;

        // Keep the cursor row on screen after keyboard moves.
        let mut scroll_to = None;
        if pane.reveal {
            pane.reveal = false;
            if let (Some(c), Some(sid)) = (pane.cursor.as_deref().and_then(|c| pane.position(c)), pane.scroll_id) {
                let y = (c + offset) as f32 * ROW_H;
                let cur = egui::scroll_area::State::load(&ctx, sid).map_or(0.0, |s| s.clamped_offset().y);
                let h = list.height();
                if y < cur {
                    scroll_to = Some(y);
                } else if y + ROW_H > cur + h {
                    scroll_to = Some(y + ROW_H - h);
                }
            }
        }

        let out = ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut sa = egui::ScrollArea::vertical().id_salt(base.with("rows")).auto_shrink([false, false]);
            if let Some(y) = scroll_to {
                sa = sa.vertical_scroll_offset(y);
            }
            sa.show_rows(ui, ROW_H, n, |ui, range| {
                for row in range {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                    let p = ui.painter();
                    let hov = resp.hovered();
                    if row < offset {
                        // "..": up one level.
                        if hov {
                            p.rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(8), Color32::from_white_alpha(6));
                        }
                        fx::glyph(p, pos2(r.min.x + 22.0, r.center().y), icon::UP, fonts::mono(11.0), t.subtext);
                        fx::text(p, pos2(r.min.x + 40.0, r.center().y), Align2::LEFT_CENTER, "..", fonts::medium(13.0), t.subtext);
                        if resp.double_clicked() {
                            acts.push(Act::Up(side));
                        } else if resp.clicked() {
                            acts.push(Act::Focus(side));
                        }
                        continue;
                    }
                    let e = pane.entries[pane.view[row - offset]].clone();
                    let selected = pane.selected.contains(&e.name);
                    let is_cursor = pane.cursor.as_deref() == Some(e.name.as_str());
                    let pr = r.shrink2(vec2(4.0, 1.0));
                    if selected {
                        p.rect_filled(pr, CornerRadius::same(8), t.accent.gamma_multiply(if focused { 0.22 } else { 0.12 }));
                    } else if hov {
                        p.rect_filled(pr, CornerRadius::same(8), Color32::from_white_alpha(6));
                    }
                    if is_cursor && focused {
                        p.rect_stroke(pr, CornerRadius::same(8), Stroke::new(1.0, t.accent.gamma_multiply(0.55)), StrokeKind::Inside);
                    }
                    let cy = r.center().y;
                    let x0 = r.min.x + 4.0;
                    let (ic, ic_color) = if e.dir {
                        (if side == Side::Local && pane.path.is_empty() { icon::DRIVE } else { icon::FOLDER }, t.accent)
                    } else if e.link {
                        (icon::LINK, t.subtext)
                    } else {
                        (icon::FILE, t.subtext.gamma_multiply(0.8))
                    };
                    fx::glyph(p, pos2(x0 + 18.0, cy), ic, fonts::mono(12.0), ic_color);
                    let name_font = fonts::medium(13.0);
                    let name_color = if e.hidden { t.subtext } else { t.text };
                    let shown = fit_text(p, &e.name, &name_font, name_w - 48.0, false);
                    fx::text(p, pos2(x0 + 36.0, cy), Align2::LEFT_CENTER, &shown, name_font, name_color);
                    let small = fonts::ui(12.0);
                    if !e.dir {
                        fx::text(p, pos2(x0 + name_w + size_w - 14.0, cy), Align2::RIGHT_CENTER, human_size(e.size), small.clone(), t.subtext);
                    }
                    if date_w > 0.0
                        && let Some(m) = e.modified
                    {
                        fx::text(p, pos2(x0 + name_w + size_w + 6.0, cy), Align2::LEFT_CENTER, platform::format_local(m), small.clone(), t.subtext);
                    }
                    if perm_w > 0.0
                        && let Some(m) = e.perms
                    {
                        fx::text(p, pos2(x0 + name_w + size_w + date_w + 6.0, cy), Align2::LEFT_CENTER, rights(m), fonts::mono(11.5), t.subtext.gamma_multiply(0.85));
                    }

                    // Mouse: click selects, Ctrl toggles, Shift extends, double-click opens.
                    if resp.clicked() {
                        let m = ui.input(|i| i.modifiers);
                        if m.command {
                            if !pane.selected.remove(&e.name) {
                                pane.selected.insert(e.name.clone());
                            }
                            pane.cursor = Some(e.name.clone());
                            pane.anchor = Some(e.name.clone());
                        } else if m.shift {
                            pane.select_range(&e.name);
                        } else {
                            pane.select_only(&e.name);
                        }
                        acts.push(Act::Focus(side));
                    }
                    if resp.double_clicked() {
                        acts.push(Act::Open(side, e.name.clone()));
                    }
                    if resp.secondary_clicked() {
                        if !selected {
                            pane.select_only(&e.name);
                        }
                        acts.push(Act::Focus(side));
                    }
                    if resp.drag_started() {
                        if !selected {
                            pane.select_only(&e.name);
                        }
                        let names = pane.targets();
                        resp.dnd_set_drag_payload(DragFiles { side, names });
                    }
                    let targets = if selected { pane.targets() } else { vec![e.name.clone()] };
                    resp.context_menu(|ui| {
                        ui.set_min_width(190.0);
                        if e.dir && ui.button(tr!("Open")).clicked() {
                            acts.push(Act::Open(side, e.name.clone()));
                            ui.close();
                        }
                        if !e.dir && side == Side::Local && ui.button(tr!("Open with the default app")).clicked() {
                            acts.push(Act::OpenLocal(e.name.clone()));
                            ui.close();
                        }
                        let send = if side == Side::Local { tr!("Upload to the server") } else { tr!("Download to this PC") };
                        if ui.button(send).clicked() {
                            acts.push(Act::Transfer(side, targets.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(tr!("Rename  (F2)")).clicked() {
                            acts.push(Act::AskRename(side, e.name.clone()));
                            ui.close();
                        }
                        if ui.button(tr!("Copy path")).clicked() {
                            acts.push(Act::CopyPath(side, e.name.clone()));
                            ui.close();
                        }
                        if ui.button(tr!("Delete  (Del)")).clicked() {
                            acts.push(Act::AskDelete(side, targets.clone()));
                            ui.close();
                        }
                    });
                }
            })
        });
        pane.scroll_id = Some(out.inner.id);

        if pane.listed && pane.view.is_empty() && !pane.loading {
            let p = ui.painter_at(list);
            let msg = if pane.entries.is_empty() { tr!("This folder is empty") } else { tr!("Only hidden files here") };
            fx::text(&p, list.center(), Align2::CENTER_CENTER, msg, fonts::ui(13.0), t.subtext.gamma_multiply(0.8));
        }

        // A chip under the pointer while dragging rows.
        if let Some(d) = egui::DragAndDrop::payload::<DragFiles>(&ctx)
            && d.side == side
            && let Some(pos) = ctx.pointer_hover_pos()
        {
            let fp = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, base.with("drag")));
            let text = match d.names.as_slice() {
                [one] => one.clone(),
                many => tr!("{n} {n:item|items}", n = many.len()),
            };
            let font = fonts::medium(12.5);
            let g = fp.layout_no_wrap(text.clone(), font.clone(), t.text);
            let chip = Rect::from_min_size(pos + vec2(14.0, 10.0), vec2(g.size().x.min(260.0) + 40.0, 30.0));
            fx::plate(&fp, chip, 15.0, t.surface, t.accent, 0.8);
            let ic = if side == Side::Local { icon::UPLOAD } else { icon::DOWNLOAD };
            fx::glyph(&fp, pos2(chip.min.x + 16.0, chip.center().y), ic, fonts::mono(11.0), t.accent);
            let shown = fit_text(&fp, &text, &font, 260.0, false);
            fx::text(&fp, pos2(chip.min.x + 30.0, chip.center().y), Align2::LEFT_CENTER, shown, font, t.text);
        }
    }

    fn transfers_height(&self) -> f32 {
        let n = self.jobs.lock().len();
        if n == 0 { 0.0 } else { 46.0 + n.min(3) as f32 * 46.0 + 8.0 }
    }

    fn transfers_ui(&mut self, ui: &mut egui::Ui, rect: Rect, look: &Look<'_>, acts: &mut Vec<Act>) {
        let t = look.theme;
        let painter = ui.painter_at(rect.expand(24.0));
        fx::shadow(&painter, rect, look.radius, 0.6);
        painter.rect_filled(rect, CornerRadius::same(look.radius as u8), t.bg.gamma_multiply(look.pane_alpha));
        fx::vgradient_stroke(&painter, rect, look.radius, 1.0, Color32::from_white_alpha(16), Color32::from_white_alpha(9));
        let inner = rect.shrink2(vec2(16.0, 10.0));
        let p = ui.painter_at(rect);
        let now = Instant::now();

        let mut jobs = self.jobs.lock();
        // Speed: a smoothed rate per running job.
        for j in jobs.iter_mut().filter(|j| j.state == JobState::Running) {
            let done = j.progress.done.load(Relaxed);
            match j.sample {
                Some((at, bytes)) if now.duration_since(at) >= Duration::from_millis(500) => {
                    let rate = done.saturating_sub(bytes) as f64 / now.duration_since(at).as_secs_f64();
                    j.speed = if j.speed == 0.0 { rate } else { j.speed * 0.6 + rate * 0.4 };
                    j.sample = Some((now, done));
                }
                None => j.sample = Some((now, done)),
                _ => {}
            }
        }
        let active = jobs.iter().filter(|j| matches!(j.state, JobState::Queued | JobState::Running)).count();
        let finished = jobs.len() - active;

        // Header.
        let hy = inner.min.y + 14.0;
        let title_font = fonts::bold(13.0);
        let tr_ = fx::text(&p, pos2(inner.min.x, hy), Align2::LEFT_CENTER, tr!("Transfers"), title_font, t.text);
        let summary = if active > 0 { tr!("{n} in progress", n = active) } else { tr!("all done").to_string() };
        fx::text(&p, pos2(tr_.max.x + 12.0, hy), Align2::LEFT_CENTER, summary, fonts::ui(12.0), t.subtext);
        if finished > 0 {
            let label = tr!("Clear finished");
            let g = p.layout_no_wrap(label.to_string(), fonts::medium(12.0), t.subtext);
            let r = Rect::from_min_max(pos2(inner.max.x - g.size().x - 20.0, hy - 13.0), pos2(inner.max.x, hy + 13.0));
            let resp = ui.interact(r, Id::unique(("files-clear", self.id)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            if resp.hovered() {
                p.rect_filled(r, CornerRadius::same(8), Color32::from_white_alpha(10));
            }
            fx::text(&p, r.center(), Align2::CENTER_CENTER, label, fonts::medium(12.0), if resp.hovered() { t.text } else { t.subtext });
            if resp.clicked() {
                acts.push(Act::ClearFinished);
            }
        }

        // Rows, newest work first: running, queued, then finished.
        let mut order: Vec<usize> = (0..jobs.len()).collect();
        let rank = |s: &JobState| match s {
            JobState::Running => 0,
            JobState::Queued => 1,
            _ => 2,
        };
        order.sort_by_key(|&i| (rank(&jobs[i].state), std::cmp::Reverse(jobs[i].id)));
        let list = Rect::from_min_max(pos2(inner.min.x - 4.0, inner.min.y + 32.0), pos2(inner.max.x + 4.0, inner.max.y));
        ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical().id_salt(("files-jobs", self.id)).auto_shrink([false, false]).show_rows(ui, 46.0, order.len(), |ui, range| {
                for k in range {
                    let j = &jobs[order[k]];
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
                    let p = ui.painter();
                    let cy = r.center().y;
                    let (ic, ic_c) = if j.way == Way::Up { (icon::UPLOAD, t.accent) } else { (icon::DOWNLOAD, t.accent2) };
                    fx::glyph(p, pos2(r.min.x + 16.0, cy), ic, fonts::mono(13.0), ic_c);
                    let right_w = (r.width() * 0.42).clamp(200.0, 360.0);
                    let bar_x = r.max.x - right_w - 36.0;
                    // The name and route stop short of the bar column.
                    let text_w = bar_x - 20.0 - (r.min.x + 36.0);
                    let name_font = fonts::medium(13.0);
                    let name = fit_text(p, &j.name, &name_font, text_w, false);
                    fx::text(p, pos2(r.min.x + 36.0, cy - 8.0), Align2::LEFT_CENTER, name, name_font, t.text);
                    let route = format!("{}  →  {}", j.src, j.dst);
                    let route_font = fonts::mono(11.0);
                    let route = fit_text(p, &route, &route_font, text_w, true);
                    fx::text(p, pos2(r.min.x + 36.0, cy + 9.0), Align2::LEFT_CENTER, route, route_font, t.subtext.gamma_multiply(0.8));

                    // Right: bar and numbers, or the outcome.
                    let total = j.progress.total.load(Relaxed);
                    let done = j.progress.done.load(Relaxed);
                    let frac = if total > 0 { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
                    let bar = Rect::from_min_size(pos2(bar_x, cy - 9.0), vec2(right_w, 5.0));
                    let small = fonts::ui(11.5);
                    match &j.state {
                        JobState::Running | JobState::Queued => {
                            p.rect_filled(bar, CornerRadius::same(3), Color32::from_white_alpha(14));
                            if frac > 0.0 {
                                let fill = Rect::from_min_size(bar.min, vec2((bar.width() * frac).max(5.0), bar.height()));
                                p.rect_filled(fill, CornerRadius::same(3), ic_c);
                            }
                            let info = if j.state == JobState::Queued {
                                tr!("waiting").to_string()
                            } else if total == 0 && done == 0 {
                                tr!("preparing…").to_string()
                            } else {
                                let mut s = tr!("{done} of {total}", done = human_size(done), total = human_size(total));
                                if j.speed > 1.0 {
                                    s.push_str(&format!("  ·  {}", tr!("{size}/s", size = human_size(j.speed as u64))));
                                    let left = total.saturating_sub(done) as f64 / j.speed;
                                    s.push_str(&format!("  ·  {}", tr!("{time} left", time = duration_short(left))));
                                }
                                s
                            };
                            fx::text(p, pos2(bar.min.x, cy + 8.0), Align2::LEFT_CENTER, info, small, t.subtext);
                        }
                        JobState::Done => {
                            let secs = match (j.started, j.ended) {
                                (Some(a), Some(b)) => b.duration_since(a).as_secs_f64(),
                                _ => 0.0,
                            };
                            fx::glyph(p, pos2(bar.min.x + 6.0, cy), icon::CHECK, fonts::mono(11.0), t.ok);
                            let s = tr!("done · {size} in {time}", size = human_size(total), time = duration_short(secs));
                            fx::text(p, pos2(bar.min.x + 20.0, cy), Align2::LEFT_CENTER, s, small, t.subtext);
                        }
                        JobState::Failed(e) => {
                            let s = fit_text(p, e, &small, right_w, false);
                            fx::text(p, pos2(bar.min.x, cy), Align2::LEFT_CENTER, s, small, t.err);
                        }
                        JobState::Cancelled => {
                            fx::text(p, pos2(bar.min.x, cy), Align2::LEFT_CENTER, tr!("cancelled"), small, t.subtext.gamma_multiply(0.8));
                        }
                    }
                    if matches!(j.state, JobState::Running | JobState::Queued) {
                        let xr = Rect::from_center_size(pos2(r.max.x - 16.0, cy), vec2(26.0, 26.0));
                        let x = ui
                            .scope_builder(UiBuilder::new().max_rect(xr), |ui| widgets::icon_button(ui, t, icon::XMARK, xr.size(), false))
                            .inner
                            .on_hover_text(tr!("Cancel"));
                        if x.clicked() {
                            acts.push(Act::Cancel(j.id));
                        }
                    }
                }
            });
        });
    }

    fn notice_ui(&mut self, ui: &mut egui::Ui, rect: Rect, t: &Theme, now: f64) {
        let Some((text, at)) = &self.notice else { return };
        let age = now - at;
        if age > 6.0 {
            self.notice = None;
            return;
        }
        let alpha = (1.0 - ((age - 5.5) / 0.5).clamp(0.0, 1.0)) as f32 * anim::smooth(((age / 0.2) as f32).min(1.0));
        let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique(("files-notice", self.id))));
        let font = fonts::medium(12.5);
        let g = p.layout(text.clone(), font.clone(), t.text, (rect.width() * 0.6).max(200.0));
        let size = g.size() + vec2(36.0, 18.0);
        let chip = Rect::from_center_size(pos2(rect.center().x, rect.max.y - 30.0 - size.y / 2.0), size);
        fx::plate(&p, chip, 14.0, t.surface.gamma_multiply(alpha), t.err.gamma_multiply(alpha), 0.6);
        p.galley(chip.min + vec2(18.0, 9.0), g, t.text.gamma_multiply(alpha));
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, t: &Theme, rt: &Handle, now: f64) {
        let Some(dialog) = &mut self.dialog else { return };
        let id = Id::unique(("files-dialog", self.id));
        let mut close = false;
        let mut done: Option<Done> = None;
        let (_, dismissed) = widgets::dialog(ctx, t, id, self.dialog_at, 380.0, |ui| match dialog {
            Dialog::Rename { side, old, name, focus } => {
                header(ui, t, icon::PENCIL, &tr!("Rename “{name}”", name = old.as_str()));
                let r = widgets::input(ui, t, name, "", false, "");
                if std::mem::take(focus) {
                    r.request_focus();
                }
                let ok = valid_name(name) && name.trim() != old.as_str();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(ok, |ui| {
                        if widgets::primary(ui, t, "", tr!("Rename"), vec2(110.0, 36.0)).clicked() || (enter && ok) {
                            done = Some(Done::Rename(*side, old.clone(), name.clone()));
                        }
                    });
                    if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                        close = true;
                    }
                });
            }
            Dialog::NewFolder { side, name, focus } => {
                header(ui, t, icon::FOLDER_PLUS, tr!("New folder"));
                let r = widgets::input(ui, t, name, tr!("Folder name"), false, "");
                if std::mem::take(focus) {
                    r.request_focus();
                }
                let ok = valid_name(name);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(ok, |ui| {
                        if widgets::primary(ui, t, "", tr!("Create"), vec2(110.0, 36.0)).clicked() || (enter && ok) {
                            done = Some(Done::NewFolder(*side, name.clone()));
                        }
                    });
                    if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Delete { side, names } => {
                let title = match names.as_slice() {
                    [one] => tr!("Delete “{name}”?", name = one.as_str()),
                    many => tr!("Delete {n} {n:item|items}?", n = many.len()),
                };
                header(ui, t, icon::TRASH, &title);
                let note = if *side == Side::Local {
                    tr!("They go to the Recycle Bin.")
                } else {
                    tr!("This deletes them on the server for good, folders with everything inside.")
                };
                ui.label(egui::RichText::new(note).color(t.subtext));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if widgets::danger(ui, t, tr!("Delete"), vec2(110.0, 36.0)).clicked() {
                        done = Some(Done::Delete(*side, names.clone()));
                    }
                    if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Replace { plan, existing } => {
                let title = match existing.as_slice() {
                    [one] => tr!("“{name}” already exists", name = one.as_str()),
                    many => tr!("{n} {n:item already exists|items already exist}", n = many.len()),
                };
                header(ui, t, icon::COPY, &title);
                let mut list: Vec<&str> = existing.iter().take(5).map(String::as_str).collect();
                if existing.len() > 5 {
                    list.push("…");
                }
                if existing.len() > 1 {
                    ui.label(egui::RichText::new(list.join(", ")).color(t.subtext).monospace());
                }
                ui.label(egui::RichText::new(tr!("Replace them, or copy only what is new?")).color(t.subtext));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if widgets::danger(ui, t, tr!("Replace"), vec2(110.0, 36.0)).clicked() {
                        done = Some(Done::Enqueue(plan.clone()));
                    }
                    if plan.len() > existing.len() && widgets::secondary(ui, t, "", tr!("Skip existing"), vec2(120.0, 36.0)).clicked() {
                        done = Some(Done::Enqueue(plan.iter().filter(|p| !existing.contains(&p.name)).cloned().collect()));
                    }
                    if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                        close = true;
                    }
                });
            }
        });
        if dismissed || close || done.is_some() {
            self.dialog = None;
        }
        match done {
            Some(Done::Rename(side, old, new)) => self.rename(side, old, new, rt, ctx),
            Some(Done::NewFolder(side, name)) => self.new_folder(side, name, rt, ctx),
            Some(Done::Delete(side, names)) => self.delete(side, names, rt, ctx),
            Some(Done::Enqueue(plan)) => self.enqueue(plan, rt, ctx),
            None => {}
        }
        let _ = now;
    }
}

/// What a confirmed dialog asks for.
enum Done {
    Rename(Side, String, String),
    NewFolder(Side, String),
    Delete(Side, Vec<String>),
    Enqueue(Vec<Plan>),
}

fn header(ui: &mut egui::Ui, t: &Theme, ic: &str, title: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let p = ui.painter();
    let badge = Rect::from_min_size(r.min, vec2(40.0, 40.0));
    fx::plate(p, badge, 12.0, t.accent.gamma_multiply(0.12), t.accent, 0.6);
    fx::glyph(p, badge.center(), ic, fonts::mono(16.0), t.accent);
    let font = fonts::display(19.0);
    let shown = fit_text(p, title, &font, r.width() - 56.0, false);
    fx::text(p, pos2(badge.max.x + 14.0, badge.center().y), Align2::LEFT_CENTER, shown, font, t.text);
    ui.add_space(16.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        assert_eq!(remote_parent("/home/me"), Some("/home".into()));
        assert_eq!(remote_parent("/home"), Some("/".into()));
        assert_eq!(remote_parent("/"), None);
        assert_eq!(remote_parent("/srv/app/"), Some("/srv".into()));
        assert_eq!(remote_join("/", "etc"), "/etc");
        assert_eq!(remote_join("/etc", "hosts"), "/etc/hosts");
        assert_eq!(last_component("/srv/app/"), Some("app".into()));
        assert_eq!(last_component(r"C:\Users\me"), Some("me".into()));
    }

    #[cfg(windows)]
    #[test]
    fn local_paths() {
        assert_eq!(local_parent(r"C:\Users"), Some("C:\\".into()));
        assert_eq!(local_parent("C:\\"), Some(String::new()));
        assert_eq!(local_parent(""), None);
        assert_eq!(local_join("", "D:"), "D:\\");
    }

    #[test]
    fn names_sort_naturally() {
        let mut v = vec!["file10", "File2", "file1", "a"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "file1", "File2", "file10"]);
    }

    #[test]
    fn formats() {
        assert_eq!(rights(0o754), "rwxr-xr--");
        assert!(valid_name("notes.txt"));
        assert!(!valid_name(".."));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("  "));
    }
}
