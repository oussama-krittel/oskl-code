//! Find & replace inside the current file (⌘F / ⌥⌘F), like VS Code's find widget.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::search::{Options, SearchPanel};
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;
use std::sync::Arc;

actions!(findbar, [ReplaceAll, ToggleFindCase, ToggleFindWord, ToggleFindRegex]);

pub fn bind_keys() {
    let c = Some("FindBar");
    crate::keybindings::collect([
        crate::keybindings::kb("alt-enter", ReplaceAll, c),
        crate::keybindings::kb("alt-cmd-c", ToggleFindCase, c),
        crate::keybindings::kb("alt-cmd-w", ToggleFindWord, c),
        crate::keybindings::kb("alt-cmd-r", ToggleFindRegex, c),
    ]);
}

pub enum FindEvent {
    Close,
    Message(String),
}

impl EventEmitter<FindEvent> for FindBar {}

pub struct FindBar {
    pub query: Entity<Editor>,
    replace: Entity<Editor>,
    pub show_replace: bool,
    opts: Options,
    target: Option<WeakEntity<Editor>>,
    /// Searching a read-only diff instead of an editor (no replace there).
    diff: Option<WeakEntity<crate::diff_view::DiffView>>,
    matches: Arc<Vec<Range<usize>>>,
    current: Option<usize>,
    error: Option<String>,
    /// Where the cursor was when the bar opened; typing searches from here.
    origin: usize,
    _subs: Vec<Subscription>,
    _target_sub: Option<Subscription>,
}

fn input(placeholder: &str, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut ed = Editor::new(EditorMode::SingleLine, cx);
        ed.placeholder = placeholder.to_string().into();
        ed
    })
}

