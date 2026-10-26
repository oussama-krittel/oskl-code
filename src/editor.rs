//! The text editor view. Used for files, the commit message box and the
//! command line, so every text field behaves the same way.

use crate::buffer::{Buffer, RowChange, Sel};
use crate::git::{self, BlameCommit};
use crate::highlight::{self, Lang, LineSpans};
use crate::{theme, tools, util};
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;
use std::path::PathBuf;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

actions!(
    editor,
    [
        MoveLeft, MoveRight, MoveUp, MoveDown,
        SelectLeft, SelectRight, SelectUp, SelectDown,
        MoveWordLeft, MoveWordRight, SelectWordLeft, SelectWordRight,
        MoveLineStart, MoveLineEnd, SelectLineStart, SelectLineEnd,
        MoveDocStart, MoveDocEnd, SelectDocStart, SelectDocEnd,
        PageUp, PageDown, SelectPageUp, SelectPageDown,
        Backspace, Delete, DeleteWordLeft, DeleteWordRight, DeleteToLineStart, KillLine,
        Newline, NewlineAbove, Indent, Outdent,
        SelectAll, SelectLine, Copy, Cut, Paste, Undo, Redo,
        MoveLineUp, MoveLineDown, DuplicateLineDown, DuplicateLineUp, DeleteLine, ToggleComment,
        Escape, Submit, ShowCharacterPalette, ScrollLineUp, ScrollLineDown,
    ]
);

