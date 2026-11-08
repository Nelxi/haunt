//! Main window: sidebar with saved servers, tab strip, terminal panes and dialogs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle as AnsiCursorStyle};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers,
    Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, pos2, vec2,
};
use uuid::Uuid;

use crate::config::{Auth, Config, CursorStyle, Server};
use crate::session::{ConnectSpec, PromptKind, Session, Status};
use crate::theme::{self, Theme, mix};
use crate::{fonts, secrets, termview};

struct Editor {
    server: Server,
    is_new: bool,
    secret: String,
    key_path: String,
    use_key: bool,
    error: Option<String>,
}

impl Editor {
    fn new(server: Server, is_new: bool) -> Self {
        let (use_key, key_path) = match &server.auth {
            Auth::Key { path } => (true, path.display().to_string()),
            Auth::Password => (false, String::new()),
        };
        Self { server, is_new, secret: String::new(), key_path, use_key, error: None }
    }
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
    prompt_input: String,
    prompt_remember: bool,
    toast: Option<(String, Instant)>,
}

impl HauntApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        fonts::install(&cc.egui_ctx);
        let (cfg, load_error) = Config::load();
        let theme = theme::by_name(&cfg.settings.theme);
        let mut app = Self {
            palette: theme.palette256(),
            applied_theme: String::new(),
            cfg,
            rt,
            sessions: Vec::new(),
            active: 0,
            next_id: 1,
            search: String::new(),
            editor: None,
            show_settings: false,
            confirm_delete: None,
            prompt_input: String::new(),
            prompt_remember: true,
            toast: None,
        };
        if let Some(e) = load_error {
            app.notify(e);
        }
        app
    }

    fn theme(&self) -> &'static Theme {
        theme::by_name(&self.cfg.settings.theme)
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn save(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.notify(format!("could not save config: {e:#}"));
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

    fn connect(&mut self, ctx: &egui::Context, server: &Server) {
        let spec = ConnectSpec::from_server(server, self.cfg.settings.keepalive_secs);
        let id = self.next_id;
        self.next_id += 1;
        let session = Session::start(id, server.display_name().to_string(), spec, self.term_config(), &self.rt, ctx);
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    fn close_tab(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.sessions.remove(idx);
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

    fn modal_open(&self) -> bool {
        self.editor.is_some()
            || self.show_settings
            || self.confirm_delete.is_some()
            || self.sessions.get(self.active).is_some_and(|s| s.shared.lock().prompt.is_some())
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
            v.panel_fill = t.mantle;
            v.window_fill = t.mantle;
            v.extreme_bg_color = t.crust;
            v.faint_bg_color = t.surface;
            v.code_bg_color = t.surface;
            v.override_text_color = Some(t.text);
            v.hyperlink_color = t.accent;
            v.selection.bg_fill = t.accent.gamma_multiply(0.35);
            v.selection.stroke = Stroke::new(1.0, t.accent);
            v.window_corner_radius = CornerRadius::same(14);
            v.window_stroke = Stroke::new(1.0, t.overlay);
            v.menu_corner_radius = CornerRadius::same(10);
            let r = CornerRadius::same(8);
            let w = &mut v.widgets;
            w.noninteractive.bg_fill = t.mantle;
            w.noninteractive.weak_bg_fill = t.mantle;
            w.noninteractive.bg_stroke = Stroke::new(1.0, t.overlay);
            w.noninteractive.fg_stroke = Stroke::new(1.0, t.subtext);
            w.noninteractive.corner_radius = r;
            w.inactive.bg_fill = t.surface;
            w.inactive.weak_bg_fill = t.surface;
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, t.text);
            w.inactive.corner_radius = r;
            w.hovered.bg_fill = mix(t.surface, t.overlay, 0.6);
            w.hovered.weak_bg_fill = mix(t.surface, t.overlay, 0.6);
            w.hovered.bg_stroke = Stroke::new(1.0, t.overlay);
            w.hovered.fg_stroke = Stroke::new(1.5, t.text);
            w.hovered.corner_radius = r;
            w.active.bg_fill = mix(t.surface, t.accent, 0.35);
            w.active.weak_bg_fill = mix(t.surface, t.accent, 0.35);
            w.active.bg_stroke = Stroke::new(1.0, t.accent);
            w.active.fg_stroke = Stroke::new(1.5, t.text);
            w.active.corner_radius = r;
            w.open = w.hovered;
            style.spacing.item_spacing = vec2(8.0, 6.0);
            style.spacing.button_padding = vec2(12.0, 6.0);
            style.spacing.interact_size.y = 30.0;
            style.spacing.text_edit_width = 260.0;
        });
    }

    // ------------------------------------------------------------ shortcuts

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        let (next, prev, close, dup, new_server, settings) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::CTRL, Key::Tab),
                i.consume_key(cs, Key::Tab),
                i.consume_key(cs, Key::W),
                i.consume_key(cs, Key::T),
                i.consume_key(cs, Key::N),
                i.consume_key(Modifiers::CTRL, Key::Comma),
            )
        });
        let n = self.sessions.len();
        if n > 0 && next {
            self.activate((self.active + 1) % n);
        }
        if n > 0 && prev {
            self.activate((self.active + n - 1) % n);
        }
        if n > 0 && close {
            self.close_tab(self.active);
        }
        if dup {
            if let Some(spec) = self.sessions.get(self.active).map(|s| s.spec.clone()) {
                if let Some(server) = spec.server_id.and_then(|id| self.cfg.servers.iter().find(|s| s.id == id)).cloned() {
                    self.connect(ctx, &server);
                }
            }
        }
        if new_server && !self.modal_open() {
            self.editor = Some(Editor::new(Server::new(), true));
        }
        if settings && !self.modal_open() {
            self.show_settings = true;
        }
    }

    // ------------------------------------------------------------ sidebar

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

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let t = self.theme();
        let ctx = ui.ctx().clone();
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(self.cfg.settings.sidebar_width)
            .min_size(190.0)
            .max_size(420.0)
            .frame(Frame::new().fill(t.mantle).inner_margin(Margin::symmetric(12, 14)))
            .show(ui, |ui| {
                // Brand.
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 28.0), Sense::hover());
                    draw_ghost(ui.painter(), rect.center(), 24.0, t.accent, t.mantle);
                    ui.label(RichText::new("haunt").font(fonts::bold(21.0)).color(t.text));
                });
                ui.add_space(10.0);
                ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Search serversâ€¦")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(10, 7)),
                );
                ui.add_space(8.0);

                let states = self.server_states();
                let query = self.search.to_lowercase();
                let mut groups: Vec<(String, Vec<Server>)> = Vec::new();
                for s in &self.cfg.servers {
                    let hay = format!("{} {} {} {}", s.name, s.host, s.user, s.group).to_lowercase();
                    if !query.is_empty() && !hay.contains(&query) {
                        continue;
                    }
                    match groups.iter_mut().find(|(g, _)| *g == s.group) {
                        Some((_, list)) => list.push(s.clone()),
                        None => groups.push((s.group.clone(), vec![s.clone()])),
                    }
                }
                groups.sort_by(|a, b| a.0.cmp(&b.0));

                let bottom_h = 84.0;
                let list_h = (ui.available_height() - bottom_h).max(60.0);
                let mut action: Option<(Server, &'static str)> = None;
                egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                    if self.cfg.servers.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("No servers yet.").color(t.subtext));
                        ui.label(RichText::new("Add one below or press Ctrl+Shift+N.").color(t.subtext).size(12.0));
                    } else if groups.is_empty() {
                        ui.label(RichText::new("Nothing matches.").color(t.subtext));
                    }
                    for (group, servers) in &groups {
                        if !group.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(group.to_uppercase()).size(11.0).color(t.subtext).strong());
                        }
                        for s in servers {
                            if let Some(a) = server_row(ui, t, s, states.get(&s.id)) {
                                action = Some((s.clone(), a));
                            }
                        }
                    }
                });

                if let Some((server, what)) = action {
                    match what {
                        "connect" => self.connect(&ctx, &server),
                        "edit" => self.editor = Some(Editor::new(server, false)),
                        "duplicate" => {
                            let mut copy = server.clone();
                            copy.id = Uuid::new_v4();
                            copy.name = format!("{} (copy)", server.display_name());
                            self.editor = Some(Editor::new(copy, true));
                        }
                        "delete" => self.confirm_delete = Some(server.id),
                        _ => {}
                    }
                }

                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let w = ui.available_width();
                    if ui.add_sized([w, 32.0], egui::Button::new("âš™  Settings")).clicked() {
                        self.show_settings = true;
                    }
                    let add = egui::Button::new(RichText::new("+  New server").color(t.crust).strong()).fill(t.accent);
                    if ui.add_sized([w, 34.0], add).clicked() {
                        self.editor = Some(Editor::new(Server::new(), true));
                    }
                });
            });
    }

    // ------------------------------------------------------------ tabs

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let t = self.theme();
        let gaps = self.cfg.settings.gaps;
        let mut activate = None;
        let mut close = None;
        egui::Panel::top("tabs")
            .resizable(false)
            .frame(Frame::new().fill(t.crust).inner_margin(Margin {
                left: gaps as i8,
                right: gaps as i8,
                top: gaps as i8,
                bottom: 0,
            }))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (i, s) in self.sessions.iter().enumerate() {
                            let (status, has_prompt, title) = {
                                let sh = s.shared.lock();
                                (sh.status.clone(), sh.prompt.is_some(), sh.title.clone())
                            };
                            let r = tab(ui, t, &s.label, i == self.active, &status, has_prompt);
                            if r.clicked {
                                activate = Some(i);
                            }
                            if r.close {
                                close = Some(i);
                            }
                            if let Some(title) = title {
                                r.response.on_hover_text(title);
                            }
                        }
                    });
                });
            });
        if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = activate {
            self.activate(i);
        }
    }

    // ------------------------------------------------------------ dialogs

    fn prompt_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let Some(session) = self.sessions.get(self.active) else { return };
        let info = {
            let sh = session.shared.lock();
            sh.prompt.as_ref().map(|p| (p.kind, p.title.clone(), p.message.clone()))
        };
        let Some((kind, title, message)) = info else { return };
        let can_remember = session.spec.server_id.is_some();
        let mut answer: Option<Option<(String, bool)>> = None;

        let modal = egui::Modal::new(Id::unique(("prompt", session.id))).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new(&title).font(fonts::bold(17.0)));
            ui.add_space(6.0);
            match kind {
                PromptKind::HostKey => {
                    ui.label(RichText::new(&message).monospace().size(12.5).color(t.subtext));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let trust = egui::Button::new(RichText::new("Trust and connect").color(t.crust).strong()).fill(t.accent);
                        if ui.add(trust).clicked() {
                            answer = Some(Some(("accept".into(), false)));
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(None);
                        }
                    });
                }
                PromptKind::Password | PromptKind::Passphrase => {
                    ui.label(RichText::new(&message).color(t.subtext));
                    ui.add_space(6.0);
                    let edit = ui.add(
                        TextEdit::singleline(&mut self.prompt_input)
                            .password(true)
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(10, 7)),
                    );
                    edit.request_focus();
                    if can_remember {
                        ui.checkbox(&mut self.prompt_remember, "Remember in Windows Credential Manager");
                    }
                    ui.add_space(6.0);
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        let ok = egui::Button::new(RichText::new("Connect").color(t.crust).strong()).fill(t.accent);
                        if ui.add(ok).clicked() || enter {
                            answer = Some(Some((std::mem::take(&mut self.prompt_input), self.prompt_remember && can_remember)));
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(None);
                        }
                    });
                }
            }
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(None);
        }
        if let Some(a) = answer {
            self.prompt_input.clear();
            if let Some(p) = session.shared.lock().prompt.take() {
                let _ = p.reply.send(a);
            }
            if let Some(s) = self.sessions.get_mut(self.active) {
                s.want_focus = true;
            }
        }
    }

    fn editor_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let Some(ed) = self.editor.as_mut() else { return };
        let mut save = false;
        let mut save_connect = false;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::unique("editor")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new(if ed.is_new { "New server" } else { "Edit server" }).font(fonts::bold(18.0)));
            ui.add_space(8.0);
            egui::Grid::new("editor-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                let field = |ui: &mut egui::Ui, v: &mut String, hint: &str| {
                    ui.add(TextEdit::singleline(v).hint_text(hint).desired_width(280.0).margin(Margin::symmetric(8, 6)));
                };
                ui.label("Name");
                field(ui, &mut ed.server.name, "My server");
                ui.end_row();
                ui.label("Host");
                field(ui, &mut ed.server.host, "example.com or 1.2.3.4");
                ui.end_row();
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut ed.server.port).range(1..=65535));
                ui.end_row();
                ui.label("User");
                field(ui, &mut ed.server.user, "root");
                ui.end_row();
                ui.label("Group");
                field(ui, &mut ed.server.group, "optional");
                ui.end_row();
                ui.label("Auth");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut ed.use_key, false, "Password");
                    ui.selectable_value(&mut ed.use_key, true, "Private key");
                });
                ui.end_row();
                if ed.use_key {
                    ui.label("Key file");
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut ed.key_path)
                                .hint_text(".ppk, OpenSSH or PEM")
                                .desired_width(196.0)
                                .margin(Margin::symmetric(8, 6)),
                        );
                        if ui.button("Browseâ€¦").clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_title("Choose a private key").pick_file() {
                                ed.key_path = p.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Passphrase");
                    ui.add(
                        TextEdit::singleline(&mut ed.secret)
                            .password(true)
                            .hint_text("empty = ask if needed")
                            .desired_width(280.0)
                            .margin(Margin::symmetric(8, 6)),
                    );
                    ui.end_row();
                } else {
                    ui.label("Password");
                    ui.add(
                        TextEdit::singleline(&mut ed.secret)
                            .password(true)
                            .hint_text(if ed.is_new { "empty = ask on connect" } else { "empty = keep current" })
                            .desired_width(280.0)
                            .margin(Margin::symmetric(8, 6)),
                    );
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("Secrets are stored in Windows Credential Manager, never in the config file.")
                    .size(11.5)
                    .color(t.subtext),
            );
            if let Some(e) = &ed.error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).color(t.err));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let primary = egui::Button::new(RichText::new("Save and connect").color(t.crust).strong()).fill(t.accent);
                if ui.add(primary).clicked() {
                    save = true;
                    save_connect = true;
                }
                if ui.button("Save").clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }

        if save {
            let ed = self.editor.as_mut().unwrap();
            ed.server.host = ed.server.host.trim().to_string();
            ed.server.user = ed.server.user.trim().to_string();
            ed.server.group = ed.server.group.trim().to_string();
            let key_path = ed.key_path.trim().trim_matches('"').to_string();
            ed.error = if ed.server.host.is_empty() {
                Some("Host is required.".into())
            } else if ed.server.host.contains(char::is_whitespace) {
                Some("Host must not contain spaces.".into())
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
            ed.server.auth = if ed.use_key { Auth::Key { path: PathBuf::from(key_path) } } else { Auth::Password };
            let server = ed.server.clone();
            let secret = std::mem::take(&mut ed.secret);
            self.editor = None;
            if !secret.is_empty() {
                if let Err(e) = secrets::set(server.id, &secret) {
                    self.notify(format!("could not store the secret: {e}"));
                }
            }
            match self.cfg.servers.iter_mut().find(|s| s.id == server.id) {
                Some(existing) => *existing = server.clone(),
                None => self.cfg.servers.push(server.clone()),
            }
            self.save();
            if save_connect {
                self.connect(ctx, &server);
            }
        } else if cancel {
            self.editor = None;
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let Some(id) = self.confirm_delete else { return };
        let name = self.cfg.servers.iter().find(|s| s.id == id).map(|s| s.display_name().to_string());
        let Some(name) = name else {
            self.confirm_delete = None;
            return;
        };
        let mut result = None;
        let modal = egui::Modal::new(Id::unique("delete")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(format!("Delete â€œ{name}â€?")).font(fonts::bold(17.0)));
            ui.label(RichText::new("Its saved password is removed too.").color(t.subtext));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let del = egui::Button::new(RichText::new("Delete").color(t.crust).strong()).fill(t.err);
                if ui.add(del).clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
        if modal.should_close() {
            result.get_or_insert(false);
        }
        if let Some(yes) = result {
            self.confirm_delete = None;
            if yes {
                self.cfg.servers.retain(|s| s.id != id);
                secrets::delete(id);
                self.save();
            }
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let t = self.theme();
        let mut close = false;
        let before = format!("{:?}", self.cfg.settings);
        let modal = egui::Modal::new(Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(RichText::new("Settings").font(fonts::bold(19.0)));
            ui.add_space(8.0);
            egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                section(ui, t, "Theme");
                ui.horizontal_wrapped(|ui| {
                    for th in theme::all() {
                        if theme_card(ui, th, th.name == self.cfg.settings.theme).clicked() {
                            self.cfg.settings.theme = th.name.to_string();
                        }
                    }
                });

                let s = &mut self.cfg.settings;
                section(ui, t, "Terminal");
                egui::Grid::new("set-term").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Font size");
                    ui.add(egui::Slider::new(&mut s.font_size, 9.0..=28.0).step_by(0.5));
                    ui.end_row();
                    ui.label("Line height");
                    ui.add(egui::Slider::new(&mut s.line_height, 1.0..=1.8).step_by(0.05));
                    ui.end_row();
                    ui.label("Cursor");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.cursor, CursorStyle::Block, "Block");
                        ui.selectable_value(&mut s.cursor, CursorStyle::Beam, "Beam");
                        ui.selectable_value(&mut s.cursor, CursorStyle::Underline, "Underline");
                    });
                    ui.end_row();
                    ui.label("Cursor blink");
                    ui.checkbox(&mut s.cursor_blink, "");
                    ui.end_row();
                    ui.label("Scrollback lines");
                    ui.add(egui::DragValue::new(&mut s.scrollback).range(100..=200_000).speed(100));
                    ui.end_row();
                    ui.label("Bold as bright");
                    ui.checkbox(&mut s.bold_is_bright, "");
                    ui.end_row();
                });

                section(ui, t, "Look");
                egui::Grid::new("set-look").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Gaps");
                    ui.add(egui::Slider::new(&mut s.gaps, 0.0..=24.0).step_by(1.0));
                    ui.end_row();
                    ui.label("Padding");
                    ui.add(egui::Slider::new(&mut s.padding, 0.0..=32.0).step_by(1.0));
                    ui.end_row();
                    ui.label("Corner radius");
                    ui.add(egui::Slider::new(&mut s.rounding, 0.0..=24.0).step_by(1.0));
                    ui.end_row();
                    ui.label("Border width");
                    ui.add(egui::Slider::new(&mut s.border_width, 0.0..=4.0).step_by(0.5));
                    ui.end_row();
                });

                section(ui, t, "Behaviour");
                ui.checkbox(&mut s.copy_on_select, "Copy on select");
                ui.checkbox(&mut s.right_click_paste, "Right click copies selection / pastes");
                ui.horizontal(|ui| {
                    ui.label("Keepalive every");
                    ui.add(egui::DragValue::new(&mut s.keepalive_secs).range(5..=600).suffix(" s"));
                });

                section(ui, t, "Shortcuts");
                for (k, v) in [
                    ("Ctrl+C", "copy selection, or interrupt when nothing is selected"),
                    ("Ctrl+V / Ctrl+Shift+V", "paste"),
                    ("Ctrl+Tab / Ctrl+Shift+Tab", "next / previous tab"),
                    ("Ctrl+Shift+T", "duplicate the current tab"),
                    ("Ctrl+Shift+W", "close tab"),
                    ("Ctrl+Shift+N", "new server"),
                    ("Shift+PgUp / PgDn", "scroll history"),
                    ("Shift + mouse", "select text even inside mouse apps"),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(k).monospace().color(t.accent));
                        ui.label(RichText::new(v).color(t.subtext));
                    });
                }
            });
            ui.add_space(10.0);
            if ui.button("Done").clicked() {
                close = true;
            }
        });
        if modal.should_close() {
            close = true;
        }
        if format!("{:?}", self.cfg.settings) != before {
            self.apply_style(ctx);
        }
        if close {
            self.show_settings = false;
            let tc = self.term_config();
            for s in &mut self.sessions {
                s.term.lock().set_options(tc.clone());
                s.want_focus = true;
            }
            self.save();
        }
    }

    // ------------------------------------------------------------ main area

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let t = self.theme();
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(self.cfg.settings.rounding as u8), t.bg);
        let c = pos2(rect.center().x, rect.center().y - 90.0);
        let time = ui.input(|i| i.time) as f32;
        let bob = (time * 1.6).sin() * 4.0;
        draw_ghost(&painter, c + vec2(0.0, bob), 84.0, t.accent, t.bg);
        ui.ctx().request_repaint_after(Duration::from_millis(33));
        painter.text(c + vec2(0.0, 74.0), Align2::CENTER_CENTER, "haunt", fonts::bold(40.0), t.text);
        painter.text(c + vec2(0.0, 106.0), Align2::CENTER_CENTER, "a ghost in your shell", FontId::proportional(15.0), t.subtext);

        let mut connect = None;
        let area = Rect::from_center_size(pos2(rect.center().x, c.y + 200.0), vec2(360.0, 160.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            ui.vertical_centered(|ui| {
                if self.cfg.servers.is_empty() {
                    let add = egui::Button::new(RichText::new("+  Add your first server").color(t.crust).strong()).fill(t.accent);
                    if ui.add_sized([240.0, 36.0], add).clicked() {
                        self.editor = Some(Editor::new(Server::new(), true));
                    }
                } else {
                    ui.label(RichText::new("Pick a server on the left, or:").color(t.subtext));
                    ui.add_space(4.0);
                    for s in self.cfg.servers.iter().take(4) {
                        let label = format!("{}   {}@{}", s.display_name(), s.user, s.host);
                        if ui.add_sized([320.0, 30.0], egui::Button::new(label)).clicked() {
                            connect = Some(s.clone());
                        }
                    }
                }
            });
        });
        if let Some(s) = connect {
            self.connect(ui.ctx(), &s);
        }
    }
}