impl FindBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = input("Find", cx);
        let replace = input("Replace", cx);
        let subs = vec![
            cx.subscribe_in(&query, window, |bar, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed => bar.refresh(true, cx),
                EditorEvent::Submit | EditorEvent::Down => bar.step(true, cx),
                EditorEvent::Up => bar.step(false, cx),
                EditorEvent::Tab if bar.show_replace => bar.replace.read(cx).focus(window),
                EditorEvent::Cancel => cx.emit(FindEvent::Close),
                _ => {}
            }),
            cx.subscribe_in(&replace, window, |bar, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => bar.replace_one(cx),
                EditorEvent::Down => bar.step(true, cx),
                EditorEvent::Up => bar.step(false, cx),
                EditorEvent::Tab => bar.query.read(cx).focus(window),
                EditorEvent::Cancel => cx.emit(FindEvent::Close),
                _ => {}
            }),
        ];
        Self {
            query,
            replace,
            show_replace: false,
            opts: Options::default(),
            target: None,
            diff: None,
            matches: Arc::default(),
            current: None,
            error: None,
            origin: 0,
            _subs: subs,
            _target_sub: None,
        }
    }

    /// Opens on `target`, starting from its selection (used as the query when it's one line).
    pub fn open(&mut self, target: Entity<Editor>, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.diff.take().and_then(|d| d.upgrade()) {
            d.update(cx, |d, cx| d.set_find(None, cx));
        }
        let (sel_text, origin) = {
            let e = target.read(cx);
            let r = e.buf.sel.range();
            (e.buf.slice(r.clone()), r.start)
        };
        self.origin = origin;
        self.show_replace |= replace;
        self._target_sub = Some(cx.subscribe(&target, |bar, _, ev: &EditorEvent, cx| {
            if let EditorEvent::Changed = ev {
                bar.refresh(false, cx);
            }
        }));
        self.target = Some(target.downgrade());
        if !sel_text.is_empty() && !sel_text.contains('\n') {
            self.query.update(cx, |q, cx| q.set_text(&sel_text, cx));
        }
        let focus = if replace { self.replace.clone() } else { self.query.clone() };
        self.query.update(cx, |q, cx| {
            q.buf.select_all();
            cx.notify();
        });
        focus.read(cx).focus(window);
        self.refresh(true, cx);
    }

    /// Opens on a read-only diff: the same search, matches highlighted in its rows.
    pub fn open_diff(&mut self, target: Entity<crate::diff_view::DiffView>, window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
        self.show_replace = false;
        self.diff = Some(target.downgrade());
        self.query.update(cx, |q, cx| {
            q.buf.select_all();
            cx.notify();
        });
        self.query.read(cx).focus(window);
        self.refresh(true, cx);
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.target.as_ref().and_then(|t| t.upgrade()) {
            t.update(cx, |e, cx| e.set_find_matches(Arc::default(), None, cx));
        }
        if let Some(d) = self.diff.take().and_then(|d| d.upgrade()) {
            d.update(cx, |d, cx| d.set_find(None, cx));
        }
        self.target = None;
        self._target_sub = None;
    }

    fn target(&self) -> Option<Entity<Editor>> {
        self.target.as_ref()?.upgrade()
    }

    /// Recomputes matches. With `jump`, selects the nearest match from where the search started.
    fn refresh(&mut self, jump: bool, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text();
        self.error = None;
        if let Some(diff) = self.diff.as_ref().and_then(|d| d.upgrade()) {
            let re = match SearchPanel::matcher(&query, self.opts) {
                Ok(re) if !query.is_empty() => Some(re),
                Ok(_) => None,
                Err(e) => {
                    self.error = Some(e);
                    None
                }
            };
            let (n, current) = diff.update(cx, |d, cx| d.set_find(re, cx));
            self.matches = Arc::new(vec![0..0; n]);
            self.current = current;
            return cx.notify();
        }
        let Some(target) = self.target() else { return };
        let matches: Vec<Range<usize>> = if query.is_empty() {
            Vec::new()
        } else {
            match SearchPanel::matcher(&query, self.opts) {
                Ok(re) => {
                    let e = target.read(cx);
                    let text = e.buf.text();
                    re.find_iter(&text)
                        .filter(|m| m.start() < m.end())
                        .take(20_000)
                        .map(|m| e.buf.rope.byte_to_char(m.start())..e.buf.rope.byte_to_char(m.end()))
                        .collect()
                }
                Err(e) => {
                    self.error = Some(e);
                    Vec::new()
                }
            }
        };
        self.matches = Arc::new(matches);
        let anchor = if jump { self.origin } else { target.read(cx).buf.sel.range().start };
        self.current = self.matches.iter().position(|m| m.start >= anchor).or((!self.matches.is_empty()).then_some(0));
        if jump && let Some(i) = self.current {
            let m = self.matches[i].clone();
            target.update(cx, |e, cx| e.select_range(m, cx));
        }
        let (matches, current) = (self.matches.clone(), self.current);
        target.update(cx, |e, cx| e.set_find_matches(matches, current, cx));
        cx.notify();
    }

    /// Next (or previous) match after the cursor, wrapping around.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(diff) = self.diff.as_ref().and_then(|d| d.upgrade()) {
            self.current = diff.update(cx, |d, cx| d.find_step(forward, cx));
            return cx.notify();
        }
        let Some(target) = self.target() else { return };
        if self.matches.is_empty() {
            return;
        }
        let sel = target.read(cx).buf.sel.range();
        let n = self.matches.len();
        let ix = if forward {
            self.matches.iter().position(|m| m.start >= sel.end && m.start != sel.start).unwrap_or(0)
        } else {
            self.matches.iter().rposition(|m| m.end <= sel.start && m.start != sel.start).unwrap_or(n - 1)
        };
        self.current = Some(ix);
        let m = self.matches[ix].clone();
        let (matches, current) = (self.matches.clone(), self.current);
        target.update(cx, |e, cx| {
            e.select_range(m, cx);
            e.set_find_matches(matches, current, cx);
        });
        cx.notify();
    }

    #[cfg(test)]
    pub fn match_count(&self) -> (usize, Option<usize>) {
        (self.matches.len(), self.current)
    }

    pub fn has_query(&self, cx: &App) -> bool {
        !self.query.read(cx).text().is_empty()
    }

    fn replacement_for(&self, matched: &str, cx: &App) -> String {
        let rep = self.replace.read(cx).text();
        if !self.opts.regex {
            return rep;
        }
        match SearchPanel::matcher(&self.query.read(cx).text(), self.opts) {
            Ok(re) => re.replace(matched, rep.as_str()).into_owned(),
            Err(_) => rep,
        }
    }

    /// Replaces the current match (if the cursor is on it) and moves to the next.
    fn replace_one(&mut self, cx: &mut Context<Self>) {
        if self.diff.is_some() {
            return;
        }
        let Some(target) = self.target() else { return };
        let Some(m) = self.current.and_then(|i| self.matches.get(i)).cloned() else { return };
        let sel = target.read(cx).buf.sel.range();
        if sel != m {
            // First Enter just goes to the match, like VS Code.
            return self.step(true, cx);
        }
        let matched = target.read(cx).buf.slice(m.clone());
        let rep = self.replacement_for(&matched, cx);
        let end = m.start + rep.chars().count();
        target.update(cx, |e, cx| e.replace_range(m, &rep, cx));
        target.update(cx, |e, _| e.buf.sel = crate::buffer::Sel::at(end));
        self.refresh(false, cx);
        self.step(true, cx);
    }

    fn replace_all(&mut self, _: &ReplaceAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.diff.is_some() {
            return;
        }
        let Some(target) = self.target() else { return };
        let query = self.query.read(cx).text();
        if query.is_empty() || self.matches.is_empty() {
            return;
        }
        let Ok(re) = SearchPanel::matcher(&query, self.opts) else { return };
        let rep = self.replace.read(cx).text();
        let text = target.read(cx).buf.text();
        let count = self.matches.len();
        let new = if self.opts.regex { re.replace_all(&text, rep.as_str()).into_owned() } else { re.replace_all(&text, regex::NoExpand(&rep)).into_owned() };
        target.update(cx, |e, cx| e.apply_format(&new, cx));
        self.refresh(false, cx);
        cx.emit(FindEvent::Message(format!("Replaced {count} occurrence{}", if count == 1 { "" } else { "s" })));
    }

    fn toggle(&mut self, which: u8, cx: &mut Context<Self>) {
        match which {
            0 => self.opts.case = !self.opts.case,
            1 => self.opts.word = !self.opts.word,
            _ => self.opts.regex = !self.opts.regex,
        }
        self.refresh(true, cx);
    }

    fn opt_button(&self, id: &'static str, label: &'static str, on: bool, which: u8, tip: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(3.))
            .text_size(px(10.5))
            .font_family(theme::MONO_FONT)
            .cursor_pointer()
            .when(on, |d| d.bg(theme::frame_focused().opacity(0.25)).border_1().border_color(theme::frame_focused()).text_color(theme::fg_bright()))
            .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.bg(theme::hover())))
            .tooltip(ui::tooltip(tip))
            .on_click(cx.listener(move |b, _, _, cx| b.toggle(which, cx)))
            .child(label)
    }

    fn field(&self, ed: &Entity<Editor>, window: &Window, cx: &App) -> Div {
        let focused = ed.read(cx).is_focused(window);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .h(px(24.))
            .pl(px(4.))
            .pr(px(2.))
            .gap(px(2.))
            .bg(theme::bg_input())
            .border_1()
            .rounded(px(3.))
            .border_color(if focused { theme::focus_border() } else { theme::border_input() })
            .child(div().flex_1().min_w_0().child(ed.clone()))
    }
}

