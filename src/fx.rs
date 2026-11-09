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

/// Translucent card with a hairline border: the "glass" look of HyDE panels.
pub fn glass(painter: &egui::Painter, rect: Rect, radius: f32, fill: Color32, line: Color32) {
    painter.rect(
        rect,
        CornerRadius::same(radius.clamp(0.0, 255.0) as u8),
        fill,
        Stroke::new(1.0, line),
        StrokeKind::Inside,
    );
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
