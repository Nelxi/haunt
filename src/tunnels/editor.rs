//! The "Tunnels" page of a tab: one card per rule, live status, buttons to add more.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Margin, Rect, RichText, Sense, Stroke, UiBuilder, pos2, vec2};
use uuid::Uuid;

use super::{Live, State, idle_state, lan_ip};
use crate::config::{Tunnel, TunnelKind};
use crate::fonts::{self, icon};
use crate::fx;
use crate::session::Session;
use crate::theme::Theme;
use crate::tr;
use crate::veil;
use crate::widgets::{self, CONTROL_H, LABEL_H};

pub struct Look<'a> {
    pub theme: &'a Theme,
    pub pane_alpha: f32,
    pub radius: f32,
    pub gaps: f32,
}

const KINDS: [TunnelKind; 3] = [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Socks];

/// A rule as typed: the numbers stay text until they parse.
struct Draft {
    rule: Tunnel,
    port: String,
    to_host: String,
    to_port: String,
}

fn port_text(p: u16) -> String {
    if p == 0 { String::new() } else { p.to_string() }
}

fn parse_port(s: &str) -> Option<u16> {
    s.trim().parse().ok().filter(|p| *p != 0)
}

impl Draft {
    fn new(rule: &Tunnel) -> Self {
        Self { rule: rule.clone(), port: port_text(rule.port), to_host: rule.to_host.clone(), to_port: port_text(rule.to_port) }
    }

    /// The rule as it stands; unfinished parts leave it invalid, so it doesn't start.
    fn rule(&self) -> Tunnel {
        Tunnel {
            port: parse_port(&self.port).unwrap_or(0),
            to_host: self.to_host.trim().to_string(),
            to_port: parse_port(&self.to_port).unwrap_or(0),
            ..self.rule.clone()
        }
    }

    /// Ports on this PC are shared by `-L` and `-D`; `-R` ports are on the server.
    fn on_pc(&self) -> bool {
        self.rule.kind != TunnelKind::Remote
    }
}

#[derive(Default)]
struct CardOut {
    commit: bool,
    remove: bool,
}

/// Runs `f` in a child ui that fills `rect`.
fn at<R>(ui: &mut egui::Ui, rect: Rect, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)), f).inner
}

/// A wrapping paragraph.
fn para(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).font(font).color(color)).wrap().selectable(false));
}

pub struct Editor {
    drafts: Vec<Draft>,
    /// A rule just added: its first field takes the focus.
    focus: Option<Uuid>,
    /// The address chip that was clicked, and when (for the "copied" flash).
    copied: Option<(Uuid, f64)>,
}

impl Editor {
    pub fn new(rules: &[Tunnel]) -> Self {
        Self { drafts: rules.iter().map(Draft::new).collect(), focus: None, copied: None }
    }

    /// Takes rules changed elsewhere (another tab of the same server).
    pub fn reload(&mut self, rules: &[Tunnel]) {
        self.drafts = rules.iter().map(Draft::new).collect();
    }

    fn rules(&self) -> Vec<Tunnel> {
        self.drafts.iter().map(Draft::rule).collect()
    }

    /// Whether another enabled rule listens on the same port in the same place.
    fn clash(&self, i: usize) -> bool {
        let d = &self.drafts[i];
        let Some(port) = parse_port(&d.port) else { return false };
        d.rule.enabled
            && self.drafts.iter().enumerate().any(|(j, o)| j != i && o.rule.enabled && o.on_pc() == d.on_pc() && parse_port(&o.port) == Some(port))
    }

    /// The first port from `from` that no rule on this PC uses.
    fn free_port(&self, from: u16) -> u16 {
        (from..from.saturating_add(100)).find(|p| !self.drafts.iter().any(|d| d.on_pc() && parse_port(&d.port) == Some(*p))).unwrap_or(from)
    }

    /// Draws the page. Returns the new rules when the user changed something.
    pub fn show(&mut self, ui: &mut egui::Ui, s: &Session, look: &Look<'_>, input_enabled: bool) -> Option<Vec<Tunnel>> {
        let t = look.theme;
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());

        // The plate, as the terminal draws it.
        let painter = ui.painter_at(rect.expand(24.0));
        fx::shadow(&painter, rect, look.radius, 0.7);
        painter.rect_filled(rect, CornerRadius::same(look.radius as u8), t.bg.gamma_multiply(look.pane_alpha));
        fx::vgradient_stroke(&painter, rect, look.radius, 1.0, Color32::from_white_alpha(16), Color32::from_white_alpha(9));

