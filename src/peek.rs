//! The change peek: a click on a change mark in the gutter opens a small card under that line
//! with what the lines were in the index and what they are now, and Stage / Revert / Open diff
//! for just that change.
//! A child of `editor`, so it works on the editor's private state.

use super::*;

/// The card that is open: the change it shows and where it hangs.
pub(super) struct Peek {
    /// Lines of the index the change replaced, and lines of the buffer it became.
    old: Range<usize>,
    new: Range<usize>,
    /// The old lines' text, and the first of the new lines'.
    old_text: Vec<String>,
    new_text: Vec<String>,
    /// The buffer this was made from; the card goes away when the text changes.
    version: u64,
    at: Point<Pixels>,
}

/// The change (index lines → buffer lines) that `row` of `current` belongs to. A deletion belongs
/// to the line it sat before.
pub(super) fn hunk_at(base: &str, current: &str, row: usize) -> Option<(Range<usize>, Range<usize>)> {
    use similar::{DiffTag, TextDiff};
    let diff = TextDiff::configure().timeout(Duration::from_millis(200)).diff_lines(base, current);
    diff.ops().iter().filter(|op| op.tag() != DiffTag::Equal).map(|op| (op.old_range(), op.new_range())).find(|(_, new)| if new.is_empty() { new.start == row } else { new.contains(&row) })
}

/// The lines `mix` should look at for a change: an empty range (a deletion) is hit by the line after it.
fn rows_of(new: &Range<usize>) -> Range<usize> {
    new.start..new.end.max(new.start + 1)
}

/// How many removed (and, apart, added) lines the card lists before it says how many more there are.
const SHOWN: usize = 12;

impl Editor {
    /// A click at `y` in the strip of change marks: opens the card for that change, or closes it
    /// when it was already open for it. False when there is no change on that line.
    pub(super) fn click_change_mark(&mut self, y: Pixels, was_open: Option<Peek>, cx: &mut Context<Self>) -> bool {
        let row = self.row_for_y(y);
        if self.marks.get(row).is_none_or(|m| *m == 0) || self.path.is_none() || self.root.is_none() {
            return false;
        }
        let Some(base) = self.base_text.clone() else { return false };
        let text = self.buf.text();
        let Some((old, new)) = hunk_at(&base, &text, row) else { return false };
        if was_open.is_some_and(|p| p.new == new && p.old == old) {
            cx.notify();
            return true;
        }
        let old_text = base.lines().skip(old.start).take(old.len()).map(str::to_string).collect();
        let new_text = (new.start..new.end.min(new.start + SHOWN)).map(|r| self.buf.line(r)).collect();
        // shortcut: the card floats over the lines below the change (it doesn't push them down), upgrade by giving the editor a row that takes room.
        // It hangs under the last line of the change (and its wrapped part), so it never covers the changed lines.
        let last = rows_of(&new).end - 1;
        let Some(at) = self.anchor_below(self.buf.rc_to_pos(last, usize::MAX)).zip(self.layout.as_ref()).map(|(a, l)| point(px(l.text_left), a.y)) else { return false };
        self.peek = Some(Peek { old, new, old_text, new_text, version: self.buf.version(), at });
        cx.notify();
        true
    }

    /// Whether the change card is open.
    #[cfg(test)]
    pub fn peek_open(&self) -> bool {
        self.peek_current().is_some()
    }

    /// Where the change mark of `row` was drawn in the last layout.
    #[cfg(test)]
    pub fn mark_spot(&self, row: usize) -> Option<Point<Pixels>> {
        self.marks.get(row).filter(|m| **m != 0)?;
        let (l, a) = (self.layout.as_ref()?, self.anchor_below(self.buf.rc_to_pos(row, 0))?);
        Some(point(px(l.text_left - 23.), a.y - px(l.line_h / 2.)))
    }

    /// The card, unless the text changed since it opened.
    fn peek_current(&self) -> Option<&Peek> {
        self.peek.as_ref().filter(|p| p.version == self.buf.version())
    }

    /// The index's text of this file, read fresh: what staging and reverting mix with.
    fn index_text(&self) -> Option<String> {
        git::read_rev(self.root.as_ref()?, &git::Rev::Index, &self.rel_path()?)
    }

    fn stage_peek(&mut self, cx: &mut Context<Self>) {
        let Some(new) = self.peek_current().map(|p| p.new.clone()) else { return };
        self.peek = None;
        let (Some(root), Some(rel), Some(index)) = (self.root.clone(), self.rel_path(), self.index_text()) else { return };
        let staged = git::mix(&index, &self.buf.text(), rows_of(&new), true, true);
        if staged == index {
            cx.emit(EditorEvent::Notice("No change here to stage".into()));
        } else {
            match git::write_index(&root, &rel, &staged) {
                Ok(()) => cx.emit(EditorEvent::Notice("Staged this change".into())),
                Err(e) => cx.emit(EditorEvent::Notice(format!("Stage failed: {e}"))),
            }
            self.refresh_base(cx);
        }
        cx.notify();
    }

