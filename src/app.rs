//! Main window. The window is undecorated and transparent: an aurora shader is the
//! "wallpaper", and the sidebar and terminal float above it as glass cards with
//! gaps between them, like tiled windows in HyDE. The titlebar, tabs, dialogs and
//! launcher are all drawn here.

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

use crate::config::{Auth, Config, CursorStyle, Server};
use crate::fonts::icon;
use crate::session::{ConnectSpec, PromptKind, Session, Status};
use crate::shader::{self, Aurora, AuroraParams};
use crate::theme::{self, Theme, mix};
use crate::{anim, fonts, fx, platform, secrets, termview, widgets};

const TITLEBAR_H: f32 = 46.0;
const RESIZE_EDGE: f32 = 6.0;

struct Editor {
    server: Server,
    is_new: bool,
    secret: String,
    key_path: String,
    port: String,
    use_key: bool,
    error: Option<String>,
}

impl Editor {
    fn new(server: Server, is_new: bool) -> Self {
        let (use_key, key_path) = match &server.auth {
            Auth::Key { path } => (true, path.display().to_string()),
            Auth::Password => (false, String::new()),
        };
        let port = server.port.to_string();
        Self { server, is_new, secret: String::new(), key_path, port, use_key, error: None }
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
}

impl HauntApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        fonts::install(&cc.egui_ctx);
        let (cfg, load_error) = Config::load();
        let theme = theme::by_name(&cfg.settings.theme);

