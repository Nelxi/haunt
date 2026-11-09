//! Main window. The window is undecorated and transparent: an aurora shader is the
//! "wallpaper", and the sidebar and terminal float above it as glass cards with
//! gaps between them, like tiled windows in HyDE. The titlebar, tabs, dialogs and
//! launcher are all drawn here.

use crate::tr;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle as AnsiCursorStyle};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Id, Key, Modifiers, PointerButton, Rect,
    RichText, Sense, Stroke, StrokeKind, UiBuilder, ViewportCommand, pos2, vec2,
};
use eframe::glow;
use uuid::Uuid;

use crate::config::{Auth, Config, CursorStyle, Proxy, Server};
use crate::import;
use crate::veil;
use crate::fonts::icon;
use crate::session::{ConnectSpec, PromptKind, Session, Status};
use crate::shader::{self, Aurora, AuroraParams};
use crate::theme::{self, Theme, mix};
use crate::{anim, fonts, fx, platform, secrets, termview, widgets};

const TITLEBAR_H: f32 = 48.0;
const RESIZE_EDGE: f32 = 6.0;

struct Editor {
    server: Server,
    is_new: bool,
    secret: String,
    key_path: String,
    port: String,
    use_key: bool,
    /// 0 direct, 1 SOCKS5, 2 HTTP, 3 jump host.
    proxy_kind: usize,
    proxy_host: String,
    proxy_port: String,
    proxy_user: String,
    proxy_secret: String,
    jump: Option<Uuid>,
    error: Option<String>,
}

impl Editor {
    fn new(server: Server, is_new: bool) -> Self {
        let (use_key, key_path) = match &server.auth {
            Auth::Key { path } => (true, path.display().to_string()),
            Auth::Password => (false, String::new()),
        };
        let port = server.port.to_string();
        let (proxy_kind, proxy_host, proxy_port, proxy_user, jump) = match &server.proxy {
            Proxy::None => (0, String::new(), String::new(), String::new(), None),
            Proxy::Socks5 { host, port, user } => (1, host.clone(), port.to_string(), user.clone(), None),
            Proxy::Http { host, port, user } => (2, host.clone(), port.to_string(), user.clone(), None),
            Proxy::Jump { server } => (3, String::new(), String::new(), String::new(), Some(*server)),
        };
        Self {
            server,
            is_new,
            secret: String::new(),
            key_path,
            port,
            use_key,
            proxy_kind,
            proxy_host,
            proxy_port,
            proxy_user,
            proxy_secret: String::new(),
            jump,
            error: None,
        }
    }
}

struct Launcher {
    query: String,
    selected: usize,
}

enum LaunchItem {
    Server(Server),
    Quick { user: String, host: String, port: u16 },
    NewServer,
    Settings,
    Import(ImportSource),
}

#[derive(Clone, Copy)]
enum ImportSource {
    Putty,
    SshConfig,
}

struct Toast {
    text: String,
    at: f64,
    error: bool,
}

pub struct HauntApp {
    cfg: Config,
    rt: tokio::runtime::Handle,
    sessions: Vec<Session>,
    active: usize,
    next_id: u64,
    palette: [Color32; 256],
    applied_theme: String,
    search: String,
    editor: Option<Editor>,
    show_settings: bool,
    confirm_delete: Option<Uuid>,
    launcher: Option<Launcher>,
    prompt_input: String,
    prompt_remember: bool,
    toasts: Vec<Toast>,

    aurora: Option<Arc<Aurora>>,
    blur_wanted: bool,
    blur_active: bool,
    /// Animation clock; only advances while the window is focused.
    clock: f32,
    started: Option<f64>,
    /// When each dialog was opened, for the pop-in animation.
    opened: HashMap<&'static str, f64>,
    /// Tab id -> time it was opened (tab pop-in).
    tab_born: HashMap<u64, f64>,
    last_active_id: Option<u64>,
    switch: (f64, f32, bool),
    sidebar_open: bool,
    /// The quick-connect field on the start screen.
    quick: String,
}

