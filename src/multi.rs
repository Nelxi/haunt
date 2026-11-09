//! One command on many servers at once.
//!
//! Each target runs through a tab of its server (an open one, or a new tab in the
//! background), so proxies, jump hosts and passwords work exactly as in the terminal. The
//! command gets its own channel next to the shell, like `ssh host 'command'`.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Key, Margin, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use tokio::sync::mpsc::{self, error::TryRecvError};
use uuid::Uuid;

use crate::anim;
use crate::config::Server;
use crate::fonts::{self, icon};
use crate::fx;
use crate::session::{ExecEvent, Session, Status};
use crate::theme::{Theme, mix};
use crate::tr;
use crate::veil;
use crate::widgets::{self, CONTROL_H};

/// Output kept per server; the rest is dropped (and the card says so).
const KEEP: usize = 4 << 20;
/// Output shown per server: the tail. Copy takes all of it.
const SHOW: usize = 64 << 10;
const HISTORY: usize = 12;

#[derive(Clone, Debug, PartialEq)]
enum State {
    /// Its tab is connecting (or waiting for a password).
    Waiting,
    Running,
    Done { code: Option<u32>, signal: Option<String> },
    Failed(String),
    Stopped,
}

struct Target {
    name: String,
    tab: u64,
    state: State,
    raw: Vec<u8>,
    cut: bool,
    text: String,
    dirty: bool,
    rx: Option<mpsc::UnboundedReceiver<ExecEvent>>,
    started: Option<Instant>,
    took: Option<Duration>,
    open: bool,
}

impl Target {
    fn live(&self) -> bool {
        matches!(self.state, State::Waiting | State::Running)
    }

    fn finish(&mut self, state: State) {
        self.state = state;
        self.rx = None;
        self.took = self.started.map(|s| s.elapsed());
    }
}

struct Run {
    command: String,
    targets: Vec<Target>,
}

/// What the dialog asks the app to do.
#[derive(Default)]
pub struct Out {
    pub close: bool,
    /// Start the command on the picked servers ([`Multi::begin`]).
    pub run: bool,
    /// Show this tab (to answer its password prompt).
    pub open_tab: Option<u64>,
}

#[derive(Default)]
pub struct Multi {
    picked: Vec<Uuid>,
    command: String,
    history: Vec<String>,
    run: Option<Run>,
}

impl Multi {
    pub fn picked(&self) -> &[Uuid] {
        &self.picked
    }

