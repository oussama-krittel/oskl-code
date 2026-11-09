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
    /// CSV / TSV, as a table.
    Table,
    /// Word: shown as the Markdown it reads as.
    Doc,
    /// Excel: its sheets as tables.
    Sheet,
    /// Its pages, drawn by macOS.
    Pdf,
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" => Kind::Image,
        "svg" => Kind::Svg,
        "md" | "markdown" | "mdx" => Kind::Markdown,
        "csv" | "tsv" | "tab" => Kind::Table,
        "docx" | "docm" | "dotx" | "doc" => Kind::Doc,
        "xlsx" | "xlsm" | "xltx" | "xls" => Kind::Sheet,
        "pdf" => Kind::Pdf,
        _ => return None,
    })
}

/// Pictures open as a preview (there's no text to edit). Markdown and SVG open as text.
pub fn opens_as_preview(path: &Path) -> bool {
    matches!(kind_of(path), Some(Kind::Image | Kind::Doc | Kind::Sheet | Kind::Pdf))
}

/// Text files that can also be shown rendered.
pub fn has_preview(path: &Path) -> bool {
    matches!(kind_of(path), Some(Kind::Svg | Kind::Markdown | Kind::Table))
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
    /// A CSV's (or spreadsheet's) table.
    table: Option<Entity<crate::table::TableView>>,
    table_task: Option<Task<()>>,
    /// A Word document, as Word shows it.
    doc: Option<Entity<crate::docx::DocView>>,
    pdf: Option<Entity<crate::pdf::PdfView>>,
    /// The shown picture's own width and height (read once per file).
    natural: std::cell::RefCell<Option<(PathBuf, Option<(f32, f32)>)>>,
    snapshot: Option<PathBuf>,
    light: bool,
    /// The file changed on disk; reloaded on the next draw.
    stale: bool,
    /// The theme the page was built in: diagrams are redrawn in a new one's colors.
    theme: theme::Theme,
    info: String,
    scroll: ScrollHandle,
    refresh_task: Option<Task<()>>,
    /// Mermaid diagrams on show, to free the ones that go away (edited or closed).
    diagrams: Vec<PathBuf>,
    /// Pinch zoom of the whole page (1 = 100%), and where the page was drawn.
    zoom: f32,
    /// The page's width at 100% when zooming started (kept while zoomed), and its last width.
    locked_w: Option<f32>,
    page_w: Rc<std::cell::Cell<f32>>,
    area: Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// Where each diagram was last drawn (by its file), to draw only those near the screen.
    boxes: Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, Bounds<Pixels>>>>,
    /// Each diagram's last step whose tiles all finished loading (shown until a new one has),
    /// and the tile pictures decoded now: any not shown any more is freed.
    ready: Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, f32>>>,
    decoded: std::collections::HashSet<PathBuf>,
    /// The zoom diagram pictures are drawn for: it follows the zoom once a pinch pauses, so
    /// pinching only stretches what's there (smooth) and a sharp one is drawn after.
    sharp_zoom: f32,
    settle: Option<Task<()>>,
    /// Hands freed memory back to macOS a moment after pictures come and go.
    relief: Option<Task<()>>,
    /// The page's pictures (diagrams, images), decoded here rather than in GPUI's app-wide
    /// cache, so the ones not shown any more and all of them on closing are really freed.
    images: Entity<RetainAllImageCache>,
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
            natural: Default::default(),
            table: None,
            table_task: None,
            doc: None,
            pdf: None,
            path,
            kind,
            source,
            version: None,
            blocks: Rc::default(),
            snapshot: None,
            light: false,
            stale: false,
            theme: theme::current(),
            info: String::new(),
            scroll: ScrollHandle::new(),
            refresh_task: None,
            diagrams: Vec::new(),
            zoom: 1.,
            locked_w: None,
            page_w: Default::default(),
            area: Default::default(),
            boxes: Default::default(),
            ready: Default::default(),
            decoded: Default::default(),
            sharp_zoom: 1.,
            settle: None,
            relief: None,
            images: RetainAllImageCache::new(cx),
            _sub: sub,
        };
        view.refresh(window, cx);
        view
    }

    pub fn title(&self) -> String {
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match self.kind {
            Kind::Image | Kind::Sheet | Kind::Pdf | Kind::Doc => name,
            Kind::Table => format!("Table {name}"),
            _ => format!("Preview {name}"),
        }
    }

    #[cfg(test)]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn focus_target(&self, _: &App) -> FocusHandle {
        self.focus.clone()
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
            Kind::Markdown => {
                self.theme = theme::current();
                // Each diagram's stand-in is kept until the diagram itself has loaded.
                let ready = self.ready.borrow();
                let prev = diagram_order(&self.blocks)
                    .into_iter()
                    .map(|d| d.map(|mut d| {
                        if ready.contains_key(&d.path) {
                            d.stand_in = None;
                        }
                        d
                    }))
                    .collect();
                drop(ready);
                PREVIOUS.with(|p| *p.borrow_mut() = prev);
                self.blocks = Rc::new(parse(&self.text(cx)));
                PREVIOUS.with(|p| p.borrow_mut().clear());
                self.draw_pending_diagrams(cx);
                let now = diagram_files(&self.blocks);
                for old in self.diagrams.iter().filter(|p| !now.contains(p)) {
                    forget_diagram(old, window, cx);
                    self.ready.borrow_mut().remove(old);
                    self.boxes.borrow_mut().remove(old);
                }
                self.diagrams = now;
            }
            // Drawn from a snapshot while it's edited, or when it's a fragment without its <svg>.
            Kind::Svg if self.source.is_some() || std::fs::read_to_string(&self.path).is_ok_and(|t| !has_svg_root(&t)) => {
                let raw = self.text(cx);
                let fragment = !has_svg_root(&raw);
                let text = if fragment { wrap_svg_fragment(&raw) } else { raw.clone() };
                let file = std::env::temp_dir().join(format!("kite-svg-{}-{:x}.svg", std::process::id(), hash(&text)));
                if Some(&file) != self.snapshot.as_ref() && std::fs::write(&file, &text).is_ok() {
                    if let Some(old) = self.snapshot.replace(file.clone()) {
                        release(&old, window, cx);
                        std::fs::remove_file(old).ok();
                    }
                    self.shown = file;
                }
                self.info = format!("SVG{} · {}", if fragment { " fragment (no <svg> element: drawn in one sized to it)" } else { "" }, human_size(raw.len() as u64));
            }
            Kind::Table => {
                // Read (and split into rows) off the main thread: a big file takes a moment.
                let (path, text) = (self.path.clone(), self.source.as_ref().map(|e| e.read(cx).text()));
                let read = cx.background_executor().spawn(async move { crate::table::Sheet::delimited(&path, text) });
                self.table_task = Some(cx.spawn(async move |this, cx| {
                    let sheet = read.await;
                    this.update(cx, |v, cx| {
                        match sheet {
                            Ok(sheet) => match &v.table {
                                Some(t) => t.update(cx, |t, cx| t.replace(vec![sheet], cx)),
                                None => v.table = Some(cx.new(|cx| crate::table::TableView::new(vec![sheet], true, cx))),
                            },
                            Err(e) => v.info = e.to_string(),
                        }
                        cx.notify();
                    })
                    .ok();
                }));
            }
            Kind::Sheet => {
                let path = self.path.clone();
                let read = cx.background_executor().spawn(async move { crate::office::xlsx(&path) });
                self.table_task = Some(cx.spawn(async move |this, cx| {
                    let book = read.await;
                    this.update(cx, |v, cx| {
                        match book {
                            Ok((sheets, _cut, palette)) => {
                                let palette = std::sync::Arc::new(palette);
                                let sheets: Vec<crate::table::Sheet> = sheets.into_iter().map(|d| crate::table::Sheet::spreadsheet(d, palette.clone())).collect();
                                match &v.table {
                                    Some(t) => t.update(cx, |t, cx| t.replace(sheets, cx)),
                                    None => v.table = Some(cx.new(|cx| crate::table::TableView::new(sheets, false, cx))),
                                }
                            }
                            Err(e) => v.info = format!("Can't show this workbook: {e}."),
                        }
                        cx.notify();
                    })
                    .ok();
                }));
            }
            Kind::Doc => {
                // Read off the main thread (its pictures are unzipped to a temporary folder).
                let path = self.path.clone();
                let read = cx.background_executor().spawn(async move { crate::docx::read(&path) });
                self.table_task = Some(cx.spawn(async move |this, cx| {
                    let doc = read.await;
                    this.update(cx, |v, cx| {
                        match &v.doc {
                            Some(d) => d.update(cx, |d, cx| d.replace(doc, cx)),
                            None => v.doc = Some(cx.new(|_| crate::docx::DocView::new(doc))),
                        }
                        cx.notify();
                    })
                    .ok();
                }));
            }
            Kind::Pdf => match &self.pdf {
                Some(p) => p.update(cx, |p, cx| p.reload(cx)),
                None => {
                    let path = self.path.clone();
                    self.pdf = Some(cx.new(|cx| crate::pdf::PdfView::new(path, cx)));
                }
            },
            Kind::Svg | Kind::Image => {
                // The file changed on disk: drop the picture decoded from the old one.
                release(&self.shown, window, cx);
                self.info = file_info(&self.path);
            }
        }
        cx.notify();
    }

    /// A trackpad pinch over the page: zooms all of it (text, tables, code, diagrams), keeping
    /// the spot under the fingers where it is. Returns whether this preview took it.
    pub fn pinch(&mut self, p: crate::pinch::Pinch, cx: &mut Context<Self>) -> bool {
        if self.kind == Kind::Doc {
            let Some(v) = self.doc.clone().filter(|v| v.read(cx).area.get().contains(&p.at)) else { return false };
            v.update(cx, |v, cx| v.pinch(p.magnify, cx));
            return true;
        }
        if self.kind == Kind::Pdf {
            let Some(v) = self.pdf.clone().filter(|v| v.read(cx).area.get().contains(&p.at)) else { return false };
            v.update(cx, |v, cx| v.pinch(p.magnify, cx));
            return true;
        }
        if matches!(self.kind, Kind::Table | Kind::Sheet) {
            let Some(t) = self.table.clone().filter(|t| t.read(cx).area.get().contains(&p.at)) else { return false };
            t.update(cx, |t, cx| t.pinch(p.magnify, cx));
            return true;
        }
        let area = self.area.get();
        if !area.contains(&p.at) {
            return false;
        }
        if self.kind != Kind::Markdown {
            return self.pinch_image(p, cx);
        }
        let old = self.zoom;
        // 100% is as small as it gets: it's a magnifier, the page itself never changes.
        let new = (old * (1. + p.magnify)).clamp(1., PAGE_ZOOM_MAX);
        if (new - old).abs() < 1e-4 {
            return true;
        }
        if self.locked_w.is_none() {
            self.locked_w = Some(self.page_w.get() / old);
        }
        // The page is laid out centered at 100%: where it starts then, to scroll from there.
        let lead = if old <= 1. { ((area.size.width / gpui::px(1.) - self.page_w.get()) / 2.).max(0.) } else { 0. };
        self.zoom = new;
        // Sharp pictures for this zoom once the fingers pause.
        self.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(180)).await;
            this.update(cx, |v, cx| {
                v.sharp_zoom = v.zoom;
                cx.notify();
            })
            .ok();
        }));
        // The spot under the fingers stays there: it moves out by the zoom, the scroll follows.
        let off = self.scroll.offset();
        let k = new / old;
        let rel = p.at - area.origin;
        let x = (rel.x - off.x - gpui::px(lead)) * k;
        let y = (rel.y - off.y) * k;
        let to = point((rel.x - x).min(gpui::px(0.)), (rel.y - y).min(gpui::px(0.)));
        self.scroll.set_offset(to);
        if new <= 1. {
            // Back to the page as it was.
            self.locked_w = None;
            self.scroll.set_offset(point(gpui::px(0.), to.y));
        }
        cx.notify();
        true
    }

    /// Pinching a picture: it grows from its fitted size (never smaller), the spot under the
    /// fingers staying put; past the pane it scrolls both ways.
    fn pinch_image(&mut self, p: crate::pinch::Pinch, cx: &mut Context<Self>) -> bool {
        let area = self.area.get();
        let old = self.zoom;
        let new = (old * (1. + p.magnify)).clamp(1., IMAGE_ZOOM_MAX);
        if (new - old).abs() < 1e-4 {
            return true;
        }
        self.zoom = new;
        let off = self.scroll.offset();
        let k = new / old;
        let rel = p.at - area.origin;
        let x = (rel.x - off.x) * k;
        let y = (rel.y - off.y) * k;
        let max = |a: Pixels| -(a * (new - 1.));
        let to = point((rel.x - x).min(gpui::px(0.)).max(max(area.size.width)), (rel.y - y).min(gpui::px(0.)).max(max(area.size.height)));
        self.scroll.set_offset(if new <= 1. { point(gpui::px(0.), gpui::px(0.)) } else { to });
        cx.notify();
        true
    }

    /// The table view (tests).
    #[cfg(test)]
    pub fn table(&self) -> Option<Entity<crate::table::TableView>> {
        self.table.clone()
    }

    /// A PDF's page count (tests).
    #[cfg(test)]
    pub fn pdf_pages(&self, cx: &App) -> Option<usize> {
        self.pdf.as_ref().map(|p| p.read(cx).pages())
    }

    /// The table's rows and zoom (tests).
    #[cfg(test)]
    pub fn table_state(&self, cx: &App) -> Option<(usize, usize, f32)> {
        self.table.as_ref().map(|t| {
            let t = t.read(cx);
            (t.sheets[0].rows(), t.sheets[0].cols(), t.zoom)
        })
    }

    /// The page's zoom (tests).
    #[cfg(test)]
    pub fn page_zoom(&self) -> f32 {
        self.zoom
    }

    /// How many diagram pictures are decoded now (tests).
    #[cfg(test)]
    /// Diagrams shown as a placeholder, waiting to be drawn (tests).
    #[cfg(test)]
    pub fn diagrams_waiting(&self) -> usize {
        diagram_order(&self.blocks).iter().filter(|d| d.is_none()).count()
    }

    /// The diagram files on the page (tests).
    #[cfg(test)]
    pub fn diagrams_shown(&self) -> Vec<PathBuf> {
        self.diagrams.clone()
    }

    #[cfg(test)]
    pub fn pictures_decoded(&self) -> usize {
        self.decoded.len()
    }

    /// Scrolls the page to `y` from the top (tests).
    #[cfg(test)]
    pub fn scroll_page_to(&mut self, y: f32, cx: &mut Context<Self>) {
        self.scroll.set_offset(point(gpui::px(0.), gpui::px(-y)));
        cx.notify();
    }

    /// How far the page scrolls each way, and how far it can (tests).
    #[cfg(test)]
    pub fn page_scroll(&self) -> (Point<Pixels>, Point<Pixels>) {
        (self.scroll.offset(), self.scroll.max_offset())
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        // Same place in the text, at its normal size.
        let off = self.scroll.offset();
        self.scroll.set_offset(point(gpui::px(0.), off.y / self.zoom));
        self.zoom = 1.;
        self.sharp_zoom = 1.;
        self.locked_w = None;
        cx.notify();
    }

    /// Draws the diagrams the page is waiting for off the main thread, several at once, so
    /// the page opens at once; it's rebuilt with each one as it's ready.
    fn draw_pending_diagrams(&mut self, cx: &mut Context<Self>) {
        for job in PENDING.with(|p| std::mem::take(&mut *p.borrow_mut())) {
            let draw = cx.background_executor().spawn(async move { job.draw() });
            cx.spawn(async move |this, cx| {
                let _ = draw.await;
                this.update(cx, |v, cx| v.mark_stale(cx)).ok();
            })
            .detach();
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        cx.notify();
    }

    /// Frees the decoded picture (the tab is closing).
    pub fn release(&mut self, window: &mut Window, cx: &mut App) {
        release(&self.shown, window, cx);
        for d in std::mem::take(&mut self.diagrams) {
            forget_diagram(&d, window, cx);
        }
        self.decoded.clear();
        self.images.update(cx, |c, cx| c.clear(window, cx));
        give_memory_back();

    }

    fn render_image(&self, cx: &mut Context<Self>) -> AnyElement {
        let bg = if self.light { theme::c(0xf3f3f3) } else { theme::bg_editor() };
        let zoom = self.zoom;
        let area = self.area.clone();
        let size = self.area.get().size;
        // Fitted: as big as the pane allows, its proportions kept (width or height fills it,
        // the other follows). Zoomed: that, bigger, in a pane that scrolls both ways.
        let picture = img(self.shown.clone()).size_full().object_fit(ObjectFit::Contain).with_fallback(|| {
            div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child("Can't show this image").into_any_element()
        });
        let zoomed = zoom > 1.001 && size.width > gpui::px(0.);
        // Its size on screen: the biggest that fits the pane (width or height fills it, the
        // other follows), times the zoom. Set exactly: left to itself, a picture's layout takes
        // its height from its width and spills out of the pane.
        let natural = {
            let mut n = self.natural.borrow_mut();
            if n.as_ref().is_none_or(|(p, _)| *p != self.shown) {
                *n = Some((self.shown.clone(), natural_size(&self.shown)));
            }
            n.as_ref().and_then(|(_, s)| *s)
        };
        let (aw, ah) = (size.width / gpui::px(1.), size.height / gpui::px(1.));
        let fitted = natural.filter(|_| aw > 0. && ah > 0.).map(|(w, h)| {
            let k = (aw / w).min(ah / h);
            (w * k * zoom, h * k * zoom)
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("image-area")
                    .debug_selector(|| "preview-image".into())
                    .relative()
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
                    .child(
                        div()
                            .relative()
                            .size_full()
                            // The pane's size, for fitting the picture: drawn again when it changes.
                            .child(
                                canvas(
                                    move |bounds, window, _| {
                                        if area.get() != bounds {
                                            area.set(bounds);
                                            window.refresh();
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                            .child(
                                div()
                                    .id("image-scroll")
                                    .size_full()
                                    .overflow_scroll()
                                    .track_scroll(&self.scroll)
                                    .child(match fitted {
                                        // Centered while it's smaller than the pane, from the corner once it's bigger.
                                        Some((w, h)) => div()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .w(gpui::px(w.max(aw)))
                                            .h(gpui::px(h.max(ah)))
                                            .child(picture.w(gpui::px(w)).h(gpui::px(h)).flex_none()),
                                        None => div().size_full().child(picture),
                                    }),
                            ),
                    )
                    .when(zoomed, |d| {
                        d.child(
                            div()
                                .id("image-zoom")
                                .debug_selector(|| "image-zoom".into())
                                .absolute()
                                .top(px(8.))
                                .right(px(10.))
                                .px(px(8.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .bg(theme::bg_popup())
                                .border_1()
                                .border_color(theme::border_input())
                                .text_size(px(11.))
                                .text_color(theme::fg())
                                .cursor_pointer()
                                .on_click(cx.listener(|v, _, _, cx| {
                                    cx.stop_propagation();
                                    v.zoom = 1.;
                                    v.scroll.set_offset(point(gpui::px(0.), gpui::px(0.)));
                                    cx.notify();
                                }))
                                .child(format!("{}% · click to fit", (zoom * 100.).round())),
                        )
                    }),
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

    /// Frees the diagram pictures the page stopped showing (scrolled far away, zoomed past,
    /// edited), then gives the memory back to macOS once decoding has settled.
    /// Remembers the pictures that have finished loading: each covers for its diagram until
    /// the next one has, so a diagram is never blank while a sharper picture loads.
    fn note_loaded(&mut self, wanted: Vec<(PathBuf, f32, Vec<PathBuf>)>, window: &mut Window, cx: &mut Context<Self>) {
        for (diagram, step, tiles) in wanted {
            if self.ready.borrow().get(&diagram) == Some(&step) {
                continue;
            }
            let mut all = true;
            for t in &tiles {
                let res = Resource::Path(Arc::from(t.as_path()));
                let state = self.images.update(cx, |c, cx| c.load(&res, window, cx));
                all &= state.is_some_and(|r| r.is_ok());
            }
            if all {
                self.ready.borrow_mut().insert(diagram, step);
                cx.notify();
            }
        }
    }

    fn keep_pictures(&mut self, shown: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let shown: std::collections::HashSet<PathBuf> = shown.into_iter().collect();
        if shown == self.decoded {
            return;
        }
        let gone: Vec<PathBuf> = self.decoded.difference(&shown).cloned().collect();
        let blocks = self.blocks.clone();
        self.ready.borrow_mut().retain(|d, step| find_diagram(&blocks, d).is_some_and(|dd| tiles(&dd, *step).iter().all(|t| !gone.contains(&t.path))));
        self.images.update(cx, |c, cx| {
            for p in gone {
                c.remove(&Resource::Path(Arc::from(p.as_path())), window, cx);
            }
        });
        self.decoded = shown;
        self.relief = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            give_memory_back();
        }));
    }

    fn render_markdown(&self, cx: &mut Context<Self>) -> (AnyElement, Vec<PathBuf>, Vec<(PathBuf, f32, Vec<PathBuf>)>) {
        let blocks = self.blocks.clone();
        // Every size on the page follows the zoom while it's built (see `px`).
        PAGE_SCALE.with(|s| s.set(self.zoom));
        let mut r = Renderer {
            base: self.path.parent().map(Path::to_path_buf).unwrap_or_default(),
            open: {
                let weak = cx.entity().downgrade();
                Rc::new(move |path, line, _, cx| {
                    weak.update(cx, |_, cx| cx.emit(Request::OpenFile { path, line, preview: false })).ok();
                })
            },
            n: 0,
            color: theme::fg(),
            boxes: self.boxes.clone(),
            ready: self.ready.clone(),
            decoded: self.decoded.clone(),
            sharp_zoom: self.sharp_zoom,
            wanted: Vec::new(),
            images: self.images.clone(),
            shown: Vec::new(),
            // Drawn a screen and a half before they scroll in; kept until four screens away, so
            // scrolling back and forth never waits for a diagram to be drawn again.
            near: around(self.area.get(), 1.5),
            keep: around(self.area.get(), 4.),
            paras: 0,
            wrap: false,
        };
        let children: Vec<AnyElement> = blocks.iter().map(|b| r.block(b)).collect();
        let area = self.area.clone();
        let page_w = self.page_w.clone();
        let zoom = self.zoom;
        // Zoomed, the page keeps the width it had at 100% and everything (that width too) grows
        // by the zoom: lines break exactly as before, it's only bigger, and scrolls both ways.
        let locked = self.locked_w.filter(|_| zoom > 1.);
        let wide = locked.is_some_and(|w| w * zoom > self.area.get().size.width / gpui::px(1.));
        let column = div()
            .debug_selector(|| "preview-page".into())
            .child(canvas(move |b, _, _| page_w.set(b.size.width / gpui::px(1.)), |_, _, _, _| {}).absolute().size_full());
        let column = match locked {
            Some(w) => column.w(gpui::px(w * zoom)),
            // 860 wide, narrower when the pane is: a definite width either way, so tables
            // (sized against it) stay as wide as the page.
            None => column.w(px(860.)).max_w(relative(1.)),
        };
        let page = div()
            .id("markdown")
            .debug_selector(|| "preview-markdown".into())
            .size_full()
            .when(locked.is_some(), |d| d.overflow_scroll())
            .when(locked.is_none(), |d| d.overflow_y_scroll())
            .track_scroll(&self.scroll)
            .child(
                div()
                    .when(!wide, |d| d.w_full().flex().justify_center())
                    // Wider than the pane: as wide as the zoomed page, so it scrolls to its edges.
                    .when_some(locked.filter(|_| wide), |d, w| d.flex_none().w(gpui::px(w * zoom)))
                    .child(
                        column
                            .relative()
                            .flex_shrink_0()
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
            .into_any_element();
        PAGE_SCALE.with(|s| s.set(1.));
        let el = div()
            .relative()
            .size_full()
            .child(page)
            .child(canvas(move |bounds, _, _| area.set(bounds), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            // While zoomed: how much, and a click to go back.
            .when((zoom - 1.).abs() > 0.01, |d| {
                d.child(
                    div()
                        .id("preview-zoom")
                        .debug_selector(|| "preview-zoom".into())
                        .absolute()
                        .top(gpui::px(8.))
                        .right(gpui::px(14.))
                        .px(gpui::px(8.))
                        .py(gpui::px(2.))
                        .rounded(gpui::px(4.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .text_size(gpui::px(11.))
                        .text_color(theme::fg_dim())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::fg_bright()))
                        .on_click(cx.listener(|v, _, _, cx| v.reset_zoom(cx)))
                        .child(format!("{}% · click for 100%", (zoom * 100.).round())),
                )
            })
            .into_any_element();
        (el, r.shown, r.wanted)
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
        let new_theme = self.kind == Kind::Markdown && self.theme != theme::current();
        if std::mem::take(&mut self.stale) || new_theme {
            self.refresh(window, cx);
        }
        let body = match self.kind {
            Kind::Markdown => {
                let (page, shown, wanted) = self.render_markdown(cx);
                self.note_loaded(wanted, window, cx);
                self.keep_pictures(shown, window, cx);
                page
            }
            Kind::Image | Kind::Svg => self.render_image(cx),
            Kind::Doc => match &self.doc {
                Some(d) => d.clone().into_any_element(),
                None => div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child("Reading…").into_any_element(),
            },
            Kind::Pdf => self.pdf.clone().map(|p| p.into_any_element()).unwrap_or_else(|| div().into_any_element()),
            Kind::Table | Kind::Sheet => match &self.table {
                Some(t) => t.clone().into_any_element(),
                None => div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child(if self.info.is_empty() { "Reading…".to_string() } else { self.info.clone() }).into_any_element(),
            },
        };
        div().key_context("Preview").track_focus(&self.focus).size_full().bg(theme::bg_editor()).child(body)
    }
}

/// Drops a picture from the image cache and the GPU (it's shown no more, or changed).
pub(crate) fn release(path: &Path, window: &mut Window, cx: &mut App) {
    let resource = Resource::Path(Arc::from(path));
    if let Some(Ok(image)) = window.get_asset::<ImgResourceLoader>(&resource, cx) {
        window.drop_image(image).ok();
    }
    ImageSource::Resource(resource).remove_asset(cx);
}


// ---------------------------------------------------------------- memory

/// Hands memory freed by big pictures (decoded diagrams and their copies) back to macOS,
/// instead of it staying counted against the app: the app's allocator, then the system's.
fn give_memory_back() {
    unsafe { libmimalloc_sys::mi_collect(true) };
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
}

/// Deletes the temporary diagram and SVG files of earlier runs (closed or crashed).
pub fn clean_old_temp_files() {
    let ours = std::process::id().to_string();
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix("kite-mermaid-").or_else(|| name.strip_prefix("kite-svg-")) else { continue };
        let pid = rest.split('-').next().unwrap_or("");
        if pid != ours && !pid.is_empty() && name.ends_with(".svg") {
            std::fs::remove_file(e.path()).ok();
        }
    }
}

// ---------------------------------------------------------------- Mermaid

/// Diagrams drawn, by their text and colors: unchanged ones aren't drawn again while typing.
static DIAGRAMS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<u64, Result<Diagram, String>>>> = std::sync::LazyLock::new(Default::default);
/// Diagrams being drawn now (so a page rebuilt meanwhile doesn't start them again).
static DRAWING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<u64>>> = std::sync::LazyLock::new(Default::default);

thread_local! {
    /// Diagrams the page being built waits for: the preview draws them in the background.
    static PENDING: std::cell::RefCell<Vec<DiagramJob>> = Default::default();
    /// The page's diagrams before this rebuild, in order: each stands in for the one in its
    /// place until that one is drawn and loaded.
    static PREVIOUS: std::cell::RefCell<Vec<Option<Diagram>>> = Default::default();
}

/// A diagram to draw: its text, and the theme's colors (taken on the main thread).
struct DiagramJob {
    key: u64,
    src: String,
    opts: mermaid_rs_renderer::RenderOptions,
}

impl DiagramJob {
    /// Draws it to an SVG file (pure Rust, no browser), keeps the result, and returns it.
    fn draw(self) -> Result<Diagram, String> {
        let key = self.key;
        // A diagram the renderer can't handle shows its source, never takes the app down.
        let svg = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mermaid_rs_renderer::render_with_options(&self.src, self.opts)))
            .map_err(|_| "the renderer couldn't draw this diagram".to_string())
            .and_then(|r| r.map_err(|e| e.to_string().lines().next().unwrap_or("not valid Mermaid").to_string()));
        let result = svg.and_then(|svg| {
            let svg = outline_text(&emoji_in_emoji_font(&svg)).unwrap_or(svg);
            let (w, h) = svg_size(&svg).ok_or("the drawing has no size")?;
            let path = std::env::temp_dir().join(format!("kite-mermaid-{}-{key:x}.svg", std::process::id()));
            std::fs::write(&path, &svg).map_err(|e| e.to_string())?;
            Ok(Diagram { path, w, h, stand_in: None })
        });
        DIAGRAMS.lock().unwrap().insert(key, result.clone());
        DRAWING.lock().unwrap().remove(&key);
        result
    }
}

/// A Mermaid diagram in the theme's colors, if it's drawn already; otherwise it's queued
/// for the preview to draw in the background, and the page shows a placeholder meanwhile.
fn mermaid(src: &str) -> Option<Result<Diagram, String>> {
    let job = match mermaid_job(src) {
        Ok(job) => job,
        Err(key) => return DIAGRAMS.lock().unwrap().get(&key).cloned(),
    };
    if DRAWING.lock().unwrap().insert(job.key) {
        PENDING.with(|p| p.borrow_mut().push(job));
    }
    None
}

/// The job to draw a diagram, or its key when it's drawn already.
fn mermaid_job(src: &str) -> Result<DiagramJob, u64> {
    let bg = theme::bg_editor().to_rgb();
    let dark = 0.2126 * bg.r + 0.7152 * bg.g + 0.0722 * bg.b < 0.5;
    let hex = |c: Hsla| {
        let c = c.to_rgb();
        format!("#{:02x}{:02x}{:02x}", (c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8)
    };
    let key = hash(&format!("{src}\u{0}{}\u{0}{}\u{0}{}", hex(theme::bg_editor()), hex(theme::fg()), hex(theme::accent())));
    if DIAGRAMS.lock().unwrap().contains_key(&key) {
        return Err(key);
    }
    let mut opts = mermaid_rs_renderer::RenderOptions::modern();
    opts.theme = diagram_theme(dark, &hex);
    Ok(DiagramJob { key, src: src.to_string(), opts })
}

/// A diagram drawn right away (tests).
#[cfg(test)]
fn mermaid_now(src: &str) -> Result<Diagram, String> {
    match mermaid_job(src) {
        Ok(job) => job.draw(),
        Err(key) => DIAGRAMS.lock().unwrap()[&key].clone(),
    }
}

/// Diagram colors from the theme's own: its background and text, nodes tinted with its
/// accent, notes with its yellow, and the theme's hues for charts.
fn diagram_theme(dark: bool, hex: &dyn Fn(Hsla) -> String) -> mermaid_rs_renderer::Theme {
    let bg = theme::bg_editor();
    let mix = |a: Hsla, t: f32| {
        let (a, b) = (a.to_rgb(), bg.to_rgb());
        hex(gpui::Rgba { r: b.r + (a.r - b.r) * t, g: b.g + (a.g - b.g) * t, b: b.b + (a.b - b.b) * t, a: 1. }.into())
    };
    let accent = theme::accent();
    let (fill, edge) = if dark { (0.2, 0.65) } else { (0.1, 0.55) };
    let mut t = if dark { mermaid_rs_renderer::Theme::dark() } else { mermaid_rs_renderer::Theme::modern() };
    t.font_family = mermaid_rs_renderer::Theme::modern().font_family;
    t.font_size = 14.;
    t.background = hex(bg);
    t.primary_color = mix(accent, fill);
    t.primary_border_color = mix(accent, edge);
    t.primary_text_color = hex(theme::fg_bright());
    t.text_color = hex(theme::fg());
    t.line_color = hex(theme::fg_dim());
    t.secondary_color = mix(theme::git_added(), fill);
    t.tertiary_color = mix(theme::git_modified(), fill);
    t.edge_label_background = hex(bg);
    t.cluster_background = mix(theme::fg(), if dark { 0.05 } else { 0.04 });
    t.cluster_border = hex(theme::border_input());
    t.sequence_actor_fill = t.primary_color.clone();
    t.sequence_actor_border = t.primary_border_color.clone();
    t.sequence_actor_line = hex(theme::fg_muted());
    t.sequence_note_fill = mix(theme::git_modified(), fill);
    t.sequence_note_border = mix(theme::git_modified(), edge);
    t.sequence_activation_fill = mix(accent, fill * 1.6);
    t.sequence_activation_border = t.primary_border_color.clone();
    let hues = theme::lane_colors();
    let more = [accent, theme::git_added(), theme::git_modified(), theme::git_deleted(), theme::ref_remote(), theme::fg_dim()];
    t.pie_colors = std::array::from_fn(|i| if i < 6 { mix(hues[i], 0.85) } else { mix(more[i - 6], 0.6) });
    t.pie_title_text_color = hex(theme::fg_bright());
    t.pie_section_text_color = hex(theme::fg_bright());
    t.pie_legend_text_color = hex(theme::fg());
    t.pie_stroke_color = hex(bg);
    t.pie_outer_stroke_color = hex(theme::border_input());
    t
}

/// Puts each emoji in a span of Apple's emoji font, so it's drawn from that font even where
/// the text's font, shaping the whole label, gives up on it.
fn emoji_in_emoji_font(svg: &str) -> String {
    let joins = |c: char| matches!(c as u32, 0xFE0F | 0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F);
    let is_emoji = |c: char, next: Option<char>| {
        let u = c as u32;
        ((0x1F000..=0x1FAFF).contains(&u) && !(0x1F3FB..=0x1F3FF).contains(&u))
            || matches!(u, 0x231A | 0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD | 0x25FE | 0x2614 | 0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 | 0x26AA | 0x26AB | 0x26BD | 0x26BE | 0x26C4 | 0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2 | 0x26F3 | 0x26F5 | 0x26FA | 0x26FD | 0x2705 | 0x270A | 0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B | 0x2B1C | 0x2B50 | 0x2B55)
            || (u > 0x7F && next == Some('\u{FE0F}'))
    };
    let mut out = String::with_capacity(svg.len());
    let mut chars = svg.chars().peekable();
    let mut in_tag = false;
    while let Some(c) = chars.next() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {}
        }
        if in_tag || !is_emoji(c, chars.peek().copied()) {
            out.push(c);
            continue;
        }
        out.push_str("<tspan font-family=\"Apple Color Emoji\">");
        out.push(c);
        while let Some(&n) = chars.peek().filter(|&&n| joins(n)) {
            out.push(n);
            chars.next();
            if n == '\u{200D}' {
                if let Some(m) = chars.next() {
                    out.push(m);
                }
            }
        }
        out.push_str("</tspan>");
    }
    out
}

/// The system's fonts, for drawing diagrams' text (loaded the first time one is drawn).
static DIAGRAM_FONTS: std::sync::LazyLock<Arc<usvg::fontdb::Database>> = std::sync::LazyLock::new(|| {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    db.set_sans_serif_family("Helvetica");
    Arc::new(db)
});

/// The diagram with its text turned into shapes (emoji into their pictures), so it draws the
/// same anywhere. GPUI's SVG drawing, missing a character in the text's font (an emoji),
/// takes the first font that has it: macOS's last-resort font, which has a box for every
/// character, so the whole label became boxes. Here the fallback skips the system's hidden
/// fonts (named with a dot) and tries Apple's emoji font first.
fn outline_text(svg: &str) -> Option<String> {
    let fallback = usvg::FontResolver::default_fallback_selector();
    let options = usvg::Options {
        fontdb: DIAGRAM_FONTS.clone(),
        font_resolver: usvg::FontResolver {
            select_font: usvg::FontResolver::default_font_selector(),
            select_fallback: Box::new(move |c, used, db| {
                let named = |db: &usvg::fontdb::Database, keep: &dyn Fn(&str) -> bool| -> Vec<usvg::fontdb::ID> {
                    db.faces().filter(|f| !f.families.iter().any(|(n, _)| keep(n))).map(|f| f.id).collect()
                };
                // The emoji font if it has it, else any font that isn't a hidden one.
                let not_emoji = named(db, &|n| n == "Apple Color Emoji");
                let hidden = named(db, &|n| !n.starts_with('.'));
                fallback(c, &[used, &not_emoji].concat(), db).or_else(|| fallback(c, &[used, &hidden].concat(), db))
            }),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).ok()?;
    Some(tree.to_string(&usvg::WriteOptions::default()))
}

/// The page's text width.
const PAGE_W: f32 = 796.;
/// How far the page zooms in (100% is the least).
const PAGE_ZOOM_MAX: f32 = 4.;
/// How far a picture can be magnified past its fitted size.
const IMAGE_ZOOM_MAX: f32 = 12.;
/// The biggest picture a diagram is drawn at (in pixels; 4 bytes each, and decoding makes a
/// few copies for a moment): past it, it's stretched instead. A bit more while zoomed in.
const MAX_SHARP_PIXELS: f32 = 2_000_000.;
const MAX_SHARP_PIXELS_ZOOMED: f32 = 4_000_000.;

thread_local! {
    /// The page zoom while a Markdown page is being built; 1 otherwise.
    static PAGE_SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.) };
}

/// `gpui::px` times the page zoom: the Markdown page uses this one (it shadows gpui's), so
/// pinching zooms all of it.
fn px(v: f32) -> Pixels {
    gpui::px(v * PAGE_SCALE.with(|s| s.get()))
}

/// The scale a diagram is drawn at for a display scale: twice for Retina screens, in steps so
/// pinching doesn't make a new picture every frame, within the pixel budget.
fn sharp_step(d: &Diagram, scale: f32) -> f32 {
    const STEPS: [f32; 12] = [0.5, 0.75, 1., 1.5, 2., 3., 4., 6., 8., 12., 16., 24.];
    let max = if scale > (PAGE_W / d.w).min(1.) + 0.01 { MAX_SHARP_PIXELS_ZOOMED } else { MAX_SHARP_PIXELS };
    let cap = (max / (d.w * d.h).max(1.)).sqrt();
    let want = (scale * 2.).min(cap);
    // The largest step within the cap (a diagram too big even at the smallest gets the cap).
    STEPS.iter().copied().filter(|s| *s <= cap).find(|s| *s >= want).or_else(|| STEPS.iter().copied().filter(|s| *s <= cap).last()).unwrap_or(cap)
}

/// The visible area grown by `screens` of its height above and below.
fn around(area: Bounds<Pixels>, screens: f32) -> Bounds<Pixels> {
    let reach = area.size.height * screens;
    Bounds::new(point(area.left(), area.top() - reach), size(area.size.width, area.size.height + reach * 2.))
}

/// One tile of a diagram drawn at some step: its file, and where it sits in the drawing (pixels).
#[derive(Clone)]
struct Tile {
    path: PathBuf,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Tiles are at most this many pixels a side, within GPUI's standard 1024-pixel textures.
const TILE: f32 = 1024.;

/// A diagram drawn at `step`, as tiles of at most TILE pixels (each an SVG of its part, written
/// once); a diagram that fits is one tile.
fn tiles(d: &Diagram, step: f32) -> Vec<Tile> {
    let (rw, rh) = ((d.w * step).max(1.).ceil(), (d.h * step).max(1.).ceil());
    let (cols, rows) = ((rw / TILE).ceil() as usize, (rh / TILE).ceil() as usize);
    let mut svg: Option<Option<String>> = None;
    let mut out = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let (x, y) = (c as f32 * TILE, r as f32 * TILE);
            let (w, h) = ((rw - x).min(TILE), (rh - y).min(TILE));
            let path = d.path.with_extension(format!("x{}.t{c}-{r}.svg", (step * 100.).round() as u32));
            if !path.exists()
                && let Some(text) = svg.get_or_insert_with(|| std::fs::read_to_string(&d.path).ok())
            {
                std::fs::write(&path, crop_svg(text, d.w, d.h, step, (x, y, w, h))).ok();
            }
            out.push(Tile { path, x, y, w, h });
        }
    }
    out
}

/// The part (x, y, w, h, in pixels at `step`) of an SVG `w0` x `h0` drawn at `step` times its
/// size, as an SVG of its own: the same drawing seen through a smaller viewBox.
fn crop_svg(svg: &str, w0: f32, h0: f32, step: f32, (x, y, w, h): (f32, f32, f32, f32)) -> String {
    let Some(end) = svg.find('>') else { return svg.to_string() };
    let Some(start) = svg[..end].find("<svg") else { return svg.to_string() };
    let mut head = svg[start..end].to_string();
    let attr = |head: &str, name: &str| -> Option<String> {
        let at = head.find(&format!(" {name}=\""))? + name.len() + 3;
        head[at..].split('"').next().map(String::from)
    };
    let vb: Vec<f32> = attr(&head, "viewBox").map(|v| v.split([' ', ',']).filter_map(|n| n.parse().ok()).collect()).unwrap_or_default();
    let (vx, vy, vw, vh) = if vb.len() == 4 { (vb[0], vb[1], vb[2], vb[3]) } else { (0., 0., w0, h0) };
    // SVG units per pixel at this step.
    let (ux, uy) = (vw / (w0 * step), vh / (h0 * step));
    for name in ["width", "height", "viewBox", "preserveAspectRatio", "style"] {
        if let Some(at) = head.find(&format!(" {name}=\"")) {
            let rest = at + name.len() + 3;
            if let Some(close) = head[rest..].find('"') {
                head.replace_range(at..rest + close + 1, "");
            }
        }
    }
    head.push_str(&format!(" viewBox=\"{} {} {} {}\" width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"none\"", vx + x * ux, vy + y * uy, w * ux, h * uy));
    format!("{}{}{}", &svg[..start], head, &svg[end..])
}

/// An SVG's size: its width/height, else its viewBox.
fn svg_size(svg: &str) -> Option<(f32, f32)> {
    let head = &svg[..svg.find('>')?];
    let attr = |name: &str| {
        let at = head.find(&format!(" {name}=\""))? + name.len() + 3;
        head[at..].split('"').next()
    };
    let num = |v: &str| v.trim_end_matches("px").parse::<f32>().ok();
    if let (Some(w), Some(h)) = (attr("width").and_then(num), attr("height").and_then(num)) {
        return Some((w, h));
    }
    let vb: Vec<f32> = attr("viewBox")?.split([' ', ',']).filter_map(|v| v.parse().ok()).collect();
    (vb.len() == 4).then(|| (vb[2], vb[3]))
}

/// A page's diagram drawn to `file`.
/// The page's diagrams in order (none where one failed or is still being drawn).
fn diagram_order(blocks: &[Block]) -> Vec<Option<Diagram>> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Mermaid(d, _) => out.push(d.as_ref().ok().cloned()),
            Block::MermaidPending => out.push(None),
            Block::Quote(inner) => out.extend(diagram_order(inner)),
            Block::List(_, items) => out.extend(items.iter().flat_map(|i| diagram_order(&i.blocks))),
            _ => {}
        }
    }
    out
}

fn find_diagram(blocks: &[Block], file: &Path) -> Option<Diagram> {
    blocks.iter().find_map(|b| match b {
        Block::Mermaid(Ok(d), _) if d.path == file => Some(d.clone()),
        Block::Mermaid(Ok(d), _) if d.stand_in.as_ref().is_some_and(|s| s.path == file) => d.stand_in.as_deref().cloned(),
        Block::Quote(inner) => find_diagram(inner, file),
        Block::List(_, items) => items.iter().find_map(|i| find_diagram(&i.blocks, file)),
        _ => None,
    })
}

/// The diagram files a page shows (in lists and quotes too).
fn diagram_files(blocks: &[Block]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Mermaid(Ok(d), _) => {
                out.push(d.path.clone());
                out.extend(d.stand_in.as_ref().map(|s| s.path.clone()));
            }
            Block::Quote(inner) => out.extend(diagram_files(inner)),
            Block::List(_, items) => out.extend(items.iter().flat_map(|i| diagram_files(&i.blocks))),
            _ => {}
        }
    }
    out
}

