//! PDF files, drawn by macOS (CoreGraphics). Only the pages on screen are drawn, at the
//! size they're shown; pages scrolled away are let go, so a long PDF costs a few pages.

use crate::theme;
use crate::ui::px;
use gpui::prelude::*;
use gpui::{Bounds, Context, ListAlignment, ListOffset, ListState, Pixels, RenderImage, Task, Window, div, img, list};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGAffineTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

type Ref = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDataProviderCreateWithFilename(name: *const std::ffi::c_char) -> Ref;
    fn CGDataProviderRelease(p: Ref);
    fn CGPDFDocumentCreateWithProvider(p: Ref) -> Ref;
    fn CGPDFDocumentRelease(d: Ref);
    fn CGPDFDocumentGetNumberOfPages(d: Ref) -> usize;
    fn CGPDFDocumentGetPage(d: Ref, n: usize) -> Ref;
    fn CGPDFDocumentIsUnlocked(d: Ref) -> bool;
    fn CGPDFPageGetBoxRect(p: Ref, b: i32) -> CGRect;
    fn CGPDFPageGetRotationAngle(p: Ref) -> i32;
    fn CGPDFPageGetDrawingTransform(p: Ref, b: i32, rect: CGRect, rotate: i32, preserve: bool) -> CGAffineTransform;
    fn CGColorSpaceCreateDeviceRGB() -> Ref;
    fn CGColorSpaceRelease(c: Ref);
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, space: Ref, info: u32) -> Ref;
    fn CGContextRelease(c: Ref);
    fn CGContextSetRGBFillColor(c: Ref, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(c: Ref, r: CGRect);
    fn CGContextScaleCTM(c: Ref, sx: f64, sy: f64);
    fn CGContextConcatCTM(c: Ref, t: CGAffineTransform);
    fn CGContextDrawPDFPage(c: Ref, p: Ref);
}

const CROP_BOX: i32 = 1;
/// Premultiplied alpha first, 32-bit little endian: BGRA in memory, what GPUI draws.
const BGRA: u32 = 2 | (2 << 12);
/// The widest a page is drawn (pixels): sharp enough, and a page stays a few MB.
const MAX_PX: f32 = 1800.;
/// Pages kept drawn besides the ones on screen.
const KEEP: usize = 1;

/// An open PDF (CoreGraphics' document), usable from the drawing thread.
struct Doc(Ref);
unsafe impl Send for Doc {}

impl Drop for Doc {
    fn drop(&mut self) {
        unsafe { CGPDFDocumentRelease(self.0) }
    }
}

impl Doc {
    fn open(path: &Path) -> Result<(Doc, Vec<(f32, f32)>), String> {
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "bad path")?;
        unsafe {
            let provider = CGDataProviderCreateWithFilename(c.as_ptr());
            if provider.is_null() {
                return Err("can't read the file".into());
            }
            let doc = CGPDFDocumentCreateWithProvider(provider);
            CGDataProviderRelease(provider);
            if doc.is_null() {
                return Err("not a PDF macOS can read".into());
            }
            let doc = Doc(doc);
            if !CGPDFDocumentIsUnlocked(doc.0) {
                return Err("this PDF is protected by a password".into());
            }
            let n = CGPDFDocumentGetNumberOfPages(doc.0);
            let sizes = (1..=n)
                .map(|i| {
                    let page = CGPDFDocumentGetPage(doc.0, i);
                    let r = CGPDFPageGetBoxRect(page, CROP_BOX);
                    let (w, h) = (r.size.width as f32, r.size.height as f32);
                    if CGPDFPageGetRotationAngle(page).rem_euclid(180) == 90 { (h, w) } else { (w, h) }
                })
                .collect();
            Ok((doc, sizes))
        }
    }

    /// Page `n` (from 0) drawn `width` pixels wide, as BGRA rows.
    fn draw(&self, n: usize, width: u32) -> Option<(u32, u32, Vec<u8>)> {
        unsafe {
            let page = CGPDFDocumentGetPage(self.0, n + 1);
            if page.is_null() {
                return None;
            }
            let r = CGPDFPageGetBoxRect(page, CROP_BOX);
            let rot = CGPDFPageGetRotationAngle(page).rem_euclid(360);
            let (pw, ph) = if rot % 180 == 90 { (r.size.height, r.size.width) } else { (r.size.width, r.size.height) };
            if pw <= 0. || ph <= 0. {
                return None;
            }
            let scale = width as f64 / pw;
            let (w, h) = (width as usize, (ph * scale).round().max(1.) as usize);
            let mut data = vec![0u8; w * h * 4];
            let space = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(data.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, space, BGRA);
            CGColorSpaceRelease(space);
            if ctx.is_null() {
                return None;
            }
            // Paper white, then the page, scaled (the transform turns and places it at 100%).
            CGContextSetRGBFillColor(ctx, 1., 1., 1., 1.);
            CGContextFillRect(ctx, CGRect { origin: CGPoint::default(), size: CGSize { width: w as f64, height: h as f64 } });
            CGContextScaleCTM(ctx, scale, scale);
            let t = CGPDFPageGetDrawingTransform(page, CROP_BOX, CGRect { origin: CGPoint::default(), size: CGSize { width: pw, height: ph } }, 0, true);
            CGContextConcatCTM(ctx, t);
            CGContextDrawPDFPage(ctx, page);
            CGContextRelease(ctx);
            Some((w as u32, h as u32, data))
        }
    }
}