pub fn bind_keys() {
    let c = Some("Editor");
    crate::keybindings::collect([
        crate::keybindings::kb("left", MoveLeft, c),
        crate::keybindings::kb("right", MoveRight, c),
        crate::keybindings::kb("up", MoveUp, c),
        crate::keybindings::kb("down", MoveDown, c),
        crate::keybindings::kb("ctrl-b", MoveLeft, c),
        crate::keybindings::kb("ctrl-f", MoveRight, c),
        crate::keybindings::kb("ctrl-p", MoveUp, c),
        crate::keybindings::kb("ctrl-n", MoveDown, c),
        crate::keybindings::kb("shift-left", SelectLeft, c),
        crate::keybindings::kb("shift-right", SelectRight, c),
        crate::keybindings::kb("shift-up", SelectUp, c),
        crate::keybindings::kb("shift-down", SelectDown, c),
        crate::keybindings::kb("alt-left", MoveWordLeft, c),
        crate::keybindings::kb("alt-right", MoveWordRight, c),
        crate::keybindings::kb("alt-b", MoveWordLeft, c),
        crate::keybindings::kb("alt-f", MoveWordRight, c),
        crate::keybindings::kb("alt-shift-left", SelectWordLeft, c),
        crate::keybindings::kb("alt-shift-right", SelectWordRight, c),
        crate::keybindings::kb("cmd-left", MoveLineStart, c),
        crate::keybindings::kb("cmd-right", MoveLineEnd, c),
        crate::keybindings::kb("home", MoveLineStart, c),
        crate::keybindings::kb("end", MoveLineEnd, c),
        crate::keybindings::kb("ctrl-a", MoveLineStart, c),
        crate::keybindings::kb("ctrl-e", MoveLineEnd, c),
        crate::keybindings::kb("cmd-shift-left", SelectLineStart, c),
        crate::keybindings::kb("cmd-shift-right", SelectLineEnd, c),
        crate::keybindings::kb("shift-home", SelectLineStart, c),
        crate::keybindings::kb("shift-end", SelectLineEnd, c),
        crate::keybindings::kb("cmd-up", MoveDocStart, c),
        crate::keybindings::kb("cmd-down", MoveDocEnd, c),
        crate::keybindings::kb("cmd-shift-up", SelectDocStart, c),
        crate::keybindings::kb("cmd-shift-down", SelectDocEnd, c),
        crate::keybindings::kb("pageup", PageUp, c),
        crate::keybindings::kb("pagedown", PageDown, c),
        crate::keybindings::kb("shift-pageup", SelectPageUp, c),
        crate::keybindings::kb("shift-pagedown", SelectPageDown, c),
        crate::keybindings::kb("backspace", Backspace, c),
        crate::keybindings::kb("shift-backspace", Backspace, c),
        crate::keybindings::kb("ctrl-h", Backspace, c),
        crate::keybindings::kb("delete", Delete, c),
        crate::keybindings::kb("ctrl-d", Delete, c),
        crate::keybindings::kb("alt-backspace", DeleteWordLeft, c),
        crate::keybindings::kb("ctrl-w", DeleteWordLeft, c),
        crate::keybindings::kb("alt-delete", DeleteWordRight, c),
        crate::keybindings::kb("cmd-backspace", DeleteToLineStart, c),
        crate::keybindings::kb("ctrl-u", DeleteToLineStart, c),
        crate::keybindings::kb("ctrl-k", KillLine, c),
        crate::keybindings::kb("enter", Newline, c),
        crate::keybindings::kb("shift-enter", Newline, c),
        crate::keybindings::kb("cmd-enter", Submit, c),
        crate::keybindings::kb("cmd-shift-enter", NewlineAbove, c),
        crate::keybindings::kb("tab", Indent, c),
        crate::keybindings::kb("shift-tab", Outdent, c),
        crate::keybindings::kb("cmd-]", Indent, c),
        crate::keybindings::kb("cmd-[", Outdent, c),
        crate::keybindings::kb("cmd-a", SelectAll, c),
        crate::keybindings::kb("cmd-l", SelectLine, c),
        crate::keybindings::kb("cmd-c", Copy, c),
        crate::keybindings::kb("cmd-x", Cut, c),
        crate::keybindings::kb("cmd-v", Paste, c),
        crate::keybindings::kb("cmd-z", Undo, c),
        crate::keybindings::kb("cmd-shift-z", Redo, c),
        crate::keybindings::kb("alt-up", MoveLineUp, c),
        crate::keybindings::kb("alt-down", MoveLineDown, c),
        crate::keybindings::kb("alt-shift-down", DuplicateLineDown, c),
        crate::keybindings::kb("alt-shift-up", DuplicateLineUp, c),
        crate::keybindings::kb("cmd-shift-k", DeleteLine, c),
        crate::keybindings::kb("cmd-/", ToggleComment, c),
        crate::keybindings::kb("escape", Escape, c),
        crate::keybindings::kb("ctrl-alt-up", ScrollLineUp, c),
        crate::keybindings::kb("ctrl-alt-down", ScrollLineDown, c),
        crate::keybindings::kb("ctrl-cmd-up", ScrollLineUp, c),
        crate::keybindings::kb("ctrl-cmd-down", ScrollLineDown, c),
        crate::keybindings::kb("ctrl-cmd-{up}", ScrollLineUp, c),
        crate::keybindings::kb("ctrl-cmd-{down}", ScrollLineDown, c),
        crate::keybindings::kb("ctrl-cmd-space", ShowCharacterPalette, c),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A problem reported by a linter or the type checker (0-based rows, char columns).
#[derive(Clone, Debug)]
pub struct Diag {
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub severity: Severity,
    pub message: String,
    pub source: String,
}

impl Severity {
    pub fn color(self) -> Hsla {
        match self {
            Severity::Error => theme::c(0xf14c4c),
            Severity::Warning => theme::c(0xe5c07b),
            Severity::Info => theme::c(0x61afef),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Full,
    SingleLine,
    MultiLine,
}

pub enum EditorEvent {
    Changed,
    Submit,
    Cancel,
    Up,
    Down,
    Tab,
    OpenCommit(String),
    Saved,
}

impl EventEmitter<EditorEvent> for Editor {}

/// How a buffer line is tinted in a diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineTint {
    None,
    Added,
    Removed,
}

/// One row on screen when the editor shows a diff.
#[derive(Clone, Copy, Debug)]
pub enum DRow {
    Line(usize),
    /// Empty row that keeps the two sides of a side-by-side diff aligned.
    Filler,
    /// A deleted line shown read-only (inline diff), index into `phantom`.
    Phantom(usize),
}

#[derive(Debug, Default)]
pub struct DiffDecor {
    pub rows: Vec<DRow>,
    pub disp_of_line: Vec<usize>,
    pub tint: Vec<LineTint>,
    pub phantom: Vec<String>,
    /// Buffer rows where each change starts.
    pub hunks: Vec<usize>,
    /// Changed words per buffer row (char columns), for VS Code-style inline highlights.
    pub words: std::collections::HashMap<usize, Vec<Range<usize>>>,
    pub phantom_words: Vec<Vec<Range<usize>>>,
}

/// Decorations for both sides plus the inline layout, all from one diff.
pub struct DiffDecors {
    pub base: Arc<String>,
    pub left: Arc<DiffDecor>,
    pub right: Arc<DiffDecor>,
    pub inline: Arc<DiffDecor>,
    pub added: usize,
    pub removed: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffRole {
    None,
    /// The editable (working tree) side of a diff; computes the diff itself.
    Right { inline: bool },
}

#[derive(Clone)]
enum DragMode {
    Char,
    Word(Range<usize>),
    Line(usize),
}

struct ScrollbarGeom {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    max_scroll: f32,
}

struct Layout {
    bounds: Bounds<Pixels>,
    text_left: f32,
    gutter_left: f32,
    blame_w: f32,
    line_h: f32,
    char_w: f32,
    pad_top: f32,
    lines: Vec<(usize, ShapedLine, Vec<usize>)>,
    scrollbar: Option<ScrollbarGeom>,
}

pub struct Editor {
    focus: FocusHandle,
    pub buf: Buffer,
    pub mode: EditorMode,
    pub path: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub placeholder: SharedString,
    pub read_only: bool,
    scroll_x: f32,
    scroll_y: f32,
    autoscroll: Option<bool>,
    layout: Option<Layout>,
    drag: Option<DragMode>,
    scroll_drag: Option<f32>,
    last_mouse: Point<Pixels>,
    autoscroll_task: Option<Task<()>>,
    marked: Option<Range<usize>>,
    blame: Option<Vec<Option<Arc<BlameCommit>>>>,
    pub blame_gutter: bool,
    blame_task: Option<Task<()>>,
    base_text: Option<Arc<String>>,
    marks: Vec<u8>,
    marks_task: Option<Task<()>>,
    pub search: Option<String>,
    pub user_email: Option<String>,
    pub font_size: f32,
    char_w: Option<(f32, f32)>,
    line_copy: Option<String>,
    center_on_next_layout: bool,
    layout_scrollbar: Option<ScrollbarGeom>,
    pub diff_role: DiffRole,
    lang: Option<Lang>,
    spans: Option<Arc<LineSpans>>,
    hl_task: Option<Task<()>>,
    pub diagnostics: Vec<Diag>,
    pub decor: Option<Arc<DiffDecor>>,
    pub decors: Option<Arc<DiffDecors>>,
    pub scroll_link: Option<Rc<Cell<(f32, u64)>>>,
    link_gen: u64,
}

const MARK_ADDED: u8 = 1;
const MARK_MODIFIED: u8 = 2;
const MARK_DELETED: u8 = 3;

impl Editor {
    pub fn new(mode: EditorMode, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            buf: Buffer::new(""),
            mode,
            path: None,
            root: None,
            placeholder: SharedString::default(),
            read_only: false,
            scroll_x: 0.,
            scroll_y: 0.,
            autoscroll: None,
            layout: None,
            drag: None,
            scroll_drag: None,
            last_mouse: Point::default(),
            autoscroll_task: None,
            marked: None,
            blame: None,
            blame_gutter: false,
            blame_task: None,
            base_text: None,
            marks: Vec::new(),
            marks_task: None,
            search: None,
            user_email: None,
            font_size: 12.,
            char_w: None,
            line_copy: None,
            center_on_next_layout: false,
            layout_scrollbar: None,
            diff_role: DiffRole::None,
            lang: None,
            spans: None,
            hl_task: None,
            diagnostics: Vec::new(),
            decor: None,
            decors: None,
            scroll_link: None,
            link_gen: 0,
        }
    }

    pub fn open(path: PathBuf, root: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut ed = Self::new(EditorMode::Full, cx);
        ed.load(path, root, cx);
        ed
    }

    pub fn load(&mut self, path: PathBuf, root: Option<PathBuf>, cx: &mut Context<Self>) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        self.buf = Buffer::new(&String::from_utf8_lossy(&bytes));
        self.lang = highlight::detect(&path);
        self.path = Some(path);
        self.root = root;
        self.refresh_git(cx);
        self.schedule_highlight(Duration::ZERO, cx);
    }

    /// Re-highlights the whole buffer in the background (tree-sitter is fast enough).
    fn schedule_highlight(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if self.lang.is_none() {
            self.lang = self.path.as_deref().and_then(highlight::detect);
        }
        let Some(lang) = self.lang else { return };
        let text = self.buf.text();
        let version = self.buf.version();
        self.hl_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let spans = cx.background_executor().spawn(async move { highlight::highlight(lang, &text) }).await;
            this.update(cx, |ed, cx| {
                if ed.buf.version() == version {
                    ed.spans = spans.map(Arc::new);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub fn set_diagnostics(&mut self, diags: Vec<Diag>, cx: &mut Context<Self>) {
        self.diagnostics = diags;
        cx.notify();
    }

    /// How far the view is scrolled down, in pixels.
    pub fn scroll_top(&self) -> f32 {
        self.scroll_y
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn text(&self) -> String {
        self.buf.text()
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buf = Buffer::new(text);
        self.buf.sel = Sel::at(self.buf.len());
        self.scroll_x = 0.;
        self.scroll_y = 0.;
        self.autoscroll = Some(false);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn rel_path(&self) -> Option<String> {
        let (path, root) = (self.path.as_ref()?, self.root.as_ref()?);
        path.strip_prefix(root).ok().map(|p| p.to_string_lossy().to_string())
    }

    pub fn cursor_rc(&self) -> (usize, usize) {
        self.buf.pos_to_rc(self.buf.sel.head)
    }

    pub fn line_height(&self) -> f32 {
        (self.font_size * 1.54).round()
    }

    pub fn go_to(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let pos = self.buf.rc_to_pos(row, col);
        self.buf.sel = Sel::at(pos);
        self.autoscroll = Some(false);
        self.center_on_next_layout = true;
        cx.notify();
    }

    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.buf.sel = Sel { anchor: range.start, head: range.end };
        self.autoscroll = Some(false);
        self.center_on_next_layout = true;
        cx.notify();
    }

    pub fn save(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let Some(path) = self.path.clone() else { anyhow::bail!("no path") };
        std::fs::write(&path, self.buf.text_for_disk())?;
        self.buf.mark_saved();
        self.refresh_git(cx);
        cx.emit(EditorEvent::Saved);
        cx.notify();
        Ok(())
    }

    /// Reloads from disk when the file changed outside and there are no unsaved edits.
    pub fn reload_if_clean(&mut self, cx: &mut Context<Self>) {
        if self.buf.is_dirty() {
            return;
        }
        let Some(path) = &self.path else { return };
        let Ok(bytes) = std::fs::read(path) else { return };
        let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
        if text != self.buf.text() {
            self.buf.set_text(&text, true);
            self.buf.mark_saved();
            self.after_edit(cx);
            self.refresh_git(cx);
        }
    }

    pub fn apply_format(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buf.set_text(text, true);
        self.after_edit(cx);
    }

    // ------------------------------------------------------------ git integration

    pub fn refresh_git(&mut self, cx: &mut Context<Self>) {
        self.refresh_base(cx);
        self.refresh_blame(cx);
    }

    pub fn refresh_base(&mut self, cx: &mut Context<Self>) {
        let (Some(root), Some(rel)) = (self.root.clone(), self.rel_path()) else { return };
        let task = cx.background_executor().spawn(async move { git::read_rev(&root, &git::Rev::Index, &rel) });
        self.marks_task = Some(cx.spawn(async move |this, cx| {
            let base = task.await;
            this.update(cx, |ed, cx| {
                ed.base_text = base.map(Arc::new);
                ed.schedule_marks(Duration::ZERO, cx);
            })
            .ok();
        }));
    }

    pub fn refresh_blame(&mut self, cx: &mut Context<Self>) {
        if self.mode != EditorMode::Full {
            return;
        }
        let (Some(root), Some(rel)) = (self.root.clone(), self.rel_path()) else { return };
        let text = self.buf.text();
        let version = self.buf.version();
        let task = cx
            .background_executor()
            .spawn(async move { git::blame(&root, &rel, None, Some(&text)).ok() });
        self.blame_task = Some(cx.spawn(async move |this, cx| {
            let blame = task.await;
            this.update(cx, |ed, cx| {
                if ed.buf.version() == version {
                    ed.blame = blame.map(|b| b.lines);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn schedule_marks(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if let DiffRole::Right { inline } = self.diff_role {
            let base = self.base_text.clone().unwrap_or_default();
            let decors = Arc::new(compute_decors(base, &self.buf.text()));
            self.decor = Some(if inline { decors.inline.clone() } else { decors.right.clone() });
            self.decors = Some(decors);
            cx.notify();
            return;
        }
        let Some(base) = self.base_text.clone() else {
            self.marks.clear();
            return;
        };
        let current = self.buf.text();
        self.marks_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let marks = cx
                .background_executor()
                .spawn(async move { compute_marks(&base, &current) })
                .await;
            this.update(cx, |ed, cx| {
                ed.marks = marks;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_blame_lines(&mut self, lines: Option<Vec<Option<Arc<BlameCommit>>>>, cx: &mut Context<Self>) {
        self.blame = lines;
        cx.notify();
    }

    /// Replaces the text of a read-only view without touching scroll position.
    pub fn set_view_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.buf.text() != text {
            self.buf = Buffer::new(text);
            self.schedule_highlight(Duration::ZERO, cx);
            cx.notify();
        }
    }

    pub fn set_inline_diff(&mut self, inline: bool, cx: &mut Context<Self>) {
        if let DiffRole::Right { .. } = self.diff_role {
            self.diff_role = DiffRole::Right { inline };
            if let Some(d) = &self.decors {
                self.decor = Some(if inline { d.inline.clone() } else { d.right.clone() });
            }
            cx.notify();
        }
    }

    fn disp_count(&self) -> usize {
        match &self.decor {
            Some(d) => d.rows.len().max(1),
            None => self.buf.line_count(),
        }
    }

    fn disp_of(&self, row: usize) -> usize {
        match &self.decor {
            Some(d) => disp_of(d, row),
            None => row,
        }
    }

    fn line_at_disp(&self, disp: usize) -> usize {
        let last = self.buf.line_count().saturating_sub(1);
        let Some(d) = &self.decor else { return disp.min(last) };
        let disp = disp.min(d.rows.len().saturating_sub(1));
        let found = d.rows[disp..].iter().find_map(|r| if let DRow::Line(l) = r { Some(*l) } else { None })
            .or_else(|| d.rows[..disp].iter().rev().find_map(|r| if let DRow::Line(l) = r { Some(*l) } else { None }));
        found.unwrap_or(0).min(last)
    }

    pub fn blame_at_cursor(&self) -> Option<Arc<BlameCommit>> {
        let (row, _) = self.cursor_rc();
        self.blame.as_ref()?.get(row)?.clone()
    }

    /// Keeps highlight spans and diagnostics lined up with rows until they're recomputed.
    fn splice_decorations(&mut self, changes: &[RowChange]) {
        if let Some(spans) = self.spans.as_mut() {
            let spans = Arc::make_mut(spans);
            for ch in changes {
                let start = ch.start.min(spans.len());
                let end = (ch.old_end + 1).min(spans.len()).max(start);
                // Keep the old colors of the first edited line; it usually still mostly matches.
                let keep = spans.get(ch.start).cloned().unwrap_or_default();
                let mut new: Vec<Vec<(Range<usize>, u8)>> = vec![Vec::new(); ch.new_end - ch.start + 1];
                new[0] = keep;
                spans.splice(start..end, new);
            }
        }
        for ch in changes {
            let delta = ch.new_end as isize - ch.old_end as isize;
            if delta == 0 {
                continue;
            }
            for d in &mut self.diagnostics {
                if d.row > ch.old_end {
                    d.row = (d.row as isize + delta).max(0) as usize;
                    d.end_row = (d.end_row as isize + delta).max(0) as usize;
                }
            }
        }
    }

    fn splice_blame(&mut self, changes: &[RowChange]) {
        let Some(lines) = self.blame.as_mut() else { return };
        for ch in changes {
            let start = ch.start.min(lines.len());
            let end = (ch.old_end + 1).min(lines.len()).max(start);
            lines.splice(start..end, std::iter::repeat_n(None, ch.new_end - ch.start + 1));
        }
    }

    // ------------------------------------------------------------ edit plumbing

    fn after_move(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.autoscroll = Some(false);
        cx.notify();
    }

    fn after_edit(&mut self, cx: &mut Context<Self>) {
        let changes = self.buf.take_changes();
        self.splice_blame(&changes);
        self.splice_decorations(&changes);
        self.schedule_highlight(Duration::from_millis(30), cx);
        let delay = if self.diff_role == DiffRole::None { Duration::from_millis(120) } else { Duration::ZERO };
        self.schedule_marks(delay, cx);
        self.autoscroll = Some(false);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn motion(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Buffer)) {
        f(&mut self.buf);
        self.after_move(cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Buffer)) {
        if self.read_only {
            return;
        }
        f(&mut self.buf);
        self.after_edit(cx);
    }

    fn page_rows(&self) -> isize {
        self.layout
            .as_ref()
            .map(|l| ((l.bounds.size.height / px(1.)) / l.line_h) as isize - 2)
            .unwrap_or(20)
            .max(1)
    }

    // ------------------------------------------------------------ actions

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_horizontal(false, false)) }
    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_horizontal(true, false)) }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_horizontal(false, true)) }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_horizontal(true, true)) }
    fn move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_word(false, false)) }
    fn move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_word(true, false)) }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_word(false, true)) }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_word(true, true)) }
    fn move_line_start(&mut self, _: &MoveLineStart, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_line_start(false)) }
    fn move_line_end(&mut self, _: &MoveLineEnd, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_line_end(false)) }
    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_line_start(true)) }
    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_line_end(true)) }
    fn move_doc_start(&mut self, _: &MoveDocStart, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_to(0, false)) }
    fn move_doc_end(&mut self, _: &MoveDocEnd, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_to(b.len(), false)) }
    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_to(0, true)) }
    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_to(b.len(), true)) }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) { self.buf.select_all(); cx.notify() }
    fn select_line(&mut self, _: &SelectLine, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.select_line()) }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Up);
        }
        self.motion(cx, |b| b.move_vertical(-1, false))
    }
    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Down);
        }
        self.motion(cx, |b| b.move_vertical(1, false))
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_vertical(-1, true)) }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.move_vertical(1, true)) }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.page_rows();
        self.scroll_y -= n as f32 * self.line_height();
        self.motion(cx, |b| b.move_vertical(-n, false))
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.page_rows();
        self.scroll_y += n as f32 * self.line_height();
        self.motion(cx, |b| b.move_vertical(n, false))
    }
    /// ⌃⌥↑ / ⌃⌥↓: scroll the view a line, leaving the cursor where it is.
    fn scroll_line_up(&mut self, _: &ScrollLineUp, _: &mut Window, cx: &mut Context<Self>) {
        // In one-line inputs (finder, command line, search) this moves through the options.
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Up);
        }
        self.scroll_y = (self.scroll_y - self.line_height()).max(0.);
        cx.notify();
    }
    fn scroll_line_down(&mut self, _: &ScrollLineDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Down);
        }
        self.scroll_y += self.line_height();
        cx.notify();
    }

    fn select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) { let n = self.page_rows(); self.motion(cx, |b| b.move_vertical(-n, true)) }
    fn select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) { let n = self.page_rows(); self.motion(cx, |b| b.move_vertical(n, true)) }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.backspace()) }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.delete_forward()) }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.delete_word(false)) }
    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.delete_word(true)) }
    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.delete_to_line_start()) }
    fn kill_line(&mut self, _: &KillLine, _: &mut Window, cx: &mut Context<Self>) {
        let (row, _) = self.cursor_rc();
        let line_end = self.buf.line_start(row) + self.buf.line_len(row);
        let killed = self.buf.slice(self.buf.sel.head..line_end);
        if !killed.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(killed));
        }
        self.edit(cx, |b| b.kill_line())
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            EditorMode::SingleLine => cx.emit(EditorEvent::Submit),
            _ => self.edit(cx, |b| b.newline()),
        }
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            EditorMode::Full => self.edit(cx, |b| b.newline_below()),
            _ => cx.emit(EditorEvent::Submit),
        }
    }
    fn newline_above(&mut self, _: &NewlineAbove, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode == EditorMode::Full {
            self.edit(cx, |b| b.newline_above())
        }
    }
    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            EditorMode::SingleLine => cx.emit(EditorEvent::Tab),
            _ => self.edit(cx, |b| b.indent_lines(false)),
        }
    }
    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode != EditorMode::SingleLine {
            self.edit(cx, |b| b.indent_lines(true))
        }
    }
    fn move_line_up(&mut self, _: &MoveLineUp, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.move_lines(false)) }
    fn move_line_down(&mut self, _: &MoveLineDown, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.move_lines(true)) }
    fn duplicate_line_down(&mut self, _: &DuplicateLineDown, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.duplicate_lines(true)) }
    fn duplicate_line_up(&mut self, _: &DuplicateLineUp, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.duplicate_lines(false)) }
    fn delete_line(&mut self, _: &DeleteLine, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| b.delete_lines()) }
    fn toggle_comment(&mut self, _: &ToggleComment, _: &mut Window, cx: &mut Context<Self>) {
        match tools::comment_style(self.path.as_deref()) {
            tools::CommentStyle::Line(prefix) => self.edit(cx, |b| b.toggle_comment(prefix)),
            tools::CommentStyle::Block(open, close) => self.edit(cx, |b| b.toggle_block_comment(open, close)),
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| { b.undo(); }) }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) { self.edit(cx, |b| { b.redo(); }) }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = if self.buf.sel.is_empty() {
            let (row, _) = self.cursor_rc();
            let line = format!("{}\n", self.buf.line(row));
            self.line_copy = Some(line.clone());
            line
        } else {
            self.line_copy = None;
            self.buf.slice(self.buf.sel.range())
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        if self.buf.sel.is_empty() {
            self.edit(cx, |b| b.delete_lines())
        } else {
            self.edit(cx, |b| b.insert(""))
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(mut text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return };
        if self.mode == EditorMode::SingleLine {
            text = text.replace(['\n', '\r'], " ");
        }
        let text = text.replace("\r\n", "\n");
        if self.buf.sel.is_empty() && self.line_copy.as_deref() == Some(text.as_str()) {
            // A whole line copied with an empty selection is pasted above the current line.
            let (row, col) = self.cursor_rc();
            let start = self.buf.line_start(row);
            let after = Sel::at(self.buf.rc_to_pos(row, col) + text.chars().count());
            return self.edit(cx, |b| b.replace(start..start, &text, Some(after), crate::buffer::EditKind::Other));
        }
        self.edit(cx, |b| b.insert(&text))
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode == EditorMode::Full && (!self.buf.sel.is_empty() || self.search.is_some()) {
            self.buf.sel = Sel::at(self.buf.sel.head);
            self.search = None;
            cx.notify();
        } else {
            cx.emit(EditorEvent::Cancel);
        }
    }

    fn show_character_palette(&mut self, _: &ShowCharacterPalette, window: &mut Window, _: &mut Context<Self>) {
        window.show_character_palette();
    }

    // ------------------------------------------------------------ mouse

    fn row_for_y(&self, y: Pixels) -> usize {
        let Some(l) = &self.layout else { return 0 };
        let rel = (y - l.bounds.top()) / px(1.) - l.pad_top + self.scroll_y;
        let disp = if rel < 0. {
            // Above the top edge: one row above the first visible row.
            ((self.scroll_y / l.line_h) as usize).saturating_sub(1)
        } else {
            (rel / l.line_h) as usize
        };
        self.line_at_disp(disp)
    }

    fn pos_for_point(&self, p: Point<Pixels>) -> usize {
        let Some(l) = &self.layout else { return self.buf.sel.head };
        let row = self.row_for_y(p.y);
        let x = p.x / px(1.) - l.text_left + self.scroll_x;
        if x <= 0. {
            return self.buf.line_start(row);
        }
        let col = match l.lines.iter().find(|(r, _, _)| *r == row) {
            Some((_, shaped, map)) => {
                let byte = shaped.closest_index_for_x(px(x));
                map.iter().position(|b| *b >= byte).unwrap_or(map.len() - 1)
            }
            None => self.buf.col_for_display(row, (x / l.char_w).round() as usize),
        };
        self.buf.rc_to_pos(row, col)
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let Some(l) = &self.layout else { return };
        let x = ev.position.x / px(1.);
        if let Some(sb) = &l.scrollbar
            && sb.track.contains(&ev.position)
        {
            let thumb_h = sb.thumb.size.height / px(1.);
            let grab = if sb.thumb.contains(&ev.position) {
                (ev.position.y - sb.thumb.top()) / px(1.)
            } else {
                thumb_h / 2.
            };
            self.scroll_drag = Some(grab);
            self.drag_scrollbar(ev.position.y);
            return;
        }
        if self.blame_gutter && x < l.gutter_left + l.blame_w && x >= l.gutter_left {
            let row = self.row_for_y(ev.position.y);
            if let Some(Some(commit)) = self.blame.as_ref().and_then(|b| b.get(row))
                && !commit.is_uncommitted()
            {
                cx.emit(EditorEvent::OpenCommit(commit.sha.clone()));
            }
            return;
        }
        let in_numbers = self.mode == EditorMode::Full && x < l.text_left;
        let pos = self.pos_for_point(ev.position);
        self.buf.goal_col = None;
        if in_numbers {
            let row = self.row_for_y(ev.position.y);
            let range = self.buf.rows_range(row, row);
            self.buf.sel = Sel { anchor: range.start, head: range.end };
            self.drag = Some(DragMode::Line(row));
        } else {
            match ev.click_count {
                0 | 1 => {
                    self.buf.move_to(pos, ev.modifiers.shift);
                    self.drag = Some(DragMode::Char);
                }
                2 => {
                    let r = self.buf.word_range_at(pos);
                    self.buf.sel = Sel { anchor: r.start, head: r.end };
                    self.drag = Some(DragMode::Word(r));
                }
                _ => {
                    let (row, _) = self.buf.pos_to_rc(pos);
                    let range = self.buf.rows_range(row, row);
                    self.buf.sel = Sel { anchor: range.start, head: range.end };
                    self.drag = Some(DragMode::Line(row));
                }
            }
        }
        self.marked = None;
        cx.notify();
    }

    fn drag_scrollbar(&mut self, y: Pixels) {
        let (Some(grab), Some(l)) = (self.scroll_drag, &self.layout) else { return };
        let Some(sb) = &l.scrollbar else { return };
        let track_h = sb.track.size.height / px(1.);
        let thumb_h = sb.thumb.size.height / px(1.);
        let top = (y - sb.track.top()) / px(1.) - grab;
        let frac = (top / (track_h - thumb_h).max(1.)).clamp(0., 1.);
        self.scroll_y = frac * sb.max_scroll;
    }

    fn extend_drag(&mut self, p: Point<Pixels>) {
        let Some(mode) = self.drag.clone() else { return };
        let pos = self.pos_for_point(p);
        match mode {
            DragMode::Char => self.buf.move_to(pos, true),
            DragMode::Word(r) => {
                let w = self.buf.word_range_at(pos);
                self.buf.sel = if pos < r.start {
                    Sel { anchor: r.end, head: w.start }
                } else {
                    Sel { anchor: r.start, head: w.end.max(r.end) }
                };
            }
            DragMode::Line(row0) => {
                let (row, _) = self.buf.pos_to_rc(pos);
                self.buf.sel = if row >= row0 {
                    Sel { anchor: self.buf.line_start(row0), head: self.buf.rows_range(row, row).end }
                } else {
                    Sel { anchor: self.buf.rows_range(row0, row0).end, head: self.buf.line_start(row) }
                };
            }
        }
        self.autoscroll = Some(true);
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.last_mouse = ev.position;
        if !ev.dragging() {
            self.drag = None;
            self.scroll_drag = None;
            return;
        }
        if self.scroll_drag.is_some() {
            self.drag_scrollbar(ev.position.y);
            cx.notify();
            return;
        }
        if self.drag.is_none() {
            return;
        }
        self.extend_drag(ev.position);
        cx.notify();
        let outside = self
            .layout
            .as_ref()
            .is_some_and(|l| ev.position.y < l.bounds.top() || ev.position.y > l.bounds.bottom());
        if outside && self.autoscroll_task.is_none() {
            self.autoscroll_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                    let keep = this
                        .update(cx, |ed, cx| {
                            let Some(l) = &ed.layout else { return false };
                            let p = ed.last_mouse;
                            if ed.drag.is_none() || (p.y >= l.bounds.top() && p.y <= l.bounds.bottom()) {
                                return false;
                            }
                            ed.extend_drag(p);
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
                this.update(cx, |ed, _| ed.autoscroll_task = None).ok();
            }));
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag = None;
        self.scroll_drag = None;
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lh = self.line_height();
        let d = ev.delta.pixel_delta(px(lh));
        if self.mode != EditorMode::Full && self.buf.line_count() as f32 * lh <= self.layout.as_ref().map(|l| l.bounds.size.height / px(1.)).unwrap_or(0.) {
            return;
        }
        self.scroll_y -= d.y / px(1.);
        self.scroll_x -= d.x / px(1.);
        if let Some(link) = &self.scroll_link {
            let (_, g) = link.get();
            self.link_gen = g + 1;
            link.set((self.scroll_y, self.link_gen));
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ------------------------------------------------------------ layout

    fn mono(&self) -> Font {
        font(theme::MONO_FONT)
    }

    fn shape_highlighted(&self, window: &mut Window, row: usize, display: String, map: &[usize]) -> ShapedLine {
        let spans = self.spans.as_ref().and_then(|s| s.get(row)).filter(|s| !s.is_empty());
        let Some(spans) = spans.filter(|_| !display.is_empty()) else {
            return self.shape(window, display, theme::fg());
        };
        let mono = self.mono();
        let mut italic = mono.clone();
        italic.style = FontStyle::Italic;
        let run = |len: usize, color: Hsla, f: &Font| TextRun { len, font: f.clone(), color, background_color: None, underline: None, strikethrough: None };
        let last = map.len() - 1;
        let mut runs = Vec::new();
        let mut pos = 0;
        for (cols, h) in spans {
            let b0 = map[cols.start.min(last)];
            let b1 = map[cols.end.min(last)];
            if b0 < pos || b1 <= b0 {
                continue;
            }
            if b0 > pos {
                runs.push(run(b0 - pos, theme::fg(), &mono));
            }
            let f = if highlight::is_comment(*h) { &italic } else { &mono };
            runs.push(run(b1 - b0, highlight::color(*h), f));
            pos = b1;
        }
        if pos < display.len() {
            runs.push(run(display.len() - pos, theme::fg(), &mono));
        }
        window.text_system().shape_line(display.into(), px(self.font_size), &runs, None)
    }

    fn shape(&self, window: &mut Window, text: String, color: Hsla) -> ShapedLine {
        let runs = if text.is_empty() {
            vec![]
        } else {
            vec![TextRun { len: text.len(), font: self.mono(), color, background_color: None, underline: None, strikethrough: None }]
        };
        window.text_system().shape_line(text.into(), px(self.font_size), &runs, None)
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window) -> EditorPaint {
        let font_size = self.font_size;
        let line_h = self.line_height();
        let char_w = match self.char_w {
            Some((size, w)) if size == font_size => w,
            _ => {
                let w = self.shape(window, "0".repeat(20), theme::fg()).width / px(1.) / 20.;
                self.char_w = Some((font_size, w));
                w
            }
        };
        let full = self.mode == EditorMode::Full;
        let focused = self.focus.is_focused(window);
        let left = bounds.left() / px(1.);
        let top = bounds.top() / px(1.);
        let width = bounds.size.width / px(1.);
        let height = bounds.size.height / px(1.);
        let line_count = self.buf.line_count();

        // Gutter geometry.
        let blame_w = if full && self.blame_gutter { (char_w * 26.).round() } else { 0. };
        let digits = line_count.to_string().len().max(3) as f32;
        let numbers_w = if full { (digits * char_w + 26.).round() } else { 0. };
        let text_pad = if full { 14. } else { 2. };
        let text_left = left + blame_w + numbers_w + text_pad;
        let text_w = (width - blame_w - numbers_w - text_pad - 10.).max(10.);
        let pad_top = if self.mode == EditorMode::SingleLine { ((height - line_h) / 2.).max(0.) } else { 0. };

        // Scrolling.
        let disp_count = self.disp_count();
        let content_h = if full { (disp_count.saturating_sub(1)) as f32 * line_h + height } else { line_count as f32 * line_h };
        let max_scroll = (content_h - height).max(0.);
        let (row, col) = self.buf.pos_to_rc(self.buf.sel.head);
        let cursor_disp = self.disp_of(row);
        if let Some(link) = &self.scroll_link {
            let (v, g) = link.get();
            if g != self.link_gen {
                self.scroll_y = v;
                self.link_gen = g;
            }
        }
        if std::mem::take(&mut self.center_on_next_layout) {
            self.scroll_y = cursor_disp as f32 * line_h - height / 3.;
            self.autoscroll = None;
        }
        if let Some(tight) = self.autoscroll.take() {
            let y = cursor_disp as f32 * line_h;
            let margin = if full && !tight { (line_h * 3.).min(height / 3.) } else { 0. };
            if y < self.scroll_y + margin {
                self.scroll_y = y - margin;
            }
            if y + line_h > self.scroll_y + height - margin {
                self.scroll_y = y + line_h - height + margin;
            }
            let x = self.buf.display_col(row, col) as f32 * char_w;
            if x < self.scroll_x {
                self.scroll_x = (x - char_w * 8.).max(0.);
            }
            if x > self.scroll_x + text_w - char_w * 2. {
                self.scroll_x = x - text_w + char_w * 8.;
            }
        }
        self.scroll_y = self.scroll_y.clamp(0., max_scroll);
        if let Some(link) = &self.scroll_link {
            let (v, g) = link.get();
            if (v - self.scroll_y).abs() > 0.01 {
                self.link_gen = g + 1;
                link.set((self.scroll_y, self.link_gen));
                window.request_animation_frame();
            }
        }

        let decor = self.decor.clone();
        let first = (self.scroll_y / line_h) as usize;
        let visible = ((height / line_h).ceil() as usize + 1).min(disp_count.saturating_sub(first));
        let scroll_y = self.scroll_y;
        let y_disp = move |d: usize| top + pad_top + d as f32 * line_h - scroll_y;
        let y_for = |row: usize| y_disp(match &decor { Some(d) => disp_of(d, row), None => row });

        let mut lines = Vec::with_capacity(visible);
        let mut max_line_w: f32 = 0.;
        let mut fillers: Vec<f32> = Vec::new();
        let mut phantoms: Vec<(f32, usize, ShapedLine)> = Vec::new();
        let mut tints: Vec<(f32, LineTint)> = Vec::new();
        for d in first..first + visible {
            let row = match decor.as_ref().map(|dec| dec.rows[d]) {
                None => d,
                Some(DRow::Line(r)) if r < line_count => {
                    let tint = decor.as_ref().and_then(|dec| dec.tint.get(r).copied()).unwrap_or(LineTint::None);
                    if tint != LineTint::None {
                        tints.push((y_disp(d), tint));
                    }
                    r
                }
                Some(DRow::Phantom(i)) => {
                    let text = decor.as_ref().and_then(|dec| dec.phantom.get(i).cloned()).unwrap_or_default().replace('\t', "    ");
                    phantoms.push((y_disp(d), i, self.shape(window, text, theme::fg_dim())));
                    continue;
                }
                Some(_) => {
                    fillers.push(y_disp(d));
                    continue;
                }
            };
            let (mut display, mut map) = self.buf.display_line(row);
            if display.len() > 6000 {
                let mut cut = 6000;
                while !display.is_char_boundary(cut) {
                    cut -= 1;
                }
                display.truncate(cut);
                for b in map.iter_mut() {
                    *b = (*b).min(cut);
                }
            }
            let shaped = self.shape_highlighted(window, row, display, &map);
            max_line_w = max_line_w.max(shaped.width / px(1.));
            lines.push((row, shaped, map));
        }
        let max_scroll_x = (max_line_w - text_w + char_w * 8.).max(0.);
        self.scroll_x = self.scroll_x.clamp(0., max_scroll_x.max(self.scroll_x.min(max_scroll_x + 1.)));
        if self.scroll_x > max_scroll_x && self.drag.is_none() {
            self.scroll_x = max_scroll_x;
        }

        let text_x = text_left - self.scroll_x;
        let x_for = |shaped: &ShapedLine, map: &Vec<usize>, col: usize| -> f32 {
            shaped.x_for_index(map[col.min(map.len() - 1)]) / px(1.)
        };

        let mut p = EditorPaint::default();
        p.bounds = bounds;
        p.text_clip = Bounds::new(point(px(text_left - text_pad / 2.), bounds.top()), size(px(width - (text_left - left) + text_pad / 2.), bounds.size.height));
        p.line_h = px(line_h);

        if full {
            p.background = Some(theme::bg_editor());
        }

        // Diff decorations.
        let full_w = size(px(width), px(line_h));
        for (y, tint) in &tints {
            let color = if *tint == LineTint::Added { theme::diff_ins_line() } else { theme::diff_del_line() };
            p.quads_under.push(fill(Bounds::new(point(px(left), px(*y)), full_w), color));
        }
        for y in &fillers {
            p.quads_under.push(fill(Bounds::new(point(px(left), px(*y)), full_w), theme::diff_filler()));
        }
        for (y, i, shaped) in phantoms {
            p.quads_under.push(fill(Bounds::new(point(px(left), px(y)), full_w), theme::diff_del_line()));
            for r in decor.as_ref().and_then(|d| d.phantom_words.get(i)).into_iter().flatten() {
                let x0 = text_x + r.start as f32 * char_w;
                p.quads_under.push(fill(Bounds::new(point(px(x0), px(y)), size(px((r.end - r.start) as f32 * char_w), px(line_h))), theme::diff_del_char()));
            }
            p.text.push((point(px(text_x), px(y)), shaped));
        }
        // Changed words inside changed lines, like VS Code.
        if let Some(dec) = &decor {
            for (r, shaped, map) in &lines {
                let Some(words) = dec.words.get(r) else { continue };
                let color = if dec.tint.get(*r) == Some(&LineTint::Added) { theme::diff_ins_char() } else { theme::diff_del_char() };
                for w in words {
                    let x0 = x_for(shaped, map, w.start);
                    let x1 = x_for(shaped, map, w.end);
                    if x1 > x0 {
                        p.quads_under.push(fill(Bounds::new(point(px(text_x + x0), px(y_for(*r))), size(px(x1 - x0), px(line_h))), color));
                    }
                }
            }
        }

        // Current line highlight.
        if full && self.buf.sel.is_empty() && lines.iter().any(|(r, _, _)| *r == row) {
            p.quads_under.push(fill(
                Bounds::new(point(px(left + blame_w), px(y_for(row))), size(px(width - blame_w), px(line_h))),
                theme::line_highlight(),
            ));
        }

        // Search matches.
        if let Some(q) = self.search.as_ref().filter(|q| !q.is_empty()) {
            let lo = lines.first().map(|l| l.0).unwrap_or(0);
            let hi = lines.last().map(|l| l.0 + 1).unwrap_or(0);
            for (row, cols) in self.buf.find_all(q, lo..hi) {
                if let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| *r == row) {
                    let x0 = x_for(shaped, map, cols.start);
                    let x1 = x_for(shaped, map, cols.end);
                    p.quads_under.push(fill(
                        Bounds::new(point(px(text_x + x0), px(y_for(row))), size(px((x1 - x0).max(2.)), px(line_h))),
                        theme::find_match(),
                    ));
                }
            }
        }

        // Selection.
        let sel = self.buf.sel.range();
        if !sel.is_empty() {
            let color = if focused { theme::selection() } else { theme::selection_inactive() };
            for (row, shaped, map) in &lines {
                let rs = self.buf.line_start(*row);
                let re = rs + self.buf.line_len(*row);
                if sel.start > re || sel.end < rs || (sel.end == rs && sel.start < rs) {
                    continue;
                }
                let s = sel.start.max(rs) - rs;
                let e = sel.end.min(re) - rs;
                let x0 = x_for(shaped, map, s);
                let mut x1 = x_for(shaped, map, e);
                if sel.end > re {
                    x1 += char_w * 0.6;
                }
                if x1 > x0 {
                    p.quads_under.push(fill(
                        Bounds::new(point(px(text_x + x0), px(y_for(*row))), size(px(x1 - x0), px(line_h))),
                        color,
                    ));
                }
            }
        }

        // Text.
        for (row, shaped, _) in &lines {
            p.text.push((point(px(text_x), px(y_for(*row))), shaped.clone()));
        }
        if self.buf.len() == 0 && !self.placeholder.is_empty() {
            let ph = self.shape(window, self.placeholder.to_string(), theme::fg_muted());
            p.text.push((point(px(text_left), px(top + pad_top)), ph));
        }

        // Cursor.
        if focused
            && let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| *r == row)
        {
            let x = text_x + x_for(shaped, map, col);
            p.cursor = Some(fill(Bounds::new(point(px(x), px(y_for(row))), size(px(2.), px(line_h))), theme::cursor()));
        } else if focused && self.buf.len() == 0 {
            p.cursor = Some(fill(Bounds::new(point(px(text_left), px(top + pad_top)), size(px(2.), px(line_h))), theme::cursor()));
        }

        if full {
            let gutter_w = blame_w + numbers_w;
            p.gutter_bg = Some(fill(Bounds::new(bounds.origin, size(px(gutter_w + text_pad / 2.), bounds.size.height)), theme::bg_editor()));

            // Line numbers.
            let numbers_right = left + blame_w + numbers_w - 14.;
            for (r, _, _) in &lines {
                let color = if *r == row { theme::line_number_active() } else { theme::line_number() };
                let label = self.shape(window, (r + 1).to_string(), color);
                let x = numbers_right - label.width / px(1.);
                p.overlay_text.push((point(px(x), px(y_for(*r))), label));
            }

            // Git change markers.
            let mark_x = left + blame_w + numbers_w - 8.;
            for (r, _, _) in lines.iter().filter(|_| self.decor.is_none()) {
                let color = match self.marks.get(*r) {
                    Some(&MARK_ADDED) => theme::gutter_added(),
                    Some(&MARK_MODIFIED) => theme::gutter_modified(),
                    Some(&MARK_DELETED) => {
                        p.quads_over.push(fill(
                            Bounds::new(point(px(mark_x - 2.), px(y_for(*r) - 2.)), size(px(7.), px(4.))),
                            theme::gutter_deleted(),
                        ));
                        continue;
                    }
                    _ => continue,
                };
                p.quads_over.push(fill(Bounds::new(point(px(mark_x), px(y_for(*r))), size(px(3.), px(line_h))), color));
            }

            // Blame gutter.
            if self.blame_gutter {
                p.quads_over.push(fill(
                    Bounds::new(bounds.origin, size(px(blame_w), bounds.size.height)),
                    theme::bg_side(),
                ));
                let blame = self.blame.as_ref();
                let mut prev_sha: Option<&str> = None;
                for (r, _, _) in &lines {
                    let commit = blame.and_then(|b| b.get(*r)).and_then(|c| c.as_ref());
                    let sha = commit.map(|c| c.sha.as_str());
                    let y = y_for(*r);
                    if let Some(c) = commit {
                        let age_days = (util::now() - c.time) as f64 / 86400.;
                        let bar = if c.is_uncommitted() { theme::accent() } else { theme::blame_age(age_days) };
                        p.quads_over.push(fill(Bounds::new(point(px(left), px(y)), size(px(3.), px(line_h))), bar));
                    }
                    let first_line = lines.first().map(|l| l.0) == Some(*r);
                    if sha != prev_sha || first_line {
                        if !first_line {
                            p.quads_over.push(fill(Bounds::new(point(px(left), px(y)), size(px(blame_w), px(1.))), theme::border()));
                        }
                        if let Some(c) = commit {
                            let label = blame_label(c, self.user_email.as_deref(), 26);
                            let shaped = self.shape(window, label, theme::fg_dim());
                            p.overlay_text.push((point(px(left + 8.), px(y)), shaped));
                        }
                    }
                    prev_sha = sha;
                }
            }

            // Diagnostics: wavy underline, faint line tint and the message at the end of the line.
            let mut diag_rows = std::collections::HashSet::new();
            for d in self.diagnostics.iter() {
                for (r, shaped, map) in lines.iter().filter(|(r, _, _)| *r >= d.row && *r <= d.end_row) {
                    let y = y_for(*r);
                    let len = self.buf.line_len(*r);
                    let c0 = if *r == d.row { d.col.min(len) } else { 0 };
                    let mut c1 = if *r == d.end_row { d.end_col.min(len) } else { len };
                    if c1 <= c0 {
                        // Point diagnostics (tsc) underline the word they point at.
                        let pos = self.buf.rc_to_pos(*r, c0);
                        let w = self.buf.word_range_at(pos);
                        let ls = self.buf.line_start(*r);
                        c1 = if w.end > pos { w.end - ls } else { c0 + 1 };
                    }
                    let x0 = text_x + x_for(shaped, map, c0);
                    let x1 = (text_x + x_for(shaped, map, c1)).max(x0 + char_w);
                    let color = d.severity.color();
                    let mut pb = PathBuilder::stroke(px(1.));
                    let base = y + line_h - 2.5;
                    pb.move_to(point(px(x0), px(base)));
                    let mut x = x0;
                    let mut up = true;
                    while x < x1 {
                        x = (x + 2.).min(x1);
                        pb.line_to(point(px(x), px(if up { base - 2. } else { base })));
                        up = !up;
                    }
                    if let Ok(path) = pb.build() {
                        p.paths.push((path, color));
                    }
                    if *r == d.row && diag_rows.insert(*r) {
                        p.quads_under.push(fill(Bounds::new(point(px(left + blame_w), px(y)), size(px(width - blame_w), px(line_h))), color.opacity(0.07)));
                        let more = self.diagnostics.iter().filter(|o| o.row == *r).count();
                        let label = if more > 1 { format!("● {}  (+{})", d.message.lines().next().unwrap_or(""), more - 1) } else { format!("● {}", d.message.lines().next().unwrap_or("")) };
                        let shaped_label = self.shape(window, label, color.opacity(0.85));
                        let lx = text_x + shaped.width / px(1.) + char_w * 3.;
                        p.text.push((point(px(lx), px(y)), shaped_label));
                    }
                }
            }

            if self.blame_gutter || diag_rows.contains(&row) {
            } else if focused
                && self.buf.sel.is_empty()
                && let Some(c) = self.blame.as_ref().and_then(|b| b.get(row)).and_then(|c| c.clone())
                && let Some((_, shaped, _)) = lines.iter().find(|(r, _, _)| *r == row)
            {
                // Inline blame for the current line, GitLens-style.
                let who = blame_author(&c, self.user_email.as_deref());
                let label = if c.is_uncommitted() {
                    format!("{who}, Uncommitted changes")
                } else {
                    format!("{who}, {} • {}", util::relative_time(c.time), c.summary)
                };
                let shaped_label = self.shape(window, label, theme::fg_muted());
                let x = text_x + shaped.width / px(1.) + char_w * 4.;
                p.text.push((point(px(x), px(y_for(row))), shaped_label));
            }

            // Scrollbar.
            if max_scroll > 0. {
                let track = Bounds::new(point(px(left + width - 12.), bounds.top()), size(px(12.), bounds.size.height));
                let thumb_h = (height * height / content_h).max(24.);
                let thumb_y = top + (self.scroll_y / max_scroll) * (height - thumb_h);
                let thumb = Bounds::new(point(px(left + width - 11.), px(thumb_y)), size(px(10.), px(thumb_h)));
                p.quads_over.push(fill(thumb, theme::scrollbar()));
                // Cursor position tick.
                let tick_y = top + (cursor_disp as f32 * line_h / content_h.max(1.)) * height;
                p.quads_over.push(fill(Bounds::new(point(px(left + width - 12.), px(tick_y)), size(px(12.), px(2.))), theme::cursor().opacity(0.6)));
                self.layout_scrollbar = Some(ScrollbarGeom { track, thumb, max_scroll });
            } else {
                self.layout_scrollbar = None;
            }
        }

        self.layout = Some(Layout {
            bounds,
            text_left,
            gutter_left: left,
            blame_w,
            line_h,
            char_w,
            pad_top,
            lines,
            scrollbar: self.layout_scrollbar.take(),
        });
        p
    }
}