        platform::round_corners(cc);
        let blur_active = cfg.settings.blur && platform::set_blur(cc, true);
        let aurora = cc.gl.as_ref().and_then(|gl| match Aurora::new(gl) {
            Ok(a) => Some(Arc::new(a)),
            Err(e) => {
                log::warn!("aurora shader disabled: {e}");
                None
            }
        });

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
        };
        if let Some(e) = load_error {
            app.toast_at(e, 0.0, true);
        }
        app
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

    fn notify(&mut self, ctx: &egui::Context, text: impl Into<String>, error: bool) {
        let now = ctx.input(|i| i.time);
        self.toast_at(text, now, error);
    }

    fn save(&mut self, ctx: &egui::Context) {
        if let Err(e) = self.cfg.save() {
            self.notify(ctx, format!("could not save config: {e:#}"), true);
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

    fn open_spec(&mut self, ctx: &egui::Context, label: String, spec: ConnectSpec) {
        let id = self.next_id;
        self.next_id += 1;
        let session = Session::start(id, label, spec, self.term_config(), &self.rt, ctx);
        self.tab_born.insert(id, ctx.input(|i| i.time));
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    fn connect(&mut self, ctx: &egui::Context, server: &Server) {
        let spec = ConnectSpec::from_server(server, self.cfg.settings.keepalive_secs);
        self.open_spec(ctx, server.display_name().to_string(), spec);
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
        let (next, prev, close, dup, new_server, settings, launcher, sidebar, jump) = ctx.input_mut(|i| {
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
        if dup {
            if let Some(s) = self.sessions.get(self.active) {
                let (label, spec) = (s.label.clone(), s.spec.clone());
                self.open_spec(ctx, label, spec);
            }
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

    fn background(&self, ui: &egui::Ui, rect: Rect, maximized: bool) {
        let t = self.theme();
        let p = ui.painter();
        let alpha = if self.blur_active { self.cfg.settings.opacity.clamp(0.3, 1.0) * 0.82 } else { 1.0 };
        match &self.aurora {
            Some(a) if self.cfg.settings.animated_background => {
                shader::paint(
                    p,
                    rect,
                    a,
                    AuroraParams {
                        time: self.clock,
                        base: t.crust,
                        c1: t.accent,
                        c2: t.accent2,
                        c3: t.ansi[5],
                        alpha,
                        intensity: 1.0,
                    },
                );
            }
            _ => {
                p.rect_filled(rect, 0.0, t.crust.gamma_multiply(alpha));
            }
        }
        if !maximized {
            p.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, t.overlay.gamma_multiply(0.7)), StrokeKind::Inside);
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

        // Logo: the little ghost + wordmark.
        let gx = bar.min.x + 26.0;
        fx::ghost(&p, pos2(gx, cy + 1.0), 22.0, t.accent, t.crust, self.clock, false);
        p.text(pos2(gx + 18.0, cy), Align2::LEFT_CENTER, "haunt", fonts::display(16.0), t.text);

        // Window controls on the right.
        let mut x = bar.max.x - 8.0;
        let btn = vec2(42.0, 30.0);
        let mut place = |w: f32| {
            x -= w;
            Rect::from_min_size(pos2(x, cy - btn.y / 2.0), vec2(w, btn.y))
        };
        let close_r = place(btn.x);
        let max_r = place(btn.x);
        let min_r = place(btn.x);
        place(10.0);
        let settings_r = place(34.0);
        let launch_r = place(34.0);
        let controls_left = launch_r.min.x - 8.0;

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
        if bt(settings_r, icon::GEAR, false, "Settings  (Ctrl+,)").clicked() {
            self.show_settings = true;
        }
        if bt(launch_r, icon::SEARCH, false, "Launcher  (Ctrl+K)").clicked() {
            self.launcher = Some(Launcher { query: String::new(), selected: 0 });
        }

        // Tabs: capsules.
        let tabs_left = bar.min.x + 112.0;
        let area = Rect::from_min_max(pos2(tabs_left, bar.min.y), pos2(controls_left, bar.max.y));
        self.tabs(ui, area);
    }

    fn tabs(&mut self, ui: &mut egui::Ui, area: Rect) {
        let t = self.theme();
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let painter = ui.painter_at(area);
        let h = 30.0;
        let y0 = area.center().y - h / 2.0;
        let mut x = area.min.x;
        let mut activate = None;
        let mut close = None;
        let n = self.sessions.len();
        let max_w = if n == 0 { 220.0 } else { ((area.width() - 44.0) / n as f32).clamp(70.0, 220.0) };

        for (i, s) in self.sessions.iter().enumerate() {
            let born = self.tab_born.get(&s.id).copied().unwrap_or(0.0);
            let grow = anim::wind(anim::since(&ctx, born, 0.45));
            let status = s.status();
            let galley = painter.layout_no_wrap(s.label.clone(), fonts::ui(13.0), Color32::PLACEHOLDER);
            let full = (galley.size().x + 58.0).min(max_w);
            let w = full * grow.clamp(0.0, 1.1);
            if w < 4.0 {
                x += w;
                continue;
            }
            let rect = Rect::from_min_size(pos2(x, y0), vec2(w, h));
            let id = Id::unique(("tab", s.id));
            let resp = ui.interact(rect, id, Sense::click());
            let active = i == self.active;
            let hov = anim::toggle(&ctx, id.with("h"), resp.hovered(), 0.15);
            let act = anim::toggle(&ctx, id.with("a"), active, 0.25);

            let fill = mix(t.surface.gamma_multiply(0.45 + 0.35 * hov), t.accent, act);
            if act > 0.01 {
                fx::glow(&painter, rect, h / 2.0, t.accent.gamma_multiply(0.35 * act), 18.0, 0.0);
            }
            painter.rect(
                rect,
                CornerRadius::same(255),
                fill,
                Stroke::new(1.0, t.overlay.gamma_multiply(0.6 * (1.0 - act))),
                StrokeKind::Inside,
            );

            let text_color = mix(mix(t.subtext, t.text, hov), t.crust, act);
            let dot = match status {
                Status::Connected => t.ok,
                Status::Connecting => t.warn,
                Status::Closed(_) => t.err,
            };
            let dot_c = pos2(rect.min.x + 15.0, rect.center().y);
            let pulse = if matches!(status, Status::Connecting) { 0.5 + 0.5 * (now as f32 * 5.0).sin() } else { 0.0 };
            painter.circle_filled(dot_c, 3.5 + pulse * 2.0, if act > 0.5 { mix(dot, t.crust, 0.35) } else { dot });
            let text_clip = Rect::from_min_max(pos2(dot_c.x + 9.0, rect.min.y), pos2(rect.max.x - 26.0, rect.max.y));
            painter
                .with_clip_rect(text_clip.intersect(area))
                .galley(pos2(text_clip.min.x, rect.center().y - galley.size().y / 2.0), galley, text_color);

            // Close button appears on hover / active.
            let xr = Rect::from_center_size(pos2(rect.max.x - 15.0, rect.center().y), vec2(18.0, 18.0));
            let xresp = ui.interact(xr, id.with("x"), Sense::click());
            let show_x = hov.max(act * 0.6);
            if show_x > 0.02 {
                if xresp.hovered() {
                    painter.circle_filled(xr.center(), 9.0, t.crust.gamma_multiply(0.25));
                }
                painter.text(xr.center(), Align2::CENTER_CENTER, icon::XMARK, fonts::mono(10.5), text_color.gamma_multiply(show_x));
            }
            if xresp.clicked() || resp.middle_clicked() {
                close = Some(i);
            } else if resp.clicked() {
                activate = Some(i);
            }
            let title = s.shared.lock().title.clone();
            let tip = format!("{}@{}:{}{}", s.spec.user, s.spec.host, s.spec.port, title.map(|t| format!("\n{t}")).unwrap_or_default());
            resp.on_hover_text(tip);
            x += w + 6.0;
        }

        // "+" opens the launcher.
        let plus = Rect::from_min_size(pos2(x, y0), vec2(h, h));
        if plus.max.x <= area.max.x {
            let resp = ui.interact(plus, Id::unique("tab-plus"), Sense::click());
            let hov = anim::toggle(&ctx, resp.id, resp.hovered(), 0.15);
            painter.rect_filled(plus, CornerRadius::same(255), t.surface.gamma_multiply(0.25 + 0.5 * hov));
            painter.text(plus.center(), Align2::CENTER_CENTER, icon::PLUS, fonts::mono(12.0), mix(t.subtext, t.accent, hov));
            if resp.on_hover_text("New connection  (Ctrl+K)").on_hover_cursor(CursorIcon::PointingHand).clicked() {
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
        let set = &self.cfg.settings;
        let radius = set.rounding;
        let p = ui.painter().clone();
        fx::glass(&p, rect, radius, t.mantle.gamma_multiply(set.opacity.clamp(0.3, 1.0)), t.overlay.gamma_multiply(0.8));

        let inner = rect.shrink2(vec2(14.0, 14.0));
        let mut connect: Option<Server> = None;
        let mut edit: Option<Server> = None;
        let mut delete: Option<Uuid> = None;
        let mut add = false;
        let mut open_settings = false;
        let states = self.server_states();

        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            ui.multiply_opacity(appear);
            ui.horizontal(|ui| {
                ui.label(RichText::new("SERVERS").font(fonts::bold(11.5)).color(t.subtext));
                ui.label(RichText::new(self.cfg.servers.len().to_string()).font(fonts::ui(11.5)).color(t.overlay));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::icon_button(ui, t, icon::PLUS, vec2(26.0, 24.0), false).on_hover_text("New server  (Ctrl+Shift+N)").clicked() {
                        add = true;
                    }
                });
            });
            ui.add_space(6.0);
            widgets::input(ui, t, &mut self.search, "Search servers…", false, icon::SEARCH);
            ui.add_space(8.0);

            let bottom_h = 40.0;
            let list_h = (ui.available_height() - bottom_h).max(40.0);
            let query = self.search.to_lowercase();
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
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
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icon::FOLDER).font(fonts::mono(11.0)).color(t.accent));
                            ui.label(RichText::new(g.to_uppercase()).font(fonts::bold(10.5)).color(t.subtext));
                        });
                    }
                    for s in items {
                        let start = self.started.unwrap_or(now) + 0.15 + idx as f64 * 0.045;
                        let k = anim::since(&ctx, start, 0.5);
                        idx += 1;
                        match server_row(ui, t, s, states.get(&s.id), k) {
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
                        ui.label(RichText::new("No servers yet").color(t.subtext));
                        ui.add_space(6.0);
                        if widgets::secondary(ui, t, icon::PLUS, "Add one", vec2(130.0, 32.0)).clicked() {
                            add = true;
                        }
                    });
                } else if idx == 0 {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new("Nothing matches").color(t.subtext)));
                }
            });

            // Footer.
            let foot = Rect::from_min_max(pos2(inner.min.x, inner.max.y - 30.0), inner.max);
            ui.painter().line_segment(
                [pos2(rect.min.x + 1.0, foot.min.y - 8.0), pos2(rect.max.x - 1.0, foot.min.y - 8.0)],
                Stroke::new(1.0, t.overlay.gamma_multiply(0.5)),
            );
            ui.scope_builder(UiBuilder::new().max_rect(foot), |ui| {
                ui.horizontal_centered(|ui| {
                    if widgets::icon_button(ui, t, icon::GEAR, vec2(30.0, 28.0), false).on_hover_text("Settings  (Ctrl+,)").clicked() {
                        open_settings = true;
                    }
                    ui.label(RichText::new("Ctrl K  launcher").font(fonts::ui(11.5)).color(t.overlay));
                });
            });
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
        let set = &self.cfg.settings;
        let p = ui.painter().clone();
        fx::glass(&p, rect, set.rounding, t.mantle.gamma_multiply(set.opacity.clamp(0.3, 1.0) * 0.85), t.overlay.gamma_multiply(0.8));

        let stagger = |i: usize| {
            let k = anim::since(&ctx, start + 0.1 + i as f64 * 0.08, 0.7);
            (anim::wind(k), anim::smooth(k.min(1.0)))
        };
        let c = rect.center();
        let top = c.y - 190.0;

        let (m0, a0) = stagger(0);
        let ghost_c = pos2(c.x, top + 60.0 + (1.0 - m0) * 30.0);
        let mut gp = p.clone();
        gp.multiply_opacity(a0);
        fx::ghost(&gp, ghost_c, 96.0, t.text, t.crust, self.clock, true);

        let (m1, a1) = stagger(1);
        let mut tp = p.clone();
        tp.multiply_opacity(a1);
        let y = top + 140.0 + (1.0 - m1) * 20.0;
        tp.text(pos2(c.x, y), Align2::CENTER_TOP, "haunt", fonts::display(46.0), t.text);
        tp.text(pos2(c.x, y + 58.0), Align2::CENTER_TOP, "a ghost in your shell", fonts::ui(15.0), t.accent);

        let (m2, a2) = stagger(2);
        let btn_y = top + 240.0 + (1.0 - m2) * 16.0;
        let mut new_server = false;
        let mut launcher = false;
        let row = Rect::from_center_size(pos2(c.x, btn_y + 18.0), vec2(340.0, 38.0));
        ui.scope_builder(UiBuilder::new().max_rect(row), |ui| {
            ui.multiply_opacity(a2);
            ui.horizontal(|ui| {
                if widgets::primary(ui, t, icon::PLUS, "New server", vec2(160.0, 38.0)).clicked() {
                    new_server = true;
                }
                ui.add_space(8.0);
                if widgets::secondary(ui, t, icon::BOLT, "Quick connect", vec2(168.0, 38.0)).clicked() {
                    launcher = true;
                }
            });
        });

        // Server cards.
        let (m3, a3) = stagger(3);
        let servers: Vec<Server> = self.cfg.servers.iter().take(6).cloned().collect();
        let mut connect = None;
        if !servers.is_empty() {
            let cols = servers.len().min(3);
            let cw = 200.0;
            let ch = 64.0;
            let gap = 12.0;
            let total_w = cols as f32 * cw + (cols - 1) as f32 * gap;
            let y0 = btn_y + 74.0 + (1.0 - m3) * 16.0;
            let mut hp = p.clone();
            hp.multiply_opacity(a3);
            hp.text(pos2(c.x, y0 - 8.0), Align2::CENTER_BOTTOM, "JUMP BACK IN", fonts::bold(10.5), t.subtext);
            let states = self.server_states();
            for (i, s) in servers.iter().enumerate() {
                let col = i % cols;
                let rowi = i / cols;
                let r = Rect::from_min_size(
                    pos2(c.x - total_w / 2.0 + col as f32 * (cw + gap), y0 + rowi as f32 * (ch + gap)),
                    vec2(cw, ch),
                );
                if r.max.y > rect.max.y - 40.0 {
                    break;
                }
                let resp = ui.interact(r, Id::unique(("card", s.id)), Sense::click());
                let h = anim::toggle(&ctx, resp.id, resp.hovered(), 0.18);
                let r = r.translate(vec2(0.0, -3.0 * h));
                let mut cp = p.clone();
                cp.multiply_opacity(a3);
                if h > 0.0 {
                    fx::glow(&cp, r, 12.0, t.accent.gamma_multiply(0.25 * h), 22.0, 0.0);
                }
                cp.rect(
                    r,
                    CornerRadius::same(12),
                    mix(t.surface.gamma_multiply(0.7), t.surface, h),
                    Stroke::new(1.0, mix(t.overlay, t.accent, h)),
                    StrokeKind::Inside,
                );
                let hue = badge_color(t, &s.id);
                let badge = Rect::from_center_size(pos2(r.min.x + 30.0, r.center().y), vec2(34.0, 34.0));
                cp.rect_filled(badge, 9.0, hue.gamma_multiply(0.22));
                let initial = s.display_name().chars().next().unwrap_or('?').to_uppercase().to_string();
                cp.text(badge.center(), Align2::CENTER_CENTER, initial, fonts::bold(15.0), hue);
                if let Some(sc) = status_color(t, states.get(&s.id)) {
                    cp.circle(badge.right_bottom() - vec2(2.0, 2.0), 4.5, sc, Stroke::new(2.0, t.surface));
                }
                let tx = r.min.x + 56.0;
                let clip = cp.with_clip_rect(Rect::from_min_max(pos2(tx, r.min.y), pos2(r.max.x - 10.0, r.max.y)));
                clip.text(pos2(tx, r.center().y - 9.0), Align2::LEFT_CENTER, s.display_name(), fonts::bold(13.5), t.text);
                clip.text(pos2(tx, r.center().y + 10.0), Align2::LEFT_CENTER, format!("{}@{}", s.user, s.host), fonts::ui(11.5), t.subtext);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    connect = Some(s.clone());
                }
            }
        }

        // Shortcut hints at the bottom.
        let (_, a4) = stagger(5);
        let mut hp = p.clone();
        hp.multiply_opacity(a4 * 0.9);
        let hints = "Ctrl K  launcher     Ctrl Shift N  new server     Ctrl B  sidebar     Ctrl ,  settings";
        hp.text(pos2(c.x, rect.max.y - 22.0), Align2::CENTER_CENTER, hints, fonts::ui(11.5), t.overlay);

        if new_server {
            self.editor = Some(Editor::new(Server::new(), true));
        }
        if launcher {
            self.launcher = Some(Launcher { query: String::new(), selected: 0 });
        }
        if let Some(s) = connect {
            self.connect(&ctx, &s);
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
                            ui.label(RichText::new(&message).font(fonts::mono(12.0)).color(t.subtext));
                        });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if widgets::primary(ui, t, icon::SHIELD, "Trust and connect", vec2(190.0, 36.0)).clicked() {
                            answer = Some(Some(("accept".into(), false)));
                        }
                        if widgets::secondary(ui, t, "", "Cancel", vec2(100.0, 36.0)).clicked() {
                            answer = Some(None);
                        }
                    });
                }
                PromptKind::Password | PromptKind::Passphrase => {
                    ui.label(RichText::new(&message).color(t.subtext));
                    ui.add_space(10.0);
                    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    let edit = widgets::input(ui, t, &mut self.prompt_input, "••••••••", true, icon::LOCK);
                    edit.request_focus();
                    if can_remember {
                        ui.add_space(6.0);
                        widgets::toggle(ui, t, &mut self.prompt_remember, "Remember", "Stored in Windows Credential Manager");
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if widgets::primary(ui, t, icon::ARROW, "Connect", vec2(130.0, 36.0)).clicked() || enter {
                            answer = Some(Some((std::mem::take(&mut self.prompt_input), self.prompt_remember && can_remember)));
                        }
                        if widgets::secondary(ui, t, "", "Cancel", vec2(100.0, 36.0)).clicked() {
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
        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("editor"), opened, 460.0, |ui| {
            dialog_header(ui, t, icon::SERVER, if ed.is_new { "New server" } else { "Edit server" });
            widgets::field(ui, t, "Name", &mut ed.server.name, "My server", false);
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                widgets::field(&mut cols[0], t, "Host", &mut ed.server.host, "example.com", false);
                widgets::field(&mut cols[1], t, "Port", &mut ed.port, "22", false);
            });
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                widgets::field(&mut cols[0], t, "User", &mut ed.server.user, "root", false);
                widgets::field(&mut cols[1], t, "Group", &mut ed.server.group, "optional", false);
            });
            ui.add_space(12.0);
            let mut mode = ed.use_key as usize;
            if widgets::segmented(ui, t, Id::unique("auth-seg"), &mut mode, &["Password", "Private key"]).changed() {
                ed.use_key = mode == 1;
            }
            ui.add_space(8.0);
            if ed.use_key {
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 110.0;
                    ui.allocate_ui(vec2(w, 56.0), |ui| {
                        widgets::field(ui, t, "Key file", &mut ed.key_path, ".ppk, OpenSSH or PEM", false);
                    });
                    ui.vertical(|ui| {
                        ui.add_space(19.0);
                        if widgets::secondary(ui, t, icon::FOLDER, "Browse", vec2(100.0, 36.0)).clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_title("Choose a private key").pick_file() {
                                ed.key_path = p.display().to_string();
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                widgets::field(ui, t, "Passphrase", &mut ed.secret, "empty = ask if needed", true);
            } else {
                let hint = if ed.is_new { "empty = ask on connect" } else { "empty = keep current" };
                widgets::field(ui, t, "Password", &mut ed.secret, hint, true);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::SHIELD).font(fonts::mono(11.0)).color(t.ok));
                ui.label(RichText::new("Secrets live in Windows Credential Manager, never in the config file.").size(11.5).color(t.subtext));
            });
            if let Some(e) = &ed.error {
                ui.add_space(6.0);
                ui.label(RichText::new(e).color(t.err));
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if widgets::primary(ui, t, icon::BOLT, "Save and connect", vec2(176.0, 36.0)).clicked() {
                    save = true;
                    save_connect = true;
                }
                if widgets::secondary(ui, t, "", "Save", vec2(86.0, 36.0)).clicked() {
                    save = true;
                }
                if widgets::secondary(ui, t, "", "Cancel", vec2(86.0, 36.0)).clicked() {
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
                Some("Host is required.".into())
            } else if ed.server.host.contains(char::is_whitespace) {
                Some("Host must not contain spaces.".into())
            } else if port.is_none() {
                Some("Port must be a number from 1 to 65535.".into())
            } else if ed.server.user.is_empty() {
                Some("User is required.".into())
            } else if ed.use_key && key_path.is_empty() {
                Some("Choose a key file.".into())
            } else if ed.use_key && !PathBuf::from(&key_path).is_file() {
                Some("Key file not found.".into())
            } else {
                None
            };
            if ed.error.is_some() {
                return;
            }
            ed.server.port = port.unwrap_or(22);
            ed.server.auth = if ed.use_key { Auth::Key { path: PathBuf::from(key_path) } } else { Auth::Password };
            let server = ed.server.clone();
            let secret = std::mem::take(&mut ed.secret);
            self.editor = None;
            if !secret.is_empty() {
                if let Err(e) = secrets::set(server.id, &secret) {
                    self.notify(ctx, format!("could not store the secret: {e}"), true);
                }
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
            dialog_header(ui, t, icon::TRASH, &format!("Delete “{name}”?"));
            ui.label(RichText::new("The server and its saved secret are removed.").color(t.subtext));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if widgets::danger(ui, t, "Delete", vec2(110.0, 36.0)).clicked() {
                    result = Some(true);
                }
                if widgets::secondary(ui, t, "", "Cancel", vec2(100.0, 36.0)).clicked() {
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
        let max_h = (ctx.content_rect().height() - 160.0).clamp(240.0, 620.0);
        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("settings"), opened, 560.0, |ui| {
            ui.horizontal(|ui| {
                dialog_header(ui, t, icon::SLIDERS, "Settings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if widgets::icon_button(ui, t, icon::XMARK, vec2(30.0, 28.0), false).clicked() {
                        close = true;
                    }
                });
            });
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(ui.available_width() - 10.0);
                widgets::section(ui, t, icon::BRUSH, "Theme");
                let w = ui.available_width();
                let cols = 3;
                let gap = 10.0;
                let cw = (w - gap * (cols as f32 - 1.0)) / cols as f32;
                let themes = theme::all();
                for chunk in themes.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for th in chunk {
                            if theme_card(ui, th, th.name == self.cfg.settings.theme, vec2(cw, 82.0)).clicked() {
                                self.cfg.settings.theme = th.name.to_string();
                            }
                        }
                    });
                    ui.add_space(gap - 6.0);
                }

                let s = &mut self.cfg.settings;
                widgets::section(ui, t, icon::GHOST, "Window");
                slider(ui, t, "Opacity", &mut s.opacity, 0.3..=1.0, 0.01, "");
                widgets::toggle(ui, t, &mut s.blur, "Acrylic blur", "Blur the desktop behind the window");
                widgets::toggle(ui, t, &mut s.animated_background, "Aurora background", "GLSL shader wallpaper");
                widgets::toggle(ui, t, &mut s.border_animation, "Animated border", "Rotating gradient around the active pane");
                widgets::toggle(ui, t, &mut s.animations, "Animations", "Turn off for a static, minimal UI");

                widgets::section(ui, t, icon::TERMINAL, "Terminal");
                slider(ui, t, "Font size", &mut s.font_size, 9.0..=28.0, 0.5, "pt");
                slider(ui, t, "Line height", &mut s.line_height, 1.0..=1.8, 0.05, "");
                ui.add_space(4.0);
                ui.label(RichText::new("Cursor").size(12.0).color(t.subtext));
                let mut cur = match s.cursor {
                    CursorStyle::Block => 0,
                    CursorStyle::Beam => 1,
                    CursorStyle::Underline => 2,
                };
                if widgets::segmented(ui, t, Id::unique("cursor-seg"), &mut cur, &["Block", "Beam", "Underline"]).changed() {
                    s.cursor = [CursorStyle::Block, CursorStyle::Beam, CursorStyle::Underline][cur];
                }
                ui.add_space(4.0);
                widgets::toggle(ui, t, &mut s.cursor_blink, "Cursor blink", "Smooth fade, stops after 15 s idle");
                widgets::toggle(ui, t, &mut s.cursor_trail, "Cursor trail", "The cursor glides between cells");
                widgets::toggle(ui, t, &mut s.bold_is_bright, "Bold is bright", "Bold text uses the bright palette");
                let mut sb = s.scrollback as f32;
                slider(ui, t, "Scrollback", &mut sb, 1000.0..=100_000.0, 1000.0, "lines, new tabs");
                s.scrollback = sb as usize;

                widgets::section(ui, t, icon::SLIDERS, "Layout");
                slider(ui, t, "Gaps", &mut s.gaps, 0.0..=24.0, 1.0, "px");
                slider(ui, t, "Padding", &mut s.padding, 0.0..=40.0, 1.0, "px");
                slider(ui, t, "Corner radius", &mut s.rounding, 0.0..=24.0, 1.0, "px");
                slider(ui, t, "Border width", &mut s.border_width, 0.0..=4.0, 0.5, "px");
                slider(ui, t, "Sidebar width", &mut s.sidebar_width, 200.0..=420.0, 5.0, "px");

                widgets::section(ui, t, icon::PLUG, "Behaviour");
                widgets::toggle(ui, t, &mut s.copy_on_select, "Copy on select", "");
                widgets::toggle(ui, t, &mut s.right_click_paste, "Right click pastes", "Copies instead when text is selected");
                let mut ka = s.keepalive_secs as f32;
                slider(ui, t, "Keepalive", &mut ka, 0.0..=120.0, 5.0, "s, 0 = off");
                s.keepalive_secs = ka as u64;
                ui.add_space(10.0);
            });
        });
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
            let r = widgets::input(ui, t, &mut l.query, "Connect to…  name, host or user@host:port", false, icon::SEARCH);
            r.request_focus();
            if l.query != before {
                l.selected = 0;
            }
            ui.add_space(10.0);
            let sel_y = ui.ctx().animate_value_with_time(Id::unique("launch-sel"), l.selected as f32, 0.12);
            let row_h = 46.0;
            let list_top = ui.cursor().min.y;
            let shown = n.min(8);
            let list = Rect::from_min_size(pos2(ui.min_rect().min.x, list_top), vec2(ui.available_width(), shown as f32 * row_h));
            ui.allocate_rect(list, Sense::hover());
            let p = ui.painter();
            if shown > 0 {
                let first = l.selected.saturating_sub(7);
                let hl_index = (sel_y - first as f32).clamp(0.0, shown as f32 - 1.0);
                let hl = Rect::from_min_size(pos2(list.min.x, list.min.y + hl_index * row_h), vec2(list.width(), row_h - 4.0));
                p.rect_filled(hl, 10.0, t.surface);
                p.rect_filled(Rect::from_min_size(hl.min + vec2(0.0, 10.0), vec2(3.0, hl.height() - 20.0)), 2.0, t.accent);
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
                            format!("{}@{}:{}{}", s.user, s.host, s.port, if s.group.is_empty() { String::new() } else { format!("  ·  {}", s.group) }),
                            status_color(t, states.get(&s.id)).unwrap_or(badge_color(t, &s.id)),
                        ),
                        LaunchItem::Quick { user, host, port } => {
                            (icon::BOLT.to_string(), format!("Quick connect {user}@{host}"), format!("port {port}, password auth, not saved"), t.warn)
                        }
                        LaunchItem::NewServer => (icon::PLUS.to_string(), "New server".into(), "Ctrl+Shift+N".into(), t.ok),
                        LaunchItem::Settings => (icon::GEAR.to_string(), "Settings".into(), "Ctrl+,".into(), t.subtext),
                    };
                    let active = k == l.selected;
                    p.text(pos2(row.min.x + 22.0, row.center().y), Align2::CENTER_CENTER, ic, fonts::mono(14.0), color);
                    p.text(pos2(row.min.x + 44.0, row.center().y - 8.0), Align2::LEFT_CENTER, title, fonts::bold(13.5), if active { t.text } else { mix(t.text, t.subtext, 0.3) });
                    p.text(pos2(row.min.x + 44.0, row.center().y + 10.0), Align2::LEFT_CENTER, sub, fonts::ui(11.5), t.subtext);
                    if active {
                        p.text(pos2(row.max.x - 14.0, row.center().y), Align2::RIGHT_CENTER, "↵", fonts::ui(14.0), t.subtext);
                    }
                }
            } else {
                ui.label(RichText::new("No matches. Type user@host to connect without saving.").color(t.subtext));
            }
            ui.add_space(6.0);
            ui.label(RichText::new("↑↓ select    ↵ connect    Esc close").size(11.0).color(t.overlay));
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
                    };
                    self.open_spec(ctx, format!("{user}@{host}"), spec);
                }
                Some(LaunchItem::NewServer) => self.editor = Some(Editor::new(Server::new(), true)),
                Some(LaunchItem::Settings) => self.show_settings = true,
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
            let galley = painter.layout(toast.text.clone(), fonts::ui(13.0), t.text, 360.0);
            let size = galley.size() + vec2(48.0, 24.0);
            let x = screen.max.x - 20.0 - size.x + (1.0 - inn) * (size.x + 30.0);
            let r = Rect::from_min_size(pos2(x, y - size.y), size);
            let mut p = painter.clone();
            p.multiply_opacity(out);
            fx::glow(&p, r, 12.0, Color32::from_black_alpha(90), 24.0, 0.0);
            p.rect(r, CornerRadius::same(12), t.mantle, Stroke::new(1.0, color.gamma_multiply(0.7)), StrokeKind::Inside);
            p.rect_filled(Rect::from_min_size(r.min + vec2(0.0, 10.0), vec2(3.0, r.height() - 20.0)), 2.0, color);
            p.text(pos2(r.min.x + 20.0, r.center().y), Align2::CENTER_CENTER, if toast.error { icon::XMARK } else { icon::BOLT }, fonts::mono(12.0), color);
            p.galley(r.min + vec2(36.0, 12.0), galley, t.text);
            y -= size.y * out.max(0.2) + 10.0;
        }
        ctx.request_repaint_after(Duration::from_millis(16));
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
        self.apply_style(&ctx);

        if self.blur_wanted != self.cfg.settings.blur {
            self.blur_wanted = self.cfg.settings.blur;
            self.blur_active = platform::set_blur(frame, self.blur_wanted);
            if self.blur_wanted && !self.blur_active {
                self.notify(&ctx, "Acrylic blur is not available on this system.", true);
            }
        }

        let (focused, maximized, dt) = ctx.input(|i| (i.viewport().focused.unwrap_or(true), i.viewport().maximized.unwrap_or(false), i.stable_dt));
        let animate = self.cfg.settings.animations;
        if focused && animate {
            self.clock += dt.min(0.1);
            if self.cfg.settings.animated_background && self.aurora.is_some() {
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

        self.prompt_dialog(&ctx);
        self.editor_dialog(&ctx);
        self.delete_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.launcher_dialog(&ctx);
        self.draw_toasts(&ctx);

        // When a dialog closes (Esc, click outside) or focus ends up nowhere, hand the
        // keyboard back to the terminal so keys don't silently go nowhere.
        if !self.modal_open() && ctx.memory(|m| m.focused().is_none()) {
            if let Some(s) = self.sessions.get_mut(self.active) {
                s.want_focus = true;
                ctx.request_repaint();
            }
        }
    }
}

