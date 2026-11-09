//! Word (.docx) and Excel (.xlsx) files, readable: a document becomes Markdown (headings,
//! bold and italic, lists, tables) shown by the Markdown preview; a workbook becomes tables,
//! one per sheet. Both are ZIP files of XML: only the parts needed are read, as they stream.

use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Starts a cell's text that came from a number.
pub const NUMBER: char = '\u{2}';

/// The most rows read from one sheet (the rest are left, and said so).
pub const MAX_ROWS: usize = 100_000;

struct Entry {
    method: u16,
    size: u32,
    offset: u32,
}

/// A ZIP file's directory; entries are read when asked for.
pub struct Zip {
    file: std::fs::File,
    entries: HashMap<String, Entry>,
}

fn le16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl Zip {
    pub fn open(path: &Path) -> Result<Zip, String> {
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        // The end of the directory is in the last 64 KB (after a comment, maybe).
        let tail = len.min(66_000);
        file.seek(SeekFrom::Start(len - tail)).map_err(|e| e.to_string())?;
        let mut buf = vec![0; tail as usize];
        file.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let eocd = (0..buf.len().saturating_sub(21)).rev().find(|&i| buf[i..i + 4] == [0x50, 0x4b, 5, 6]).ok_or("not a ZIP file (is it the old binary format?)")?;
        let count = le16(&buf, eocd + 10) as usize;
        let dir_size = le32(&buf, eocd + 12) as usize;
        let dir_at = le32(&buf, eocd + 16) as u64;
        file.seek(SeekFrom::Start(dir_at)).map_err(|e| e.to_string())?;
        let mut dir = vec![0; dir_size];
        file.read_exact(&mut dir).map_err(|e| e.to_string())?;
        let mut entries = HashMap::new();
        let mut i = 0;
        for _ in 0..count {
            if i + 46 > dir.len() || dir[i..i + 4] != [0x50, 0x4b, 1, 2] {
                break;
            }
            let method = le16(&dir, i + 10);
            let size = le32(&dir, i + 20);
            let (n, x, c) = (le16(&dir, i + 28) as usize, le16(&dir, i + 30) as usize, le16(&dir, i + 32) as usize);
            let offset = le32(&dir, i + 42);
            let name = String::from_utf8_lossy(&dir[i + 46..i + 46 + n]).to_string();
            entries.insert(name, Entry { method, size, offset });
            i += 46 + n + x + c;
        }
        Ok(Zip { file, entries })
    }

    pub fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        let e = self.entries.get(name)?;
        let (method, size, offset) = (e.method, e.size as usize, e.offset as u64);
        let mut head = [0u8; 30];
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        self.file.read_exact(&mut head).ok()?;
        let skip = le16(&head, 26) as u64 + le16(&head, 28) as u64;
        self.file.seek(SeekFrom::Current(skip as i64)).ok()?;
        let mut raw = vec![0; size];
        self.file.read_exact(&mut raw).ok()?;
        match method {
            0 => Some(raw),
            8 => miniz_oxide::inflate::decompress_to_vec(&raw).ok(),
            _ => None,
        }
    }
}

pub(crate) fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|c| *c == b':').next().unwrap_or(name)
}

pub(crate) fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| local(a.key.as_ref()) == key).map(|a| unescape(&a.value))
}

/// An entity (`&amp;`, `&#233;`…) as its text: quick-xml gives them apart from the text.
pub(crate) fn entity(g: &quick_xml::events::BytesRef) -> String {
    if let Ok(Some(c)) = g.resolve_char_ref() {
        return c.to_string();
    }
    let name: &[u8] = g; match name {
        b"amp" => "&",
        b"lt" => "<",
        b"gt" => ">",
        b"quot" => "\"",
        b"apos" => "'",
        _ => "",
    }
    .to_string()
}

pub(crate) fn unescape(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    quick_xml::escape::unescape(&s).map(|c| c.into_owned()).unwrap_or_else(|_| s.into_owned())
}

// ---------------------------------------------------------------- Excel

/// "B12" → (11, 1): zero-based row and column.
fn cell_ref(r: &str) -> Option<(usize, usize)> {
    let letters: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &r[letters.len()..];
    let col = letters.bytes().fold(0usize, |a, b| a * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize).checked_sub(1)?;
    Some((digits.parse::<usize>().ok()?.checked_sub(1)?, col))
}

