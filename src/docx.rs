//! Word documents (.docx) as Word shows them: a white page, the document's fonts, sizes,
//! colors, highlights, bold and italic, headings (from its own styles), lists, tables with
//! their shading, and pictures. Only the blocks on screen are drawn.

use crate::office::{Zip, attr, entity, guarded, local, unescape};
use crate::theme;
use crate::ui::px;
use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, FontStyle, FontWeight, HighlightStyle, ListAlignment, ListState, Pixels, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window, div, img, list,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A run's look (unset: as the style above says).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunProps {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub color: Option<u32>,
    pub highlight: Option<u32>,
    /// Points.
    pub size: Option<f32>,
    pub font: Option<String>,
    pub caps: Option<bool>,
}

impl RunProps {
    /// `self`, with what it leaves unset taken from `under`.
    fn over(&self, under: &RunProps) -> RunProps {
        RunProps {
            bold: self.bold.or(under.bold),
            italic: self.italic.or(under.italic),
            underline: self.underline.or(under.underline),
            strike: self.strike.or(under.strike),
            color: self.color.or(under.color),
            highlight: self.highlight.or(under.highlight),
            size: self.size.or(under.size),
            font: self.font.clone().or_else(|| under.font.clone()),
            caps: self.caps.or(under.caps),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParaProps {
    /// 0 left, 1 center, 2 right, 3 justified.
    pub align: Option<u8>,
    pub shade: Option<u32>,
    /// Outline level from the style (headings get room above).
    pub heading: Option<u8>,
    /// Space before and after (points).
    pub before: Option<f32>,
    pub after: Option<f32>,
    /// Left indent (points).
    pub indent: Option<f32>,
}

impl ParaProps {
    fn over(&self, under: &ParaProps) -> ParaProps {
        ParaProps {
            align: self.align.or(under.align),
            shade: self.shade.or(under.shade),
            heading: self.heading.or(under.heading),
            before: self.before.or(under.before),
            after: self.after.or(under.after),
            indent: self.indent.or(under.indent),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub props: RunProps,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub blocks: Vec<Block>,
    pub shade: Option<u32>,
    pub span: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para { props: ParaProps, runs: Vec<Run>, list: Option<String>, level: usize },
    Table { rows: Vec<Vec<Cell>>, widths: Vec<f32>, borders: bool },
    /// A picture (a file in the document's folder), its size in points, and how it's aligned.
    Image { path: PathBuf, w: f32, h: f32, align: u8 },
}

pub struct Document {
    pub blocks: Vec<Block>,
    /// The width text runs in (points), between the page's margins.
    pub text_w: f32,
    pub margin: f32,
    pub base: RunProps,
    /// Where its pictures were put (removed with the view).
    pub media: Option<PathBuf>,
}

fn hex(v: &str) -> Option<u32> {
    if v.eq_ignore_ascii_case("auto") {
        return None;
    }
    u32::from_str_radix(v.trim_start_matches('#'), 16).ok()
}

fn highlight(name: &str) -> Option<u32> {
    Some(match name {
        "yellow" => 0xFFFF00,
        "green" => 0x00FF00,
        "cyan" => 0x00FFFF,
        "magenta" => 0xFF00FF,
        "blue" => 0x0000FF,
        "red" => 0xFF0000,
        "darkBlue" => 0x000080,
        "darkCyan" => 0x008080,
        "darkGreen" => 0x008000,
        "darkMagenta" => 0x800080,
        "darkRed" => 0x800000,
        "darkYellow" => 0x808000,
        "darkGray" => 0x808080,
        "lightGray" => 0xC0C0C0,
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        _ => return None,
    })
}

fn on(e: &quick_xml::events::BytesStart) -> bool {
    attr(e, b"val").is_none_or(|v| v != "0" && v != "false" && v != "none")
}

/// A run property element into `p`.
fn run_prop(name: &[u8], e: &quick_xml::events::BytesStart, p: &mut RunProps, theme: &[u32]) {
    match name {
        b"b" => p.bold = Some(on(e)),
        b"i" => p.italic = Some(on(e)),
        b"u" => p.underline = Some(on(e)),
        b"strike" | b"dstrike" => p.strike = Some(on(e)),
        b"caps" => p.caps = Some(on(e)),
        b"color" => {
            p.color = attr(e, b"val").and_then(|v| hex(&v)).or_else(|| {
                // Only a theme color: from the theme.
                let i = match attr(e, b"themeColor")?.as_str() {
                    "text1" | "dark1" => 1,
                    "background1" | "light1" => 0,
                    "text2" | "dark2" => 3,
                    "background2" | "light2" => 2,
                    "accent1" => 4,
                    "accent2" => 5,
                    "accent3" => 6,
                    "accent4" => 7,
                    "accent5" => 8,
                    "accent6" => 9,
                    "hyperlink" => 10,
                    _ => return None,
                };
                theme.get(i).copied()
            })
        }
        b"highlight" => p.highlight = attr(e, b"val").and_then(|v| highlight(&v)),
        b"shd" => {
            if let Some(f) = attr(e, b"fill").and_then(|v| hex(&v)) {
                p.highlight = Some(f);
            }
        }
        b"sz" => p.size = attr(e, b"val").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 2.),
        b"rFonts" => p.font = attr(e, b"ascii").or_else(|| attr(e, b"hAnsi")),
        _ => {}
    }
}

fn para_prop(name: &[u8], e: &quick_xml::events::BytesStart, p: &mut ParaProps) {
    match name {
        b"jc" => {
            p.align = Some(match attr(e, b"val").as_deref() {
                Some("center") => 1,
                Some("right") | Some("end") => 2,
                Some("both") | Some("distribute") => 3,
                _ => 0,
            })
        }
        b"shd" => p.shade = attr(e, b"fill").and_then(|v| hex(&v)),
        b"outlineLvl" => p.heading = attr(e, b"val").and_then(|v| v.parse::<u8>().ok()).map(|v| v + 1),
        b"spacing" => {
            let pt = |k: &[u8]| attr(e, k).and_then(|v| v.parse::<f32>().ok()).map(|v| v / 20.);
            if let Some(b) = pt(b"before") {
                p.before = Some(b);
            }
            if let Some(a) = pt(b"after") {
                p.after = Some(a);
            }
        }
        b"ind" => p.indent = attr(e, b"left").or_else(|| attr(e, b"start")).and_then(|v| v.parse::<f32>().ok()).map(|v| v / 20.),
        _ => {}
    }
}

struct Style {
    based_on: Option<String>,
    run: RunProps,
    para: ParaProps,
}

/// The document's styles: paragraph and character ones by id, and the defaults.
fn read_styles(x: &[u8], theme: &[u32]) -> (HashMap<String, Style>, RunProps, ParaProps) {
    let mut styles = HashMap::new();
    let (mut defaults, mut para_defaults) = (RunProps::default(), ParaProps::default());
    let mut r = Reader::from_reader(x);
    let mut buf = Vec::new();
    let mut cur: Option<(String, Style)> = None;
    let (mut in_defaults, mut in_ppr) = (false, false);
    loop {
        let ev = r.read_event_into(&mut buf);
        match &ev {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = local(e.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"docDefaults" => in_defaults = true,
                    b"style" => {
                        cur = Some((attr(e, b"styleId").unwrap_or_default(), Style { based_on: None, run: RunProps::default(), para: ParaProps::default() }));
                    }
                    // "heading 2" and "Title": outline levels even when not written down.
                    b"name" if cur.is_some() => {
                        if let (Some((_, s)), Some(n)) = (&mut cur, attr(e, b"val")) {
                            let n = n.to_lowercase();
                            if let Some(l) = n.strip_prefix("heading ").and_then(|l| l.trim().parse::<u8>().ok()) {
                                s.para.heading = Some(l);
                            } else if n == "title" {
                                s.para.heading = Some(1);
                            }
                        }
                    }
                    b"basedOn" => {
                        if let Some((_, s)) = &mut cur {
                            s.based_on = attr(e, b"val");
                        }
                    }
                    b"pPr" => in_ppr = true,
                    n => {
                        if let Some((_, s)) = &mut cur {
                            run_prop(n, e, &mut s.run, theme);
                            if in_ppr {
                                para_prop(n, e, &mut s.para);
                            }
                        } else if in_defaults {
                            run_prop(n, e, &mut defaults, theme);
                            if in_ppr {
                                para_prop(n, e, &mut para_defaults);
                            }
                        }
                    }
                }
                if matches!(ev, Ok(Event::Empty(_))) && name == b"pPr" {
                    in_ppr = false;
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"style" => {
                    if let Some((id, s)) = cur.take() {
                        styles.insert(id, s);
                    }
                }
                b"docDefaults" => in_defaults = false,
                b"pPr" => in_ppr = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (styles, defaults, para_defaults)
}

/// A style with everything it's based on folded in.
fn resolve(styles: &HashMap<String, Style>, id: &str) -> (RunProps, ParaProps) {
    let (mut run, mut para) = (RunProps::default(), ParaProps::default());
    let mut at = Some(id.to_string());
    for _ in 0..12 {
        let Some(s) = at.as_ref().and_then(|i| styles.get(i)) else { break };
        run = run.over(&s.run);
        para = para.over(&s.para);
        at = s.based_on.clone();
    }
    (run, para)
}

/// Whether each list (by its number id) is numbered (else bulleted), per level.
fn read_numbering(x: &[u8]) -> HashMap<String, Vec<bool>> {
    let mut abs: HashMap<String, Vec<bool>> = HashMap::new();
    let mut nums: HashMap<String, String> = HashMap::new();
    let mut r = Reader::from_reader(x);
    let mut buf = Vec::new();
    let (mut cur_abs, mut cur_num, mut lvl) = (String::new(), String::new(), 0usize);
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) => match local(e.name().as_ref()) {
                b"abstractNum" => cur_abs = attr(&e, b"abstractNumId").unwrap_or_default(),
                b"lvl" => lvl = attr(&e, b"ilvl").and_then(|v| v.parse().ok()).unwrap_or(0),
                b"numFmt" => {
                    let v = abs.entry(cur_abs.clone()).or_default();
                    if v.len() <= lvl {
                        v.resize(lvl + 1, false);
                    }
                    v[lvl] = attr(&e, b"val").is_some_and(|v| v != "bullet" && v != "none");
                }
                b"num" => cur_num = attr(&e, b"numId").unwrap_or_default(),
                b"abstractNumId" => {
                    nums.insert(cur_num.clone(), attr(&e, b"val").unwrap_or_default());
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    nums.into_iter().map(|(n, a)| (n, abs.get(&a).cloned().unwrap_or_default())).collect()
}

/// Where each relationship id points (pictures).
fn read_rels(x: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut r = Reader::from_reader(x);
    let mut buf = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) if local(e.name().as_ref()) == b"Relationship" => {
                if let (Some(id), Some(t)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    let t = t.trim_start_matches('/');
                    out.insert(id, if t.starts_with("word/") { t.to_string() } else { format!("word/{t}") });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

const BULLETS: [&str; 3] = ["•", "◦", "▪"];

/// Reads a .docx (off the main thread: it unzips its pictures into a temporary folder).
pub fn read(path: &Path) -> Result<Document, String> {
    let path = path.to_path_buf();
    guarded(move || read_docx(&path))
}

struct TableBuild {
    rows: Vec<Vec<Cell>>,
    widths: Vec<f32>,
    /// Its own width: points, or a share of the text width (negative: -0.5 for 50%).
    want: Option<f32>,
    borders: bool,
    /// The cell being filled has a vertical merge's continuation (shown empty).
    hidden: bool,
}

fn read_docx(path: &Path) -> Result<Document, String> {
    let mut z = Zip::open(path)?;
    let x = z.read("word/document.xml").ok_or("no document in this file (is it the old .doc format?)")?;
    let theme = z.read("word/theme/theme1.xml").map(|t| crate::office::theme_colors(&t)).unwrap_or_default();
    let (styles, base_run, base_para) = z.read("word/styles.xml").map(|s| read_styles(&s, &theme)).unwrap_or_default();
    let numbering = z.read("word/numbering.xml").map(|n| read_numbering(&n)).unwrap_or_default();
    let rels = z.read("word/_rels/document.xml.rels").map(|r| read_rels(&r)).unwrap_or_default();
    // "Normal" under everything.
    let (normal_run, normal_para) = resolve(&styles, "Normal");
    let base = normal_run.over(&base_run);
    let base_para = normal_para.over(&base_para);

    let mut media: Option<PathBuf> = None;
    let mut saved: HashMap<String, PathBuf> = HashMap::new();
    let mut save_picture = |z: &mut Zip, target: &str| -> Option<PathBuf> {
        if let Some(p) = saved.get(target) {
            return Some(p.clone());
        }
        let ext = target.rsplit('.').next().unwrap_or("png").to_lowercase();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "svg") {
            return None;
        }
        let dir = media.get_or_insert_with(|| std::env::temp_dir().join(format!("kite-docx-{}-{}", std::process::id(), crate::api::new_id()))).clone();
        std::fs::create_dir_all(&dir).ok()?;
        let bytes = z.read(target)?;
        // A big photo is kept 1,400 pixels wide at most: sharp on the page, a fraction of the memory.
        let big = (ext != "svg").then(|| image::load_from_memory(&bytes).ok()).flatten().filter(|i| i.width() > 1400);
        let file = match big {
            Some(i) => {
                let file = dir.join(format!("{}.png", saved.len()));
                let h = (i.height() as f32 * 1400. / i.width() as f32).round().max(1.) as u32;
                i.thumbnail(1400, h).save(&file).ok()?;
                file
            }
            None => {
                let file = dir.join(format!("{}.{ext}", saved.len()));
                std::fs::write(&file, &bytes).ok()?;
                file
            }
        };
        saved.insert(target.to_string(), file.clone());
        Some(file)
    };

    let mut body: Vec<Block> = Vec::new();
    let mut tables: Vec<TableBuild> = Vec::new();
    let mut r = Reader::from_reader(x.as_slice());
    let mut buf = Vec::new();
    // The paragraph being read.
    let (mut pprops, mut pstyle, mut runs, mut list_num, mut list_lvl) = (ParaProps::default(), String::new(), Vec::<Run>::new(), None::<String>, 0usize);
    let mut pictures: Vec<(PathBuf, f32, f32)> = Vec::new();
    // The run being read.
    let (mut rprops, mut rstyle, mut text) = (RunProps::default(), String::new(), String::new());
    let (mut in_ppr, mut in_rpr, mut in_t, mut in_tcpr, mut in_tblpr) = (false, false, false, false, false);
    let mut extent = (0f32, 0f32);
    let mut in_change = 0usize;
    let mut counters: HashMap<(String, usize), usize> = HashMap::new();
    let (mut page_w, mut margin_l, mut margin_r) = (612f32, 72f32, 72f32);
    let push = |b: Block, body: &mut Vec<Block>, tables: &mut Vec<TableBuild>| match tables.last_mut().and_then(|t| t.rows.last_mut()).and_then(|r| r.last_mut()) {
        Some(cell) => cell.blocks.push(b),
        None => body.push(b),
    };
    loop {
        let ev = r.read_event_into(&mut buf);
        let (e, empty) = match &ev {
            Ok(Event::Start(e)) => (Some(e.clone()), false),
            Ok(Event::Empty(e)) => (Some(e.clone()), true),
            _ => (None, false),
        };
        // What a tracked change replaced (old grids, old formatting) isn't what's shown.
        if let Some(e) = &e
            && local(e.name().as_ref()).ends_with(b"Change")
        {
            if !empty {
                in_change += 1;
            }
            buf.clear();
            continue;
        }
        if let Ok(Event::End(e)) = &ev
            && local(e.name().as_ref()).ends_with(b"Change")
        {
            in_change = in_change.saturating_sub(1);
            buf.clear();
            continue;
        }
        if in_change > 0 {
            buf.clear();
            continue;
        }
        if let Some(e) = e {
            let name = local(e.name().as_ref()).to_vec();
            match name.as_slice() {
                b"p" => {
                    pprops = ParaProps::default();
                    pstyle.clear();
                    runs.clear();
                    list_num = None;
                    list_lvl = 0;
                    pictures.clear();
                }
                b"pPr" => in_ppr = !empty,
                b"rPr" => in_rpr = !empty,
                b"r" => {
                    rprops = RunProps::default();
                    rstyle.clear();
                    text.clear();
                }
                b"t" if !empty => in_t = true,
                b"tab" if !in_ppr => text.push('\t'),
                b"br" | b"cr" => text.push('\n'),
                b"pStyle" => pstyle = attr(&e, b"val").unwrap_or_default(),
                b"rStyle" => rstyle = attr(&e, b"val").unwrap_or_default(),
                b"numId" => list_num = attr(&e, b"val").filter(|v| v != "0"),
                b"ilvl" => list_lvl = attr(&e, b"val").and_then(|v| v.parse().ok()).unwrap_or(0),
                b"extent" => {
                    let emu = |k: &[u8]| attr(&e, k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.) / 12_700.;
                    extent = (emu(b"cx"), emu(b"cy"));
                }
                b"blip" => {
                    if let Some(t) = attr(&e, b"embed").and_then(|id| rels.get(&id).cloned())
                        && let Some(p) = save_picture(&mut z, &t)
                    {
                        pictures.push((p, extent.0, extent.1));
                    }
                }
                b"tbl" => tables.push(TableBuild { rows: Vec::new(), widths: Vec::new(), want: None, borders: false, hidden: false }),
                b"tblW" if in_tblpr => {
                    if let Some(t) = tables.last_mut() {
                        let w = attr(&e, b"w").and_then(|v| v.trim_end_matches('%').parse::<f32>().ok()).unwrap_or(0.);
                        t.want = match attr(&e, b"type").as_deref() {
                            Some("pct") if w > 0. => Some(-(if attr(&e, b"w").is_some_and(|v| v.ends_with('%')) { w / 100. } else { w / 5000. })),
                            Some("dxa") if w > 0. => Some(w / 20.),
                            _ => None,
                        };
                    }
                }
                b"tblPr" => in_tblpr = !empty,
                b"tblStyle" if in_tblpr => {
                    if let Some(t) = tables.last_mut() {
                        t.borders |= attr(&e, b"val").is_some_and(|v| v.contains("Grid") || v.contains("Table"));
                    }
                }
                b"gridCol" => {
                    if let Some(t) = tables.last_mut() {
                        t.widths.push(attr(&e, b"w").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.) / 20.);
                    }
                }
                b"tr" => {
                    if let Some(t) = tables.last_mut() {
                        t.rows.push(Vec::new());
                    }
                }
                b"tc" => {
                    if let Some(t) = tables.last_mut() {
                        t.hidden = false;
                        if let Some(row) = t.rows.last_mut() {
                            row.push(Cell { blocks: Vec::new(), shade: None, span: 1 });
                        }
                    }
                }
                b"tcPr" => in_tcpr = !empty,
                b"gridSpan" if in_tcpr => {
                    if let Some(c) = tables.last_mut().and_then(|t| t.rows.last_mut()).and_then(|r| r.last_mut()) {
                        c.span = attr(&e, b"val").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
                    }
                }
                b"vMerge" if in_tcpr => {
                    if let Some(t) = tables.last_mut() {
                        t.hidden = attr(&e, b"val").is_none_or(|v| v != "restart");
                    }
                }
                b"shd" if in_tcpr => {
                    if let Some(c) = tables.last_mut().and_then(|t| t.rows.last_mut()).and_then(|r| r.last_mut()) {
                        c.shade = attr(&e, b"fill").and_then(|v| hex(&v));
                    }
                }
                b"top" | b"left" | b"bottom" | b"right" | b"insideH" | b"insideV" if in_tblpr => {
                    if let Some(t) = tables.last_mut()
                        && attr(&e, b"val").is_some_and(|v| v != "nil" && v != "none")
                    {
                        t.borders = true;
                    }
                }
                b"pgSz" => page_w = attr(&e, b"w").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 20.).unwrap_or(page_w),
                b"pgMar" => {
                    margin_l = attr(&e, b"left").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 20.).unwrap_or(margin_l);
                    margin_r = attr(&e, b"right").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 20.).unwrap_or(margin_r);
                }
                n if in_rpr && !in_ppr => run_prop(n, &e, &mut rprops, &theme),
                n if in_ppr && !in_rpr => para_prop(n, &e, &mut pprops),
                _ => {}
            }
            if !empty {
                buf.clear();
                continue;
            }
        }
        match ev {
            Ok(Event::Text(t)) if in_t => text.push_str(&unescape(&t)),
            Ok(Event::GeneralRef(g)) if in_t => text.push_str(&entity(&g)),
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"t" => in_t = false,
                b"pPr" => in_ppr = false,
                b"rPr" => in_rpr = false,
                b"tcPr" => in_tcpr = false,
                b"tblPr" => in_tblpr = false,
                b"r" => {
                    if !text.is_empty() {
                        let (style_run, _) = if rstyle.is_empty() { Default::default() } else { resolve(&styles, &rstyle) };
                        runs.push(Run { text: std::mem::take(&mut text), props: rprops.over(&style_run) });
                    }
                }
                b"p" => {
                    if tables.last().is_some_and(|t| t.hidden) {
                        continue;
                    }
                    let (style_run, style_para) = if pstyle.is_empty() { Default::default() } else { resolve(&styles, &pstyle) };
                    let props = pprops.over(&style_para).over(&base_para);
                    let runs: Vec<Run> = std::mem::take(&mut runs).into_iter().map(|r| Run { text: r.text, props: r.props.over(&style_run).over(&base) }).collect();
                    let list = list_num.as_ref().map(|n| {
                        let ordered = numbering.get(n).and_then(|v| v.get(list_lvl)).copied().unwrap_or(false);
                        // A list's deeper levels start again under each item.
                        counters.retain(|(k, l), _| k != n || *l <= list_lvl);
                        let c = counters.entry((n.clone(), list_lvl)).or_insert(0);
                        *c += 1;
                        if ordered { format!("{}.", c) } else { BULLETS[list_lvl % 3].to_string() }
                    });
                    let has_text = runs.iter().any(|r| !r.text.trim().is_empty());
                    if has_text || pictures.is_empty() {
                        push(Block::Para { props: props.clone(), runs, list, level: list_lvl }, &mut body, &mut tables);
                    }
                    for (p, w, h) in pictures.drain(..) {
                        push(Block::Image { path: p, w, h, align: props.align.unwrap_or(0) }, &mut body, &mut tables);
                    }
                }
                b"tbl" => {
                    if let Some(mut t) = tables.pop() {
                        // Its columns scaled to the width it asks for (a share of the text width
                        // is kept as negative numbers: the view knows that width).
                        if let Some(want) = t.want {
                            let total: f32 = t.widths.iter().sum();
                            if want < 0. {
                                let n = t.widths.len().max(1) as f32;
                                t.widths = if total > 0. { t.widths.iter().map(|w| w / total * want).collect() } else { vec![want / n; n as usize] };
                            } else if total > 0. {
                                t.widths = t.widths.iter().map(|w| w / total * want).collect();
                            }
                        }
                        push(Block::Table { rows: t.rows, widths: t.widths, borders: t.borders }, &mut body, &mut tables);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(Document { blocks: body, text_w: (page_w - margin_l - margin_r).max(200.), margin: margin_l.clamp(18., 108.), base, media })
}

// ---------------------------------------------------------------- the view

/// Fonts a document may name that this Mac has; others show in the system font.
fn font(name: Option<&str>) -> SharedString {
    match name.unwrap_or("") {
        n @ ("Arial" | "Helvetica" | "Helvetica Neue" | "Times New Roman" | "Times" | "Georgia" | "Verdana" | "Courier New" | "Menlo" | "Trebuchet MS" | "Palatino" | "Avenir" | "Avenir Next") => n.to_string().into(),
        _ => ".SystemUIFont".into(),
    }
}

const PAPER: u32 = 0xFFFFFF;
const INK: u32 = 0x000000;

pub struct DocView {
    /// Pictures drawn in the last frame, and the ones decoded (let go once off screen).
    seen: Rc<std::cell::RefCell<std::collections::HashSet<PathBuf>>>,
    loaded: std::collections::HashSet<PathBuf>,
    pub doc: Option<Document>,
    pub error: Option<String>,
    list: ListState,
    pub zoom: f32,
    pub area: Rc<std::cell::Cell<Bounds<Pixels>>>,
}

impl Drop for DocView {
    fn drop(&mut self) {
        if let Some(m) = self.doc.as_ref().and_then(|d| d.media.clone()) {
            std::fs::remove_dir_all(m).ok();
        }
    }
}

impl DocView {
    pub fn new(result: Result<Document, String>) -> DocView {
        let (doc, error) = match result {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e)),
        };
        let n = doc.as_ref().map(|d| d.blocks.len()).unwrap_or(0);
        DocView { seen: Default::default(), loaded: Default::default(), doc, error, list: ListState::new(n + 2, ListAlignment::Top, gpui::px(800.)), zoom: 1., area: Default::default() }
    }

    pub fn replace(&mut self, result: Result<Document, String>, cx: &mut Context<Self>) {
        if let Some(m) = self.doc.as_ref().and_then(|d| d.media.clone()) {
            std::fs::remove_dir_all(m).ok();
        }
        let top = self.list.logical_scroll_top();
        let (doc, error) = match result {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e)),
        };
        self.list.reset(doc.as_ref().map(|d| d.blocks.len()).unwrap_or(0) + 2);
        self.doc = doc;
        self.error = error;
        self.list.scroll_to(top);
        cx.notify();
    }

    pub fn pinch(&mut self, magnify: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom * (1. + magnify)).clamp(0.5, 4.);
        let top = self.list.logical_scroll_top();
        self.list.reset(self.doc.as_ref().map(|d| d.blocks.len()).unwrap_or(0) + 2);
        self.list.scroll_to(gpui::ListOffset { item_ix: top.item_ix, offset_in_item: gpui::px(0.) });
        cx.notify();
    }

    /// Points to pixels on screen: the page fits the pane, at the zoom.
    fn scale(&self) -> f32 {
        let Some(d) = &self.doc else { return 1. };
        let page = d.text_w + d.margin * 2.;
        let pane = self.area.get().size.width / gpui::px(1.);
        let fit = if pane > 0. { ((pane - 32.) / page).min(1.333) } else { 1.333 };
        fit.max(0.3) * self.zoom
    }

    fn render_block(&self, b: &Block, k: f32, width: f32) -> AnyElement {
        let base = self.doc.as_ref().map(|d| d.base.clone()).unwrap_or_default();
        match b {
            Block::Para { props, runs, list, level } => {
                let size = runs.iter().find_map(|r| r.props.size).or(base.size).unwrap_or(11.);
                let fam = font(runs.first().and_then(|r| r.props.font.as_deref()).or(base.font.as_deref()));
                let mut text = String::new();
                let mut marks: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
                for r in runs {
                    let t = if r.props.caps == Some(true) { r.text.to_uppercase() } else { r.text.clone() };
                    let start = text.len();
                    text.push_str(&t);
                    let p = &r.props;
                    marks.push((
                        start..text.len(),
                        HighlightStyle {
                            color: Some(theme::c(p.color.unwrap_or(INK))),
                            font_weight: p.bold.filter(|b| *b).map(|_| FontWeight::BOLD),
                            font_style: p.italic.filter(|b| *b).map(|_| FontStyle::Italic),
                            background_color: p.highlight.map(theme::c),
                            underline: p.underline.filter(|b| *b).map(|_| UnderlineStyle { thickness: gpui::px(1.), color: None, wavy: false }),
                            strikethrough: p.strike.filter(|b| *b).map(|_| StrikethroughStyle { thickness: gpui::px(1.), color: None }),
                            fade_out: None,
                        },
                    ));
                }
                let empty = text.trim().is_empty();
                let heading = props.heading.unwrap_or(0);
                let before = props.before.unwrap_or(if heading > 0 { 12. } else { 0. });
                let after = props.after.unwrap_or(if heading > 0 { 4. } else { 8. });
                let indent = props.indent.unwrap_or(0.) + if list.is_some() { 18. + *level as f32 * 18. } else { 0. };
                let styled = StyledText::new(if empty { " ".to_string() } else { text }).with_highlights(marks);
                div()
                    .w(gpui::px(width))
                    .pt(gpui::px(before * k))
                    .pb(gpui::px(after * k))
                    .pl(gpui::px(indent.min(width / k / 2.) * k))
                    .when_some(props.shade, |d, s| d.bg(theme::c(s)))
                    .font_family(fam)
                    .text_size(gpui::px(size * k))
                    .line_height(gpui::relative(1.3))
                    .text_color(theme::c(INK))
                    .relative()
                    .when(props.align == Some(1), |d| d.text_center())
                    .when(props.align == Some(2), |d| d.text_right())
                    .when_some(list.clone(), |d, l| d.child(div().absolute().left(gpui::px((indent - 16.) * k)).child(l)))
                    .child(styled)
                    .into_any_element()
            }
            Block::Image { path, w, h, align } => {
                self.seen.borrow_mut().insert(path.clone());
                let (mut w, mut h) = (*w, *h);
                if w <= 0. || h <= 0. {
                    (w, h) = (width / k, width / k * 0.6);
                }
                let max = width / k;
                if w > max {
                    h *= max / w;
                    w = max;
                }
                div()
                    .w(gpui::px(width))
                    .flex()
                    .py(gpui::px(4. * k))
                    .when(*align == 1, |d| d.justify_center())
                    .when(*align == 2, |d| d.justify_end())
                    .child(img(path.clone()).w(gpui::px(w * k)).h(gpui::px(h * k)).object_fit(gpui::ObjectFit::Contain))
                    .into_any_element()
            }
            Block::Table { rows, widths, borders } => {
                // Shares of the text width (negative) become points here.
                let widths: Vec<f32> = widths.iter().map(|w| if *w < 0. { -w * width / k } else { *w }).collect();
                let total: f32 = widths.iter().sum();
                let cols = rows.iter().map(|r| r.iter().map(|c| c.span).sum::<usize>()).max().unwrap_or(1).max(1);
                // The table's own column widths, scaled to fit the text width.
                let col_w: Vec<f32> = if widths.len() >= cols && total > 0. { widths.iter().map(|w| w * (width / k / total).min(1.)).collect() } else { vec![width / k / cols as f32; cols] };
                let line = theme::c(if *borders { 0x000000 } else { 0xD0D0D0 });
                div()
                    .w(gpui::px(width))
                    .py(gpui::px(6. * k))
                    .flex()
                    .flex_col()
                    .children(rows.iter().map(|row| {
                        let mut c = 0;
                        div().flex().children(row.iter().map(|cell| {
                            let w: f32 = col_w[c.min(col_w.len() - 1)..(c + cell.span).min(col_w.len())].iter().sum::<f32>().max(20.);
                            c += cell.span;
                            div()
                                .w(gpui::px(w * k))
                                .flex_none()
                                .p(gpui::px(4. * k))
                                .border(gpui::px(0.5))
                                .border_color(line)
                                .when_some(cell.shade, |d, s| d.bg(theme::c(s)))
                                .children(cell.blocks.iter().map(|b| self.render_block(b, k, (w - 8.) * k)))
                        }))
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_item(&mut self, ix: usize, _: &mut Context<Self>) -> AnyElement {
        let Some(d) = &self.doc else { return div().into_any_element() };
        let k = self.scale();
        let (text_w, margin) = (d.text_w * k, d.margin * k);
        // The page: a white column, its margins around the text; first and last items are its
        // top and bottom.
        let page = |inner: AnyElement| div().w_full().flex().justify_center().child(div().w(gpui::px(text_w + margin * 2.)).px(gpui::px(margin)).bg(theme::c(PAPER)).child(inner));
        if ix == 0 || ix == d.blocks.len() + 1 {
            return div().w_full().flex().justify_center().pt(gpui::px(if ix == 0 { 16. } else { 0. })).pb(gpui::px(if ix == 0 { 0. } else { 24. })).child(div().w(gpui::px(text_w + margin * 2.)).h(gpui::px(margin)).bg(theme::c(PAPER))).into_any_element();
        }
        let b = &d.blocks[ix - 1];
        page(self.render_block(b, k, text_w)).into_any_element()
    }
}

impl Render for DocView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Pictures scrolled off screen are let go of (decoded, they can be big).
        let seen: std::collections::HashSet<PathBuf> = std::mem::take(&mut *self.seen.borrow_mut());
        for gone in self.loaded.difference(&seen).cloned().collect::<Vec<_>>() {
            crate::preview::release(&gone, window, cx);
            self.loaded.remove(&gone);
        }
        self.loaded.extend(seen);
        let area = self.area.clone();
        let body: AnyElement = match &self.error {
            Some(e) => div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child(format!("Can't show this document: {e}.")).into_any_element(),
            None => list(self.list.clone(), cx.processor(|v: &mut DocView, ix: usize, _, cx| v.render_item(ix, cx))).size_full().into_any_element(),
        };
        div()
            .debug_selector(|| "docx".into())
            .relative()
            .size_full()
            .bg(theme::c(0xE8E8E8))
            .child(
                gpui::canvas(
                    move |b, window, _| {
                        if area.get().size.width != b.size.width {
                            window.refresh();
                        }
                        area.set(b);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(body)
            .child(div().absolute().bottom(px(8.)).right(px(12.)).when((self.zoom - 1.).abs() > 0.01, |d| d.px(px(6.)).rounded(px(3.)).bg(theme::c(0x333333)).text_color(theme::c(0xffffff)).text_size(px(11.)).child(format!("{}%", (self.zoom * 100.).round()))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_keep_their_styles_lists_tables_and_pictures() {
        let p = std::env::temp_dir().join(format!("kite-doc-{}.docx", std::process::id()));
        let doc = r#"<w:document><w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Plan</w:t></w:r></w:p>
            <w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t xml:space="preserve">Ship it </w:t></w:r><w:r><w:rPr><w:b/><w:color w:val="FF0000"/><w:highlight w:val="yellow"/></w:rPr><w:t>today</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> </w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Second</w:t></w:r></w:p>
            <w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="1000"/></w:tblGrid>
              <w:tr><w:tc><w:tcPr><w:shd w:fill="D9E2F3"/></w:tcPr><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Qty</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            <w:p><w:r><w:drawing><wp:extent cx="1270000" cy="635000"/><a:blip r:embed="rId9"/></w:drawing></w:r></w:p>
        </w:body></w:document>"#;
        crate::office::tests::zip(&p, &[
            ("word/document.xml", doc),
            ("word/styles.xml", r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:color w:val="2F5496"/><w:sz w:val="32"/></w:rPr></w:style></w:styles>"#),
            ("word/numbering.xml", r#"<w:numbering><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#),
            ("word/_rels/document.xml.rels", r#"<Relationships><Relationship Id="rId9" Target="media/image1.png"/></Relationships>"#),
        ]);
        let d = read(&p).unwrap();
        let Block::Para { runs, props, .. } = &d.blocks[0] else { panic!() };
        assert_eq!((runs[0].props.color, runs[0].props.size, props.heading), (Some(0x2F5496), Some(16.), Some(1)), "the heading's style");
        let Block::Para { runs, props, .. } = &d.blocks[1] else { panic!() };
        assert_eq!(props.align, Some(1));
        assert_eq!((runs[1].props.bold, runs[1].props.color, runs[1].props.highlight, runs[1].props.size), (Some(true), Some(0xFF0000), Some(0xFFFF00), Some(11.)));
        assert!(matches!(&d.blocks[2], Block::Para { list: Some(l), .. } if l == "1."));
        assert!(matches!(&d.blocks[3], Block::Para { list: Some(l), .. } if l == "2."));
        let Block::Table { rows, widths, borders } = &d.blocks[4] else { panic!("{:?}", d.blocks[4]) };
        assert!(*borders);
        assert_eq!(widths, &[100., 50.]);
        assert_eq!(rows[0][0].shade, Some(0xD9E2F3));
        std::fs::remove_file(p).ok();
    }
}

