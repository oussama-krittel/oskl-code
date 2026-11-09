//! A read-only table: CSV/TSV files (each column in its own color, like the editor shows them)
//! and spreadsheet sheets. Only the rows on screen are parsed and drawn; a CSV is kept once
//! (big files are mapped from disk, not read in) with where each row starts.

use crate::theme;
use crate::ui::px;
use gpui::prelude::*;
use gpui::{Bounds, Context, FontWeight, Pixels, SharedString, UniformListScrollHandle, Window, div, uniform_list};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

/// Files bigger than this are mapped from disk instead of read into memory.
const MAP_OVER: u64 = 32 * 1024 * 1024;
/// The most of one cell shown (the rest is cut, with an ellipsis).
const CELL_MAX: usize = 300;

enum Text {
    Owned(Arc<str>),
    Mapped(Arc<memmap2::Mmap>),
}

impl Text {
    fn bytes(&self) -> &[u8] {
        match self {
            Text::Owned(s) => s.as_bytes(),
            Text::Mapped(m) => m,
        }
    }
}

enum Store {
    /// Delimited text: the text, where each row starts, the separator.
    Delimited { text: Text, starts: Vec<usize>, sep: u8 },
    Cells(Vec<Vec<String>>),
}

/// How a spreadsheet cell looks (from the workbook's styles).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Text and background colors (RGB).
    pub color: Option<u32>,
    pub fill: Option<u32>,
    /// Points.
    pub size: Option<f32>,
    /// Borders drawn: left 1, right 2, top 4, bottom 8.
    pub borders: u8,
    pub border_color: Option<u32>,
    /// How heavy the borders are (points): thin, medium, thick.
    pub border_w: f32,
    /// 0 as usual (numbers right, text left), 1 left, 2 center, 3 right.
    pub align: u8,
    pub wrap: bool,
}

impl Look {
    /// Shows something even with no text in the cell.
    pub fn visible(&self) -> bool {
        self.fill.is_some() || self.borders != 0
    }
}

/// A spreadsheet's look: each cell's style, merged cells, the default row height.
pub struct Paper {
    pub looks: Vec<Vec<u32>>,
    pub palette: Arc<Vec<Look>>,
    /// A merged range's first cell → its last (row, col).
    pub merges: std::collections::HashMap<(usize, usize), (usize, usize)>,
    /// Cells inside a merged range (not its first) → its first.
    pub covered: std::collections::HashMap<(usize, usize), (usize, usize)>,
    /// Points.
    pub row_h: f32,
}

pub struct Sheet {
    pub name: String,
    store: Store,
    cols: usize,
    /// The first row names the columns (CSV); otherwise columns are lettered (A, B…).
    header: bool,
    widths: Vec<f32>,
    /// A spreadsheet drawn as it looks in Excel (white, gridlines, its colors and borders).
    paper: Option<Paper>,
}

/// The separator a delimited file uses (by its name, else by its first line).
pub fn separator(path: &Path, first_line: &str) -> u8 {
    match path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("tsv" | "tab") => b'\t',
        Some("psv") => b'|',
        _ => {
            let count = |c: char| first_line.matches(c).count();
            [',', ';', '\t', '|'].into_iter().max_by_key(|c| count(*c)).filter(|c| count(*c) > 0).map(|c| c as u8).unwrap_or(b',')
        }
    }
}

/// Where each row starts (a newline inside quotes doesn't end one).
fn row_starts(b: &[u8]) -> Vec<usize> {
    let mut out = vec![0];
    let mut quoted = false;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'"' => quoted = !quoted,
            b'\n' if !quoted && i + 1 < b.len() => out.push(i + 1),
            _ => {}
        }
    }
    if b.is_empty() {
        out.clear();
    }
    out
}

/// One row's fields ("" quotes and doubled quotes inside them understood).
pub fn fields(line: &[u8], sep: u8) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut quoted = false;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        match c {
            b'"' if quoted && line.get(i + 1) == Some(&b'"') => {
                cur.push(b'"');
                i += 1;
            }
            b'"' if quoted => quoted = false,
            b'"' if cur.iter().all(|c| c.is_ascii_whitespace()) => {
                cur.clear();
                quoted = true;
            }
            c if c == sep && !quoted => out.push(String::from_utf8_lossy(&std::mem::take(&mut cur)).into_owned()),
            c => cur.push(c),
        }
        i += 1;
    }
    out.push(String::from_utf8_lossy(&cur).trim_end_matches(['\r', '\n']).to_string());
    out
}