/// Days since 1899-12-30 (Excel's dates) → "2026-03-14" (with the time when there is one).
fn excel_date(v: f64) -> String {
    let days = v.floor() as i64;
    let secs = ((v - v.floor()) * 86_400.).round() as i64;
    let (y, m, d) = crate::api::vars::civil(days - 25_569);
    if secs == 0 { format!("{y:04}-{m:02}-{d:02}") } else { format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs / 60 % 60) }
}

/// Runs a reader so that a file it trips over is an error, never a crash.
pub(crate) fn guarded<T>(what: impl FnOnce() -> Result<T, String> + std::panic::UnwindSafe) -> Result<T, String> {
    std::panic::catch_unwind(what).unwrap_or_else(|_| Err("this file has something this reader doesn't understand".into()))
}

/// The sheets of a workbook: (name, rows), and whether some were cut at MAX_ROWS.
pub fn xlsx(path: &Path) -> Result<(Vec<SheetData>, bool, Vec<Look>), String> {
    let path = path.to_path_buf();
    guarded(move || read_xlsx(&path))
}

fn read_xlsx(path: &Path) -> Result<(Vec<SheetData>, bool, Vec<Look>), String> {
    let mut z = Zip::open(path)?;
    // Shared strings: most text lives here, cells point to it.
    let mut shared: Vec<String> = Vec::new();
    if let Some(x) = z.read("xl/sharedStrings.xml") {
        let mut r = Reader::from_reader(x.as_slice());
        let (mut cur, mut in_t, mut in_si) = (String::new(), false, false);
        let mut buf = Vec::new();
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                    b"si" => {
                        in_si = true;
                        cur.clear();
                    }
                    b"t" => in_t = true,
                    _ => {}
                },
                Ok(Event::Text(t)) if in_t && in_si => cur.push_str(&unescape(&t)),
                Ok(Event::GeneralRef(g)) if in_t && in_si => cur.push_str(&entity(&g)),
                Ok(Event::End(e)) => match local(e.name().as_ref()) {
                    b"t" => in_t = false,
                    b"si" => {
                        shared.push(std::mem::take(&mut cur));
                        in_si = false;
                    }
                    _ => {}
                },
                Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"si" => shared.push(String::new()),
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    // How each cell style looks, and how it shows numbers.
    let theme = z.read("xl/theme/theme1.xml").map(|x| theme_colors(&x)).unwrap_or_default();
    let styles = z.read("xl/styles.xml").map(|x| read_styles(&x, &theme)).unwrap_or_default();
    // Sheet names, in order, and their files.
    let rels: HashMap<String, String> = z
        .read("xl/_rels/workbook.xml.rels")
        .map(|x| {
            let mut out = HashMap::new();
            let mut r = Reader::from_reader(x.as_slice());
            let mut buf = Vec::new();
            loop {
                match r.read_event_into(&mut buf) {
                    Ok(Event::Start(e) | Event::Empty(e)) if local(e.name().as_ref()) == b"Relationship" => {
                        if let (Some(id), Some(t)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                            let t = t.trim_start_matches('/');
                            out.insert(id, if t.starts_with("xl/") { t.to_string() } else { format!("xl/{t}") });
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break,
                    _ => {}
                }
                buf.clear();
            }
            out
        })
        .unwrap_or_default();
    let mut sheets: Vec<(String, String)> = Vec::new();
    if let Some(x) = z.read("xl/workbook.xml") {
        let mut r = Reader::from_reader(x.as_slice());
        let mut buf = Vec::new();
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e) | Event::Empty(e)) if local(e.name().as_ref()) == b"sheet" => {
                    let name = attr(&e, b"name").unwrap_or_default();
                    if let Some(file) = attr(&e, b"id").and_then(|id| rels.get(&id).cloned()) {
                        sheets.push((name, file));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    if sheets.is_empty() {
        return Err("no sheets in this workbook".into());
    }
    let mut out = Vec::new();
    let mut cut = false;
    for (name, file) in sheets {
        let Some(x) = z.read(&file) else { continue };
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut looks: Vec<Vec<u32>> = Vec::new();
        let mut widths: Vec<Option<f32>> = Vec::new();
        let mut merges: Vec<(usize, usize, usize, usize)> = Vec::new();
        let mut row_h: Option<f32> = None;
        let mut r = Reader::from_reader(x.as_slice());
        let mut buf = Vec::new();
        let (mut at, mut kind, mut style, mut val, mut in_v) = ((0usize, 0usize), String::new(), 0usize, String::new(), false);
        let mut next_row = 0usize;
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                    // A styled cell with nothing in it (a colored or bordered blank).
                    b"c" => {
                        if let (Some((row, col)), Some(st)) = (attr(&e, b"r").and_then(|c| cell_ref(&c)), attr(&e, b"s").and_then(|s| s.parse::<u32>().ok()))
                            && row < MAX_ROWS
                            && col < 16_384
                            && styles.xfs.get(st as usize).is_some_and(|x| x.look.visible())
                        {
                            put(&mut rows, &mut looks, row, col, String::new(), st);
                        }
                    }
                    b"col" => {
                        let (min, max) = (attr(&e, b"min").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1), attr(&e, b"max").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1));
                        let hidden = attr(&e, b"hidden").is_some_and(|v| v == "1" || v == "true");
                        if let Some(w) = attr(&e, b"width").and_then(|v| v.parse::<f32>().ok()) {
                            for c in (min - 1)..max.min(1024) {
                                if widths.len() <= c {
                                    widths.resize(c + 1, None);
                                }
                                widths[c] = Some(if hidden { 0. } else { w });
                            }
                        }
                    }
                    b"mergeCell" => {
                        if let Some((a, b)) = attr(&e, b"ref").and_then(|r| r.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())))
                            && let (Some(a), Some(b)) = (cell_ref(&a), cell_ref(&b))
                        {
                            merges.push((a.0, a.1, b.0, b.1));
                        }
                    }
                    b"sheetFormatPr" => row_h = attr(&e, b"defaultRowHeight").and_then(|v| v.parse().ok()),
                    _ => {}
                },
                Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                    b"row" => {
                        next_row = attr(&e, b"r").and_then(|n| n.parse::<usize>().ok()).map(|n| n - 1).unwrap_or(next_row);
                    }
                    b"c" => {
                        at = attr(&e, b"r").and_then(|c| cell_ref(&c)).unwrap_or((next_row, rows.get(next_row).map(|r| r.len()).unwrap_or(0)));
                        kind = attr(&e, b"t").unwrap_or_default();
                        style = attr(&e, b"s").and_then(|s| s.parse().ok()).unwrap_or(0);
                        val.clear();
                    }
                    b"v" | b"t" => in_v = true,
                    _ => {}
                },
                Ok(Event::Text(t)) if in_v => val.push_str(&unescape(&t)),
                Ok(Event::GeneralRef(g)) if in_v => val.push_str(&entity(&g)),
                Ok(Event::End(e)) => match local(e.name().as_ref()) {
                    b"v" | b"t" => in_v = false,
                    b"c" => {
                        let (row, col) = at;
                        if row >= MAX_ROWS {
                            cut = true;
                            continue;
                        }
                        let shown = match kind.as_str() {
                            "s" => val.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                            "b" => if val.trim() == "1" { "TRUE".into() } else { "FALSE".into() },
                            "e" | "str" | "inlineStr" => val.clone(),
                            _ => match val.trim().parse::<f64>() {
                                // Marked as a number (it sits on the right, like Excel's).
                                Ok(n) => format!("{NUMBER}{}", styles.xfs.get(style).map(|x| format_number(n, &x.format)).unwrap_or_else(|| format_number(n, "General"))),
                                Err(_) => val.clone(),
                            },
                        };
                        let look = style as u32;
                        let styled = styles.xfs.get(style).is_some_and(|x| x.look.visible());
                        if (!shown.is_empty() || styled) && col < 16_384 {
                            put(&mut rows, &mut looks, row, col, shown, look);
                        }
                    }
                    b"row" => next_row += 1,
                    _ => {}
                },
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        drop(x);
        let looks_used = looks.iter().any(|r| r.iter().any(|l| *l != 0));
        out.push(SheetData {
            name,
            rows,
            looks: if looks_used { looks } else { Vec::new() },
            widths,
            merges,
            row_h,
        });
    }
    Ok((out, cut, styles.xfs.into_iter().map(|x| x.look).collect()))
}

