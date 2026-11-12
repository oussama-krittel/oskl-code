//! The text of a PDF, from macOS's PDFKit (as Preview.app selects it): what a drag between two
//! points, a double click (a word) or a triple click (a line) selects, where it is on the page
//! (to highlight), and the text to copy. Points are given as fractions of the page as shown
//! (0..1 across, 0..1 down), so they hold at any zoom.

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::path::Path;

#[link(name = "PDFKit", kind = "framework")]
unsafe extern "C" {}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct NSPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct NSSize {
    w: f64,
    h: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

const CROP_BOX: i64 = 1;

/// A place on a page: its index, and the fractions across and down the page as shown.
pub type Spot = (usize, f32, f32);

/// What's selected: its text, and on each page the boxes to highlight (fractions of the page
/// as shown: left, top, right, bottom).
#[derive(Clone, Debug, Default)]
pub struct Picked {
    pub text: String,
    pub boxes: HashMap<usize, Vec<[f32; 4]>>,
}

/// An open PDFDocument (kept while the PDF is shown, after the first press on it).
pub struct PdfText {
    doc: *mut Object,
}

impl Drop for PdfText {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.doc, release];
        }
    }
}

impl PdfText {
    pub fn open(path: &Path) -> Option<PdfText> {
        let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
        unsafe {
            let s: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
            let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: s];
            let alloc: *mut Object = msg_send![class!(PDFDocument), alloc];
            let doc: *mut Object = msg_send![alloc, initWithURL: url];
            (!doc.is_null()).then_some(PdfText { doc })
        }
    }

    fn page(&self, n: usize) -> Option<*mut Object> {
        unsafe {
            let count: usize = msg_send![self.doc, pageCount];
            if n >= count {
                return None;
            }
            let p: *mut Object = msg_send![self.doc, pageAtIndex: n];
            (!p.is_null()).then_some(p)
        }
    }

    /// The page's crop box and how it's turned (as the page is drawn).
    fn frame(page: *mut Object) -> (NSRect, i64) {
        unsafe {
            let r: NSRect = msg_send![page, boundsForBox: CROP_BOX];
            let rot: i64 = msg_send![page, rotation];
            (r, rot.rem_euclid(360))
        }
    }

    /// A place on the page as shown → PDF page space (points, from the bottom left, unturned).
    fn to_page(page: *mut Object, u: f32, v: f32) -> NSPoint {
        let (r, rot) = Self::frame(page);
        let (u, v) = (u.clamp(0., 1.) as f64, v.clamp(0., 1.) as f64);
        let (cx, cy, cw, ch) = (r.origin.x, r.origin.y, r.size.w, r.size.h);
        match rot {
            90 => NSPoint { x: cx + v * cw, y: cy + u * ch },
            180 => NSPoint { x: cx + (1. - u) * cw, y: cy + v * ch },
            270 => NSPoint { x: cx + (1. - v) * cw, y: cy + (1. - u) * ch },
            _ => NSPoint { x: cx + u * cw, y: cy + (1. - v) * ch },
        }
    }

    /// PDF page space → the fractions of the page as shown.
    fn to_view(page: *mut Object, x: f64, y: f64) -> (f32, f32) {
        let (r, rot) = Self::frame(page);
        let (fx, fy) = ((x - r.origin.x) / r.size.w.max(1e-6), (y - r.origin.y) / r.size.h.max(1e-6));
        let (u, v) = match rot {
            90 => (fy, fx),
            180 => (1. - fx, fy),
            270 => (1. - fy, 1. - fx),
            _ => (fx, 1. - fy),
        };
        (u as f32, v as f32)
    }

    /// From one place to another (either order, across pages).
    pub fn between(&self, a: Spot, b: Spot) -> Option<Picked> {
        let (pa, pb) = (self.page(a.0)?, self.page(b.0)?);
        // In reading order: the earlier page (or the higher place on the same page) first.
        let ((pa, a), (pb, b)) = if (b.0, b.2) < (a.0, a.2) { ((pb, b), (pa, a)) } else { ((pa, a), (pb, b)) };
        unsafe {
            let s: *mut Object = msg_send![self.doc, selectionFromPage: pa atPoint: Self::to_page(pa, a.1, a.2) toPage: pb atPoint: Self::to_page(pb, b.1, b.2)];
            self.read(s)
        }
    }

    /// The word (double click) or the line (triple click) there.
    pub fn around(&self, at: Spot, line: bool) -> Option<Picked> {
        let page = self.page(at.0)?;
        let p = Self::to_page(page, at.1, at.2);
        unsafe {
            let s: *mut Object = if line { msg_send![page, selectionForLineAtPoint: p] } else { msg_send![page, selectionForWordAtPoint: p] };
            self.read(s)
        }
    }

    /// ⌘A: the whole document (highlighted as each page's text area).
    pub fn everything(&self) -> Option<Picked> {
        unsafe {
            let s: *mut Object = msg_send![self.doc, selectionForEntireDocument];
            if s.is_null() {
                return None;
            }
            let text = Self::string(s);
            let mut boxes: HashMap<usize, Vec<[f32; 4]>> = HashMap::new();
            let pages: *mut Object = msg_send![s, pages];
            let n: usize = msg_send![pages, count];
            for i in 0..n {
                let page: *mut Object = msg_send![pages, objectAtIndex: i];
                let r: NSRect = msg_send![s, boundsForPage: page];
                let ix: usize = msg_send![self.doc, indexForPage: page];
                boxes.entry(ix).or_default().push(Self::view_box(page, r));
            }
            (!text.is_empty()).then_some(Picked { text, boxes })
        }
    }

    fn view_box(page: *mut Object, r: NSRect) -> [f32; 4] {
        let (u0, v0) = Self::to_view(page, r.origin.x, r.origin.y);
        let (u1, v1) = Self::to_view(page, r.origin.x + r.size.w, r.origin.y + r.size.h);
        [u0.min(u1), v0.min(v1), u0.max(u1), v0.max(v1)]
    }

    unsafe fn string(s: *mut Object) -> String {
        unsafe {
            let ns: *mut Object = msg_send![s, string];
            if ns.is_null() {
                return String::new();
            }
            let c: *const c_char = msg_send![ns, UTF8String];
            if c.is_null() { String::new() } else { CStr::from_ptr(c).to_string_lossy().into_owned() }
        }
    }

    /// The text of a selection and its lines' boxes.
    unsafe fn read(&self, s: *mut Object) -> Option<Picked> {
        if s.is_null() {
            return None;
        }
        unsafe {
            let text = Self::string(s);
            if text.trim().is_empty() {
                return None;
            }
            let mut boxes: HashMap<usize, Vec<[f32; 4]>> = HashMap::new();
            let lines: *mut Object = msg_send![s, selectionsByLine];
            let n: usize = msg_send![lines, count];
            for i in 0..n {
                let line: *mut Object = msg_send![lines, objectAtIndex: i];
                let pages: *mut Object = msg_send![line, pages];
                let m: usize = msg_send![pages, count];
                for j in 0..m {
                    let page: *mut Object = msg_send![pages, objectAtIndex: j];
                    let r: NSRect = msg_send![line, boundsForPage: page];
                    let ix: usize = msg_send![self.doc, indexForPage: page];
                    boxes.entry(ix).or_default().push(Self::view_box(page, r));
                }
            }
            Some(Picked { text, boxes })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page 400×200 with two lines of text, written by hand.
    fn sample() -> std::path::PathBuf {
        let content = "BT /F1 20 Tf 20 150 Td (Hello brave world) Tj 0 -40 Td (Second line here) Tj ET";
        let objs = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 400 200]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".to_string(),
            format!("<</Length {}>>stream\n{content}\nendstream", content.len()),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj{o}endobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1));
        for o in offsets {
            pdf.push_str(&format!("{o:010} 00000 n \n"));
        }
        pdf.push_str(&format!("trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1));
        let p = std::env::temp_dir().join(format!("oskl-text-{}.pdf", std::process::id()));
        std::fs::write(&p, pdf).unwrap();
        p
    }

    #[test]
    fn words_lines_drags_and_everything() {
        let p = sample();
        let t = PdfText::open(&p).expect("opens");
        // "brave" sits around x 80..130 of 400, the first line at y 150 of 200 (a quarter down).
        let word = t.around((0, 0.26, 0.23), false).expect("a word");
        assert_eq!(word.text, "brave");
        let b = word.boxes[&0][0];
        assert!(b[0] > 0.15 && b[2] < 0.4 && b[1] < 0.25 && b[3] > 0.2, "{b:?}");
        assert_eq!(t.around((0, 0.1, 0.43), true).expect("a line").text.trim(), "Second line here");
        let drag = t.between((0, 0.26, 0.23), (0, 0.2, 0.43)).expect("a drag");
        // From the letter under the press (inside "brave") to the one under the release.
        assert!(drag.text.contains("ve world\nSec") && !drag.text.contains("Hello") && !drag.text.contains("here"), "{:?}", drag.text);
        assert_eq!(drag.boxes[&0].len(), 2, "one box per line");
        // Backwards gives the same.
        assert_eq!(t.between((0, 0.2, 0.43), (0, 0.26, 0.23)).unwrap().text, drag.text);
        let all = t.everything().expect("everything");
        assert!(all.text.contains("Hello brave world") && all.text.contains("Second line here"));
        std::fs::remove_file(p).ok();
    }
}
