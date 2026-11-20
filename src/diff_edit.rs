//! Editable diff for working-tree changes (Index ↔ Working Tree), like VS Code:
//! the right side is a normal editor, the left side is the read-only index.

use crate::editor::{DiffRole, Editor, EditorEvent, EditorMode};
use crate::git::{self, Rev};
use crate::request::{DiffSpec, Request};
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

actions!(diff_edit, [NextChange, PrevChange, ToggleInlineDiff]);

pub fn bind_keys() {
    let c = Some("DiffEdit");
    crate::keybindings::collect([
        crate::keybindings::kb("f7", NextChange, c),
        crate::keybindings::kb("shift-f7", PrevChange, c),
        crate::keybindings::kb("alt-f5", NextChange, c),
        crate::keybindings::kb("shift-alt-f5", PrevChange, c),
        crate::keybindings::kb("cmd-k i", ToggleInlineDiff, c),
    ]);
}

pub struct DiffEditView {
    pub root: PathBuf,
    pub spec: DiffSpec,
    pub left: Entity<Editor>,
    pub right: Entity<Editor>,
    inline: bool,
    split: f32,
    dragging_split: bool,
    body_bounds: Rc<Cell<Bounds<Pixels>>>,
    _subs: Vec<Subscription>,
    _blame_task: Option<Task<()>>,
}

impl EventEmitter<Request> for DiffEditView {}