fn blame_author(c: &BlameCommit, user_email: Option<&str>) -> String {
    if c.is_uncommitted() || user_email.is_some_and(|e| e == c.email) {
        "You".into()
    } else {
        c.author.clone()
    }
}

fn blame_label(c: &BlameCommit, user_email: Option<&str>, width: usize) -> String {
    if c.is_uncommitted() {
        return "You · uncommitted".into();
    }
    let when = util::short_time(c.time);
    let who = blame_author(c, user_email);
    let room = width.saturating_sub(when.len() + 2);
    let who: String = if who.chars().count() > room {
        who.chars().take(room.saturating_sub(1)).chain(std::iter::once('…')).collect()
    } else {
        who
    };
    format!("{who:<room$} {when}")
}

fn disp_of(d: &DiffDecor, row: usize) -> usize {
    match d.disp_of_line.get(row) {
        Some(v) => *v,
        None => {
            let last = d.disp_of_line.last().copied().unwrap_or(0);
            last + row + 1 - d.disp_of_line.len()
        }
    }
}

/// Aligns `base` (left) against `current` (right) for side-by-side and inline diffs.
pub fn compute_decors(base: Arc<String>, current: &str) -> DiffDecors {
    use similar::{DiffOp, TextDiff};
    let base_lines: Vec<&str> = base.split('\n').collect();
    let cur_lines: Vec<&str> = current.split('\n').collect();
    let cur_count = cur_lines.len();
    let mut left = DiffDecor { tint: vec![LineTint::None; base_lines.len()], disp_of_line: vec![0; base_lines.len()], ..Default::default() };
    let mut right = DiffDecor { tint: vec![LineTint::None; cur_count], disp_of_line: vec![0; cur_count], ..Default::default() };
    let mut inline = DiffDecor { tint: vec![LineTint::None; cur_count], disp_of_line: vec![0; cur_count], ..Default::default() };
    let (mut added, mut removed) = (0, 0);
    let diff = TextDiff::configure().timeout(Duration::from_millis(300)).diff_lines(base.as_str(), current);
    fn push_line(d: &mut DiffDecor, row: usize, tint: LineTint) {
        if row < d.disp_of_line.len() {
            d.disp_of_line[row] = d.rows.len();
            d.tint[row] = tint;
        }
        d.rows.push(DRow::Line(row));
    }
    for op in diff.ops() {
        let (old_index, old_len, new_index, new_len) = match *op {
            DiffOp::Equal { old_index, new_index, len } => {
                for i in 0..len {
                    push_line(&mut left, old_index + i, LineTint::None);
                    push_line(&mut right, new_index + i, LineTint::None);
                    push_line(&mut inline, new_index + i, LineTint::None);
                }
                continue;
            }
            DiffOp::Delete { old_index, old_len, new_index } => (old_index, old_len, new_index, 0),
            DiffOp::Insert { old_index, new_index, new_len } => (old_index, 0, new_index, new_len),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => (old_index, old_len, new_index, new_len),
        };
        added += new_len;
        removed += old_len;
        left.hunks.push(old_index);
        right.hunks.push(new_index.min(cur_count.saturating_sub(1)));
        inline.hunks.push(new_index.min(cur_count.saturating_sub(1)));
        let n = old_len.max(new_len);
        let mut old_words = Vec::new();
        for i in 0..old_len.min(new_len) {
            let (o, nw) = (old_index + i, new_index + i);
            let (lw, rw) = word_ranges(base_lines.get(o).unwrap_or(&""), cur_lines.get(nw).unwrap_or(&""));
            left.words.insert(o, lw.clone());
            right.words.insert(nw, rw.clone());
            inline.words.insert(nw, rw);
            old_words.push(lw);
        }
        for i in 0..n {
            if i < old_len {
                push_line(&mut left, old_index + i, LineTint::Removed);
            } else {
                left.rows.push(DRow::Filler);
            }
            if i < new_len {
                push_line(&mut right, new_index + i, LineTint::Added);
            } else {
                right.rows.push(DRow::Filler);
            }
        }
        for i in 0..old_len {
            inline.phantom.push(base_lines.get(old_index + i).unwrap_or(&"").to_string());
            inline.phantom_words.push(old_words.get(i).cloned().unwrap_or_default());
            inline.rows.push(DRow::Phantom(inline.phantom.len() - 1));
        }
        for i in 0..new_len {
            push_line(&mut inline, new_index + i, LineTint::Added);
        }
    }
    DiffDecors { base, left: Arc::new(left), right: Arc::new(right), inline: Arc::new(inline), added, removed }
}

