//! Custom widgets with eased hover/press animations.
//!
//! Everything sits on a 4 px grid: controls are [`CONTROL_H`] tall, field labels take
//! [`LABEL_H`], icons and text are placed on their optical centre (see `fx::glyph`/`fx::text`),
//! so a button next to an input, or an icon next to a label, lines up exactly.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, LayerId, Margin, Order, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, TextEdit, Ui, emath::TSTransform, pos2, vec2,
};

use crate::anim;
use crate::fonts;
use crate::fx;
use crate::theme::{Theme, mix};
use crate::veil;

/// Height of buttons, inputs and segmented controls.
pub const CONTROL_H: f32 = 36.0;
/// Height of the label row above a field (so a button beside a field skips exactly this).
pub const LABEL_H: f32 = 20.0;
/// Corner radius of controls.
pub const RADIUS: f32 = 12.0;

fn hover_t(ui: &Ui, resp: &Response) -> f32 {
    anim::toggle(ui.ctx(), resp.id.with("hover"), resp.hovered(), 0.14)
}

fn cr(r: f32) -> CornerRadius {
    CornerRadius::same(r.clamp(0.0, 255.0) as u8)
}

/// Accent-filled call to action: a light gradient from the top, a soft glow on hover.
pub fn primary(ui: &mut Ui, t: &Theme, icon: &str, text: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(fit(ui, icon, text, size), Sense::click());
    let h = hover_t(ui, &resp);
    let press = anim::toggle(ui.ctx(), resp.id.with("press"), resp.is_pointer_button_down_on(), 0.08);
    let rect = rect.shrink(press);
    let p = ui.painter();
    if h > 0.0 {
        fx::glow(p, rect.translate(vec2(0.0, 4.0)), RADIUS, t.accent.gamma_multiply(0.30 * h), 20.0, 0.0);
    }
    p.rect_filled(rect, cr(RADIUS), mix(t.accent, Color32::WHITE, 0.06 * h));
    fx::vgradient(p, rect, RADIUS, |y| Color32::from_white_alpha(((1.0 - y) * (30.0 + 12.0 * h)) as u8));
    fx::vgradient_stroke(p, rect, RADIUS, 1.0, Color32::from_white_alpha(70), Color32::from_white_alpha(10));
    label_with_icon(p, rect, icon, text, t.crust, true);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Quiet secondary button: a translucent plate with a hairline edge.
pub fn secondary(ui: &mut Ui, t: &Theme, icon: &str, text: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(fit(ui, icon, text, size), Sense::click());
    let h = hover_t(ui, &resp);
    let p = ui.painter();
    p.rect_filled(rect, cr(RADIUS), Color32::from_white_alpha((6.0 + 8.0 * h) as u8));
    fx::vgradient_stroke(
        p,
        rect,
        RADIUS,
        1.0,
        fx::mix_alpha(Color32::from_white_alpha(22), t.accent.gamma_multiply(0.7), h),
        Color32::from_white_alpha((10.0 + 8.0 * h) as u8),
    );
    label_with_icon(p, rect, icon, text, mix(t.subtext, t.text, 0.55 + 0.45 * h), false);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Danger button (filled red).
pub fn danger(ui: &mut Ui, t: &Theme, text: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(fit(ui, "", text, size), Sense::click());
    let h = hover_t(ui, &resp);
    let p = ui.painter();
    if h > 0.0 {
        fx::glow(p, rect.translate(vec2(0.0, 4.0)), RADIUS, t.err.gamma_multiply(0.3 * h), 18.0, 0.0);
    }
    p.rect_filled(rect, cr(RADIUS), mix(t.err, Color32::WHITE, 0.06 * h));
    fx::vgradient(p, rect, RADIUS, |y| Color32::from_white_alpha(((1.0 - y) * 26.0) as u8));
    label_with_icon(p, rect, "", text, t.crust, true);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Icon + label centred as one group inside `rect`.
/// Width a button needs for its icon and label (padding included); buttons never get
/// narrower than this, so translated labels are not clipped.
pub fn button_width(ui: &Ui, icon: &str, text: &str) -> f32 {
    let g = ui.painter().layout_no_wrap(text.to_string(), fonts::bold(13.5), Color32::PLACEHOLDER);
    let icon_box = if icon.is_empty() { 0.0 } else { 14.0 };
    let gap = if icon.is_empty() || text.is_empty() { 0.0 } else { 8.0 };
    (icon_box + gap + g.size().x + 40.0).ceil()
}

fn fit(ui: &Ui, icon: &str, text: &str, size: egui::Vec2) -> egui::Vec2 {
    vec2(size.x.max(button_width(ui, icon, text)), size.y)
}

fn label_with_icon(p: &egui::Painter, rect: Rect, icon: &str, text: &str, color: Color32, strong: bool) {
    let font = if strong { fonts::bold(13.5) } else { fonts::medium(13.5) };
    let g = p.layout_no_wrap(text.to_string(), font.clone(), color);
    let icon_box = if icon.is_empty() { 0.0 } else { 14.0 };
    let gap = if icon.is_empty() || text.is_empty() { 0.0 } else { 8.0 };
    let total = icon_box + gap + g.size().x;
    let x = (rect.center().x - total / 2.0).round();
    let cy = rect.center().y;
    if !icon.is_empty() {
        fx::glyph(p, pos2(x + icon_box / 2.0, cy), icon, fonts::mono(12.5), color);
    }
    if !text.is_empty() {
        fx::text(p, pos2(x + icon_box + gap, cy), Align2::LEFT_CENTER, text, font, color);
    }
}

/// Small square icon button (titlebar, toolbar).
pub fn icon_button(ui: &mut Ui, t: &Theme, icon: &str, size: egui::Vec2, danger: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = hover_t(ui, &resp);
    let p = ui.painter();
    if h > 0.0 {
        let fill = if danger { t.err.gamma_multiply(0.9 * h) } else { Color32::from_white_alpha((14.0 * h) as u8) };
        p.rect_filled(rect, cr(8.0), fill);
    }
    let color = if danger { mix(t.subtext, t.crust, h) } else { mix(t.subtext, t.text, h) };
    fx::glyph(p, rect.center(), icon, fonts::mono(13.0), color);
    resp
}

/// Switch with a sliding knob; the label (and optional hint) on the left.
pub fn toggle(ui: &mut Ui, t: &Theme, on: &mut bool, label: &str, hint: &str) -> Response {
    let w = ui.available_width();
    // The hint wraps short of the switch; translations can be much longer than English.
    let hint_galley = (!hint.is_empty()).then(|| ui.painter().layout(hint.to_owned(), fonts::ui(11.5), t.subtext, (w - 60.0).max(80.0)));
    let h = match &hint_galley {
        None => CONTROL_H,
        Some(g) => (34.0 + g.size().y).max(48.0),
    };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let k = anim::toggle(ui.ctx(), resp.id.with("knob"), *on, 0.18);
    let hov = hover_t(ui, &resp);
    let p = ui.painter();
    if hov > 0.0 {
        p.rect_filled(rect.expand2(vec2(8.0, 0.0)), cr(10.0), Color32::from_white_alpha((7.0 * hov) as u8));
    }
    match hint_galley {
        None => {
            fx::text(p, pos2(rect.min.x, rect.center().y), Align2::LEFT_CENTER, label, fonts::ui(13.5), t.text);
        }
        Some(g) => {
            fx::text(p, pos2(rect.min.x, rect.min.y + 16.0), Align2::LEFT_CENTER, label, fonts::ui(13.5), t.text);
            let top = rect.min.y + 34.0 - g.rows.first().map_or(g.size().y, |r| r.height()) / 2.0;
            p.galley(pos2(rect.min.x, top), g, t.subtext);
        }
    }
    let track = Rect::from_center_size(pos2(rect.max.x - 20.0, rect.center().y), vec2(40.0, 24.0));
    p.rect_filled(track, cr(12.0), mix(Color32::from_rgb(0x2a, 0x26, 0x36), t.accent, k));
    if k > 0.0 {
        fx::vgradient(p, track, 12.0, |y| Color32::from_white_alpha(((1.0 - y) * 30.0 * k) as u8));
    }
    let knob_x = track.min.x + 12.0 + k * 16.0;
    p.circle_filled(pos2(knob_x, track.center().y), 9.0, mix(t.text, Color32::WHITE, k));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Segmented control with a highlight that slides between options.
pub fn segmented(ui: &mut Ui, t: &Theme, id: Id, selected: &mut usize, options: &[&str]) -> Response {
    let w = ui.available_width();
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, CONTROL_H), Sense::click());
    let n = options.len().max(1);
    let seg = rect.width() / n as f32;
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos() {
            let i = (((pos.x - rect.min.x) / seg) as usize).min(n - 1);
            if i != *selected {
                *selected = i;
                resp.mark_changed();
            }
        }
    let x = ui.ctx().animate_value_with_time(id.with("seg"), *selected as f32, 0.22);
    let p = ui.painter();
    p.rect(rect, cr(RADIUS), Color32::from_black_alpha(70), Stroke::new(1.0, Color32::from_white_alpha(10)), StrokeKind::Inside);
    let hl = Rect::from_min_size(pos2(rect.min.x + x * seg + 3.0, rect.min.y + 3.0), vec2(seg - 6.0, rect.height() - 6.0));
    fx::plate(p, hl, RADIUS - 3.0, t.surface, t.accent, 0.7);
    for (i, o) in options.iter().enumerate() {
        let c = pos2(rect.min.x + seg * (i as f32 + 0.5), rect.center().y);
        let active = 1.0 - (x - i as f32).abs().min(1.0);
        fx::text(p, c, Align2::CENTER_CENTER, *o, fonts::medium(13.0), mix(t.subtext, t.text, active));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// The label row above a field: exactly [`LABEL_H`] tall.
pub fn field_label(ui: &mut Ui, t: &Theme, label: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), LABEL_H), Sense::hover());
    fx::text(ui.painter(), pos2(r.min.x + 2.0, r.min.y + 7.0), Align2::LEFT_CENTER, label, fonts::medium(12.0), t.subtext);
}

/// Labelled text field with an animated focus ring.
pub fn field(ui: &mut Ui, t: &Theme, label: &str, value: &mut String, hint: &str, password: bool) -> Response {
    if !label.is_empty() {
        field_label(ui, t, label);
    }
    input(ui, t, value, hint, password, "")
}

/// Bare input box (optionally with a leading icon).
pub fn input(ui: &mut Ui, t: &Theme, value: &mut String, hint: &str, password: bool, icon: &str) -> Response {
    input_impl(ui, t, value, hint, password, icon, false, false)
}

/// Labelled input whose content is sensitive (a host or IP): in streamer mode it is masked
/// for screen capture and shown for real in the overlay.
pub fn veiled_field(ui: &mut Ui, t: &Theme, label: &str, value: &mut String, hint: &str) -> Response {
    if !label.is_empty() {
        field_label(ui, t, label);
    }
    let veiled = veil::enabled();
    input_impl(ui, t, value, hint, veiled, "", veiled, false)
}

#[allow(clippy::too_many_arguments)]
fn input_impl(
    ui: &mut Ui,
    t: &Theme,
    value: &mut String,
    hint: &str,
    password: bool,
    icon: &str,
    veiled: bool,
    mono: bool,
) -> Response {
    let veiled = veiled || (veil::enabled() && !password && !value.is_empty() && !veil::find(value).is_empty());
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, CONTROL_H), Sense::hover());
    let left = if icon.is_empty() { 14.0 } else { 38.0 };
    let font = if mono { fonts::mono(13.0) } else { fonts::ui(13.5) };
    // The text box is centred on the line box; shift it so the text sits on its optical centre.
    let dy = fx::text_dy(ui.painter(), &font);
    let inner = Rect::from_min_max(pos2(rect.min.x + left, rect.min.y), pos2(rect.max.x - 12.0, rect.max.y)).translate(vec2(0.0, dy));
    // Reserve slots so the box is painted *under* the text, filled in once we know the focus state.
    let ring_slot = ui.painter().add(Shape::Noop);
    let box_slot = ui.painter().add(Shape::Noop);
    let edit = TextEdit::singleline(value)
        .hint_text(egui::RichText::new(hint).font(font.clone()).color(t.subtext.gamma_multiply(0.6)))
        .password(password)
        .text_color(if veiled { Color32::TRANSPARENT } else { t.text })
        .font(font.clone())
        .frame(Frame::NONE)
        .margin(Margin::ZERO)
        .align(Align2::LEFT_CENTER)
        .min_size(inner.size())
        .desired_width(inner.width());
    let resp = if veiled {
        ui.scope(|ui| {
            ui.visuals_mut().text_cursor.stroke.color = Color32::TRANSPARENT;
            ui.put(inner, edit)
        })
        .inner
    } else {
        ui.put(inner, edit)
    };
    let f = anim::toggle(ui.ctx(), resp.id.with("focus"), resp.has_focus(), 0.18);
    let hov = anim::toggle(ui.ctx(), resp.id.with("hov"), ui.rect_contains_pointer(rect), 0.14);
    let p = ui.painter();
    p.set(
        box_slot,
        egui::epaint::RectShape::new(
            rect,
            cr(RADIUS),
            Color32::from_black_alpha(90),
            Stroke::new(1.0, fx::mix_alpha(Color32::from_white_alpha((12.0 + 8.0 * hov) as u8), t.accent.gamma_multiply(0.9), f)),
            StrokeKind::Inside,
        ),
    );
    if f > 0.0 {
        // A soft accent ring outside the box, like the site's focused inputs.
        p.set(
            ring_slot,
            egui::epaint::RectShape::new(
                rect.expand(2.0),
                cr(RADIUS + 2.0),
                Color32::TRANSPARENT,
                Stroke::new(3.0, t.accent.gamma_multiply(0.16 * f)),
                StrokeKind::Inside,
            ),
        );
    }
    if !icon.is_empty() {
        fx::glyph(p, pos2(rect.min.x + 20.0, rect.center().y), icon, fonts::mono(12.5), mix(t.subtext, t.accent, f));
    }
    if veiled && !value.is_empty() {
        let g = p.layout_no_wrap(value.clone(), font.clone(), Color32::PLACEHOLDER);
        let pos = pos2(inner.min.x, inner.center().y - g.size().y / 2.0);
        let mut extra = Vec::new();
        if resp.has_focus() {
            let idx = TextEdit::load_state(ui.ctx(), resp.id)
                .and_then(|st| st.cursor.char_range())
                .map_or(value.chars().count(), |r| r.primary.index.into());
            let prefix: String = value.chars().take(idx).collect();
            let x = pos.x + p.layout_no_wrap(prefix, font, Color32::PLACEHOLDER).size().x;
            extra.push((Rect::from_min_size(pos2(x, pos.y), vec2(2.0, g.size().y)), t.accent));
        }
        veil::cover_with(p, inner, t.mantle, 0.0, vec![(pos, g, t.text)], extra);
    }
    resp
}

