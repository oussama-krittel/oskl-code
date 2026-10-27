//! 3-way merge editor: Incoming (theirs) and Current (ours) side by side with a checkbox per
//! conflict, and the editable Result underneath. "Accept Merge" saves and stages the file.

use crate::buffer::{Buffer, Conflict};
use crate::editor::Editor;
use crate::request::Request;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;

/// One conflict as it was when the merge editor opened.
struct Block {
    conflict: Conflict,
    ours: String,
    theirs: String,
}

#[derive(Clone, Copy, Default)]
struct Choice {
    current: bool,
    incoming: bool,
    /// Incoming was ticked first, so it goes first when both are taken.
    incoming_first: bool,
}

pub struct MergeView {
    focus: FocusHandle,
    pub path: PathBuf,
    pub rel: String,
    snapshot: String,
    blocks: Vec<Block>,
    choices: Vec<Choice>,
    pub result: Entity<Editor>,
    /// What the checkboxes last produced; if the result differs, it was edited by hand.
    generated: String,
    current_label: String,
    incoming_label: String,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for MergeView {}

impl MergeView {
    pub fn new(path: PathBuf, rel: String, root: PathBuf, font_size: f32, cx: &mut Context<Self>) -> Self {
        let result = cx.new(|cx| {
            let mut ed = Editor::open(path.clone(), Some(root), cx);
            ed.font_size = font_size;
            ed
        });
        let snapshot = result.read(cx).text();
        let buf = Buffer::new(&snapshot);
        let blocks: Vec<Block> = buf
            .conflicts()
            .into_iter()
            .map(|c| {
                let (ours, theirs) = buf.conflict_sides(&c);
                Block { conflict: c, ours, theirs }
            })
            .collect();
        let label = |row: Option<usize>, fallback: &str| {
            row.map(|r| buf.line(r).get(7..).unwrap_or("").trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback.to_string())
        };
        let current_label = label(blocks.first().map(|b| b.conflict.start), "HEAD");
        let incoming_label = label(blocks.first().map(|b| b.conflict.end), "incoming");
        let subs = vec![cx.observe(&result, |_, _, cx| cx.notify())];
        Self {
            focus: cx.focus_handle(),
            path,
            rel,
            choices: vec![Choice::default(); blocks.len()],
            generated: snapshot.clone(),
            snapshot,
            blocks,
            result,
            current_label,
            incoming_label,
            _subs: subs,
        }
    }

    pub fn title(&self) -> String {
        format!("Merging {}", ui::file_name(&self.rel))
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.result.read(cx).focus(window);
    }

    /// The snapshot with every ticked conflict replaced by its chosen side(s).
    fn build(&self) -> String {
        let lines: Vec<&str> = self.snapshot.split_inclusive('\n').collect();
        let mut out = String::with_capacity(self.snapshot.len());
        let mut row = 0;
        let mut next = 0;
        while row < lines.len() {
            if let Some(b) = self.blocks.get(next).filter(|b| b.conflict.start == row) {
                let ch = self.choices[next];
                next += 1;
                if ch.current || ch.incoming {
                    let parts = if ch.incoming_first { [(ch.incoming, &b.theirs), (ch.current, &b.ours)] } else { [(ch.current, &b.ours), (ch.incoming, &b.theirs)] };
                    for (take, text) in parts {
                        if take {
                            out.push_str(text);
                        }
                    }
                    row = b.conflict.end + 1;
                    if row >= lines.len() && !lines[b.conflict.end].ends_with('\n') && out.ends_with('\n') {
                        out.pop();
                    }
                    continue;
                }
            }
            out.push_str(lines[row]);
            row += 1;
        }
        out
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let text = self.build();
        self.generated = text.clone();
        self.result.update(cx, |e, cx| e.apply_format(&text, cx));
        cx.notify();
    }

    fn edited_by_hand(&self, cx: &App) -> bool {
        self.result.read(cx).text() != self.generated
    }

    pub(crate) fn toggle(&mut self, ix: usize, incoming: bool, cx: &mut Context<Self>) {
        if self.edited_by_hand(cx) {
            return cx.emit(Request::Error("The result was edited by hand — Reset to use the checkboxes again".into()));
        }
        let ch = &mut self.choices[ix];
        if incoming {
            ch.incoming = !ch.incoming;
            if ch.incoming && !ch.current {
                ch.incoming_first = true;
            }
        } else {
            ch.current = !ch.current;
            if ch.current && !ch.incoming {
                ch.incoming_first = false;
            }
        }
        self.apply(cx);
    }