/// Char ranges that differ between two versions of a line (word granularity).
fn word_ranges(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    use similar::{ChangeTag, TextDiff};
    if old.len() + new.len() > 4000 {
        return (Vec::new(), Vec::new());
    }
    let diff = TextDiff::configure().diff_words(old, new);
    let (mut l, mut r) = (Vec::<Range<usize>>::new(), Vec::<Range<usize>>::new());
    let (mut lo, mut ro) = (0usize, 0usize);
    let push = |v: &mut Vec<Range<usize>>, range: Range<usize>| match v.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => v.push(range),
    };
    for change in diff.iter_all_changes() {
        let n = change.value().chars().count();
        match change.tag() {
            ChangeTag::Equal => {
                lo += n;
                ro += n;
            }
            ChangeTag::Delete => {
                push(&mut l, lo..lo + n);
                lo += n;
            }
            ChangeTag::Insert => {
                push(&mut r, ro..ro + n);
                ro += n;
            }
        }
    }
    (l, r)
}

fn compute_marks(base: &str, current: &str) -> Vec<u8> {
    use similar::{DiffOp, TextDiff};
    let lines = current.split('\n').count();
    let mut marks = vec![0u8; lines];
    let diff = TextDiff::configure().timeout(Duration::from_millis(200)).diff_lines(base, current);
    for op in diff.ops() {
        match *op {
            DiffOp::Insert { new_index, new_len, .. } => {
                for m in marks.iter_mut().skip(new_index).take(new_len) {
                    *m = MARK_ADDED;
                }
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                for m in marks.iter_mut().skip(new_index).take(new_len) {
                    *m = MARK_MODIFIED;
                }
            }
            DiffOp::Delete { new_index, .. } => {
                if let Some(m) = marks.get_mut(new_index)
                    && *m == 0
                {
                    *m = MARK_DELETED;
                }
            }
            DiffOp::Equal { .. } => {}
        }
    }
    marks
}

