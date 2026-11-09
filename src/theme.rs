//! Color themes: terminal palette plus the UI chrome colors derived from it.

use eframe::egui::Color32;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
    /// ANSI 0..16.
    pub ansi: [Color32; 16],
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,

    /// Darkest layer (window background, gaps between panes).
    pub crust: Color32,
    /// Sidebar / tab bar.
    pub mantle: Color32,
    /// Raised surfaces: inputs, hovered rows.
    pub surface: Color32,
    /// Borders and inactive outlines.
    pub overlay: Color32,
    pub text: Color32,
    pub subtext: Color32,
    pub accent: Color32,
    /// Secondary accent, reserved for the gradient pane border.
    #[allow(dead_code)]
    pub accent2: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

macro_rules! ansi {
    ($($c:expr),* $(,)?) => { [$(hex($c)),*] };
}

pub fn all() -> &'static [Theme] {
    use std::sync::OnceLock;
    static THEMES: OnceLock<Vec<Theme>> = OnceLock::new();
    THEMES.get_or_init(|| {
        vec![
            Theme {
                name: "Catppuccin Mocha",
                ansi: ansi![
                    0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
                    0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
                ],
                fg: hex(0xcdd6f4),
                bg: hex(0x1e1e2e),
                cursor: hex(0xf5e0dc),
                selection: hex(0x585b70),
                crust: hex(0x11111b),
                mantle: hex(0x181825),
                surface: hex(0x313244),
                overlay: hex(0x45475a),
                text: hex(0xcdd6f4),
                subtext: hex(0x9399b2),
                accent: hex(0xcba6f7),
                accent2: hex(0x89b4fa),
                ok: hex(0xa6e3a1),
                warn: hex(0xf9e2af),
                err: hex(0xf38ba8),
            },
            Theme {
                name: "Graphite Mono",
                ansi: ansi![
                    0x262626, 0xef6b7b, 0xa8cc8c, 0xdbab79, 0x71beeb, 0xd290e4, 0x66c2cd, 0xd4d4d4,
                    0x5a5a5a, 0xf48795, 0xbddba4, 0xe6bd93, 0x8cd0f7, 0xdda4ed, 0x7fd4df, 0xffffff,
                ],
                fg: hex(0xd4d4d4),
                bg: hex(0x141414),
                cursor: hex(0xf0f0f0),
                selection: hex(0x3c3c3c),
                crust: hex(0x0a0a0a),
                mantle: hex(0x101010),
                surface: hex(0x222222),
                overlay: hex(0x343434),
                text: hex(0xe4e4e4),
                subtext: hex(0x8a8a8a),
                accent: hex(0xf0f0f0),
                accent2: hex(0x7a7a7a),
                ok: hex(0xa8cc8c),
                warn: hex(0xdbab79),
                err: hex(0xef6b7b),
            },
            Theme {
                name: "Monochrome",
                ansi: ansi![
                    0x262626, 0xb0b0b0, 0xd0d0d0, 0xe0e0e0, 0x9a9a9a, 0xc4c4c4, 0xaaaaaa, 0xd4d4d4,
                    0x5a5a5a, 0xcfcfcf, 0xe8e8e8, 0xf2f2f2, 0xb8b8b8, 0xdedede, 0xc8c8c8, 0xffffff,
                ],
                fg: hex(0xd4d4d4),
                bg: hex(0x141414),
                cursor: hex(0xf0f0f0),
                selection: hex(0x3c3c3c),
                crust: hex(0x0a0a0a),
                mantle: hex(0x101010),
                surface: hex(0x222222),
                overlay: hex(0x343434),
                text: hex(0xe4e4e4),
                subtext: hex(0x8a8a8a),
                accent: hex(0xf0f0f0),
                accent2: hex(0x7a7a7a),
                ok: hex(0xd0d0d0),
                warn: hex(0xbdbdbd),
                err: hex(0x9e9e9e),
            },
            Theme {
                name: "Tokyo Night",
                ansi: ansi![
                    0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
                    0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
                ],
                fg: hex(0xc0caf5),
                bg: hex(0x1a1b26),
                cursor: hex(0xc0caf5),
                selection: hex(0x33467c),
                crust: hex(0x101014),
                mantle: hex(0x16161e),
                surface: hex(0x24283b),
                overlay: hex(0x414868),
                text: hex(0xc0caf5),
                subtext: hex(0x787c99),
                accent: hex(0x7aa2f7),
                accent2: hex(0xbb9af7),
                ok: hex(0x9ece6a),
                warn: hex(0xe0af68),
                err: hex(0xf7768e),
            },
            Theme {
                name: "Rosé Pine",
                ansi: ansi![
                    0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
                    0x6e6a86, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
                ],
                fg: hex(0xe0def4),
                bg: hex(0x191724),
                cursor: hex(0xe0def4),
                selection: hex(0x403d52),
                crust: hex(0x110f19),
                mantle: hex(0x1f1d2e),
                surface: hex(0x26233a),
                overlay: hex(0x403d52),
                text: hex(0xe0def4),
                subtext: hex(0x908caa),
                accent: hex(0xebbcba),
                accent2: hex(0xc4a7e7),
                ok: hex(0x9ccfd8),
                warn: hex(0xf6c177),
                err: hex(0xeb6f92),
            },
            Theme {
                name: "Gruvbox Dark",
                ansi: ansi![
                    0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
                    0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
                ],
                fg: hex(0xebdbb2),
                bg: hex(0x282828),
                cursor: hex(0xebdbb2),
                selection: hex(0x504945),
                crust: hex(0x1a1a1a),
                mantle: hex(0x1d2021),
                surface: hex(0x3c3836),
                overlay: hex(0x504945),
                text: hex(0xebdbb2),
                subtext: hex(0xa89984),
                accent: hex(0xfe8019),
                accent2: hex(0xfabd2f),
                ok: hex(0xb8bb26),
                warn: hex(0xfabd2f),
                err: hex(0xfb4934),
            },
            Theme {
                name: "Nord",
                ansi: ansi![
                    0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
                    0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
                ],
                fg: hex(0xd8dee9),
                bg: hex(0x2e3440),
                cursor: hex(0xd8dee9),
                selection: hex(0x434c5e),
                crust: hex(0x21252d),
                mantle: hex(0x272c36),
                surface: hex(0x3b4252),
                overlay: hex(0x4c566a),
                text: hex(0xeceff4),
                subtext: hex(0x9aa5b8),
                accent: hex(0x88c0d0),
                accent2: hex(0x81a1c1),
                ok: hex(0xa3be8c),
                warn: hex(0xebcb8b),
                err: hex(0xbf616a),
            },
        ]
    })
}

