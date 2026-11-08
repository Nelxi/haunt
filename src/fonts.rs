//! Font setup. Uses fonts that ship with Windows (Cascadia Mono, Segoe UI) so the
//! binary stays small; falls back to egui's bundled fonts when they are missing.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("Fonts")
}

fn read_first(names: &[&str]) -> Option<Vec<u8>> {
    let dir = fonts_dir();
    names.iter().find_map(|n| std::fs::read(dir.join(n)).ok())
}

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();

    if let Some(data) = read_first(&["CascadiaMono.ttf", "CascadiaCode.ttf", "consola.ttf"]) {
        defs.font_data.insert("term".into(), Arc::new(FontData::from_owned(data)));
        if let Some(mono) = defs.families.get_mut(&FontFamily::Monospace) {
            mono.insert(0, "term".into());
        }
    } else {
        log::warn!("no Windows monospace font found, using the bundled one");
    }

    if let Some(data) = read_first(&["segoeui.ttf"]) {
        defs.font_data.insert("ui".into(), Arc::new(FontData::from_owned(data)));
        if let Some(prop) = defs.families.get_mut(&FontFamily::Proportional) {
            prop.insert(0, "ui".into());
        }
    }
    if let Some(data) = read_first(&["segoeuib.ttf"]) {
        defs.font_data.insert("ui-bold".into(), Arc::new(FontData::from_owned(data)));
        let mut family = vec!["ui-bold".to_string()];
        family.extend(defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
        defs.families.insert(FontFamily::Name("bold".into()), family);
    } else {
        let prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
        defs.families.insert(FontFamily::Name("bold".into()), prop);
    }

    // Symbols (arrows, box drawing extras, dingbats) as a last-resort fallback.
    if let Some(data) = read_first(&["seguisym.ttf"]) {
        defs.font_data.insert("symbols".into(), Arc::new(FontData::from_owned(data)));
        for fam in [FontFamily::Monospace, FontFamily::Proportional] {
            if let Some(list) = defs.families.get_mut(&fam) {
                list.push("symbols".into());
            }
        }
    }

    ctx.set_fonts(defs);
}

pub fn bold(size: f32) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Name("bold".into()))
}
