//! Side-by-side (or inline) diff with word-level highlights and blame on both sides.

use crate::git::{self, BlameCommit, Rev};
use crate::request::{DiffSpec, Request};
use crate::{theme, ui, util};
use gpui::prelude::*;
use gpui::*;
use similar::{ChangeTag, DiffOp, TextDiff};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

actions!(diff_view, [NextRow, PrevRow, NextHunk, PrevHunk, ToggleBlame, ToggleInline, OpenFile, CopyRow, Top, Bottom, PageDownRows, PageUpRows]);

pub fn bind_keys() {
    let c = Some("DiffView");
    crate::keybindings::collect([
        crate::keybindings::kb("j", NextRow, c),
        crate::keybindings::kb("down", NextRow, c),
        crate::keybindings::kb("k", PrevRow, c),
        crate::keybindings::kb("up", PrevRow, c),
        crate::keybindings::kb("n", NextHunk, c),
        crate::keybindings::kb("alt-down", NextHunk, c),
        crate::keybindings::kb("f7", NextHunk, c),
        crate::keybindings::kb("p", PrevHunk, c),
        crate::keybindings::kb("shift-n", PrevHunk, c),
        crate::keybindings::kb("alt-up", PrevHunk, c),
        crate::keybindings::kb("shift-f7", PrevHunk, c),
        crate::keybindings::kb("b", ToggleBlame, c),
        crate::keybindings::kb("i", ToggleInline, c),
        crate::keybindings::kb("enter", OpenFile, c),
        crate::keybindings::kb("o", OpenFile, c),
        crate::keybindings::kb("cmd-c", CopyRow, c),
        crate::keybindings::kb("g", Top, c),
        crate::keybindings::kb("shift-g", Bottom, c),
        crate::keybindings::kb("space", PageDownRows, c),
        crate::keybindings::kb("pagedown", PageDownRows, c),
        crate::keybindings::kb("pageup", PageUpRows, c),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Equal,
    Delete,
    Insert,
    Change,
}

#[derive(Clone)]
struct Row {
    kind: Kind,
    l: Option<usize>,
    r: Option<usize>,
    l_hl: Vec<Range<usize>>,
    r_hl: Vec<Range<usize>>,
}

type BlameLines = Vec<Option<Arc<BlameCommit>>>;

struct Loaded {
    left: Vec<String>,
    right: Vec<String>,
    side: Vec<Row>,
    inline: Vec<Row>,
}

pub struct DiffView {
    focus: FocusHandle,
    root: PathBuf,
    pub spec: DiffSpec,
    data: Option<Arc<Loaded>>,
    error: Option<String>,
    left_blame: Option<Arc<BlameLines>>,
    right_blame: Option<Arc<BlameLines>>,
    pub show_blame: bool,
    inline: bool,
    /// Width of the old (left) side, 0..1; drag the divider to change it.
    split: f32,
    dragging_split: bool,
    body_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    scroll_x: f32,
    user_email: Option<String>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<Request> for DiffView {}

impl DiffView {
    pub fn new(root: PathBuf, spec: DiffSpec, user_email: Option<String>, show_blame: bool, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus: cx.focus_handle(),
            root,
            spec,
            data: None,
            error: None,
            left_blame: None,
            right_blame: None,
            show_blame,
            inline: false,
            split: 0.5,
            dragging_split: false,
            body_bounds: Default::default(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            scroll_x: 0.,
            user_email,
            _tasks: Vec::new(),
        };
        view.reload(cx);
        view
    }

    pub fn title(&self) -> String {
        let name = ui::file_name(&self.spec.path);
        format!("{name} ({} ↔ {})", self.spec.left.label(), self.spec.right.label())
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let spec = self.spec.clone();
        let load = cx.background_executor().spawn(async move {
            let left_path = spec.old_path.clone().unwrap_or(spec.path.clone());
            let left = git::read_rev(&root, &spec.left, &left_path);
            let right = git::read_rev(&root, &spec.right, &spec.path);
            match (left, right) {
                (Some(l), Some(r)) => Ok(compute(&l, &r)),
                _ => Err("Binary file or missing revision — nothing to show.".to_string()),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                match result {
                    Ok(data) => {
                        let first_hunk = data.side.iter().position(|r| r.kind != Kind::Equal);
                        view.data = Some(Arc::new(data));
                        view.error = None;
                        if view.selected.is_none()
                            && let Some(ix) = first_hunk
                        {
                            view.selected = Some(ix);
                            view.scroll.scroll_to_item(ix.saturating_sub(5), ScrollStrategy::Top);
                        }
                    }
                    Err(e) => view.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks = vec![task];
        if self.show_blame {
            self.load_blame(cx);
        }
    }

    fn load_blame(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let spec = self.spec.clone();
        let task = cx.background_executor().spawn(async move {
            let left_path = spec.old_path.clone().unwrap_or(spec.path.clone());
            (blame_side(&root, &spec.left, &left_path), blame_side(&root, &spec.right, &spec.path))
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let (l, r) = task.await;
            this.update(cx, |view, cx| {
                view.left_blame = l.map(Arc::new);
                view.right_blame = r.map(Arc::new);
                cx.notify();
            })
            .ok();
        }));
    }

    /// New or deleted files have only one side: show it full width instead of half empty.
    fn one_sided(&self) -> bool {
        self.spec.left == Rev::Empty || self.spec.right == Rev::Empty
    }

    fn show_inline(&self) -> bool {
        self.inline || self.one_sided()
    }

    fn rows(&self) -> &[Row] {
        match &self.data {
            Some(d) if self.show_inline() => &d.inline,
            Some(d) => &d.side,
            None => &[],
        }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let ix = ix.min(n - 1);
        self.selected = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        cx.notify();
    }

    fn next_row(&mut self, _: &NextRow, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.map(|s| s + 1).unwrap_or(0);
        self.select(ix, cx);
    }
    fn prev_row(&mut self, _: &PrevRow, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.map(|s| s.saturating_sub(1)).unwrap_or(0);
        self.select(ix, cx);
    }
    fn page_down(&mut self, _: &PageDownRows, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.unwrap_or(0) + 30;
        self.select(ix, cx);
    }
    fn page_up(&mut self, _: &PageUpRows, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.unwrap_or(0).saturating_sub(30);
        self.select(ix, cx);
    }
    fn top(&mut self, _: &Top, _: &mut Window, cx: &mut Context<Self>) {
        self.select(0, cx);
    }
    fn bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) {
        self.select(usize::MAX, cx);
    }

    fn hunk_starts(&self) -> Vec<usize> {
        let rows = self.rows();
        (0..rows.len())
            .filter(|&i| rows[i].kind != Kind::Equal && (i == 0 || rows[i - 1].kind == Kind::Equal))
            .collect()
    }

    fn next_hunk(&mut self, _: &NextHunk, _: &mut Window, cx: &mut Context<Self>) {
        let cur = self.selected.unwrap_or(0);
        let hunks = self.hunk_starts();
        if let Some(&h) = hunks.iter().find(|&&h| h > cur).or(hunks.first()) {
            self.select(h, cx);
        }
    }
    fn prev_hunk(&mut self, _: &PrevHunk, _: &mut Window, cx: &mut Context<Self>) {
        let cur = self.selected.unwrap_or(0);
        let hunks = self.hunk_starts();
        if let Some(&h) = hunks.iter().rev().find(|&&h| h < cur).or(hunks.last()) {
            self.select(h, cx);
        }
    }

    pub fn toggle_blame(&mut self, cx: &mut Context<Self>) {
        self.show_blame = !self.show_blame;
        if self.show_blame && self.left_blame.is_none() && self.right_blame.is_none() {
            self.load_blame(cx);
        }
        cx.notify();
    }
    fn on_toggle_blame(&mut self, _: &ToggleBlame, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_blame(cx);
    }
    fn toggle_inline(&mut self, _: &ToggleInline, _: &mut Window, cx: &mut Context<Self>) {
        // Keep roughly the same position when switching layouts.
        let line = self.selected.and_then(|s| self.rows().get(s)).and_then(|r| r.r.or(r.l));
        self.inline = !self.inline;
        if let Some(line) = line
            && let Some(ix) = self.rows().iter().position(|r| r.r == Some(line) || r.l == Some(line))
        {
            self.select(ix, cx);
        }
        cx.notify();
    }

    fn open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.selected.and_then(|s| self.rows().get(s)).and_then(|r| r.r.or(r.l));
        self.emit_open(line, cx);
    }

    fn emit_open(&self, line: Option<usize>, cx: &mut Context<Self>) {
        if self.spec.right == Rev::Empty {
            return cx.emit(Request::Error("File was deleted".into()));
        }
        cx.emit(Request::OpenFile { path: self.root.join(&self.spec.path), line, preview: false });
    }

    fn copy_row(&mut self, _: &CopyRow, _: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = &self.data else { return };
        let Some(row) = self.selected.and_then(|s| self.rows().get(s)) else { return };
        let text = row.r.map(|i| data.right[i].clone()).or_else(|| row.l.map(|i| data.left[i].clone()));
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let added = self.data.as_ref().map(|d| d.side.iter().filter(|r| r.r.is_some() && r.kind != Kind::Equal).count()).unwrap_or(0);
        let removed = self.data.as_ref().map(|d| d.side.iter().filter(|r| r.l.is_some() && r.kind != Kind::Equal).count()).unwrap_or(0);
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(32.))
            .px(px(12.))
            .gap(px(8.))
            .border_b_1()
            .border_color(theme::border())
            .child(ui::file_icon(&self.spec.path))
            .child(div().text_color(theme::fg_bright()).child(self.spec.path.clone()))
            .child(
                div()
                    .text_color(theme::fg_muted())
                    .child(format!("{} ↔ {}", self.spec.left.label(), self.spec.right.label())),
            )
            .child(div().text_color(theme::git_added()).child(format!("+{added}")))
            .child(div().text_color(theme::git_deleted()).child(format!("-{removed}")))
            .child(div().flex_1())
            .child(ui::icon_button("prev-hunk", "arrow-up").on_click(cx.listener(|v, _, w, cx| v.prev_hunk(&PrevHunk, w, cx))))
            .child(ui::icon_button("next-hunk", "arrow-down").on_click(cx.listener(|v, _, w, cx| v.next_hunk(&NextHunk, w, cx))))
            .when(!self.one_sided(), |d| {
                d.child(
                    ui::icon_button("toggle-inline", if self.inline { "split" } else { "inline" })
                        .on_click(cx.listener(|v, _, w, cx| v.toggle_inline(&ToggleInline, w, cx))),
                )
            })
            .child(
                ui::icon_button("toggle-blame", "blame")
                    .when(self.show_blame, |b| b.bg(theme::selected()))
                    .on_click(cx.listener(|v, _, _, cx| v.toggle_blame(cx))),
            )
            .child(ui::icon_button("open-file", "open").on_click(cx.listener(|v, _, w, cx| v.open_file(&OpenFile, w, cx))))
    }
}

fn blame_side(root: &std::path::Path, rev: &Rev, path: &str) -> Option<BlameLines> {
    let blame = match rev {
        Rev::Empty => return None,
        Rev::Commit(sha) => git::blame(root, path, Some(sha), None),
        Rev::Working => git::blame(root, path, None, None),
        Rev::Index => {
            let text = git::read_rev(root, &Rev::Index, path)?;
            git::blame(root, path, None, Some(&text))
        }
    };
    blame.ok().map(|b| b.lines)
}

fn compute(left: &str, right: &str) -> Loaded {
    let left_lines: Vec<String> = left.split('\n').map(String::from).collect();
    let right_lines: Vec<String> = right.split('\n').map(String::from).collect();
    let diff = TextDiff::configure().timeout(Duration::from_secs(2)).diff_lines(left, right);
    let mut side = Vec::new();
    let mut inline = Vec::new();
    let row = |kind, l, r| Row { kind, l, r, l_hl: Vec::new(), r_hl: Vec::new() };
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { old_index, new_index, len } => {
                for i in 0..len {
                    side.push(row(Kind::Equal, Some(old_index + i), Some(new_index + i)));
                    inline.push(row(Kind::Equal, Some(old_index + i), Some(new_index + i)));
                }
            }
            DiffOp::Delete { old_index, old_len, .. } => {
                for i in 0..old_len {
                    side.push(row(Kind::Delete, Some(old_index + i), None));
                    inline.push(row(Kind::Delete, Some(old_index + i), None));
                }
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                for i in 0..new_len {
                    side.push(row(Kind::Insert, None, Some(new_index + i)));
                    inline.push(row(Kind::Insert, None, Some(new_index + i)));
                }
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                let n = old_len.max(new_len);
                let mut dels = Vec::new();
                let mut ins = Vec::new();
                for i in 0..n {
                    let l = (i < old_len).then_some(old_index + i);
                    let r = (i < new_len).then_some(new_index + i);
                    let (l_hl, r_hl) = match (l, r) {
                        (Some(l), Some(r)) => word_highlights(&left_lines[l], &right_lines[r]),
                        _ => (Vec::new(), Vec::new()),
                    };
                    side.push(Row { kind: Kind::Change, l, r, l_hl: l_hl.clone(), r_hl: r_hl.clone() });
                    if let Some(l) = l {
                        dels.push(Row { kind: Kind::Delete, l: Some(l), r: None, l_hl, r_hl: Vec::new() });
                    }
                    if let Some(r) = r {
                        ins.push(Row { kind: Kind::Insert, l: None, r: Some(r), l_hl: Vec::new(), r_hl });
                    }
                }
                inline.extend(dels);
                inline.extend(ins);
            }
        }
    }
    Loaded { left: left_lines, right: right_lines, side, inline }
}

/// Byte ranges that differ between two versions of a line.
fn word_highlights(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old.len() + new.len() > 2000 {
        return (Vec::new(), Vec::new());
    }
    let diff = TextDiff::configure().diff_words(old, new);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    let (mut lo, mut ro) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                lo += len;
                ro += len;
            }
            ChangeTag::Delete => {
                push_range(&mut l, lo..lo + len);
                lo += len;
            }
            ChangeTag::Insert => {
                push_range(&mut r, ro..ro + len);
                ro += len;
            }
        }
    }
    (l, r)
}