impl DiffEditView {
    pub fn new(root: PathBuf, spec: DiffSpec, email: Option<String>, blame: bool, font_size: f32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = Rc::new(Cell::new((0f32, 0u64)));
        let path = root.join(&spec.path);
        let right = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::Full, cx);
            ed.diff_role = DiffRole::Right { inline: crate::editor::diff_inline() };
            ed.changes_only = crate::editor::diff_changes_only();
            ed.scroll_link = Some(link.clone());
            ed.font_size = font_size;
            ed.blame_gutter = blame;
            ed.user_email = email.clone();
            ed.load(path.clone(), Some(root.clone()), cx);
            ed
        });
        let left = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::Full, cx);
            ed.read_only = true;
            ed.scroll_link = Some(link.clone());
            ed.font_size = font_size;
            ed.blame_gutter = blame;
            ed.user_email = email;
            ed.path = Some(path.clone());
            ed
        });
        let subs = vec![
            cx.observe(&right, |view, right, cx| {
                // Keep the read-only index side in step with the diff the right side computed.
                if let Some(d) = right.read(cx).decors.clone() {
                    view.left.update(cx, |l, cx| {
                        l.set_view_text(&d.base, cx);
                        if !l.decor.as_ref().is_some_and(|x| std::sync::Arc::ptr_eq(x, &d.left)) {
                            l.decor = Some(d.left.clone());
                            cx.notify();
                        }
                    });
                }
                cx.notify();
            }),
            cx.observe(&left, |_, _, cx| cx.notify()),
            cx.subscribe_in(&left, window, |_, _, ev: &EditorEvent, _, cx| {
                if let EditorEvent::OpenCommit(sha) = ev {
                    cx.emit(Request::OpenCommit(sha.clone()));
                }
            }),
        ];
        let mut view = Self { root, spec, left, right, inline: crate::editor::diff_inline(), split: 0.5, dragging_split: false, body_bounds: Default::default(), _subs: subs, _blame_task: None };
        view.load_left_blame(cx);
        view
    }

    pub fn title(&self) -> String {
        format!("{} (Index ↔ Working Tree)", ui::file_name(&self.spec.path))
    }

    pub fn focus_target(&self, cx: &App) -> FocusHandle {
        self.right.read(cx).focus_target(cx)
    }

    fn load_left_blame(&mut self, cx: &mut Context<Self>) {
        let (root, path) = (self.root.clone(), self.spec.path.clone());
        let task = cx.background_executor().spawn(async move {
            let text = git::read_rev(&root, &Rev::Index, &path)?;
            git::blame(&root, &path, None, Some(&text)).ok().map(|b| b.lines)
        });
        let left = self.left.clone();
        self._blame_task = Some(cx.spawn(async move |_, cx| {
            let lines = task.await;
            left.update(cx, |l, cx| l.set_blame_lines(lines, cx));
        }));
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.right.update(cx, |r, cx| {
            r.reload_if_clean(cx);
            r.refresh_git(cx);
        });
        self.load_left_blame(cx);
    }

    pub fn set_blame(&mut self, on: bool, cx: &mut Context<Self>) {
        for ed in [&self.left, &self.right] {
            ed.update(cx, |e, cx| {
                e.blame_gutter = on;
                cx.notify();
            });
        }
    }

    /// Only the changes (and a few lines around each) instead of the whole file, on both sides;
    /// every other diff follows, and the choice is saved.
    pub fn toggle_changes_only(&mut self, cx: &mut Context<Self>) {
        let on = !self.right.read(cx).changes_only;
        self.right.update(cx, |r, cx| r.set_changes_only(on, cx));
        cx.emit(Request::ChangesOnlyShown(on));
        cx.notify();
    }

    /// The button or the key: every diff follows, and the choice is saved.
    pub fn toggle_inline(&mut self, cx: &mut Context<Self>) {
        let on = !self.inline;
        self.set_inline(on, cx);
        cx.emit(Request::InlineShown(on));
    }

    #[cfg(test)]
    pub fn is_inline(&self) -> bool {
        self.inline
    }

    pub fn set_inline(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.inline == on {
            return;
        }
        self.inline = on;
        self.right.update(cx, |r, cx| r.set_inline_diff(on, cx));
        cx.notify();
    }

    /// Working-tree lines the selection covers, and whether it's just a cursor (the whole change
    /// there is meant) rather than chosen lines.
    fn picked_rows(&self, cx: &App) -> (std::ops::Range<usize>, bool) {
        let buf = &self.right.read(cx).buf;
        let r = buf.sel.range();
        let ((a, _), (b, bcol)) = (buf.pos_to_rc(r.start), buf.pos_to_rc(r.end));
        let end = if bcol == 0 && b > a { b } else { b + 1 };
        (a..end, r.is_empty())
    }

    /// Stages the change at the cursor, or only the selected lines.
    pub fn stage_part(&mut self, cx: &mut Context<Self>) {
        let (rows, whole) = self.picked_rows(cx);
        let work = self.right.read(cx).text();
        let index = git::read_rev(&self.root, &Rev::Index, &self.spec.path).unwrap_or_default();
        let staged = git::mix(&index, &work, rows, true, whole);
        if staged == index {
            return cx.emit(Request::Error("No change here to stage".into()));
        }
        match git::write_index(&self.root, &self.spec.path, &staged) {
            Ok(()) => cx.emit(Request::Message(if whole { "Staged this change".into() } else { "Staged the selected lines".into() })),
            Err(e) => cx.emit(Request::Error(format!("Stage failed: {e}"))),
        }
        self.refresh(cx);
    }

    /// Puts the change at the cursor (or the selected lines) back as staged, as an edit you can
    /// undo, and saves.
    pub fn discard_part(&mut self, cx: &mut Context<Self>) {
        let (rows, whole) = self.picked_rows(cx);
        let work = self.right.read(cx).text();
        let index = git::read_rev(&self.root, &Rev::Index, &self.spec.path).unwrap_or_default();
        let kept = git::mix(&work, &index, rows, false, whole);
        if kept == work {
            return cx.emit(Request::Error("No change here to discard".into()));
        }
        let saved = self.right.update(cx, |r, cx| {
            r.apply_format(&kept, cx);
            r.save(cx)
        });
        match saved {
            Ok(()) => cx.emit(Request::Message("Discarded (⌘Z in the diff brings it back)".into())),
            Err(e) => cx.emit(Request::Error(format!("Discard failed: {e}"))),
        }
    }

    fn step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let right = self.right.read(cx);
        let Some(d) = right.decors.clone() else { return };
        let hunks = if self.inline { &d.inline.hunks } else { &d.right.hunks };
        let (row, _) = right.cursor_rc();
        let target = if forward {
            hunks.iter().find(|h| **h > row).or(hunks.first())
        } else {
            hunks.iter().rev().find(|h| **h < row).or(hunks.last())
        };
        if let Some(&t) = target {
            self.right.update(cx, |r, cx| r.go_to(t, 0, cx));
            window.focus(&self.right.read(cx).focus_target(cx), cx);
        }
    }
}