impl HauntApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        let t0 = std::time::Instant::now();
        fonts::install(&cc.egui_ctx);
        log::debug!("startup: fonts {:?}", t0.elapsed());
        let (mut cfg, load_error) = Config::load();
        crate::i18n::set(&cfg.settings.language);
        // Debug builds: override opacity for visual tests without touching the saved config.
        #[cfg(debug_assertions)]
        if let Some(op) = std::env::var("HAUNT_OPACITY").ok().and_then(|v| v.parse().ok()) {
            cfg.settings.opacity = op;
        }
        let theme = theme::by_name(&cfg.settings.theme);

        log::debug!("startup: config {:?}", t0.elapsed());
        platform::round_corners(cc);
        let blur_active = cfg.settings.blur && platform::set_blur(cc, true);
        log::debug!("startup: window chrome {:?}", t0.elapsed());
        let aurora = cc.gl.as_ref().and_then(|gl| match Aurora::new(gl) {
            Ok(a) => Some(Arc::new(a)),
            Err(e) => {
                log::warn!("aurora shader disabled: {e}");
                None
            }
        });

        log::debug!("startup: shader {:?}", t0.elapsed());
        let mut app = Self {
            palette: theme.palette256(),
            applied_theme: String::new(),
            blur_wanted: cfg.settings.blur,
            blur_active,
            cfg,
            rt,
            sessions: Vec::new(),
            active: 0,
            next_id: 1,
            search: String::new(),
            editor: None,
            show_settings: false,
            confirm_delete: None,
            launcher: None,
            prompt_input: String::new(),
            prompt_remember: true,
            toasts: Vec::new(),
            aurora,
            clock: 0.0,
            started: None,
            opened: HashMap::new(),
            tab_born: HashMap::new(),
            last_active_id: None,
            switch: (f64::NEG_INFINITY, 0.0, false),
            sidebar_open: true,
            quick: String::new(),
        };
        // Debug builds: open a screen on start for screenshot checks (no key presses needed).
        #[cfg(debug_assertions)]
        match std::env::var("HAUNT_PREVIEW").as_deref() {
            Ok("editor") => app.editor = Some(Editor::new(Server::new(), true)),
            Ok("editor-jump") | Ok("editor-socks") => {
                let mut ed = Editor::new(Server::new(), true);
                ed.proxy_kind = if std::env::var("HAUNT_PREVIEW").as_deref() == Ok("editor-jump") { 3 } else { 1 };
                ed.jump = app.cfg.servers.first().map(|s| s.id);
                app.editor = Some(ed);
            }
            Ok("settings") => app.show_settings = true,
            Ok("launcher") => app.launcher = Some(Launcher { query: String::new(), selected: 0 }),
            Ok("connect") => {
                if let Some(s) = app.cfg.servers.first().cloned() {
                    app.connect(&cc.egui_ctx, &s);
                }
            }
            _ => {}
        }
        if let Some(e) = load_error {
            app.toast_at(e, 0.0, true);
        }
        app
    }

    /// (background alpha, card alpha). With blur on, the two layers stack to the
    /// configured opacity: 1 - (1 - a)^2 = opacity. Without blur, whole-window opacity
    /// is handled via OS layered window attributes.
    fn layer_alpha(&self) -> (f32, f32) {
        let op = self.cfg.settings.opacity.clamp(0.2, 1.0);
        if self.blur_active {
            let a = 1.0 - (1.0 - op).sqrt();
            (a, a)
        } else {
            (1.0, 1.0)
        }
    }

    fn theme(&self) -> &'static Theme {
        theme::by_name(&self.cfg.settings.theme)
    }

    fn toast_at(&mut self, text: impl Into<String>, at: f64, error: bool) {
        self.toasts.push(Toast { text: text.into(), at, error });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    fn import_sessions(&mut self, ctx: &egui::Context, src: ImportSource) {
        let (found, label) = match src {
            ImportSource::Putty => (import::putty(), "PuTTY"),
            ImportSource::SshConfig => (import::ssh_config(), "~/.ssh/config"),
        };
        if found.servers.is_empty() {
            self.notify(ctx, tr!("No sessions found in {label}", label = label), true);
            return;
        }
        let total = found.servers.len();
        let group = found.servers.first().map(|s| s.group.clone()).unwrap_or_default();
        let added = import::merge(&mut self.cfg.servers, found.servers);
        let linked = import::link_jumps(&mut self.cfg.servers, found.jumps, &group);
        let mut msg = match added {
            0 => tr!("All {n} {n:session|sessions} from {label} are already saved", n = total, label = label),
            n => tr!("Imported {n} of {total} {total:session|sessions} from {label}", n = n, total = total, label = label),
        };
        if linked > 0 {
            msg.push_str(&tr!(", {n} through a jump host", n = linked));
        }
        if found.unsupported > 0 && added > 0 {
            msg.push_str(&tr!(" ({n} with an unsupported proxy, imported as direct)", n = found.unsupported));
        }
        self.notify(ctx, msg, false);
        if added > 0 || linked > 0 {
            self.save(ctx);
        }
    }

    fn notify(&mut self, ctx: &egui::Context, text: impl Into<String>, error: bool) {
        let now = ctx.input(|i| i.time);
        self.toast_at(text, now, error);
    }

    fn save(&mut self, ctx: &egui::Context) {
        if let Err(e) = self.cfg.save() {
            self.notify(ctx, tr!("Could not save the config: {e}", e = format!("{e:#}")), true);
        }
    }

    fn term_config(&self) -> TermConfig {
        let s = &self.cfg.settings;
        let shape = match s.cursor {
            CursorStyle::Block => CursorShape::Block,
            CursorStyle::Beam => CursorShape::Beam,
            CursorStyle::Underline => CursorShape::Underline,
        };
        TermConfig {
            scrolling_history: s.scrollback.clamp(100, 200_000),
            default_cursor_style: AnsiCursorStyle { shape, blinking: s.cursor_blink },
            ..TermConfig::default()
        }
    }

    fn open_spec(&mut self, ctx: &egui::Context, label: String, mut spec: ConnectSpec) {
        spec.reconnect = self.cfg.settings.auto_reconnect;
        let id = self.next_id;
        self.next_id += 1;
        let session = Session::start(id, label, spec, self.term_config(), &self.rt, ctx);
        self.tab_born.insert(id, ctx.input(|i| i.time));
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    fn connect(&mut self, ctx: &egui::Context, server: &Server) {
        let spec = ConnectSpec::from_server(server, &self.cfg.servers, self.cfg.settings.keepalive_secs);
        self.open_spec(ctx, server.display_name().to_string(), spec);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if let Some(s) = self.cfg.servers.iter_mut().find(|s| s.id == server.id) {
            s.last_used = Some(now);
            self.save(ctx);
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            let s = self.sessions.remove(idx);
            self.tab_born.remove(&s.id);
            if self.active >= self.sessions.len() {
                self.active = self.sessions.len().saturating_sub(1);
            } else if idx < self.active {
                self.active -= 1;
            }
            if let Some(s) = self.sessions.get_mut(self.active) {
                s.want_focus = true;
            }
        }
    }

    fn activate(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.active = idx;
            self.sessions[idx].want_focus = true;
        }
    }

    fn active_prompt(&self) -> bool {
        self.sessions.get(self.active).is_some_and(|s| s.shared.lock().prompt.is_some())
    }

    fn modal_open(&self) -> bool {
        self.editor.is_some()
            || self.show_settings
            || self.confirm_delete.is_some()
            || self.launcher.is_some()
            || self.active_prompt()
    }

    /// Time the dialog `key` was opened (records it on first call).
    fn opened_at(&mut self, key: &'static str, now: f64) -> f64 {
        *self.opened.entry(key).or_insert(now)
    }

    fn apply_style(&mut self, ctx: &egui::Context) {
        if self.applied_theme == self.cfg.settings.theme {
            return;
        }
        let t = self.theme();
        self.applied_theme = self.cfg.settings.theme.clone();
        self.palette = t.palette256();
        ctx.all_styles_mut(|style| {
            let v = &mut style.visuals;
            *v = egui::Visuals::dark();
            v.panel_fill = Color32::TRANSPARENT;
            v.window_fill = t.mantle;
            v.extreme_bg_color = t.crust;
            v.faint_bg_color = t.surface;
            v.code_bg_color = t.surface;
            v.override_text_color = Some(t.text);
            v.hyperlink_color = t.accent;
            v.selection.bg_fill = t.accent.gamma_multiply(0.35);
            v.selection.stroke = Stroke::new(1.0, t.accent);
            v.text_cursor.stroke = Stroke::new(2.0, t.accent);
            v.window_corner_radius = CornerRadius::same(16);
            v.window_stroke = Stroke::new(1.0, t.overlay);
            v.menu_corner_radius = CornerRadius::same(10);
            v.slider_trailing_fill = true;
            let r = CornerRadius::same(8);
            let w = &mut v.widgets;
            w.noninteractive.bg_fill = t.mantle;
            w.noninteractive.weak_bg_fill = t.mantle;
            w.noninteractive.bg_stroke = Stroke::new(1.0, t.overlay);
            w.noninteractive.fg_stroke = Stroke::new(1.0, t.subtext);
            w.noninteractive.corner_radius = r;
            w.inactive.bg_fill = t.overlay;
            w.inactive.weak_bg_fill = t.surface;
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, t.text);
            w.inactive.corner_radius = r;
            w.hovered.bg_fill = t.accent;
            w.hovered.weak_bg_fill = mix(t.surface, t.overlay, 0.6);
            w.hovered.bg_stroke = Stroke::new(1.0, t.accent);
            w.hovered.fg_stroke = Stroke::new(1.5, t.text);
            w.hovered.corner_radius = r;
            w.active.bg_fill = t.accent;
            w.active.weak_bg_fill = mix(t.surface, t.accent, 0.35);
            w.active.bg_stroke = Stroke::new(1.0, t.accent);
            w.active.fg_stroke = Stroke::new(1.5, t.text);
            w.active.corner_radius = r;
            w.open = w.hovered;
            style.spacing.item_spacing = vec2(8.0, 6.0);
            style.spacing.button_padding = vec2(12.0, 6.0);
            style.spacing.interact_size.y = 28.0;
            style.spacing.slider_width = 200.0;
            style.spacing.scroll.floating = true;
            style.spacing.scroll.bar_width = 6.0;
            style.visuals.selection.bg_fill = t.accent;
        });
    }

    // ------------------------------------------------------------ shortcuts

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        let (next, prev, close, dup, new_server, settings, launcher, sidebar, jump, streamer) = ctx.input_mut(|i| {
            let jump = digits.iter().position(|k| i.consume_key(Modifiers::CTRL, *k));
            (
                i.consume_key(Modifiers::CTRL, Key::Tab),
                i.consume_key(cs, Key::Tab),
                i.consume_key(cs, Key::W),
                i.consume_key(cs, Key::T),
                i.consume_key(cs, Key::N),
                i.consume_key(Modifiers::CTRL, Key::Comma),
                i.consume_key(Modifiers::CTRL, Key::K) || i.consume_key(cs, Key::P),
                i.consume_key(Modifiers::CTRL, Key::B),
                jump,
                i.consume_key(cs, Key::S),
            )
        });
        let n = self.sessions.len();
        if n > 0 && next {
            self.activate((self.active + 1) % n);
        }
        if n > 0 && prev {
            self.activate((self.active + n - 1) % n);
        }
        if let Some(j) = jump {
            self.activate(if j == 8 { n.saturating_sub(1) } else { j });
        }
        if n > 0 && close {
            self.close_tab(self.active);
        }
        if dup
            && let Some(s) = self.sessions.get(self.active) {
                let (label, spec) = (s.label.clone(), s.spec.clone());
                self.open_spec(ctx, label, spec);
            }
        if sidebar {
            self.sidebar_open = !self.sidebar_open;
        }
        if new_server {
            self.editor = Some(Editor::new(Server::new(), true));
        }
        if settings {
            self.show_settings = true;
        }
        if launcher {
            self.launcher = Some(Launcher { query: String::new(), selected: 0 });
        }
        if streamer {
            self.cfg.settings.streamer_mode = !self.cfg.settings.streamer_mode;
            self.notify(
                ctx,
                if self.cfg.settings.streamer_mode {
                    tr!("Streamer mode enabled")
                } else {
                    tr!("Streamer mode disabled")
                },
                false,
            );
            self.save(ctx);
        }
    }

    fn server_states(&self) -> HashMap<Uuid, Status> {
        let mut map: HashMap<Uuid, Status> = HashMap::new();
        for s in &self.sessions {
            if let Some(id) = s.spec.server_id {
                let st = s.status();
                let better = match (map.get(&id), &st) {
                    (None, _) => true,
                    (Some(Status::Connected), _) => false,
                    (_, Status::Connected) => true,
                    (Some(Status::Closed(_)), Status::Connecting) => true,
                    _ => false,
                };
                if better {
                    map.insert(id, st);
                }
            }
        }
        map
    }

    // ------------------------------------------------------------ chrome

    /// Snow falls only in winter (by the PC's local date) and only if the user wants it.
    fn snowing(&self) -> bool {
        self.cfg.settings.winter_snow && self.aurora.is_some() && platform::is_winter()
    }

    fn background(&self, ui: &egui::Ui, rect: Rect, maximized: bool) {
        let t = self.theme();
        let p = ui.painter();
        let alpha = self.layer_alpha().0;
        match &self.aurora {
            Some(a) => {
                // The night is always drawn; "animated background" only decides whether it moves.
                let moving = self.cfg.settings.animated_background && self.cfg.settings.animations;
                shader::paint(
                    p,
                    rect,
                    a,
                    AuroraParams {
                        time: if moving { self.clock } else { 7.0 },
                        base: t.crust,
                        c1: t.accent,
                        c2: t.accent2,
                        c3: t.ansi[5],
                        alpha: if self.blur_active { alpha } else { 1.0 },
                        intensity: 1.0,
                        snow: if self.snowing() { 1.0 } else { 0.0 },
                        snow_time: self.clock,
                    },
                );
            }
            None => {
                p.rect_filled(rect, 0.0, if self.blur_active { t.crust.gamma_multiply(alpha) } else { t.crust });
            }
        }
        if !maximized {
            p.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, Color32::from_white_alpha(14)), StrokeKind::Inside);
        }
    }

    fn titlebar(&mut self, ui: &mut egui::Ui, bar: Rect, maximized: bool) {
        let t = self.theme();
        let ctx = ui.ctx().clone();
        // The bar itself drags the window; widgets painted later sit on top of it.
        let drag = ui.interact(bar, Id::unique("titlebar"), Sense::click_and_drag());
        if drag.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }

        let p = ui.painter().clone();
        let cy = bar.center().y;
        // Everything in the bar shares one left edge with the cards below and one centre line.
        let edge = self.cfg.settings.gaps.clamp(0.0, 32.0).max(6.0);

        // Logo: the little ghost + wordmark.
        let gx = bar.min.x + edge + 16.0 + 20.0 * 0.37; // ghost left edge = sidebar content edge
        fx::ghost(&p, pos2(gx, cy), 20.0, t.accent, t.crust, self.clock, false);
        let word = fx::text(&p, pos2(gx + 16.0, cy), Align2::LEFT_CENTER, "haunt", fonts::display(16.0), t.text);

        // Window controls on the right: 46 x 32, flush with the edge like Windows' own.
        let btn = vec2(46.0, 32.0);
        let mut x = bar.max.x - 6.0;
        let mut place = |w: f32, h: f32| {
            x -= w;
            Rect::from_min_size(pos2(x, cy - h / 2.0), vec2(w, h))
        };
        let close_r = place(btn.x, btn.y);
        let max_r = place(btn.x, btn.y);
        let min_r = place(btn.x, btn.y);
        place(8.0, 0.0);
        let settings_r = place(32.0, 32.0);
        place(4.0, 0.0);
        let launch_r = place(32.0, 32.0);
        let controls_left = launch_r.min.x - 12.0;

        let mut bt = |r: Rect, ic: &str, danger: bool, tip: &str| {
            let resp = ui.scope_builder(UiBuilder::new().max_rect(r), |ui| widgets::icon_button(ui, t, ic, r.size(), danger)).inner;
            if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
        };
        if bt(close_r, icon::CLOSE, true, "").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if bt(max_r, if maximized { icon::RESTORE } else { icon::MAXIMIZE }, false, "").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
        if bt(min_r, icon::MINIMIZE, false, "").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        if bt(settings_r, icon::GEAR, false, tr!("Settings  (Ctrl+,)")).clicked() {
            self.show_settings = true;
        }
        if bt(launch_r, icon::SEARCH, false, tr!("Launcher  (Ctrl+K)")).clicked() {
            self.launcher = Some(Launcher { query: String::new(), selected: 0 });
        }

        // Tabs start one gap after the wordmark.
        let tabs_left = (word.max.x + 20.0).round();
        let area = Rect::from_min_max(pos2(tabs_left, bar.min.y), pos2(controls_left, bar.max.y));
        self.tabs(ui, area);
    }

    fn tabs(&mut self, ui: &mut egui::Ui, area: Rect) {
        let t = self.theme();
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let screen = ctx.content_rect();
        // Clip horizontally to the tab strip only, so the active tab's light is not sliced.
        let painter = ui.painter().with_clip_rect(Rect::from_min_max(pos2(area.min.x - 8.0, screen.min.y), pos2(area.max.x, screen.max.y)));
        let h = 32.0;
        let y0 = (area.center().y - h / 2.0).round();
        let mut x = area.min.x;
        let mut activate = None;
        let mut close = None;
        let n = self.sessions.len();
        let max_w = if n == 0 { 220.0 } else { ((area.width() - 44.0) / n as f32 - 4.0).clamp(84.0, 220.0) };
        let font = fonts::medium(13.0);

        for (i, s) in self.sessions.iter().enumerate() {
            let born = self.tab_born.get(&s.id).copied().unwrap_or(0.0);
            let grow = anim::wind(anim::since(&ctx, born, 0.45));
            let status = s.status();
            let galley = painter.layout_no_wrap(s.label.clone(), font.clone(), Color32::PLACEHOLDER);
            // dot(28) + text + close(28)
            let full = (galley.size().x + 28.0 + 32.0).min(max_w).round();
            let w = full * grow.clamp(0.0, 1.1);
            if w < 4.0 {
                x += w;
                continue;
            }
            let rect = Rect::from_min_size(pos2(x, y0), vec2(w, h));
            let id = Id::unique(("tab", s.id));
            let resp = ui.interact(rect, id, Sense::click());
            let active = i == self.active;
            let hov = anim::toggle(&ctx, id.with("h"), resp.hovered() || resp.contains_pointer(), 0.15);
            let act = anim::toggle(&ctx, id.with("a"), active, 0.25);

            if act > 0.01 {
                fx::plate(&painter, rect, h / 2.0, t.surface.gamma_multiply(0.85 * act), t.accent, act);
            } else if hov > 0.01 {
                painter.rect_filled(rect, CornerRadius::same(255), Color32::from_white_alpha((10.0 * hov) as u8));
            }

            let text_color = mix(mix(t.subtext, t.text, hov), t.text, act);
            let dot = match status {
                Status::Connected => t.ok,
                Status::Connecting | Status::Reconnecting(_) => t.warn,
                Status::Closed(_) => t.err,
            };
            let dot_c = pos2(rect.min.x + 16.0, rect.center().y);
            if matches!(status, Status::Connecting | Status::Reconnecting(_)) {
                let pulse = 0.5 + 0.5 * (now as f32 * 5.0).sin();
                painter.circle_filled(dot_c, 3.0 + pulse * 3.0, dot.gamma_multiply(0.25 * (1.0 - pulse)));
            } else if matches!(status, Status::Connected) {
                painter.circle_filled(dot_c, 6.0, dot.gamma_multiply(0.18));
            }
            painter.circle_filled(dot_c, 3.0, dot);
            let text_clip = Rect::from_min_max(pos2(rect.min.x + 28.0, rect.min.y), pos2(rect.max.x - 28.0, rect.max.y));
            let dy = fx::text_dy(&painter, &font);
            veil::text_auto(
                &painter.with_clip_rect(text_clip.intersect(area)),
                pos2(text_clip.min.x, rect.center().y + dy),
                Align2::LEFT_CENTER,
                &s.label,
                font.clone(),
                text_color,
                mix(t.mantle, t.surface, act),
            );

            // Close button appears on hover / active.
            let xr = Rect::from_center_size(pos2(rect.max.x - 16.0, rect.center().y), vec2(20.0, 20.0));
            let xresp = ui.interact(xr, id.with("x"), Sense::click());
            let show_x = hov.max(act * 0.6);
            if show_x > 0.02 {
                if xresp.hovered() {
                    painter.circle_filled(xr.center(), 10.0, Color32::from_white_alpha(18));
                }
                fx::glyph(&painter, xr.center(), icon::XMARK, fonts::mono(10.5), text_color.gamma_multiply(show_x));
            }
            if xresp.clicked() || resp.middle_clicked() {
                close = Some(i);
            } else if resp.clicked() {
                activate = Some(i);
            }
            let title = s.shared.lock().title.clone();
            let tip = format!("{}{}", s.spec.route(), title.map(|t| format!("\n{t}")).unwrap_or_default());
            resp.on_hover_text(tip);
            x += w + 4.0;
        }

        // "+" opens the launcher.
        let plus = Rect::from_min_size(pos2(x + 2.0, y0), vec2(h, h));
        if plus.max.x <= area.max.x {
            let resp = ui.interact(plus, Id::unique("tab-plus"), Sense::click());
            let hov = anim::toggle(&ctx, resp.id, resp.hovered(), 0.15);
            painter.rect_filled(plus, CornerRadius::same(255), Color32::from_white_alpha((4.0 + 12.0 * hov) as u8));
            fx::glyph(&painter, plus.center(), icon::PLUS, fonts::mono(12.0), mix(t.subtext, t.accent, hov));
            if resp.on_hover_text(tr!("New connection  (Ctrl+K)")).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.launcher = Some(Launcher { query: String::new(), selected: 0 });
            }
        }

        if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = activate {
            self.activate(i);
        }
    }

    fn resize_handles(&self, ui: &mut egui::Ui, win: Rect) {
        use egui::viewport::ResizeDirection as D;
        let e = RESIZE_EDGE;
        let c = e * 2.0;
        let handles = [
            (Rect::from_min_max(win.min, win.min + vec2(c, c)), D::NorthWest, CursorIcon::ResizeNwSe),
            (Rect::from_min_max(pos2(win.max.x - c, win.min.y), pos2(win.max.x, win.min.y + c)), D::NorthEast, CursorIcon::ResizeNeSw),
            (Rect::from_min_max(pos2(win.min.x, win.max.y - c), pos2(win.min.x + c, win.max.y)), D::SouthWest, CursorIcon::ResizeNeSw),
            (Rect::from_min_max(win.max - vec2(c, c), win.max), D::SouthEast, CursorIcon::ResizeNwSe),
            (Rect::from_min_max(win.min, pos2(win.max.x, win.min.y + e)), D::North, CursorIcon::ResizeVertical),
            (Rect::from_min_max(pos2(win.min.x, win.max.y - e), win.max), D::South, CursorIcon::ResizeVertical),
            (Rect::from_min_max(win.min, pos2(win.min.x + e, win.max.y)), D::West, CursorIcon::ResizeHorizontal),
            (Rect::from_min_max(pos2(win.max.x - e, win.min.y), win.max), D::East, CursorIcon::ResizeHorizontal),
        ];
        for (i, (r, dir, cursor)) in handles.into_iter().enumerate() {
            let resp = ui.interact(r, Id::unique(("resize", i)), Sense::drag()).on_hover_cursor(cursor);
            if resp.drag_started() {
                ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        }
    }

    // ------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ui: &mut egui::Ui, rect: Rect, appear: f32) {
        let t = self.theme();
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let radius = self.cfg.settings.rounding.clamp(0.0, 30.0);
        let p = ui.painter().clone();
        fx::shadow(&p, rect, radius, 0.7 * appear);
        fx::plate(&p, rect, radius, t.mantle.gamma_multiply(self.layer_alpha().1 * 0.94), t.accent, 0.0);

        // 16 px inner grid: header row, search, list, footer.
        let pad = 16.0;
        let inner = rect.shrink(pad);
        let mut connect: Option<Server> = None;
        let mut edit: Option<Server> = None;
        let mut delete: Option<Uuid> = None;
        let mut add = false;
        let mut open_settings = false;
        let states = self.server_states();
        let routes: HashMap<Uuid, String> = self.cfg.servers.iter().filter_map(|s| route_label(s, &self.cfg.servers).map(|r| (s.id, r))).collect();

        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            ui.multiply_opacity(appear);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

            // Header: title + count on the left, "+" on the right, one centre line.
            let (hr, _) = ui.allocate_exact_size(vec2(inner.width(), 28.0), Sense::hover());
            let hp = ui.painter();
            let title = fx::text(hp, pos2(hr.min.x + 2.0, hr.center().y), Align2::LEFT_CENTER, tr!("Servers"), fonts::bold(14.0), t.text);
            fx::text(
                hp,
                pos2(title.max.x + 8.0, hr.center().y),
                Align2::LEFT_CENTER,
                self.cfg.servers.len().to_string(),
                fonts::mono(11.5),
                t.subtext.gamma_multiply(0.7),
            );
            let plus = Rect::from_min_size(pos2(hr.max.x - 28.0, hr.min.y), vec2(28.0, 28.0));
            let pr = ui.scope_builder(UiBuilder::new().max_rect(plus), |ui| widgets::icon_button(ui, t, icon::PLUS, plus.size(), false)).inner;
            if pr.on_hover_text(tr!("New server  (Ctrl+Shift+N)")).clicked() {
                add = true;
            }
            ui.add_space(12.0);
            widgets::input(ui, t, &mut self.search, tr!("Search servers"), false, icon::SEARCH);
            ui.add_space(12.0);

            let foot_h = 44.0;
            let list_h = (ui.available_height() - foot_h).max(40.0);
            let query = self.search.to_lowercase();
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                let mut groups: Vec<&str> = Vec::new();
                for s in &self.cfg.servers {
                    if !groups.contains(&s.group.as_str()) {
                        groups.push(&s.group);
                    }
                }
                groups.sort_by_key(|g| (!g.is_empty(), g.to_lowercase()));
                let mut idx = 0usize;
                for g in groups {
                    let items: Vec<&Server> = self
                        .cfg
                        .servers
                        .iter()
                        .filter(|s| s.group == g)
                        .filter(|s| query.is_empty() || matches_query(s, &query))
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    if !g.is_empty() {
                        ui.add_space(8.0);
                        let (gr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                        let gp = ui.painter();
                        fx::glyph(gp, pos2(gr.min.x + 14.0, gr.center().y), icon::FOLDER, fonts::mono(10.5), t.accent.gamma_multiply(0.8));
                        let gt = fx::text(gp, pos2(gr.min.x + 28.0, gr.center().y), Align2::LEFT_CENTER, g.to_uppercase(), fonts::bold(10.5), t.subtext);
                        fx::text(gp, pos2(gt.max.x + 6.0, gr.center().y), Align2::LEFT_CENTER, items.len().to_string(), fonts::mono(10.0), t.subtext.gamma_multiply(0.55));
                    }
                    for s in items {
                        let start = self.started.unwrap_or(now) + 0.15 + idx as f64 * 0.045;
                        let k = anim::since(&ctx, start, 0.5);
                        idx += 1;
                        match server_row(ui, t, s, states.get(&s.id), routes.get(&s.id).map(String::as_str), k) {
                            Some(RowAction::Connect) => connect = Some(s.clone()),
                            Some(RowAction::Edit) => edit = Some(s.clone()),
                            Some(RowAction::Delete) => delete = Some(s.id),
                            None => {}
                        }
                    }
                }
                if self.cfg.servers.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(tr!("No servers yet")).font(fonts::ui(13.0)).color(t.subtext));
                        ui.add_space(10.0);
                        if widgets::secondary(ui, t, icon::PLUS, tr!("Add one"), vec2(132.0, widgets::CONTROL_H)).clicked() {
                            add = true;
                        }
                    });
                } else if idx == 0 {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new(tr!("Nothing matches")).font(fonts::ui(13.0)).color(t.subtext)));
                }
            });

            // Footer: hairline across the plate, then gear + shortcut on one line.
            let foot = Rect::from_min_max(pos2(inner.min.x, inner.max.y - 32.0), inner.max);
            ui.painter().line_segment(
                [pos2(rect.min.x + 1.0, foot.min.y - 12.0), pos2(rect.max.x - 1.0, foot.min.y - 12.0)],
                Stroke::new(1.0, Color32::from_white_alpha(10)),
            );
            let gear = Rect::from_min_size(pos2(foot.min.x, foot.center().y - 16.0), vec2(32.0, 32.0));
            let gr = ui.scope_builder(UiBuilder::new().max_rect(gear), |ui| widgets::icon_button(ui, t, icon::GEAR, gear.size(), false)).inner;
            if gr.on_hover_text(tr!("Settings  (Ctrl+,)")).clicked() {
                open_settings = true;
            }
            let fp = ui.painter();
            let line = Color32::from_white_alpha(22);
            let mut x = gear.max.x + 10.0;
            x += fx::kbd(fp, pos2(x, foot.center().y), "Ctrl", t.subtext, line) + 4.0;
            x += fx::kbd(fp, pos2(x, foot.center().y), "K", t.subtext, line) + 8.0;
            fx::text(fp, pos2(x, foot.center().y), Align2::LEFT_CENTER, tr!("launcher"), fonts::ui(12.0), t.subtext.gamma_multiply(0.8));
        });

        if add {
            self.editor = Some(Editor::new(Server::new(), true));
        }
        if open_settings {
            self.show_settings = true;
        }
        if let Some(s) = edit {
            self.editor = Some(Editor::new(s, false));
        }
        if let Some(id) = delete {
            self.confirm_delete = Some(id);
        }
        if let Some(s) = connect {
            self.connect(&ctx, &s);
        }
    }

    // ------------------------------------------------------------ welcome

    fn welcome(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme();
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let start = self.started.unwrap_or(now);
        let radius = self.cfg.settings.rounding.clamp(0.0, 30.0);
        let p = ui.painter().clone();
        fx::shadow(&p, rect, radius, 0.7);
        fx::plate(&p, rect, radius, t.mantle.gamma_multiply(self.layer_alpha().1 * 0.82), t.accent, 0.0);

        let stagger = |i: usize| {
            let k = anim::since(&ctx, start + 0.08 + i as f64 * 0.07, 0.6);
            (1.0 - anim::wind(k), anim::smooth(k.min(1.0)))
        };

        // Recent first (last connect), then the rest in saved order.
        let mut recent: Vec<Server> = self.cfg.servers.clone();
        recent.sort_by_key(|s| std::cmp::Reverse(s.last_used.unwrap_or(0)));
        let max_rows = (((rect.height() - 330.0) / 52.0).floor() as usize).clamp(1, 7);
        recent.truncate(max_rows);
        let states = self.server_states();

        // One centred column on the 4 px grid.
        let col_w = (rect.width() - 96.0).clamp(300.0, 680.0).round();
        let x0 = (rect.center().x - col_w / 2.0).round();
        let x1 = x0 + col_w;
        let list_h = if recent.is_empty() { 132.0 } else { 28.0 + recent.len() as f32 * 52.0 };
        let content_h = 52.0 + 28.0 + 56.0 + 36.0 + list_h;
        let mut y = (rect.min.y + ((rect.height() - content_h - 40.0) / 2.0).max(28.0)).round();

        let mut new_server = false;
        let mut launcher = false;
        let mut import = None;
        let mut connect: Option<Server> = None;
        let mut quick_go = false;

        // ---- Header: title and summary on the left, "New server" on the right.
        let (dy, a) = stagger(0);
        let mut hp = p.clone();
        hp.multiply_opacity(a);
        let hy = y + dy * 12.0;
        fx::text(&hp, pos2(x0, hy + 14.0), Align2::LEFT_CENTER, tr!("Connect"), fonts::display(26.0), t.text);
        let open = self.sessions.len();
        let summary = match (self.cfg.servers.len(), open) {
            (0, _) => tr!("Add a server or type an address below").to_string(),
            (n, _) => tr!("{n} saved {n:server|servers}", n = n),
        };
        fx::text(&hp, pos2(x0, hy + 42.0), Align2::LEFT_CENTER, summary, fonts::ui(13.0), t.subtext);
        let nb_w = widgets::button_width(ui, icon::PLUS, tr!("New server")).max(140.0);
        let nb = Rect::from_min_size(pos2(x1 - nb_w, hy + 10.0), vec2(nb_w, widgets::CONTROL_H));
        ui.scope_builder(UiBuilder::new().max_rect(nb), |ui| {
            ui.multiply_opacity(a);
            if widgets::secondary(ui, t, icon::PLUS, tr!("New server"), nb.size()).clicked() {
                new_server = true;
            }
        });
        y += 52.0 + 28.0;

        // ---- Quick connect bar: type an address or a saved name, Enter.
        let (dy, a) = stagger(1);
        let bar = Rect::from_min_size(pos2(x0, y + dy * 12.0), vec2(col_w, 56.0));
        let edit_id = Id::unique("welcome-quick");
        let focused = ctx.memory(|m| m.has_focus(edit_id));
        let f = anim::toggle(&ctx, edit_id.with("f"), focused, 0.2);
        let mut bp = p.clone();
        bp.multiply_opacity(a);
        if f > 0.0 {
            fx::glow(&bp, bar, 16.0, t.accent.gamma_multiply(0.16 * f), 24.0, 0.0);
        }
        fx::plate(&bp, bar, 16.0, Color32::from_black_alpha(110), t.accent, 0.35 + 0.65 * f);
        fx::glyph(&bp, pos2(bar.min.x + 26.0, bar.center().y), icon::BOLT, fonts::mono(14.0), mix(t.subtext, t.accent, 0.4 + 0.6 * f));
        let btn_w = widgets::button_width(ui, icon::ARROW, tr!("Connect")).max(116.0);
        let btn = Rect::from_min_size(pos2(bar.max.x - 8.0 - btn_w, bar.center().y - 20.0), vec2(btn_w, 40.0));
        let font = fonts::mono(14.0);
        let tdy = fx::text_dy(&bp, &font);
        let field = Rect::from_min_max(pos2(bar.min.x + 48.0, bar.min.y), pos2(btn.min.x - 12.0, bar.max.y)).translate(vec2(0.0, tdy));
        let resp = ui
            .scope_builder(UiBuilder::new().max_rect(field), |ui| {
                ui.multiply_opacity(a);
                ui.add_sized(
                    field.size(),
                    egui::TextEdit::singleline(&mut self.quick)
                        .id(edit_id)
                        .hint_text(RichText::new(tr!("user@host:port  or a server name")).font(font.clone()).color(t.subtext.gamma_multiply(0.55)))
                        .font(font.clone())
                        .text_color(t.text)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::ZERO)
                        .align(Align2::LEFT_CENTER),
                )
            })
            .inner;
        if resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
            quick_go = true;
        }
        ui.scope_builder(UiBuilder::new().max_rect(btn), |ui| {
            ui.multiply_opacity(a);
            if widgets::primary(ui, t, icon::ARROW, tr!("Connect"), btn.size()).clicked() {
                quick_go = true;
            }
        });
        y += 56.0 + 36.0;

        // ---- Recent servers: a quiet table, one row per server.
        let (dy, a) = stagger(2);
        let mut lp = p.clone();
        lp.multiply_opacity(a);
        let ly = y + dy * 12.0;
        if !recent.is_empty() {
            fx::text(&lp, pos2(x0 + 4.0, ly + 10.0), Align2::LEFT_CENTER, tr!("RECENT"), fonts::bold(11.0), t.subtext);
            let line = Color32::from_white_alpha(20);
            let label = tr!("to search everything");
            let lw = lp.layout_no_wrap(label.into(), fonts::ui(12.0), Color32::PLACEHOLDER).size().x;
            let mut kx = x1 - 4.0 - lw - 8.0 - kbd_w(&lp, "K") - 4.0 - kbd_w(&lp, "Ctrl");
            kx += fx::kbd(&lp, pos2(kx, ly + 10.0), "Ctrl", t.subtext, line) + 4.0;
            kx += fx::kbd(&lp, pos2(kx, ly + 10.0), "K", t.subtext, line) + 8.0;
            fx::text(&lp, pos2(kx, ly + 10.0), Align2::LEFT_CENTER, label, fonts::ui(12.0), t.subtext.gamma_multiply(0.8));

            let now_s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            for (i, s) in recent.iter().enumerate() {
                let (rdy, ra) = stagger(3 + i);
                let r = Rect::from_min_size(pos2(x0, ly + 28.0 + i as f32 * 52.0 + rdy * 10.0), vec2(col_w, 52.0));
                let resp = ui.interact(r, Id::unique(("recent", s.id)), Sense::click());
                let h = anim::toggle(&ctx, resp.id, resp.hovered(), 0.16);
                let mut rp = p.clone();
                rp.multiply_opacity(ra);
                if h > 0.0 {
                    fx::plate(&rp, r, 14.0, Color32::from_white_alpha((8.0 * h) as u8), t.accent, h * 0.8);
                } else if i > 0 {
                    rp.line_segment([pos2(r.min.x + 40.0, r.min.y), pos2(r.max.x - 16.0, r.min.y)], Stroke::new(1.0, Color32::from_white_alpha(9)));
                }
                let cy = r.center().y;
                status_dot(&rp, t, pos2(r.min.x + 20.0, cy), states.get(&s.id));
                let tx = r.min.x + 40.0;
                // Right side: time since last connect, then the arrow on hover.
                let ago = s.last_used.map(|u| ago(now_s.saturating_sub(u))).unwrap_or_default();
                let mut right = r.max.x - 16.0;
                fx::glyph(&rp, pos2(right - 6.0 + 4.0 * (1.0 - h), cy), icon::ARROW, fonts::mono(12.0), t.accent.gamma_multiply(h));
                right -= 24.0;
                if !ago.is_empty() {
                    let ar = fx::text(&rp, pos2(right, cy), Align2::RIGHT_CENTER, &ago, fonts::mono(11.5), t.subtext.gamma_multiply(0.8));
                    right = ar.min.x - 12.0;
                }
                if let Some(route) = route_label(s, &self.cfg.servers) {
                    let cw = chip(&rp, t, pos2(right, cy), &route);
                    right -= cw + 12.0;
                }
                let clip = rp.with_clip_rect(Rect::from_min_max(pos2(tx, r.min.y), pos2(right, r.max.y)).intersect(rp.clip_rect()));
                let nf = fonts::medium(14.0);
                let mf = fonts::mono(12.0);
                veil::text_auto(&clip, pos2(tx, cy - 9.0 + fx::text_dy(&rp, &nf)), Align2::LEFT_CENTER, s.display_name(), nf, t.text, t.mantle);
                veil::text(&clip, pos2(tx, cy + 10.0 + fx::text_dy(&rp, &mf)), Align2::LEFT_CENTER, address(s), mf, t.subtext, t.mantle);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    connect = Some(s.clone());
                }
            }
        } else {
            // First run: bring sessions over from PuTTY / OpenSSH.
            let box_r = Rect::from_min_size(pos2(x0, ly), vec2(col_w, list_h));
            fx::plate(&lp, box_r, 16.0, Color32::from_white_alpha(4), t.accent, 0.0);
            fx::text(&lp, pos2(x0 + 24.0, ly + 34.0), Align2::LEFT_CENTER, tr!("Already have sessions?"), fonts::bold(14.0), t.text);
            fx::text(&lp, pos2(x0 + 24.0, ly + 56.0), Align2::LEFT_CENTER, tr!("Hosts, users, keys and jump hosts come over as they are."), fonts::ui(12.5), t.subtext);
            let row = Rect::from_min_size(pos2(x0 + 24.0, ly + 76.0), vec2(col_w - 48.0, widgets::CONTROL_H));
            ui.scope_builder(UiBuilder::new().max_rect(row), |ui| {
                ui.multiply_opacity(a);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if widgets::secondary(ui, t, icon::FOLDER, tr!("Import PuTTY"), vec2(156.0, widgets::CONTROL_H)).clicked() {
                        import = Some(ImportSource::Putty);
                    }
                    if widgets::secondary(ui, t, icon::KEY, tr!("Import ~/.ssh/config"), vec2(196.0, widgets::CONTROL_H)).clicked() {
                        import = Some(ImportSource::SshConfig);
                    }
                });
            });
        }

        // ---- Shortcut strip along the bottom edge.
        let (_, a) = stagger(6);
        let mut sp = p.clone();
        sp.multiply_opacity(a * 0.9);
        let hints: [(&[&str], &str); 4] = [
            (&["Ctrl", "K"], tr!("launcher")),
            (&["Ctrl", "Shift", "N"], tr!("new server")),
            (&["Ctrl", "B"], tr!("sidebar")),
            (&["Ctrl", "Shift", "S"], tr!("streamer mode")),
        ];
        let lf = fonts::ui(12.0);
        let width: f32 = hints
            .iter()
            .map(|(keys, l)| {
                keys.iter().map(|k| kbd_w(&sp, k) + 4.0).sum::<f32>() + 4.0 + sp.layout_no_wrap(l.to_string(), lf.clone(), Color32::PLACEHOLDER).size().x
            })
            .sum::<f32>()
            + 28.0 * (hints.len() - 1) as f32;
        if width < rect.width() - 48.0 {
            let cy = rect.max.y - 26.0;
            let mut x = (rect.center().x - width / 2.0).round();
            let line = Color32::from_white_alpha(18);
            for (keys, l) in hints {
                for k in keys {
                    x += fx::kbd(&sp, pos2(x, cy), k, t.subtext.gamma_multiply(0.85), line) + 4.0;
                }
                x += 4.0;
                let r = fx::text(&sp, pos2(x, cy), Align2::LEFT_CENTER, l, lf.clone(), t.subtext.gamma_multiply(0.7));
                x = r.max.x + 28.0;
            }
        }

        if quick_go {
            let q = self.quick.trim().to_string();
            if let Some(s) = self.cfg.servers.iter().find(|s| s.display_name().eq_ignore_ascii_case(&q)).cloned() {
                self.quick.clear();
                connect = Some(s);
            } else if let Some((user, host, port)) = parse_quick(&q) {
                self.quick.clear();
                let spec = ConnectSpec {
                    server_id: None,
                    host: host.clone(),
                    port,
                    user: user.clone(),
                    auth: Auth::Password,
                    keepalive: Duration::from_secs(self.cfg.settings.keepalive_secs),
                    via: None,
                    reconnect: false,
                };
                self.open_spec(&ctx, format!("{user}@{host}"), spec);
            } else if !q.is_empty() {
                self.notify(&ctx, tr!("Type user@host[:port] or the name of a saved server"), true);
            } else {
                launcher = true;
            }
        }
        if new_server {
            self.editor = Some(Editor::new(Server::new(), true));
        }
        if launcher {
            self.launcher = Some(Launcher { query: String::new(), selected: 0 });
        }
        if let Some(s) = connect {
            self.connect(&ctx, &s);
        }
        if let Some(src) = import {
            self.import_sessions(&ctx, src);
        }
    }

    // ------------------------------------------------------------ dialogs

    fn prompt_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let now = ctx.input(|i| i.time);
        let Some(session) = self.sessions.get(self.active) else { return };
        let info = {
            let sh = session.shared.lock();
            sh.prompt.as_ref().map(|p| (p.kind, p.title.clone(), p.message.clone()))
        };
        let Some((kind, title, message)) = info else {
            self.opened.remove("prompt");
            return;
        };
        let sid = session.id;
        let can_remember = session.spec.server_id.is_some();
        let opened = self.opened_at("prompt", now);
        let mut answer: Option<Option<(String, bool)>> = None;

        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique(("prompt", sid)), opened, 440.0, |ui| {
            let ic = match kind {
                PromptKind::HostKey => icon::SHIELD,
                PromptKind::Password => icon::LOCK,
                PromptKind::Passphrase => icon::KEY,
            };
            dialog_header(ui, t, ic, &title);
            match kind {
                PromptKind::HostKey => {
                    egui::Frame::new()
                        .fill(t.crust)
                        .corner_radius(10)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            veiled_label(ui, &message, fonts::mono(12.0), t.subtext, t.crust);
                        });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if widgets::primary(ui, t, icon::SHIELD, tr!("Trust and connect"), vec2(190.0, 36.0)).clicked() {
                            answer = Some(Some(("accept".into(), false)));
                        }
                        if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                            answer = Some(None);
                        }
                    });
                }
                PromptKind::Password | PromptKind::Passphrase => {
                    veiled_label(ui, &message, fonts::ui(13.0), t.subtext, t.mantle);
                    ui.add_space(10.0);
                    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    let edit = widgets::input(ui, t, &mut self.prompt_input, "••••••••", true, icon::LOCK);
                    edit.request_focus();
                    if can_remember {
                        ui.add_space(6.0);
                        widgets::toggle(ui, t, &mut self.prompt_remember, tr!("Remember"), tr!("Stored in Windows Credential Manager"));
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if widgets::primary(ui, t, icon::ARROW, tr!("Connect"), vec2(130.0, 36.0)).clicked() || enter {
                            answer = Some(Some((std::mem::take(&mut self.prompt_input), self.prompt_remember && can_remember)));
                        }
                        if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                            answer = Some(None);
                        }
                    });
                }
            }
        });
        if dismissed && answer.is_none() {
            answer = Some(None);
        }
        if let Some(a) = answer {
            self.prompt_input.clear();
            self.opened.remove("prompt");
            if let Some(s) = self.sessions.get_mut(self.active) {
                if let Some(p) = s.shared.lock().prompt.take() {
                    let _ = p.reply.send(a);
                }
                s.want_focus = true;
            }
        }
    }

    fn editor_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let now = ctx.input(|i| i.time);
        if self.editor.is_none() {
            self.opened.remove("editor");
            return;
        }
        let opened = self.opened_at("editor", now);
        let Some(ed) = self.editor.as_mut() else { return };
        let mut save = false;
        let mut save_connect = false;
        let mut cancel = false;
        // Other saved servers, for the jump host picker (not this one, not ones that jump through it).
        let me = ed.server.id;
        let jumpable: Vec<(Uuid, String, String)> = self
            .cfg
            .servers
            .iter()
            .filter(|s| s.id != me && !matches!(s.proxy, Proxy::Jump { server } if server == me))
            .map(|s| (s.id, s.display_name().to_string(), format!("{}@{}:{}", s.user, s.host, s.port)))
            .collect();
        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("editor"), opened, 520.0, |ui| {
            dialog_header(ui, t, icon::SERVER, if ed.is_new { tr!("New server") } else { tr!("Edit server") });
            widgets::field(ui, t, tr!("Name"), &mut ed.server.name, tr!("My server"), false);
            ui.add_space(4.0);
            two_cols(ui, 0.72, |ui| {
                widgets::veiled_field(ui, t, tr!("Host"), &mut ed.server.host, tr!("example.com or 203.0.113.5"));
            }, |ui| {
                widgets::field(ui, t, tr!("Port"), &mut ed.port, "22", false);
            });
            ui.add_space(4.0);
            two_cols(ui, 0.5, |ui| {
                widgets::field(ui, t, tr!("User"), &mut ed.server.user, "root", false);
            }, |ui| {
                widgets::field(ui, t, tr!("Group"), &mut ed.server.group, tr!("optional"), false);
            });

            widgets::section(ui, t, icon::KEY, tr!("Authentication"));
            let mut mode = ed.use_key as usize;
            if widgets::segmented(ui, t, Id::unique("auth-seg"), &mut mode, &[tr!("Password"), tr!("Private key")]).changed() {
                ed.use_key = mode == 1;
            }
            ui.add_space(4.0);
            if ed.use_key {
                let browse_w = widgets::button_width(ui, icon::FOLDER, tr!("Browse")).max(112.0);
                let mut browse = false;
                two_cols_fixed(ui, browse_w, |ui| {
                    widgets::field(ui, t, tr!("Key file"), &mut ed.key_path, tr!(".ppk, OpenSSH or PEM"), false);
                }, |ui| {
                    // Skip exactly the label row so the button sits level with the input.
                    ui.add_space(widgets::LABEL_H);
                    browse = widgets::secondary(ui, t, icon::FOLDER, tr!("Browse"), vec2(browse_w, widgets::CONTROL_H)).clicked();
                });
                if browse
                    && let Some(p) = rfd::FileDialog::new().set_title(tr!("Choose a private key")).pick_file() {
                        ed.key_path = p.display().to_string();
                    }
                ui.add_space(4.0);
                widgets::field(ui, t, tr!("Passphrase"), &mut ed.secret, tr!("empty = ask if needed"), true);
            } else {
                let hint = if ed.is_new { tr!("empty = ask on connect") } else { tr!("empty = keep current") };
                widgets::field(ui, t, tr!("Password"), &mut ed.secret, hint, true);
            }

            widgets::section(ui, t, icon::ROUTE, tr!("Route"));
            widgets::segmented(ui, t, Id::unique("proxy-seg"), &mut ed.proxy_kind, &[tr!("Direct"), "SOCKS5", "HTTP", tr!("Jump host")]);
            ui.add_space(4.0);
            match ed.proxy_kind {
                1 | 2 => {
                    two_cols(ui, 0.72, |ui| {
                        widgets::veiled_field(ui, t, tr!("Proxy host"), &mut ed.proxy_host, "127.0.0.1");
                    }, |ui| {
                        let def = if ed.proxy_kind == 1 { "1080" } else { "3128" };
                        widgets::field(ui, t, tr!("Port"), &mut ed.proxy_port, def, false);
                    });
                    ui.add_space(4.0);
                    two_cols(ui, 0.5, |ui| {
                        widgets::field(ui, t, tr!("Proxy user"), &mut ed.proxy_user, tr!("optional"), false);
                    }, |ui| {
                        let hint = if ed.is_new { tr!("empty = ask") } else { tr!("empty = keep") };
                        widgets::field(ui, t, tr!("Proxy password"), &mut ed.proxy_secret, hint, true);
                    });
                    hint_line(ui, t, if ed.proxy_kind == 1 {
                        tr!("The proxy resolves the host name, so DNS does not leak locally.")
                    } else {
                        tr!("Uses HTTP CONNECT; the proxy must allow the SSH port.")
                    });
                }
                3 => {
                    if jumpable.is_empty() {
                        hint_line(ui, t, tr!("Save the jump server first, then pick it here."));
                    } else {
                        egui::ScrollArea::vertical().max_height(46.0 * 3.0).auto_shrink([false, true]).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for (id, name, addr) in &jumpable {
                                if widgets::choice_row(ui, t, ed.jump == Some(*id), name, addr).clicked() {
                                    ed.jump = Some(*id);
                                }
                            }
                        });
                        hint_line(ui, t, tr!("haunt logs in to that server first and tunnels through it (like ProxyJump). Jump servers can chain."));
                    }
                }
                _ => {}
            }

            ui.add_space(8.0);
            hint_line(ui, t, tr!("Secrets live in Windows Credential Manager, never in the config file."));
            if let Some(e) = &ed.error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).font(fonts::ui(13.0)).color(t.err));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if widgets::primary(ui, t, icon::BOLT, tr!("Save and connect"), vec2(184.0, widgets::CONTROL_H)).clicked() {
                    save = true;
                    save_connect = true;
                }
                if widgets::secondary(ui, t, "", tr!("Save"), vec2(88.0, widgets::CONTROL_H)).clicked() {
                    save = true;
                }
                if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(88.0, widgets::CONTROL_H)).clicked() {
                    cancel = true;
                }
            });
        });
        if dismissed {
            cancel = true;
        }

        if save {
            let ed = self.editor.as_mut().unwrap();
            ed.server.host = ed.server.host.trim().to_string();
            ed.server.user = ed.server.user.trim().to_string();
            ed.server.group = ed.server.group.trim().to_string();
            let key_path = ed.key_path.trim().trim_matches('"').to_string();
            let port = ed.port.trim().parse::<u16>().ok().filter(|p| *p > 0);
            ed.error = if ed.server.host.is_empty() {
                Some(tr!("Host is required.").into())
            } else if ed.server.host.contains(char::is_whitespace) {
                Some(tr!("Host must not contain spaces.").into())
            } else if port.is_none() {
                Some(tr!("Port must be a number from 1 to 65535.").into())
            } else if ed.server.user.is_empty() {
                Some(tr!("User is required.").into())
            } else if ed.use_key && key_path.is_empty() {
                Some(tr!("Choose a key file.").into())
            } else if ed.use_key && !PathBuf::from(&key_path).is_file() {
                Some(tr!("Key file not found.").into())
            } else if matches!(ed.proxy_kind, 1 | 2) && ed.proxy_host.trim().is_empty() {
                Some(tr!("Proxy host is required.").into())
            } else if matches!(ed.proxy_kind, 1 | 2)
                && !ed.proxy_port.trim().is_empty()
                && ed.proxy_port.trim().parse::<u16>().ok().filter(|p| *p > 0).is_none()
            {
                Some(tr!("Proxy port must be a number from 1 to 65535.").into())
            } else if ed.proxy_kind == 3 && ed.jump.is_none() {
                Some(tr!("Pick a jump server.").into())
            } else {
                None
            };
            if ed.error.is_some() {
                return;
            }
            ed.server.port = port.unwrap_or(22);
            ed.server.auth = if ed.use_key { Auth::Key { path: PathBuf::from(key_path) } } else { Auth::Password };
            let p_port = |def: u16| ed.proxy_port.trim().parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(def);
            ed.server.proxy = match ed.proxy_kind {
                1 => Proxy::Socks5 { host: ed.proxy_host.trim().into(), port: p_port(1080), user: ed.proxy_user.trim().into() },
                2 => Proxy::Http { host: ed.proxy_host.trim().into(), port: p_port(3128), user: ed.proxy_user.trim().into() },
                3 => ed.jump.map_or(Proxy::None, |server| Proxy::Jump { server }),
                _ => Proxy::None,
            };
            let server = ed.server.clone();
            let secret = std::mem::take(&mut ed.secret);
            let proxy_secret = std::mem::take(&mut ed.proxy_secret);
            let proxy_auth = matches!(ed.proxy_kind, 1 | 2) && !ed.proxy_user.trim().is_empty();
            self.editor = None;
            if !secret.is_empty()
                && let Err(e) = secrets::set(server.id, &secret) {
                    self.notify(ctx, tr!("Could not store the secret: {e}", e = e), true);
                }
            if !proxy_auth {
                secrets::delete_proxy(server.id);
            } else if !proxy_secret.is_empty()
                && let Err(e) = secrets::set_proxy(server.id, &proxy_secret) {
                    self.notify(ctx, tr!("Could not store the proxy password: {e}", e = e), true);
                }
            match self.cfg.servers.iter_mut().find(|s| s.id == server.id) {
                Some(existing) => *existing = server.clone(),
                None => self.cfg.servers.push(server.clone()),
            }
            self.save(ctx);
            if save_connect {
                self.connect(ctx, &server);
            }
        } else if cancel {
            self.editor = None;
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let now = ctx.input(|i| i.time);
        let Some(id) = self.confirm_delete else {
            self.opened.remove("delete");
            return;
        };
        let name = self.cfg.servers.iter().find(|s| s.id == id).map(|s| s.display_name().to_string());
        let Some(name) = name else {
            self.confirm_delete = None;
            return;
        };
        let opened = self.opened_at("delete", now);
        let mut result = None;
        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("delete"), opened, 360.0, |ui| {
            dialog_header(ui, t, icon::TRASH, &tr!("Delete “{name}”?", name = name));
            ui.label(RichText::new(tr!("The server and its saved secret are removed.")).color(t.subtext));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if widgets::danger(ui, t, tr!("Delete"), vec2(110.0, 36.0)).clicked() {
                    result = Some(true);
                }
                if widgets::secondary(ui, t, "", tr!("Cancel"), vec2(100.0, 36.0)).clicked() {
                    result = Some(false);
                }
            });
        });
        if dismissed {
            result.get_or_insert(false);
        }
        if let Some(yes) = result {
            self.confirm_delete = None;
            if yes {
                self.cfg.servers.retain(|s| s.id != id);
                secrets::delete(id);
                self.save(ctx);
            }
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if !self.show_settings {
            self.opened.remove("settings");
            return;
        }
        let t = self.theme();
        let opened = self.opened_at("settings", now);
        let mut close = false;
        let mut import = None;
        let max_h = (ctx.content_rect().height() - 160.0).clamp(240.0, 620.0);
        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("settings"), opened, 560.0, |ui| {
            ui.horizontal(|ui| {
                dialog_header(ui, t, icon::SLIDERS, tr!("Settings"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if widgets::icon_button(ui, t, icon::XMARK, vec2(30.0, 28.0), false).clicked() {
                        close = true;
                    }
                });
            });
            #[allow(unused_mut)]
            let mut scroll = egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]);
            // Debug builds: HAUNT_PREVIEW_SCROLL=<px> pins the settings scroll for screenshot checks.
            #[cfg(debug_assertions)]
            if let Some(y) = std::env::var("HAUNT_PREVIEW_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
                scroll = scroll.vertical_scroll_offset(y);
            }
            scroll.show(ui, |ui| {
                ui.set_width(ui.available_width() - 10.0);
                widgets::section(ui, t, icon::GLOBE, tr!("Language"));
                {
                    let w = ui.available_width();
                    let cols = 3;
                    let gap = 8.0;
                    let cw = ((w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                    let system = tr!("System ({lang})", lang = crate::i18n::system().native_name());
                    let mut options: Vec<(String, String)> = vec![(String::new(), system)];
                    options.extend(crate::i18n::Lang::ALL.iter().map(|l| (l.code().to_string(), l.native_name().to_string())));
                    let mut picked = None;
                    for chunk in options.chunks(cols) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for (code, name) in chunk {
                                let selected = self.cfg.settings.language == *code;
                                if widgets::option_tile(ui, t, selected, name, vec2(cw, widgets::CONTROL_H)).clicked() {
                                    picked = Some(code.clone());
                                }
                            }
                        });
                        ui.add_space(gap - ui.spacing().item_spacing.y);
                    }
                    if let Some(code) = picked {
                        self.cfg.settings.language = code;
                        crate::i18n::set(&self.cfg.settings.language);
                    }
                }

                widgets::section(ui, t, icon::BRUSH, tr!("Theme"));
                let w = ui.available_width();
                let cols = 3;
                let gap = 10.0;
                let cw = (w - gap * (cols as f32 - 1.0)) / cols as f32;
                let themes = theme::all();
                let current_theme = theme::by_name(&self.cfg.settings.theme);
                for chunk in themes.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for th in chunk {
                            let selected = std::ptr::eq(th, current_theme);
                            if theme_card(ui, th, selected, vec2(cw, 82.0)).clicked() {
                                self.cfg.settings.theme = th.name.to_string();
                            }
                        }
                    });
                    ui.add_space(gap - 6.0);
                }

                let s = &mut self.cfg.settings;
                widgets::section(ui, t, icon::GHOST, tr!("Window"));
                slider(ui, t, tr!("Opacity"), &mut s.opacity, 0.3..=1.0, 0.01, "");
                widgets::toggle(ui, t, &mut s.blur, tr!("Acrylic blur"), tr!("Blur the desktop behind the window"));
                widgets::toggle(ui, t, &mut s.animated_background, tr!("Animated sky"), tr!("Drifting fog and twinkling stars"));
                widgets::toggle(ui, t, &mut s.winter_snow, tr!("Winter snow"), tr!("Falls from December to February, by your PC's date"));
                widgets::toggle(ui, t, &mut s.border_animation, tr!("Animated border"), tr!("Rotating gradient around the active pane"));
                widgets::toggle(ui, t, &mut s.animations, tr!("Animations"), tr!("Turn off for a static, minimal UI"));
                widgets::toggle(ui, t, &mut s.streamer_mode, tr!("Streamer mode"), tr!("Mask IPs, hosts and tokens from screen capture (OBS, Discord)"));

                widgets::section(ui, t, icon::TERMINAL, tr!("Terminal"));
                slider(ui, t, tr!("Font size"), &mut s.font_size, 9.0..=28.0, 0.5, "pt");
                slider(ui, t, tr!("Line height"), &mut s.line_height, 1.0..=1.8, 0.05, "");
                ui.add_space(4.0);
                ui.label(RichText::new(tr!("Cursor")).size(12.0).color(t.subtext));
                let mut cur = match s.cursor {
                    CursorStyle::Block => 0,
                    CursorStyle::Beam => 1,
                    CursorStyle::Underline => 2,
                };
                if widgets::segmented(ui, t, Id::unique("cursor-seg"), &mut cur, &[tr!("Block"), tr!("Beam"), tr!("Underline")]).changed() {
                    s.cursor = [CursorStyle::Block, CursorStyle::Beam, CursorStyle::Underline][cur];
                }
                ui.add_space(4.0);
                widgets::toggle(ui, t, &mut s.cursor_blink, tr!("Cursor blink"), tr!("Smooth fade, stops after 15 s idle"));
                widgets::toggle(ui, t, &mut s.cursor_trail, tr!("Cursor trail"), tr!("The cursor glides between cells"));
                widgets::toggle(ui, t, &mut s.bold_is_bright, tr!("Bold is bright"), tr!("Bold text uses the bright palette"));
                let mut sb = s.scrollback as f32;
                slider(ui, t, tr!("Scrollback"), &mut sb, 1000.0..=100_000.0, 1000.0, tr!("lines, new tabs"));
                s.scrollback = sb as usize;

                widgets::section(ui, t, icon::SLIDERS, tr!("Layout"));
                slider(ui, t, tr!("Gaps"), &mut s.gaps, 0.0..=24.0, 1.0, "px");
                slider(ui, t, tr!("Padding"), &mut s.padding, 0.0..=40.0, 1.0, "px");
                slider(ui, t, tr!("Corner radius"), &mut s.rounding, 0.0..=24.0, 1.0, "px");
                slider(ui, t, tr!("Border width"), &mut s.border_width, 0.0..=4.0, 0.5, "px");
                slider(ui, t, tr!("Sidebar width"), &mut s.sidebar_width, 200.0..=420.0, 5.0, "px");

                widgets::section(ui, t, icon::PLUG, tr!("Behaviour"));
                widgets::toggle(ui, t, &mut s.copy_on_select, tr!("Copy on select"), "");
                widgets::toggle(ui, t, &mut s.right_click_paste, tr!("Right click pastes"), tr!("Copies instead when text is selected"));
                widgets::toggle(ui, t, &mut s.auto_reconnect, tr!("Reconnect automatically"), tr!("When a working connection drops, try again with growing pauses"));
                let mut ka = s.keepalive_secs as f32;
                slider(ui, t, tr!("Keepalive"), &mut ka, 0.0..=120.0, 5.0, tr!("s, 0 = off"));
                s.keepalive_secs = ka as u64;

                widgets::section(ui, t, icon::FOLDER, tr!("Import"));
                ui.label(RichText::new(tr!("Adds sessions that are not saved yet. Passwords are never imported.")).size(12.0).color(t.subtext));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if widgets::secondary(ui, t, icon::TERMINAL, tr!("From PuTTY"), vec2(150.0, 34.0)).clicked() {
                        import = Some(ImportSource::Putty);
                    }
                    ui.add_space(6.0);
                    if widgets::secondary(ui, t, icon::KEY, tr!("From ~/.ssh/config"), vec2(190.0, 34.0)).clicked() {
                        import = Some(ImportSource::SshConfig);
                    }
                });
                ui.add_space(10.0);
            });
        });
        if let Some(src) = import {
            self.import_sessions(ctx, src);
        }
        if dismissed || close {
            self.show_settings = false;
            self.save(ctx);
        }
    }

    fn launcher_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let now = ctx.input(|i| i.time);
        if self.launcher.is_none() {
            self.opened.remove("launcher");
            return;
        }
        let opened = self.opened_at("launcher", now);
        let items = {
            let l = self.launcher.as_ref().unwrap();
            launcher_items(&self.cfg.servers, &l.query)
        };
        let states = self.server_states();
        let Some(l) = self.launcher.as_mut() else { return };
        let n = items.len();
        let (up, down, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown) || i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if n > 0 {
            if up {
                l.selected = (l.selected + n - 1) % n;
            }
            if down {
                l.selected = (l.selected + 1) % n;
            }
            l.selected = l.selected.min(n - 1);
        }
        let mut chosen: Option<usize> = enter.then_some(l.selected).filter(|_| n > 0);

        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("launcher"), opened, 560.0, |ui| {
            let before = l.query.clone();
            let r = widgets::input(ui, t, &mut l.query, tr!("Connect to…  name, host or user@host:port"), false, icon::SEARCH);
            r.request_focus();
            if l.query != before {
                l.selected = 0;
            }
            ui.add_space(12.0);
            let sel_y = ui.ctx().animate_value_with_time(Id::unique("launch-sel"), l.selected as f32, 0.12);
            let row_h = 52.0;
            let list_top = ui.cursor().min.y;
            let shown = n.min(8);
            let list = Rect::from_min_size(pos2(ui.min_rect().min.x, list_top), vec2(ui.available_width(), shown as f32 * row_h));
            ui.allocate_rect(list, Sense::hover());
            let p = ui.painter();
            if shown > 0 {
                let first = l.selected.saturating_sub(7);
                let hl_index = (sel_y - first as f32).clamp(0.0, shown as f32 - 1.0);
                let hl = Rect::from_min_size(pos2(list.min.x, list.min.y + hl_index * row_h), vec2(list.width(), row_h - 4.0));
                fx::plate(p, hl, 12.0, t.surface.gamma_multiply(0.9), t.accent, 0.8);
                for (k, item) in items.iter().enumerate().skip(first).take(shown) {
                    let row = Rect::from_min_size(pos2(list.min.x, list.min.y + (k - first) as f32 * row_h), vec2(list.width(), row_h - 4.0));
                    let resp = ui.interact(row, Id::unique(("launch-row", k)), Sense::click());
                    if resp.hovered() && ui.input(|i| i.pointer.delta().length_sq() > 0.0) {
                        l.selected = k;
                    }
                    if resp.clicked() {
                        chosen = Some(k);
                    }
                    let (ic, title, sub, color) = match item {
                        LaunchItem::Server(s) => (
                            icon::SERVER.to_string(),
                            s.display_name().to_string(),
                            format!(
                                "{}{}{}",
                                address(s),
                                route_label(s, &self.cfg.servers).map(|r| format!("  ·  {r}")).unwrap_or_default(),
                                if s.group.is_empty() { String::new() } else { format!("  ·  {}", s.group) }
                            ),
                            status_color(t, states.get(&s.id)).unwrap_or(t.accent),
                        ),
                        LaunchItem::Quick { user, host, port } => {
                            (icon::BOLT.to_string(), tr!("Quick connect {target}", target = format!("{user}@{host}")), tr!("port {port}, password login, not saved", port = port), t.warn)
                        }
                        LaunchItem::NewServer => (icon::PLUS.to_string(), tr!("New server").into(), "Ctrl+Shift+N".into(), t.ok),
                        LaunchItem::Settings => (icon::GEAR.to_string(), tr!("Settings").into(), "Ctrl+,".into(), t.subtext),
                        LaunchItem::Import(ImportSource::Putty) => (icon::FOLDER.to_string(), tr!("Import from PuTTY").into(), tr!("saved sessions in the registry").into(), t.subtext),
                        LaunchItem::Import(ImportSource::SshConfig) => (icon::FOLDER.to_string(), tr!("Import from ~/.ssh/config").into(), tr!("Host blocks").into(), t.subtext),
                    };
                    let active = k == l.selected;
                    let cy = row.center().y;
                    let badge = Rect::from_center_size(pos2(row.min.x + 24.0, cy), vec2(32.0, 32.0));
                    p.rect_filled(badge, 10.0, color.gamma_multiply(if active { 0.16 } else { 0.08 }));
                    fx::glyph(p, badge.center(), &ic, fonts::mono(13.0), color);
                    let tf = fonts::medium(13.5);
                    let sf = fonts::mono(11.5);
                    let tx = badge.max.x + 14.0;
                    let title_c = if active { t.text } else { mix(t.text, t.subtext, 0.25) };
                    veil::text_auto(p, pos2(tx, cy - 8.0 + fx::text_dy(p, &tf)), Align2::LEFT_CENTER, &title, tf, title_c, t.surface);
                    veil::text_auto(p, pos2(tx, cy + 9.0 + fx::text_dy(p, &sf)), Align2::LEFT_CENTER, &sub, sf, t.subtext, t.surface);
                    if active {
                        fx::kbd(p, pos2(row.max.x - 12.0 - 44.0, cy), "Enter", t.subtext, Color32::from_white_alpha(22));
                    }
                }
            } else {
                ui.label(RichText::new(tr!("No matches. Type user@host to connect without saving.")).color(t.subtext));
            }
            ui.add_space(8.0);
            let (fr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            let fp = ui.painter();
            let line = Color32::from_white_alpha(18);
            let mut x = fr.min.x + 2.0;
            for (keys, label) in [(&["↑", "↓"][..], tr!("select")), (&["Enter"][..], tr!("connect")), (&["Esc"][..], tr!("close"))] {
                for k in keys {
                    x += fx::kbd(fp, pos2(x, fr.center().y), k, t.subtext, line) + 4.0;
                }
                let r = fx::text(fp, pos2(x + 4.0, fr.center().y), Align2::LEFT_CENTER, label, fonts::ui(12.0), t.subtext.gamma_multiply(0.75));
                x = r.max.x + 20.0;
            }
        });

        if dismissed {
            self.launcher = None;
            return;
        }
        if let Some(k) = chosen {
            self.launcher = None;
            match items.into_iter().nth(k) {
                Some(LaunchItem::Server(s)) => self.connect(ctx, &s),
                Some(LaunchItem::Quick { user, host, port }) => {
                    let spec = ConnectSpec {
                        server_id: None,
                        host: host.clone(),
                        port,
                        user: user.clone(),
                        auth: Auth::Password,
                        keepalive: Duration::from_secs(self.cfg.settings.keepalive_secs),
                        via: None,
                        reconnect: false,
                    };
                    self.open_spec(ctx, format!("{user}@{host}"), spec);
                }
                Some(LaunchItem::NewServer) => self.editor = Some(Editor::new(Server::new(), true)),
                Some(LaunchItem::Settings) => self.show_settings = true,
                Some(LaunchItem::Import(src)) => self.import_sessions(ctx, src),
                None => {}
            }
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let now = ctx.input(|i| i.time);
        const LIFE: f64 = 6.0;
        self.toasts.retain(|x| now - x.at < LIFE);
        if self.toasts.is_empty() {
            return;
        }
        let screen = ctx.content_rect();
        let mut y = screen.max.y - 20.0;
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("toasts")));
        for toast in self.toasts.iter().rev() {
            let age = (now - toast.at) as f32;
            let inn = anim::wind((age / 0.45).clamp(0.0, 1.0));
            let out = anim::smooth(((LIFE as f32 - age) / 0.4).clamp(0.0, 1.0));
            let color = if toast.error { t.err } else { t.ok };
            let sensitive = veil::enabled() && !veil::find(&toast.text).is_empty();
            let galley = if sensitive {
                painter.layout_no_wrap(toast.text.clone(), fonts::ui(13.0), t.text)
            } else {
                painter.layout(toast.text.clone(), fonts::ui(13.0), t.text, 360.0)
            };
            let size = galley.size() + vec2(48.0, 24.0);
            let x = screen.max.x - 20.0 - size.x + (1.0 - inn) * (size.x + 30.0);
            let r = Rect::from_min_size(pos2(x, y - size.y), size);
            let mut p = painter.clone();
            p.multiply_opacity(out);
            fx::shadow(&p, r, 14.0, 0.9);
            fx::plate(&p, r, 14.0, t.mantle, color, 0.6);
            let first_row = galley.rows.first().map_or(10.0, |row| row.rect().height() / 2.0);
            fx::glyph(&p, pos2(r.min.x + 20.0, r.min.y + 12.0 + first_row), if toast.error { icon::XMARK } else { icon::BOLT }, fonts::mono(12.0), color);
            if sensitive {
                veil::text_auto(&p, r.min + vec2(36.0, 12.0), Align2::LEFT_TOP, &toast.text, fonts::ui(13.0), t.text, t.surface);
            } else {
                p.galley(r.min + vec2(36.0, 12.0), galley, t.text);
            }
            y -= size.y * out.max(0.2) + 10.0;
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn veil_overlay(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let secrets = veil::take();
        if !self.cfg.settings.streamer_mode {
            return;
        }

        let is_minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if is_minimized {
            return;
        }

        let win_rect = ctx.input(|i| i.viewport().inner_rect.or(i.viewport().outer_rect));
        let Some(rect) = win_rect else { return };
        if rect.width() < 10.0 || rect.height() < 10.0 {
            return;
        }

        let builder = egui::ViewportBuilder::default()
            .with_title("haunt-veil-overlay")
            .with_app_id("haunt-veil-overlay")
            .with_transparent(true)
            .with_decorations(false)
            .with_active(false)
            .with_mouse_passthrough(true)
            .with_taskbar(false)
            .with_position(rect.min)
            .with_inner_size(rect.size());

        let modal = self.modal_open();
        let overlay_id = egui::ViewportId::from_hash_of("haunt-veil-overlay");
        ctx.show_viewport_immediate(
            overlay_id,
            builder,
            |ui, _class| {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(rect.min));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(rect.size()));
                #[cfg(windows)]
                platform::setup_overlay_window("haunt-veil-overlay", Some(frame), self.cfg.settings.opacity);

                let painter = ui.painter();
                for s in &secrets {
                    if modal && !s.top {
                        continue;
                    }
                    veil::paint_real(painter, s);
                }
            },
        );
    }
}

