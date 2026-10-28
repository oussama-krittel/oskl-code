//! Previews: images shown as pictures instead of bytes, and live previews of Markdown and SVG
//! that follow the editor as you type (⇧⌘V in place, ⌘K V beside). Markdown is parsed with
//! pulldown-cmark and drawn with the app's own elements. Nothing is fetched from the network:
//! remote images show as links.

use crate::editor::Editor;
use crate::highlight::{self, LineSpans};
use crate::request::Request;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Prose in Markdown reads better in the system font than in the code font.
const PROSE_FONT: &str = ".SystemUIFont";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Image,
    Svg,
    Markdown,
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" => Kind::Image,
        "svg" => Kind::Svg,
        "md" | "markdown" | "mdx" => Kind::Markdown,
        _ => return None,
    })
}

/// Pictures open as a preview (there's no text to edit). Markdown and SVG open as text.
pub fn opens_as_preview(path: &Path) -> bool {
    kind_of(path) == Some(Kind::Image)
}

/// Text files that can also be shown rendered.
pub fn has_preview(path: &Path) -> bool {
    matches!(kind_of(path), Some(Kind::Svg | Kind::Markdown))
}

pub struct PreviewView {
    focus: FocusHandle,
    pub path: PathBuf,
    pub kind: Kind,
    /// The editor a Markdown / SVG preview follows; None shows the file on disk.
    pub source: Option<Entity<Editor>>,
    version: Option<u64>,
    blocks: Rc<Vec<Block>>,
    /// What the picture shows: the file, or a snapshot of the SVG being edited (a temp file,
    /// so it's drawn as sharply as files are).
    shown: PathBuf,
    snapshot: Option<PathBuf>,
    light: bool,
    /// The file changed on disk; reloaded on the next draw.
    stale: bool,
    info: String,
    scroll: ScrollHandle,
    refresh_task: Option<Task<()>>,
    _sub: Option<Subscription>,
}

impl EventEmitter<Request> for PreviewView {}

impl PreviewView {
    pub fn new(path: PathBuf, source: Option<Entity<Editor>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = kind_of(&path).unwrap_or(Kind::Markdown);
        let sub = source.as_ref().map(|ed| cx.observe_in(ed, window, |view, _, window, cx| view.source_changed(window, cx)));
        let mut view = Self {
            focus: cx.focus_handle(),
            shown: path.clone(),
            path,
            kind,
            source,
            version: None,
            blocks: Rc::default(),
            snapshot: None,
            light: false,
            stale: false,
            info: String::new(),
            scroll: ScrollHandle::new(),
            refresh_task: None,
            _sub: sub,
        };
        view.refresh(window, cx);
        view
    }

    pub fn title(&self) -> String {
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if self.kind == Kind::Image { name } else { format!("Preview {name}") }
    }