pub fn by_name(name: &str) -> &'static Theme {
    let all = all();
    if name == "Graphite" {
        if let Some(t) = all.iter().find(|t| t.name == "Graphite Mono") {
            return t;
        }
    }
    all.iter().find(|t| t.name == name).unwrap_or(&all[0])
}

impl Theme {
    /// Full xterm-256 palette: 16 theme colors, 6x6x6 cube, 24 grays.
    pub fn palette256(&self) -> [Color32; 256] {
        let mut p = [Color32::BLACK; 256];
        p[..16].copy_from_slice(&self.ansi);
        let level = |v: usize| if v == 0 { 0u8 } else { (55 + v * 40) as u8 };
        for i in 0..216 {
            let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
            p[16 + i] = Color32::from_rgb(level(r), level(g), level(b));
        }
        for i in 0..24 {
            let v = (8 + i * 10) as u8;
            p[232 + i] = Color32::from_rgb(v, v, v);
        }
        p
    }
}

/// Linear blend of two colors, `t` in 0..=1.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_themes_present() {
        let themes = all();
        assert!(themes.len() >= 6);
        let names: Vec<&str> = themes.iter().map(|t| t.name).collect();
        assert!(names.contains(&"Catppuccin Mocha"));
        assert!(names.contains(&"Graphite Mono"));
        assert!(names.contains(&"Tokyo Night"));
        assert!(names.contains(&"Rosé Pine"));
        assert!(names.contains(&"Gruvbox Dark"));
        assert!(names.contains(&"Nord"));
        assert!(names.contains(&"Monochrome"));
    }

    #[test]
    fn test_graphite_has_colors() {
        let gm = by_name("Graphite Mono");
        // Ensure red, green, blue are distinct and have color (not all grayscale)
        let red = gm.ansi[1];
        let green = gm.ansi[2];
        let blue = gm.ansi[4];
        assert_ne!(red, green);
        assert_ne!(green, blue);
        assert!(red.r() > red.g() && red.r() > red.b(), "red should have dominant red channel");
        assert!(green.g() > green.r() && green.g() > green.b(), "green should have dominant green channel");
        assert!(blue.b() > blue.r(), "blue should have dominant blue channel");
    }

    #[test]
    fn test_by_name_aliases_and_fallback() {
        assert_eq!(by_name("Catppuccin Mocha").name, "Catppuccin Mocha");
        assert_eq!(by_name("Graphite Mono").name, "Graphite Mono");
        assert_eq!(by_name("Graphite").name, "Graphite Mono"); // alias
        assert_eq!(by_name("NonExistentTheme").name, "Catppuccin Mocha"); // fallback to default
    }

    #[test]
    fn test_palette256() {
        let t = by_name("Catppuccin Mocha");
        let p = t.palette256();
        assert_eq!(p.len(), 256);
        // First 16 match theme ansi
        for i in 0..16 {
            assert_eq!(p[i], t.ansi[i]);
        }
        // Cube starts at 16, grayscale starts at 232
        assert_eq!(p[16], Color32::from_rgb(0, 0, 0));
        assert_eq!(p[231], Color32::from_rgb(255, 255, 255));
        // Grayscale ramp ends near 238
        assert_eq!(p[232], Color32::from_rgb(8, 8, 8));
        assert_eq!(p[255], Color32::from_rgb(238, 238, 238));
    }

    #[test]
    fn test_mix() {
        let black = Color32::from_rgb(0, 0, 0);
        let white = Color32::from_rgb(200, 200, 200);
        assert_eq!(mix(black, white, 0.0), black);
        assert_eq!(mix(black, white, 1.0), white);
        assert_eq!(mix(black, white, 0.5), Color32::from_rgb(100, 100, 100));
    }
}