pub struct PdfView {
    pub path: PathBuf,
    doc: Option<Arc<Mutex<Doc>>>,
    /// Each page's size in points (turned as it's shown).
    sizes: Vec<(f32, f32)>,
    pub error: Option<String>,
    list: ListState,
    /// Drawn pages: (pixel width, picture).
    drawn: HashMap<usize, (u32, Arc<RenderImage>)>,
    drawing: HashSet<usize>,
    /// Pages to let go of (the GPU's copy is freed when next drawn).
    dropped: Vec<Arc<RenderImage>>,
    /// Pages laid out in the last frame.
    on_screen: Rc<std::cell::RefCell<Vec<usize>>>,
    pub zoom: f32,
    /// The zoom pages are drawn for (it follows the zoom once a pinch pauses).
    sharp_zoom: f32,
    settle: Option<Task<()>>,
    pub area: Rc<std::cell::Cell<Bounds<Pixels>>>,
    width: f32,
    scale: f32,
    /// The text, read by PDFKit on the first press (selecting, copying).
    text: Option<crate::pdf_text::PdfText>,
    /// What's selected, where the press started, and whether it's being dragged.
    picked: Option<crate::pdf_text::Picked>,
    press: Option<crate::pdf_text::Spot>,
    dragging: bool,
    /// Where each page on screen was last drawn (to find the place under the pointer).
    boxes: Rc<std::cell::RefCell<HashMap<usize, Bounds<Pixels>>>>,
}