impl Sheet {
    /// A delimited file (read, or mapped when it's big), or `text` when given (what's in the
    /// editor, unsaved changes and all).
    pub fn delimited(path: &Path, text: Option<String>) -> std::io::Result<Sheet> {
        let text = match text {
            Some(t) => Text::Owned(t.into()),
            None => {
                let f = std::fs::File::open(path)?;
                if f.metadata()?.len() > MAP_OVER {
                    // Safety: read only; a file changed under us shows what's there when drawn.
                    Text::Mapped(Arc::new(unsafe { memmap2::Mmap::map(&f)? }))
                } else {
                    Text::Owned(String::from_utf8_lossy(&std::fs::read(path)?).into())
                }
            }
        };
        let b = text.bytes();
        let first = b.split(|c| *c == b'\n').next().unwrap_or(&[]);
        let sep = separator(path, &String::from_utf8_lossy(first));
        let starts = row_starts(b);
        let mut s = Sheet { name: String::new(), store: Store::Delimited { text, starts, sep }, cols: 0, header: true, widths: Vec::new(), paper: None };
        s.measure();
        Ok(s)
    }

    /// A workbook's sheet, with its looks, column widths and merged cells.
    pub fn spreadsheet(d: crate::office::SheetData, palette: Arc<Vec<Look>>) -> Sheet {
        let mut merges = std::collections::HashMap::new();
        let mut covered = std::collections::HashMap::new();
        for (r0, c0, r1, c1) in d.merges {
            merges.insert((r0, c0), (r1, c1));
            if (r1 - r0 + 1) * (c1 - c0 + 1) > 20_000 {
                continue;
            }
            for r in r0..=r1 {
                for c in c0..=c1 {
                    if (r, c) != (r0, c0) {
                        covered.insert((r, c), (r0, c0));
                    }
                }
            }
        }
        let mut s = Sheet {
            name: d.name,
            store: Store::Cells(d.rows),
            cols: 0,
            header: false,
            widths: Vec::new(),
            paper: Some(Paper { looks: d.looks, palette, merges, covered, row_h: d.row_h.unwrap_or(15.).clamp(8., 60.) }),
        };
        s.measure();
        // Excel's widths (characters) where the sheet sets them; its default otherwise.
        // Empty columns past the data too, like a sheet (at least A to Z).
        let n = s.cols.max(d.widths.len().min(200)).max(26);
        s.cols = n;
        s.widths = (0..s.cols).map(|c| d.widths.get(c).copied().flatten().map(|w| (w * 7. + 5.).round()).unwrap_or(64.)).collect();
        s
    }

    fn look(&self, r: usize, c: usize) -> Option<&Look> {
        let p = self.paper.as_ref()?;
        let (r, c) = p.covered.get(&(r, c)).copied().unwrap_or((r, c));
        let ix = *p.looks.get(r)?.get(c)? as usize;
        p.palette.get(ix).filter(|l| **l != Look::default())
    }

    pub fn rows(&self) -> usize {
        match &self.store {
            Store::Delimited { starts, .. } => starts.len(),
            Store::Cells(r) => r.len(),
        }
    }

    pub fn row(&self, i: usize) -> Vec<String> {
        match &self.store {
            Store::Delimited { text, starts, sep } => {
                let b = text.bytes();
                let Some(&a) = starts.get(i) else { return Vec::new() };
                let e = starts.get(i + 1).copied().unwrap_or(b.len());
                fields(&b[a..e], *sep)
            }
            Store::Cells(r) => r.get(i).cloned().unwrap_or_default(),
        }
    }

    /// Column count and widths from the first rows (a few hundred: enough, and quick).
    fn measure(&mut self) {
        let n = self.rows().min(300);
        let mut widths: Vec<usize> = Vec::new();
        for i in 0..n {
            for (c, f) in self.row(i).iter().enumerate() {
                let w = f.chars().count().min(48);
                if c >= widths.len() {
                    widths.push(w);
                } else {
                    widths[c] = widths[c].max(w);
                }
            }
        }
        self.cols = widths.len().max(1);
        self.widths = widths.into_iter().map(|w| (w as f32 * 7.6 + 22.).clamp(48., 380.)).collect();
        self.widths.resize(self.cols, 60.);
    }