// ---------------------------------------------------------------- input handler (typing, IME)

impl EntityInputHandler for Editor {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let r = self.chars_from_utf16(&range_utf16);
        actual.replace(self.utf16_from_chars(&r));
        Some(self.buf.slice(r))
    }

    fn selected_text_range(&mut self, _ignore_disabled: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.utf16_from_chars(&self.buf.sel.range()),
            reversed: self.buf.sel.head < self.buf.sel.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.utf16_from_chars(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .map(|r| self.chars_from_utf16(&r))
            .or(self.marked.clone())
            .unwrap_or(self.buf.sel.range());
        self.marked = None;
        let text = if self.mode == EditorMode::SingleLine { text.replace('\n', " ") } else { text.to_string() };
        self.buf.sel = Sel { anchor: range.start, head: range.end };
        self.buf.insert(&text);
        self.after_edit(cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str, new_sel_utf16: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .map(|r| self.chars_from_utf16(&r))
            .or(self.marked.clone())
            .unwrap_or(self.buf.sel.range());
        self.buf.sel = Sel { anchor: range.start, head: range.end };
        self.buf.insert(text);
        let n = text.chars().count();
        self.marked = (n > 0).then(|| range.start..range.start + n);
        if let Some(sel) = new_sel_utf16 {
            let local: Vec<u16> = text.encode_utf16().collect();
            let to_chars = |u: usize| String::from_utf16_lossy(&local[..u.min(local.len())]).chars().count();
            self.buf.sel = Sel { anchor: range.start + to_chars(sel.start), head: range.start + to_chars(sel.end) };
        }
        self.after_edit(cx);
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, _element_bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let l = self.layout.as_ref()?;
        let r = self.chars_from_utf16(&range_utf16);
        let (row, col) = self.buf.pos_to_rc(r.start);
        let x = l.text_left - self.scroll_x + self.buf.display_col(row, col) as f32 * l.char_w;
        let y = l.bounds.top() / px(1.) + l.pad_top + row as f32 * l.line_h - self.scroll_y;
        Some(Bounds::new(point(px(x), px(y)), size(px(l.char_w), px(l.line_h))))
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let pos = self.pos_for_point(point);
        Some(self.buf.rope.char_to_utf16_cu(pos))
    }
}

impl Editor {
    fn chars_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        let total = self.buf.rope.len_utf16_cu();
        self.buf.rope.utf16_cu_to_char(r.start.min(total))..self.buf.rope.utf16_cu_to_char(r.end.min(total))
    }

    fn utf16_from_chars(&self, r: &Range<usize>) -> Range<usize> {
        let len = self.buf.len();
        self.buf.rope.char_to_utf16_cu(r.start.min(len))..self.buf.rope.char_to_utf16_cu(r.end.min(len))
    }
}