impl eframe::App for HauntApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_style(&ctx);
        if !self.modal_open() {
            self.shortcuts(&ctx);
        }
        let t = self.theme();

        self.sidebar(ui);
        if !self.sessions.is_empty() {
            self.tab_strip(ui);
        }

        let gaps = self.cfg.settings.gaps as i8;
        let input_enabled = !self.modal_open();
        let mut reconnect = false;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(t.crust).inner_margin(Margin::same(gaps)))
            .show(ui, |ui| {
                if let Some(idx) = (!self.sessions.is_empty()).then_some(self.active) {
                    let look = termview::Look { theme: t, palette: &self.palette, settings: &self.cfg.settings };
                    let s = &mut self.sessions[idx];
                    if let termview::Action::Reconnect = termview::show(ui, s, &look, input_enabled) {
                        reconnect = true;
                    }
                } else {
                    self.welcome(ui);
                }
            });
        if reconnect {
            let rt = self.rt.clone();
            if let Some(s) = self.sessions.get_mut(self.active) {
                s.reconnect(&rt, &ctx);
            }
        }

        self.prompt_dialog(&ctx);
        self.editor_dialog(&ctx);
        self.delete_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.draw_toast(&ctx);
    }
}

impl HauntApp {
    fn draw_toast(&mut self, ctx: &egui::Context) {
        let t = self.theme();
        let Some((msg, at)) = &self.toast else { return };
        if at.elapsed() > Duration::from_secs(6) {
            self.toast = None;
            return;
        }
        let msg = msg.clone();
        egui::Area::new(Id::unique("toast"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0))
            .show(ctx, |ui| {
                Frame::new()
                    .fill(t.surface)
                    .stroke(Stroke::new(1.0, t.warn))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.label(RichText::new(msg).color(t.text));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------- widgets

fn section(ui: &mut egui::Ui, t: &Theme, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.5).strong().color(t.accent));
    ui.add_space(2.0);
}

fn status_color(t: &Theme, st: Option<&Status>) -> Option<Color32> {
    match st? {
        Status::Connected => Some(t.ok),
        Status::Connecting => Some(t.warn),
        Status::Closed(_) => Some(t.err),
    }
}

fn server_row(ui: &mut egui::Ui, t: &Theme, s: &Server, st: Option<&Status>) -> Option<&'static str> {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 46.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 8.0, t.surface);
    }
    // Monogram badge.
    let badge = Rect::from_min_size(rect.min + vec2(6.0, 8.0), vec2(30.0, 30.0));
    let hue = badge_color(t, &s.id);
    p.rect_filled(badge, 8.0, hue.gamma_multiply(0.22));
    let initial = s.display_name().chars().next().unwrap_or('?').to_uppercase().to_string();
    p.text(badge.center(), Align2::CENTER_CENTER, initial, fonts::bold(14.0), hue);
    if let Some(c) = status_color(t, st) {
        p.circle(badge.right_bottom() - vec2(2.0, 2.0), 4.5, c, Stroke::new(2.0, if resp.hovered() { t.surface } else { t.mantle }));
    }
    let x = badge.max.x + 10.0;
    let max_w = rect.max.x - x - 6.0;
    let name = p.layout(s.display_name().to_string(), FontId::proportional(14.0), t.text, max_w);
    p.galley(pos2(x, rect.min.y + 6.0), name, t.text);
    let sub = format!("{}@{}{}", s.user, s.host, if s.port != 22 { format!(":{}", s.port) } else { String::new() });
    let sub = p.layout(sub, FontId::proportional(12.0), t.subtext, max_w);
    p.galley(pos2(x, rect.min.y + 25.0), sub, t.subtext);