// ---------------------------------------------------------------- helpers

fn dialog_header(ui: &mut egui::Ui, t: &Theme, ic: &str, title: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
        ui.painter().rect_filled(r, 10.0, t.accent.gamma_multiply(0.16));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, ic, fonts::mono(15.0), t.accent);
        ui.add_space(4.0);
        ui.label(RichText::new(title).font(fonts::display(19.0)).color(t.text));
    });
    ui.add_space(12.0);
}

fn slider(ui: &mut egui::Ui, t: &Theme, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, unit: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(130.0, 28.0), Sense::hover());
        ui.painter().text(pos2(r.min.x, r.center().y), Align2::LEFT_CENTER, label, fonts::ui(13.5), t.text);
        ui.spacing_mut().slider_width = (ui.available_width() - 120.0).max(80.0);
        ui.add(egui::Slider::new(v, range).step_by(step).show_value(true).trailing_fill(true));
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
    if q.is_empty() || "new server".contains(&q) {
        items.push(LaunchItem::NewServer);
    }
    if q.is_empty() || "settings".contains(&q) {
        items.push(LaunchItem::Settings);
    }
    items
}

fn status_color(t: &Theme, st: Option<&Status>) -> Option<Color32> {
    match st? {
        Status::Connected => Some(t.ok),
        Status::Connecting => Some(t.warn),
        Status::Closed(_) => Some(t.err),
    }
}