    #[cfg(test)]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn text(&self, cx: &App) -> String {
        match &self.source {
            Some(ed) => ed.read(cx).text(),
            None => std::fs::read_to_string(&self.path).unwrap_or_default(),
        }
    }

    fn source_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source.as_ref().map(|e| e.read(cx).buf.version()) == self.version {
            return;
        }
        // Once typing pauses for a moment.
        self.refresh_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            this.update_in(cx, |view, window, cx| view.refresh(window, cx)).ok();
        }));
    }

    /// Re-reads the text (or picture) and redraws.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.version = self.source.as_ref().map(|e| e.read(cx).buf.version());
        match self.kind {
            Kind::Markdown => self.blocks = Rc::new(parse(&self.text(cx))),
            Kind::Svg if self.source.is_some() => {
                let text = self.text(cx);
                let file = std::env::temp_dir().join(format!("kite-svg-{}-{:x}.svg", std::process::id(), hash(&text)));
                if Some(&file) != self.snapshot.as_ref() && std::fs::write(&file, &text).is_ok() {
                    if let Some(old) = self.snapshot.replace(file.clone()) {
                        release(&old, window, cx);
                        std::fs::remove_file(old).ok();
                    }
                    self.shown = file;
                }
                self.info = format!("SVG · {}", human_size(text.len() as u64));
            }
            Kind::Svg | Kind::Image => {
                // The file changed on disk: drop the picture decoded from the old one.
                release(&self.shown, window, cx);
                self.info = file_info(&self.path);
            }
        }
        cx.notify();
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        cx.notify();
    }

    /// Frees the decoded picture (the tab is closing).
    pub fn release(&mut self, window: &mut Window, cx: &mut App) {
        release(&self.shown, window, cx);
    }

    fn render_image(&self, cx: &mut Context<Self>) -> AnyElement {
        let bg = if self.light { theme::c(0xf3f3f3) } else { theme::bg_editor() };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("image-area")
                    .debug_selector(|| "preview-image".into())
                    .flex_1()
                    .min_h_0()
                    .p(px(20.))
                    .bg(bg)
                    .cursor_pointer()
                    .tooltip(ui::tooltip("Click to switch between a dark and a light background"))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.light = !v.light;
                        cx.notify();
                    }))
                    .child(img(self.shown.clone()).size_full().object_fit(ObjectFit::ScaleDown).with_fallback(|| {
                        div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child("Can't show this image").into_any_element()
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(26.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(px(11.))
                    .text_color(theme::fg_dim())
                    .child(self.info.clone()),
            )
            .into_any_element()
    }

    fn render_markdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let blocks = self.blocks.clone();
        let mut r = Renderer { base: self.path.parent().map(Path::to_path_buf).unwrap_or_default(), weak: cx.entity().downgrade(), n: 0, color: theme::fg() };
        let children: Vec<AnyElement> = blocks.iter().map(|b| r.block(b)).collect();
        div()
            .id("markdown")
            .debug_selector(|| "preview-markdown".into())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(860.))
                            .px(px(32.))
                            .py(px(28.))
                            .flex()
                            .flex_col()
                            .gap(px(14.))
                            .font_family(PROSE_FONT)
                            .text_size(px(14.))
                            .line_height(relative(1.55))
                            .text_color(theme::fg())
                            .children(children),
                    ),
            )
            .into_any_element()
    }
}

impl Drop for PreviewView {
    fn drop(&mut self) {
        if let Some(f) = self.snapshot.take() {
            std::fs::remove_file(f).ok();
        }
    }
}

impl Focusable for PreviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.stale) {
            self.refresh(window, cx);
        }
        let body = match self.kind {
            Kind::Markdown => self.render_markdown(cx),
            Kind::Image | Kind::Svg => self.render_image(cx),
        };
        div().key_context("Preview").track_focus(&self.focus).size_full().bg(theme::bg_editor()).child(body)
    }
}

/// Drops a picture from the image cache and the GPU (it's shown no more, or changed).
fn release(path: &Path, window: &mut Window, cx: &mut App) {
    let resource = Resource::Path(Arc::from(path));
    if let Some(Ok(image)) = window.get_asset::<ImgResourceLoader>(&resource, cx) {
        window.drop_image(image).ok();
    }
    ImageSource::Resource(resource).remove_asset(cx);
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.),
        b => format!("{:.1} MB", b as f64 / 1024. / 1024.),
    }
}

/// `PNG · 512 × 512 · 18.4 KB`.
fn file_info(path: &Path) -> String {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
    let size = std::fs::metadata(path).map(|m| human_size(m.len())).unwrap_or_default();
    match image_size(path) {
        Some((w, h)) => format!("{ext} · {w} × {h} · {size}"),
        None => format!("{ext} · {size}"),
    }
}