impl eframe::App for HauntApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let (Some(gl), Some(a)) = (gl, self.aurora.take()) {
            a.destroy(gl);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let started = *self.started.get_or_insert(now);
        if self.started == Some(now) {
            log::debug!("startup: first frame begins");
        }
        self.apply_style(&ctx);
        veil::begin_frame(
            self.cfg.settings.streamer_mode,
            self.cfg
                .servers
                .iter()
                .flat_map(|s| [s.host.clone(), s.name.clone()])
                .chain(self.sessions.iter().map(|s| s.spec.host.clone())),
        );

        if self.blur_wanted != self.cfg.settings.blur {
            self.blur_wanted = self.cfg.settings.blur;
            self.blur_active = platform::set_blur(frame, self.blur_wanted);
            if self.blur_wanted && !self.blur_active {
                self.notify(&ctx, tr!("Acrylic needs Windows 11 22H2 or newer."), true);
            }
        }

        if self.blur_active {
            platform::hide_caption_buttons(frame);
        }
        #[cfg(windows)]
        platform::set_window_opacity(frame, self.cfg.settings.opacity);
        let (focused, maximized, dt) = ctx.input(|i| (i.viewport().focused.unwrap_or(true), i.viewport().maximized.unwrap_or(false), i.stable_dt));
        let animate = self.cfg.settings.animations;
        if focused && animate {
            self.clock += dt.min(0.1);
            if self.snowing() {
                ctx.request_repaint_after(Duration::from_millis(33));
            } else if self.cfg.settings.animated_background && self.aurora.is_some() {
                ctx.request_repaint_after(Duration::from_millis(50));
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }

        if !self.modal_open() {
            self.shortcuts(&ctx);
        }
        let t = self.theme();
        let win = ctx.content_rect();
        self.background(ui, win, maximized);

        let intro = if animate { anim::since(&ctx, started, 0.8) } else { 1.0 };
        let fade = anim::smooth((intro * 1.6).min(1.0));
        ui.multiply_opacity(fade);

        let bar = Rect::from_min_max(win.min, pos2(win.max.x, win.min.y + TITLEBAR_H));
        self.titlebar(ui, bar, maximized);

        // Body: floating cards with gaps.
        let gaps = self.cfg.settings.gaps.clamp(0.0, 32.0);
        let edge = gaps.max(6.0);
        let body = Rect::from_min_max(pos2(win.min.x + edge, bar.max.y), pos2(win.max.x - edge, win.max.y - edge));
        let lift = (1.0 - anim::wind(intro)) * 18.0;

        let open = ctx.animate_bool_with_time_and_easing(Id::unique("sidebar-open"), self.sidebar_open, if animate { 0.4 } else { 0.0 }, anim::wind);
        let sw = self.cfg.settings.sidebar_width.clamp(200.0, 420.0).min(body.width() * 0.45);
        let side_w = sw * open.max(0.0);
        let mut main = body;
        if side_w > 8.0 {
            let side = Rect::from_min_size(pos2(body.min.x - (sw - side_w), body.min.y + lift), vec2(sw, body.height()));
            self.sidebar(ui, side, open.clamp(0.0, 1.0));
            main.min.x = body.min.x + side_w + gaps;
        }
        let main = main.translate(vec2(0.0, lift * 1.4));

        // Detect tab switches for the slide animation.
        let cur_id = self.sessions.get(self.active).map(|s| s.id);
        if cur_id != self.last_active_id {
            let old_idx = self.last_active_id.and_then(|id| self.sessions.iter().position(|s| s.id == id));
            let dir = match old_idx {
                Some(o) if o < self.active => 1.0,
                Some(_) => -1.0,
                None => 0.0,
            };
            let fresh = cur_id.is_some_and(|id| self.tab_born.get(&id).is_some_and(|b| now - *b < 0.1));
            self.switch = (now, dir, fresh);
            self.last_active_id = cur_id;
        }

        let input_enabled = !self.modal_open();
        let mut reconnect = false;
        if !self.sessions.is_empty() {
            let (at, dir, fresh) = self.switch;
            let k = if animate { anim::since(&ctx, at, 0.42) } else { 1.0 };
            let e = anim::wind(k);
            let off = if fresh { vec2(0.0, (1.0 - e) * 26.0) } else { vec2((1.0 - e) * 48.0 * dir, 0.0) };
            let alpha = anim::smooth((k * 1.8).min(1.0));
            let look = termview::Look {
                theme: t,
                palette: &self.palette,
                settings: &self.cfg.settings,
                time: self.clock,
                window_focused: focused,
                pane_alpha: self.layer_alpha().1,
            };
            let s = &mut self.sessions[self.active];
            ui.scope_builder(UiBuilder::new().max_rect(main.translate(off)), |ui| {
                ui.multiply_opacity(alpha);
                if let termview::Action::Reconnect = termview::show(ui, s, &look, input_enabled) {
                    reconnect = true;
                }
            });
        } else {
            self.welcome(ui, main);
        }
        if reconnect {
            let rt = self.rt.clone();
            if let Some(s) = self.sessions.get_mut(self.active) {
                s.reconnect(&rt, &ctx);
            }
        }

        if !maximized {
            self.resize_handles(ui, win);
        }

        veil::set_top(true);
        self.prompt_dialog(&ctx);
        self.editor_dialog(&ctx);
        self.delete_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.launcher_dialog(&ctx);
        self.draw_toasts(&ctx);
        self.veil_overlay(&ctx, frame);
        if self.started == Some(now) {
            log::debug!("startup: first frame built");
        }

        // When a dialog closes (Esc, click outside) or focus ends up nowhere, hand the
        // keyboard back to the terminal so keys don't silently go nowhere.
        if !self.modal_open() && ctx.memory(|m| m.focused().is_none())
            && let Some(s) = self.sessions.get_mut(self.active) {
                s.want_focus = true;
                ctx.request_repaint();
            }
    }
}