    let mut out = None;
    if resp.clicked() {
        out = Some("connect");
    }
    resp.context_menu(|ui| {
        if ui.button("Connect").clicked() {
            out = Some("connect");
        }
        if ui.button("Edit").clicked() {
            out = Some("edit");
        }
        if ui.button("Duplicate").clicked() {
            out = Some("duplicate");
        }
        ui.separator();
        if ui.button(RichText::new("Delete").color(t.err)).clicked() {
            out = Some("delete");
        }
    });
    out
}

fn badge_color(t: &Theme, id: &Uuid) -> Color32 {
    let picks = [t.ansi[1], t.ansi[2], t.ansi[3], t.ansi[4], t.ansi[5], t.ansi[6], t.accent];
    picks[(id.as_bytes()[0] as usize) % picks.len()]
}

struct TabResult {
    response: egui::Response,
    clicked: bool,
    close: bool,
}

fn tab(ui: &mut egui::Ui, t: &Theme, label: &str, active: bool, st: &Status, attention: bool) -> TabResult {
    let font = FontId::proportional(13.5);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, t.text);
    let w = (galley.size().x + 58.0).clamp(110.0, 240.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
    let p = ui.painter();
    let fill = if active {
        t.bg
    } else if resp.hovered() {
        t.surface
    } else {
        t.mantle
    };
    p.rect(rect, CornerRadius::same(9), fill, Stroke::new(1.0, if active { t.accent } else { t.overlay.gamma_multiply(0.5) }), StrokeKind::Inside);
    let dot = match st {
        Status::Connected => t.ok,
        Status::Connecting => t.warn,
        Status::Closed(_) => t.err,
    };
    p.circle_filled(pos2(rect.min.x + 14.0, rect.center().y), 3.5, if attention { t.accent } else { dot });
    let text_rect = Rect::from_min_max(pos2(rect.min.x + 26.0, rect.min.y), pos2(rect.max.x - 28.0, rect.max.y));
    let color = if active { t.text } else { t.subtext };
    let g = p.layout(label.to_string(), FontId::proportional(13.5), color, text_rect.width());
    let ty = rect.center().y - g.size().y / 2.0;
    p.with_clip_rect(text_rect).galley(pos2(text_rect.min.x, ty), g, color);

    let close_rect = Rect::from_center_size(pos2(rect.max.x - 15.0, rect.center().y), vec2(18.0, 18.0));
    let close_resp = ui.interact(close_rect, resp.id.with("close"), Sense::click());
    if close_resp.hovered() {
        p.rect_filled(close_rect, 5.0, t.overlay);
    }
    p.text(close_rect.center(), Align2::CENTER_CENTER, "Ã—", FontId::proportional(15.0), if close_resp.hovered() { t.text } else { t.subtext });
    let middle = resp.clicked_by(egui::PointerButton::Middle);
    TabResult { clicked: resp.clicked(), close: close_resp.clicked() || middle, response: resp }
}