impl Render for FindBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = match (&self.error, self.matches.len(), self.current) {
            (Some(_), _, _) => "bad regex".to_string(),
            (_, 0, _) if self.has_query(cx) => "No results".to_string(),
            (_, 0, _) => String::new(),
            (_, n, Some(i)) => format!("{} of {n}", i + 1),
            (_, n, None) => format!("{n} found"),
        };
        let icon_btn = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).size(px(20.)).tooltip(ui::tooltip(tip));
        div()
            .key_context("FindBar")
            .on_action(cx.listener(Self::replace_all))
            .on_action(cx.listener(|b, _: &ToggleFindCase, _, cx| b.toggle(0, cx)))
            .on_action(cx.listener(|b, _: &ToggleFindWord, _, cx| b.toggle(1, cx)))
            .on_action(cx.listener(|b, _: &ToggleFindRegex, _, cx| b.toggle(2, cx)))
            .occlude()
            .w(px(440.))
            .p(px(5.))
            .flex()
            .gap(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(11.))
            .child(
                div()
                    .id("find-toggle-replace")
                    .flex()
                    .justify_center()
                    .w(px(16.))
                    .pt(px(5.))
                    .cursor_pointer()
                    .rounded(px(3.))
                    .hover(|s| s.bg(theme::hover()))
                    .tooltip(ui::tooltip("Toggle replace (⌥⌘F)"))
                    .on_click(cx.listener(|b, _, _, cx| {
                        b.show_replace = !b.show_replace;
                        cx.notify();
                    }))
                    .child(ui::small_icon(if self.show_replace { "chevron-down" } else { "chevron-right" }, theme::fg())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                self.field(&self.query, window, cx)
                                    .child(self.opt_button("find-case", "Aa", self.opts.case, 0, "Match case (⌥⌘C)", cx))
                                    .child(self.opt_button("find-word", "ab", self.opts.word, 1, "Whole word (⌥⌘W)", cx))
                                    .child(self.opt_button("find-regex", ".*", self.opts.regex, 2, "Regex (⌥⌘R)", cx)),
                            )
                            .child(div().w(px(62.)).flex_none().text_color(if self.error.is_some() || (self.matches.is_empty() && self.has_query(cx)) { theme::git_deleted() } else { theme::fg_dim() }).child(count))
                            .child(icon_btn("find-prev", "arrow-up", "Previous match (↑ · ⇧⌘G)").on_click(cx.listener(|b, _, _, cx| b.step(false, cx))))
                            .child(icon_btn("find-next", "arrow-down", "Next match (↓ · Enter · ⌘G)").on_click(cx.listener(|b, _, _, cx| b.step(true, cx))))
                            .child(icon_btn("find-close", "close", "Close (Esc)").on_click(cx.listener(|_, _, _, cx| cx.emit(FindEvent::Close)))),
                    )
                    .when(self.show_replace, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(self.field(&self.replace, window, cx))
                                .child(icon_btn("find-replace-one", "check", "Replace (Enter)").on_click(cx.listener(|b, _, _, cx| b.replace_one(cx))))
                                .child(
                                    div()
                                        .id("find-replace-all")
                                        .px(px(6.))
                                        .h(px(20.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .text_color(theme::fg())
                                        .hover(|s| s.bg(theme::hover()))
                                        .tooltip(ui::tooltip("Replace all (⌥Enter)"))
                                        .on_click(cx.listener(|b, _, w, cx| b.replace_all(&ReplaceAll, w, cx)))
                                        .child("All"),
                                ),
                        )
                    }),
            )
    }
}