// ---------------------------------------------------------------- helpers

/// Multi-line label whose IPs / hosts are hidden from screen capture in streamer mode.
fn veiled_label(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: Color32, fill: Color32) {
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
    for line in text.lines() {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
        if !line.is_empty() {
            veil::text_auto(ui.painter(), r.left_center(), Align2::LEFT_CENTER, line, font.clone(), color, fill);
        }
    }
}

fn dialog_header(ui: &mut egui::Ui, t: &Theme, ic: &str, title: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let p = ui.painter();
    let badge = Rect::from_min_size(r.min, vec2(40.0, 40.0));
    fx::plate(p, badge, 12.0, t.accent.gamma_multiply(0.12), t.accent, 0.6);
    fx::glyph(p, badge.center(), ic, fonts::mono(16.0), t.accent);
    let font = fonts::display(20.0);
    let dy = fx::text_dy(p, &font);
    veil::text_auto(p, pos2(badge.max.x + 14.0, badge.center().y + dy), Align2::LEFT_CENTER, title, font, t.text, t.mantle);
    ui.add_space(16.0);
}

/// A dim one-line note with a shield/info glyph, aligned to the field grid.
fn hint_line(ui: &mut egui::Ui, t: &Theme, text: &str) {
    let font = fonts::ui(12.0);
    let w = ui.available_width() - 22.0;
    let galley = ui.painter().layout(text.to_string(), font, t.subtext, w);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y.max(20.0) + 4.0), Sense::hover());
    let first = r.min.y + 2.0 + galley.rows.first().map_or(10.0, |row| row.rect().height() / 2.0);
    fx::glyph(ui.painter(), pos2(r.min.x + 7.0, first), icon::SHIELD, fonts::mono(12.5), t.ok.gamma_multiply(0.85));
    ui.painter().galley(pos2(r.min.x + 22.0, r.min.y + 2.0), galley, t.subtext);
}

