//! Easing curves and small animation helpers.
//!
//! The bezier curves are the ones HyDE's Hyprland config uses for windows
//! (`wind`, `winIn`, `winOut`), so motion here feels like the desktop it imitates.

use eframe::egui::{self, Id};

#[derive(Clone, Copy)]
pub struct Bezier {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Bezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn sample(a: f32, b: f32, t: f32) -> f32 {
        // Cubic bezier with P0 = 0, P3 = 1.
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    }

    fn slope(a: f32, b: f32, t: f32) -> f32 {
        let u = 1.0 - t;
        3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
    }

    /// y for a given x (time) in 0..=1, solved with a few Newton steps plus bisection fallback.
    pub fn eval(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x == 0.0 || x == 1.0 {
            return x;
        }
        let mut t = x;
        for _ in 0..6 {
            let err = Self::sample(self.x1, self.x2, t) - x;
            if err.abs() < 1e-4 {
                return Self::sample(self.y1, self.y2, t);
            }
            let d = Self::slope(self.x1, self.x2, t);
            if d.abs() < 1e-5 {
                break;
            }
            t = (t - err / d).clamp(0.0, 1.0);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..20 {
            t = (lo + hi) / 2.0;
            if Self::sample(self.x1, self.x2, t) < x {
                lo = t;
            } else {
                hi = t;
            }
        }
        Self::sample(self.y1, self.y2, t)
    }
}

/// HyDE `wind`: fast start, slight overshoot. Used for movement and workspace slides.
pub const WIND: Bezier = Bezier::new(0.05, 0.9, 0.1, 1.05);
/// HyDE `winIn`: springy pop-in for new windows.
pub const WIN_IN: Bezier = Bezier::new(0.1, 1.1, 0.1, 1.1);
/// HyDE `winOut`: anticipation before leaving.
#[allow(dead_code)]
pub const WIN_OUT: Bezier = Bezier::new(0.3, -0.3, 0.0, 1.0);
/// Smooth, no overshoot: hover states and fades.
pub const SMOOTH: Bezier = Bezier::new(0.25, 0.1, 0.25, 1.0);

pub fn wind(t: f32) -> f32 {
    WIND.eval(t)
}
#[allow(dead_code)]
pub fn win_in(t: f32) -> f32 {
    WIN_IN.eval(t)
}
pub fn smooth(t: f32) -> f32 {
    SMOOTH.eval(t)
}

/// Animated 0..1 for a boolean, eased; returns the eased value.
pub fn toggle(ctx: &egui::Context, id: Id, on: bool, secs: f32) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, secs, smooth)
}

/// Progress of a one-shot animation that started at `start` (seconds, from `i.time`).
/// Requests repaints while it runs.
pub fn since(ctx: &egui::Context, start: f64, secs: f32) -> f32 {
    let now = ctx.input(|i| i.time);
    let t = ((now - start) as f32 / secs).clamp(0.0, 1.0);
    if t < 1.0 {
        ctx.request_repaint();
    }
    t
}

/// Frame-rate independent exponential approach: moves `current` towards `target`
/// with a time constant `tau` seconds.
pub fn approach(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    if tau <= 0.0 {
        return target;
    }
    let k = 1.0 - (-dt / tau).exp();
    current + (target - current) * k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_monotone_linear() {
        let lin = Bezier::new(0.0, 0.0, 1.0, 1.0);
        for i in 0..=10 {
            let x = i as f32 / 10.0;
            assert!((lin.eval(x) - x).abs() < 1e-3);
        }
        assert_eq!(WIND.eval(0.0), 0.0);
        assert_eq!(WIND.eval(1.0), 1.0);
        // winIn overshoots past 1 before settling.
        assert!((0..100).any(|i| WIN_IN.eval(i as f32 / 100.0) > 1.0));
        // winOut dips below 0 at the start.
        assert!((1..30).any(|i| WIN_OUT.eval(i as f32 / 100.0) < 0.0));
    }
}