/// Width and height from the file's header (PNG, GIF, BMP, JPEG, WebP), without decoding it.
pub fn image_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path).ok()?.take(256 * 1024).read_to_end(&mut head).ok()?;
    let b = &head;
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let be32 = |i: usize| Some(u32::from_be_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]));
    let le32 = |i: usize| Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]));
    let le24 = |i: usize| Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, 0]));
    if b.starts_with(b"\x89PNG") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        return Some((le32(18)?, (le32(22)? as i32).unsigned_abs()));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let (b1, b2, b3, b4) = (*b.get(21)? as u32, *b.get(22)? as u32, *b.get(23)? as u32, *b.get(24)? as u32);
                Some((1 + (((b2 & 0x3f) << 8) | b1), 1 + (((b4 & 0xf) << 10) | (b3 << 2) | ((b2 & 0xc0) >> 6))))
            }
            b"VP8X" => Some((1 + le24(24)?, 1 + le24(27)?)),
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

// ---------------------------------------------------------------- Markdown model

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: bool,
}

/// Inline text: the string, styled runs (byte lengths) and link ranges.
#[derive(Clone, Debug, Default)]
struct Inline {
    text: String,
    runs: Vec<(usize, Style)>,
    links: Vec<(Range<usize>, String)>,
}

#[derive(Clone, Debug)]
struct Item {
    task: Option<bool>,
    blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
enum Block {
    Heading(u8, Inline),
    Para(Inline),
    /// Code (tabs expanded) and its highlighting, when the language is known.
    Code(String, Option<Arc<LineSpans>>),
    Quote(Vec<Block>),
    List(Option<u64>, Vec<Item>),
    Rule,
    /// Column alignment, then rows (the first is the header).
    Table(Vec<Alignment>, Vec<Vec<Inline>>),
    Image(String, String),
    Html(String),
}

enum Frame {
    Quote(Vec<Block>),
    List(Option<u64>, Vec<Item>),
    Item(Option<bool>, Vec<Block>),
    Table(Vec<Alignment>, Vec<Vec<Inline>>, Vec<Inline>),
}

#[derive(Default)]
struct Builder {
    out: Vec<Block>,
    stack: Vec<Frame>,
    inline: Option<Inline>,
    heading: Option<u8>,
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<String>,
    code: Option<(Option<String>, String)>,
    image: Option<(String, String)>,
    /// Images met in the paragraph being read: drawn as blocks after it.
    images: Vec<Block>,
    html: Option<String>,
}

fn parse(text: &str) -> Vec<Block> {
    let mut b = Builder::default();
    for ev in Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS) {
        b.event(ev);
    }
    b.flush_inline();
    b.out
}

/// The highlighting language of a fence (` ```ts `).
fn code_lang(lang: &str) -> Option<highlight::Lang> {
    let l = lang.to_ascii_lowercase();
    let ext = match l.as_str() {
        "typescript" => "ts",
        "javascript" | "jsx" | "mjs" => "js",
        "jsonc" | "json5" => "json",
        "scss" | "less" => "css",
        "xml" | "vue" | "svelte" => "html",
        "rust" => "rs",
        "python" => "py",
        "shell" | "zsh" | "console" | "bash" => "sh",
        "yml" => "yaml",
        other => other,
    };
    highlight::detect(Path::new(&format!("x.{ext}")))
}

impl Builder {
    fn push_block(&mut self, b: Block) {
        match self.stack.last_mut() {
            Some(Frame::Quote(v)) | Some(Frame::Item(_, v)) => v.push(b),
            Some(Frame::Table(..)) => {}
            _ => self.out.push(b),
        }
    }

    /// The text read so far becomes a paragraph (also for tight list items), then any images.
    fn flush_inline(&mut self) {
        if let Some(inl) = self.inline.take()
            && !inl.text.trim().is_empty()
        {
            self.push_block(Block::Para(inl));
        }
        for img in std::mem::take(&mut self.images) {
            self.push_block(img);
        }
    }