/// Two columns with a 12 px gutter; `split` is the left column's share of the width.
fn two_cols(ui: &mut egui::Ui, split: f32, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    let gutter = 12.0;
    let w = ui.available_width();
    let lw = ((w - gutter) * split).round();
    two_cols_px(ui, lw, w - gutter - lw, left, right);
}

/// Two columns where the right one is exactly `right_w` wide.
fn two_cols_fixed(ui: &mut egui::Ui, right_w: f32, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    let gutter = 12.0;
    let w = ui.available_width();
    two_cols_px(ui, w - gutter - right_w, right_w, left, right);
}

fn two_cols_px(ui: &mut egui::Ui, lw: f32, rw: f32, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    let top = ui.cursor().min;
    let l = ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(top, vec2(lw, 400.0))).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        left(ui);
    });
    let r = ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(pos2(top.x + lw + 12.0, top.y), vec2(rw, 400.0))).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        right(ui);
    });
    let h = l.response.rect.height().max(r.response.rect.height());
    ui.advance_cursor_after_rect(Rect::from_min_size(top, vec2(lw + 12.0 + rw, h)));
}

fn slider(ui: &mut egui::Ui, t: &Theme, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, unit: &str) {
    // One label column for every slider, as wide as the longest label in this language,
    // so the tracks start in line and no translation runs under a track.
    let labels = [
        tr!("Opacity"),
        tr!("Font size"),
        tr!("Line height"),
        tr!("Scrollback"),
        tr!("Gaps"),
        tr!("Padding"),
        tr!("Corner radius"),
        tr!("Border width"),
        tr!("Sidebar width"),
        tr!("Keepalive"),
    ];
    let measure = |s: &str, font: egui::FontId| ui.painter().layout_no_wrap(s.to_owned(), font, t.text).size().x;
    let label_w = labels.iter().map(|l| measure(l, fonts::ui(13.5))).fold(130.0_f32, |a, b| a.max(b + 16.0));
    let unit_w = if unit.is_empty() { 0.0 } else { measure(unit, egui::FontId::proportional(11.0)) };
    let decimals = if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else { 2 };
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let (r, _) = ui.allocate_exact_size(vec2(label_w, 28.0), Sense::hover());
        ui.painter().text(pos2(r.min.x, r.center().y), Align2::LEFT_CENTER, label, fonts::ui(13.5), t.text);
        // Room for the value box (~64 px) and the unit, so the row never widens the dialog.
        let tail = gap + 64.0 + if unit.is_empty() { 0.0 } else { gap + unit_w };
        ui.spacing_mut().slider_width = (ui.available_width() - tail).max(80.0);
        ui.add(egui::Slider::new(v, range).step_by(step).fixed_decimals(decimals).show_value(true).trailing_fill(true));
        if !unit.is_empty() {
            ui.label(RichText::new(unit).size(11.0).color(t.subtext));
        }
    });
}