    /// Whether some server is still connecting or running.
    pub fn busy(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.targets.iter().any(Target::live))
    }

    /// Starts the command on `(name, tab id)` targets.
    pub fn begin(&mut self, targets: Vec<(String, u64)>) {
        let command = self.command.trim().to_string();
        self.history.retain(|c| *c != command);
        self.history.insert(0, command.clone());
        self.history.truncate(HISTORY);
        let one = targets.len() == 1;
        let targets = targets
            .into_iter()
            .map(|(name, tab)| Target {
                name,
                tab,
                state: State::Waiting,
                raw: Vec::new(),
                cut: false,
                text: String::new(),
                dirty: false,
                rx: None,
                started: None,
                took: None,
                open: one,
            })
            .collect();
        self.run = Some(Run { command, targets });
    }

    pub fn stop(&mut self) {
        for t in self.run.iter_mut().flat_map(|r| r.targets.iter_mut()).filter(|t| t.live()) {
            t.finish(State::Stopped);
        }
    }

    /// Moves the run forward; called every frame, also while the dialog is closed.
    pub fn tick(&mut self, sessions: &[Session]) {
        let Some(run) = self.run.as_mut() else { return };
        for tg in &mut run.targets {
            if tg.state == State::Waiting {
                match sessions.iter().find(|s| s.id == tg.tab).map(Session::status) {
                    None => tg.finish(State::Failed(tr!("the tab was closed").into())),
                    Some(Status::Connected) => {
                        if let Some(rx) = sessions.iter().find(|s| s.id == tg.tab).and_then(|s| s.exec(run.command.clone())) {
                            tg.rx = Some(rx);
                            tg.state = State::Running;
                            tg.started = Some(Instant::now());
                        }
                    }
                    Some(Status::Closed(why)) => tg.finish(State::Failed(why)),
                    Some(_) => {}
                }
            }
            if let Some(rx) = tg.rx.as_mut() {
                loop {
                    match rx.try_recv() {
                        Ok(ExecEvent::Out(b)) => {
                            let room = KEEP.saturating_sub(tg.raw.len());
                            tg.cut |= b.len() > room;
                            tg.raw.extend_from_slice(&b[..b.len().min(room)]);
                            tg.dirty = true;
                        }
                        Ok(ExecEvent::Exit { code, signal }) => {
                            tg.finish(State::Done { code, signal });
                            break;
                        }
                        Ok(ExecEvent::Failed(e)) => {
                            tg.finish(State::Failed(e));
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            tg.finish(State::Failed(tr!("connection lost").into()));
                            break;
                        }
                    }
                }
            }
            if tg.dirty {
                tg.text = clean(&tg.raw);
                tg.dirty = false;
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, t: &Theme, opened: f64, servers: &[Server], sessions: &[Session]) -> Out {
        let mut out = Out::default();
        let width = (ctx.content_rect().width() - 140.0).clamp(480.0, 900.0);
        let command_id = Id::unique("multi-command");
        // Ctrl+Enter runs; taken before the text box would turn it into a new line.
        let run_key = ctx.memory(|m| m.has_focus(command_id)) && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Enter));
        let busy = self.busy();
        let (_, dismissed) = widgets::dialog(ctx, t, Id::unique("multi"), opened, width, |ui| {
            ui.horizontal(|ui| {
                crate::app::dialog_header(ui, t, icon::BOLT, tr!("Run on servers"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if widgets::icon_button(ui, t, icon::XMARK, vec2(30.0, 28.0), false).clicked() {
                        out.close = true;
                    }
                });
            });

            self.servers_ui(ui, t, servers, sessions);

            widgets::section(ui, t, icon::TERMINAL, tr!("Command"));
            ui.add_space(8.0);
            command_box(ui, t, command_id, &mut self.command);
            ui.add_space(2.0);
            para(ui, tr!("Runs without a terminal and without input, like ssh host 'command' < /dev/null: a password prompt fails at once. Stop ends the command on the servers too."), fonts::ui(12.0), t.subtext);
            if !self.history.is_empty() {
                ui.add_space(4.0);
                let mut pick = None;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for (i, c) in self.history.iter().enumerate() {
                        let short: String = if c.chars().count() > 48 { c.chars().take(47).chain(['…']).collect() } else { c.clone() };
                        let short = short.replace('\n', " ⏎ ");
                        if chip(ui, t, Id::unique(("multi-recent", i)), &short, false, fonts::mono(11.5)).on_hover_text(c).clicked() {
                            pick = Some(c.clone());
                        }
                    }
                });
                if let Some(c) = pick {
                    self.command = c;
                    ctx.memory_mut(|m| m.request_focus(command_id));
                }
            }

            ui.add_space(12.0);
            let n = self.picked.iter().filter(|id| servers.iter().any(|s| s.id == **id)).count();
            ui.horizontal(|ui| {
                let can = n > 0 && !self.command.trim().is_empty() && !busy;
                let label = tr!("Run on {n} {n:server|servers}", n = n);
                let resp = ui.add_enabled_ui(can, |ui| widgets::primary(ui, t, icon::BOLT, &label, vec2(200.0, CONTROL_H))).inner;
                if (resp.clicked() || run_key) && can {
                    out.run = true;
                }
                if busy {
                    ui.add_space(4.0);
                    if widgets::danger(ui, t, tr!("Stop"), vec2(110.0, CONTROL_H)).clicked() {
                        self.stop();
                    }
                }
                fx::text(
                    ui.painter(),
                    pos2(ui.cursor().min.x + 10.0, ui.min_rect().center().y),
                    Align2::LEFT_CENTER,
                    "Ctrl+Enter",
                    fonts::ui(12.0),
                    t.subtext.gamma_multiply(0.7),
                );
            });

            if self.run.is_some() {
                self.results_ui(ui, t, sessions, &mut out);
            }
        });
        out.close |= dismissed;
        if busy {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        out
    }

    fn servers_ui(&mut self, ui: &mut egui::Ui, t: &Theme, servers: &[Server], sessions: &[Session]) {
        widgets::section(ui, t, icon::SERVER, tr!("Servers"));
        ui.add_space(6.0);
        self.picked.retain(|id| servers.iter().any(|s| s.id == *id));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(14.0, 0.0);
            if link(ui, t, tr!("All")) {
                self.picked = servers.iter().map(|s| s.id).collect();
            }
            if link(ui, t, tr!("None")) {
                self.picked.clear();
            }
            let open: Vec<Uuid> = servers.iter().filter(|s| sessions.iter().any(|x| x.spec.server_id == Some(s.id))).map(|s| s.id).collect();
            if !open.is_empty() && link(ui, t, tr!("Open tabs")) {
                self.picked = open;
            }
        });
        ui.add_space(8.0);
        if servers.is_empty() {
            para(ui, tr!("No servers yet"), fonts::ui(13.0), t.subtext);
            return;
        }
        let mut groups: Vec<&str> = Vec::new();
        for s in servers {
            if !groups.contains(&s.group.as_str()) {
                groups.push(&s.group);
            }
        }
        groups.sort_by_key(|g| (!g.is_empty(), g.to_lowercase()));
        for g in groups {
            let members: Vec<&Server> = servers.iter().filter(|s| s.group == g).collect();
            if !g.is_empty() {
                ui.add_space(4.0);
                let all = members.iter().all(|s| self.picked.contains(&s.id));
                let (gr, resp) = ui.allocate_exact_size(vec2(ui.available_width().min(320.0), 20.0), Sense::click());
                let h = widgets::hover_t(ui, &resp);
                let p = ui.painter();
                fx::glyph(p, pos2(gr.min.x + 6.0, gr.center().y), icon::FOLDER, fonts::mono(10.5), t.accent.gamma_multiply(0.8));
                let c = mix(t.subtext, t.text, h);
                let tr_ = fx::text(p, pos2(gr.min.x + 20.0, gr.center().y), Align2::LEFT_CENTER, g.to_uppercase(), fonts::bold(10.5), c);
                if all {
                    fx::glyph(p, pos2(tr_.max.x + 10.0, gr.center().y), icon::CHECK, fonts::mono(10.0), t.accent);
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    if all {
                        self.picked.retain(|id| !members.iter().any(|s| s.id == *id));
                    } else {
                        for s in &members {
                            if !self.picked.contains(&s.id) {
                                self.picked.push(s.id);
                            }
                        }
                    }
                }
                ui.add_space(4.0);
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for s in members {
                    let on = self.picked.contains(&s.id);
                    if chip(ui, t, Id::unique(("multi-server", s.id)), s.display_name(), on, fonts::ui(12.5)).clicked() {
                        if on {
                            self.picked.retain(|id| *id != s.id);
                        } else {
                            self.picked.push(s.id);
                        }
                    }
                }
            });
            ui.add_space(4.0);
        }
    }

    fn results_ui(&mut self, ui: &mut egui::Ui, t: &Theme, sessions: &[Session], out: &mut Out) {
        let Some(run) = self.run.as_mut() else { return };
        widgets::section(ui, t, icon::BOLT, tr!("Results"));
        ui.add_space(6.0);
        let count = |f: &dyn Fn(&State) -> bool| run.targets.iter().filter(|x| f(&x.state)).count();
        let ok = count(&|s| matches!(s, State::Done { code: Some(0), .. }));
        let failed = count(&|s| matches!(s, State::Done { .. } | State::Failed(_)) && !matches!(s, State::Done { code: Some(0), .. }));
        let live = count(&|s| matches!(s, State::Waiting | State::Running));
        let mut parts = Vec::new();
        if ok > 0 {
            parts.push(tr!("{n} succeeded", n = ok));
        }
        if failed > 0 {
            parts.push(tr!("{n} failed", n = failed));
        }
        if live > 0 {
            parts.push(tr!("{n} in progress", n = live));
        }
        ui.horizontal(|ui| {
            let summary = parts.join("  ·  ");
            ui.label(RichText::new(summary).font(fonts::ui(12.5)).color(if failed > 0 { t.err } else { t.subtext }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if link(ui, t, tr!("Copy all")) {
                    let all: String = run.targets.iter().map(|x| format!("==> {} <==\n{}\n", x.name, x.text.trim_end())).collect::<Vec<_>>().join("\n");
                    ui.ctx().copy_text(all);
                }
                ui.add_space(12.0);
                let any_open = run.targets.iter().any(|x| x.open);
                if link(ui, t, if any_open { tr!("Collapse all") } else { tr!("Expand all") }) {
                    for x in &mut run.targets {
                        x.open = !any_open;
                    }
                }
            });
        });
        ui.add_space(6.0);
        for (i, tg) in run.targets.iter_mut().enumerate() {
            target_card(ui, t, i, tg, sessions, out);
            ui.add_space(6.0);
        }
    }
}

