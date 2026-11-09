//! Drawing effects: gradient borders, glows, glass cards and the mascot.

use std::f32::consts::{PI, TAU};

use eframe::egui::{
    self, Color32, CornerRadius, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, epaint, pos2, vec2,
};

use crate::theme::mix;

const CORNER_SEGMENTS: usize = 10;

/// Points along a rounded rectangle, clockwise from the top-left arc.
/// Always returns `4 * (CORNER_SEGMENTS + 1)` points so rings line up for meshing.
pub fn rounded_path(rect: Rect, radius: f32) -> Vec<Pos2> {
    let r = radius.max(0.0).min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (pos2(rect.min.x + r, rect.min.y + r), PI),
        (pos2(rect.max.x - r, rect.min.y + r), 1.5 * PI),
        (pos2(rect.max.x - r, rect.max.y - r), 0.0),
        (pos2(rect.min.x + r, rect.max.y - r), 0.5 * PI),
    ];
    let mut out = Vec::with_capacity(4 * (CORNER_SEGMENTS + 1));
    for (c, start) in corners {
        for i in 0..=CORNER_SEGMENTS {
            let a = start + (i as f32 / CORNER_SEGMENTS as f32) * (PI / 2.0);
            out.push(pos2(c.x + r * a.cos(), c.y + r * a.sin()));
        }
    }
    out
}

