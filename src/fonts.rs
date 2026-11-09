//! Fonts. The terminal uses CaskaydiaCove Nerd Font Mono (the font HyDE ships for
//! kitty), embedded in the binary so prompts with Nerd glyphs render out of the box.
//! The UI uses Segoe UI from Windows, with the Nerd font as icon fallback.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};

static MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/CaskaydiaCoveNerdFontMono-Regular.ttf");
static MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/CaskaydiaCoveNerdFontMono-Bold.ttf");
static MONO_ITALIC: &[u8] = include_bytes!("../assets/fonts/CaskaydiaCoveNerdFontMono-Italic.ttf");

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
}

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("Fonts")
}

fn read_system(names: &[&str]) -> Option<Vec<u8>> {
    let dir = fonts_dir();
    names.iter().find_map(|n| std::fs::read(dir.join(n)).ok())
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

    let has_ui = if let Some(d) = read_system(&["segoeui.ttf"]) {
        add("ui", FontData::from_owned(d));
        true
    } else {
        false
    };
    let has_semibold = if let Some(d) = read_system(&["seguisb.ttf", "segoeuib.ttf"]) {
        add("ui-semibold", FontData::from_owned(d));
        true
    } else {
        false
    };
    let has_display = if let Some(d) = read_system(&["segoeuib.ttf"]) {
        add("ui-display", FontData::from_owned(d));
        true
    } else {
        false
    };
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

    defs.families.insert(FontFamily::Monospace, chain(&["mono"], &egui_mono));
    defs.families.insert(FontFamily::Name("mono-bold".into()), chain(&["mono-bold", "mono"], &egui_mono));
    defs.families.insert(FontFamily::Name("mono-italic".into()), chain(&["mono-italic", "mono"], &egui_mono));

    let ui: Vec<&str> = if has_ui { vec!["ui", "mono"] } else { vec!["mono"] };
    defs.families.insert(FontFamily::Proportional, chain(&ui, &egui_prop));
    let semi: Vec<&str> = if has_semibold { vec!["ui-semibold", "mono"] } else { ui.clone() };
    defs.families.insert(FontFamily::Name("bold".into()), chain(&semi, &egui_prop));
    let display: Vec<&str> = if has_display { vec!["ui-display", "mono"] } else { semi.clone() };
    defs.families.insert(FontFamily::Name("display".into()), chain(&display, &egui_prop));

    ctx.set_fonts(defs);
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
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
