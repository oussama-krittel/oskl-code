//! Word (.docx) and Excel (.xlsx) files, readable: a document becomes Markdown (headings,
//! bold and italic, lists, tables) shown by the Markdown preview; a workbook becomes tables,
//! one per sheet. Both are ZIP files of XML: only the parts needed are read, as they stream.

use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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

    pub fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name)
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

fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|c| *c == b':').next().unwrap_or(name)
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| local(a.key.as_ref()) == key).map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

/// An entity (`&amp;`, `&#233;`…) as its text: quick-xml gives them apart from the text.
fn entity(g: &quick_xml::events::BytesRef) -> String {
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

fn unescape(raw: &[u8]) -> String {
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
fn guarded<T>(what: impl FnOnce() -> Result<T, String> + std::panic::UnwindSafe) -> Result<T, String> {
    std::panic::catch_unwind(what).unwrap_or_else(|_| Err("this file has something this reader doesn't understand".into()))
}

/// The sheets of a workbook: (name, rows), and whether some were cut at MAX_ROWS.
pub fn xlsx(path: &Path) -> Result<(Vec<(String, Vec<Vec<String>>)>, bool), String> {
    let path = path.to_path_buf();
    guarded(move || read_xlsx(&path))
}

fn read_xlsx(path: &Path) -> Result<(Vec<(String, Vec<Vec<String>>)>, bool), String> {
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
    // Which cell styles are dates (so their numbers show as dates).
    let mut date_styles: Vec<bool> = Vec::new();
    if let Some(x) = z.read("xl/styles.xml") {
        let mut custom: HashMap<String, bool> = HashMap::new();
        let mut r = Reader::from_reader(x.as_slice());
        let mut in_xfs = false;
        let mut buf = Vec::new();
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e) | Event::Empty(e)) => match local(e.name().as_ref()) {
                    b"numFmt" => {
                        let code = attr(&e, b"formatCode").unwrap_or_default().to_lowercase();
                        let code: String = code.split('"').step_by(2).collect();
                        custom.insert(attr(&e, b"numFmtId").unwrap_or_default(), code.contains('y') || code.contains('d') || (code.contains('m') && code.contains('h')));
                    }
                    b"cellXfs" => in_xfs = true,
                    b"xf" if in_xfs => {
                        let id = attr(&e, b"numFmtId").unwrap_or_default();
                        let n: u32 = id.parse().unwrap_or(0);
                        date_styles.push((14..=22).contains(&n) || (45..=47).contains(&n) || custom.get(&id).copied().unwrap_or(false));
                    }
                    _ => {}
                },
                Ok(Event::End(e)) if local(e.name().as_ref()) == b"cellXfs" => in_xfs = false,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
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
        let mut r = Reader::from_reader(x.as_slice());
        let mut buf = Vec::new();
        let (mut at, mut kind, mut style, mut val, mut in_v) = ((0usize, 0usize), String::new(), 0usize, String::new(), false);
        let mut next_row = 0usize;
        loop {
            match r.read_event_into(&mut buf) {
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
                                Ok(n) if date_styles.get(style).copied().unwrap_or(false) => excel_date(n),
                                Ok(n) if n.fract() == 0. && n.abs() < 1e15 => format!("{}", n as i64),
                                Ok(n) => {
                                    let s = format!("{n:.10}");
                                    s.trim_end_matches('0').trim_end_matches('.').to_string()
                                }
                                Err(_) => val.clone(),
                            },
                        };
                        if !shown.is_empty() && col < 16_384 {
                            if rows.len() <= row {
                                rows.resize(row + 1, Vec::new());
                            }
                            let cells = &mut rows[row];
                            if cells.len() <= col {
                                cells.resize(col + 1, String::new());
                            }
                            cells[col] = shown;
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
        out.push((name, rows));
    }
    Ok((out, cut))
}

// ---------------------------------------------------------------- Word

/// A Word document as Markdown.
pub fn docx_markdown(path: &Path) -> Result<String, String> {
    let path = path.to_path_buf();
    guarded(move || read_docx(&path))
}

fn read_docx(path: &Path) -> Result<String, String> {
    let mut z = Zip::open(path)?;
    let x = z.read("word/document.xml").ok_or("no document in this file (is it the old .doc format?)")?;
    // Numbered lists vs bullets: numbering definitions say which.
    let mut ordered: HashMap<String, bool> = HashMap::new();
    if let Some(n) = z.read("word/numbering.xml") {
        let mut abs_ordered: HashMap<String, bool> = HashMap::new();
        let mut r = Reader::from_reader(n.as_slice());
        let mut buf = Vec::new();
        let (mut cur_abs, mut first_lvl, mut cur_num) = (String::new(), false, String::new());
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e) | Event::Empty(e)) => match local(e.name().as_ref()) {
                    b"abstractNum" => {
                        cur_abs = attr(&e, b"abstractNumId").unwrap_or_default();
                        first_lvl = true;
                    }
                    b"numFmt" if first_lvl => {
                        abs_ordered.insert(cur_abs.clone(), attr(&e, b"val").is_some_and(|v| v != "bullet" && v != "none"));
                        first_lvl = false;
                    }
                    b"num" => cur_num = attr(&e, b"numId").unwrap_or_default(),
                    b"abstractNumId" => {
                        let a = attr(&e, b"val").unwrap_or_default();
                        ordered.insert(cur_num.clone(), abs_ordered.get(&a).copied().unwrap_or(false));
                    }
                    _ => {}
                },
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    let mut md = String::new();
    let mut r = Reader::from_reader(x.as_slice());
    let mut buf = Vec::new();
    // Paragraph state.
    let (mut para, mut style, mut level, mut num_id) = (String::new(), String::new(), None::<usize>, String::new());
    let (mut bold, mut italic, mut in_text) = (false, false, false);
    let mut run = String::new();
    // Tables: rows of cells; nested tables flatten into their cell.
    let mut tables: Vec<Vec<Vec<String>>> = Vec::new();
    let mut counters: HashMap<(String, usize), usize> = HashMap::new();
    let flush_run = |para: &mut String, run: &mut String, bold: bool, italic: bool| {
        if run.is_empty() {
            return;
        }
        let t = std::mem::take(run);
        // Only spaces: nothing to mark (and no inside to cut out).
        if t.trim().is_empty() {
            para.push_str(&t);
            return;
        }
        let (lead, body, trail) = {
            let start = t.len() - t.trim_start().len();
            let end = t.trim_end().len();
            (&t[..start], &t[start..end], &t[end..])
        };
        if body.is_empty() {
            para.push_str(&t);
            return;
        }
        let mark = match (bold, italic) {
            (true, true) => "***",
            (true, false) => "**",
            (false, true) => "*",
            _ => "",
        };
        para.push_str(lead);
        para.push_str(mark);
        para.push_str(&body.replace('*', "\\*"));
        para.push_str(mark);
        para.push_str(trail);
    };
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                b"p" => {
                    para.clear();
                    style.clear();
                    level = None;
                    num_id.clear();
                }
                b"r" => {
                    bold = false;
                    italic = false;
                    run.clear();
                }
                b"t" => in_text = true,
                b"tbl" => tables.push(Vec::new()),
                b"tr" => {
                    if let Some(t) = tables.last_mut() {
                        t.push(Vec::new());
                    }
                }
                b"tc" => {
                    if let Some(row) = tables.last_mut().and_then(|t| t.last_mut()) {
                        row.push(String::new());
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"pStyle" => style = attr(&e, b"val").unwrap_or_default(),
                b"ilvl" => level = Some(attr(&e, b"val").and_then(|v| v.parse().ok()).unwrap_or(0)),
                b"numId" => {
                    num_id = attr(&e, b"val").unwrap_or_default();
                    level.get_or_insert(0);
                }
                b"b" => bold = attr(&e, b"val").is_none_or(|v| v != "0" && v != "false"),
                b"i" => italic = attr(&e, b"val").is_none_or(|v| v != "0" && v != "false"),
                b"tab" => run.push('\t'),
                b"br" | b"cr" => run.push_str("  \n"),
                b"drawing" | b"pict" => run.push_str("[picture]"),
                _ => {}
            },
            Ok(Event::Text(t)) if in_text => run.push_str(&unescape(&t)),
                Ok(Event::GeneralRef(g)) if in_text => run.push_str(&entity(&g)),
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"t" => in_text = false,
                b"r" => flush_run(&mut para, &mut run, bold, italic),
                b"p" => {
                    let text = para.trim_end().to_string();
                    if let Some(cell) = tables.last_mut().and_then(|t| t.last_mut()).and_then(|r| r.last_mut()) {
                        if !cell.is_empty() && !text.is_empty() {
                            cell.push_str("<br>");
                        }
                        cell.push_str(&text.replace('|', "\\|").replace('\n', " "));
                        continue;
                    }
                    let s = style.to_lowercase();
                    let heading = if s == "title" {
                        Some(1)
                    } else if s == "subtitle" {
                        Some(2)
                    } else {
                        s.strip_prefix("heading").and_then(|n| n.trim().parse::<usize>().ok()).map(|n| n.clamp(1, 6))
                    };
                    if text.is_empty() {
                        continue;
                    }
                    match (heading, level) {
                        (Some(h), _) => md.push_str(&format!("{} {}\n\n", "#".repeat(h), text.replace("**", ""))),
                        (None, Some(l)) if !num_id.is_empty() || s.contains("list") => {
                            let indent = "  ".repeat(l);
                            if ordered.get(&num_id).copied().unwrap_or(false) {
                                let n = counters.entry((num_id.clone(), l)).or_insert(0);
                                *n += 1;
                                md.push_str(&format!("{indent}{}. {text}\n", n));
                            } else {
                                md.push_str(&format!("{indent}- {text}\n"));
                            }
                        }
                        _ if s.contains("quote") => md.push_str(&format!("> {text}\n\n")),
                        _ => {
                            if md.ends_with("\n") && !md.ends_with("\n\n") && !md.is_empty() {
                                md.push('\n');
                            }
                            md.push_str(&text);
                            md.push_str("\n\n");
                        }
                    }
                }
                b"tbl" => {
                    let Some(t) = tables.pop() else { continue };
                    if let Some(cell) = tables.last_mut().and_then(|t| t.last_mut()).and_then(|r| r.last_mut()) {
                        // A table in a table: its text goes in the cell.
                        cell.push_str(&t.iter().map(|r| r.join(" · ")).collect::<Vec<_>>().join("<br>"));
                        continue;
                    }
                    let cols = t.iter().map(|r| r.len()).max().unwrap_or(0);
                    if cols == 0 {
                        continue;
                    }
                    if !md.ends_with("\n\n") && !md.is_empty() {
                        md.push('\n');
                    }
                    for (i, row) in t.iter().enumerate() {
                        let mut cells = row.clone();
                        cells.resize(cols, String::new());
                        md.push_str(&format!("| {} |\n", cells.join(" | ")));
                        if i == 0 {
                            md.push_str(&format!("|{}\n", " --- |".repeat(cols)));
                        }
                    }
                    md.push('\n');
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(md)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ZIP with these files, stored (and one deflated).
    fn zip(path: &Path, files: &[(&str, &str)]) {
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
        let (sheets, cut) = xlsx(&p).unwrap();
        assert!(!cut);
        assert_eq!(sheets.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["Orders", "Empty"]);
        assert_eq!(sheets[0].1, vec![vec!["Item".to_string(), String::new(), "When".into()], vec!["Pen & ink".into(), "2.5".into(), "2026-03-14".into(), "TRUE".into()]]);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn documents_read_as_markdown() {
        let p = std::env::temp_dir().join(format!("kite-{}.docx", std::process::id()));
        zip(&p, &[
            ("word/document.xml", r#"<w:document><w:body>
                <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Plan</w:t></w:r></w:p>
                <w:p><w:r><w:t xml:space="preserve">Ship it </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>today</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> </w:t></w:r><w:r><w:t>.</w:t></w:r></w:p>
                <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p>
                <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Second</w:t></w:r></w:p>
                <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Qty</w:t></w:r></w:p></w:tc></w:tr>
                <w:tr><w:tc><w:p><w:r><w:t>Pen</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#),
            ("word/numbering.xml", r#"<w:numbering><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#),
        ]);
        let md = docx_markdown(&p).unwrap();
        assert_eq!(md, "# Plan\n\nShip it **today** .\n\n1. First\n2. Second\n\n| Name | Qty |\n| --- | --- |\n| Pen | 2 |\n\n");
        std::fs::remove_file(p).ok();
    }
}