/// Anti-aliased border whose color rotates around the rect like Hyprland's
/// `col.active_border = a b 45deg` with the `borderangle` animation.
pub fn gradient_border(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    width: f32,
    a: Color32,
    b: Color32,
    angle: f32,
) {
    if width <= 0.0 {
        return;
    }
    let feather = 0.8;
    let rings = [
        (rounded_path(rect.expand(feather), radius + feather), 0.0),
        (rounded_path(rect, radius), 1.0),
        (rounded_path(rect.shrink(width), (radius - width).max(0.0)), 1.0),
        (rounded_path(rect.shrink(width + feather), (radius - width - feather).max(0.0)), 0.0),
    ];
    let center = rect.center();
    let n = rings[0].0.len();
    let mut mesh = Mesh::default();
    for (path, alpha) in &rings {
        for p in path {
            let ang = (p.y - center.y).atan2(p.x - center.x);
            let t = 0.5 + 0.5 * (ang - angle).cos();
            let c = mix(a, b, t);
            mesh.colored_vertex(*p, if *alpha > 0.0 { c } else { Color32::TRANSPARENT });
        }
    }
    for ring in 0..3u32 {
        let base = ring * n as u32;
        let next = base + n as u32;
        for i in 0..n as u32 {
            let j = (i + 1) % n as u32;
            mesh.add_triangle(base + i, base + j, next + i);
            mesh.add_triangle(base + j, next + j, next + i);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Soft drop shadow / glow behind a card.
pub fn glow(painter: &egui::Painter, rect: Rect, radius: f32, color: Color32, blur: f32, spread: f32) {
    let shadow = epaint::Shadow {
        offset: [0, 0],
        blur: blur.clamp(0.0, 255.0) as u8,
        spread: spread.clamp(0.0, 255.0) as u8,
        color,
    };
    painter.add(shadow.as_shape(rect, CornerRadius::same(radius.clamp(0.0, 255.0) as u8)));
}

/// The haunt mascot. `t` is time in seconds (for the idle float and blink),
/// `glow_color` paints a halo behind it.
pub fn ghost(painter: &egui::Painter, center: Pos2, size: f32, body: Color32, eyes: Color32, t: f32, halo: bool) {
    let center = center + vec2(0.0, (t * 1.7).sin() * size * 0.045);
    if halo {
        for i in 0..6 {
            let k = i as f32 / 6.0;
            painter.circle_filled(center, size * (0.55 + k * 0.5), body.gamma_multiply(0.035 * (1.0 - k)));
        }
    }
    let w = size * 0.74;
    let h = size * 0.86;
    let top = center.y - h / 2.0;
    let r = w / 2.0;
    let hem_y = center.y + h / 2.0 - size * 0.1;

    // Body as one polygon: head arc + sides + wavy hem (sway with time).
    let mut pts = Vec::with_capacity(64);
    for i in 0..=24 {
        let a = PI + (i as f32 / 24.0) * PI;
        pts.push(pos2(center.x + r * a.cos(), top + r + r * a.sin()));
    }
    let waves = 3.0;
    let sway = (t * 2.4).sin() * 0.35;
    for i in 0..=30 {
        let k = i as f32 / 30.0;
        let x = center.x + r - k * w;
        let y = hem_y + (k * waves * TAU + sway).cos().abs() * size * 0.09;
        pts.push(pos2(x, y));
    }
    let mut mesh = Mesh::default();
    let c = pos2(center.x, top + r * 1.2);
    mesh.colored_vertex(c, body);
    for p in &pts {
        mesh.colored_vertex(*p, body);
    }
    let n = pts.len() as u32;
    for i in 1..=n {
        let j = if i == n { 1 } else { i + 1 };
        mesh.add_triangle(0, i, j);
    }
    painter.add(Shape::mesh(mesh));

    // Eyes, with a blink every few seconds.
    let blink = ((t % 4.2) - 4.05).abs() < 0.08;
    let ey = top + r * 1.02;
    let er = size * 0.072;
    let dx = w * 0.19;
    for side in [-1.0, 1.0] {
        let p = pos2(center.x + side * dx, ey);
        if blink {
            painter.line_segment([p - vec2(er, 0.0), p + vec2(er, 0.0)], Stroke::new(size * 0.035, eyes));
        } else {
            painter.add(Shape::ellipse_filled(p, Vec2::new(er * 0.85, er * 1.15), eyes));
        }
    }
}

// ---------------------------------------------------------------- alignment

/// Ink bounds of a laid-out galley (the pixels its glyphs actually cover), relative to its origin.
fn ink_bounds(galley: &epaint::Galley) -> Option<Rect> {
    let mut r = Rect::NOTHING;
    for row in &galley.rows {
        for g in &row.glyphs {
            if g.uv_rect.size.x <= 0.0 || g.uv_rect.size.y <= 0.0 {
                continue;
            }
            let min = row.pos + g.pos.to_vec2() + g.uv_rect.offset;
            r = r.union(Rect::from_min_size(min, g.uv_rect.size));
        }
    }
    r.is_positive().then_some(r)
}

/// Paints an icon glyph with its *ink* centred on `center`. Nerd Font icons sit at different
/// heights inside the line box, so centring the line (what `Align2::CENTER_CENTER` does) leaves
/// them a pixel or two off; this puts every icon on the same optical centre.
pub fn glyph(painter: &egui::Painter, center: Pos2, icon: &str, font: egui::FontId, color: Color32) -> Rect {
    let galley = painter.layout_no_wrap(icon.to_string(), font, color);
    let ink = ink_bounds(&galley).unwrap_or(Rect::from_min_size(Pos2::ZERO, galley.size()));
    let origin = center - ink.center().to_vec2();
    let rect = ink.translate(origin.to_vec2());
    painter.galley(origin, galley, color);
    rect
}

/// Vertical shift that puts a line of text on its optical centre: halfway between the cap
/// height and the baseline instead of the middle of the line box (which includes descender and
/// line gap, so `*_CENTER` text sits visibly off next to icons). Add it to the y of any
/// `Align2::*_CENTER` text.
pub fn text_dy(painter: &egui::Painter, font: &egui::FontId) -> f32 {
    let galley = painter.layout_no_wrap("H".into(), font.clone(), Color32::WHITE);
    let Some(row) = galley.rows.first() else { return 0.0 };
    let Some(g) = row.glyphs.first() else { return 0.0 };
    let cap_top = row.pos.y + g.pos.y + g.uv_rect.offset.y;
    let baseline = row.pos.y + g.pos.y;
    let optical = (cap_top + baseline) / 2.0;
    galley.size().y / 2.0 - optical
}

/// Text at `pos` with `align`, on the optical centre when the alignment is vertically centred.
pub fn text(
    painter: &egui::Painter,
    pos: Pos2,
    align: egui::Align2,
    text: impl ToString,
    font: egui::FontId,
    color: Color32,
) -> Rect {
    let dy = if align.y() == egui::Align::Center { text_dy(painter, &font) } else { 0.0 };
    painter.text(pos + vec2(0.0, dy), align, text, font, color)
}

// ---------------------------------------------------------------- plates

/// Fills a rounded rect with a vertical gradient: `color(t)` for t = 0 (top) .. 1 (bottom).
/// Built from horizontal slices, so the gradient is exact and the corners stay round.
pub fn vgradient(painter: &egui::Painter, rect: Rect, radius: f32, color: impl Fn(f32) -> Color32) {
    painter.add(Shape::mesh(vgradient_mesh(rect, radius, color)));
}

/// The mesh behind [`vgradient`], for painting into a reserved shape slot.
pub fn vgradient_mesh(rect: Rect, radius: f32, color: impl Fn(f32) -> Color32) -> Mesh {
    let r = radius.max(0.0).min(rect.width() / 2.0).min(rect.height() / 2.0);
    let h = rect.height();
    let mut mesh = Mesh::default();
    if h <= 0.0 || rect.width() <= 0.0 {
        return mesh;
    }
    // Dense samples inside the corner bands, sparse in between.
    let mut ys: Vec<f32> = Vec::with_capacity(40);
    let steps = 12;
    for i in 0..=steps {
        ys.push(rect.min.y + r * i as f32 / steps as f32);
    }
    for i in 1..8 {
        ys.push(rect.min.y + r + (h - 2.0 * r) * i as f32 / 8.0);
    }
    for i in 0..=steps {
        ys.push(rect.max.y - r + r * i as f32 / steps as f32);
    }
    let inset = |y: f32| -> f32 {
        let d = if y < rect.min.y + r {
            rect.min.y + r - y
        } else if y > rect.max.y - r {
            y - (rect.max.y - r)
        } else {
            0.0
        }
        .min(r);
        r - (r * r - d * d).max(0.0).sqrt()
    };
    for y in &ys {
        let c = color(((y - rect.min.y) / h).clamp(0.0, 1.0));
        let dx = inset(*y);
        mesh.colored_vertex(pos2(rect.min.x + dx, *y), c);
        mesh.colored_vertex(pos2(rect.max.x - dx, *y), c);
    }
    for i in 0..ys.len() as u32 - 1 {
        let a = i * 2;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    mesh
}

/// A rounded border (inside `rect`) whose colour runs from `top` to `bottom`.
pub fn vgradient_stroke(painter: &egui::Painter, rect: Rect, radius: f32, width: f32, top: Color32, bottom: Color32) {
    let feather = 0.7;
    let rings = [
        (rounded_path(rect.expand(feather * 0.5), radius + feather * 0.5), false),
        (rounded_path(rect, radius), true),
        (rounded_path(rect.shrink(width), (radius - width).max(0.0)), true),
        (rounded_path(rect.shrink(width + feather), (radius - width - feather).max(0.0)), false),
    ];
    let h = rect.height().max(1.0);
    let n = rings[0].0.len() as u32;
    let mut mesh = Mesh::default();
    for (path, solid) in &rings {
        for p in path {
            let c = if *solid { mix_alpha(top, bottom, ((p.y - rect.min.y) / h).clamp(0.0, 1.0)) } else { Color32::TRANSPARENT };
            mesh.colored_vertex(*p, c);
        }
    }
    for ring in 0..3u32 {
        let base = ring * n;
        let next = base + n;
        for i in 0..n {
            let j = (i + 1) % n;
            mesh.add_triangle(base + i, base + j, next + i);
            mesh.add_triangle(base + j, next + j, next + i);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Blend of two premultiplied colours, alpha included.
pub fn mix_alpha(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// The Ghostly plate: translucent night ink, a faint wash of sky light from the top, and a
/// hairline whose top edge catches that light. `lit` (0..1, hover/focus) warms the edge
/// towards the accent.
pub fn plate(painter: &egui::Painter, rect: Rect, radius: f32, fill: Color32, accent: Color32, lit: f32) {
    let rr = CornerRadius::same(radius.clamp(0.0, 255.0) as u8);
    painter.rect_filled(rect, rr, fill);
    let wash = (90.0 / rect.height().max(1.0)).min(1.0);
    vgradient(painter, rect, radius, |t| {
        let k = (1.0 - t / wash).max(0.0);
        accent.gamma_multiply((0.06 + 0.05 * lit) * k * k)
    });
    let top = mix_alpha(Color32::from_white_alpha(18), accent.gamma_multiply(0.6), lit);
    let bottom = Color32::from_white_alpha((9.0 + 6.0 * lit) as u8);
    vgradient_stroke(painter, rect, radius, 1.0, top, bottom);
}

/// One soft shadow under a plate, tinted like the night.
pub fn shadow(painter: &egui::Painter, rect: Rect, radius: f32, strength: f32) {
    let s = epaint::Shadow {
        offset: [0, 10],
        blur: 34,
        spread: 0,
        color: Color32::from_rgba_premultiplied(2, 1, 6, (120.0 * strength).clamp(0.0, 255.0) as u8),
    };
    painter.add(s.as_shape(rect, CornerRadius::same(radius.clamp(0.0, 255.0) as u8)));
}

/// A key cap ("Ctrl", "K"): mono text in a small hairline box, vertically centred on
/// `left_center`. Returns its width.
pub fn kbd(painter: &egui::Painter, left_center: Pos2, key: &str, fg: Color32, line: Color32) -> f32 {
    let font = egui::FontId::monospace(10.5);
    let g = painter.layout_no_wrap(key.to_string(), font.clone(), fg);
    let w = (g.size().x + 12.0).max(20.0).round();
    let r = Rect::from_min_size(pos2(left_center.x, left_center.y - 10.0), vec2(w, 20.0));
    painter.rect(r, CornerRadius::same(6), Color32::from_white_alpha(5), Stroke::new(1.0, line), StrokeKind::Inside);
    text(painter, r.center(), egui::Align2::CENTER_CENTER, key, font, fg);
    w
}
