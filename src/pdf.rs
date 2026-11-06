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
        self.list.reset(self.sizes.len());
        cx.notify();
    }

    #[cfg(test)]
    pub fn pages(&self) -> usize {
        self.sizes.len()
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
        div()
            .w_full()
            .flex()
            .justify_center()
            .py(px(8.))
            .child(
                div()
                    .w(gpui::px(w))
                    .h(gpui::px(h))
                    .flex_none()
                    .bg(theme::c(0xffffff))
                    .shadow_md()
                    .children(pic.map(|p| img(p).size_full())),
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
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::c(0x3a3d41))
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
        let p = std::env::temp_dir().join(format!("kite-{}.pdf", std::process::id()));
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
