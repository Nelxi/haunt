//! Custom widgets with eased hover/press animations.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, LayerId, Margin, Order, Rect,
    Response, Sense, Stroke, StrokeKind, TextEdit, Ui, emath::TSTransform, pos2, vec2,
};

use crate::anim;
use crate::fonts;
use crate::fx;
use crate::theme::{Theme, mix};

fn hover_t(ui: &Ui, resp: &Response) -> f32 {
    anim::toggle(ui.ctx(), resp.id.with("hover"), resp.hovered(), 0.14)
}

/// Accent-filled call to action with a soft glow on hover.
pub fn primary(ui: &mut Ui, t: &Theme, icon: &str, text: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = hover_t(ui, &resp);
    let press = anim::toggle(ui.ctx(), resp.id.with("press"), resp.is_pointer_button_down_on(), 0.08);
    let rect = rect.shrink(press * 1.0);
    let p = ui.painter();
    let r = 10.0;
    if h > 0.0 {
        fx::glow(p, rect, r, t.accent.gamma_multiply(0.35 * h), 18.0, 0.0);
    }
    let fill = mix(t.accent, Color32::WHITE, 0.08 * h);
    p.rect_filled(rect, CornerRadius::same(r as u8), fill);
    label_with_icon(p, rect, icon, text, t.crust, true);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Outlined secondary button.
pub fn secondary(ui: &mut Ui, t: &Theme, icon: &str, text: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = hover_t(ui, &resp);
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(10),
        t.surface.gamma_multiply(0.55 + 0.45 * h),
        Stroke::new(1.0, mix(t.overlay, t.accent, 0.6 * h)),
        StrokeKind::Inside,
    );
    label_with_icon(p, rect, icon, text, mix(t.subtext, t.text, 0.5 + 0.5 * h), false);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Danger button (filled red).
pub fn danger(ui: &mut Ui, t: &Theme, text: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = hover_t(ui, &resp);
    let p = ui.painter();
    if h > 0.0 {
        fx::glow(p, rect, 10.0, t.err.gamma_multiply(0.35 * h), 16.0, 0.0);
    }
    p.rect_filled(rect, CornerRadius::same(10), mix(t.err, Color32::WHITE, 0.08 * h));
    label_with_icon(p, rect, "", text, t.crust, true);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn label_with_icon(p: &egui::Painter, rect: Rect, icon: &str, text: &str, color: Color32, strong: bool) {
    let font = if strong { fonts::bold(13.5) } else { fonts::ui(13.5) };
    let g = p.layout_no_wrap(text.to_string(), font, color);
    let icon_w = if icon.is_empty() { 0.0 } else { 20.0 };
    let total = g.size().x + icon_w;
    let x = rect.center().x - total / 2.0;
    if !icon.is_empty() {
        p.text(pos2(x + 7.0, rect.center().y), Align2::CENTER_CENTER, icon, fonts::mono(13.0), color);
    }
    p.galley(pos2(x + icon_w, rect.center().y - g.size().y / 2.0), g, color);
}

/// Small square icon button (titlebar, toolbar).
pub fn icon_button(ui: &mut Ui, t: &Theme, icon: &str, size: egui::Vec2, danger: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = hover_t(ui, &resp);
    let p = ui.painter();
    if h > 0.0 {
        let fill = if danger { t.err.gamma_multiply(0.9 * h) } else { t.overlay.gamma_multiply(0.6 * h) };
        p.rect_filled(rect, CornerRadius::same(8), fill);
    }
    let color = if danger { mix(t.subtext, t.crust, h) } else { mix(t.subtext, t.text, h) };
    p.text(rect.center(), Align2::CENTER_CENTER, icon, fonts::mono(13.0), color);
    resp
}

/// iOS-style switch with a sliding knob.
pub fn toggle(ui: &mut Ui, t: &Theme, on: &mut bool, label: &str, hint: &str) -> Response {
    let w = ui.available_width();
    let h = if hint.is_empty() { 30.0 } else { 44.0 };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let k = anim::toggle(ui.ctx(), resp.id.with("knob"), *on, 0.18);
    let hov = hover_t(ui, &resp);
    let p = ui.painter();
    if hov > 0.0 {
        p.rect_filled(rect.expand2(vec2(6.0, 0.0)), CornerRadius::same(8), t.surface.gamma_multiply(0.5 * hov));
    }
    p.text(pos2(rect.min.x, rect.min.y + 8.0), Align2::LEFT_TOP, label, fonts::ui(13.5), t.text);
    if !hint.is_empty() {
        p.text(pos2(rect.min.x, rect.min.y + 26.0), Align2::LEFT_TOP, hint, fonts::ui(11.5), t.subtext);
    }
    let track = Rect::from_center_size(pos2(rect.max.x - 22.0, rect.center().y), vec2(38.0, 22.0));
    p.rect_filled(track, CornerRadius::same(11), mix(t.overlay, t.accent, k));
    let knob_x = track.min.x + 11.0 + k * 16.0;
    p.circle_filled(pos2(knob_x, track.center().y), 8.0, mix(t.text, t.crust, k * 0.85));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Segmented control with an animated highlight that slides between options.
pub fn segmented(ui: &mut Ui, t: &Theme, id: Id, selected: &mut usize, options: &[&str]) -> Response {
    let w = ui.available_width().min(360.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
    let n = options.len().max(1);
    let seg = rect.width() / n as f32;
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let i = (((pos.x - rect.min.x) / seg) as usize).min(n - 1);
            if i != *selected {
                *selected = i;
                resp.mark_changed();
            }
        }
    }
    let x = ui.ctx().animate_value_with_time(id.with("seg"), *selected as f32, 0.2);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(10), t.crust.gamma_multiply(0.8));
    let hl = Rect::from_min_size(pos2(rect.min.x + x * seg + 3.0, rect.min.y + 3.0), vec2(seg - 6.0, rect.height() - 6.0));
    p.rect_filled(hl, CornerRadius::same(8), t.surface);
    p.rect_stroke(hl, CornerRadius::same(8), Stroke::new(1.0, t.accent.gamma_multiply(0.6)), StrokeKind::Inside);
    for (i, o) in options.iter().enumerate() {
        let c = pos2(rect.min.x + seg * (i as f32 + 0.5), rect.center().y);
        let active = 1.0 - (x - i as f32).abs().min(1.0);
        p.text(c, Align2::CENTER_CENTER, *o, fonts::ui(13.0), mix(t.subtext, t.text, active));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Labelled text field with an animated focus ring.
pub fn field(ui: &mut Ui, t: &Theme, label: &str, value: &mut String, hint: &str, password: bool) -> Response {
    if !label.is_empty() {
        ui.label(egui::RichText::new(label).size(12.0).color(t.subtext));
        ui.add_space(-2.0);
    }
    input(ui, t, value, hint, password, "")
}

/// Bare input box (optionally with a leading icon).
pub fn input(ui: &mut Ui, t: &Theme, value: &mut String, hint: &str, password: bool, icon: &str) -> Response {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 36.0), Sense::hover());
    let left = if icon.is_empty() { 12.0 } else { 34.0 };
    let inner = Rect::from_min_max(pos2(rect.min.x + left, rect.min.y), pos2(rect.max.x - 10.0, rect.max.y));
    let edit = TextEdit::singleline(value)
        .hint_text(egui::RichText::new(hint).color(t.subtext.gamma_multiply(0.7)))
        .password(password)
        .frame(Frame::NONE)
        .margin(Margin::symmetric(0, 9))
        .desired_width(inner.width());
    let resp = ui.put(inner, edit);
    let f = anim::toggle(ui.ctx(), resp.id.with("focus"), resp.has_focus(), 0.18);
    let p = ui.painter();
    // Painted under the text by inserting at layer bottom is not possible here, so use
    // a translucent fill that keeps the text readable.
    p.rect(
        rect,
        CornerRadius::same(10),
        t.crust.gamma_multiply(0.55),
        Stroke::new(1.0 + f * 0.5, mix(t.overlay.gamma_multiply(0.8), t.accent, f)),
        StrokeKind::Inside,
    );
    if f > 0.0 {
        fx::glow(p, rect, 10.0, t.accent.gamma_multiply(0.12 * f), 12.0, 0.0);
    }
    if !icon.is_empty() {
        p.text(pos2(rect.min.x + 17.0, rect.center().y), Align2::CENTER_CENTER, icon, fonts::mono(12.5), mix(t.subtext, t.accent, f));
    }
    resp
}

pub fn section(ui: &mut Ui, t: &Theme, icon: &str, title: &str) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(icon).font(fonts::mono(12.0)).color(t.accent));
        ui.label(egui::RichText::new(title.to_uppercase()).font(fonts::bold(11.5)).color(t.subtext));
    });
    ui.add_space(4.0);
}

/// A modal that pops in with HyDE's `winIn` curve and fades its backdrop.
/// Returns the closure result and whether the user dismissed it (Esc / click outside).
pub fn dialog<R>(
    ctx: &egui::Context,
    t: &Theme,
    id: Id,
    opened_at: f64,
    width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> (R, bool) {
    let p = anim::since(ctx, opened_at, 0.42);
    let scale = 0.9 + 0.1 * anim::WIN_IN.eval(p);
    let alpha = anim::smooth((p * 2.2).min(1.0));
    let frame = Frame::new()
        .fill(t.mantle)
        .stroke(Stroke::new(1.0, t.overlay))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(22))
        .shadow(egui::epaint::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(140) });
    let modal = egui::Modal::new(id)
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha((110.0 * alpha) as u8))
        .show(ctx, |ui| {
            ui.multiply_opacity(alpha);
            ui.set_width(width);
            add(ui)
        });
    let c = modal.response.rect.center().to_vec2();
    ctx.set_transform_layer(LayerId::new(Order::Foreground, id), TSTransform::new(c * (1.0 - scale), scale));
    let dismissed = modal.should_close();
    (modal.inner, dismissed)
}