/// One cell's text and look, growing the grid as needed.
fn put(rows: &mut Vec<Vec<String>>, looks: &mut Vec<Vec<u32>>, row: usize, col: usize, text: String, look: u32) {
    if rows.len() <= row {
        rows.resize(row + 1, Vec::new());
        looks.resize(row + 1, Vec::new());
    }
    if rows[row].len() <= col {
        rows[row].resize(col + 1, String::new());
    }
    if looks[row].len() <= col {
        looks[row].resize(col + 1, 0);
    }
    rows[row][col] = text;
    looks[row][col] = look;
}

/// A sheet as read: its cells' text and looks (indexes into the workbook's), column widths
/// (Excel's character units), merged ranges (first row, col, last row, col), row height (points).
pub struct SheetData {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    pub looks: Vec<Vec<u32>>,
    pub widths: Vec<Option<f32>>,
    pub merges: Vec<(usize, usize, usize, usize)>,
    pub row_h: Option<f32>,
}

pub use crate::table::Look;

struct Xf {
    look: Look,
    format: String,
}

#[derive(Default)]
struct Styles {
    xfs: Vec<Xf>,
}

/// The theme's colors, in the order cells name them: light 1, dark 1, light 2, dark 2,
/// accents 1–6, link, followed link.
pub(crate) fn theme_colors(x: &[u8]) -> Vec<u32> {
    let mut found: Vec<(String, u32)> = Vec::new();
    let mut r = Reader::from_reader(x);
    let mut buf = Vec::new();
    let (mut in_scheme, mut slot) = (false, String::new());
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                b"clrScheme" => in_scheme = true,
                n if in_scheme => slot = String::from_utf8_lossy(n).into_owned(),
                _ => {}
            },
            Ok(Event::Empty(e)) if in_scheme => {
                let v = match local(e.name().as_ref()) {
                    b"srgbClr" => attr(&e, b"val"),
                    b"sysClr" => attr(&e, b"lastClr"),
                    _ => None,
                };
                if let Some(v) = v.and_then(|v| u32::from_str_radix(&v, 16).ok()) {
                    found.push((slot.clone(), v));
                }
            }
            Ok(Event::End(e)) if local(e.name().as_ref()) == b"clrScheme" => break,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let get = |n: &str| found.iter().find(|(s, _)| s == n).map(|(_, v)| *v).unwrap_or(0);
    ["lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"].iter().map(|n| get(n)).collect()
}