    fn text(&mut self, t: &str, code: bool) {
        let st = Style { bold: self.bold > 0, italic: self.italic > 0, strike: self.strike > 0, code, link: !self.links.is_empty() };
        let inl = self.inline.get_or_insert_with(Inline::default);
        let start = inl.text.len();
        inl.text.push_str(t);
        match inl.runs.last_mut() {
            Some((len, s)) if *s == st => *len += t.len(),
            _ => inl.runs.push((t.len(), st)),
        }
        if let Some(url) = self.links.last() {
            let end = inl.text.len();
            match inl.links.last_mut() {
                Some((r, u)) if r.end == start && u == url => r.end = end,
                _ => inl.links.push((start..end, url.clone())),
            }
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph | Tag::TableCell => self.inline = Some(Inline::default()),
                Tag::Heading { level, .. } => {
                    self.flush_inline();
                    self.heading = Some(level as u8);
                    self.inline = Some(Inline::default());
                }
                Tag::BlockQuote(_) => {
                    self.flush_inline();
                    self.stack.push(Frame::Quote(Vec::new()));
                }
                Tag::CodeBlock(kind) => {
                    self.flush_inline();
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_string),
                        CodeBlockKind::Indented => None,
                    };
                    self.code = Some((lang, String::new()));
                }
                Tag::HtmlBlock => self.html = Some(String::new()),
                Tag::List(start) => {
                    self.flush_inline();
                    self.stack.push(Frame::List(start, Vec::new()));
                }
                Tag::Item => self.stack.push(Frame::Item(None, Vec::new())),
                Tag::Table(align) => self.stack.push(Frame::Table(align, Vec::new(), Vec::new())),
                Tag::Emphasis => self.italic += 1,
                Tag::Strong => self.bold += 1,
                Tag::Strikethrough => self.strike += 1,
                Tag::Link { dest_url, .. } => self.links.push(dest_url.to_string()),
                Tag::Image { dest_url, .. } => self.image = Some((dest_url.to_string(), String::new())),
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => self.flush_inline(),
                TagEnd::Heading(_) => {
                    let inl = self.inline.take().unwrap_or_default();
                    let level = self.heading.take().unwrap_or(1);
                    self.push_block(Block::Heading(level, inl));
                    self.flush_inline();
                }
                TagEnd::BlockQuote(_) => {
                    self.flush_inline();
                    if let Some(Frame::Quote(v)) = self.stack.pop() {
                        self.push_block(Block::Quote(v));
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, text)) = self.code.take() {
                        let text = text.trim_end_matches('\n').replace('\t', "    ");
                        let spans = lang.as_deref().and_then(code_lang).and_then(|l| highlight::highlight(l, &text)).map(Arc::new);
                        self.push_block(Block::Code(text, spans));
                    }
                }
                TagEnd::HtmlBlock => {
                    if let Some(h) = self.html.take()
                        && !h.trim_start().starts_with("<!--")
                    {
                        self.push_block(Block::Html(h));
                    }
                }
                TagEnd::List(_) => {
                    self.flush_inline();
                    if let Some(Frame::List(start, items)) = self.stack.pop() {
                        self.push_block(Block::List(start, items));
                    }
                }
                TagEnd::Item => {
                    self.flush_inline();
                    if let Some(Frame::Item(task, blocks)) = self.stack.pop()
                        && let Some(Frame::List(_, items)) = self.stack.last_mut()
                    {
                        items.push(Item { task, blocks });
                    }
                }
                TagEnd::TableCell => {
                    let inl = self.inline.take().unwrap_or_default();
                    if let Some(Frame::Table(_, _, row)) = self.stack.last_mut() {
                        row.push(inl);
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(Frame::Table(_, rows, row)) = self.stack.last_mut() {
                        rows.push(std::mem::take(row));
                    }
                }
                TagEnd::Table => {
                    if let Some(Frame::Table(align, rows, _)) = self.stack.pop() {
                        self.push_block(Block::Table(align, rows));
                    }
                }
                TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
                TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
                TagEnd::Link => {
                    self.links.pop();
                }
                TagEnd::Image => {
                    if let Some((url, alt)) = self.image.take() {
                        self.images.push(Block::Image(url, alt));
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&t);
                } else if let Some((_, alt)) = &mut self.image {
                    alt.push_str(&t);
                } else if let Some(h) = &mut self.html {
                    h.push_str(&t);
                } else {
                    self.text(&t, false);
                }
            }
            Event::Code(t) => self.text(&t, true),
            Event::Html(t) => {
                if let Some(h) = &mut self.html {
                    h.push_str(&t);
                }
            }
            Event::InlineHtml(t) => {
                let tag = t.trim().to_ascii_lowercase();
                if tag.starts_with("<br") {
                    self.text("\n", false);
                }
            }
            Event::SoftBreak => self.text(" ", false),
            Event::HardBreak => self.text("\n", false),
            Event::Rule => {
                self.flush_inline();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item(task, _)) = self.stack.last_mut() {
                    *task = Some(done);
                }
            }
            Event::FootnoteReference(t) => self.text(&format!("[{t}]"), false),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- Markdown drawing