fn push_range(v: &mut Vec<Range<usize>>, r: Range<usize>) {
    if let Some(last) = v.last_mut()
        && last.end == r.start
    {
        last.end = r.end;
        return;
    }
    v.push(r);
}

fn styled_line(text: &str, highlights: &[Range<usize>], hl_color: Hsla) -> StyledText {
    let display = text.replace('\t', "    ");
    if highlights.is_empty() || display.len() != text.len() {
        return StyledText::new(display);
    }
    let font = font(theme::MONO_FONT);
    let mut runs = Vec::new();
    let mut pos = 0;
    let run = |len: usize, bg: Option<Hsla>| TextRun { len, font: font.clone(), color: theme::fg(), background_color: bg, underline: None, strikethrough: None };
    for r in highlights {
        let (s, e) = (r.start.min(text.len()), r.end.min(text.len()));
        if s > pos {
            runs.push(run(s - pos, None));
        }
        if e > s {
            runs.push(run(e - s, Some(hl_color)));
        }
        pos = e.max(pos);
    }
    if pos < text.len() {
        runs.push(run(text.len() - pos, None));
    }
    StyledText::new(display).with_runs(runs)
}

const ROW_H: f32 = 20.;
const NUM_W: f32 = 44.;
const BLAME_W: f32 = 190.;