/// Excel's old 64-color palette (cells that name a color by number).
const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000, 0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF, 0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF, 0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399, 0x333333,
];

/// A color element (`<color rgb=… / theme=… tint=… / indexed=…>`) as RGB.
fn color_of(e: &quick_xml::events::BytesStart, theme: &[u32]) -> Option<u32> {
    let base = if let Some(rgb) = attr(e, b"rgb") {
        let v = u32::from_str_radix(rgb.trim_start_matches('#'), 16).ok()?;
        v & 0xFF_FFFF
    } else if let Some(t) = attr(e, b"theme").and_then(|t| t.parse::<usize>().ok()) {
        *theme.get(t)?
    } else if let Some(i) = attr(e, b"indexed").and_then(|t| t.parse::<usize>().ok()) {
        *INDEXED.get(i)?
    } else {
        return None;
    };
    let tint: f32 = attr(e, b"tint").and_then(|t| t.parse().ok()).unwrap_or(0.);
    if tint == 0. {
        return Some(base);
    }
    let ch = |shift: u32| {
        let c = ((base >> shift) & 0xFF) as f32;
        let c = if tint < 0. { c * (1. + tint) } else { c + (255. - c) * tint };
        (c.round().clamp(0., 255.) as u32) << shift
    };
    Some(ch(16) | ch(8) | ch(0))
}