struct Renderer {
    /// The folder of the Markdown file (relative links and images).
    base: PathBuf,
    weak: WeakEntity<PreviewView>,
    n: usize,
    color: Hsla,
}

fn open_link(url: &str, base: &Path, weak: &WeakEntity<PreviewView>, cx: &mut App) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        return cx.open_url(url);
    }
    let rel = url.split('#').next().unwrap_or("");
    if rel.is_empty() {
        return;
    }
    let path = base.join(rel);
    if path.exists() {
        weak.update(cx, |_, cx| cx.emit(Request::OpenFile { path, line: None, preview: false })).ok();
    }
}

impl Renderer {
    fn id(&mut self) -> usize {
        self.n += 1;
        self.n
    }

    fn run(&self, len: usize, st: Style, bold: bool) -> TextRun {
        let mut f = font(if st.code { theme::MONO_FONT } else { PROSE_FONT });
        if st.bold || bold {
            f.weight = FontWeight::BOLD;
        }
        if st.italic {
            f.style = FontStyle::Italic;
        }
        TextRun {
            len,
            font: f,
            color: if st.link { theme::accent() } else if st.code { highlight::color(string_index()) } else { self.color },
            background_color: st.code.then(|| theme::ca(0xffffff14)),
            underline: st.link.then(|| UnderlineStyle { thickness: px(1.), color: Some(theme::accent().opacity(0.6)), wavy: false }),
            strikethrough: st.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(theme::fg_dim()) }),
        }
    }

    fn inline(&mut self, inl: &Inline, bold: bool) -> AnyElement {
        let runs: Vec<TextRun> = inl.runs.iter().map(|(len, st)| self.run(*len, *st, bold)).collect();
        let styled = StyledText::new(inl.text.clone()).with_runs(runs);
        if inl.links.is_empty() {
            return styled.into_any_element();
        }
        let ranges: Vec<Range<usize>> = inl.links.iter().map(|(r, _)| r.clone()).collect();
        let urls: Vec<String> = inl.links.iter().map(|(_, u)| u.clone()).collect();
        let (base, weak) = (self.base.clone(), self.weak.clone());
        InteractiveText::new(("md-text", self.id()), styled)
            .on_click(ranges, move |ix, _, cx| {
                if let Some(url) = urls.get(ix) {
                    open_link(url, &base, &weak, cx);
                }
            })
            .into_any_element()
    }

    fn block(&mut self, b: &Block) -> AnyElement {
        match b {
            Block::Heading(level, inl) => {
                let size = match level {
                    1 => 26.,
                    2 => 21.,
                    3 => 17.,
                    4 => 15.,
                    _ => 14.,
                };
                let color = std::mem::replace(&mut self.color, theme::fg_bright());
                let text = self.inline(inl, true);
                self.color = color;
                div()
                    .text_size(px(size))
                    .line_height(relative(1.3))
                    .mt(px(if *level <= 2 { 10. } else { 4. }))
                    .when(*level <= 2, |d| d.pb(px(6.)).border_b_1().border_color(theme::border()))
                    .child(text)
                    .into_any_element()
            }
            Block::Para(inl) => div().child(self.inline(inl, false)).into_any_element(),
            Block::Code(text, spans) => code_block(self.id(), text, spans.as_deref()),
            Block::Quote(blocks) => {
                let color = std::mem::replace(&mut self.color, theme::fg_dim());
                let children: Vec<AnyElement> = blocks.iter().map(|b| self.block(b)).collect();
                self.color = color;
                div().pl(px(14.)).border_l_4().border_color(theme::border_input()).flex().flex_col().gap(px(8.)).children(children).into_any_element()
            }
            Block::List(start, items) => {
                let mut rows = Vec::new();
                for (k, item) in items.iter().enumerate() {
                    let marker = match (item.task, start) {
                        (Some(true), _) => "☑".to_string(),
                        (Some(false), _) => "☐".to_string(),
                        (None, Some(n)) => format!("{}.", n + k as u64),
                        (None, None) => "•".to_string(),
                    };
                    let content: Vec<AnyElement> = item.blocks.iter().map(|b| self.block(b)).collect();
                    rows.push(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(div().flex_none().min_w(px(16.)).text_color(theme::fg_dim()).child(marker))
                            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(6.)).children(content))
                            .into_any_element(),
                    );
                }
                div().flex().flex_col().gap(px(4.)).children(rows).into_any_element()
            }
            Block::Rule => div().my(px(6.)).h(px(1.)).bg(theme::border()).into_any_element(),
            Block::Table(align, rows) => {
                let mut out = Vec::new();
                for (r, row) in rows.iter().enumerate() {
                    let mut cells = Vec::new();
                    for (c, cell) in row.iter().enumerate() {
                        let a = align.get(c).copied().unwrap_or(Alignment::None);
                        let text = self.inline(cell, r == 0);
                        cells.push(
                            div()
                                .flex_1()
                                .min_w(px(60.))
                                .px(px(10.))
                                .py(px(5.))
                                .flex()
                                .when(c > 0, |d| d.border_l_1())
                                .border_color(theme::border())
                                .when(a == Alignment::Center, |d| d.justify_center())
                                .when(a == Alignment::Right, |d| d.justify_end())
                                .child(text)
                                .into_any_element(),
                        );
                    }
                    out.push(div().flex().when(r > 0, |d| d.border_t_1()).border_color(theme::border()).when(r == 0, |d| d.bg(theme::ca(0xffffff08))).children(cells).into_any_element());
                }
                div().flex().flex_col().border_1().border_color(theme::border()).rounded(px(4.)).overflow_hidden().children(out).into_any_element()
            }
            Block::Image(url, alt) => self.image(url, alt),
            Block::Html(text) => div().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::fg_muted()).child(text.trim().to_string()).into_any_element(),
        }
    }

    fn image(&mut self, url: &str, alt: &str) -> AnyElement {
        let local = !url.contains("://") && !url.starts_with("data:");
        let path = self.base.join(url.split(['?', '#']).next().unwrap_or(url));
        if local && path.is_file() {
            let alt = alt.to_string();
            let el = img(path.clone()).object_fit(ObjectFit::Contain).with_fallback(move || div().text_color(theme::fg_dim()).child(format!("[{alt}]")).into_any_element());
            let el = match image_size(&path) {
                Some((w, h)) if w > 0 && h > 0 => {
                    let scale = (760. / w as f32).min(1.);
                    el.w(px(w as f32 * scale)).h(px(h as f32 * scale))
                }
                _ => el.max_w(px(760.)),
            };
            return div().child(el).into_any_element();
        }
        // Remote pictures aren't downloaded: the alt text links to them.
        let label = format!("[image: {}]", if alt.is_empty() { url } else { alt });
        let inl = Inline { runs: vec![(label.len(), Style { link: true, ..Default::default() })], links: vec![(0..label.len(), url.to_string())], text: label };
        div().child(self.inline(&inl, false)).into_any_element()
    }
}