    #[cfg(test)]
    pub fn cols(&self) -> usize {
        self.cols
    }
}

/// A spreadsheet column's letters: 0 → A, 26 → AA.
pub fn column_name(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

pub struct TableView {
    pub sheets: Vec<Sheet>,
    pub current: usize,
    list: UniformListScrollHandle,
    pub zoom: f32,
    /// Columns in colors (CSV, like the editor shows them).
    rainbow: bool,
    pub area: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// The selected cell (row, column), as in a spreadsheet.
    pub sel: Option<(usize, usize)>,
    /// A column being resized: (column, where the drag started, its width then).
    resizing: Option<(usize, Pixels, f32)>,
    focus: gpui::FocusHandle,
    /// Scrolled sideways (points), and the most it can be.
    hx: f32,
    hx_max: f32,
    /// A gesture's direction, decided when it starts: true sideways, false up and down.
    sideways: Option<bool>,
    last_scroll: Option<std::time::Instant>,
}

impl TableView {
    pub fn new(sheets: Vec<Sheet>, rainbow: bool, cx: &mut Context<Self>) -> TableView {
        TableView { sheets, current: 0, list: UniformListScrollHandle::new(), zoom: 1., rainbow, area: Default::default(), sel: None, resizing: None, focus: cx.focus_handle(), hx: 0., hx_max: 0., sideways: None, last_scroll: None }
    }

    #[cfg(test)]
    pub fn width(&self, c: usize) -> f32 {
        self.sheets[self.current].widths[c]
    }

    /// The text of a cell, as shown (whole).
    #[cfg(test)]
    pub fn cell_text(&self, r: usize, c: usize) -> String {
        let Some(s) = self.sheets.get(self.current) else { return String::new() };
        s.row(r).get(c).map(|t| t.trim_start_matches(crate::office::NUMBER).to_string()).unwrap_or_default()
    }

    fn select(&mut self, r: usize, c: usize, window: &mut Window, cx: &mut Context<Self>) {
        // A merged range selects as its first cell.
        let (r, c) = self.sheets.get(self.current).and_then(|s| s.paper.as_ref()).and_then(|p| p.covered.get(&(r, c)).copied()).unwrap_or((r, c));
        self.sel = Some((r, c));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Arrow keys move the selection, the view follows it down.
    fn key(&mut self, ev: &gpui::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((r, c)) = self.sel else { return };
        let Some(s) = self.sheets.get(self.current) else { return };
        let header = s.header as usize;
        let last_row = if s.paper.is_some() { s.rows().max(1) + 29 } else { s.rows().saturating_sub(1).max(header) };
        let (nr, nc) = match ev.keystroke.key.as_str() {
            "up" => (r.saturating_sub(1).max(header), c),
            "down" => ((r + 1).min(last_row), c),
            "left" => (r, c.saturating_sub(1)),
            "right" | "tab" => (r, (c + 1).min(s.cols.saturating_sub(1))),
            "escape" => {
                self.sel = None;
                cx.notify();
                return;
            }
            _ => return,
        };
        // Past a merged range it covers, to the next cell.
        let (nr, nc) = s.paper.as_ref().and_then(|p| p.covered.get(&(nr, nc)).copied()).filter(|o| *o != (r, c)).unwrap_or((nr, nc));
        self.sel = Some((nr, nc));
        self.list.scroll_to_item(nr - header, gpui::ScrollStrategy::Top);
        cx.stop_propagation();
        cx.notify();
    }

    /// Replaces the content (the file changed), keeping the sheet and the scroll.
    pub fn replace(&mut self, sheets: Vec<Sheet>, cx: &mut Context<Self>) {
        self.sheets = sheets;
        self.current = self.current.min(self.sheets.len().saturating_sub(1));
        cx.notify();
    }

    /// Pinch: everything grows or shrinks (half to four times).
    pub fn pinch(&mut self, magnify: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom * (1. + magnify)).clamp(0.5, 4.);
        cx.notify();
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(s) = self.sheets.get(self.current) else { return div().into_any_element() };
        if s.paper.is_some() {
            return self.render_paper_row(s, ix, cx);
        }
        let z = self.zoom;
        let r = if s.header { ix + 1 } else { ix };
        let cells = s.row(r);
        div()
            .id(("tbl-row", ix))
            .flex()
            .h(px(22. * z))
            .border_b_1()
            .border_color(theme::border())
            .hover(|st| st.bg(theme::hover()))
            .child(number_cell(r + if s.header { 0 } else { 1 }, z))
            .children((0..s.cols).map(|c| {
                let text = cells.get(c).map(|t| cut(t)).unwrap_or_default();
                let numeric = !text.is_empty() && text.trim().parse::<f64>().is_ok();
                let on = self.sel == Some((r, c));
                let full = cells.get(c).cloned().unwrap_or_default();
                let cw = s.widths[c] * z;
                let long = on && (full.contains('\n') || full.chars().count() as f32 * 12.5 * z * 0.62 > cw - 14. * z);
                div()
                    .relative()
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |t, _, w, cx| t.select(r, c, w, cx)))
                    .when(on, |d| d.bg(theme::selected_focused()))
                    .when(long, |d| {
                        d.child(gpui::deferred(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .w(px(cw))
                                .min_h(px(22. * z))
                                .px(px(7. * z))
                                .py(px(3. * z))
                                .bg(theme::bg_popup())
                                .border_1()
                                .border_color(theme::focus_border())
                                .shadow_md()
                                .whitespace_normal()
                                .text_color(if self.rainbow { crate::highlight::column_color(c) } else { theme::fg() })
                                .child(full),
                        ).with_priority(1))
                    })
                    .w(px(s.widths[c] * z))
                    .flex_none()
                    .flex()
                    .items_center()
                    .when(numeric, |d| d.justify_end())
                    .px(px(7. * z))
                    .border_r_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(if self.rainbow { crate::highlight::column_color(c) } else { theme::fg() })
                    .child(text)
            }))
            .into_any_element()
    }
}