    fn take_all(&mut self, incoming: bool, cx: &mut Context<Self>) {
        if self.edited_by_hand(cx) {
            return cx.emit(Request::Error("The result was edited by hand — Reset to use the checkboxes again".into()));
        }
        for ch in &mut self.choices {
            *ch = Choice { current: !incoming, incoming, incoming_first: incoming };
        }
        self.apply(cx);
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.choices = vec![Choice::default(); self.blocks.len()];
        self.apply(cx);
    }

    pub(crate) fn accept(&mut self, cx: &mut Context<Self>) {
        let left = self.result.update(cx, |e, _| {
            e.refresh_conflicts();
            e.conflicts.len()
        });
        if left > 0 {
            return cx.emit(Request::Error(format!("{left} conflict{} left in the result", if left == 1 { "" } else { "s" })));
        }
        match self.result.update(cx, |e, cx| e.save(cx)) {
            Ok(()) => cx.emit(Request::MarkResolved(self.rel.clone())),
            Err(e) => cx.emit(Request::Error(format!("Couldn't save: {e}"))),
        }
    }

    fn side(&self, incoming: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, label, head) = if incoming {
            ("Incoming", self.incoming_label.clone(), theme::ca(0x3794ff4d))
        } else {
            ("Current", self.current_label.clone(), theme::ca(0x2ea0434d))
        };
        let cards = self.blocks.iter().enumerate().map(|(ix, b)| {
            let ch = self.choices[ix];
            let on = if incoming { ch.incoming } else { ch.current };
            let text = if incoming { &b.theirs } else { &b.ours };
            div()
                .flex()
                .flex_col()
                .mb(px(8.))
                .rounded(px(4.))
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .id(SharedString::from(format!("merge-{incoming}-{ix}")))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .h(px(24.))
                        .bg(head)
                        .cursor_pointer()
                        .on_click(cx.listener(move |v, _, _, cx| v.toggle(ix, incoming, cx)))
                        .child(
                            div()
                                .size(px(13.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(2.))
                                .border_1()
                                .border_color(theme::fg_dim())
                                .when(on, |d| d.bg(theme::accent()).text_color(theme::c(0xffffff)).child("✓")),
                        )
                        .child(if incoming { "Accept Incoming" } else { "Accept Current" })
                        .child(div().flex_1())
                        .child(div().text_color(theme::fg_muted()).child(format!("conflict {}", ix + 1))),
                )
                .child(
                    div()
                        .p(px(8.))
                        .font_family(theme::MONO_FONT)
                        .text_size(px(11.5))
                        .children(text.trim_end_matches('\n').split('\n').map(|l| div().min_h(px(17.)).whitespace_nowrap().child(l.replace('\t', "    ")))),
                )
        });
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .h(px(26.))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_color(theme::fg_bright()).child(title))
                    .child(div().text_color(theme::fg_muted()).overflow_hidden().whitespace_nowrap().child(label)),
            )
            .child(
                div()
                    .id(if incoming { "merge-incoming" } else { "merge-current" })
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .p(px(8.))
                    .children(cards),
            )
    }
}

impl Focusable for MergeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MergeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let resolved = self.choices.iter().filter(|c| c.current || c.incoming).count();
        let left = {
            let e = self.result.read(cx);
            e.buf.text().lines().filter(|l| l.starts_with("<<<<<<<")).count()
        };
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .px(px(10.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .cursor_pointer()
                .when(primary, |d| d.bg(theme::accent()).text_color(theme::c(0xffffff)).hover(|s| s.bg(theme::accent_hover())))
                .when(!primary, |d| d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())))
                .child(label)
        };
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .text_size(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .h(px(34.))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_color(theme::fg_bright()).child(self.rel.clone()))
                    .child(div().text_color(theme::fg_muted()).child(if left == 0 {
                        "no conflicts left".to_string()
                    } else {
                        format!("{resolved} of {} ticked · {left} left in result", self.blocks.len())
                    }))
                    .child(div().flex_1())
                    .child(button("merge-all-incoming", "Take all Incoming", false).on_click(cx.listener(|v, _, _, cx| v.take_all(true, cx))))
                    .child(button("merge-all-current", "Take all Current", false).on_click(cx.listener(|v, _, _, cx| v.take_all(false, cx))))
                    .child(button("merge-reset", "Reset", false).on_click(cx.listener(|v, _, _, cx| v.reset(cx))))
                    .child(button("merge-accept", "Accept Merge", true).on_click(cx.listener(|v, _, _, cx| v.accept(cx)))),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.side(true, cx))
                    .child(div().w(px(1.)).bg(theme::border()))
                    .child(self.side(false, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(10.))
                    .h(px(26.))
                    .border_t_1()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_color(theme::fg_bright()).child("Result"))
                    .child(div().pl(px(8.)).text_color(theme::fg_muted()).child("editable · F7 next conflict")),
            )
            .child(div().flex_1().min_h_0().child(self.result.clone()))
    }
}