fn secs(d: Duration) -> String {
    let s = format!("{:.1}", d.as_secs_f32());
    let s = if crate::i18n::current().decimal_comma() { s.replace('.', ",") } else { s };
    tr!("{n} s", n = s)
}

fn target_card(ui: &mut egui::Ui, t: &Theme, i: usize, tg: &mut Target, sessions: &[Session], out: &mut Out) {
    let now_t = ui.ctx().input(|i| i.time);
    let tab = sessions.iter().find(|s| s.id == tg.tab);
    let asking = tab.is_some_and(|s| s.shared.lock().prompt.is_some());
    let (color, status) = match &tg.state {
        State::Waiting if asking => (t.warn, tr!("waiting for you to log in, in its tab").to_string()),
        State::Waiting => match tab.map(Session::status) {
            Some(Status::Reconnecting(_)) => (t.warn, tr!("connection lost, reconnecting…").to_string()),
            _ => (t.overlay, tr!("connecting…").to_string()),
        },
        State::Running => (t.accent, format!("{}  ·  {}", tr!("running"), tg.started.map(|s| secs(s.elapsed())).unwrap_or_default())),
        State::Done { code: Some(0), .. } => (t.ok, format!("{}  ·  {}", tr!("done"), tg.took.map(secs).unwrap_or_default())),
        State::Done { code: Some(c), .. } => (t.err, format!("{}  ·  {}", tr!("exit code {code}", code = c), tg.took.map(secs).unwrap_or_default())),
        State::Done { signal: Some(sig), .. } => (t.err, tr!("killed by signal {sig}", sig = sig)),
        State::Done { .. } => (t.subtext, tr!("done").to_string()),
        State::Failed(e) => (t.err, e.clone()),
        State::Stopped => (t.subtext, tr!("stopped").to_string()),
    };
    egui::Frame::new()
        .fill(Color32::from_black_alpha(60))
        .stroke(Stroke::new(1.0, Color32::from_white_alpha(12)))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let w = ui.available_width();
            ui.set_width(w);
            let (row, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
            let h = widgets::hover_t(ui, &resp);
            let p = ui.painter();
            // A pulsing dot while it runs.
            let pulse = if tg.live() { 0.55 + 0.45 * (now_t * 4.0).sin().abs() as f32 } else { 1.0 };
            p.circle_filled(pos2(row.min.x + 6.0, row.center().y), 4.0, color.gamma_multiply(pulse));
            let chevron = if tg.open { icon::CARET_DOWN } else { icon::CARET_UP };
            let name_r = veil::text_auto(p, pos2(row.min.x + 20.0, row.center().y + fx::text_dy(p, &fonts::medium(13.0))), Align2::LEFT_CENTER, &tg.name, fonts::medium(13.0), t.text, t.mantle);

            // Right side: open-tab button when it waits for a password, copy, chevron.
            let mut right = row.max.x;
            fx::glyph(p, pos2(right - 10.0, row.center().y), chevron, fonts::mono(11.0), mix(t.subtext, t.text, h));
            right -= 28.0;
            if !tg.text.is_empty() {
                let cr = Rect::from_center_size(pos2(right - 12.0, row.center().y), vec2(26.0, 26.0));
                let c = ui.scope_builder(egui::UiBuilder::new().max_rect(cr), |ui| widgets::icon_button(ui, t, icon::COPY, cr.size(), false)).inner;
                if c.on_hover_text(tr!("Copy the output")).clicked() {
                    ui.ctx().copy_text(tg.text.clone());
                }
                right -= 32.0;
            }
            if asking && tg.state == State::Waiting {
                let label = tr!("Open tab");
                let bw = widgets::button_width(ui, "", label) + 8.0;
                let br = Rect::from_min_size(pos2(right - bw, row.center().y - 14.0), vec2(bw, 28.0));
                let b = ui.scope_builder(egui::UiBuilder::new().max_rect(br), |ui| widgets::secondary(ui, t, "", label, br.size())).inner;
                if b.clicked() {
                    out.open_tab = Some(tg.tab);
                }
                right = br.min.x - 8.0;
            }
            let st_x = name_r.max.x + 14.0;
            if right > st_x {
                let p = ui.painter_at(Rect::from_min_max(pos2(st_x, row.min.y), pos2(right - 8.0, row.max.y)));
                fx::text(&p, pos2(st_x, row.center().y), Align2::LEFT_CENTER, &status, fonts::ui(12.5), if matches!(tg.state, State::Failed(_)) { t.err } else { t.subtext });
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                tg.open = !tg.open;
            }

            if tg.open {
                ui.add_space(6.0);
                let text = &tg.text;
                if text.is_empty() {
                    let none = if tg.live() { tr!("no output yet") } else { tr!("no output") };
                    para(ui, none, fonts::ui(12.0), t.subtext.gamma_multiply(0.8));
                } else {
                    let mut start = text.len().saturating_sub(SHOW);
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    if start > 0 || tg.cut {
                        para(ui, tr!("Showing the end of a long output; copy takes all that was kept."), fonts::ui(11.5), t.warn);
                    }
                    egui::ScrollArea::vertical()
                        .id_salt(("multi-out", i))
                        .max_height(260.0)
                        .auto_shrink([false, true])
                        .stick_to_bottom(true)
                        .show(ui, |ui| output(ui, t, &text[start..]));
                }
            }
        });
}