/// Excel's own colors: the sheet is white whatever the theme, as the colors in it expect.
const PAPER: u32 = 0xFFFFFF;
const GRID: u32 = 0xE1E1E1;
const HEAD: u32 = 0xF3F3F3;
const HEAD_TEXT: u32 = 0x5C5C5C;
const INK: u32 = 0x000000;

/// One device pixel on a Retina screen: the line Excel draws for a thin border.
const HAIR: f32 = 0.5;
/// Excel's selection.
const SELECT: u32 = 0x217346;
const HEAD_SEL: u32 = 0xD3E3D6;

/// Borders of `w` points on these sides (left 1, right 2, top 4, bottom 8).
fn edges<E: Styled>(mut el: E, sides: u8, w: f32) -> E {
    let w: gpui::AbsoluteLength = gpui::px(w).into();
    let b = &mut el.style().border_widths;
    if sides & 1 != 0 {
        b.left = Some(w);
    }
    if sides & 2 != 0 {
        b.right = Some(w);
    }
    if sides & 4 != 0 {
        b.top = Some(w);
    }
    if sides & 8 != 0 {
        b.bottom = Some(w);
    }
    el
}

impl TableView {
    /// How far the sheet is scrolled sideways (row numbers stay put by that much).
    fn hoff(&self) -> f32 {
        self.hx
    }