fn matches_query(s: &Server, q: &str) -> bool {
    let hay = format!("{} {}@{} {}", s.name, s.user, s.host, s.group).to_lowercase();
    if hay.contains(q) {
        return true;
    }
    let mut it = hay.chars();
    q.chars().filter(|c| !c.is_whitespace()).all(|c| it.any(|h| h == c))
}

/// Parses `[user@]host[:port]` for quick connect.
fn parse_quick(q: &str) -> Option<(String, String, u16)> {
    let q = q.trim();
    if q.is_empty() || q.contains(char::is_whitespace) {
        return None;
    }
    let (user, rest) = match q.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() => (u.to_string(), r),
        Some(_) => return None,
        None => ("root".to_string(), q),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !rest.starts_with('[') => (h, p.parse().ok().filter(|p: &u16| *p > 0)?),
        _ => (rest, 22),
    };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    let plausible = host.contains('.') || host.contains(':') || host.eq_ignore_ascii_case("localhost") || q.contains('@');
    (!host.is_empty() && plausible).then(|| (user, host.to_string(), port))
}

fn launcher_items(servers: &[Server], query: &str) -> Vec<LaunchItem> {
    let q = query.trim().to_lowercase();
    let mut items: Vec<LaunchItem> = servers
        .iter()
        .filter(|s| q.is_empty() || matches_query(s, &q))
        .map(|s| LaunchItem::Server(s.clone()))
        .collect();
    if let Some((user, host, port)) = parse_quick(query) {
        let exact = servers.iter().any(|s| s.host.eq_ignore_ascii_case(&host) && s.user == user && s.port == port);
        if !exact {
            items.insert(0, LaunchItem::Quick { user, host, port });
        }
    }
    if q.is_empty() || "new server".contains(&q) || tr!("New server").to_lowercase().contains(&q) {
        items.push(LaunchItem::NewServer);
    }
    if q.is_empty() || "settings".contains(&q) || tr!("Settings").to_lowercase().contains(&q) {
        items.push(LaunchItem::Settings);
    }
    if !q.is_empty() && ("import putty".contains(&q) || q.starts_with("imp") || tr!("Import from PuTTY").to_lowercase().contains(&q)) {
        items.push(LaunchItem::Import(ImportSource::Putty));
    }
    if !q.is_empty() && ("import ssh config".contains(&q) || q.starts_with("imp") || tr!("Import from ~/.ssh/config").to_lowercase().contains(&q)) {
        items.push(LaunchItem::Import(ImportSource::SshConfig));
    }
    items
}