/// Command output; in streamer mode painted line by line so addresses get hidden.
fn output(ui: &mut egui::Ui, t: &Theme, text: &str) {
    let font = fonts::mono(12.0);
    if veil::enabled() {
        let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 1.0;
        for line in text.lines() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            if !line.is_empty() {
                veil::text_auto(ui.painter(), r.left_center(), Align2::LEFT_CENTER, line, font.clone(), t.text, t.mantle);
            }
        }
    } else {
        ui.add(egui::Label::new(RichText::new(text).font(font).color(t.text)).wrap().selectable(true));
    }
}

/// A wrapping paragraph.
fn para(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).font(font).color(color)).wrap().selectable(false));
}

/// A small text button.
fn link(ui: &mut egui::Ui, t: &Theme, text: &str) -> bool {
    let font = fonts::medium(12.5);
    let g = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), t.accent);
    let (r, resp) = ui.allocate_exact_size(g.size() + vec2(0.0, 6.0), Sense::click());
    let h = widgets::hover_t(ui, &resp);
    fx::text(ui.painter(), r.left_center(), Align2::LEFT_CENTER, text, font, mix(t.accent.gamma_multiply(0.85), t.text, h));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// A toggle pill.
fn chip(ui: &mut egui::Ui, t: &Theme, id: Id, label: &str, on: bool, font: egui::FontId) -> egui::Response {
    let g = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), t.text);
    let k = anim::toggle(ui.ctx(), id.with("on"), on, 0.15);
    let check_w = 16.0 * k;
    let size = Vec2::new(g.size().x + 24.0 + check_w, 28.0);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = widgets::hover_t(ui, &resp);
    let p = ui.painter();
    let fill = mix(Color32::from_white_alpha((8.0 + 10.0 * h) as u8), t.accent.gamma_multiply(0.2), k);
    let stroke = mix(Color32::from_white_alpha((14.0 + 10.0 * h) as u8), t.accent.gamma_multiply(0.8), k);
    p.rect(r, CornerRadius::same(14), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    if k > 0.01 {
        fx::glyph(p, pos2(r.min.x + 12.0 + 3.0, r.center().y), icon::CHECK, fonts::mono(10.0), t.accent.gamma_multiply(k));
    }
    let x = r.min.x + 12.0 + check_w;
    veil::text_auto(p, pos2(x, r.center().y + fx::text_dy(p, &font)), Align2::LEFT_CENTER, label, font, mix(t.subtext, t.text, k.max(h)), t.mantle);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The command box: monospace, a few lines tall.
fn command_box(ui: &mut egui::Ui, t: &Theme, id: Id, text: &mut String) {
    let w = ui.available_width();
    let slot = ui.painter().add(egui::Shape::Noop);
    let resp = egui::Frame::new()
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(text)
                    .id(id)
                    .font(fonts::mono(13.0))
                    .text_color(t.text)
                    .hint_text(RichText::new("uptime && df -h /").font(fonts::mono(13.0)).color(t.subtext.gamma_multiply(0.6)))
                    .frame(egui::Frame::NONE)
                    .margin(Margin::ZERO)
                    .desired_rows(3)
                    .desired_width(w - 28.0),
            )
        });
    let focused = resp.inner.has_focus();
    let f = anim::toggle(ui.ctx(), id.with("focus"), focused, 0.18);
    let r = resp.response.rect;
    ui.painter().set(
        slot,
        egui::epaint::RectShape::new(
            r,
            CornerRadius::same(10),
            Color32::from_black_alpha(90),
            Stroke::new(1.0, fx::mix_alpha(Color32::from_white_alpha(14), t.accent.gamma_multiply(0.9), f)),
            StrokeKind::Inside,
        ),
    );
    if resp.response.interact(Sense::click()).clicked() {
        resp.inner.request_focus();
    }
}