// ---------------------------------------------------------------- element

#[derive(Default)]
pub struct EditorPaint {
    bounds: Bounds<Pixels>,
    text_clip: Bounds<Pixels>,
    line_h: Pixels,
    background: Option<Hsla>,
    quads_under: Vec<PaintQuad>,
    text: Vec<(Point<Pixels>, ShapedLine)>,
    gutter_bg: Option<PaintQuad>,
    overlay_text: Vec<(Point<Pixels>, ShapedLine)>,
    quads_over: Vec<PaintQuad>,
    paths: Vec<(Path<Pixels>, Hsla)>,
    cursor: Option<PaintQuad>,
}

pub struct EditorElement {
    editor: Entity<Editor>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let ed = self.editor.read(cx);
        let lh = ed.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        match ed.mode {
            EditorMode::Full => style.size.height = relative(1.).into(),
            EditorMode::SingleLine => style.size.height = px(lh + 6.).into(),
            EditorMode::MultiLine => {
                let rows = ed.buf.line_count().clamp(1, 10) as f32;
                style.size.height = px(rows * lh).into();
            }
        }
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> EditorPaint {
        self.editor.update(cx, |ed, _| ed.layout(bounds, window))
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), p: &mut EditorPaint, window: &mut Window, cx: &mut App) {
        let focus = self.editor.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.editor.clone()), cx);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(bg) = p.background {
                window.paint_quad(fill(p.bounds, bg));
            }
            for q in p.quads_under.drain(..) {
                window.paint_quad(q);
            }
            window.with_content_mask(Some(ContentMask { bounds: p.text_clip }), |window| {
                for (origin, line) in &p.text {
                    line.paint(*origin, p.line_h, window, cx).ok();
                }
                for (path, color) in p.paths.drain(..) {
                    window.paint_path(path, color);
                }
                if let Some(c) = p.cursor.take() {
                    window.paint_quad(c);
                }
            });
            if let Some(g) = p.gutter_bg.take() {
                window.paint_quad(g);
            }
            for q in p.quads_over.drain(..) {
                window.paint_quad(q);
            }
            for (origin, line) in &p.overlay_text {
                line.paint(*origin, p.line_h, window, cx).ok();
            }
        });
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .key_context("Editor")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::move_word_left))
            .on_action(cx.listener(Self::move_word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::move_line_start))
            .on_action(cx.listener(Self::move_line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::move_doc_start))
            .on_action(cx.listener(Self::move_doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::select_page_up))
            .on_action(cx.listener(Self::select_page_down))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::kill_line))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::newline_above))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_line))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::move_line_up))
            .on_action(cx.listener(Self::move_line_down))
            .on_action(cx.listener(Self::duplicate_line_down))
            .on_action(cx.listener(Self::duplicate_line_up))
            .on_action(cx.listener(Self::delete_line))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::scroll_line_up))
            .on_action(cx.listener(Self::scroll_line_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(EditorElement { editor: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::{DRow, LineTint, compute_decors};
    use std::sync::Arc;

    #[test]
    fn decors_align_both_sides() {
        let base = Arc::new("a\nb\nc\nd".to_string());
        let d = compute_decors(base, "a\nB\nB2\nc\nd");
        // b -> B,B2: left gets one filler so rows line up.
        assert_eq!(d.left.rows.len(), d.right.rows.len());
        assert!(matches!(d.left.rows[2], DRow::Filler));
        assert_eq!(d.right.tint[1], LineTint::Added);
        assert_eq!(d.left.tint[1], LineTint::Removed);
        assert_eq!((d.added, d.removed), (2, 1));
        // Inline: old "b" shown as a phantom before the new lines.
        assert!(matches!(d.inline.rows[1], DRow::Phantom(0)));
        assert_eq!(d.inline.disp_of_line[1], 2);
        assert_eq!(d.right.hunks, vec![1]);
        // "b" -> "B": the changed word is highlighted, not the whole line.
        let d = compute_decors(Arc::new("hello".into()), "hello in oskl code");
        assert_eq!(d.right.words.get(&0).unwrap(), &vec![5..18]);
        assert!(d.left.words.get(&0).unwrap().is_empty());
    }
}