    /// Puts the index's lines back, as an edit that ⌘Z takes back.
    fn revert_peek(&mut self, cx: &mut Context<Self>) {
        let Some(new) = self.peek_current().map(|p| p.new.clone()) else { return };
        self.peek = None;
        let Some(index) = self.index_text() else { return };
        let work = self.buf.text();
        let kept = git::mix(&work, &index, rows_of(&new), false, true);
        if kept != work {
            self.apply_format(&kept, cx);
        }
        cx.notify();
    }

    fn peek_open_diff(&mut self, cx: &mut Context<Self>) {
        self.peek = None;
        cx.emit(EditorEvent::OpenDiff);
        cx.notify();
    }

    pub(super) fn peek_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.peek_current()?;
        let (old, added) = (p.old_text.len(), p.new.len());
        let title = match (old, added) {
            (0, n) => format!("Added {n} {}", if n == 1 { "line" } else { "lines" }),
            (_, 0) => format!("Removed {old} {}", if old == 1 { "line" } else { "lines" }),
            _ => format!("Changed, was {old} {}", if old == 1 { "line" } else { "lines" }),
        };
        let button = |id: &'static str, icon: &'static str, label: &'static str, tip: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .h(px(22.))
                .px(px(7.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(theme::fg())
                .hover(|s| s.bg(theme::hover()))
                .tooltip(crate::ui::tooltip(tip))
                .child(crate::ui::small_icon(icon, theme::fg_dim()))
                .child(label)
        };
        // A run of lines under the title: removed ones struck through, added ones plain.
        let lines = |id: &'static str, text: &[String], total: usize, sign: &'static str, sign_color: Hsla, bg: Hsla, removed: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex()
                .flex_col()
                .pb(px(4.))
                .font_family(theme::MONO_FONT)
                .text_size(px(self.font_size.max(10.)))
                .children(text.iter().take(SHOWN).map(|l| {
                    let shown = if l.is_empty() { " ".to_string() } else { l.replace('\t', "    ") };
                    div()
                        .flex()
                        .gap(px(8.))
                        .px(px(10.))
                        .bg(bg)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(div().flex_none().text_color(sign_color).child(sign))
                        .child(div().min_w_0().overflow_hidden().when(removed, |d| d.text_color(theme::fg_dim()).line_through()).child(shown))
                }))
                .when(total > SHOWN, |d| d.child(div().px(px(10.)).text_color(theme::fg_muted()).child(format!("… and {} more", total - SHOWN))))
        };
        Some(
            deferred(
                anchored().position(p.at).snap_to_window().child(
                    div()
                        .id("peek-card")
                        .debug_selector(|| "peek-card".into())
                        .occlude()
                        .min_w(px(300.))
                        .max_w(px(640.))
                        .flex()
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .rounded(px(6.))
                        .overflow_hidden()
                        .shadow_lg()
                        .font_family(theme::UI_FONT)
                        .text_size(px(12.))
                        .text_color(theme::fg())
                        .child(div().w(px(3.)).flex_none().bg(theme::gutter_modified()))
                        .child(div().flex().flex_col().flex_1().min_w_0().child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(4.))
                                .py(px(3.))
                                .pl(px(10.))
                                .pr(px(4.))
                                .child(div().flex_1().min_w(px(90.)).text_color(theme::fg_dim()).child(title))
                                .child(button("peek-stage", "plus", "Stage", "Stage only this change").on_click(cx.listener(|ed, _, _, cx| ed.stage_peek(cx))))
                                .when(!self.read_only, |d| d.child(button("peek-revert", "discard", "Revert", "Put the old lines back (⌘Z undoes it)").on_click(cx.listener(|ed, _, _, cx| ed.revert_peek(cx)))))
                                .child(button("peek-diff", "open", "Open diff", "Open this file's changes in the editable diff").on_click(cx.listener(|ed, _, _, cx| ed.peek_open_diff(cx))))
                                .child(
                                    crate::ui::icon_button("peek-close", "close")
                                        .debug_selector(|| "peek-close".into())
                                        .tooltip(crate::ui::tooltip("Close (Esc)"))
                                        .on_click(cx.listener(|ed, _, _, cx| {
                                            ed.peek = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(old > 0, |d| d.child(lines("peek-old", &p.old_text, old, "−", theme::git_deleted(), theme::diff_del_line(), true)))
                        .when(added > 0, |d| d.child(lines("peek-new", &p.new_text, added, "+", theme::git_added(), theme::diff_ins_line(), false)))),
                ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::hunk_at;

    #[test]
    fn the_change_a_row_belongs_to() {
        let base = "a\nb\nc\nd\n";
        let now = "a\nB\nc\nx\ny\nd\n";
        assert_eq!(hunk_at(base, now, 1), Some((1..2, 1..2)), "a changed line");
        assert_eq!(hunk_at(base, now, 4), Some((3..3, 3..5)), "added lines have nothing before");
        assert_eq!(hunk_at(base, now, 0), None, "unchanged");
        // The deleted line sat before row 1 ("c").
        assert_eq!(hunk_at("a\nb\nc\n", "a\nc\n", 1), Some((1..2, 1..1)));
    }
}