impl Render for DiffEditView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (added, removed) = self.right.read(cx).decors.as_ref().map(|d| (d.added, d.removed)).unwrap_or((0, 0));
        // A file that isn't in the index yet has nothing on the left: show it full width.
        let one_sided = self.right.read(cx).decors.as_ref().is_some_and(|d| d.base.is_empty());
        if one_sided && !self.inline {
            self.inline = true;
            self.right.update(cx, |r, cx| r.set_inline_diff(true, cx));
        }
        let dirty = self.right.read(cx).buf.is_dirty();
        let path = self.root.join(&self.spec.path);
        let blame_on = self.right.read(cx).blame_gutter;
        let changes_only = self.right.read(cx).changes_only;
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(32.))
            .px(px(12.))
            .gap(px(8.))
            .border_b_1()
            .border_color(theme::border())
            // One line at any width: the path and labels shorten, the buttons stay.
            .child(ui::file_icon(&self.spec.path).flex_none())
            .child(div().min_w(px(40.)).overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme::MONO_FONT).text_color(theme::fg_bright()).child(self.spec.path.clone()))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_muted()).child("Index ↔ Working Tree"))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(10.5)).px(px(6.)).rounded(px(3.)).bg(theme::selected()).child(if dirty { "editing • unsaved" } else { "editable" }))
            .child(div().flex_none().text_color(theme::git_added()).child(format!("+{added}")))
            .child(div().flex_none().text_color(theme::git_deleted()).child(format!("-{removed}")))
            .child(div().flex_1())
            .child(ui::icon_button("de-stage", "plus").debug_selector(|| "de-stage".into()).tooltip(ui::tooltip("Stage the change at the cursor, or the selected lines")).on_click(cx.listener(|v, _, _, cx| v.stage_part(cx))))
            .child(ui::icon_button("de-discard", "discard").debug_selector(|| "de-discard".into()).tooltip(ui::tooltip("Discard the change at the cursor, or the selected lines (⌘Z undoes)")).on_click(cx.listener(|v, _, _, cx| v.discard_part(cx))))
            .child(ui::icon_button("de-prev", "arrow-up").on_click(cx.listener(|v, _, w, cx| v.step(false, w, cx))))
            .child(ui::icon_button("de-next", "arrow-down").on_click(cx.listener(|v, _, w, cx| v.step(true, w, cx))))
            .child(
                ui::icon_button("de-changes-only", if changes_only { "unfold-changes" } else { "fold-changes" })
                    .debug_selector(|| "de-changes-only".into())
                    .tooltip(ui::tooltip(if changes_only { "Show the whole file" } else { "Show only the changes" }))
                    .when(changes_only, |b| b.bg(theme::selected()))
                    .on_click(cx.listener(|v, _, _, cx| v.toggle_changes_only(cx))),
            )
            .when(!one_sided, |d| d.child(ui::icon_button("de-inline", if self.inline { "split" } else { "inline" }).debug_selector(|| "de-inline".into()).tooltip(ui::tooltip(if self.inline { "Show side by side" } else { "Show in one column" })).on_click(cx.listener(|v, _, _, cx| v.toggle_inline(cx)))))
            .child(
                ui::icon_button("de-blame", "blame")
                    .tooltip(ui::tooltip("Blame: who wrote each line, on both sides"))
                    .when(blame_on, |b| b.bg(theme::selected()))
                    .on_click(cx.listener(|v, _, _, cx| {
                        let on = !v.right.read(cx).blame_gutter;
                        v.set_blame(on, cx);
                        cx.emit(Request::BlameShown(on));
                    })),
            )
            .child(ui::icon_button("de-history", "history").debug_selector(|| "de-history".into()).tooltip(ui::tooltip("This file's history")).on_click(cx.listener(|v, _, _, cx| cx.emit(Request::FileHistory(v.root.join(&v.spec.path))))))
            .child(ui::icon_button("de-open", "open").tooltip(ui::tooltip("Open the file")).on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(Request::OpenFile { path: path.clone(), line: None, preview: false });
            })));

        div()
            .key_context("DiffEdit")
            .on_action(cx.listener(|v, _: &NextChange, w, cx| v.step(true, w, cx)))
            .on_action(cx.listener(|v, _: &PrevChange, w, cx| v.step(false, w, cx)))
            .on_action(cx.listener(|v, _: &ToggleInlineDiff, _, cx| v.toggle_inline(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .child(header)
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                if !v.dragging_split {
                    return;
                }
                if !ev.dragging() {
                    v.dragging_split = false;
                    return;
                }
                let b = v.body_bounds.get();
                if b.size.width > px(0.) {
                    v.split = ((ev.position.x - b.left()) / b.size.width).clamp(0.12, 0.88);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, _| v.dragging_split = false))
            .child({
                let cell = self.body_bounds.clone();
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full())
                    .when(!self.inline, |d| {
                        d.child(div().flex_none().w(relative(self.split)).min_w_0().h_full().child(self.left.clone())).child(
                            // Drag to resize old vs new.
                            div()
                                .id("de-divider")
                                .flex()
                                .flex_none()
                                .justify_center()
                                .w(px(5.))
                                .h_full()
                                .cursor(CursorStyle::ResizeLeftRight)
                                .hover(|s| s.bg(theme::frame_focused().opacity(0.35)))
                                .on_mouse_down(MouseButton::Left, cx.listener(|v, _, _, cx| {
                                    v.dragging_split = true;
                                    cx.stop_propagation();
                                }))
                                .child(div().w(px(1.)).h_full().bg(theme::border())),
                        )
                    })
                    .child(div().flex_1().min_w_0().h_full().child(self.right.clone()))
            })
    }
}