/// Frees a diagram no longer shown: its decoded picture, its file, and its cache entry.
fn forget_diagram(path: &Path, window: &mut Window, cx: &mut App) {
    release(path, window, cx);
    std::fs::remove_file(path).ok();
    // Its sharp copies (name.x200.svg…).
    if let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().map(|s| s.to_string_lossy().to_string())) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{stem}.x")) && name.ends_with(".svg") {
                release(&e.path(), window, cx);
                std::fs::remove_file(e.path()).ok();
            }
        }
    }
    DIAGRAMS.lock().unwrap().retain(|_, v| v.as_ref().map(|x| x.path.as_path() != path).unwrap_or(true));
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

/// A picture's own size: from a raster file's header, or an SVG's width and height (or viewBox).
fn natural_size(path: &Path) -> Option<(f32, f32)> {
    let is_svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    let (w, h) = if is_svg {
        let data = std::fs::read(path).ok()?;
        let t = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
        (t.size().width(), t.size().height())
    } else {
        let (w, h) = image_size(path)?;
        (w as f32, h as f32)
    };
    (w > 0. && h > 0.).then_some((w, h))
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

// ---------------------------------------------------------------- Markdown elsewhere

/// Markdown parsed once and drawn in another view (the Claude chat). Diagrams stay code.
pub struct Markdown(Vec<Block>);

impl Markdown {
    /// Inline code naming a file under `root` links to it.
    pub fn parse(text: &str, root: &Path) -> Self {
        Markdown(parse_with(text, Some(root)))
    }

    /// The blocks in a column; relative links and pictures are from `base`, files open with `open`.
    pub fn render(&self, base: &Path, images: &Entity<RetainAllImageCache>, open: Opener) -> AnyElement {
        let mut r = Renderer {
            base: base.to_path_buf(),
            open,
            n: 0,
            color: theme::fg(),
            boxes: Default::default(),
            ready: Default::default(),
            decoded: Default::default(),
            sharp_zoom: 1.,
            wanted: Vec::new(),
            images: images.clone(),
            shown: Vec::new(),
            near: Bounds::default(),
            keep: Bounds::default(),
            paras: 0,
            wrap: true,
        };
        let children: Vec<AnyElement> = self.0.iter().map(|b| r.block(b)).collect();
        div().flex().flex_col().gap(px(8.)).font_family(PROSE_FONT).text_size(px(13.)).line_height(relative(1.5)).text_color(theme::fg()).children(children).into_any_element()
    }
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
    /// HTML's `<p align="center">` (or right): what's inside, aligned.
    Align(Alignment, Vec<Block>),
    /// HTML's `<img>`: where, its alt text, and the width and height it asks for.
    Img { url: String, alt: String, w: Option<f32>, h: Option<f32> },
    /// Pictures side by side (badges, a row of screenshots).
    Row(Vec<Block>),
    /// A ```mermaid fence: the drawn diagram, or why it couldn't be (and its source).
    Mermaid(Result<Diagram, String>, String),
    /// A diagram still being drawn (in the background), by its text.
    MermaidPending,
}

/// A Mermaid diagram drawn to an SVG file (temporary, freed when no longer shown).
#[derive(Clone, Debug, PartialEq)]
struct Diagram {
    path: PathBuf,
    w: f32,
    h: f32,
    /// The picture this one replaces (another theme's colors, the text before an edit): it
    /// stays on screen, at this one's size, until this one has loaded, so nothing moves or
    /// goes blank meanwhile.
    stand_in: Option<Box<Diagram>>,
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
    /// ```mermaid fences stay code (the chat draws no diagrams).
    no_diagrams: bool,
    /// Inline code naming a file in this folder becomes a link to it (the chat).
    files_in: Option<PathBuf>,
    /// Diagrams met so far.
    diagrams: usize,
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
    parse_with(text, None)
}

fn parse_with(text: &str, files_in: Option<&Path>) -> Vec<Block> {
    let mut b = Builder { no_diagrams: files_in.is_some(), files_in: files_in.map(Path::to_path_buf), ..Default::default() };
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
        let file = code && self.links.is_empty() && self.files_in.as_deref().is_some_and(|b| is_file_ref(t, b));
        let st = Style { bold: self.bold > 0, italic: self.italic > 0, strike: self.strike > 0, code, link: !self.links.is_empty() || file };
        let inl = self.inline.get_or_insert_with(Inline::default);
        let start = inl.text.len();
        inl.text.push_str(t);
        if file {
            inl.runs.push((t.len(), st));
            inl.links.push((start..inl.text.len(), t.trim().to_string()));
            return;
        }
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
                        if !self.no_diagrams && lang.as_deref().is_some_and(|l| l.eq_ignore_ascii_case("mermaid")) {
                            let src = text.trim_end().to_string();
                            let prev = PREVIOUS.with(|p| p.borrow().get(self.diagrams).cloned().flatten());
                            self.diagrams += 1;
                            self.push_block(match (mermaid(&src), prev) {
                                // Drawn: the one it replaces covers for it until it has loaded.
                                (Some(Ok(mut d)), Some(p)) => {
                                    d.stand_in = if p.path == d.path { p.stand_in } else { Some(Box::new(Diagram { stand_in: None, ..p })) };
                                    Block::Mermaid(Ok(d), src)
                                }
                                (Some(d), _) => Block::Mermaid(d, src),
                                // Being drawn: the one before shows meanwhile, else a placeholder.
                                (None, Some(p)) => Block::Mermaid(Ok(p), src),
                                (None, None) => Block::MermaidPending,
                            });
                            return;
                        }
                        let text = text.trim_end_matches('\n').replace('\t', "    ");
                        let spans = lang.as_deref().and_then(code_lang).and_then(|l| highlight::highlight(l, &text)).map(Arc::new);
                        self.push_block(Block::Code(text, spans));
                    }
                }
                TagEnd::HtmlBlock => {
                    if let Some(h) = self.html.take() {
                        for b in html_blocks(&h) {
                            self.push_block(b);
                        }
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
                } else if tag.starts_with("<img") {
                    // A picture in a line of text: shown after it, like Markdown's own.
                    self.images.extend(html_blocks(&t));
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
    /// Opens a linked file.
    open: Opener,
    n: usize,
    color: Hsla,
    /// The picture each diagram showed last (covering while a sharper loads).
    boxes: Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, Bounds<Pixels>>>>,
    ready: Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, f32>>>,
    decoded: std::collections::HashSet<PathBuf>,
    sharp_zoom: f32,
    /// Each diagram on show, the step it wants and that step's tiles: checked after drawing,
    /// to know when all of them have loaded.
    wanted: Vec<(PathBuf, f32, Vec<PathBuf>)>,
    images: Entity<RetainAllImageCache>,
    /// Pictures this page shows now, and the part of the window near the screen.
    shown: Vec<PathBuf>,
    near: Bounds<Pixels>,
    /// Diagrams that have a picture keep it while they're within this.
    keep: Bounds<Pixels>,
    /// Paragraphs drawn so far (for their test names).
    paras: usize,
    /// Code blocks wrap their long lines (the chat) instead of scrolling sideways.
    wrap: bool,
}

/// Opens a file link: the file and the line it names, if any.
pub type Opener = Rc<dyn Fn(PathBuf, Option<usize>, &mut Window, &mut App)>;

fn open_link(url: &str, base: &Path, open: &Opener, window: &mut Window, cx: &mut App) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        return cx.open_url(url);
    }
    let (rel, line) = split_file_ref(url);
    if rel.is_empty() {
        return;
    }
    let path = base.join(rel);
    if path.exists() {
        open(path, line, window, cx);
    }
}

