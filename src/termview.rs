//! Terminal widget: draws the alacritty grid with the egui painter and turns
//! keyboard/mouse input into the byte sequences an xterm-compatible host expects.

use std::f32::consts::{PI, TAU};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Event, EventFilter, FontId, Key, Mesh, Modifiers,
    MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2,
};

use crate::anim;
use crate::config::Settings;
use crate::fonts;
use crate::fx;
use crate::session::{GridSize, Session, Status};
use crate::theme::{Theme, mix};
use crate::veil;

/// Per-tab view state that survives between frames.
pub struct ViewState {
    scroll_acc: f32,
    blink_epoch: Instant,
    /// Mouse button currently held down and reported to the remote app.
    reported_button: Option<u8>,
    last_report_cell: Option<(usize, usize)>,
    selecting: bool,
    /// Animated cursor quad (tl, tr, br, bl) and the rect it is heading to.
    cursor_corners: Option<[Pos2; 4]>,
    cursor_target: Rect,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            scroll_acc: 0.0,
            blink_epoch: Instant::now(),
            reported_button: None,
            last_report_cell: None,
            selecting: false,
            cursor_corners: None,
            cursor_target: Rect::NOTHING,
        }
    }
}

pub enum Action {
    None,
    Reconnect,
}

pub struct Look<'a> {
    pub theme: &'a Theme,
    pub palette: &'a [Color32; 256],
    pub settings: &'a Settings,
    /// Seconds since start, drives the border rotation.
    pub time: f32,
    /// Whether the OS window has focus (animations pause otherwise).
    pub window_focused: bool,
    /// Fill alpha for the pane (already accounts for the layer underneath).
    pub pane_alpha: f32,
}

struct Metrics {
    font: FontId,
    font_bold: FontId,
    font_italic: FontId,
    cw: f32,
    ch: f32,
    text_dy: f32,
    origin: Pos2,
    cols: usize,
    rows: usize,
}

impl Metrics {
    fn cell_at(&self, pos: Pos2) -> (usize, usize, Side) {
        let x = ((pos.x - self.origin.x) / self.cw).max(0.0);
        let y = ((pos.y - self.origin.y) / self.ch).max(0.0);
        let col = (x as usize).min(self.cols - 1);
        let row = (y as usize).min(self.rows - 1);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (col, row, side)
    }

    fn cell_rect(&self, col: usize, row: usize, width: usize) -> Rect {
        Rect::from_min_size(
            pos2(self.origin.x + col as f32 * self.cw, self.origin.y + row as f32 * self.ch),
            vec2(self.cw * width as f32, self.ch),
        )
    }
}

pub fn show(ui: &mut egui::Ui, s: &mut Session, look: &Look<'_>, input_enabled: bool) -> Action {
    let set = look.settings;
    let theme = look.theme;
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    if input_enabled && (s.want_focus || resp.clicked() || resp.drag_started() || resp.secondary_clicked()) {
        resp.request_focus();
        s.want_focus = false;
    }
    let focused = resp.has_focus();
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }

    // Pane: a Ghostly plate. Its top edge catches the accent light when focused; the optional
    // rotating gradient border (Hyprland `borderangle`) replaces that edge when enabled.
    let painter = ui.painter_at(rect.expand(24.0));
    let radius = set.rounding.clamp(0.0, 30.0);
    let focus_t = anim::toggle(ui.ctx(), resp.id.with("focus"), focused, 0.25);
    fx::shadow(&painter, rect, radius, 0.7);
    painter.rect_filled(rect, CornerRadius::same(radius as u8), theme.bg.gamma_multiply(look.pane_alpha));
    let animate_border = set.border_animation && set.animations && look.window_focused && focused;
    if set.border_animation && focus_t > 0.01 {
        let angle = if animate_border { look.time * TAU / 3.0 } else { PI / 4.0 };
        fx::gradient_border(
            &painter,
            rect,
            radius,
            set.border_width.max(1.0),
            theme.accent.gamma_multiply(focus_t),
            theme.accent2.gamma_multiply(focus_t),
            angle,
        );
    } else {
        let top = fx::mix_alpha(Color32::from_white_alpha(16), theme.accent.gamma_multiply(0.75), focus_t);
        let bottom = fx::mix_alpha(Color32::from_white_alpha(9), theme.accent.gamma_multiply(0.12), focus_t);
        fx::vgradient_stroke(&painter, rect, radius, 1.0, top, bottom);
    }
    if animate_border {
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    let inner = rect.shrink(set.padding + set.border_width);
    let font = fonts::mono(set.font_size);
    let (cw, row_h) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let ch = (row_h * set.line_height).round().max(1.0);
    let cols = ((inner.width() / cw).floor() as usize).max(2);
    let rows = ((inner.height() / ch).floor() as usize).max(1);
    let m = Metrics {
        font,
        font_bold: fonts::mono_bold(set.font_size),
        font_italic: fonts::mono_italic(set.font_size),
        cw,
        ch,
        text_dy: ((ch - row_h) / 2.0).round(),
        origin: inner.min,
        cols,
        rows,
    };
    s.resize(GridSize { cols: cols.min(u16::MAX as usize) as u16, rows: rows.min(u16::MAX as usize) as u16 });

    let status = s.status();
    let mut action = Action::None;
    if focused && input_enabled {
        if matches!(status, Status::Closed(_)) {
            let reconnect = ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::R));
            if reconnect {
                action = Action::Reconnect;
            }
        } else {
            handle_keyboard(ui, s, set);
        }
    }
    if input_enabled {
        handle_mouse(ui, s, &resp, &m, set);
    }

    draw(ui, &painter, s, look, &m, focused);
    draw_status(ui, &painter, rect, theme, &status, resp.id);
    action
}

