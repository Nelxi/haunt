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