        let mut commit = false;
        let mut remove = None;
        let mut add = None;
        let mut any_active = false;
        let inner = rect.shrink2(vec2(0.0, (look.radius / 2.0).max(4.0)));
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
        egui::ScrollArea::vertical().id_salt(("tunnels", s.id)).auto_shrink([false, false]).show(&mut child, |ui| {
            let full = ui.available_width();
            let w = (full - 2.0 * (look.gaps + 24.0)).clamp(280.0, 760.0).min(full);
            ui.horizontal(|ui| {
                ui.add_space(((full - w) / 2.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.add_space(26.0);
                    para(ui, tr!("Port forwarding"), fonts::medium(20.0), t.text);
                    ui.add_space(6.0);
                    para(
                        ui,
                        tr!("Reach services behind the server, let the server reach this PC, or send apps through the server. Rules belong to the server and start on every connect."),
                        fonts::ui(13.0),
                        t.subtext,
                    );
                    if s.spec.server_id.is_none() {
                        ui.add_space(6.0);
                        para(ui, tr!("This tab is a quick connect, not a saved server: its rules last until the tab is closed."), fonts::ui(13.0), t.warn);
                    }
                    ui.add_space(18.0);

                    for i in 0..self.drafts.len() {
                        let clash = self.clash(i);
                        let live = s.tunnel_live(self.drafts[i].rule.id);
                        any_active |= live.as_ref().is_some_and(|l| l.state == State::Active);
                        let out = self.card(ui, t, i, clash, live, now, input_enabled);
                        commit |= out.commit;
                        if out.remove {
                            remove = Some(i);
                        }
                        ui.add_space(12.0);
                    }
                    ui.add_space(if self.drafts.is_empty() { 4.0 } else { 6.0 });
                    let lead = if self.drafts.is_empty() { tr!("No tunnels yet. Add one:") } else { tr!("Add a tunnel:") };
                    para(ui, lead, fonts::ui(13.5), t.subtext);
                    ui.add_space(8.0);
                    let (row, _) = ui.allocate_exact_size(vec2(w, CONTROL_H), Sense::hover());
                    let bw = (w - 2.0 * 10.0) / 3.0;
                    let items = [
                        (TunnelKind::Local, icon::ARROW, tr!("Local forward")),
                        (TunnelKind::Remote, icon::ARROW_LEFT, tr!("Remote forward")),
                        (TunnelKind::Socks, icon::GLOBE, tr!("SOCKS proxy")),
                    ];
                    for (k, (kind, ic, label)) in items.into_iter().enumerate() {
                        let r = Rect::from_min_size(pos2(row.min.x + k as f32 * (bw + 10.0), row.min.y), vec2(bw, CONTROL_H));
                        if at(ui, r, |ui| widgets::secondary(ui, t, ic, label, r.size())).clicked() {
                            add = Some(kind);
                        }
                    }
                    ui.add_space(28.0);
                });
            });
        });

        if let Some(i) = remove {
            self.drafts.remove(i);
            commit = true;
        }
        if let Some(kind) = add {
            let rule = Tunnel::new(kind);
            let mut d = Draft::new(&rule);
            match kind {
                TunnelKind::Socks => d.port = self.free_port(1080).to_string(),
                _ => d.to_host = "127.0.0.1".into(),
            }
            self.focus = Some(rule.id);
            self.drafts.push(d);
            commit = true;
        }
        if any_active {
            // The counters move without telling the UI.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let rules = self.rules();
        (commit && rules != s.spec.tunnels).then_some(rules)
    }