// ---------------------------------------------------------------- keyboard

fn handle_keyboard(ui: &mut egui::Ui, s: &mut Session, set: &Settings) {
    let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
    let mode = *s.term.lock().mode();
    let mut out: Vec<u8> = Vec::new();

    for ev in &events {
        match ev {
            Event::Text(t) => {
                // Alt+key sends ESC-prefixed text. Ctrl+Alt is AltGr on Windows: plain text.
                if mods.alt && !mods.ctrl {
                    out.push(0x1b);
                }
                out.extend_from_slice(t.as_bytes());
            }
            Event::Paste(t) => paste_into(&mut out, t, mode),
            Event::Copy => {
                if !copy_selection(ui, s) {
                    out.push(0x03); // Ctrl+C with nothing selected = interrupt
                }
            }
            Event::Cut => out.push(0x18),
            Event::Key { key, pressed: true, modifiers, .. } => {
                if modifiers.ctrl && modifiers.shift && !modifiers.alt {
                    match key {
                        Key::C => {
                            copy_selection(ui, s);
                            continue;
                        }
                        Key::V => {
                            if let Some(t) = read_clipboard() {
                                paste_into(&mut out, &t, mode);
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
                if modifiers.shift && !modifiers.ctrl && !modifiers.alt {
                    let scroll = match key {
                        Key::PageUp => Some(Scroll::PageUp),
                        Key::PageDown => Some(Scroll::PageDown),
                        Key::Home => Some(Scroll::Top),
                        Key::End => Some(Scroll::Bottom),
                        _ => None,
                    };
                    if let Some(sc) = scroll
                        && !mode.contains(TermMode::ALT_SCREEN) {
                            s.term.lock().scroll_display(sc);
                            continue;
                        }
                }
                if let Some(bytes) = key_bytes(*key, *modifiers, mode) {
                    out.extend_from_slice(&bytes);
                }
            }
            _ => {}
        }
    }

    if !out.is_empty() {
        {
            let mut term = s.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        s.view.blink_epoch = Instant::now();
        s.send(out);
    }
    let _ = set;
}

fn paste_into(out: &mut Vec<u8>, text: &str, mode: TermMode) {
    // Normalise line endings to CR, as a real keyboard would send.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // Strip any embedded end-of-paste marker so pasted text cannot break out
        // of bracketed mode and get executed.
        let clean = text.replace("\x1b[201~", "");
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
}

fn read_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn copy_selection(ui: &egui::Ui, s: &Session) -> bool {
    let text = s.term.lock().selection_to_string();
    match text {
        Some(t) if !t.is_empty() => {
            ui.ctx().copy_text(t);
            true
        }
        _ => false,
    }
}

/// xterm encoding for special keys. Returns `None` for keys that produce text
/// (those arrive separately as `Event::Text`).
fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let modcode = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let cursor_key = |c: char| -> Vec<u8> {
        if modcode > 1 {
            format!("\x1b[1;{modcode}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modcode > 1 { format!("\x1b[{n};{modcode}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let ss3 = |c: char| -> Vec<u8> {
        if modcode > 1 { format!("\x1b[1;{modcode}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
    };

    let bytes = match key {
        Key::Enter => {
            if m.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }
        }
        Key::Backspace => {
            if m.ctrl {
                // Erase the previous word (unix-word-rubout), like Windows Terminal/kitty users expect.
                vec![0x17]
            } else if m.alt {
                b"\x1b\x7f".to_vec()
            } else {
                vec![0x7f]
            }
        }
        Key::Tab => {
            if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }
        }
        Key::Escape => vec![0x1b],
        Key::ArrowUp => cursor_key('A'),
        Key::ArrowDown => cursor_key('B'),
        Key::ArrowRight => cursor_key('C'),
        Key::ArrowLeft => cursor_key('D'),
        Key::Home => cursor_key('H'),
        Key::End => cursor_key('F'),
        Key::Insert => tilde(2),
        Key::Delete if m.ctrl && !m.alt && !m.shift => b"\x1bd".to_vec(), // kill-word forward
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => ss3('P'),
        Key::F2 => ss3('Q'),
        Key::F3 => ss3('R'),
        Key::F4 => ss3('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        // Ctrl combos that produce no text event.
        _ if m.ctrl && !m.alt => {
            let c = match key {
                Key::Space | Key::Num2 => 0x00,
                Key::OpenBracket | Key::Num3 => 0x1b,
                Key::Backslash | Key::Num4 => 0x1c,
                Key::CloseBracket | Key::Num5 => 0x1d,
                Key::Num6 => 0x1e,
                Key::Slash | Key::Minus | Key::Num7 => 0x1f,
                _ => {
                    let name = key.name();
                    let mut chars = name.chars();
                    match (chars.next(), chars.next()) {
                        (Some(ch), None) if ch.is_ascii_alphabetic() => (ch.to_ascii_uppercase() as u8) - b'@',
                        _ => return None,
                    }
                }
            };
            vec![c]
        }
        // Alt+Ctrl combos (not AltGr text) – rare; ignore.
        _ => return None,
    };
    Some(bytes)
}

// ---------------------------------------------------------------- mouse

fn handle_mouse(ui: &mut egui::Ui, s: &mut Session, resp: &egui::Response, m: &Metrics, set: &Settings) {
    let (mode, display_offset) = {
        let t = s.term.lock();
        (*t.mode(), t.grid().display_offset())
    };
    let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
    // Holding Shift always gives local selection, even when the app grabs the mouse.
    let app_mouse = mode.intersects(TermMode::MOUSE_MODE) && !mods.shift;
    let hovered = resp.hovered() || resp.dragged();

    // Wheel.
    if hovered {
        let mut lines = 0.0f32;
        for ev in &events {
            if let Event::MouseWheel { unit, delta, .. } = ev {
                lines += match unit {
                    MouseWheelUnit::Line => delta.y,
                    MouseWheelUnit::Point => delta.y / m.ch,
                    MouseWheelUnit::Page => delta.y * m.rows as f32,
                };
            }
        }
        if lines != 0.0 {
            s.view.scroll_acc += lines;
            let whole = s.view.scroll_acc.trunc() as i32;
            s.view.scroll_acc -= whole as f32;
            if whole != 0 {
                let pos = resp.hover_pos().unwrap_or(m.origin);
                let (col, row, _) = m.cell_at(pos);
                if app_mouse {
                    let button = if whole > 0 { 64 } else { 65 };
                    let mut out = Vec::new();
                    for _ in 0..whole.unsigned_abs().min(10) {
                        out.extend(mouse_report(button, col, row, true, mode, mods));
                    }
                    s.send(out);
                } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                    // less/vim without mouse mode: wheel becomes arrow keys.
                    let seq: &[u8] = match (whole > 0, mode.contains(TermMode::APP_CURSOR)) {
                        (true, true) => b"\x1bOA",
                        (true, false) => b"\x1b[A",
                        (false, true) => b"\x1bOB",
                        (false, false) => b"\x1b[B",
                    };
                    s.send(seq.repeat(whole.unsigned_abs().min(30) as usize));
                } else {
                    s.term.lock().scroll_display(Scroll::Delta(whole));
                }
            }
        }
    }

    // Mouse reporting to the remote application (htop, mc, vim with mouse=a, ...).
    if app_mouse {
        for ev in &events {
            if let Event::PointerButton { pos, button, pressed, .. } = ev {
                if !resp.rect.contains(*pos) && *pressed {
                    continue;
                }
                let code = match button {
                    PointerButton::Primary => 0,
                    PointerButton::Middle => 1,
                    PointerButton::Secondary => 2,
                    _ => continue,
                };
                let (col, row, _) = m.cell_at(*pos);
                if *pressed {
                    s.view.reported_button = Some(code);
                } else {
                    s.view.reported_button = None;
                }
                s.view.last_report_cell = Some((col, row));
                s.send(mouse_report(code, col, row, *pressed, mode, mods));
            }
        }
        let wants_motion = mode.contains(TermMode::MOUSE_MOTION)
            || (mode.contains(TermMode::MOUSE_DRAG) && s.view.reported_button.is_some());
        if wants_motion
            && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
                && resp.rect.contains(pos) {
                    let (col, row, _) = m.cell_at(pos);
                    if s.view.last_report_cell != Some((col, row)) {
                        s.view.last_report_cell = Some((col, row));
                        let code = s.view.reported_button.unwrap_or(3) + 32;
                        s.send(mouse_report(code, col, row, true, mode, mods));
                    }
                }
        return;
    }

    // Local selection.
    let point_at = |pos: Pos2| {
        let (col, row, side) = m.cell_at(pos);
        (Point::new(Line(row as i32 - display_offset as i32), Column(col)), side)
    };

    if resp.drag_started_by(PointerButton::Primary) {
        if let Some(pos) = resp.interact_pointer_pos() {
            let (p, side) = point_at(pos);
            s.term.lock().selection = Some(Selection::new(SelectionType::Simple, p, side));
            s.view.selecting = true;
        }
    } else if resp.dragged_by(PointerButton::Primary) && s.view.selecting {
        if let Some(pos) = resp.interact_pointer_pos() {
            // Auto-scroll when dragging past the top or bottom edge.
            if pos.y < m.origin.y {
                s.term.lock().scroll_display(Scroll::Delta(1));
                ui.ctx().request_repaint();
            } else if pos.y > m.origin.y + m.rows as f32 * m.ch {
                s.term.lock().scroll_display(Scroll::Delta(-1));
                ui.ctx().request_repaint();
            }
            let offset = s.term.lock().grid().display_offset();
            let (col, row, side) = m.cell_at(pos);
            let p = Point::new(Line(row as i32 - offset as i32), Column(col));
            if let Some(sel) = s.term.lock().selection.as_mut() {
                sel.update(p, side);
            }
        }
    } else if resp.drag_stopped_by(PointerButton::Primary) && s.view.selecting {
        s.view.selecting = false;
        if set.copy_on_select {
            copy_selection(ui, s);
        }
    }

    if let Some(pos) = resp.interact_pointer_pos() {
        let (p, side) = point_at(pos);
        if resp.triple_clicked() {
            let mut sel = Selection::new(SelectionType::Lines, p, side);
            sel.update(p, side);
            s.term.lock().selection = Some(sel);
            if set.copy_on_select {
                copy_selection(ui, s);
            }
        } else if resp.double_clicked() {
            let mut sel = Selection::new(SelectionType::Semantic, p, side);
            sel.update(p, side);
            s.term.lock().selection = Some(sel);
            if set.copy_on_select {
                copy_selection(ui, s);
            }
        } else if resp.clicked() {
            s.term.lock().selection = None;
        }
    }

    // Right click: copy if something is selected, otherwise paste (Windows Terminal / PuTTY).
    if set.right_click_paste && resp.secondary_clicked() {
        if copy_selection(ui, s) {
            s.term.lock().selection = None;
        } else if let Some(t) = read_clipboard() {
            let mut out = Vec::new();
            paste_into(&mut out, &t, mode);
            s.send(out);
        }
    }
}

fn mouse_report(button: u8, col: usize, row: usize, pressed: bool, mode: TermMode, mods: Modifiers) -> Vec<u8> {
    let mut b = button;
    if mods.shift {
        b += 4;
    }
    if mods.alt {
        b += 8;
    }
    if mods.ctrl {
        b += 16;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{b};{};{}{end}", col + 1, row + 1).into_bytes()
    } else {
        // Legacy X10 encoding: coordinates above 222 cannot be represented.
        if col > 222 || row > 222 {
            return Vec::new();
        }
        let b = if pressed { b } else { 3 + (b & !3) };
        vec![0x1b, b'[', b'M', 32 + b, 33 + col as u8, 33 + row as u8]
    }
}

// ---------------------------------------------------------------- drawing

fn to_c32(rgb: alacritty_terminal::vte::ansi::Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

fn resolve(color: AnsiColor, colors: &Colors, look: &Look<'_>) -> Color32 {
    let t = look.theme;
    match color {
        AnsiColor::Spec(rgb) => to_c32(rgb),
        AnsiColor::Indexed(i) => colors[i as usize].map(to_c32).unwrap_or(look.palette[i as usize]),
        AnsiColor::Named(n) => {
            if let Some(c) = colors[n] {
                return to_c32(c);
            }
            let idx = n as usize;
            if idx < 16 {
                return look.palette[idx];
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => t.fg,
                NamedColor::Background => t.bg,
                NamedColor::Cursor => t.cursor,
                NamedColor::DimForeground => mix(t.fg, t.bg, 0.35),
                _ => {
                    // DimBlack..DimWhite
                    let base = idx.saturating_sub(NamedColor::DimBlack as usize).min(7);
                    mix(look.palette[base], t.bg, 0.35)
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Regular,
    Bold,
    Italic,
}

struct TextRun {
    row: usize,
    col: usize,
    next_col: usize,
    color: Color32,
    face: Face,
    text: String,
    ascii: bool,
}

fn draw(ui: &egui::Ui, painter: &egui::Painter, s: &mut Session, look: &Look<'_>, m: &Metrics, focused: bool) {
    let theme = look.theme;
    let set = look.settings;
    let term = s.term.lock();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let colors = content.colors;
    let selection = content.selection;
    let cursor = content.cursor;

    let mut bg_rects: Vec<(usize, usize, usize, Color32)> = Vec::new(); // row, col, width, color
    let mut lines: Vec<(Rect, Color32)> = Vec::new();
    let mut runs: Vec<TextRun> = Vec::new();
    // Streamer mode keeps a plain copy of the screen to search for secrets.
    let veil_on = veil::enabled();
    let mut cells: Vec<(char, Color32, Face)> =
        if veil_on { vec![(' ', theme.fg, Face::Regular); m.rows * m.cols] } else { Vec::new() };

    for item in content.display_iter {
        let cell = item.cell;
        let row = item.point.line.0 + offset;
        if row < 0 || row as usize >= m.rows {
            continue;
        }
        let row = row as usize;
        let col = item.point.column.0;
        if col >= m.cols {
            continue;
        }
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

        let mut fg_color = cell.fg;
        if set.bold_is_bright && flags.contains(Flags::BOLD) {
            match fg_color {
                AnsiColor::Named(n) if (n as usize) < 8 => {
                    fg_color = AnsiColor::Indexed(n as u8 + 8);
                }
                AnsiColor::Indexed(i) if i < 8 => {
                    fg_color = AnsiColor::Indexed(i + 8);
                }
                _ => {}
            }
        }
        let mut fg = resolve(fg_color, colors, look);
        let mut bg = resolve(cell.bg, colors, look);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::DIM) {
            fg = mix(fg, bg, 0.4);
        }
        if selection.is_some_and(|sel| sel.contains(item.point)) {
            bg = theme.selection;
        }
        if flags.contains(Flags::HIDDEN) {
            fg = bg;
        }

        if bg != theme.bg {
            match bg_rects.last_mut() {
                Some((r, c, w, color)) if *r == row && *c + *w == col && *color == bg => *w += width,
                _ => bg_rects.push((row, col, width, bg)),
            }
        }

        if flags.intersects(Flags::ALL_UNDERLINES) {
            let r = m.cell_rect(col, row, width);
            let y = (r.max.y - (m.ch * 0.14).max(1.0)).round();
            lines.push((Rect::from_min_max(pos2(r.min.x, y), pos2(r.max.x, y + 1.0)), fg));
        }
        if flags.contains(Flags::STRIKEOUT) {
            let r = m.cell_rect(col, row, width);
            let y = r.center().y.round();
            lines.push((Rect::from_min_max(pos2(r.min.x, y), pos2(r.max.x, y + 1.0)), fg));
        }

        let c = cell.c;
        if c == ' ' || c == '\0' || c == '\t' {
            continue;
        }
        let face = if flags.contains(Flags::BOLD) {
            Face::Bold
        } else if flags.contains(Flags::ITALIC) {
            Face::Italic
        } else {
            Face::Regular
        };
        if veil_on {
            cells[row * m.cols + col] = (c, fg, face);
        }
        let ascii = c.is_ascii() && width == 1;
        match runs.last_mut() {
            Some(run)
                if ascii
                    && run.ascii
                    && run.row == row
                    && run.next_col == col
                    && run.color == fg
                    && run.face == face =>
            {
                run.text.push(c);
                run.next_col += 1;
            }
            _ => {
                let mut text = String::with_capacity(16);
                text.push(c);
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
                runs.push(TextRun { row, col, next_col: col + width, color: fg, face, text, ascii });
            }
        }
    }

    for (row, col, w, color) in bg_rects {
        painter.rect_filled(m.cell_rect(col, row, w), 0.0, color);
    }

    // ---- cursor: smooth glide + trail + fading blink
    let crow = cursor.point.line.0 + offset;
    let visible_row = crow >= 0 && (crow as usize) < m.rows;
    let mut cursor_glyph: Option<(Rect, Color32)> = None;
    if cursor.shape != CursorShape::Hidden && visible_row {
        let ccol = cursor.point.column.0.min(m.cols - 1);
        let wide = term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR);
        let cell = m.cell_rect(ccol, crow as usize, if wide { 2 } else { 1 });
        let shape = if focused { cursor.shape } else { CursorShape::HollowBlock };
        let target = match shape {
            CursorShape::Beam => Rect::from_min_size(cell.min, vec2(2.0, cell.height())),
            CursorShape::Underline => Rect::from_min_max(pos2(cell.min.x, cell.max.y - 2.0), cell.max),
            _ => cell,
        };

        let now = Instant::now();
        let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.05);
        let view = &mut s.view;
        if view.cursor_target != target {
            view.cursor_target = target;
            view.blink_epoch = now;
        }
        let goal = [target.left_top(), target.right_top(), target.right_bottom(), target.left_bottom()];
        let trail = set.cursor_trail && set.animations;
        let corners = match view.cursor_corners {
            Some(mut cur) if trail => {
                let cur_center = cur.iter().fold(pos2(0.0, 0.0), |a, p| a + p.to_vec2()) / 4.0;
                let travel = target.center() - cur_center;
                let len = travel.length();
                for i in 0..4 {
                    let tau = if len < 0.5 {
                        0.03
                    } else {
                        let o = (goal[i] - target.center()).normalized();
                        let lead = (o.dot(travel / len) + 1.0) / 2.0; // 1 = leading corner
                        0.12 - 0.09 * lead
                    };
                    cur[i].x = anim::approach(cur[i].x, goal[i].x, dt, tau);
                    cur[i].y = anim::approach(cur[i].y, goal[i].y, dt, tau);
                }
                cur
            }
            _ => goal,
        };
        let moving = corners.iter().zip(goal.iter()).any(|(a, b)| (*a - *b).length() > 0.4);
        view.cursor_corners = Some(if moving { corners } else { goal });
        if moving {
            ui.ctx().request_repaint();
        }

        // kitty-style blink: solid for a moment after activity, then a smooth fade,
        // and no blinking after 15 s idle.
        let idle = view.blink_epoch.elapsed().as_secs_f32();
        let blinking = set.cursor_blink && focused && look.window_focused && !moving && idle > 0.6 && idle < 15.0;
        let alpha = if blinking {
            let phase = ((idle - 0.6) / 1.2).fract();
            let x = (phase * TAU).cos() * 0.5 + 0.5;
            anim::smooth(x)
        } else {
            1.0
        };
        if blinking {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        } else if set.cursor_blink && focused && idle <= 0.6 {
            ui.ctx().request_repaint_after(Duration::from_secs_f32(0.6 - idle + 0.01));
        }

        let color = theme.cursor.gamma_multiply(alpha);
        if moving {
            let mut mesh = Mesh::default();
            for p in corners {
                mesh.colored_vertex(p, theme.cursor.gamma_multiply(0.85));
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(Shape::mesh(mesh));
        } else {
            match shape {
                CursorShape::Block => {
                    painter.rect_filled(target, 2.0, color);
                    if alpha > 0.5 {
                        cursor_glyph = Some((cell, theme.bg));
                    }
                }
                CursorShape::HollowBlock => {
                    painter.rect_stroke(target.shrink(0.5), 2.0, Stroke::new(1.0, theme.cursor), StrokeKind::Inside);
                }
                CursorShape::Beam | CursorShape::Underline => {
                    painter.rect_filled(target, 1.0, color);
                }
                CursorShape::Hidden => {}
            }
        }
    }

    let face_font = |f: Face| match f {
        Face::Regular => &m.font,
        Face::Bold => &m.font_bold,
        Face::Italic => &m.font_italic,
    };
    let masked = if veil_on { veil_screen(painter, theme, m, &cells, &face_font) } else { Vec::new() };
    let is_masked = |row: usize, col: usize| masked.get(row * m.cols + col).copied().unwrap_or(false);

    for run in &runs {
        let r = m.cell_rect(run.col, run.row, 1);
        let font = face_font(run.face);
        let text: std::borrow::Cow<'_, str> = if masked.is_empty() {
            run.text.as_str().into()
        } else if run.ascii {
            run.text.chars().enumerate().map(|(i, c)| if is_masked(run.row, run.col + i) { ' ' } else { c }).collect::<String>().into()
        } else if is_masked(run.row, run.col) {
            continue;
        } else {
            run.text.as_str().into()
        };
        painter.text(r.min + vec2(0.0, m.text_dy), Align2::LEFT_TOP, text, font.clone(), run.color);
    }
    // Glyph under a block cursor, in the background color.
    let cursor_glyph = cursor_glyph.filter(|_| {
        let row = cursor.point.line.0 + offset;
        row < 0 || !is_masked(row as usize, cursor.point.column.0)
    });
    if let Some((r, color)) = cursor_glyph {
        let ch = term.grid()[cursor.point].c;
        if ch != ' ' && ch != '\0' {
            painter.text(r.min + vec2(0.0, m.text_dy), Align2::LEFT_TOP, ch, m.font.clone(), color);
        }
    }
    for (r, color) in lines {
        painter.rect_filled(r, 0.0, color);
    }

    // Thin scrollbar while looking at history.
    if offset > 0 {
        let total = term.grid().total_lines().max(1) as f32;
        let screen = m.rows as f32;
        let track = Rect::from_min_max(
            pos2(m.origin.x + m.cols as f32 * m.cw + 3.0, m.origin.y),
            pos2(m.origin.x + m.cols as f32 * m.cw + 6.0, m.origin.y + screen * m.ch),
        );
        let h = (screen / total * track.height()).max(16.0);
        let top_frac = 1.0 - (offset as f32 + screen) / total;
        let y = track.min.y + top_frac.clamp(0.0, 1.0) * (track.height() - h);
        painter.rect_filled(
            Rect::from_min_size(pos2(track.min.x, y), vec2(3.0, h)),
            2.0,
            theme.accent.gamma_multiply(0.7),
        );
    }
}

/// Finds secrets on the visible screen, paints their masks and queues the real text for the
/// overlay. Returns which cells are masked (row-major).
fn veil_screen<'f>(
    painter: &egui::Painter,
    theme: &Theme,
    m: &Metrics,
    cells: &[(char, Color32, Face)],
    face_font: &dyn Fn(Face) -> &'f FontId,
) -> Vec<bool> {
    let mut masked = vec![false; cells.len()];
    let chip = mix(theme.bg, theme.overlay, 0.45);
    let mut text = String::with_capacity(m.cols);
    let mut col_at: Vec<usize> = Vec::with_capacity(m.cols + 1);
    for row in 0..m.rows {
        let line = &cells[row * m.cols..(row + 1) * m.cols];
        if line.iter().all(|c| c.0 == ' ') {
            continue;
        }
        text.clear();
        col_at.clear();
        for (c, _, _) in line {
            col_at.push(text.len());
            text.push(*c);
        }
        col_at.push(text.len());
        for r in veil::find(&text) {
            let c0 = col_at.partition_point(|&b| b < r.start);
            let c1 = col_at.partition_point(|&b| b < r.end).min(m.cols);
            if c1 <= c0 {
                continue;
            }
            masked[row * m.cols + c0..row * m.cols + c1].fill(true);
            let mut texts = Vec::new();
            let mut seg = c0;
            for c in c0 + 1..=c1 {
                if c == c1 || line[c].1 != line[seg].1 || line[c].2 != line[seg].2 {
                    let s: String = line[seg..c].iter().map(|x| x.0).collect();
                    if !s.trim().is_empty() {
                        let g = painter.layout_no_wrap(s, face_font(line[seg].2).clone(), Color32::PLACEHOLDER);
                        texts.push((m.cell_rect(seg, row, 1).min + vec2(0.0, m.text_dy), g, line[seg].1));
                    }
                    seg = c;
                }
            }
            veil::cover(painter, m.cell_rect(c0, row, c1 - c0), chip, 0.0, texts);
        }
    }
    masked
}

fn draw_status(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, theme: &Theme, status: &Status, id: egui::Id) {
    let ctx = ui.ctx();
    let shown = !matches!(status, Status::Connected);
    let t = ctx.animate_bool_with_time_and_easing(id.with("status-pill"), shown, 0.35, anim::wind);
    if t <= 0.001 {
        return;
    }
    let (text, color) = match status {
        Status::Connecting | Status::Connected => ("connecting…", theme.warn),
        Status::Closed(_) => ("disconnected  ·  press Enter to reconnect", theme.err),
    };
    let galley = painter.layout_no_wrap(text.to_string(), fonts::ui(13.0), theme.text);
    let size = galley.size() + vec2(36.0, 14.0);
    let y = rect.max.y - 22.0 - size.y / 2.0 + (1.0 - t) * 24.0;
    let pill = Rect::from_center_size(pos2(rect.center().x, y), size);
    let mut p = painter.clone();
    p.multiply_opacity(t.clamp(0.0, 1.0));
    fx::glow(&p, pill, size.y / 2.0, color.gamma_multiply(0.25), 16.0, 0.0);
    p.rect(pill, CornerRadius::same(255), theme.mantle, Stroke::new(1.0, color.gamma_multiply(0.8)), StrokeKind::Inside);
    let time = ui.input(|i| i.time) as f32;
    let pulse = if matches!(status, Status::Connecting) { 0.5 + 0.5 * (time * 5.0).sin() } else { 1.0 };
    let dot = pos2(pill.min.x + 15.0, pill.center().y);
    p.circle_filled(dot, 6.0 * pulse, color.gamma_multiply(0.25));
    p.circle_filled(dot, 3.5, color);
    p.galley(pill.min + vec2(26.0, 7.0), galley, theme.text);
    if matches!(status, Status::Connecting) {
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
    use crate::theme;

    struct Dummy;
    impl alacritty_terminal::event::EventListener for Dummy {
        fn send_event(&self, _: alacritty_terminal::event::Event) {}
    }

    fn dummy_look<'a>(th: &'a Theme, pal: &'a [Color32; 256], set: &'a Settings) -> Look<'a> {
        Look {
            theme: th,
            palette: pal,
            settings: set,
            time: 0.0,
            window_focused: true,
            pane_alpha: 1.0,
        }
    }

    #[test]
    fn test_resolve_named_and_spec_colors() {
        let th = theme::by_name("Catppuccin Mocha");
        let pal = th.palette256();
        let set = Settings::default();
        let look = dummy_look(th, &pal, &set);

        let size = GridSize { cols: 80, rows: 24 };
        let term = alacritty_terminal::term::Term::new(TermConfig::default(), &size, Dummy);
        let content = term.renderable_content();
        let colors = content.colors;

        // Named 0..15 match palette 0..15
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Black), colors, &look), pal[0]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Red), colors, &look), pal[1]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Green), colors, &look), pal[2]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Yellow), colors, &look), pal[3]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Blue), colors, &look), pal[4]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Magenta), colors, &look), pal[5]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Cyan), colors, &look), pal[6]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::White), colors, &look), pal[7]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::BrightBlack), colors, &look), pal[8]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::BrightRed), colors, &look), pal[9]);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::BrightWhite), colors, &look), pal[15]);

        // Special named colors
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Foreground), colors, &look), th.fg);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::BrightForeground), colors, &look), th.fg);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Background), colors, &look), th.bg);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::Cursor), colors, &look), th.cursor);
        assert_eq!(resolve(AnsiColor::Named(NamedColor::DimForeground), colors, &look), mix(th.fg, th.bg, 0.35));

        // Dim colors
        assert_eq!(resolve(AnsiColor::Named(NamedColor::DimRed), colors, &look), mix(pal[1], th.bg, 0.35));

        // Spec truecolor
        let rgb = Rgb { r: 123, g: 210, b: 45 };
        assert_eq!(resolve(AnsiColor::Spec(rgb), colors, &look), Color32::from_rgb(123, 210, 45));
    }

    #[test]
    fn test_resolve_all_256_indexed_colors() {
        let th = theme::by_name("Catppuccin Mocha");
        let pal = th.palette256();
        let set = Settings::default();
        let look = dummy_look(th, &pal, &set);

        let size = GridSize { cols: 80, rows: 24 };
        let term = alacritty_terminal::term::Term::new(TermConfig::default(), &size, Dummy);
        let content = term.renderable_content();
        let colors = content.colors;

        for i in 0..=255u8 {
            assert_eq!(
                resolve(AnsiColor::Indexed(i), colors, &look),
                pal[i as usize],
                "index {i} should resolve to palette[{i}]"
            );
        }
    }

    #[test]
    fn test_osc4_color_override_takes_precedence() {
        let th = theme::by_name("Catppuccin Mocha");
        let pal = th.palette256();
        let set = Settings::default();
        let look = dummy_look(th, &pal, &set);

        let size = GridSize { cols: 80, rows: 24 };
        let mut term = alacritty_terminal::term::Term::new(TermConfig::default(), &size, Dummy);
        let mut parser = alacritty_terminal::vte::ansi::Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();

        // OSC 4 ; 1 ; rgb:aa/bb/cc \x07 (override color 1)
        parser.advance(&mut term, b"\x1b]4;1;rgb:aa/bb/cc\x07");

        let content = term.renderable_content();
        let colors = content.colors;

        let resolved_named = resolve(AnsiColor::Named(NamedColor::Red), colors, &look);
        let resolved_indexed = resolve(AnsiColor::Indexed(1), colors, &look);
        let expected = Color32::from_rgb(0xaa, 0xbb, 0xcc);

        assert_eq!(resolved_named, expected, "OSC 4 should override Named Red");
        assert_eq!(resolved_indexed, expected, "OSC 4 should override Indexed 1");
    }

    #[test]
    fn test_bold_is_bright_promotion_logic() {
        let promote = |fg_color: AnsiColor| -> AnsiColor {
            let mut c = fg_color;
            match c {
                AnsiColor::Named(n) if (n as usize) < 8 => {
                    c = AnsiColor::Indexed(n as u8 + 8);
                }
                AnsiColor::Indexed(i) if i < 8 => {
                    c = AnsiColor::Indexed(i + 8);
                }
                _ => {}
            }
            c
        };

        // Named 0..7 become Indexed 8..15
        assert_eq!(promote(AnsiColor::Named(NamedColor::Black)), AnsiColor::Indexed(8));
        assert_eq!(promote(AnsiColor::Named(NamedColor::Red)), AnsiColor::Indexed(9));
        assert_eq!(promote(AnsiColor::Named(NamedColor::White)), AnsiColor::Indexed(15));

        // Indexed 0..7 become Indexed 8..15
        assert_eq!(promote(AnsiColor::Indexed(0)), AnsiColor::Indexed(8));
        assert_eq!(promote(AnsiColor::Indexed(3)), AnsiColor::Indexed(11));
        assert_eq!(promote(AnsiColor::Indexed(7)), AnsiColor::Indexed(15));

        // Already bright or standard 16..255 or foreground do not change
        assert_eq!(promote(AnsiColor::Named(NamedColor::BrightRed)), AnsiColor::Named(NamedColor::BrightRed));
        assert_eq!(promote(AnsiColor::Indexed(8)), AnsiColor::Indexed(8));
        assert_eq!(promote(AnsiColor::Indexed(196)), AnsiColor::Indexed(196));
        assert_eq!(promote(AnsiColor::Named(NamedColor::Foreground)), AnsiColor::Named(NamedColor::Foreground));
    }

    #[test]
    fn test_veil_screen_masking_logic() {
        let th = theme::by_name("Catppuccin Mocha");
        veil::begin_frame(true, ["secret.corp.internal".to_string()]);

        let cols = 35;
        let rows = 2;
        let line0 = "Host: secret.corp.internal ready   ";
        let line1 = "Connecting to 10.20.30.40 now      ";

        let mut cells = Vec::new();
        for c in line0.chars() {
            cells.push((c, th.fg, Face::Regular));
        }
        for c in line1.chars() {
            cells.push((c, th.fg, Face::Regular));
        }

        // Mock egui painter via Context
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |_| {});
        output.textures_delta.clear();
        let painter = ctx.layer_painter(egui::LayerId::background());
        let font = FontId::monospace(13.0);
        let m = Metrics {
            font: font.clone(),
            font_bold: font.clone(),
            font_italic: font.clone(),
            cw: 8.0,
            ch: 16.0,
            text_dy: 0.0,
            origin: Pos2::ZERO,
            cols,
            rows,
        };

        let masked = veil_screen(&painter, th, &m, &cells, &|_| &font);
        assert_eq!(masked.len(), cols * rows);

        // Row 0: "Host: secret.corp.internal ready   "
        // "secret.corp.internal" is cols 6..26
        assert!(!masked[0], "col 0 ('H') should not be masked");
        assert!(!masked[5], "col 5 (' ') should not be masked");
        for c in 6..26 {
            assert!(masked[c], "col {c} in 'secret.corp.internal' should be masked");
        }
        assert!(!masked[26], "col 26 (' ') should not be masked");

        // Row 1: "Connecting to 10.20.30.40 now      "
        // "10.20.30.40" is cols 14..25
        let r1 = cols;
        assert!(!masked[r1 + 0], "row 1 col 0 should not be masked");
        assert!(!masked[r1 + 13], "row 1 col 13 should not be masked");
        for c in 14..25 {
            assert!(masked[r1 + c], "row 1 col {c} in '10.20.30.40' should be masked");
        }
        assert!(!masked[r1 + 25], "row 1 col 25 should not be masked");
    }
}