    /// Scrolling: each gesture goes one way, the way it mostly goes when it starts, so
    /// scrolling down never drifts sideways (Shift + wheel scrolls sideways).
    fn scroll(&mut self, ev: &gpui::ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let d = ev.delta.pixel_delta(window.line_height());
        let (mut dx, dy) = (d.x / gpui::px(1.), d.y / gpui::px(1.));
        if ev.modifiers.shift && dx == 0. {
            dx = dy;
        }
        // A new gesture: the fingers went down, or (a mouse wheel) scrolling paused.
        let paused = self.last_scroll.is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(150));
        self.last_scroll = Some(std::time::Instant::now());
        if matches!(ev.touch_phase, gpui::TouchPhase::Started) || paused {
            self.sideways = None;
        }
        let side = *self.sideways.get_or_insert(dx.abs() > dy.abs() || ev.modifiers.shift);
        if matches!(ev.touch_phase, gpui::TouchPhase::Ended) {
            self.sideways = None;
        }
        if side {
            // Sideways only: the rows don't move up or down meanwhile.
            cx.stop_propagation();
            let hx = (self.hx - dx).clamp(0., self.hx_max.max(0.));
            if hx != self.hx {
                self.hx = hx;
                cx.notify();
            }
        }
    }

    fn render_paper_row(&self, s: &Sheet, r: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let z = self.zoom;
        let p = s.paper.as_ref().unwrap();
        let h = p.row_h * 1.333 * z;
        let cells = s.row(r);
        let mut out: Vec<gpui::AnyElement> = Vec::new();
        let mut c = 0;
        while c < s.cols {
            // A merge's first cell spans its columns; the cells it covers on this row are skipped.
            let span_end = p.merges.get(&(r, c)).map(|(_, c1)| (*c1).min(s.cols - 1)).unwrap_or(c);
            let covered_left = p.covered.get(&(r, c)).is_some_and(|(r0, _)| *r0 == r);
            if covered_left {
                c += 1;
                continue;
            }
            let w: f32 = s.widths[c..=span_end].iter().sum::<f32>() * z;
            let inside_merge = p.covered.contains_key(&(r, c));
            let raw = if inside_merge { "" } else { cells.get(c).map(String::as_str).unwrap_or("") };
            let numeric = raw.starts_with(crate::office::NUMBER);
            let text = cut(raw.trim_start_matches(crate::office::NUMBER));
            let look = s.look(r, c).cloned().unwrap_or_default();
            let align = match look.align {
                1 => 1,
                2 => 2,
                3 => 3,
                _ if numeric => 3,
                _ => 1,
            };
            let size = look.size.unwrap_or(11.) * 1.2 * z;
            let b = look.borders;
            let selected = self.sel == Some((r, c));
            let col = c;
            let mut cell = div()
                .relative()
                .w(px(w))
                .h(px(h))
                .flex_none()
                .flex()
                .items_center()
                .px(px(4. * z))
                .overflow_hidden()
                .whitespace_nowrap()
                .bg(theme::c(look.fill.unwrap_or(PAPER)))
                .text_size(px(size))
                .text_color(theme::c(look.color.unwrap_or(INK)))
                .when(align == 2, |d| d.justify_center())
                .when(align == 3, |d| d.justify_end())
                .when(look.bold, |d| d.font_weight(FontWeight::BOLD))
                .when(look.italic, |d| d.italic())
                .when(look.underline, |d| d.underline())
                .when(look.strike, |d| d.line_through())
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |t, _, w, cx| t.select(r, col, w, cx)));
            // Its own borders where it has them (as heavy as set); hairline gridlines elsewhere.
            cell = if b == 0 {
                edges(cell, 2 | 8, HAIR).border_color(theme::c(GRID))
            } else {
                edges(cell, b, look.border_w.max(HAIR)).border_color(theme::c(look.border_color.unwrap_or(INK)))
            };
            // The selected cell: Excel's green frame. Text too long for it opens in place, as
            // wide as its column, down over the cells below until it all shows.
            let full = raw.trim_start_matches(crate::office::NUMBER);
            let long = selected && (full.contains('\n') || full.chars().count() as f32 * size * 0.55 > w - 8. * z);
            let cell = cell.when(selected && !long, |d| d.child(div().absolute().top_0().left_0().size_full().border_2().border_color(theme::c(SELECT))));
            let cell = cell.when(long, |d| {
                d.child(gpui::deferred(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(w))
                        .min_h(px(h))
                        .px(px(4. * z))
                        .py(px(3. * z))
                        .bg(theme::c(look.fill.unwrap_or(PAPER)))
                        .border_2()
                        .border_color(theme::c(SELECT))
                        .shadow_md()
                        .text_size(px(size))
                        .text_color(theme::c(look.color.unwrap_or(INK)))
                        .when(look.bold, |d| d.font_weight(FontWeight::BOLD))
                        .when(look.italic, |d| d.italic())
                        .whitespace_normal()
                        .child(full.to_string()),
                ).with_priority(1))
            });
            out.push(cell.child(text).into_any_element());
            c = span_end + 1;
        }
        let row_on = self.sel.is_some_and(|(sr, _)| sr == r);
        div()
            .id(("tbl-row", r))
            .relative()
            .flex()
            .h(px(h))
            .bg(theme::c(PAPER))
            .child(div().w(px(46. * z)).flex_none())
            .children(out)
            // The row's number, pinned at the left however far the sheet scrolls sideways.
            .child(row_number(r + 1, h, z, row_on).absolute().top_0().left(px(self.hoff())))
            .into_any_element()
    }
}

