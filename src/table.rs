//! A read-only table: CSV/TSV files (each column in its own color, like the editor shows them)
//! and spreadsheet sheets. Only the rows on screen are parsed and drawn; a CSV is kept once
//! (big files are mapped from disk, not read in) with where each row starts.

use crate::theme;
use crate::ui::px;
use gpui::prelude::*;
use gpui::{App, Bounds, Context, FontWeight, Pixels, ScrollHandle, SharedString, UniformListScrollHandle, Window, div, uniform_list};
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

    pub fn cells(name: String, rows: Vec<Vec<String>>) -> Sheet {
        let mut s = Sheet { name, store: Store::Cells(rows), cols: 0, header: false, widths: Vec::new(), paper: None };
        s.measure();
        s
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
    hscroll: ScrollHandle,
    pub zoom: f32,
    /// Columns in colors (CSV, like the editor shows them).
    rainbow: bool,
    pub area: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
}

impl TableView {
    pub fn new(sheets: Vec<Sheet>, rainbow: bool) -> TableView {
        TableView { sheets, current: 0, list: UniformListScrollHandle::new(), hscroll: ScrollHandle::new(), zoom: 1., rainbow, area: Default::default() }
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

    fn render_row(&self, ix: usize, cx: &App) -> gpui::AnyElement {
        let _ = cx;
        let Some(s) = self.sheets.get(self.current) else { return div().into_any_element() };
        if s.paper.is_some() {
            return self.render_paper_row(s, ix);
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
                div()
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

impl TableView {
    fn render_paper_row(&self, s: &Sheet, r: usize) -> gpui::AnyElement {
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
            let bc = theme::c(look.border_color.unwrap_or(INK));
            let mut cell = div()
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
                .when(look.strike, |d| d.line_through());
            // Its own borders where it has them; light gridlines elsewhere.
            cell = if b == 0 {
                cell.border_r_1().border_b_1().border_color(theme::c(GRID))
            } else {
                // One color per cell: the border's (sides it doesn't set show no line).
                cell.border_color(bc).when(b & 1 != 0, |d| d.border_l_1()).when(b & 2 != 0, |d| d.border_r_1()).when(b & 4 != 0, |d| d.border_t_1()).when(b & 8 != 0, |d| d.border_b_1())
            };
            out.push(cell.child(text).into_any_element());
            c = span_end + 1;
        }
        div()
            .id(("tbl-row", r))
            .flex()
            .h(px(h))
            .bg(theme::c(PAPER))
            .child(div().w(px(46. * z)).h(px(h)).flex_none().flex().items_center().justify_center().border_r_1().border_b_1().border_color(theme::c(0xC8C8C8)).bg(theme::c(HEAD)).text_size(px(11. * z)).text_color(theme::c(HEAD_TEXT)).child((r + 1).to_string()))
            .children(out)
            .into_any_element()
    }
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
        let paper_header = div()
            .flex()
            .flex_none()
            .h(px(20. * z))
            .bg(theme::c(HEAD))
            .text_size(px(11. * z))
            .text_color(theme::c(HEAD_TEXT))
            .child(div().w(px(46. * z)).h_full().flex_none().border_r_1().border_b_1().border_color(theme::c(0xC8C8C8)))
            .children((0..s.cols).map(|c| {
                div().w(px(s.widths[c] * z)).h_full().flex_none().flex().items_center().justify_center().border_r_1().border_b_1().border_color(theme::c(0xC8C8C8)).overflow_hidden().child(column_name(c))
            }));
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
        let body = uniform_list("tbl-rows", shown_rows, cx.processor(|t: &mut TableView, range: Range<usize>, _, cx| range.map(|ix| t.render_row(ix, cx)).collect::<Vec<_>>())).track_scroll(self.list.clone()).flex_1();
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
        div()
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
                    .overflow_x_scroll()
                    .track_scroll(&self.hscroll)
                    .child(gpui::canvas(move |b, _, _| area.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                    .child(if paper {
                        // A spreadsheet: Excel's look, its own font.
                        div().min_w(px(total_w)).w(px(total_w)).h_full().flex().flex_col().bg(theme::c(PAPER)).font_family(".SystemUIFont").child(paper_header).child(body)
                    } else {
                        div().min_w(px(total_w)).w(px(total_w)).h_full().flex().flex_col().font_family(theme::MONO_FONT).text_size(px(12.5 * z)).child(header).child(body)
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
