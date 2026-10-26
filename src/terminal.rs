//! A real terminal (PTY + VT emulation via alacritty_terminal), rendered with GPUI.

use crate::request::Request;
use crate::{theme, util};
use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as TPoint, Side as TSide};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor, Rgb};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::*;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Once};

#[derive(Clone)]
struct Listener(UnboundedSender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.unbounded_send(event);
    }
}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub enum TerminalEvent {
    Exited,
    Title,
}

impl EventEmitter<TerminalEvent> for TerminalView {}
impl EventEmitter<Request> for TerminalView {}

struct Geometry {
    origin: Point<Pixels>,
    cell_w: f32,
    line_h: f32,
}

pub struct TerminalView {
    focus: FocusHandle,
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    size: Size,
    pub title: String,
    pub cwd: PathBuf,
    pub exited: bool,
    pub font_size: f32,
    scroll_acc: f32,
    selecting: bool,
    geometry: Option<Geometry>,
    _events: Task<()>,
}

static ENV: Once = Once::new();

/// A started shell: emulator state, the PTY writer and the event stream.
pub struct Started {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    events: futures::channel::mpsc::UnboundedReceiver<TermEvent>,
    size: Size,
}

/// Starts the user's login shell in `cwd`.
pub fn start(cwd: &std::path::Path) -> anyhow::Result<Started> {
    ENV.call_once(tty::setup_env);
    let size = Size { cols: 100, rows: 30 };
    let (tx, rx) = unbounded();
    let listener = Listener(tx);
    let config = Config { scrolling_history: 10_000, ..Default::default() };
    let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
    let options = tty::Options {
        shell: None,
        working_directory: Some(cwd.to_path_buf()),
        drain_on_exit: false,
        env: [("TERM_PROGRAM".to_string(), "kite".to_string())].into_iter().collect(),
    };
    let window_size = WindowSize { num_lines: size.rows as u16, num_cols: size.cols as u16, cell_width: 8, cell_height: 16 };
    let pty = tty::new(&options, window_size, 0)?;
    let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
    let sender = event_loop.channel();
    let _io = event_loop.spawn();
    Ok(Started { term, sender, events: rx, size })
}