impl PdfView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> PdfView {
        let mut v = PdfView {
            path,
            doc: None,
            sizes: Vec::new(),
            error: None,
            list: ListState::new(0, ListAlignment::Top, gpui::px(1000.)),
            drawn: HashMap::new(),
            drawing: HashSet::new(),
            dropped: Vec::new(),
            on_screen: Default::default(),
            zoom: 1.,
            sharp_zoom: 1.,
            settle: None,
            area: Default::default(),
            width: 0.,
            scale: 2.,
            text: None,
            picked: None,
            press: None,
            dragging: false,
            boxes: Default::default(),
        };
        v.reload(cx);
        v
    }

    /// Opens the file again (it changed).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match Doc::open(&self.path) {
            Ok((doc, sizes)) => {
                self.doc = Some(Arc::new(Mutex::new(doc)));
                self.sizes = sizes;
                self.error = None;
            }
            Err(e) => {
                self.doc = None;
                self.sizes.clear();
                self.error = Some(e);
            }
        }
        self.dropped.extend(self.drawn.drain().map(|(_, (_, i))| i));
        (self.text, self.picked, self.press) = (None, None, None);
        self.list.reset(self.sizes.len());
        cx.notify();
    }

    #[cfg(test)]
    pub fn pages(&self) -> usize {
        self.sizes.len()
    }

    /// The selected text (⌘C).
    pub fn selected_text(&self) -> String {
        self.picked.as_ref().map(|p| p.text.clone()).unwrap_or_default()
    }

    /// ⌘A: the whole document.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.text_reader() {
            self.picked = t.everything();
        }
        cx.notify();
    }

    fn text_reader(&mut self) -> Option<&crate::pdf_text::PdfText> {
        if self.text.is_none() && self.error.is_none() {
            self.text = crate::pdf_text::PdfText::open(&self.path);
        }
        self.text.as_ref()
    }

    /// The page and the place on it under `pos` (fractions of the page). Off the pages (while
    /// dragging): the nearest page, at its edge.
    fn spot(&self, pos: gpui::Point<Pixels>, nearest: bool) -> Option<crate::pdf_text::Spot> {
        let boxes = self.boxes.borrow();
        let frac = |n: usize, b: &Bounds<Pixels>| (n, ((pos.x - b.left()) / b.size.width).clamp(0., 1.), ((pos.y - b.top()) / b.size.height).clamp(0., 1.));
        if let Some((n, b)) = boxes.iter().find(|(_, b)| b.contains(&pos)) {
            return Some(frac(*n, b));
        }
        if !nearest {
            return None;
        }
        let dist = |b: &Bounds<Pixels>| if pos.y < b.top() { b.top() - pos.y } else if pos.y > b.bottom() { pos.y - b.bottom() } else { gpui::px(0.) };
        boxes.iter().min_by(|a, b| dist(a.1).partial_cmp(&dist(b.1)).unwrap_or(std::cmp::Ordering::Equal)).map(|(n, b)| frac(*n, b))
    }

    /// A press on a page: starts a selection there; twice selects the word, three times the
    /// line; with shift, the selection reaches here.
    fn press(&mut self, ev: &gpui::MouseDownEvent, cx: &mut Context<Self>) {
        let Some(at) = self.spot(ev.position, false) else {
            self.picked = None;
            return cx.notify();
        };
        let anchor = self.press;
        let Some(t) = self.text_reader() else { return };
        self.picked = match ev.click_count {
            1 if ev.modifiers.shift && anchor.is_some() => anchor.and_then(|a| t.between(a, at)),
            1 => None,
            2 => t.around(at, false),
            _ => t.around(at, true),
        };
        if ev.click_count == 1 && !(ev.modifiers.shift && anchor.is_some()) {
            self.press = Some(at);
        }
        self.dragging = ev.click_count == 1;
        cx.notify();
    }

    fn drag(&mut self, ev: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.dragging || !ev.dragging() {
            return;
        }
        let (Some(a), Some(b)) = (self.press, self.spot(ev.position, true)) else { return };
        let picked = self.text.as_ref().and_then(|t| t.between(a, b));
        self.picked = picked;
        cx.notify();
    }

    /// How wide a page is shown (points), at the zoom.
    fn page_w(&self) -> f32 {
        (self.width - 48.).clamp(200., 900.) * self.zoom
    }

    /// Pinch: magnifies from the fitted size (half to five times), sharp once it pauses.
    pub fn pinch(&mut self, magnify: f32, cx: &mut Context<Self>) {
        let old = self.zoom;
        self.zoom = (old * (1. + magnify)).clamp(0.5, 5.);
        if (self.zoom - old).abs() > 1e-4 {
            self.relayout();
            self.settle = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                this.update(cx, |v, cx| {
                    v.sharp_zoom = v.zoom;
                    cx.notify();
                })
                .ok();
            }));
        }
        cx.notify();
    }

    fn relayout(&mut self) {
        let top = self.list.logical_scroll_top();
        self.list.reset(self.sizes.len());
        self.list.scroll_to(ListOffset { item_ix: top.item_ix, offset_in_item: gpui::px(0.) });
    }

    /// Draws page `n` off the main thread, at the width it's shown (on this screen's pixels).
    fn draw(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else { return };
        let px_w = (self.width_for(self.sharp_zoom) * self.scale).min(MAX_PX).round() as u32;
        if self.drawing.contains(&n) || self.drawn.get(&n).is_some_and(|(w, _)| (*w as f32 - px_w as f32).abs() < 4.) {
            return;
        }
        self.drawing.insert(n);
        let work = cx.background_executor().spawn(async move { doc.lock().ok()?.draw(n, px_w) });
        cx.spawn(async move |this, cx| {
            let page = work.await;
            this.update(cx, |v, cx| {
                v.drawing.remove(&n);
                if let Some((w, h, data)) = page
                    && let Some(buf) = image::RgbaImage::from_raw(w, h, data)
                {
                    let pic = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(buf)]));
                    if let Some((_, old)) = v.drawn.insert(n, (w, pic)) {
                        v.dropped.push(old);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn width_for(&self, zoom: f32) -> f32 {
        (self.width - 48.).clamp(200., 900.) * zoom
    }

    fn render_page(&mut self, n: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.on_screen.borrow_mut().push(n);
        let (pw, ph) = self.sizes.get(n).copied().unwrap_or((612., 792.));
        let w = self.page_w();
        let h = w * ph / pw;
        if self.drawn.get(&n).is_none_or(|(dw, _)| (*dw as f32 - (self.width_for(self.sharp_zoom) * self.scale).min(MAX_PX)).abs() >= 4.) {
            self.draw(n, cx);
        }
        let pic = self.drawn.get(&n).map(|(_, p)| p.clone());
        let boxes = self.boxes.clone();
        // The selected text on this page, highlighted as in Preview.
        let marks: Vec<[f32; 4]> = self.picked.as_ref().and_then(|p| p.boxes.get(&n).cloned()).unwrap_or_default();
        div()
            .w_full()
            .flex()
            .justify_center()
            .py(px(8.))
            .child(
                div()
                    .debug_selector(move || format!("pdf-page-{n}"))
                    .relative()
                    .w(gpui::px(w))
                    .h(gpui::px(h))
                    .flex_none()
                    .bg(theme::c(0xffffff))
                    .shadow_md()
                    .cursor_text()
                    .child(gpui::canvas(move |b, _, _| { boxes.borrow_mut().insert(n, b); }, |_, _, _, _| {}).absolute().size_full())
                    .children(pic.map(|p| img(p).size_full()))
                    .children(marks.into_iter().map(move |[l, t, r, b]| {
                        div().absolute().left(gpui::px(l * w)).top(gpui::px(t * h)).w(gpui::px((r - l) * w)).h(gpui::px((b - t) * h)).bg(theme::c(0x3390ff).opacity(0.35))
                    })),
            )
            .into_any_element()
    }
}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Pages scrolled far away are let go of (their memory, and the GPU's copy).
        let seen: Vec<usize> = std::mem::take(&mut *self.on_screen.borrow_mut());
        if let (Some(lo), Some(hi)) = (seen.iter().min(), seen.iter().max()) {
            let (lo, hi) = (lo.saturating_sub(KEEP), hi + KEEP);
            let far: Vec<usize> = self.drawn.keys().copied().filter(|n| *n < lo || *n > hi).collect();
            for n in far {
                if let Some((_, p)) = self.drawn.remove(&n) {
                    self.dropped.push(p);
                }
            }
        }
        for p in self.dropped.drain(..) {
            window.drop_image(p).ok();
        }
        self.scale = window.scale_factor();
        let width = self.area.get().size.width / gpui::px(1.);
        if (width - self.width).abs() > 1. && width > 0. {
            self.width = width;
            self.relayout();
        }
        if let Some(e) = &self.error {
            return div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child(format!("Can't show this PDF: {e}."));
        }
        let area = self.area.clone();
        let first = self.list.logical_scroll_top().item_ix + 1;
        // Pages drawn this frame put their places back.
        self.boxes.borrow_mut().clear();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::c(0x3a3d41))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|v, ev: &gpui::MouseDownEvent, _, cx| v.press(ev, cx)))
            .on_mouse_move(cx.listener(|v, ev: &gpui::MouseMoveEvent, _, cx| v.drag(ev, cx)))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|v, _, _, _| v.dragging = false))
            .child(
                div()
                    .debug_selector(|| "pdf".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        gpui::canvas(
                            move |b, window, _| {
                                if area.get() != b {
                                    area.set(b);
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
                    .child(list(self.list.clone(), cx.processor(|v: &mut PdfView, ix: usize, _, cx| v.render_page(ix, cx))).size_full()),
            )
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
                    .bg(theme::bg_editor())
                    .text_size(px(11.))
                    .text_color(theme::fg_dim())
                    .child(format!("Page {} of {}", first.min(self.sizes.len()), self.sizes.len()))
                    .when((self.zoom - 1.).abs() > 0.01, |d| d.child(format!("{}%", (self.zoom * 100.).round()))),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_are_drawn_at_the_width_asked() {
        // A one-page PDF, written by hand.
        let pdf = "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Contents 4 0 R>>endobj\n4 0 obj<</Length 26>>stream\n0 0 1 rg 0 0 100 100 re f\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";
        let p = std::env::temp_dir().join(format!("oskl-{}.pdf", std::process::id()));
        std::fs::write(&p, pdf).unwrap();
        let (doc, sizes) = super::Doc::open(&p).unwrap();
        assert_eq!(sizes, [(200., 100.)]);
        let (w, h, data) = doc.draw(0, 400).unwrap();
        assert_eq!((w, h), (400, 200));
        // BGRA: the left half is blue, the right half white.
        let px = |x: usize, y: usize| &data[(y * 400 + x) * 4..(y * 400 + x) * 4 + 4];
        assert_eq!(px(50, 100), [255, 0, 0, 255]);
        assert_eq!(px(350, 100), [255, 255, 255, 255]);
        std::fs::remove_file(p).ok();
    }
}