/// Section heading: accent icon + small caps title, on one optical line.
pub fn section(ui: &mut Ui, t: &Theme, icon: &str, title: &str) {
    ui.add_space(16.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter();
    let cy = r.center().y;
    fx::glyph(p, pos2(r.min.x + 7.0, cy), icon, fonts::mono(12.0), t.accent);
    fx::text(p, pos2(r.min.x + 22.0, cy), Align2::LEFT_CENTER, title.to_uppercase(), fonts::bold(11.0), t.subtext);
    ui.add_space(6.0);
}

/// A compact radio tile for a grid of options (languages): a dot and a label, centred
/// vertically. The selected tile gets the lit plate.
pub fn option_tile(ui: &mut Ui, t: &Theme, selected: bool, label: &str, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = hover_t(ui, &resp);
    let s = anim::toggle(ui.ctx(), resp.id.with("sel"), selected, 0.18);
    let p = ui.painter();
    p.rect(rect, cr(RADIUS), Color32::from_black_alpha(50), Stroke::new(1.0, Color32::from_white_alpha(8)), StrokeKind::Inside);
    if s > 0.0 || h > 0.0 {
        fx::plate(p, rect, RADIUS, Color32::from_white_alpha((5.0 + 6.0 * h) as u8), t.accent, s);
    }
    let dot = pos2(rect.min.x + 18.0, rect.center().y);
    p.circle_stroke(dot, 6.0, Stroke::new(1.5, mix(t.overlay, t.accent, s.max(h * 0.5))));
    if s > 0.0 {
        p.circle_filled(dot, 3.5 * s, t.accent);
    }
    let clip = p.with_clip_rect(Rect::from_min_max(pos2(dot.x + 14.0, rect.min.y), pos2(rect.max.x - 8.0, rect.max.y)).intersect(p.clip_rect()));
    fx::text(&clip, pos2(dot.x + 16.0, rect.center().y), Align2::LEFT_CENTER, label, fonts::medium(13.0), mix(t.subtext, t.text, s.max(h)));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// One selectable row in a short list (e.g. picking a jump server): a radio dot, a title and a
/// mono subtitle. 44 px tall.
pub fn choice_row(ui: &mut Ui, t: &Theme, selected: bool, title: &str, sub: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let h = hover_t(ui, &resp);
    let s = anim::toggle(ui.ctx(), resp.id.with("sel"), selected, 0.18);
    let p = ui.painter();
    if s > 0.0 || h > 0.0 {
        fx::plate(p, rect, 10.0, Color32::from_white_alpha((5.0 + 6.0 * h) as u8), t.accent, s);
    }
    let dot = pos2(rect.min.x + 20.0, rect.center().y);
    p.circle_stroke(dot, 7.0, Stroke::new(1.5, mix(t.overlay, t.accent, s.max(h * 0.5))));
    if s > 0.0 {
        p.circle_filled(dot, 4.0 * s, t.accent);
    }
    let tx = rect.min.x + 40.0;
    let clip = p.with_clip_rect(Rect::from_min_max(pos2(tx, rect.min.y), pos2(rect.max.x - 12.0, rect.max.y)).intersect(p.clip_rect()));
    let dy_t = fx::text_dy(p, &fonts::medium(13.0));
    let dy_s = fx::text_dy(p, &fonts::mono(11.5));
    veil::text_auto(&clip, pos2(tx, rect.center().y - 8.0 + dy_t), Align2::LEFT_CENTER, title, fonts::medium(13.0), t.text, t.mantle);
    veil::text_auto(&clip, pos2(tx, rect.center().y + 9.0 + dy_s), Align2::LEFT_CENTER, sub, fonts::mono(11.5), t.subtext, t.mantle);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A modal that pops in with HyDE's `winIn` curve and fades its backdrop.
/// Returns the closure result and whether the user dismissed it (Esc / click outside).
pub fn dialog<R>(ctx: &egui::Context, t: &Theme, id: Id, opened_at: f64, width: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let p = anim::since(ctx, opened_at, 0.42);
    let scale = 0.94 + 0.06 * anim::WIN_IN.eval(p);
    let alpha = anim::smooth((p * 2.2).min(1.0));
    let radius = 20.0;
    let frame = Frame::new()
        .fill(t.mantle)
        .stroke(Stroke::NONE)
        .corner_radius(cr(radius))
        .inner_margin(Margin::same(24))
        .shadow(egui::epaint::Shadow { offset: [0, 16], blur: 48, spread: 0, color: Color32::from_rgba_premultiplied(2, 1, 8, 170) });
    let modal = egui::Modal::new(id)
        .frame(frame)
        .backdrop_color(Color32::from_rgba_premultiplied(2, 1, 6, (150.0 * alpha) as u8))
        .show(ctx, |ui| {
            ui.multiply_opacity(alpha);
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            let slot = ui.painter().add(Shape::Noop);
            // Tall dialogs scroll instead of running off a small window.
            // The modal's area limits the available height on its first frames, so ask for the
            // content's height (measured last frame) explicitly.
            let max_h = (ctx.content_rect().height() - 112.0).max(160.0);
            let hid = id.with("content-h");
            let last_h: f32 = ctx.data(|d| d.get_temp(hid)).unwrap_or(0.0);
            let sa = egui::ScrollArea::vertical()
                .max_height(max_h)
                .min_scrolled_height(last_h.min(max_h))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(width);
                    add(ui)
                });
            let content_h = sa.content_size.y;
            if (content_h - last_h).abs() > 0.5 {
                ctx.data_mut(|d| d.insert_temp(hid, content_h));
                ctx.request_repaint();
            }
            let out = sa.inner;
            // The plate's sky light, under the content now that its size is known.
            let r = ui.min_rect().expand(24.0);
            let wash = (110.0 / r.height().max(1.0)).min(1.0);
            let acc = t.accent;
            let mesh = fx::vgradient_mesh(r, radius, |y| {
                let k = (1.0 - y / wash).max(0.0);
                acc.gamma_multiply(0.07 * k * k)
            });
            ui.painter().set(slot, Shape::mesh(mesh));
            out
        });
    let rect = modal.response.rect;
    let lp = ctx.layer_painter(modal.response.layer_id);
    fx::vgradient_stroke(
        &lp,
        rect,
        radius,
        1.0,
        fx::mix_alpha(Color32::from_white_alpha(22), t.accent.gamma_multiply(0.45), 0.5).gamma_multiply(alpha),
        Color32::from_white_alpha(10).gamma_multiply(alpha),
    );
    let c = rect.center().to_vec2();
    ctx.set_transform_layer(LayerId::new(Order::Foreground, id), TSTransform::new(c * (1.0 - scale), scale));
    let dismissed = modal.should_close();
    (modal.inner, dismissed)
}