fn badge_color(t: &Theme, id: &Uuid) -> Color32 {
    let b = id.as_bytes();
    let options = [t.ansi[1], t.ansi[2], t.ansi[3], t.ansi[4], t.ansi[5], t.ansi[6], t.accent];
    options[(b[0] as usize ^ b[15] as usize) % options.len()]
}

enum RowAction {
    Connect,
    Edit,
    Delete,
}

fn server_row(ui: &mut egui::Ui, t: &Theme, s: &Server, st: Option<&Status>, appear: f32) -> Option<RowAction> {
    let ctx = ui.ctx().clone();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
    let h = anim::toggle(&ctx, resp.id.with("h"), resp.hovered(), 0.16);
    let e = anim::wind(appear);
    let rect = rect.translate(vec2((1.0 - e) * -24.0, 0.0));
    let mut p = ui.painter().clone();
    p.multiply_opacity(anim::smooth(appear));

    if h > 0.0 {
        p.rect_filled(rect, CornerRadius::same(10), t.surface.gamma_multiply(0.85 * h));
        let bar_h = (rect.height() - 20.0) * h;
        p.rect_filled(Rect::from_center_size(pos2(rect.min.x + 1.5, rect.center().y), vec2(3.0, bar_h)), 2.0, t.accent);
    }
    let badge = Rect::from_center_size(pos2(rect.min.x + 26.0 + 2.0 * h, rect.center().y), vec2(32.0, 32.0));
    let hue = badge_color(t, &s.id);
    p.rect_filled(badge, 9.0, hue.gamma_multiply(0.2 + 0.1 * h));
    let initial = s.display_name().chars().next().unwrap_or('?').to_uppercase().to_string();
    p.text(badge.center(), Align2::CENTER_CENTER, initial, fonts::bold(14.0), hue);
    if let Some(c) = status_color(t, st) {
        if matches!(st, Some(Status::Connected)) {
            p.circle_filled(badge.right_bottom() - vec2(2.0, 2.0), 7.0, c.gamma_multiply(0.25));
        }
        p.circle(badge.right_bottom() - vec2(2.0, 2.0), 4.5, c, Stroke::new(2.0, mix(t.mantle, t.surface, h)));
    }

    let tx = badge.max.x + 12.0;
    let actions_w = 60.0 * h;
    let clip = Rect::from_min_max(pos2(tx, rect.min.y), pos2(rect.max.x - 8.0 - actions_w, rect.max.y));
    let tp = p.with_clip_rect(clip.intersect(p.clip_rect()));
    tp.text(pos2(tx, rect.center().y - 8.5), Align2::LEFT_CENTER, s.display_name(), fonts::bold(13.5), t.text);
    let sub = if s.port == 22 { format!("{}@{}", s.user, s.host) } else { format!("{}@{}:{}", s.user, s.host, s.port) };
    tp.text(pos2(tx, rect.center().y + 9.5), Align2::LEFT_CENTER, sub, fonts::ui(11.5), t.subtext);

    let mut action = None;
    if h > 0.05 {
        let mut x = rect.max.x - 8.0;
        for (ic, act, danger) in [(icon::TRASH, RowAction::Delete, true), (icon::PENCIL, RowAction::Edit, false)] {
            x -= 26.0;
            let r = Rect::from_min_size(pos2(x, rect.center().y - 13.0), vec2(26.0, 26.0));
            let br = ui.interact(r, resp.id.with(ic), Sense::click());
            let bh = anim::toggle(&ctx, br.id, br.hovered(), 0.12);
            if bh > 0.0 {
                p.rect_filled(r, 7.0, if danger { t.err.gamma_multiply(0.2 * bh) } else { t.overlay.gamma_multiply(0.6 * bh) });
            }
            let c = if danger { mix(t.subtext, t.err, bh) } else { mix(t.subtext, t.text, bh) };
            p.text(r.center(), Align2::CENTER_CENTER, ic, fonts::mono(12.0), c.gamma_multiply(h));
            if br.clicked() {
                action = Some(act);
            }
        }
    }
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
