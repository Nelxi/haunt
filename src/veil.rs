//! Streamer mode.
//!
//! Sensitive text (IPs, saved hosts, emails, API tokens) is painted *masked* in the main window,
//! which is what OBS, ShareX, Discord and every other capture tool sees. The real text is painted
//! into a second, transparent, click-through window that sits exactly on top of the main one and
//! is excluded from capture with `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`. On your
//! monitor the two layers combine into the real thing; in a recording the overlay simply is not
//! there.
//!
//! All UI drawing happens on one thread, so the per-frame state lives in a thread-local and any
//! widget can call [`text`] / [`cover`] without plumbing.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use eframe::egui::{Align2, Color32, FontId, Galley, Painter, Pos2, Rect, pos2, vec2};
use regex::Regex;

/// One masked area: what to paint in the overlay over it.
pub struct Secret {
    _rect: Rect,
    clip: Rect,
    _fill: Color32,
    _radius: f32,
    opacity: f32,
    texts: Vec<(Pos2, Arc<Galley>, Color32)>,
    /// Extra solid rects painted over the text (e.g. a text caret).
    extra: Vec<(Rect, Color32)>,
    /// Drawn from a dialog / popup layer (stays visible while a modal is open).
    pub top: bool,
}

#[derive(Default)]
struct State {
    on: bool,
    top: bool,
    keywords: Vec<String>,
    secrets: Vec<Secret>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        // IPv4, optional CIDR suffix
        r"\b(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(?:\.(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}(?:/\d{1,2})?\b",
        // IPv6: full form, or compressed with `::`
        r"|(?i:\b(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}\b|\b(?:[0-9a-f]{1,4}:){1,6}:(?:[0-9a-f]{1,4}(?::[0-9a-f]{1,4}){0,5})?)",
        // email
        r"|[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
        // well-known token shapes: GitHub, Slack, OpenAI/Anthropic, AWS, Google, JWT
        r"|\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}",
        r"|sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,}",
        r"|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})",
    ))
    .expect("streamer-mode pattern")
});

/// Starts a frame. `keywords` are always-sensitive strings such as saved host names.
pub fn begin_frame(on: bool, keywords: impl IntoIterator<Item = String>) {
    STATE.with_borrow_mut(|s| {
        s.on = on;
        s.top = false;
        s.secrets.clear();
        s.keywords.clear();
        if on {
            s.keywords.extend(
                keywords
                    .into_iter()
                    .map(|k| k.trim().to_lowercase())
                    .filter(|k| k.len() >= 4 && k != "localhost"),
            );
            s.keywords.sort();
            s.keywords.dedup();
        }
    });
}

pub fn enabled() -> bool {
    STATE.with_borrow(|s| s.on)
}

/// Marks following secrets as drawn on a dialog layer.
pub fn set_top(top: bool) {
    STATE.with_borrow_mut(|s| s.top = top);
}

pub fn take() -> Vec<Secret> {
    STATE.with_borrow_mut(|s| std::mem::take(&mut s.secrets))
}