fn string_index() -> u8 {
    (0..u8::MAX).find(|i| highlight::name(*i) == "string").unwrap_or(0)
}

fn code_block(id: usize, text: &str, spans: Option<&LineSpans>) -> AnyElement {
    let lines: Vec<AnyElement> = text
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let shown = if line.is_empty() { " " } else { line };
            let runs = code_runs(shown, spans.and_then(|s| s.get(i)));
            div().whitespace_nowrap().child(StyledText::new(shown.to_string()).with_runs(runs)).into_any_element()
        })
        .collect();
    div()
        .id(("md-code", id))
        .rounded(px(6.))
        .bg(theme::ca(0xffffff0a))
        .border_1()
        .border_color(theme::border())
        .px(px(14.))
        .py(px(10.))
        .overflow_x_scroll()
        .font_family(theme::MONO_FONT)
        .text_size(px(12.5))
        .line_height(relative(1.45))
        .child(div().flex().flex_col().children(lines))
        .into_any_element()
}

/// Coloured runs for a code line from its highlight spans (char columns).
fn code_runs(line: &str, spans: Option<&Vec<(Range<usize>, u8)>>) -> Vec<TextRun> {
    let mono = font(theme::MONO_FONT);
    let run = |len: usize, color: Hsla| TextRun { len, font: mono.clone(), color, background_color: None, underline: None, strikethrough: None };
    let offsets: Vec<usize> = line.char_indices().map(|(b, _)| b).chain(std::iter::once(line.len())).collect();
    let byte = |col: usize| offsets[col.min(offsets.len() - 1)];
    let mut runs = Vec::new();
    let mut pos = 0;
    for (cols, h) in spans.into_iter().flatten() {
        let (b0, b1) = (byte(cols.start), byte(cols.end));
        if b0 < pos || b1 <= b0 {
            continue;
        }
        if b0 > pos {
            runs.push(run(b0 - pos, theme::fg()));
        }
        runs.push(run(b1 - b0, highlight::color(*h)));
        pos = b1;
    }
    if pos < line.len() {
        runs.push(run(line.len() - pos, theme::fg()));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::{Block, image_size, parse};

    #[test]
    fn parses_the_usual_markdown() {
        let md = "# Title\n\nSome *em* and **bold** with `code` and a [link](docs/a.md).\n\n- one\n- [x] done\n  1. nested\n\n> quote\n\n```ts\nconst a = 1;\n```\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n\n---\n\n![logo](logo.png)\n";
        let b = parse(md);
        assert!(matches!(&b[0], Block::Heading(1, i) if i.text == "Title"));
        let Block::Para(p) = &b[1] else { panic!("{:?}", b[1]) };
        assert_eq!(p.text, "Some em and bold with code and a link.");
        assert_eq!(p.links, vec![(p.text.find("link").unwrap()..p.text.find("link").unwrap() + 4, "docs/a.md".to_string())]);
        assert_eq!(p.runs.iter().map(|r| r.0).sum::<usize>(), p.text.len(), "runs cover the text");
        let Block::List(None, items) = &b[2] else { panic!("{:?}", b[2]) };
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].task, Some(true));
        assert!(items[1].blocks.iter().any(|x| matches!(x, Block::List(Some(1), _))), "nested ordered list");
        assert!(matches!(&b[3], Block::Quote(q) if q.len() == 1));
        assert!(matches!(&b[4], Block::Code(t, Some(_)) if t == "const a = 1;"), "highlighted");
        assert!(matches!(&b[5], Block::Table(a, rows) if a.len() == 2 && rows.len() == 2));
        assert!(matches!(&b[6], Block::Rule));
        assert!(matches!(&b[7], Block::Image(u, a) if u == "logo.png" && a == "logo"));
    }

    #[test]
    fn reads_picture_sizes_from_headers() {
        let dir = std::env::temp_dir().join(format!("kite-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(300u32.to_be_bytes());
        bytes.extend(200u32.to_be_bytes());
        std::fs::write(&png, &bytes).unwrap();
        assert_eq!(image_size(&png), Some((300, 200)));
        let gif = dir.join("a.gif");
        std::fs::write(&gif, b"GIF89a\x40\x01\xf0\x00").unwrap();
        assert_eq!(image_size(&gif), Some((320, 240)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
