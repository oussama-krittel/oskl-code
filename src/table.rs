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

pub struct Sheet {
    pub name: String,
    store: Store,
    cols: usize,
    /// The first row names the columns (CSV); otherwise columns are lettered (A, B…).
    header: bool,
    widths: Vec<f32>,
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
        let mut s = Sheet { name: String::new(), store: Store::Delimited { text, starts, sep }, cols: 0, header: true, widths: Vec::new() };
        s.measure();
        Ok(s)
    }

    pub fn cells(name: String, rows: Vec<Vec<String>>) -> Sheet {
        let mut s = Sheet { name, store: Store::Cells(rows), cols: 0, header: false, widths: Vec::new() };
        s.measure();
        s
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
        let total_w: f32 = (s.widths.iter().sum::<f32>() + 52.) * z;
        let head_cells: Vec<String> = if s.header { s.row(0) } else { (0..s.cols).map(column_name).collect() };
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
        let body = uniform_list("tbl-rows", rows, cx.processor(|t: &mut TableView, range: Range<usize>, _, cx| range.map(|ix| t.render_row(ix, cx)).collect::<Vec<_>>())).track_scroll(self.list.clone()).flex_1();
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
                    .child(div().min_w(px(total_w)).w(px(total_w)).h_full().flex().flex_col().font_family(theme::MONO_FONT).text_size(px(12.5 * z)).child(header).child(body)),
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