    #[allow(clippy::too_many_arguments)]
    fn card(&mut self, ui: &mut egui::Ui, t: &Theme, i: usize, clash: bool, live: Option<Live>, now: f64, input_enabled: bool) -> CardOut {
        let mut out = CardOut::default();
        let focus = self.focus;
        let copied = self.copied;
        let mut copy_now = None;
        let d = &mut self.drafts[i];
        let id = d.rule.id;
        egui::Frame::new()
            .fill(Color32::from_black_alpha(60))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(12)))
            .corner_radius(14)
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                let w = ui.available_width();
                ui.set_width(w);

                // Kind, on/off, delete.
                let (row, _) = ui.allocate_exact_size(vec2(w, CONTROL_H), Sense::hover());
                let seg_w = (w - 110.0).clamp(160.0, 430.0);
                let mut kind = KINDS.iter().position(|k| *k == d.rule.kind).unwrap_or(0);
                let names = [tr!("Local  -L"), tr!("Remote  -R"), tr!("SOCKS  -D")];
                let seg_r = Rect::from_min_size(row.min, vec2(seg_w, CONTROL_H));
                if at(ui, seg_r, |ui| widgets::segmented(ui, t, Id::unique(("tunnel-kind", id)), &mut kind, &names)).changed() {
                    d.rule.kind = KINDS[kind];
                    if d.rule.kind != TunnelKind::Socks && d.to_host.trim().is_empty() {
                        d.to_host = "127.0.0.1".into();
                    }
                    out.commit = true;
                }
                let del = Rect::from_center_size(pos2(row.max.x - 16.0, row.center().y), vec2(32.0, 32.0));
                if at(ui, del, |ui| widgets::icon_button(ui, t, icon::TRASH, del.size(), true)).on_hover_text(tr!("Delete")).clicked() {
                    out.remove = true;
                }
                let sw = Rect::from_center_size(pos2(del.min.x - 14.0 - 20.0, row.center().y), vec2(40.0, 24.0));
                let sw_tip = if d.rule.enabled { tr!("Turn off") } else { tr!("Turn on") };
                if at(ui, sw, |ui| widgets::switch(ui, t, &mut d.rule.enabled)).on_hover_text(sw_tip).changed() {
                    out.commit = true;
                }

                ui.add_space(8.0);
                let what = match d.rule.kind {
                    TunnelKind::Local => tr!("Opens a service only the server can reach (a database, an admin panel) as a port on this PC."),
                    TunnelKind::Remote => tr!("Lets the server reach a service that runs on this PC or in its network."),
                    TunnelKind::Socks => tr!("A proxy on this PC: browsers and apps set to use it go online from the server. Speaks SOCKS5, SOCKS4 and HTTP."),
                };
                para(ui, what, fonts::ui(12.5), t.subtext);
                ui.add_space(10.0);

                // The fields.
                let (row, _) = ui.allocate_exact_size(vec2(w, LABEL_H + CONTROL_H), Sense::hover());
                let port_w = 150.0_f32.min(w);
                let port_label = match d.rule.kind {
                    TunnelKind::Remote => tr!("Port on the server"),
                    _ => tr!("Port on this PC"),
                };
                let port_hint = if d.rule.kind == TunnelKind::Socks { "1080" } else { "8080" };
                let pr = Rect::from_min_size(row.min, vec2(port_w, row.height()));
                let resp = at(ui, pr, |ui| widgets::field(ui, t, port_label, &mut d.port, port_hint, false));
                if focus == Some(id) && input_enabled {
                    resp.request_focus();
                }
                out.commit |= resp.lost_focus();
                if d.rule.kind != TunnelKind::Socks {
                    let arrow_w = 36.0;
                    let to_port_w = 110.0;
                    let host_x = pr.max.x + arrow_w;
                    let host_w = (row.max.x - host_x - 12.0 - to_port_w).max(60.0);
                    let cy = row.min.y + LABEL_H + CONTROL_H / 2.0;
                    fx::glyph(ui.painter(), pos2(pr.max.x + arrow_w / 2.0, cy), icon::ARROW, fonts::mono(13.0), t.subtext);
                    let host_label = match d.rule.kind {
                        TunnelKind::Remote => tr!("Host, as this PC sees it"),
                        _ => tr!("Host, as the server sees it"),
                    };
                    let hr = Rect::from_min_size(pos2(host_x, row.min.y), vec2(host_w, row.height()));
                    out.commit |= at(ui, hr, |ui| widgets::veiled_field(ui, t, host_label, &mut d.to_host, "127.0.0.1")).lost_focus();
                    let pr2 = Rect::from_min_size(pos2(hr.max.x + 12.0, row.min.y), vec2(to_port_w, row.height()));
                    out.commit |= at(ui, pr2, |ui| widgets::field(ui, t, tr!("Port"), &mut d.to_port, "80", false)).lost_focus();
                }
                let bad_port = |s: &str| !s.trim().is_empty() && parse_port(s).is_none();
                let problem = if bad_port(&d.port) || (d.rule.kind != TunnelKind::Socks && bad_port(&d.to_port)) {
                    Some(tr!("Port must be a number from 1 to 65535.").to_string())
                } else if clash {
                    Some(tr!("Another tunnel already uses port {port}", port = d.port.trim()))
                } else {
                    None
                };
                if let Some(p) = problem {
                    ui.add_space(6.0);
                    para(ui, &p, fonts::ui(12.5), t.err);
                }

                ui.add_space(6.0);
                let (lan_label, lan_hint) = match d.rule.kind {
                    TunnelKind::Remote => (
                        tr!("Listen on all of the server's addresses"),
                        tr!("Otherwise only programs on the server itself can connect. The server needs GatewayPorts enabled in sshd_config."),
                    ),
                    _ => (tr!("Open to the local network"), tr!("Other devices in your network can connect through this PC's address, not only programs on this PC.")),
                };
                if widgets::toggle(ui, t, &mut d.rule.lan, lan_label, lan_hint).changed() {
                    out.commit = true;
                }

                // Status, and the address to copy.
                ui.add_space(8.0);
                let (row, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
                let rule = d.rule();
                let state = live.as_ref().map_or_else(|| idle_state(&rule), |l| l.state.clone());
                let (dot, text) = match &state {
                    State::Off if !rule.enabled => (t.overlay, tr!("off").to_string()),
                    State::Off => (t.overlay, tr!("fill in the fields to start").to_string()),
                    State::Waiting => (t.overlay, tr!("starts when the tab connects").to_string()),
                    State::Starting => (t.warn, tr!("starting…").to_string()),
                    State::Active => {
                        let mut text = tr!("active").to_string();
                        if let Some(st) = live.as_ref().map(|l| &l.stats) {
                            let n = st.conns.load(Relaxed);
                            if n > 0 {
                                text += &format!("  ·  {}", tr!("{n} {n:connection|connections}", n = n));
                            }
                            let (up, down) = (st.up.load(Relaxed), st.down.load(Relaxed));
                            if up + down > 0 {
                                text += &format!("  ·  ↑ {}  ↓ {}", crate::files::human_size(up), crate::files::human_size(down));
                            }
                        }
                        (t.ok, text)
                    }
                    State::Failed(e) => (t.err, e.clone()),
                };
                let failed = matches!(state, State::Failed(_));
                let addr = match rule.kind {
                    TunnelKind::Remote => None,
                    _ if rule.port == 0 || state != State::Active => None,
                    _ if rule.lan => Some(format!("{}:{}", lan_ip().unwrap_or_else(|| "0.0.0.0".into()), rule.port)),
                    _ => Some(format!("127.0.0.1:{}", rule.port)),
                };
                // The address chip sits on the right; the status text wraps short of it.
                let chip = addr.map(|addr| {
                    let flash = copied.is_some_and(|(c, at)| c == id && now - at < 1.4);
                    let label = if flash { tr!("copied").to_string() } else { addr.clone() };
                    let gw = ui.painter().layout_no_wrap(label.clone(), fonts::mono(12.0), t.text).size().x;
                    let r = Rect::from_min_max(pos2(row.max.x - gw - 36.0, row.min.y), row.max);
                    (addr, label, flash, r)
                });
                let text_right = chip.as_ref().map_or(row.max.x, |c| c.3.min.x - 12.0);
                let p = ui.painter();
                p.circle_filled(pos2(row.min.x + 5.0, row.center().y), 4.0, dot);
                let g = p.layout_no_wrap(text, fonts::ui(12.5), if failed { t.err } else { t.subtext });
                let clip = Rect::from_min_max(row.min, pos2(text_right, row.max.y));
                ui.painter_at(clip).galley(pos2(row.min.x + 18.0, row.center().y - g.size().y / 2.0 + fx::text_dy(p, &fonts::ui(12.5))), g.clone(), t.subtext);
                if g.size().x > clip.width() - 18.0 {
                    ui.interact(clip, Id::unique(("tunnel-status", id)), Sense::hover()).on_hover_text(g.text());
                }

                if let Some((addr, label, flash, r)) = chip {
                    let resp = ui.interact(r, Id::unique(("tunnel-addr", id)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tr!("Copy the address"));
                    let h = widgets::hover_t(ui, &resp);
                    let p = ui.painter();
                    p.rect_filled(r, CornerRadius::same(12), Color32::from_white_alpha((10.0 + 14.0 * h) as u8));
                    let c = if flash { t.ok } else { t.text };
                    veil::text_auto(p, pos2(r.min.x + 10.0, r.center().y), Align2::LEFT_CENTER, &label, fonts::mono(12.0), c, t.surface);
                    fx::glyph(p, pos2(r.max.x - 14.0, r.center().y), if flash { icon::CHECK } else { icon::COPY }, fonts::mono(11.0), c);
                    if resp.clicked() {
                        ui.ctx().copy_text(if rule.kind == TunnelKind::Socks { format!("socks5://{addr}") } else { addr });
                        copy_now = Some((id, now));
                    }
                    if flash {
                        ui.ctx().request_repaint_after(Duration::from_millis(200));
                    }
                }
            });
        if copy_now.is_some() {
            self.copied = copy_now;
        }
        if self.focus == Some(id) {
            self.focus = None;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_parse_and_clash() {
        let mut a = Tunnel::new(TunnelKind::Local);
        a.port = 8080;
        a.to_host = "db".into();
        a.to_port = 5432;
        let mut b = Tunnel::new(TunnelKind::Socks);
        b.port = 8080;
        let mut c = Tunnel::new(TunnelKind::Remote);
        c.port = 8080;
        c.to_host = "127.0.0.1".into();
        c.to_port = 3000;
        let mut ed = Editor::new(&[a.clone(), b, c]);
        // Local and SOCKS share this PC's ports; the -R one listens on the server.
        assert!(ed.clash(0) && ed.clash(1) && !ed.clash(2));
        assert_eq!(ed.rules()[0], a);
        ed.drafts[0].port = "80a".into();
        assert!(!ed.rules()[0].is_valid());
        assert_eq!(ed.free_port(8080), 8081);
    }
}