fn theme_card(ui: &mut egui::Ui, th: &Theme, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(152.0, 74.0), Sense::click());
    let p = ui.painter();
    let stroke = if selected {
        Stroke::new(2.0, th.accent)
    } else if resp.hovered() {
        Stroke::new(1.0, th.overlay)
    } else {
        Stroke::new(1.0, th.surface)
    };
    p.rect(rect, CornerRadius::same(10), th.bg, stroke, StrokeKind::Inside);
    p.text(rect.min + vec2(10.0, 9.0), Align2::LEFT_TOP, th.name, FontId::proportional(12.5), th.fg);
    for i in 0..8 {
        let sw = Rect::from_min_size(rect.min + vec2(10.0 + i as f32 * 16.5, 36.0), vec2(13.0, 13.0));
        p.rect_filled(sw, 3.0, th.ansi[i + 8]);
    }
    p.rect_filled(Rect::from_min_size(rect.min + vec2(10.0, 56.0), vec2(40.0, 6.0)), 3.0, th.accent);
    resp
}

/// The haunt mascot: a small ghost built from simple shapes.
pub fn draw_ghost(p: &egui::Painter, center: Pos2, size: f32, body: Color32, eyes: Color32) {
    let w = size * 0.78;
    let h = size * 0.86;
    let top = center.y - h / 2.0;
    let r = w / 2.0;
    // Head + body.
    p.circle_filled(pos2(center.x, top + r), r, body);
    let body_rect = Rect::from_min_max(pos2(center.x - r, top + r), pos2(center.x + r, center.y + h / 2.0 - size * 0.08));
    p.rect_filled(body_rect, 0.0, body);
    // Wavy hem: three bumps.
    let bump_r = w / 6.0;
    for i in 0..3 {
        let x = body_rect.min.x + bump_r + i as f32 * bump_r * 2.0;
        p.circle_filled(pos2(x, body_rect.max.y), bump_r, body);
    }
    // Eyes.
    let ey = top + r * 1.05;
    let er = size * 0.075;
    p.circle_filled(pos2(center.x - w * 0.18, ey), er, eyes);
    p.circle_filled(pos2(center.x + w * 0.18, ey), er, eyes);
}