fn row_number(n: usize, h: f32, z: f32, on: bool) -> gpui::Div {
    edges(div().w(px(46. * z)).h(px(h)).flex().items_center().justify_center(), 2 | 8, HAIR)
        .border_color(theme::c(0xC8C8C8))
        .bg(theme::c(if on { HEAD_SEL } else { HEAD }))
        .text_size(px(11. * z))
        .text_color(theme::c(if on { SELECT } else { HEAD_TEXT }))
        .when(on, |d| d.font_weight(FontWeight::BOLD))
        .child(n.to_string())
}

fn cut(t: &str) -> String {
    if t.chars().count() <= CELL_MAX { t.replace(['\n', '\r'], " ") } else { format!("{}…", t.chars().take(CELL_MAX).collect::<String>().replace(['\n', '\r'], " ")) }
}

fn number_cell(n: usize, z: f32) -> gpui::Div {
    div().w(px(52. * z)).flex_none().flex().items_center().justify_end().pr(px(8. * z)).border_r_1().border_color(theme::border()).bg(theme::bg_side()).text_color(theme::fg_muted()).child(n.to_string())
}

impl Render for TableView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(s) = self.sheets.get(self.current) else {
            return div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child("Empty");
        };
        let z = self.zoom;
        let paper = s.paper.is_some();
        let total_w: f32 = (s.widths.iter().sum::<f32>() + if paper { 46. } else { 52. }) * z;
        let head_cells: Vec<String> = if s.header { s.row(0) } else { (0..s.cols).map(column_name).collect() };
        let sel_col = self.sel.map(|(_, c)| c);
        let hoff = self.hoff();
        let paper_header = div()
            .relative()
            .flex()
            .flex_none()
            .h(px(20. * z))
            .bg(theme::c(HEAD))
            .text_size(px(11. * z))
            .text_color(theme::c(HEAD_TEXT))
            .child(div().w(px(46. * z)).h_full().flex_none())
            .children((0..s.cols).map(|c| {
                let on = sel_col == Some(c);
                edges(div().relative().w(px(s.widths[c] * z)).h_full().flex_none().flex().items_center().justify_center(), 2 | 8, HAIR)
                    .border_color(theme::c(0xC8C8C8))
                    .when(on, |d| d.bg(theme::c(HEAD_SEL)).text_color(theme::c(SELECT)).font_weight(FontWeight::BOLD))
                    .overflow_hidden()
                    .child(column_name(c))
                    // Drag its right edge to make the column wider or narrower.
                    .child(
                        div()
                            .id(("tbl-resize", c))
                            .debug_selector(move || format!("tbl-resize-{c}"))
                            .absolute()
                            .top_0()
                            .right_0()
                            .w(px(8.))
                            .h_full()
                            .cursor_col_resize()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |t, ev: &gpui::MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    let w = t.sheets.get(t.current).and_then(|s| s.widths.get(c).copied()).unwrap_or(64.);
                                    t.resizing = Some((c, ev.position.x, w));
                                }),
                            ),
                    )
            }))
            .child(edges(div().absolute().top_0().left(px(hoff)).w(px(46. * z)).h_full().bg(theme::c(HEAD)), 2 | 8, HAIR).border_color(theme::c(0xC8C8C8)));
        let header = div()
            .flex()
            .flex_none()
            .h(px(26. * z))
            .border_b_1()
            .border_color(theme::border_input())
            .bg(theme::bg_side())
            .child(number_cell(if s.header { 0 } else { 0 }, z).text_color(gpui::transparent_black()))
            .children((0..s.cols).map(|c| {
                div()
                    .w(px(s.widths[c] * z))
                    .flex_none()
                    .flex()
                    .items_center()
                    .when(!s.header, |d| d.justify_center())
                    .px(px(7. * z))
                    .border_r_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if s.header && self.rainbow { crate::highlight::column_color(c) } else if s.header { theme::fg_bright() } else { theme::fg_muted() })
                    .child(head_cells.get(c).map(|t| cut(t)).unwrap_or_default())
            }));
        let rows = s.rows().saturating_sub(s.header as usize);
        // A spreadsheet goes on past its last row, empty, like Excel's does.
        let shown_rows = if paper { (rows + 30).max(100) } else { rows };
        let body = uniform_list("tbl-rows", shown_rows, cx.processor(|t: &mut TableView, range: Range<usize>, _, cx| range.map(|ix| t.render_row(ix, cx)).collect::<Vec<_>>())).track_scroll(&self.list).flex_1();
        let _ = &mut *cx;
        let tabs = (self.sheets.len() > 1).then(|| {
            div().flex().flex_none().gap(px(2.)).px(px(6.)).h(px(28.)).items_center().border_t_1().border_color(theme::border()).bg(theme::bg_side()).children(self.sheets.iter().enumerate().map(|(i, sh)| {
                let on = i == self.current;
                div()
                    .id(SharedString::from(format!("tbl-sheet-{i}")))
                    .debug_selector(move || format!("tbl-sheet-{i}"))
                    .px(px(10.))
                    .py(px(2.))
                    .rounded(px(3.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected_focused()).text_color(theme::fg_bright()))
                    .when(!on, |d| d.text_color(theme::fg_dim()).hover(|st| st.bg(theme::hover())))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.current = i;
                        cx.notify();
                    }))
                    .child(sh.name.clone())
            }))
        });
        let area = self.area.clone();
        let me = cx.entity().downgrade();
        // At least as wide as the pane; scrolled sideways no further than its edge.
        let pane_w = self.area.get().size.width / gpui::px(1.);
        let total_w = total_w.max(pane_w);
        self.hx_max = (total_w - pane_w).max(0.);
        self.hx = self.hx.min(self.hx_max);
        let hx = self.hx;
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|t, ev: &gpui::MouseMoveEvent, _, cx| {
                if let Some((c, x0, w0)) = t.resizing {
                    if !ev.dragging() {
                        t.resizing = None;
                        return;
                    }
                    let z = t.zoom;
                    if let Some(w) = t.sheets.get_mut(t.current).and_then(|s| s.widths.get_mut(c)) {
                        *w = (w0 + (ev.position.x - x0) / gpui::px(1.) / z).max(16.);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|t, _, _, _| t.resizing = None))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("tbl-hscroll")
                    .debug_selector(|| "table".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        gpui::canvas(
                            move |b, window, _| {
                                if area.get() != b {
                                    area.set(b);
                                    window.refresh();
                                }
                                b
                            },
                            // Scrolling is seen here first (before the rows' list), so each gesture
                            // goes only the way it's going: up and down, or sideways.
                            move |_, b, window, _| {
                                let me = me.clone();
                                window.on_mouse_event(move |ev: &gpui::ScrollEvent, phase, window, cx| {
                                    if phase == gpui::DispatchPhase::Capture && b.contains(&ev.position) {
                                        me.update(cx, |t, cx| t.scroll(ev, window, cx)).ok();
                                    }
                                });
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .child(if paper {
                        // A spreadsheet: Excel's look, its own font.
                        div().absolute().top_0().left(px(-hx)).min_w(px(total_w)).w(px(total_w)).h_full().flex().flex_col().bg(theme::c(PAPER)).font_family(".SystemUIFont").child(paper_header).child(body)
                    } else {
                        div().absolute().top_0().left(px(-hx)).min_w(px(total_w)).w(px(total_w)).h_full().flex().flex_col().font_family(theme::MONO_FONT).text_size(px(12.5 * z)).child(header).child(body)
                    }),
            )
            .children(tabs)
            .child(
                div()
                    .flex_none()
                    .h(px(24.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(px(11.))
                    .text_color(theme::fg_dim())
                    .child(format!("{} rows × {} columns", group(rows), s.cols))
                    .when((z - 1.).abs() > 0.01, |d| d.child(format!("{}%", (z * 100.).round()))),
            )
    }
}

fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimited_rows_and_fields() {
        assert_eq!(fields(b"a,\"b, c\",\"say \"\"hi\"\"\",\r\n", b','), ["a", "b, c", "say \"hi\"", ""]);
        let text = "name,note\nAda,\"two\nlines\"\nLinus,x\n";
        let starts = row_starts(text.as_bytes());
        assert_eq!(starts.len(), 3, "the quoted newline doesn't end the row");
        assert_eq!(fields(&text.as_bytes()[starts[1]..starts[2]], b','), ["Ada", "two\nlines"]);
        assert_eq!(separator(Path::new("x.csv"), "a;b;c"), b';');
        assert_eq!(separator(Path::new("x.tsv"), "a,b"), b'\t');
        assert_eq!((column_name(0), column_name(25), column_name(26), column_name(701)), ("A".into(), "Z".into(), "AA".into(), "ZZ".into()));
    }
}