impl DiffView {
    fn render_side(&self, row: &Row, left: bool, data: &Loaded, row_ix: usize, cx: &mut Context<Self>) -> Div {
        let line = if left { row.l } else { row.r };
        let (bg, hl, sign) = match (row.kind, left, line.is_some()) {
            (Kind::Equal, _, _) => (None, theme::diff_del_char(), " "),
            (_, _, false) => (Some(theme::diff_filler()), theme::diff_del_char(), " "),
            (Kind::Delete, _, true) | (Kind::Change, true, true) => (Some(theme::diff_del_line()), theme::diff_del_char(), "-"),
            (Kind::Insert, _, true) | (Kind::Change, false, true) => (Some(theme::diff_ins_line()), theme::diff_ins_char(), "+"),
        };
        let text = line.map(|i| if left { data.left[i].as_str() } else { data.right[i].as_str() }).unwrap_or("");
        let highlights = if left { &row.l_hl } else { &row.r_hl };
        let blame = if left { &self.left_blame } else { &self.right_blame };

        let mut side = div().flex().flex_1().min_w_0().h_full().overflow_hidden();
        if let Some(bg) = bg {
            side = side.bg(bg);
        }
        if self.show_blame {
            let commit = line.and_then(|i| blame.as_ref().and_then(|b| b.get(i)).and_then(|c| c.clone()));
            let show = match (line, &commit) {
                (Some(i), Some(c)) => i == 0 || blame.as_ref().and_then(|b| b.get(i - 1)).and_then(|p| p.as_ref()).map(|p| p.sha != c.sha).unwrap_or(true),
                _ => false,
            };
            let cell = div()
                .id(("blame", row_ix * 2 + left as usize))
                .flex_none()
                .w(px(BLAME_W))
                .h_full()
                .px(px(6.))
                .overflow_hidden()
                .whitespace_nowrap()
                .bg(theme::bg_side())
                .text_color(theme::fg_dim())
                .text_size(px(11.))
                .when_some(commit.filter(|_| show), |cell, c| {
                    let sha = c.sha.clone();
                    let label = if c.is_uncommitted() {
                        "You · uncommitted".to_string()
                    } else {
                        let who = if self.user_email.as_deref() == Some(c.email.as_str()) { "You".to_string() } else { c.author.clone() };
                        format!("{} {} · {}", util::short_time(c.time), who, c.summary)
                    };
                    cell.cursor_pointer()
                        .hover(|s| s.text_color(theme::fg_bright()))
                        .border_t_1()
                        .border_color(theme::border())
                        .child(label)
                        .when(!c.is_uncommitted(), |cell| {
                            cell.on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::OpenCommit(sha.clone()))))
                        })
                });
            side = side.child(cell);
        }
        side.child(
            div()
                .flex_none()
                .w(px(NUM_W))
                .pr(px(8.))
                .flex()
                .justify_end()
                .text_color(theme::line_number())
                .child(line.map(|i| (i + 1).to_string()).unwrap_or_default()),
        )
        .child(div().flex_none().w(px(16.)).text_color(theme::fg_muted()).child(sign))
        .child(
            div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(
                div()
                    .ml(px(-self.scroll_x))
                    .child(styled_line(text, highlights, hl)),
            ),
        )
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = if let Some(err) = &self.error {
            div().p(px(20.)).text_color(theme::fg_muted()).child(err.clone()).into_any_element()
        } else if let Some(data) = self.data.clone() {
            let count = if self.show_inline() { data.inline.len() } else { data.side.len() };
            if count == 0 || (count == 1 && data.side.iter().all(|r| r.kind == Kind::Equal) && data.left == data.right) {
                div().p(px(20.)).text_color(theme::fg_muted()).child("No changes.").into_any_element()
            } else {
                uniform_list(
                    "diff-rows",
                    count,
                    cx.processor(move |view, range: Range<usize>, _window, cx| {
                        let rows = if view.show_inline() { &data.inline } else { &data.side };
                        range
                            .map(|ix| {
                                let row = rows[ix].clone();
                                let selected = view.selected == Some(ix);
                                let base = div()
                                    .id(("diff-row", ix))
                                    .flex()
                                    .h(px(ROW_H))
                                    .w_full()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(11.5))
                                    .when(selected, |d| d.border_1().border_color(theme::focus_border()))
                                    .on_click(cx.listener(move |view, ev: &ClickEvent, _, cx| {
                                        view.selected = Some(ix);
                                        if ev.click_count() >= 2 {
                                            let line = view.rows().get(ix).and_then(|r| r.r.or(r.l));
                                            view.emit_open(line, cx);
                                        }
                                        cx.notify();
                                    }));
                                if view.show_inline() {
                                    let left = row.r.is_none();
                                    base.child(view.render_side(&row, left, &data, ix, cx))
                                } else {
                                    let split = view.split;
                                    base.child(div().flex().flex_none().w(relative(split)).h_full().child(view.render_side(&row, true, &data, ix, cx)))
                                        .child(
                                            // Drag to resize old vs new.
                                            div()
                                                .id(("diff-divider", ix))
                                                .flex()
                                                .flex_none()
                                                .justify_center()
                                                .w(px(5.))
                                                .h_full()
                                                .cursor(CursorStyle::ResizeLeftRight)
                                                .hover(|s| s.bg(theme::frame_focused().opacity(0.35)))
                                                .on_mouse_down(MouseButton::Left, cx.listener(|view, _, _, cx| {
                                                    view.dragging_split = true;
                                                    cx.stop_propagation();
                                                }))
                                                .child(div().w(px(1.)).h_full().bg(theme::border())),
                                        )
                                        .child(view.render_side(&row, false, &data, ix, cx))
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1()
                .into_any_element()
            }
        } else {
            div().p(px(20.)).text_color(theme::fg_muted()).child("Loading diff…").into_any_element()
        };

        div()
            .key_context("DiffView")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .on_action(cx.listener(Self::next_row))
            .on_action(cx.listener(Self::prev_row))
            .on_action(cx.listener(Self::next_hunk))
            .on_action(cx.listener(Self::prev_hunk))
            .on_action(cx.listener(Self::on_toggle_blame))
            .on_action(cx.listener(Self::toggle_inline))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::copy_row))
            .on_action(cx.listener(Self::top))
            .on_action(cx.listener(Self::bottom))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::page_up))
            .on_mouse_down(MouseButton::Left, cx.listener(|view, _, window, _| window.focus(&view.focus)))
            .on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _, cx| {
                if !view.dragging_split {
                    return;
                }
                if !ev.dragging() {
                    view.dragging_split = false;
                    return;
                }
                let b = view.body_bounds.get();
                if b.size.width > px(0.) {
                    view.split = ((ev.position.x - b.left()) / b.size.width).clamp(0.12, 0.88);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|view, _, _, _| view.dragging_split = false))
            .on_scroll_wheel(cx.listener(|view, ev: &ScrollWheelEvent, _, cx| {
                let d = ev.delta.pixel_delta(px(ROW_H));
                let dx = d.x / px(1.);
                if dx.abs() > 0.5 {
                    view.scroll_x = (view.scroll_x - dx).max(0.);
                    cx.notify();
                }
            }))
            .child(self.render_header(cx))
            .child({
                let cell = self.body_bounds.clone();
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full())
                    .child(body)
            })
    }
}
