//! Fonts. The terminal uses CaskaydiaCove Nerd Font Mono (the font HyDE ships for
//! kitty), embedded in the binary so prompts with Nerd glyphs render out of the box.
//! The UI uses Segoe UI from Windows, with the Nerd font as icon fallback.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};

static MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/CaskaydiaCoveNerdFontMono-Regular.ttf");
static MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/CaskaydiaCoveNerdFontMono-Bold.ttf");
static MONO_ITALIC: &[u8] = include_bytes!("../assets/fonts/CaskaydiaCoveNerdFontMono-Italic.ttf");
static UI_REGULAR: &[u8] = include_bytes!("../assets/fonts/Onest-regular.ttf");
static UI_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Onest-medium.ttf");
static UI_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Onest-semibold.ttf");
static UI_BOLD: &[u8] = include_bytes!("../assets/fonts/Onest-bold.ttf");

/// Icon codepoints (Nerd Fonts).
#[allow(dead_code)]
pub mod icon {
    pub const GEAR: &str = "\u{f013}";
    pub const PLUS: &str = "\u{f067}";
    pub const SEARCH: &str = "\u{f002}";
    pub const CLOSE: &str = "\u{eab8}";
    pub const MINIMIZE: &str = "\u{eaba}";
    pub const MAXIMIZE: &str = "\u{eab9}";
    pub const RESTORE: &str = "\u{eabb}";
    pub const TERMINAL: &str = "\u{e795}";
    pub const BOLT: &str = "\u{f0e7}";
    pub const KEY: &str = "\u{f084}";
    pub const LOCK: &str = "\u{f023}";
    pub const PENCIL: &str = "\u{f040}";
    pub const TRASH: &str = "\u{f1f8}";
    pub const COPY: &str = "\u{f0c5}";
    pub const PLUG: &str = "\u{f1e6}";
    pub const REFRESH: &str = "\u{f021}";
    pub const BRUSH: &str = "\u{f1fc}";
    pub const SHIELD: &str = "\u{f132}";
    pub const SLIDERS: &str = "\u{f1de}";
    pub const ARROW: &str = "\u{f061}";
    pub const XMARK: &str = "\u{f00d}";
    pub const GHOST: &str = "\u{f02a0}";
    pub const SERVER: &str = "\u{f233}";
    pub const FOLDER: &str = "\u{f07b}";
    pub const ROUTE: &str = "\u{f0e8}";
    pub const GLOBE: &str = "\u{f0ac}";
    pub const UP: &str = "\u{f062}";
    pub const EYE: &str = "\u{f06e}";
    pub const EYE_OFF: &str = "\u{f070}";
    pub const FILE: &str = "\u{f15b}";
    pub const LINK: &str = "\u{f0c1}";
    pub const DOWNLOAD: &str = "\u{f019}";
    pub const UPLOAD: &str = "\u{f093}";
    pub const DRIVE: &str = "\u{f0a0}";
    pub const DESKTOP: &str = "\u{f108}";
    pub const FOLDER_PLUS: &str = "\u{f0257}";
    pub const CARET_UP: &str = "\u{f0d8}";
    pub const CARET_DOWN: &str = "\u{f0d7}";
    pub const CHECK: &str = "\u{f00c}";
    pub const TUNNEL: &str = "\u{f0ec}";
    pub const ARROW_LEFT: &str = "\u{f060}";
    pub const ROBOT: &str = "\u{f06a9}";
}

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("Fonts")
}

/// Per-user installs land in %LOCALAPPDATA%\Microsoft\Windows\Fonts, machine-wide in %WINDIR%\Fonts.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![fonts_dir()];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
    }
    dirs
}