/// Byte ranges of sensitive substrings, sorted and merged.
pub fn find(text: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for m in PATTERN.find_iter(text) {
        if plausible(m.as_str()) {
            out.push(m.range());
        }
    }
    STATE.with_borrow(|s| {
        if s.keywords.is_empty() {
            return;
        }
        let lower = text.to_lowercase();
        // `to_lowercase` can change byte lengths for non-ASCII text; only trust it when it didn't.
        if lower.len() != text.len() {
            return;
        }
        for k in &s.keywords {
            let mut from = 0;
            while let Some(i) = lower[from..].find(k.as_str()) {
                let start = from + i;
                let end = start + k.len();
                let bounded = |b: Option<u8>| b.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
                if bounded(start.checked_sub(1).map(|j| lower.as_bytes()[j])) && bounded(lower.as_bytes().get(end).copied()) {
                    out.push(start..end);
                }
                from = end;
            }
        }
    });
    out.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(out.len());
    for r in out {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// Filters regex hits that are almost never secrets.
fn plausible(m: &str) -> bool {
    if m.contains("::") || m.matches(':').count() >= 2 && !m.contains('.') && !m.contains('@') {
        // IPv6 candidates: skip loopback/short noise like `a::b` and `::1`.
        let groups = m.split(':').filter(|g| !g.is_empty()).count();
        return m.len() >= 6 && groups >= 2;
    }
    if let Some(ip) = m.split('/').next().filter(|ip| ip.bytes().all(|b| b.is_ascii_digit() || b == b'.')) {
        // Version numbers like 1.2.3.4 look like IPs too; still hide them, except the obvious
        // non-routable ones that carry no information.
        return !matches!(ip, "0.0.0.0" | "127.0.0.1" | "255.255.255.255");
    }
    true
}

/// Paints `text` as fully sensitive: masked here, real in the overlay.
/// Without streamer mode it is a plain `painter.text`.
pub fn text(painter: &Painter, pos: Pos2, align: Align2, text: impl ToString, font: FontId, color: Color32, fill: Color32) -> Rect {
    let galley = painter.layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
    let rect = align.anchor_size(pos, galley.size());
    if !enabled() {
        painter.galley(rect.min, galley, color);
        return rect;
    }
    let chip = rect;
    cover(painter, chip, fill, 0.0, vec![(rect.min, galley, color)]);
    rect
}

/// Like [`text`], but only the sensitive parts of `text` are hidden.
pub fn text_auto(painter: &Painter, pos: Pos2, align: Align2, text: &str, font: FontId, color: Color32, fill: Color32) -> Rect {
    let spans = if enabled() { find(text) } else { Vec::new() };
    if spans.is_empty() {
        return painter.text(pos, align, text, font, color);
    }
    let full = painter.layout_no_wrap(text.to_string(), font.clone(), Color32::PLACEHOLDER);
    let rect = align.anchor_size(pos, full.size());
    let mut x = rect.min.x;
    let mut at = 0;
    for r in spans.iter().cloned().chain(std::iter::once(text.len()..text.len())) {
        if r.start > at {
            let g = painter.layout_no_wrap(text[at..r.start].to_string(), font.clone(), Color32::PLACEHOLDER);
            let w = g.size().x;
            painter.galley(pos2(x, rect.min.y), g, color);
            x += w;
        }
        if r.end > r.start {
            let g = painter.layout_no_wrap(text[r.clone()].to_string(), font.clone(), Color32::PLACEHOLDER);
            let w = g.size().x;
            let chip = Rect::from_min_size(pos2(x, rect.min.y), vec2(w, rect.height()));
            cover(painter, chip, fill, 0.0, vec![(pos2(x, rect.min.y), g, color)]);
            x += w;
        }
        at = r.end;
    }
    rect
}

/// Paints the mask for `rect` in the main window and queues the real content for the overlay.
/// `fill` must be opaque: it is what hides the mask on your screen.
pub fn cover(painter: &Painter, rect: Rect, fill: Color32, radius: f32, texts: Vec<(Pos2, Arc<Galley>, Color32)>) {
    cover_with(painter, rect, fill, radius, texts, Vec::new());
}

pub fn cover_with(
    painter: &Painter,
    rect: Rect,
    fill: Color32,
    radius: f32,
    texts: Vec<(Pos2, Arc<Galley>, Color32)>,
    extra: Vec<(Rect, Color32)>,
) {
    let clip = painter.clip_rect();
    if !clip.intersects(rect) {
        return;
    }
    let fill = fill.to_opaque();
    paint_mask(painter, rect, fill, radius);
    STATE.with_borrow_mut(|s| {
        let top = s.top;
        s.secrets.push(Secret { _rect: rect, clip, _fill: fill, _radius: radius, opacity: painter.opacity(), texts, extra, top });
    });
}

/// The masked look: empty space in the captured window (OBS / Discord / recording).
fn paint_mask(_painter: &Painter, _rect: Rect, _fill: Color32, _radius: f32) {
    // Left as blank empty space in screen capture.
}

/// Paints the real content; called inside the capture-excluded overlay window.
pub fn paint_real(painter: &Painter, s: &Secret) {
    let p = painter.with_clip_rect(s.clip);
    let k = s.opacity;
    if k <= 0.0 {
        return;
    }
    // No background podlozhka: clean text that looks identical to normal unmasked text!
    for (pos, g, color) in &s.texts {
        p.galley(*pos, g.clone(), color.gamma_multiply(k));
    }
    for (r, color) in &s.extra {
        p.rect_filled(*r, 1.0, color.gamma_multiply(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(t: &str) -> Vec<&str> {
        find(t).into_iter().map(|r| &t[r]).collect()
    }

    #[test]
    fn detects() {
        begin_frame(true, ["Edge.Example.net".to_string(), "pi".to_string()]);
        assert_eq!(hits("inet 203.0.113.7/24 brd 203.0.113.255"), ["203.0.113.7/24", "203.0.113.255"]);
        assert_eq!(hits("listen [2001:db8::1]:443"), ["2001:db8::1"]);
        assert_eq!(hits("from root@edge.example.net port 22"), ["root@edge.example.net"]);
        assert_eq!(hits("ssh edge.example.net"), ["edge.example.net"]);
        assert_eq!(hits("export GH=ghp_abcdefghijklmnopqrstuvwxyz0123"), ["ghp_abcdefghijklmnopqrstuvwxyz0123"]);
        assert!(hits("std::vec::Vec 12:34:56 a::b ::1 127.0.0.1 root@box:~# pip install").is_empty());
    }

    #[test]
    fn detects_tokens_and_secrets() {
        begin_frame(true, Vec::new());
        // Slack token
        assert_eq!(hits("token=xoxb-1234567890-abcdefghij"), ["xoxb-1234567890-abcdefghij"]);
        // OpenAI key
        assert_eq!(hits("api_key: sk-abcdefghijklmnopqrstuvwxyz123456"), ["sk-abcdefghijklmnopqrstuvwxyz123456"]);
        // AWS Access Key ID
        assert_eq!(hits("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"), ["AKIAIOSFODNN7EXAMPLE"]);
        // Google API key
        assert_eq!(hits("key=AIzaSyD-1234567890abcdefghijklmnopqrstuv"), ["AIzaSyD-1234567890abcdefghijklmnopqrstuv"]);
        // JWT
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozGz6ePqT1x_sample_sig_12345";
        assert_eq!(hits(&format!("Bearer {jwt}")), [jwt]);
    }

    #[test]
    fn disabled_mode() {
        begin_frame(false, ["secret.server.local".to_string()]);
        assert!(!enabled());
        // find still matches regex patterns if called directly, but keywords are not loaded
        assert!(hits("connect to secret.server.local").is_empty());
    }

    #[test]
    fn boundary_and_empty() {
        begin_frame(true, ["myhost.org".to_string()]);
        assert!(hits("").is_empty());
        assert_eq!(hits("host: myhost.org"), ["myhost.org"]);
        // inside an identifier, should not match
        assert!(hits("notmyhost.org").is_empty());
        assert!(hits("myhost.orgsuffix").is_empty());
    }
}