/// `src/a.rs:12:3`, `src/a.rs#L12-20`, `a.md#part` → the path and the line (1-based).
fn split_file_ref(url: &str) -> (&str, Option<usize>) {
    let (path, frag) = url.split_once('#').unwrap_or((url, ""));
    let line = frag.strip_prefix('L').and_then(|l| l.split('-').next()?.parse().ok());
    let mut parts = path.splitn(3, ':');
    let p = parts.next().unwrap_or("");
    match parts.next().and_then(|l| l.parse().ok()) {
        Some(l) => (p, Some(l)),
        None => (path, line),
    }
}

/// Inline code naming a file under `base` (`src/a.rs`, `src/a.rs:12`): drawn as a link to it.
fn is_file_ref(t: &str, base: &Path) -> bool {
    let t = t.trim();
    let inside = |p: &Path| p.canonicalize().ok().zip(base.canonicalize().ok()).is_some_and(|(p, b)| p.starts_with(b));
    !t.is_empty() && t.len() < 300 && !t.contains(char::is_whitespace) && (t.contains('/') || t.contains('.')) && !t.contains("://") && {
        let p = base.join(split_file_ref(t).0);
        p.is_file() && inside(&p)
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
            // A tint of the text color, so code stands out on light themes as well as dark.
            background_color: st.code.then(|| theme::fg().opacity(0.1)),
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
        let (base, open) = (self.base.clone(), self.open.clone());
        InteractiveText::new(("md-text", self.id()), styled)
            .on_click(ranges, move |ix, window, cx| {
                if let Some(url) = urls.get(ix) {
                    open_link(url, &base, &open, window, cx);
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
            Block::Para(inl) => {
                let n = self.paras;
                self.paras += 1;
                div().debug_selector(move || format!("md-para-{n}")).child(self.inline(inl, false)).into_any_element()
            }
            Block::Code(text, spans) => code_block(self.id(), text, spans.as_deref(), self.wrap),
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
                // Columns share the width by how much they hold (like a browser's table), every
                // row the same, and text wraps inside its cell. Too many columns to fit: the
                // table keeps a readable width per column and scrolls sideways.
                let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
                let weights = column_weights(rows, cols);
                let table_id = self.id();
                let mut out = Vec::new();
                for (r, row) in rows.iter().enumerate() {
                    let mut cells = Vec::new();
                    for (c, weight) in weights.iter().enumerate() {
                        let a = align.get(c).copied().unwrap_or(Alignment::None);
                        let text = row.get(c).map(|cell| self.inline(cell, r == 0));
                        let selector = format!("md-cell-{table_id}-{r}-{c}");
                        let text_sel = format!("md-cell-text-{table_id}-{r}-{c}");
                        cells.push(
                            div()
                                .debug_selector(move || selector)
                                .flex_basis(px(0.))
                                .min_w(px(MIN_COL_W))
                                .px(px(10.))
                                .py(px(5.))
                                .overflow_hidden()
                                .when(c > 0, |d| d.border_l_1())
                                .border_color(theme::border())
                                .map(|mut d| {
                                    d.style().flex_grow = Some(*weight);
                                    d.style().flex_shrink = Some(*weight);
                                    d
                                })
                                .child(
                                    div()
                                        .debug_selector(move || text_sel)
                                        .w_full()
                                        .min_w_0()
                                        .line_height(relative(1.45))
                                        .when(a == Alignment::Center, |d| d.text_center())
                                        .when(a == Alignment::Right, |d| d.text_right())
                                        .children(text),
                                )
                                .into_any_element(),
                        );
                    }
                    out.push(
                        div()
                            .flex()
                            .w_full()
                            .when(r > 0, |d| d.border_t_1())
                            .border_color(theme::border())
                            .when(r == 0, |d| d.bg(theme::fg().opacity(0.05)))
                            .children(cells)
                            .into_any_element(),
                    );
                }
                div()
                    .id(("md-table", table_id))
                    .w_full()
                    .overflow_x_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(cols as f32 * MIN_COL_W))
                            .border_1()
                            .border_color(theme::border())
                            .rounded(px(4.))
                            .overflow_hidden()
                            .children(out),
                    )
                    .into_any_element()
            }
            Block::Image(url, alt) => self.image(url, alt, None, None),
            Block::Img { url, alt, w, h } => self.image(url, alt, *w, *h),
            Block::Row(items) => {
                let children: Vec<AnyElement> = items.iter().map(|b| self.block(b)).collect();
                div().flex().flex_wrap().items_center().gap(px(6.)).children(children).into_any_element()
            }
            Block::Align(a, blocks) => {
                let children: Vec<AnyElement> = blocks.iter().map(|b| self.block(b)).collect();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .when(matches!(a, Alignment::Center), |d| d.items_center().text_center())
                    .when(matches!(a, Alignment::Right), |d| d.items_end().text_right())
                    .children(children)
                    .into_any_element()
            }
            Block::Mermaid(Ok(d), _) => {
                // Fitted to the page and zoomed with it. Only diagrams on or near the screen get
                // a picture (drawn for that size, in steps); the rest are boxes of the same size,
                // and their pictures are freed. Until a sharper picture is ready, the last one
                // shows, stretched to fit.
                let fit = (PAGE_W / d.w).min(1.);
                let (w, h) = (px(d.w * fit), px(d.h * fit));
                let drawn = self.ready.borrow().contains_key(&d.path);
                let near = self.boxes.borrow().get(&d.path).is_some_and(|b| b.intersects(&self.near) || (drawn && b.intersects(&self.keep)));
                let id = self.id();
                let boxes = self.boxes.clone();
                let key = d.path.clone();
                let measure = canvas(
                    move |b, window, _| {
                        let first = boxes.borrow_mut().insert(key, b).is_none();
                        if first {
                            // Now its place is known: draw again to decide whether it's near.
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full();
                let body: AnyElement = if near {
                    // Drawn in tiles of at most TILE pixels: GPUI keeps pictures in 1024-pixel
                    // textures, and bigger ones (long diagrams, zoomed in) went missing on screen.
                    let step = sharp_step(d, fit * self.sharp_zoom);
                    let want = tiles(d, step);
                    self.wanted.push((d.path.clone(), step, want.iter().map(|t| t.path.clone()).collect()));
                    // On screen only tiles that have loaded: the last full set, stretched, while
                    // the next one loads out of sight (GPUI draws nothing while an image loads).
                    let ready_of = |x: &Diagram| self.ready.borrow().get(&x.path).copied().map(|s| (s, tiles(x, s))).filter(|(_, t)| t.iter().all(|t| self.decoded.contains(&t.path)));
                    // Its own loaded picture, else the one it replaces (stretched to its size).
                    let shown_by = ready_of(d).map(|r| (d, r)).or_else(|| d.stand_in.as_deref().and_then(|o| ready_of(o).map(|r| (o, r))));
                    let first_load = shown_by.is_none();
                    let (by, (show_step, show)) = shown_by.unwrap_or((d, (step, want.clone())));
                    self.shown.extend(show.iter().map(|t| t.path.clone()));
                    if show_step != step || by.path != d.path {
                        self.shown.extend(want.iter().map(|t| t.path.clone()));
                    }
                    let (rw, rh) = (by.w * show_step, by.h * show_step);
                    let mut layer = div().relative().w(w).h(h);
                    for (k, t) in show.into_iter().enumerate() {
                        // A hair of overlap so no seam shows between tiles.
                        let pic = img(t.path)
                            .image_cache(&self.images)
                            .object_fit(ObjectFit::Fill)
                            .absolute()
                            .left(w * (t.x / rw))
                            .top(h * (t.y / rh))
                            .w(w * (t.w / rw) + gpui::px(0.5))
                            .h(h * (t.h / rh) + gpui::px(0.5))
                            .with_fallback(move || div().debug_selector(move || format!("md-mermaid-failed-{id}")).into_any_element());
                        let pic = if first_load && k == 0 { pic.with_loading(move || div().debug_selector(move || format!("md-mermaid-blank-{id}")).into_any_element()) } else { pic };
                        layer = layer.child(pic);
                    }
                    layer.into_any_element()
                } else {
                    div().w(w).h(h).flex_none().into_any_element()
                };
                div()
                    .debug_selector(move || format!("md-mermaid-{id}"))
                    .flex()
                    .justify_center()
                    .py(px(4.))
                    .child(div().relative().flex_none().w(w).h(h).child(body).child(measure))
                    .into_any_element()
            }
            Block::MermaidPending => {
                let id = self.id();
                div()
                    .debug_selector(move || format!("md-mermaid-pending-{id}"))
                    .flex()
                    .justify_center()
                    .items_center()
                    .h(px(120.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(12.))
                    .text_color(theme::fg_muted())
                    .child("Drawing diagram…")
                    .into_any_element()
            }
            Block::Mermaid(Err(e), src) => {
                let id = self.id();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().debug_selector(move || format!("md-mermaid-error-{id}")).text_size(px(12.)).text_color(theme::git_deleted()).child(format!("Mermaid diagram not drawn: {e}")))
                    .child(code_block(self.id(), src, None, self.wrap))
                    .into_any_element()
            }
        }
    }

    /// A picture: its own size (or the `width`/`height` HTML asked for), no wider than the page.
    fn image(&mut self, url: &str, alt: &str, want_w: Option<f32>, want_h: Option<f32>) -> AnyElement {
        let local = !url.contains("://") && !url.starts_with("data:");
        let path = self.base.join(url.split(['?', '#']).next().unwrap_or(url));
        if local && path.is_file() {
            let alt = alt.to_string();
            let el = img(path.clone()).image_cache(&self.images).object_fit(ObjectFit::Contain).with_fallback(move || div().text_color(theme::fg_dim()).child(format!("[{alt}]")).into_any_element());
            let natural = image_size(&path).filter(|(w, h)| *w > 0 && *h > 0).map(|(w, h)| (w as f32, h as f32));
            let size = match (natural, want_w, want_h) {
                (Some((w, h)), Some(ww), None) => Some((ww, ww * h / w)),
                (Some((w, h)), None, Some(hh)) => Some((hh * w / h, hh)),
                (_, Some(ww), Some(hh)) => Some((ww, hh)),
                (Some(n), None, None) => Some(n),
                (None, _, _) => None,
            };
            let el = match size {
                Some((w, h)) => {
                    let scale = (760. / w).min(1.);
                    el.w(px(w * scale)).h(px(h * scale))
                }
                None => el.max_w(px(760.)),
            };
            return div().child(el).into_any_element();
        }
        // Remote pictures aren't downloaded: the alt text links to them.
        let label = format!("[image: {}]", if alt.is_empty() { url } else { alt });
        let inl = Inline { runs: vec![(label.len(), Style { link: true, ..Default::default() })], links: vec![(0..label.len(), url.to_string())], text: label };
        div().child(self.inline(&inl, false)).into_any_element()
    }
}

/// Whether an SVG file has its `<svg>` element (a fragment of one doesn't draw on its own).
fn has_svg_root(text: &str) -> bool {
    text.contains("<svg") || text.contains("<SVG")
}

/// An SVG fragment (shapes and defs, meant to go inside another drawing) in an `<svg>` of its
/// own, the size of what it draws.
fn wrap_svg_fragment(body: &str) -> String {
    const NS: &str = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#;
    let probe = format!(r#"<svg {NS} width="100000" height="100000">{body}</svg>"#);
    let bounds = usvg::Tree::from_data(probe.as_bytes(), &usvg::Options::default()).ok().map(|t| t.root().abs_stroke_bounding_box()).filter(|b| b.width() > 0. && b.height() > 0.);
    let (x, y, w, h) = match bounds {
        Some(b) => {
            let pad = b.width().max(b.height()) * 0.04;
            (b.x() - pad, b.y() - pad, b.width() + 2. * pad, b.height() + 2. * pad)
        }
        None => (0., 0., 1024., 1024.),
    };
    format!(r#"<svg {NS} viewBox="{x} {y} {w} {h}" width="{w}" height="{h}">{body}</svg>"#)
}

/// What an HTML block in Markdown shows, for the HTML READMEs use: `<p align="center">` and
/// `<div align>`, `<img src width height>`, `<br>`, `<a href>`, `<b>`/`<strong>`, `<i>`/`<em>`,
/// `<code>`, `<h1>`…`<h6>`. Other tags are left out and their text kept; comments are dropped.
fn html_blocks(html: &str) -> Vec<Block> {
    #[derive(Default)]
    struct P {
        out: Vec<Block>,
        group: Vec<Block>,
        align: Option<Alignment>,
        heading: Option<u8>,
        inline: Inline,
        style: Style,
        link: Option<(usize, String)>,
    }
    impl P {
        fn push(&mut self, t: &str) {
            // HTML runs spaces and line breaks together; none at the start of a line.
            let mut s = String::new();
            for c in t.chars() {
                let prev = s.chars().last().or_else(|| self.inline.text.chars().last());
                if c.is_whitespace() {
                    if prev.is_some_and(|p| !p.is_whitespace()) {
                        s.push(' ');
                    }
                } else {
                    s.push(c);
                }
            }
            if !s.is_empty() {
                self.inline.text.push_str(&s);
                self.inline.runs.push((s.len(), self.style.clone()));
            }
        }
        fn flush(&mut self) {
            let mut inl = std::mem::take(&mut self.inline);
            let trimmed = inl.text.trim_end().len();
            if trimmed == 0 {
                return;
            }
            // The trailing space goes, from the last runs.
            let mut cut = inl.text.len() - trimmed;
            while cut > 0 && let Some(last) = inl.runs.last_mut() {
                let take = cut.min(last.0);
                last.0 -= take;
                cut -= take;
                if last.0 == 0 {
                    inl.runs.pop();
                }
            }
            inl.text.truncate(trimmed);
            self.group.push(match self.heading {
                Some(n) => Block::Heading(n, inl),
                None => Block::Para(inl),
            });
        }
        fn end_group(&mut self) {
            self.flush();
            let mut items: Vec<Block> = Vec::new();
            for b in std::mem::take(&mut self.group) {
                match (items.last_mut(), b) {
                    (Some(Block::Row(row)), img @ Block::Img { .. }) => row.push(img),
                    (Some(prev @ Block::Img { .. }), img @ Block::Img { .. }) => {
                        let first = std::mem::replace(prev, Block::Rule);
                        *prev = Block::Row(vec![first, img]);
                    }
                    (_, b) => items.push(b),
                }
            }
            match self.align.take() {
                Some(a) if !items.is_empty() => self.out.push(Block::Align(a, items)),
                _ => self.out.extend(items),
            }
        }
    }
    fn attrs(tag: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut rest = tag.split_once(|c: char| c.is_whitespace()).map(|(_, r)| r).unwrap_or("");
        loop {
            rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
            let n = rest.find(|c: char| c == '=' || c.is_whitespace() || c == '/').unwrap_or(rest.len());
            if n == 0 {
                return out;
            }
            let key = rest[..n].to_ascii_lowercase();
            rest = rest[n..].trim_start();
            let value = if let Some(r) = rest.strip_prefix('=') {
                let r = r.trim_start();
                let (v, after) = match r.chars().next() {
                    Some(q @ ('"' | '\'')) => r[1..].split_once(q).unwrap_or((&r[1..], "")),
                    _ => r.split_at(r.find(char::is_whitespace).unwrap_or(r.len())),
                };
                rest = after;
                v.to_string()
            } else {
                String::new()
            };
            out.push((key, entities(&value)));
        }
    }
    fn entities(s: &str) -> String {
        if !s.contains('&') {
            return s.to_string();
        }
        s.replace("&nbsp;", "\u{a0}").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&copy;", "©").replace("&middot;", "·").replace("&mdash;", "—").replace("&ndash;", "–").replace("&amp;", "&")
    }
    fn size(v: Option<&String>) -> Option<f32> {
        v.and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).filter(|n| *n > 0.)
    }
    let mut p = P::default();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        let text = entities(&rest[..lt]);
        p.push(&text);
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.split_once("-->").map(|(_, r)| r).unwrap_or("");
            continue;
        }
        // The tag, to its '>' (not one inside quotes).
        let mut end = None;
        let mut quote = None;
        for (i, c) in rest.char_indices().skip(1) {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            p.push(&entities(rest));
            rest = "";
            break;
        };
        let tag = rest[1..end].trim();
        rest = &rest[end + 1..];
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("p" | "div" | "center" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "section" | "header" | "footer" | "blockquote", false) => {
                p.end_group();
                let a = attrs(tag);
                let align = a.iter().find(|(k, _)| k == "align").map(|(_, v)| v.to_ascii_lowercase());
                p.align = match align.as_deref() {
                    _ if name == "center" => Some(Alignment::Center),
                    Some("center") => Some(Alignment::Center),
                    Some("right") => Some(Alignment::Right),
                    _ => None,
                };
                p.heading = name.strip_prefix('h').and_then(|n| n.parse().ok());
            }
            ("p" | "div" | "center" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "section" | "header" | "footer" | "blockquote", true) => {
                p.end_group();
                p.heading = None;
            }
            ("br", _) => {
                p.inline.text.push('\n');
                p.inline.runs.push((1, p.style.clone()));
            }
            ("img", false) => {
                let a = attrs(tag);
                let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
                if let Some(url) = get("src") {
                    p.flush();
                    p.group.push(Block::Img { url, alt: get("alt").unwrap_or_default(), w: size(get("width").as_ref()), h: size(get("height").as_ref()) });
                }
            }
            ("a", false) => {
                let href = attrs(tag).into_iter().find(|(k, _)| k == "href").map(|(_, v)| v);
                p.link = href.map(|h| (p.inline.text.len(), h));
                p.style.link = p.link.is_some();
            }
            ("a", true) => {
                if let Some((start, href)) = p.link.take()
                    && start < p.inline.text.len()
                {
                    p.inline.links.push((start..p.inline.text.len(), href));
                }
                p.style.link = false;
            }
            ("b" | "strong", c) => p.style.bold = !c,
            ("i" | "em", c) => p.style.italic = !c,
            ("code" | "kbd", c) => p.style.code = !c,
            ("s" | "del" | "strike", c) => p.style.strike = !c,
            ("hr", _) => {
                p.end_group();
                p.out.push(Block::Rule);
            }
            _ => {}
        }
    }
    p.push(&entities(rest));
    p.end_group();
    p.out
}