fn read_system(names: &[&str]) -> Option<Vec<u8>> {
    font_dirs().iter().find_map(|dir| names.iter().find_map(|n| std::fs::read(dir.join(n)).ok()))
}

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let egui_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let egui_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut add = |name: &str, data: FontData| {
        defs.font_data.insert(name.to_string(), Arc::new(data));
    };
    add("mono", FontData::from_static(MONO_REGULAR));
    add("mono-bold", FontData::from_static(MONO_BOLD));
    add("mono-italic", FontData::from_static(MONO_ITALIC));

    // Onest (the Ghostly brand face, OFL) for the interface; Segoe UI only as a glyph fallback.
    add("ui", FontData::from_static(UI_REGULAR));
    add("ui-medium", FontData::from_static(UI_MEDIUM));
    add("ui-semibold", FontData::from_static(UI_SEMIBOLD));
    add("ui-display", FontData::from_static(UI_BOLD));
    // SF Pro Display can't be shipped (Apple licence), but when it is installed it wins over Onest.
    let sf: Vec<(&str, bool)> = [
        ("sf", &["SF-Pro-Display-Regular.otf", "SFProDisplay-Regular.otf", "SF-Pro-Display-Regular.ttf"][..]),
        ("sf-medium", &["SF-Pro-Display-Medium.otf", "SFProDisplay-Medium.otf", "SF-Pro-Display-Medium.ttf"][..]),
        ("sf-semibold", &["SF-Pro-Display-Semibold.otf", "SFProDisplay-Semibold.otf", "SF-Pro-Display-Semibold.ttf"][..]),
        ("sf-display", &["SF-Pro-Display-Bold.otf", "SFProDisplay-Bold.otf", "SF-Pro-Display-Bold.ttf"][..]),
    ]
    .into_iter()
    .map(|(name, files)| match read_system(files) {
        Some(d) => {
            add(name, FontData::from_owned(d));
            (name, true)
        }
        None => (name, false),
    })
    .collect();
    let has_sf = |name: &str| sf.iter().any(|(n, ok)| *n == name && *ok);
    let has_segoe = if let Some(d) = read_system(&["segoeui.ttf"]) {
        add("segoe", FontData::from_owned(d));
        true
    } else {
        false
    };
    // Chinese, Japanese and Korean come from Windows' own UI fonts. The same Han character is
    // drawn differently in Chinese and Japanese, so the interface language picks which goes first.
    let cjk_files: [(&str, &[&str]); 3] = [
        ("cjk-zh", &["msyh.ttc", "msyh.ttf"]),
        ("cjk-ja", &["YuGothM.ttc", "YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"]),
        ("cjk-ko", &["malgun.ttf"]),
    ];
    let mut cjk: Vec<&str> = Vec::new();
    for (name, files) in cjk_files {
        if let Some(d) = read_system(files) {
            add(name, FontData::from_owned(d));
            cjk.push(name);
        }
    }
    let first = match crate::i18n::current() {
        crate::i18n::Lang::Ja => "cjk-ja",
        crate::i18n::Lang::Ko => "cjk-ko",
        _ => "cjk-zh",
    };
    cjk.sort_by_key(|n| *n != first);
    let has_symbols = if let Some(d) = read_system(&["seguisym.ttf"]) {
        add("symbols", FontData::from_owned(d));
        true
    } else {
        false
    };

    let chain = |first: &[&str], rest: &[String]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().map(|s| s.to_string()).collect();
        if has_symbols {
            v.push("symbols".into());
        }
        v.extend(rest.iter().cloned());
        v
    };

    let mono_chain = |first: &[&'static str]| -> Vec<&'static str> {
        let mut v = first.to_vec();
        v.extend(cjk.iter().copied());
        v
    };
    defs.families.insert(FontFamily::Monospace, chain(&mono_chain(&["mono"]), &egui_mono));
    defs.families.insert(FontFamily::Name("mono-bold".into()), chain(&mono_chain(&["mono-bold", "mono"]), &egui_mono));
    defs.families.insert(FontFamily::Name("mono-italic".into()), chain(&mono_chain(&["mono-italic", "mono"]), &egui_mono));

    let face = |sf: &'static str, onest: &'static str| -> Vec<&'static str> {
        let mut v = Vec::new();
        if has_sf(sf) {
            v.push(sf);
        }
        v.push(onest);
        if has_segoe {
            v.push("segoe");
        }
        v.extend(cjk.iter().copied());
        v.push("mono");
        v
    };
    defs.families.insert(FontFamily::Proportional, chain(&face("sf", "ui"), &egui_prop));
    defs.families.insert(FontFamily::Name("medium".into()), chain(&face("sf-medium", "ui-medium"), &egui_prop));
    defs.families.insert(FontFamily::Name("bold".into()), chain(&face("sf-semibold", "ui-semibold"), &egui_prop));
    defs.families.insert(FontFamily::Name("display".into()), chain(&face("sf-display", "ui-display"), &egui_prop));

    ctx.set_fonts(defs);
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

pub fn ui(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn mono_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("mono-bold".into()))
}

pub fn mono_italic(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("mono-italic".into()))
}
