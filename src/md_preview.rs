//! A Markdown box's text, drawn as it will read on GitHub, kept up while it's typed.

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
    _sub: Subscription,
}

impl MdPreview {
    pub fn new(source: Entity<Editor>, root: PathBuf, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&source, |v, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                v.parse(cx);
            }
        });
        let mut v = Self { source, root, md: Rc::new(Markdown::parse_doc("")), empty: true, images: RetainAllImageCache::new(cx), _sub: sub };
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
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let opener: Opener = Rc::new(|path, _, _, _| {
            let _ = std::process::Command::new("open").arg(path).spawn();
        });
        div()
            .id("md-preview")
            .debug_selector(|| "md-preview".into())
            .size_full()
            .overflow_y_scroll()
            .px(px(10.))
            .py(px(6.))
            .child(if self.empty { div().text_color(theme::fg_muted()).child("Nothing to preview yet.").into_any_element() } else { self.md.render(&self.root, &self.images, opener).into_any_element() })
    }
}

/// A Markdown box beside its preview, GitHub's Write and Preview at once: `editor` (already in
/// its box) on the left, what it will read like on the right.
pub fn side_by_side(editor: AnyElement, preview: Entity<MdPreview>, height: f32) -> Div {
    let pane = |label: &'static str, child: AnyElement| {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(4.))
            .child(div().text_size(px(10.5)).text_color(theme::fg_muted()).child(label))
            .child(div().flex_1().min_h_0().h(px(height)).rounded(px(4.)).border_1().border_color(theme::border_input()).overflow_hidden().child(child))
    };
    div().flex().gap(px(10.)).w_full().child(pane("Write", editor)).child(pane("Preview", div().size_full().bg(theme::bg_editor()).child(preview).into_any_element()))
}
