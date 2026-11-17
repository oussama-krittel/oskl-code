//! A Markdown box with GitHub's Write and Preview: the text as typed, or as it will read on
//! GitHub (kept up while it's typed).

use crate::editor::{Editor, EditorEvent};
use crate::preview::{Markdown, Opener};
use crate::theme;
use crate::ui::px;
use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;
use std::rc::Rc;

pub struct MdPreview {
    source: Entity<Editor>,
    /// Where relative pictures and links are read from.
    root: PathBuf,
    md: Rc<Markdown>,
    empty: bool,
    images: Entity<RetainAllImageCache>,
    /// Preview shown instead of the box.
    pub previewing: bool,
    height: f32,
    /// The box's name for the tests.
    selector: &'static str,
    _sub: Subscription,
}

impl MdPreview {
    /// `source`'s box, `height` tall, with Write and Preview above it.
    pub fn new(source: Entity<Editor>, root: PathBuf, height: f32, selector: &'static str, cx: &mut Context<Self>) -> Self {
        // It grows with its text (as on GitHub): the page scrolls, not a box inside it.
        source.update(cx, |e, _| e.max_rows = 100_000);
        let sub = cx.subscribe(&source, |v, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                v.parse(cx);
            }
        });
        let mut v = Self { source, root, md: Rc::new(Markdown::parse_doc("")), empty: true, images: RetainAllImageCache::new(cx), previewing: false, height, selector, _sub: sub };
        v.parse(cx);
        v
    }

    /// Reads the box again (also once a diagram or picture it shows is ready).
    pub fn parse(&mut self, cx: &mut Context<Self>) {
        let text = self.source.read(cx).text();
        self.empty = text.trim().is_empty();
        self.md = Rc::new(Markdown::parse_doc(&text));
        Markdown::fetch_waiting(cx, |v: &mut Self, cx| {
            v.parse(cx);
            cx.notify();
        });
        cx.notify();
    }
}

impl Render for MdPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |id: &'static str, label: &'static str, preview: bool, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(12.))
                .h(px(28.))
                .flex()
                .items_center()
                .cursor_pointer()
                .rounded_t(px(5.))
                .border_1()
                .border_b_0()
                .border_color(if on { theme::border_input() } else { gpui::transparent_black() })
                .when(on, |d| d.bg(theme::bg_input()))
                .text_color(if on { theme::fg_bright() } else { theme::fg_dim() })
                .hover(|s| s.text_color(theme::fg_bright()))
                .child(label)
                .on_click(cx.listener(move |v, _, window, cx| {
                    v.previewing = preview;
                    if !preview {
                        window.focus(&v.source.focus_handle(cx), cx);
                    }
                    cx.notify();
                }))
        };
        let body: AnyElement = if self.previewing {
            let opener: Opener = Rc::new(|path, _, _, _| {
                let _ = std::process::Command::new("open").arg(path).spawn();
            });
            div()
                .id("md-preview")
                .debug_selector(|| "md-preview".into())
                .w_full()
                .min_h(px(self.height))
                .px(px(12.))
                .py(px(8.))
                .bg(theme::bg_editor())
                .child(if self.empty { div().text_color(theme::fg_muted()).child("Nothing to preview").into_any_element() } else { self.md.render(&self.root, &self.images, opener).into_any_element() })
                .into_any_element()
        } else {
            let sel = self.selector;
            // A click below the text still writes in the box.
            div()
                .id("md-write")
                .debug_selector(move || sel.into())
                .w_full()
                .min_h(px(self.height))
                .p(px(6.))
                .bg(theme::bg_input())
                .cursor_text()
                .on_click(cx.listener(|v, _, window, cx| window.focus(&v.source.focus_handle(cx), cx)))
                .child(self.source.clone())
                .into_any_element()
        };
        let previewing = self.previewing;
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(div().flex().gap(px(2.)).child(tab("md-tab-write", "Write", false, !previewing, cx)).child(tab("md-tab-preview", "Preview", true, previewing, cx)))
            .child(div().min_h(px(self.height)).rounded_b(px(5.)).rounded_tr(px(5.)).border_1().border_color(theme::border_input()).overflow_hidden().child(body))
    }
}