impl TerminalView {
    pub fn new(started: Started, cwd: PathBuf, font_size: f32, cx: &mut Context<Self>) -> Self {
        let Started { term, sender, events: mut rx, size } = started;
        let events = cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                // Coalesce bursts of output into one repaint.
                let mut batch = vec![ev];
                while let Ok(Some(more)) = rx.try_next() {
                    batch.push(more);
                }
                let alive = this
                    .update(cx, |view, cx| {
                        for ev in batch {
                            view.handle_event(ev, cx);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "zsh".into());
        let title = shell.rsplit('/').next().unwrap_or("shell").to_string();
        Self {
            focus: cx.focus_handle(),
            term,
            sender,
            size,
            title,
            cwd,
            exited: false,
            font_size,
            scroll_acc: 0.,
            selecting: false,
            geometry: None,
            _events: events,
        }
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes));
        }
    }

    /// Types a command into the shell and runs it.
    pub fn run(&self, command: &str) {
        self.write(format!("{command}\r").into_bytes());
    }

    fn handle_event(&mut self, ev: TermEvent, cx: &mut Context<Self>) {
        match ev {
            TermEvent::Title(t) => {
                self.title = t;
                cx.emit(TerminalEvent::Title);
            }
            TermEvent::ResetTitle => {}
            TermEvent::PtyWrite(s) => self.write(s.into_bytes()),
            TermEvent::ClipboardStore(_, text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            TermEvent::ClipboardLoad(_, f) => {
                let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
                self.write(f(&text).into_bytes());
            }
            TermEvent::ColorRequest(ix, f) => {
                let c = palette(ix);
                self.write(f(c).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(f) => {
                let ws = self.window_size();
                self.write(f(ws).into_bytes());
            }
            TermEvent::Exit | TermEvent::ChildExit(_) => {
                if !self.exited {
                    self.exited = true;
                    cx.emit(TerminalEvent::Exited);
                }
            }
            _ => {}
        }
    }

    fn window_size(&self) -> WindowSize {
        let (cw, lh) = self.geometry.as_ref().map(|g| (g.cell_w, g.line_h)).unwrap_or((8., 16.));
        WindowSize { num_lines: self.size.rows as u16, num_cols: self.size.cols as u16, cell_width: cw as u16, cell_height: lh as u16 }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.size.cols && rows == self.size.rows {
            return;
        }
        self.size = Size { cols, rows };
        self.term.lock().resize(self.size);
        let _ = self.sender.send(Msg::Resize(self.window_size()));
    }

    // ------------------------------------------------------------ keyboard

    fn key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        if m.platform {
            match ks.key.as_str() {
                "c" => {
                    if let Some(text) = self.term.lock().selection_to_string().filter(|s| !s.is_empty()) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        cx.stop_propagation();
                    }
                }
                "v" => {
                    self.paste(cx);
                    cx.stop_propagation();
                }
                "k" if !m.shift => {
                    let mut term = self.term.lock();
                    term.grid_mut().clear_history();
                    drop(term);
                    self.write(&b"\x0c"[..]);
                    cx.stop_propagation();
                }
                _ => {} // let app shortcuts (⌘P, ⌘D, ⌘W …) through
            }
            return;
        }
        // Keys the app keeps even inside the terminal.
        if m.control && (matches!(ks.key.as_str(), "`" | "tab") || (m.alt && matches!(ks.key.as_str(), "h" | "j" | "k" | "l"))) {
            return;
        }
        let mode = *self.term.lock().mode();
        if let Some(bytes) = key_bytes(ks, mode) {
            {
                let mut term = self.term.lock();
                term.scroll_display(Scroll::Bottom);
                term.selection = None;
            }
            self.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.term.lock().mode().contains(TermMode::BRACKETED_PASTE) {
            self.write(format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes());
        } else {
            self.write(text.into_bytes());
        }
    }

    // ------------------------------------------------------------ mouse

    fn grid_point(&self, p: Point<Pixels>) -> Option<(usize, usize)> {
        let g = self.geometry.as_ref()?;
        let x = ((p.x - g.origin.x) / px(1.)).max(0.);
        let y = ((p.y - g.origin.y) / px(1.)).max(0.);
        let col = ((x / g.cell_w) as usize).min(self.size.cols.saturating_sub(1));
        let row = ((y / g.line_h) as usize).min(self.size.rows.saturating_sub(1));
        Some((col, row))
    }

    fn term_point(&self, col: usize, row: usize) -> TPoint {
        let offset = self.term.lock().grid().display_offset() as i32;
        TPoint::new(Line(row as i32 - offset), Column(col))
    }

    fn mouse_report(&self, button: u8, col: usize, row: usize, pressed: bool) {
        let suffix = if pressed { 'M' } else { 'm' };
        self.write(format!("\x1b[<{button};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let Some((col, row)) = self.grid_point(ev.position) else { return };
        let mode = *self.term.lock().mode();
        if ev.modifiers.platform {
            // ⌘-click opens file:line references printed in the terminal.
            if let Some((path, line, _)) = self.location_at(col, row) {
                let path = if PathBuf::from(&path).is_absolute() { PathBuf::from(path) } else { self.cwd.join(path) };
                cx.emit(Request::OpenFile { path, line: Some(line), preview: false });
            }
            return;
        }
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) && !ev.modifiers.shift {
            self.mouse_report(0, col, row, true);
            return;
        }
        let point = self.term_point(col, row);
        let ty = match ev.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.lock().selection = Some(Selection::new(ty, point, TSide::Left));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !ev.dragging() {
            self.selecting = false;
            return;
        }
        let Some((col, row)) = self.grid_point(ev.position) else { return };
        let mode = *self.term.lock().mode();
        if mode.contains(TermMode::MOUSE_DRAG) && mode.contains(TermMode::SGR_MOUSE) && !ev.modifiers.shift {
            self.mouse_report(32, col, row, true);
            return;
        }
        if self.selecting {
            let point = self.term_point(col, row);
            if let Some(sel) = self.term.lock().selection.as_mut() {
                sel.update(point, TSide::Right);
            }
            cx.notify();
        }
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        let mode = *self.term.lock().mode();
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) && !ev.modifiers.shift
            && let Some((col, row)) = self.grid_point(ev.position)
        {
            self.mouse_report(0, col, row, false);
        }
        // A click without a drag clears the selection.
        let mut term = self.term.lock();
        if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
            term.selection = None;
        }
        drop(term);
        cx.notify();
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lh = self.geometry.as_ref().map(|g| g.line_h).unwrap_or(16.);
        self.scroll_acc += ev.delta.pixel_delta(px(lh)).y / px(1.);
        let lines = (self.scroll_acc / lh) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_acc -= lines as f32 * lh;
        let mode = *self.term.lock().mode();
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) {
            let (col, row) = self.grid_point(ev.position).unwrap_or((0, 0));
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.abs().min(10) {
                self.mouse_report(button, col, row, true);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            // less, man, vim without mouse: scroll with arrow keys.
            let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let mut bytes = Vec::new();
            for _ in 0..lines.abs().min(10) {
                bytes.extend_from_slice(seq);
            }
            self.write(bytes);
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn location_at(&self, col: usize, row: usize) -> Option<(String, usize, usize)> {
        let term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let line = Line(row as i32 - offset);
        let text: String = (0..self.size.cols).map(|c| term.grid()[line][Column(c)].c).collect();
        // Find the whitespace-separated token under the click and parse it.
        let chars: Vec<char> = text.chars().collect();
        let col = col.min(chars.len().saturating_sub(1));
        let mut start = col;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let mut end = col;
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        let token: String = chars[start..end].iter().collect();
        util::parse_location(&token).or_else(|| {
            let t = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-');
            self.cwd.join(t).is_file().then(|| (t.to_string(), 1, 1))
        })
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// Bytes a key press sends to the shell (xterm conventions; ⌥ acts as Meta).
fn key_bytes(ks: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let key = ks.key.as_str();
    let modp = 1 + m.shift as u8 + 2 * (m.alt as u8) + 4 * (m.control as u8);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let csi_letter = |c: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 { format!("\x1b[{n};{modp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let esc = |mut v: Vec<u8>| -> Vec<u8> {
        if m.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    Some(match key {
        "enter" => esc(b"\r".to_vec()),
        "backspace" => esc(if m.control { vec![0x08] } else { vec![0x7f] }),
        "tab" => if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() },
        "escape" => b"\x1b".to_vec(),
        "space" => if m.control { vec![0] } else { esc(b" ".to_vec()) },
        "up" => csi_letter('A'),
        "down" => csi_letter('B'),
        "right" => csi_letter('C'),
        "left" => csi_letter('D'),
        "home" => csi_letter('H'),
        "end" => csi_letter('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.clone().next()) else {
                // Multi-char key names we don't map; fall back to typed text.
                return ks.key_char.as_ref().map(|s| s.as_bytes().to_vec());
            };
            if m.control {
                let code = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '7' => 0x1f,
                    '/' => 0x1f,
                    '8' => 0x7f,
                    _ => return None,
                };
                return Some(esc(vec![code]));
            }
            if m.alt {
                // Option as Meta: send ESC + the unmodified key.
                let s = if m.shift { c.to_uppercase().to_string() } else { c.to_string() };
                return Some(esc(s.into_bytes()));
            }
            ks.key_char.clone().unwrap_or_else(|| c.to_string()).into_bytes()
        }
    })
}

// ---------------------------------------------------------------- colors

const ANSI: [u32; 16] = [
    0x1d1f21, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd7dae0,
    0x5c6370, 0xff7a85, 0xb5e890, 0xffd68a, 0x82c4ff, 0xde9bf0, 0x7fd4dd, 0xffffff,
];

fn palette(ix: usize) -> Rgb {
    let hex = match ix {
        0..16 => ANSI[ix],
        16..232 => {
            let i = ix - 16;
            let level = |v: usize| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
        }
        232..256 => {
            let v = 8 + (ix as u32 - 232) * 10;
            (v << 16) | (v << 8) | v
        }
        256 => theme::TERM_FG,
        257 => theme::TERM_BG,
        258 => 0xaeafad,
        _ => theme::TERM_FG,
    };
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

fn resolve(color: AColor, term_colors: &alacritty_terminal::term::color::Colors, dim: bool) -> Hsla {
    let rgb = match color {
        AColor::Spec(rgb) => rgb,
        AColor::Indexed(i) => term_colors[i as usize].unwrap_or_else(|| palette(i as usize)),
        AColor::Named(n) => {
            let ix = n as usize;
            let base = match n {
                NamedColor::DimBlack | NamedColor::DimRed | NamedColor::DimGreen | NamedColor::DimYellow | NamedColor::DimBlue
                | NamedColor::DimMagenta | NamedColor::DimCyan | NamedColor::DimWhite => {
                    let normal = ix - NamedColor::DimBlack as usize;
                    let c = term_colors[normal].unwrap_or_else(|| palette(normal));
                    return Hsla::from(rgb_color(c)).opacity(0.7);
                }
                NamedColor::BrightForeground => palette(15),
                NamedColor::DimForeground => palette(7),
                _ => term_colors[ix].unwrap_or_else(|| palette(ix)),
            };
            base
        }
    };
    let c: Hsla = rgb_color(rgb).into();
    if dim { c.opacity(0.65) } else { c }
}

fn rgb_color(c: Rgb) -> Rgba {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
}

// ---------------------------------------------------------------- element

struct Run {
    col: usize,
    text: String,
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
}

pub struct TermPaint {
    quads: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PaintQuad>,
    line_h: Pixels,
}

struct TerminalElement {
    view: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TermPaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> TermPaint {
        self.view.update(cx, |view, cx| view.layout(bounds, window, cx))
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), p: &mut TermPaint, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for q in p.quads.drain(..) {
                window.paint_quad(q);
            }
            if let Some(c) = p.cursor.take() {
                window.paint_quad(c);
            }
            for (origin, line) in &p.lines {
                line.paint(*origin, p.line_h, window, cx).ok();
            }
        });
    }
}

impl TerminalView {
    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> TermPaint {
        let font_size = px(self.font_size);
        let mono = font(theme::MONO_FONT);
        let probe = window.text_system().shape_line(
            "0".repeat(20).into(),
            font_size,
            &[TextRun { len: 20, font: mono.clone(), color: theme::fg(), background_color: None, underline: None, strikethrough: None }],
            None,
        );
        let cell_w = probe.width / px(1.) / 20.;
        let line_h = (self.font_size * 1.35).round();
        let pad = 6.;
        let origin = point(bounds.left() + px(pad), bounds.top() + px(pad / 2.));
        let cols = (((bounds.size.width / px(1.)) - pad * 2.) / cell_w).floor().max(2.) as usize;
        let rows = (((bounds.size.height / px(1.)) - pad) / line_h).floor().max(1.) as usize;
        self.resize(cols, rows);
        self.geometry = Some(Geometry { origin, cell_w, line_h });
        let focused = self.focus.is_focused(window);

        let term = self.term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let default_bg = theme::c(theme::TERM_BG);
        let mut quads = vec![fill(bounds, default_bg)];
        let mut rows_runs: Vec<Vec<Run>> = (0..rows).map(|_| Vec::new()).collect();
        let selection = content.selection;
        let cursor = content.cursor;
        let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR);

        for cell in content.display_iter {
            let row = cell.point.line.0 + offset;
            if row < 0 || row as usize >= rows {
                continue;
            }
            let (row, col) = (row as usize, cell.point.column.0);
            if col >= cols || cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let dim = cell.flags.contains(Flags::DIM);
            let mut fg = resolve(cell.fg, colors, dim);
            let mut bg = match cell.bg {
                AColor::Named(NamedColor::Background) => None,
                c => Some(resolve(c, colors, false)),
            };
            if cell.flags.contains(Flags::INVERSE) {
                let new_bg = fg;
                fg = bg.unwrap_or(default_bg);
                bg = Some(new_bg);
            }
            if selection.is_some_and(|s| s.contains(cell.point)) {
                bg = Some(theme::selection());
            }
            let x = origin.x + px(col as f32 * cell_w);
            let y = origin.y + px(row as f32 * line_h);
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2. } else { 1. };
            if let Some(bg) = bg {
                quads.push(fill(Bounds::new(point(x, y), size(px(cell_w * width), px(line_h))), bg));
            }
            if cell.c == ' ' || cell.c == '\0' || cell.flags.contains(Flags::HIDDEN) {
                continue;
            }
            let bold = cell.flags.contains(Flags::BOLD);
            let italic = cell.flags.contains(Flags::ITALIC);
            let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
            let runs = &mut rows_runs[row];
            match runs.last_mut() {
                Some(r) if r.fg == fg && r.bold == bold && r.italic == italic && r.underline == underline && r.col + r.text.chars().count() == col && width == 1. => {
                    r.text.push(cell.c);
                }
                _ => runs.push(Run { col, text: cell.c.to_string(), fg, bold, italic, underline }),
            }
        }

        let mut cursor_quad = None;
        let cursor_row = cursor.point.line.0 + offset;
        if show_cursor && cursor_row >= 0 && (cursor_row as usize) < rows && cursor.shape != CursorShape::Hidden {
            let x = origin.x + px(cursor.point.column.0 as f32 * cell_w);
            let y = origin.y + px(cursor_row as f32 * line_h);
            let color = theme::cursor();
            cursor_quad = Some(match (focused, cursor.shape) {
                (true, CursorShape::Beam) => fill(Bounds::new(point(x, y), size(px(2.), px(line_h))), color),
                (true, CursorShape::Underline) => fill(Bounds::new(point(x, y + px(line_h - 2.)), size(px(cell_w), px(2.))), color),
                (true, _) => fill(Bounds::new(point(x, y), size(px(cell_w), px(line_h))), color.opacity(0.75)),
                (false, _) => outline(Bounds::new(point(x, y), size(px(cell_w), px(line_h))), color, BorderStyle::default()),
            });
        }
        drop(term);

        let mut lines = Vec::new();
        for (row, runs) in rows_runs.into_iter().enumerate() {
            for r in runs {
                let mut f = mono.clone();
                if r.bold {
                    f.weight = FontWeight::BOLD;
                }
                if r.italic {
                    f.style = FontStyle::Italic;
                }
                let len = r.text.len();
                let underline = r.underline.then(|| UnderlineStyle { color: Some(r.fg), thickness: px(1.), wavy: false });
                let shaped = window.text_system().shape_line(
                    r.text.into(),
                    font_size,
                    &[TextRun { len, font: f, color: r.fg, background_color: None, underline, strikethrough: None }],
                    None,
                );
                lines.push((point(origin.x + px(r.col as f32 * cell_w), origin.y + px(row as f32 * line_h)), shaped));
            }
        }
        let _ = cx;
        TermPaint { quads, lines, cursor: cursor_quad, line_h: px(line_h) }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(TerminalElement { view: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::{TermMode, key_bytes};
    use gpui::{Keystroke, Modifiers};

    fn ks(key: &str, m: Modifiers, ch: Option<&str>) -> Keystroke {
        Keystroke { modifiers: m, key: key.into(), key_char: ch.map(String::from) }
    }

    #[test]
    fn maps_common_keys() {
        let none = Modifiers::default();
        let ctrl = Modifiers { control: true, ..Default::default() };
        let alt = Modifiers { alt: true, ..Default::default() };
        assert_eq!(key_bytes(&ks("c", ctrl, None), TermMode::empty()).unwrap(), vec![3]);
        assert_eq!(key_bytes(&ks("up", none, None), TermMode::empty()).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(&ks("up", none, None), TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(&ks("b", alt, Some("∫")), TermMode::empty()).unwrap(), b"\x1bb");
        assert_eq!(key_bytes(&ks("a", none, Some("A")), TermMode::empty()).unwrap(), b"A");
        assert_eq!(key_bytes(&ks("backspace", none, None), TermMode::empty()).unwrap(), vec![0x7f]);
    }

    #[test]
    fn shell_runs_and_renders_output() {
        use alacritty_terminal::event::Notify;
        use alacritty_terminal::event_loop::Notifier;
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Line};
        let dir = std::env::temp_dir();
        let started = super::start(&dir).expect("pty starts");
        Notifier(started.sender.clone()).notify(&b"echo kite-$((40+2))\r"[..]);
        let mut found = false;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let term = started.term.lock();
            let text: String = (0..term.grid().screen_lines())
                .flat_map(|l| (0..term.grid().columns()).map(move |c| (l, c)))
                .map(|(l, c)| term.grid()[Line(l as i32)][Column(c)].c)
                .collect();
            if text.contains("kite-42") {
                found = true;
                break;
            }
        }
        assert!(found, "shell output never appeared in the terminal grid");
    }
}