fn status_color(t: &Theme, st: Option<&Status>) -> Option<Color32> {
    match st? {
        Status::Connected => Some(t.ok),
        Status::Connecting | Status::Reconnecting(_) => Some(t.warn),
        Status::Closed(_) => Some(t.err),
    }
}

/// Status dot: filled with a soft halo when there is a live session, a quiet ring otherwise.
fn status_dot(p: &egui::Painter, t: &Theme, c: egui::Pos2, st: Option<&Status>) {
    match status_color(t, st) {
        Some(col) => {
            p.circle_filled(c, 7.0, col.gamma_multiply(0.18));
            p.circle_filled(c, 3.5, col);
        }
        None => {
            p.circle_stroke(c, 3.5, Stroke::new(1.2, Color32::from_white_alpha(46)));
        }
    }
}

/// `user@host` (`:port` when not 22).
fn address(s: &Server) -> String {
    if s.port == 22 { format!("{}@{}", s.user, s.host) } else { format!("{}@{}:{}", s.user, s.host, s.port) }
}

/// Short description of a server's proxy for chips and tooltips.
fn route_label(s: &Server, all: &[Server]) -> Option<String> {
    match &s.proxy {
        Proxy::None => None,
        Proxy::Socks5 { .. } => Some("socks5".into()),
        Proxy::Http { .. } => Some(tr!("http proxy").into()),
        Proxy::Jump { server } => Some(match all.iter().find(|x| x.id == *server) {
            Some(j) => tr!("via {name}", name = j.display_name()),
            None => tr!("jump host missing").into(),
        }),
    }
}