/// Plain text from terminal output: escape sequences go, a bare carriage return starts its
/// line over (progress bars), backspace takes back a character.
pub fn clean(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                Some('[') => {
                    for c in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(c) = it.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            it.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                Some('(' | ')' | '*' | '+' | '#' | '%') => {
                    it.next();
                }
                _ => {}
            },
            '\r' => match it.peek() {
                Some('\n') | None => {}
                Some(_) => out.truncate(out.rfind('\n').map_or(0, |i| i + 1)),
            },
            '\x08' => {
                if !out.ends_with('\n') {
                    out.pop();
                }
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real run through a real server (the first in the config):
    /// `HAUNT_CONFIG_DIR=<dir> cargo test live_multi -- --ignored`.
    #[test]
    #[ignore]
    fn live_multi() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let ctx = egui::Context::default();
        let (cfg, err) = crate::config::Config::load();
        assert!(err.is_none(), "{err:?}");
        let server = cfg.servers.first().expect("a server in the config").clone();
        let spec = crate::session::ConnectSpec::from_server(&server, &cfg.servers, 15);
        let sessions = vec![Session::start(7, "live".into(), spec, Default::default(), rt.handle(), &ctx)];
        let wait = |m: &mut Multi, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(25);
            while m.busy() {
                assert!(Instant::now() < deadline, "{what}: still busy");
                m.tick(&sessions);
                std::thread::sleep(Duration::from_millis(50));
            }
        };

        let mut m = Multi { command: r"printf 'out\n'; printf 'err\n' >&2; printf '\033[31mred\033[0m\n'; exit 3".into(), ..Default::default() };
        m.begin(vec![("live".into(), 7)]);
        wait(&mut m, "first run");
        let tg = &m.run.as_ref().unwrap().targets[0];
        assert_eq!(tg.state, State::Done { code: Some(3), signal: None });
        // stdout and stderr may arrive in either order.
        let mut lines: Vec<&str> = tg.text.lines().collect();
        lines.sort();
        assert_eq!(lines, ["err", "out", "red"], "{:?}", tg.text);

        // Stop: the command is cut off and the connection stays usable.
        m.command = "sleep 31".into();
        m.begin(vec![("live".into(), 7)]);
        let deadline = Instant::now() + Duration::from_secs(25);
        while m.run.as_ref().unwrap().targets[0].state != State::Running {
            assert!(Instant::now() < deadline);
            m.tick(&sessions);
            std::thread::sleep(Duration::from_millis(50));
        }
        m.stop();
        assert_eq!(m.run.as_ref().unwrap().targets[0].state, State::Stopped);
        std::thread::sleep(Duration::from_millis(700));
        m.command = "pgrep -x sleep -a | grep -c 'sleep 31' || true".into();
        m.begin(vec![("live".into(), 7)]);
        wait(&mut m, "pgrep");
        assert_eq!(m.run.as_ref().unwrap().targets[0].text.trim(), "0", "stopped command still runs: {:?}", m.run.as_ref().unwrap().targets[0].state);
        m.command = "echo again".into();
        m.begin(vec![("live".into(), 7)]);
        wait(&mut m, "after stop");
        let tg = &m.run.as_ref().unwrap().targets[0];
        assert_eq!((&tg.state, tg.text.as_str()), (&State::Done { code: Some(0), signal: None }, "again\n"));
        assert_eq!(m.history.len(), 4);
        assert_eq!(m.history[0], "echo again");
        drop(sessions);
        rt.shutdown_timeout(Duration::from_secs(2));
    }

    #[test]
    fn cleans_terminal_output() {
        assert_eq!(clean(b"\x1b[1;32mok\x1b[0m\r\n"), "ok\n");
        assert_eq!(clean(b"\x1b]0;title\x07done\n"), "done\n");
        assert_eq!(clean(b"10%\r50%\r100%\nnext"), "100%\nnext");
        assert_eq!(clean(b"ab\x08c\n"), "ac\n");
        assert_eq!(clean("пр\u{0}ивет".as_bytes()), "привет");
        // A trailing CR (more output still to come) keeps the line.
        assert_eq!(clean(b"half\r"), "half");
    }
}