/// The narrowest a table column gets before the table scrolls sideways instead.
const MIN_COL_W: f32 = 90.;

/// How much of a table's width each column gets: its longest cell, within limits, so a column
/// of short dates doesn't take as much as one of sentences (and long ones wrap).
fn column_weights(rows: &[Vec<Inline>], cols: usize) -> Vec<f32> {
    (0..cols)
        .map(|c| {
            let longest = rows.iter().filter_map(|r| r.get(c)).map(|cell| cell.text.chars().count()).max().unwrap_or(0);
            (longest as f32).clamp(6., 60.)
        })
        .collect()
}

fn string_index() -> u8 {
    (0..u8::MAX).find(|i| highlight::name(*i) == "string").unwrap_or(0)
}

fn code_block(id: usize, text: &str, spans: Option<&LineSpans>, wrap: bool) -> AnyElement {
    let lines: Vec<AnyElement> = text
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let shown = if line.is_empty() { " " } else { line };
            let runs = code_runs(shown, spans.and_then(|s| s.get(i)));
            div().when(!wrap, |d| d.whitespace_nowrap()).child(StyledText::new(shown.to_string()).with_runs(runs)).into_any_element()
        })
        .collect();
    div()
        .id(("md-code", id))
        .rounded(px(6.))
        .bg(theme::fg().opacity(0.05))
        .border_1()
        .border_color(theme::border())
        .px(px(14.))
        .py(px(10.))
        .when(!wrap, |d| d.overflow_x_scroll())
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
    /// Real files from KITE_OFFICE_FILES (one path a line): read without panicking.
    #[test]
    fn real_office_files_read() {
        let Ok(list) = std::env::var("KITE_OFFICE_FILES") else { return };
        for p in list.lines().filter(|l| !l.trim().is_empty()) {
            let path = std::path::Path::new(p.trim());
            eprintln!("-- {p}");
            if p.ends_with(".docx") {
                let d = crate::docx::read(path).unwrap();
                eprintln!("{} blocks", d.blocks.len());
            } else {
                let (s, _, _) = crate::office::xlsx(path).unwrap();
                eprintln!("{} sheets", s.len());
            }
        }
    }

    use super::{Block, Diagram, PENDING, PREVIOUS, TILE, crop_svg, emoji_in_emoji_font, image_size, mermaid_now, outline_text, parse, tiles};

    #[test]
    fn html_in_markdown_is_drawn_not_shown_as_code() {
        let readme = "<p align=\"center\">\n  <img src=\"packaging/wordmark.png\" alt=\"OSKL Code\" width=\"420\">\n</p>\n\n<p align=\"center\">\n  A fast, <b>lightweight</b> editor.<br>\n  See <a href=\"https://x.dev\">the site</a> &amp; more.\n</p>\n\n<!-- hidden -->\n\n# After";
        let b = parse(readme);
        let Block::Align(_, first) = &b[0] else { panic!("{b:?}") };
        assert!(matches!(&first[0], super::Block::Img { url, alt, w: Some(w), h: None } if url == "packaging/wordmark.png" && alt == "OSKL Code" && *w == 420.), "{first:?}");
        let Block::Align(_, second) = &b[1] else { panic!("{b:?}") };
        let Block::Para(inl) = &second[0] else { panic!("{second:?}") };
        assert_eq!(inl.text, "A fast, lightweight editor.\nSee the site & more.");
        assert_eq!(inl.links.len(), 1);
        assert_eq!(&inl.text[inl.links[0].0.clone()], "the site");
        assert!(inl.runs.iter().any(|(_, s)| s.bold), "bold kept");
        assert_eq!(inl.runs.iter().map(|r| r.0).sum::<usize>(), inl.text.len(), "the runs cover the text");
        assert!(matches!(&b[2], Block::Heading(1, h) if h.text == "After"), "the comment is dropped: {b:?}");
        // Badges side by side.
        let b = parse("<p><img src=\"a.png\"> <img src=\"b.png\"></p>");
        assert!(matches!(&b[0], super::Block::Row(r) if r.len() == 2), "{b:?}");
    }

    #[test]
    fn an_svg_fragment_is_drawn_in_an_svg_of_its_own() {
        let art = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/logo-art.svg")).unwrap();
        assert!(!super::has_svg_root(&art), "the logo's art is a fragment");
        let whole = super::wrap_svg_fragment(&art);
        let tree = usvg::Tree::from_data(whole.as_bytes(), &usvg::Options::default()).expect("draws");
        let (w, h) = (tree.size().width(), tree.size().height());
        assert!(w > 500. && w < 900. && h > 300. && h < 700., "sized to the letters: {w}×{h}");
    }

    #[test]
    fn chat_code_spans_naming_files_become_links() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(super::split_file_ref("src/a.rs:12:3"), ("src/a.rs", Some(12)));
        assert_eq!(super::split_file_ref("src/a.rs#L4-9"), ("src/a.rs", Some(4)));
        assert_eq!(super::split_file_ref("a.md#part"), ("a.md", None));
        // `../<this folder>/Cargo.toml` goes out and back in: inside the project, whatever the checkout is called.
        let back_in = format!("../{}/Cargo.toml", root.file_name().unwrap().to_string_lossy());
        let blocks = super::parse_with(&format!("See `src/main.rs:3` and `not/here.rs` and `x` and `/etc/hosts` and `{back_in}`."), Some(root));
        let Block::Para(inl) = &blocks[0] else { panic!("{blocks:?}") };
        // `/etc/hosts` is outside the project.
        assert_eq!(inl.links.iter().map(|(_, u)| u.as_str()).collect::<Vec<_>>(), ["src/main.rs:3", back_in.as_str()]);
    }

    #[test]
    fn big_diagrams_are_drawn_in_tiles_that_cover_them_exactly() {
        let dir = std::env::temp_dir().join(format!("kite-tiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.svg");
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="800" height="2600" viewBox="0 0 400 1300"><rect width="400" height="1300"/></svg>"#).unwrap();
        let d = Diagram { path: path.clone(), w: 800., h: 2600., stand_in: None };
        // At 1.5x: 1200 x 3900 pixels, so 2 x 4 tiles, none past TILE.
        let t = tiles(&d, 1.5);
        assert_eq!(t.len(), 8);
        assert!(t.iter().all(|t| t.w <= TILE && t.h <= TILE && t.w > 0. && t.h > 0.));
        let area: f32 = t.iter().map(|t| t.w * t.h).sum();
        assert_eq!(area, 1200. * 3900., "they cover it, no gaps or overlaps");
        // Each tile's SVG shows its part: the last tile starts at (1024, 3072) px, which is
        // (341.33, 1024) in this SVG's units (a third of a unit per pixel).
        let last = std::fs::read_to_string(&t[7].path).unwrap();
        assert!(last.contains(r#"viewBox="341.33334 1024 58.666668 276""#) && last.contains(r#"width="176" height="828""#), "{last}");
        // A small one is a single tile.
        assert_eq!(tiles(&d, 0.25).len(), 1);
        assert!(crop_svg("<svg width=\"10\" height=\"10\">x</svg>", 10., 10., 1., (0., 0., 10., 10.)).contains(r#"viewBox="0 0 10 10""#), "a missing viewBox is the SVG's own size");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn emoji_get_a_span_of_the_emoji_font() {
        let svg = "<svg><text x=\"1\">\u{2705} Verified \u{1F468}\u{200D}\u{1F4BB}!</text><g id=\"\u{2705}\"/></svg>";
        assert_eq!(
            emoji_in_emoji_font(svg),
            "<svg><text x=\"1\"><tspan font-family=\"Apple Color Emoji\">\u{2705}</tspan> Verified <tspan font-family=\"Apple Color Emoji\">\u{1F468}\u{200D}\u{1F4BB}</tspan>!</text><g id=\"\u{2705}\"/></svg>",
            "text only, never inside tags; joined emoji stay one"
        );
        assert_eq!(emoji_in_emoji_font("<text>a → b ✓</text>"), "<text>a → b ✓</text>", "arrows and plain marks stay in the text's font");
    }

    #[test]
    fn a_diagram_being_redrawn_keeps_showing_the_one_before() {
        let old = mermaid_now("flowchart LR\n  Old --> Pic").unwrap();
        // The text changed (or the theme): the new one is queued, the old one shows meanwhile.
        PREVIOUS.with(|p| *p.borrow_mut() = vec![Some(old.clone())]);
        let edited = "```mermaid\nflowchart LR\n  New --> Pic\n```\n";
        let b = parse(edited);
        assert!(matches!(&b[0], Block::Mermaid(Ok(d), _) if *d == old), "{:?}", b[0]);
        // Drawn: it's shown, with the old one standing in until it has loaded.
        for j in PENDING.with(|p| std::mem::take(&mut *p.borrow_mut())) {
            j.draw().ok();
        }
        let b = parse(edited);
        let Block::Mermaid(Ok(d), _) = &b[0] else { panic!("{:?}", b[0]) };
        assert_ne!(d.path, old.path);
        assert_eq!(d.stand_in.as_deref(), Some(&old));
        PREVIOUS.with(|p| p.borrow_mut().clear());
    }

    #[test]
    fn diagram_text_becomes_shapes_and_emoji_pictures() {
        // A label starting with an emoji: the one the text's font alone drew as a box.
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"40\"><text x=\"5\" y=\"30\" font-family=\"Inter, sans-serif, Apple Color Emoji\" font-size=\"14\">\u{2705} Verified</text></svg>";
        let out = outline_text(&emoji_in_emoji_font(svg)).expect("drawn");
        assert!(!out.contains("<text"), "no text left for GPUI to look fonts up for");
        assert!(out.contains("<path"), "the letters are shapes");
        assert!(out.contains("data:image/png"), "the emoji is Apple's picture of it");
        assert_eq!(super::svg_size(&out), Some((200., 40.)), "same size");
    }

    #[test]
    fn mermaid_fences_become_diagrams() {
        // This process's memory (macOS `ps`, in KB).
        let rss = || {
            let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.) / 1024.
        };
        let before = rss();
        for (kind, src) in [
            ("flowchart", "flowchart LR\n  A[Start] --> B{Ok?}\n  B -->|yes| C[Done]\n  B -->|no| A"),
            ("sequence", "sequenceDiagram\n  Alice->>Bob: Hello\n  Bob-->>Alice: Hi"),
            ("class", "classDiagram\n  Animal <|-- Duck\n  Animal : +int age"),
            ("state", "stateDiagram-v2\n  [*] --> Still\n  Still --> Moving\n  Moving --> [*]"),
            ("er", "erDiagram\n  CUSTOMER ||--o{ ORDER : places"),
            ("gantt", "gantt\n  title Plan\n  dateFormat YYYY-MM-DD\n  section A\n  Task :a1, 2026-01-01, 3d"),
            ("pie", "pie title Pets\n  \"Dogs\" : 386\n  \"Cats\" : 85"),
        ] {
            let d = mermaid_now(src).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert!(d.w > 10. && d.h > 10., "{kind}: {d:?}");
            assert!(std::fs::read_to_string(&d.path).unwrap().contains("<svg"), "{kind}");
            assert_eq!(mermaid_now(src).unwrap(), d, "{kind}: drawn once, then reused");
            std::fs::remove_file(&d.path).ok();
        }
        eprintln!("[mermaid] memory {before:.1} MB -> {:.1} MB after drawing 7 diagrams", rss());
        assert!(mermaid_now("flowchart LR\n  A --> ").is_err() || mermaid_now("this is not mermaid").is_err(), "nonsense is an error, not a panic");
        let md = "Before\n\n```mermaid\ngraph TD; A-->B\n```\n\n```mermaid\nnot a diagram at all\n```\n";
        let b = parse(md);
        assert!(matches!(&b[1], Block::MermaidPending) && matches!(&b[2], Block::MermaidPending), "not drawn while the page is built: {:?}", &b[1..]);
        let jobs = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
        assert_eq!(jobs.len(), 2, "both queued to draw");
        for j in jobs {
            j.draw().ok();
        }
        assert_eq!(parse(md).len(), b.len());
        assert!(PENDING.with(|p| p.borrow().is_empty()), "drawn ones aren't queued again");
        let b = parse(md);
        assert!(matches!(&b[1], Block::Mermaid(Ok(_), _)), "{:?}", b[1]);
        assert!(matches!(&b[2], Block::Mermaid(Err(_), src) if src == "not a diagram at all"), "the source stays, to show with the error");
    }

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