/// "now", "5m", "3h", "2d", "6w".
fn ago(secs: u64) -> String {
    match secs {
        0..60 => tr!("now").into(),
        60..3600 => tr!("{n}m", n = secs / 60),
        3600..86400 => tr!("{n}h", n = secs / 3600),
        86400..1_209_600 => tr!("{n}d", n = secs / 86400),
        _ => tr!("{n}w", n = secs / 604_800),
    }
}

fn kbd_w(p: &egui::Painter, key: &str) -> f32 {
    (p.layout_no_wrap(key.to_string(), fonts::mono(10.5), Color32::PLACEHOLDER).size().x + 12.0).max(20.0).round()
}

/// A small mono chip right-aligned at `right_center`; returns its width. Hosts inside are veiled.
fn chip(p: &egui::Painter, t: &Theme, right_center: egui::Pos2, text: &str) -> f32 {
    let font = fonts::mono(11.0);
    let w = (p.layout_no_wrap(text.to_string(), font.clone(), Color32::PLACEHOLDER).size().x + 16.0).round();
    let r = Rect::from_min_size(pos2(right_center.x - w, right_center.y - 11.0), vec2(w, 22.0));
    p.rect(r, CornerRadius::same(11), t.accent.gamma_multiply(0.10), Stroke::new(1.0, t.accent.gamma_multiply(0.28)), StrokeKind::Inside);
    veil::text_auto(p, pos2(r.center().x, r.center().y + fx::text_dy(p, &font)), Align2::CENTER_CENTER, text, font, t.accent, t.mantle);
    w
}

enum RowAction {
    Connect,
    Edit,
    Delete,
}

fn server_row(ui: &mut egui::Ui, t: &Theme, s: &Server, st: Option<&Status>, route: Option<&str>, appear: f32) -> Option<RowAction> {
    let ctx = ui.ctx().clone();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
    // contains_pointer, not hovered: the edit/delete buttons sit on top of the row and would
    // steal `hovered`, hiding themselves and flickering.
    let h = anim::toggle(&ctx, resp.id.with("h"), resp.hovered() || resp.contains_pointer(), 0.16);
    let e = anim::wind(appear);
    let rect = rect.translate(vec2((1.0 - e) * -24.0, 0.0));
    let mut p = ui.painter().clone();
    p.multiply_opacity(anim::smooth(appear));

    if h > 0.0 {
        fx::plate(&p, rect, 12.0, Color32::from_white_alpha((8.0 * h) as u8), t.accent, 0.8 * h);
    }
    let cy = rect.center().y;
    status_dot(&p, t, pos2(rect.min.x + 16.0, cy), st);

    let tx = rect.min.x + 32.0;
    let actions_w = 64.0 * h;
    let route_w = if route.is_some() { 20.0 } else { 0.0 };
    let clip = Rect::from_min_max(pos2(tx, rect.min.y), pos2(rect.max.x - 8.0 - actions_w.max(route_w), rect.max.y));
    let tp = p.with_clip_rect(clip.intersect(p.clip_rect()));
    let nf = fonts::medium(13.5);
    let mf = fonts::mono(11.5);
    veil::text_auto(&tp, pos2(tx, cy - 8.0 + fx::text_dy(&p, &nf)), Align2::LEFT_CENTER, s.display_name(), nf, t.text, t.mantle);
    veil::text(&tp, pos2(tx, cy + 9.0 + fx::text_dy(&p, &mf)), Align2::LEFT_CENTER, address(s), mf, t.subtext, t.mantle);

    // A route marker when the server goes through a proxy or jump host (fades for the actions).
    if route.is_some() {
        let c = pos2(rect.max.x - 18.0, cy);
        let ic = if matches!(s.proxy, Proxy::Jump { .. }) { icon::ROUTE } else { icon::GLOBE };
        fx::glyph(&p, c, ic, fonts::mono(11.0), t.accent.gamma_multiply(0.75 * (1.0 - h)));
    }

    let mut action = None;
    if h > 0.05 {
        let mut x = rect.max.x - 8.0;
        for (ic, act, danger) in [(icon::TRASH, RowAction::Delete, true), (icon::PENCIL, RowAction::Edit, false)] {
            x -= 28.0;
            let r = Rect::from_min_size(pos2(x, cy - 14.0), vec2(28.0, 28.0));
            let br = ui.interact(r, resp.id.with(ic), Sense::click());
            let bh = anim::toggle(&ctx, br.id, br.hovered(), 0.12);
            if bh > 0.0 {
                p.rect_filled(r, 8.0, if danger { t.err.gamma_multiply(0.2 * bh) } else { Color32::from_white_alpha((16.0 * bh) as u8) });
            }
            let c = if danger { mix(t.subtext, t.err, bh) } else { mix(t.subtext, t.text, bh) };
            fx::glyph(&p, r.center(), ic, fonts::mono(12.0), c.gamma_multiply(h));
            if br.clicked() {
                action = Some(act);
            }
        }
    }
    let resp = match route {
        Some(r) => resp.on_hover_text(r),
        None => resp,
    };
    if action.is_none() && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        action = Some(RowAction::Connect);
    }
    action
}

fn theme_card(ui: &mut egui::Ui, th: &Theme, selected: bool, size: egui::Vec2) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let ctx = ui.ctx().clone();
    let h = anim::toggle(&ctx, resp.id.with("h"), resp.hovered(), 0.15);
    let s = anim::toggle(&ctx, resp.id.with("s"), selected, 0.25);
    let rect = rect.translate(vec2(0.0, -2.0 * h));
    let p = ui.painter();
    if s > 0.0 {
        fx::glow(p, rect, 12.0, th.accent.gamma_multiply(0.35 * s), 18.0, 0.0);
    }
    p.rect_filled(rect, CornerRadius::same(12), th.bg);
    // Mini terminal preview.
    let x0 = rect.min.x + 12.0;
    let y0 = rect.min.y + 14.0;
    p.text(pos2(x0, y0), Align2::LEFT_CENTER, "❯", fonts::mono(11.0), th.ansi[2]);
    p.rect_filled(Rect::from_min_size(pos2(x0 + 14.0, y0 - 3.0), vec2(34.0, 6.0)), 3.0, th.fg.gamma_multiply(0.7));
    p.rect_filled(Rect::from_min_size(pos2(x0 + 52.0, y0 - 3.0), vec2(22.0, 6.0)), 3.0, th.ansi[4]);
    p.rect_filled(Rect::from_min_size(pos2(x0 + 14.0, y0 + 9.0), vec2(50.0, 6.0)), 3.0, th.ansi[5].gamma_multiply(0.8));
    p.rect_filled(Rect::from_min_size(pos2(x0 + 68.0, y0 + 9.0), vec2(7.0, 10.0)), 1.0, th.cursor);
    for i in 0..8 {
        p.circle_filled(pos2(x0 + 4.0 + i as f32 * 12.0, rect.max.y - 30.0), 4.0, th.ansi[1 + i]);
    }
    p.text(pos2(x0, rect.max.y - 13.0), Align2::LEFT_CENTER, th.name, fonts::bold(12.0), th.fg);
    let stroke = Stroke::new(1.0 + s, mix(mix(th.selection, th.fg, 0.2 * h), th.accent, s));
    p.rect_stroke(rect, CornerRadius::same(12), stroke, StrokeKind::Inside);
    if s > 0.01 {
        let c = pos2(rect.max.x - 14.0, rect.min.y + 14.0);
        p.circle_filled(c, 8.0 * s, th.accent);
        p.text(c, Align2::CENTER_CENTER, "✓", fonts::ui(10.0), th.bg);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