/// Built-in number formats by id (the ones in use).
fn builtin_format(id: u32) -> &'static str {
    match id {
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        14..=17 | 22 | 45..=47 => "date",
        18..=21 => "time",
        37 | 38 => "#,##0",
        39 | 40 => "#,##0.00",
        49 => "@",
        _ => "General",
    }
}

fn read_styles(x: &[u8], theme: &[u32]) -> Styles {
    #[derive(Default, Clone)]
    struct Font {
        bold: bool,
        italic: bool,
        underline: bool,
        strike: bool,
        color: Option<u32>,
        size: Option<f32>,
    }
    let mut fonts: Vec<Font> = Vec::new();
    let mut fills: Vec<Option<u32>> = Vec::new();
    let mut borders: Vec<(u8, Option<u32>, f32)> = Vec::new();
    let mut formats: HashMap<u32, String> = HashMap::new();
    let mut xfs = Vec::new();
    let mut r = Reader::from_reader(x);
    let mut buf = Vec::new();
    // Where in the file we are.
    let (mut sect, mut font, mut fill, mut border, mut side) = (String::new(), Font::default(), None::<u32>, (0u8, None::<u32>, 0f32), 0u8);
    let mut xf: Option<(Look, String)> = None;
    loop {
        let ev = r.read_event_into(&mut buf);
        let (e, empty) = match &ev {
            Ok(Event::Start(e)) => (Some(e.clone()), false),
            Ok(Event::Empty(e)) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = e {
            let name = local(e.name().as_ref()).to_vec();
            match name.as_slice() {
                b"fonts" | b"fills" | b"borders" | b"cellXfs" | b"cellStyleXfs" | b"dxfs" => sect = String::from_utf8_lossy(&name).into_owned(),
                b"numFmt" => {
                    if let Some(id) = attr(&e, b"numFmtId").and_then(|v| v.parse().ok()) {
                        formats.insert(id, attr(&e, b"formatCode").unwrap_or_default());
                    }
                }
                b"font" if sect == "fonts" => font = Font::default(),
                b"b" if sect == "fonts" => font.bold = attr(&e, b"val").is_none_or(|v| v != "0"),
                b"i" if sect == "fonts" => font.italic = attr(&e, b"val").is_none_or(|v| v != "0"),
                b"u" if sect == "fonts" => font.underline = attr(&e, b"val").is_none_or(|v| v != "none"),
                b"strike" if sect == "fonts" => font.strike = attr(&e, b"val").is_none_or(|v| v != "0"),
                b"sz" if sect == "fonts" => font.size = attr(&e, b"val").and_then(|v| v.parse().ok()),
                b"color" if sect == "fonts" => font.color = color_of(&e, theme),
                b"fill" if sect == "fills" => fill = None,
                b"patternFill" if sect == "fills" => {
                    if attr(&e, b"patternType").is_some_and(|p| p == "none") {
                        fill = None;
                    }
                }
                b"fgColor" if sect == "fills" => fill = color_of(&e, theme),
                b"bgColor" if sect == "fills" && fill.is_none() => fill = color_of(&e, theme).filter(|c| *c != 0),
                b"stop" if sect == "fills" => {}
                b"border" if sect == "borders" => border = (0, None, 0.),
                b"left" | b"start" | b"right" | b"end" | b"top" | b"bottom" if sect == "borders" => {
                    side = match name.as_slice() {
                        b"left" | b"start" => 1,
                        b"right" | b"end" => 2,
                        b"top" => 4,
                        _ => 8,
                    };
                    let style = attr(&e, b"style").unwrap_or_default();
                    if !style.is_empty() && style != "none" {
                        border.0 |= side;
                        // One pixel for thin lines; medium and thick ones are heavier.
                        let w = match style.as_str() {
                            "medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" | "slantDashDot" => 1.,
                            "thick" | "double" => 1.5,
                            _ => 0.5,
                        };
                        border.2 = border.2.max(w);
                    } else {
                        side = 0;
                    }
                }
                b"color" if sect == "borders" && side != 0 => {
                    if border.1.is_none() {
                        border.1 = color_of(&e, theme);
                    }
                }
                b"xf" if sect == "cellXfs" => {
                    let n = |k: &[u8]| attr(&e, k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                    let f = fonts.get(n(b"fontId")).cloned().unwrap_or_default();
                    let (sides, bcolor, bw) = borders.get(n(b"borderId")).copied().unwrap_or((0, None, 0.));
                    let look = Look { bold: f.bold, italic: f.italic, underline: f.underline, strike: f.strike, color: f.color, size: f.size, fill: fills.get(n(b"fillId")).copied().flatten(), borders: sides, border_color: bcolor, border_w: bw, align: 0, wrap: false };
                    let id = n(b"numFmtId") as u32;
                    let format = formats.get(&id).cloned().unwrap_or_else(|| builtin_format(id).to_string());
                    if empty {
                        xfs.push(Xf { look, format });
                    } else {
                        xf = Some((look, format));
                    }
                }
                b"alignment" if sect == "cellXfs" => {
                    if let Some((look, _)) = xf.as_mut() {
                        look.align = match attr(&e, b"horizontal").as_deref() {
                            Some("left") => 1,
                            Some("center") | Some("centerContinuous") => 2,
                            Some("right") => 3,
                            _ => 0,
                        };
                        look.wrap = attr(&e, b"wrapText").is_some_and(|v| v == "1" || v == "true");
                    }
                }
                _ => {}
            }
            if !empty {
                buf.clear();
                continue;
            }
        }
        match ev {
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"font" if sect == "fonts" => fonts.push(std::mem::take(&mut font)),
                b"fill" if sect == "fills" => fills.push(fill.take()),
                b"border" if sect == "borders" => borders.push(std::mem::take(&mut border)),
                b"left" | b"start" | b"right" | b"end" | b"top" | b"bottom" => side = 0,
                b"xf" if sect == "cellXfs" => {
                    if let Some((look, format)) = xf.take() {
                        xfs.push(Xf { look, format });
                    }
                }
                b"fonts" | b"fills" | b"borders" | b"cellXfs" | b"cellStyleXfs" | b"dxfs" => sect.clear(),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Styles { xfs }
}

/// A number as its format shows it: decimals, thousands, percent, currency signs, dates.
pub fn format_number(n: f64, code: &str) -> String {
    let general = |n: f64| {
        if n.fract() == 0. && n.abs() < 1e15 {
            format!("{}", n as i64)
        } else {
            let s = format!("{n:.10}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    };
    // The section for this sign (positive;negative;zero;text).
    let sections: Vec<&str> = code.split(';').collect();
    let (code, n, neg_shown) = match sections.len() {
        _ if n < 0. && sections.len() >= 2 => (sections[1], -n, true),
        _ if n == 0. && sections.len() >= 3 => (sections[2], n, false),
        _ => (sections[0], n, false),
    };
    let lower = code.to_lowercase();
    if lower == "general" || lower.is_empty() || lower == "@" {
        return general(if neg_shown { -n } else { n });
    }
    // Text in quotes, escaped characters and [$€-409] currency blocks are shown as written.
    let mut plain = String::new();
    let mut lits = String::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                    plain.push('\u{1}');
                    lits.push(d);
                }
            }
            '\\' => {
                if let Some(d) = chars.next() {
                    plain.push('\u{1}');
                    lits.push(d);
                }
            }
            '[' => {
                let inner: String = chars.by_ref().take_while(|d| *d != ']').collect();
                if let Some(cur) = inner.strip_prefix('$') {
                    for d in cur.split('-').next().unwrap_or("").chars() {
                        plain.push('\u{1}');
                        lits.push(d);
                    }
                }
            }
            '_' | '*' => {
                chars.next();
            }
            c => plain.push(c),
        }
    }
    let pl = plain.to_lowercase();
    if lower == "date" {
        return excel_date(n);
    }
    if lower == "time" {
        return date_pattern(n, "hh:mm");
    }
    if !pl.contains('0') && !pl.contains('#') && (pl.contains('y') || pl.contains('d') || pl.contains('m') || pl.contains('h') || pl.contains('s')) {
        let mut l = lits.chars();
        let pattern: String = plain.chars().map(|c| if c == '\u{1}' { l.next().unwrap_or(' ') } else { c }).collect();
        return date_pattern(n, &pattern);
    }
    let Some(first) = plain.find(['0', '#', '?']) else {
        // No digits in it: just the text.
        let mut l = lits.chars();
        return plain.chars().map(|c| if c == '\u{1}' { l.next().unwrap_or(' ') } else { c }).collect();
    };
    let last = plain.rfind(['0', '#', '?', ',', '.']).unwrap_or(first);
    let num_part = &plain[first..=last];
    let percent = plain.contains('%');
    let sci = num_part.to_lowercase().contains('e');
    let v = if percent { n * 100. } else { n };
    let decimals = num_part.split_once('.').map(|(_, d)| d.chars().filter(|c| matches!(c, '0' | '#' | '?')).count()).unwrap_or(0);
    let thousands = num_part.split('.').next().unwrap_or("").contains(',');
    let body = if sci {
        format!("{v:.decimals$E}")
    } else {
        let s = format!("{:.*}", decimals, v.abs());
        let (int, frac) = s.split_once('.').map(|(a, b)| (a.to_string(), Some(b.to_string()))).unwrap_or((s.clone(), None));
        let int = if thousands {
            let mut o = String::new();
            for (i, c) in int.chars().enumerate() {
                if i > 0 && (int.len() - i) % 3 == 0 {
                    o.push(',');
                }
                o.push(c);
            }
            o
        } else {
            int
        };
        let sign = if v < 0. && !neg_shown { "-" } else { "" };
        match frac {
            Some(f) => format!("{sign}{int}.{f}"),
            None => format!("{sign}{int}"),
        }
    };
    let mut l = lits.chars();
    let mut fill = |s: &str| s.chars().map(|c| if c == '\u{1}' { l.next().unwrap_or(' ') } else { c }).collect::<String>();
    let before = fill(&plain[..first]);
    let after = fill(&plain[last + 1..]);
    let out = format!("{before}{body}{after}");
    let out = out.replace(['(', ')'], "").trim().to_string();
    if neg_shown && code.contains('(') { format!("({out})") } else if neg_shown && !code.contains('-') { format!("-{out}") } else { out }
}

/// A date (Excel's day number) written as a pattern says: yyyy yy mmmm mmm mm m dd d ddd
/// dddd hh h mm (after hours) ss AM/PM.
fn date_pattern(n: f64, pattern: &str) -> String {
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const DAYS: [&str; 7] = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"];
    let days = n.floor() as i64 - 25_569;
    let (y, m, d) = crate::api::vars::civil(days);
    let secs = ((n - n.floor()) * 86_400.).round() as i64;
    let (hh, mi, ss) = (secs / 3600, secs / 60 % 60, secs % 60);
    let ampm = pattern.to_lowercase().contains("am/pm");
    let p: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut after_hour = false;
    while i < p.len() {
        let c = p[i].to_ascii_lowercase();
        let run = p[i..].iter().take_while(|x| x.to_ascii_lowercase() == c).count();
        match c {
            'y' => out.push_str(&if run <= 2 { format!("{:02}", y % 100) } else { y.to_string() }),
            'd' => out.push_str(&match run {
                1 => d.to_string(),
                2 => format!("{d:02}"),
                3 => DAYS[days.rem_euclid(7) as usize][..3].to_string(),
                _ => DAYS[days.rem_euclid(7) as usize].to_string(),
            }),
            'm' if after_hour || p[i + run..].iter().find(|x| x.is_ascii_alphabetic()).is_some_and(|x| x.to_ascii_lowercase() == 's') => {
                out.push_str(&if run >= 2 { format!("{mi:02}") } else { mi.to_string() });
                after_hour = false;
            }
            'm' => out.push_str(&match run {
                1 => m.to_string(),
                2 => format!("{m:02}"),
                3 => MONTHS[(m - 1) as usize][..3].to_string(),
                _ => MONTHS[(m - 1) as usize].to_string(),
            }),
            'h' => {
                let h = if ampm { (hh + 11) % 12 + 1 } else { hh };
                out.push_str(&if run >= 2 { format!("{h:02}") } else { h.to_string() });
                after_hour = true;
            }
            's' => out.push_str(&format!("{ss:02}")),
            'a' if pattern[pattern.char_indices().nth(i).map(|(b, _)| b).unwrap_or(0)..].to_lowercase().starts_with("am/pm") => {
                out.push_str(if hh < 12 { "AM" } else { "PM" });
                i += 5;
                continue;
            }
            _ => {
                out.extend(std::iter::repeat_n(p[i], run));
            }
        }
        i += run;
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn numbers_show_as_their_format_says() {
        assert_eq!(format_number(1234.5, "#,##0.00"), "1,234.50");
        assert_eq!(format_number(0.256, "0.0%"), "25.6%");
        assert_eq!(format_number(-42.0, "#,##0;(#,##0)"), "(42)");
        assert_eq!(format_number(19.9, "\"$\"#,##0.00"), "$19.90");
        assert_eq!(format_number(5.0, "[$€-407]#,##0.00"), "€5.00");
        assert_eq!(format_number(46095.0, "dd/mm/yyyy"), "14/03/2026");
        assert_eq!(format_number(46095.5, "d mmm yyyy h:mm AM/PM"), "14 Mar 2026 12:00 PM");
        assert_eq!(format_number(3.0, "General"), "3");
    }

    /// A ZIP with these files, stored (and one deflated).
    pub(crate) fn zip(path: &Path, files: &[(&str, &str)]) {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (i, (name, text)) in files.iter().enumerate() {
            let deflate = i % 2 == 1;
            let data = if deflate { miniz_oxide::deflate::compress_to_vec(text.as_bytes(), 6) } else { text.as_bytes().to_vec() };
            let offset = out.len() as u32;
            let method: u16 = if deflate { 8 } else { 0 };
            let mut h = vec![0x50, 0x4b, 3, 4, 20, 0, 0, 0];
            h.extend(method.to_le_bytes());
            h.extend([0u8; 8]);
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((text.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            h.extend(name.as_bytes());
            out.extend(h);
            out.extend(&data);
            let mut d = vec![0x50, 0x4b, 1, 2, 20, 0, 20, 0, 0, 0];
            d.extend(method.to_le_bytes());
            d.extend([0u8; 8]);
            d.extend((data.len() as u32).to_le_bytes());
            d.extend((text.len() as u32).to_le_bytes());
            d.extend((name.len() as u16).to_le_bytes());
            d.extend([0u8; 12]);
            d.extend(offset.to_le_bytes());
            d.extend(name.as_bytes());
            dir.extend(d);
        }
        let at = out.len() as u32;
        out.extend(&dir);
        let mut e = vec![0x50, 0x4b, 5, 6, 0, 0, 0, 0];
        e.extend((files.len() as u16).to_le_bytes());
        e.extend((files.len() as u16).to_le_bytes());
        e.extend((dir.len() as u32).to_le_bytes());
        e.extend(at.to_le_bytes());
        e.extend(0u16.to_le_bytes());
        out.extend(e);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn workbooks_read_as_sheets() {
        let p = std::env::temp_dir().join(format!("kite-{}.xlsx", std::process::id()));
        zip(&p, &[
            ("xl/workbook.xml", r#"<workbook><sheets><sheet name="Orders" sheetId="1" r:id="rId1"/><sheet name="Empty" sheetId="2" r:id="rId2"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#),
            ("xl/sharedStrings.xml", r#"<sst><si><t>Item</t></si><si><r><t>Pen</t></r><r><t xml:space="preserve"> &amp; ink</t></r></si><si><t>When</t></si></sst>"#),
            ("xl/styles.xml", r#"<styleSheet><cellXfs><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>"#),
            ("xl/worksheets/sheet1.xml", r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>2</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><v>2.5</v></c><c r="C2" s="1"><v>46095</v></c><c r="D2" t="b"><v>1</v></c></row></sheetData></worksheet>"#),
            ("xl/worksheets/sheet2.xml", r#"<worksheet><sheetData/></worksheet>"#),
        ]);
        let (sheets, cut, looks) = xlsx(&p).unwrap();
        assert!(!cut);
        assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Orders", "Empty"]);
        assert_eq!(looks.len(), 2);
        assert_eq!(sheets[0].rows, vec![vec!["Item".to_string(), String::new(), "When".into()], vec!["Pen & ink".into(), "\u{2}2.5".into(), "\u{2}2026-03-14".into(), "TRUE".into()]]);
        std::fs::remove_file(p).ok();
    }

}
