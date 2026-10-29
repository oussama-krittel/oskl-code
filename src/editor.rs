//! The text editor view. Used for files, the commit message box and the
//! command line, so every text field behaves the same way.

use crate::buffer::{Buffer, RowChange, Sel};
use crate::git::{self, BlameCommit};
use crate::highlight::{self, Lang, LineSpans};
use crate::complete::{self, Ctx, Flavor, Item, Kind};
use crate::{colors, emmet, secrets, theme, tools, tserror, util};
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
        Escape, Submit, ShowCharacterPalette, ScrollLineUp, ScrollLineDown, SelectNextOccurrence,
        SelectAllOccurrences, GoToDefinition, Fold, Unfold, FoldAll, UnfoldAll, NextConflict, PrevConflict, ShowHover,
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
        crate::keybindings::kb("cmd-d", SelectNextOccurrence, c),
        crate::keybindings::kb("f12", GoToDefinition, c),
        crate::keybindings::kb("cmd-k cmd-i", ShowHover, c),
        crate::keybindings::kb("cmd-shift-l", SelectAllOccurrences, c),
        crate::keybindings::kb("alt-cmd-[", Fold, c),
        crate::keybindings::kb("alt-cmd-]", Unfold, c),
        crate::keybindings::kb("cmd-k cmd-0", FoldAll, c),
        crate::keybindings::kb("cmd-k cmd-j", UnfoldAll, c),
        crate::keybindings::kb("f7", NextConflict, c),
        crate::keybindings::kb("shift-f7", PrevConflict, c),
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
    /// "Compare" on a conflict: current vs incoming of this file.
    CompareConflict,
    /// "Resolve in Merge Editor".
    OpenMergeEditor,
    /// Go to definition landed in another file.
    OpenAt(crate::definition::Target),
    /// A short message for the status bar.
    Notice(String),
}

impl EventEmitter<EditorEvent> for Editor {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictAction {
    Current,
    Incoming,
    Both,
    Compare,
    MergeEditor,
}

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
    /// Continuation of a soft-wrapped line: (buffer row, segment ≥ 1).
    Cont(usize, usize),
}

thread_local! {
    /// Word wrap: on for every code editor when the user turns it on (⌥Z). Per UI thread
    /// (there's one in the app; tests each get their own).
    static WORD_WRAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn word_wrap() -> bool {
    WORD_WRAP.with(|w| w.get())
}

pub fn set_word_wrap(on: bool) {
    WORD_WRAP.with(|w| w.set(on));
}

/// Where each line is soft-wrapped, for the current text, width and folds.
pub struct WrapData {
    key: (u64, usize, Vec<(usize, usize)>),
    /// Char columns where each row's segments start (always begins with 0).
    starts: Vec<Vec<usize>>,
    /// Display columns continuation segments are indented by (the line's own indent).
    indent: Vec<usize>,
    decor: Arc<DiffDecor>,
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
    /// ⌥⇧-drag box selection from (row, display column).
    Column(usize, usize),
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

pub struct HoverCard {
    /// The name it's for.
    range: Range<usize>,
    at: Point<Pixels>,
    keyboard: bool,
    info: crate::signature::Info,
}

/// What a project-wide definition search is for.
enum Then {
    Underline,
    Go,
    Hover(Range<usize>, bool),
}

pub struct Completion {
    pub ctx: Ctx,
    pub items: Vec<Item>,
    /// Indices into `items` that match what's typed, best first.
    pub shown: Vec<usize>,
    pub sel: usize,
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
    /// Where a ⌘D run started (the first selection and the scroll), to come back to once
    /// every occurrence is selected.
    occurrence_origin: Option<(Range<usize>, f32)>,
    /// The suggestion list under the cursor (tags, attributes, words, import paths).
    pub completion: Option<Completion>,
    /// The name or import path under the mouse while ⌘ is held (underlined, ⌘-click follows it).
    link: Option<(Range<usize>, Hsla)>,
    /// Whether the last name hovered with ⌘ leads anywhere: (its range, buffer version, answer).
    link_memo: Option<(Range<usize>, u64, bool)>,
    cmd_held: bool,
    /// Project-wide lookups of names not found in the file, kept briefly so hover and click
    /// don't search twice.
    def_cache: std::collections::HashMap<String, (std::time::Instant, Option<crate::definition::Target>)>,
    def_pending: std::collections::HashSet<String>,
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
    find_matches: Arc<Vec<Range<usize>>>,
    /// Folded regions: (first row, last hidden row); rows after the first are hidden.
    folds: Vec<(usize, usize)>,
    fold_decor: Option<Arc<DiffDecor>>,
    wrap_data: Option<Arc<WrapData>>,
    pub conflicts: Vec<crate::buffer::Conflict>,
    conflicts_ver: Option<u64>,
    /// Clickable conflict actions from the last layout: (bounds, conflict, action).
    conflict_hits: Vec<(Bounds<Pixels>, usize, ConflictAction)>,
    find_current: Option<usize>,
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
    /// The problems card: the row it's for, where it hangs, and whether the keyboard opened it.
    hover_diag: Option<(usize, Point<Pixels>, bool)>,
    /// Where problems are drawn (underlines and end-of-line labels), for hovering.
    diag_hits: Vec<(Bounds<Pixels>, usize)>,
    /// The card for the name under the mouse (or the cursor, with ⌘K ⌘I): its declaration and docs.
    hover_info: Option<HoverCard>,
    /// The name the mouse rests on, before its card opens.
    hover_wait: Option<Range<usize>>,
    /// `.env` values to hide, per row, for this buffer version.
    env_masks: Option<(u64, Arc<Vec<Option<Range<usize>>>>)>,
}

/// Lines per press for the scroll-view shortcuts.
const SCROLL_STEP: f32 = 3.;

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
            occurrence_origin: None,
            completion: None,
            link: None,
            link_memo: None,
            cmd_held: false,
            def_cache: Default::default(),
            def_pending: Default::default(),
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
            find_matches: Arc::default(),
            folds: Vec::new(),
            fold_decor: None,
            wrap_data: None,
            conflicts: Vec::new(),
            conflicts_ver: None,
            conflict_hits: Vec::new(),
            find_current: None,
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
            hover_diag: None,
            diag_hits: Vec::new(),
            hover_info: None,
            hover_wait: None,
            env_masks: None,
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

    /// Matches highlighted by the find bar (char ranges), with the current one emphasized.
    pub fn set_find_matches(&mut self, matches: Arc<Vec<Range<usize>>>, current: Option<usize>, cx: &mut Context<Self>) {
        self.find_matches = matches;
        self.find_current = current;
        cx.notify();
    }

    /// Replaces a range as one undo step (used by find & replace).
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.buf.replace(range, text, None, crate::buffer::EditKind::Other);
        self.after_edit(cx);
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
        self.buf.single_cursor();
        self.reveal_row(row.min(self.buf.line_count().saturating_sub(1)));
        self.buf.sel = Sel::at(pos);
        self.autoscroll = Some(false);
        self.center_on_next_layout = true;
        cx.notify();
    }

    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.buf.single_cursor();
        let (row, _) = self.buf.pos_to_rc(range.start.min(self.buf.len()));
        self.reveal_row(row);
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

    // ------------------------------------------------------------ merge conflicts

    /// Re-reads conflict blocks when the text changed (cheap check: only files with markers).
    pub fn refresh_conflicts(&mut self) {
        if self.conflicts_ver == Some(self.buf.version()) {
            return;
        }
        self.conflicts_ver = Some(self.buf.version());
        self.conflicts = if self.mode == EditorMode::Full && self.buf.len() < 4_000_000 { self.buf.conflicts() } else { Vec::new() };
    }

    pub fn resolve_conflict(&mut self, ix: usize, action: ConflictAction, cx: &mut Context<Self>) {
        self.refresh_conflicts();
        let Some(c) = self.conflicts.get(ix).copied() else { return };
        match action {
            ConflictAction::Compare => return cx.emit(EditorEvent::CompareConflict),
            ConflictAction::MergeEditor => return cx.emit(EditorEvent::OpenMergeEditor),
            _ if self.read_only => return,
            ConflictAction::Current => self.buf.resolve_conflict(&c, true, false),
            ConflictAction::Incoming => self.buf.resolve_conflict(&c, false, true),
            ConflictAction::Both => self.buf.resolve_conflict(&c, true, true),
        }
        self.buf.single_cursor();
        self.after_edit(cx);
    }

    fn jump_conflict(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.refresh_conflicts();
        let (row, _) = self.cursor_rc();
        let target = if forward {
            self.conflicts.iter().find(|c| c.start > row).or(self.conflicts.first())
        } else {
            self.conflicts.iter().rev().find(|c| c.start < row).or(self.conflicts.last())
        };
        if let Some(c) = target.copied() {
            self.go_to(c.start, 0, cx);
        }
    }

    fn next_conflict(&mut self, _: &NextConflict, _: &mut Window, cx: &mut Context<Self>) { self.jump_conflict(true, cx) }
    fn prev_conflict(&mut self, _: &PrevConflict, _: &mut Window, cx: &mut Context<Self>) { self.jump_conflict(false, cx) }

    // ------------------------------------------------------------ folding

    fn rebuild_folds(&mut self) {
        self.folds.sort();
        self.folds.dedup();
        if self.folds.is_empty() || self.decor.is_some() {
            self.fold_decor = None;
            return;
        }
        let n = self.buf.line_count();
        let mut rows = Vec::with_capacity(n);
        let mut disp_of_line = Vec::with_capacity(n);
        let mut hidden_until: Option<usize> = None;
        for r in 0..n {
            if hidden_until.is_some_and(|e| r <= e) {
                disp_of_line.push(rows.len().saturating_sub(1));
                continue;
            }
            disp_of_line.push(rows.len());
            rows.push(DRow::Line(r));
            if let Some((_, e)) = self.folds.iter().find(|(s, _)| *s == r) {
                hidden_until = Some(hidden_until.map_or(*e, |h| h.max(*e)));
            }
        }
        self.fold_decor = Some(Arc::new(DiffDecor { rows, disp_of_line, ..Default::default() }));
    }

    /// Cheap check for the gutter: the next line with text is indented deeper.
    fn foldable_quick(&self, row: usize) -> bool {
        let Some(base) = self.buf.indent_width(row) else { return false };
        (row + 1..(row + 50).min(self.buf.line_count())).find_map(|r| self.buf.indent_width(r)).is_some_and(|w| w > base)
    }

    pub fn is_folded(&self, row: usize) -> bool {
        self.folds.iter().any(|(s, _)| *s == row)
    }

    fn row_hidden(&self, row: usize) -> bool {
        self.folds.iter().any(|(s, e)| row > *s && row <= *e)
    }

    /// Folds the region at `row` (or the innermost one containing it); unfolds it if already folded.
    pub fn toggle_fold(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(i) = self.folds.iter().position(|(s, _)| *s == row) {
            self.folds.remove(i);
        } else if let Some(f) = self.buf.fold_range(row) {
            self.folds.push(f);
        }
        self.rebuild_folds();
        cx.notify();
    }

    /// Opens every fold hiding `row` (when the cursor or a search lands inside one).
    fn reveal_row(&mut self, row: usize) {
        let before = self.folds.len();
        self.folds.retain(|(s, e)| !(row > *s && row <= *e));
        if self.folds.len() != before {
            self.rebuild_folds();
        }
    }

    /// Keeps folds on the right rows after an edit; folds an edit touched are opened.
    fn splice_folds(&mut self, changes: &[RowChange]) {
        if self.folds.is_empty() {
            return;
        }
        for ch in changes {
            let delta = ch.new_end as isize - ch.old_end as isize;
            self.folds.retain(|(s, e)| ch.old_end < *s || ch.start > *e);
            for (s, e) in self.folds.iter_mut() {
                if *s > ch.old_end {
                    *s = (*s as isize + delta).max(0) as usize;
                    *e = (*e as isize + delta).max(0) as usize;
                }
            }
        }
        self.rebuild_folds();
    }

    fn fold(&mut self, _: &Fold, _: &mut Window, cx: &mut Context<Self>) {
        let (row, _) = self.cursor_rc();
        // The innermost region containing the cursor.
        let target = (0..=row).rev().filter_map(|r| self.buf.fold_range(r)).find(|(s, e)| *s == row || (row > *s && row <= *e));
        if let Some(f) = target
            && !self.folds.contains(&f)
        {
            self.folds.push(f);
            self.buf.single_cursor();
            if row != f.0 {
                let col = self.buf.line_len(f.0);
                self.buf.sel = Sel::at(self.buf.rc_to_pos(f.0, col));
            }
            self.rebuild_folds();
            self.after_move(cx);
        }
    }

    fn unfold(&mut self, _: &Unfold, _: &mut Window, cx: &mut Context<Self>) {
        let (row, _) = self.cursor_rc();
        self.folds.retain(|(s, e)| !(row >= *s && row <= *e));
        self.rebuild_folds();
        cx.notify();
    }

    fn fold_all(&mut self, _: &FoldAll, _: &mut Window, cx: &mut Context<Self>) {
        // Top-level regions only (nested ones are inside anyway).
        let mut r = 0;
        self.folds.clear();
        while r < self.buf.line_count() {
            match self.buf.fold_range(r) {
                Some(f) => {
                    self.folds.push(f);
                    r = f.1 + 1;
                }
                None => r += 1,
            }
        }
        let (row, _) = self.cursor_rc();
        if let Some((s, _)) = self.folds.iter().find(|(s, e)| row > *s && row <= *e) {
            let s = *s;
            self.buf.single_cursor();
            self.buf.sel = Sel::at(self.buf.line_start(s));
        }
        self.rebuild_folds();
        self.after_move(cx);
    }

    fn unfold_all(&mut self, _: &UnfoldAll, _: &mut Window, cx: &mut Context<Self>) {
        self.folds.clear();
        self.rebuild_folds();
        cx.notify();
    }

    // ------------------------------------------------------------ word wrap

    fn wrap_active(&self) -> bool {
        word_wrap() && self.mode == EditorMode::Full && self.decor.is_none()
    }

    /// Recomputes wrap points when the text, the width (in columns) or the folds changed.
    fn ensure_wrap(&mut self, cols: usize) {
        let key = (self.buf.version(), cols, self.folds.clone());
        if self.wrap_data.as_ref().is_some_and(|w| w.key == key) {
            return;
        }
        let n = self.buf.line_count();
        let mut starts = Vec::with_capacity(n);
        let mut indent = Vec::with_capacity(n);
        let mut rows = Vec::with_capacity(n);
        let mut disp_of_line = Vec::with_capacity(n);
        for r in 0..n {
            let (st, ind) = wrap_points(&self.buf, r, cols);
            if self.row_hidden(r) {
                disp_of_line.push(rows.len().saturating_sub(1));
            } else {
                disp_of_line.push(rows.len());
                rows.push(DRow::Line(r));
                rows.extend((1..st.len()).map(|k| DRow::Cont(r, k)));
            }
            starts.push(st);
            indent.push(ind);
        }
        let decor = Arc::new(DiffDecor { rows, disp_of_line, ..Default::default() });
        self.wrap_data = Some(Arc::new(WrapData { key, starts, indent, decor }));
    }

    /// Segment starts of a row (just `[0]` without wrap).
    pub(crate) fn seg_starts(&self, row: usize) -> &[usize] {
        match self.wrap_data.as_ref().filter(|_| self.wrap_active()) {
            Some(w) => w.starts.get(row).map(|v| v.as_slice()).unwrap_or(&[0]),
            None => &[0],
        }
    }

    /// Up/down by screen row when lines are wrapped (keeps the column inside the segment).
    fn wrapped_vertical(&mut self, delta: isize, extend: bool) -> bool {
        let Some(w) = self.wrap_data.clone().filter(|_| self.wrap_active() && self.buf.extra.is_empty()) else { return false };
        let (r, c) = self.buf.pos_to_rc(self.buf.sel.head);
        let st = &w.starts[r.min(w.starts.len() - 1)];
        let k = seg_index(st, c);
        let goal = self.buf.goal_col.unwrap_or_else(|| self.buf.display_col(r, c) - self.buf.display_col(r, st[k]));
        let disp = disp_of(&w.decor, r) + k;
        let last = w.decor.rows.len().saturating_sub(1) as isize;
        let target = disp as isize + delta;
        let pos = if target < 0 {
            0
        } else if target > last {
            self.buf.len()
        } else {
            let (tr, tk) = match w.decor.rows[target as usize] {
                DRow::Cont(tr, tk) => (tr, tk),
                DRow::Line(tr) => (tr, 0),
                _ => return false,
            };
            let ts = &w.starts[tr];
            let base = self.buf.display_col(tr, ts[tk]);
            let end = ts.get(tk + 1).map(|e| e - 1).unwrap_or(self.buf.line_len(tr));
            let col = self.buf.col_for_display(tr, base + goal).clamp(ts[tk], end.max(ts[tk]));
            self.buf.rc_to_pos(tr, col)
        };
        if !extend && !self.buf.sel.is_empty() {
            self.buf.sel = Sel::at(self.buf.sel.head);
        }
        self.buf.move_to(pos, extend);
        self.buf.goal_col = Some(goal);
        true
    }

    /// Display rows: the diff layout, or the folds of a plain file.
    fn view_decor(&self) -> Option<&Arc<DiffDecor>> {
        self.decor.as_ref().or(self.wrap_data.as_ref().map(|w| &w.decor)).or(self.fold_decor.as_ref())
    }

    fn disp_count(&self) -> usize {
        match self.view_decor() {
            Some(d) => d.rows.len().max(1),
            None => self.buf.line_count(),
        }
    }

    fn disp_of(&self, row: usize) -> usize {
        match self.view_decor() {
            Some(d) => disp_of(d, row),
            None => row,
        }
    }

    fn line_at_disp(&self, disp: usize) -> usize {
        let last = self.buf.line_count().saturating_sub(1);
        let Some(d) = self.view_decor() else { return disp.min(last) };
        let disp = disp.min(d.rows.len().saturating_sub(1));
        let line = |r: &DRow| match r {
            DRow::Line(l) | DRow::Cont(l, _) => Some(*l),
            _ => None,
        };
        let found = d.rows[disp..].iter().find_map(line).or_else(|| d.rows[..disp].iter().rev().find_map(line));
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

    // ------------------------------------------------------------ go to definition

    /// The syntax capture at a column (None for plain text).
    pub(crate) fn capture_at(&self, row: usize, col: usize) -> Option<u8> {
        let spans = self.spans.as_ref()?.get(row)?;
        spans.iter().rev().find(|(r, _)| r.start <= col && col < r.end).map(|(_, h)| *h)
    }

    /// With ⌘ held over `p`: the range to underline and its text colour, only when a click there
    /// would go somewhere (its own declaration counts).
    fn link_at(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) -> Option<(Range<usize>, Hsla)> {
        let l = self.layout.as_ref()?;
        if p.x / px(1.) < l.text_left {
            return None;
        }
        let pos = self.pos_for_point(p);
        self.link_for_pos(pos, cx)
    }

    pub(crate) fn link_for_pos(&mut self, pos: usize, cx: &mut Context<Self>) -> Option<(Range<usize>, Hsla)> {
        use crate::definition::{Found, Hit};
        let hit = crate::definition::hit_at(&self.buf, pos)?;
        let (range, module) = match &hit {
            Hit::Module(_, r) => (r.clone(), true),
            Hit::Name(_, r) => (r.clone(), false),
        };
        let (row, col) = self.buf.pos_to_rc(range.start);
        let cap = self.capture_at(row, col);
        if !module && self.inert(&range) {
            return None;
        }
        let version = self.buf.version();
        let ok = match &self.link_memo {
            Some((r, v, ok)) if *r == range && *v == version => *ok,
            _ => {
                let ok = match crate::definition::find(&self.buf, range.start, self.path.as_deref(), self.root.as_deref()) {
                    Found::At(_) => true,
                    Found::Nothing => false,
                    Found::Search(name) => match self.cached_definition(&name) {
                        Some(t) => t.is_some(),
                        None => {
                            // Unknown yet: look it up in the background and underline when found.
                            self.search_definition(name, Then::Underline, cx);
                            return None;
                        }
                    },
                };
                self.link_memo = Some((range.clone(), version, ok));
                ok
            }
        };
        ok.then(|| (range, cap.map(highlight::color).unwrap_or_else(theme::fg)))
    }

    /// Whether the name at `range` is code that can't lead anywhere (a keyword, string, comment…).
    fn inert(&self, range: &Range<usize>) -> bool {
        const INERT: &[&str] = &["comment", "string", "number", "keyword", "boolean", "punctuation", "operator", "escape", "character", "constant.builtin", "type.builtin", "variable.builtin", "text."];
        let (row, col) = self.buf.pos_to_rc(range.start);
        let kind = self.capture_at(row, col).map(highlight::name).unwrap_or("");
        INERT.iter().any(|k| kind.starts_with(k))
    }

    fn update_link(&mut self, cx: &mut Context<Self>) {
        let link = (self.cmd_held && self.mode == EditorMode::Full).then(|| self.link_at(self.last_mouse, cx)).flatten();
        if link != self.link {
            self.link = link;
            cx.notify();
        }
    }

    fn modifiers_changed(&mut self, ev: &ModifiersChangedEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.cmd_held = ev.modifiers.platform;
        self.update_link(cx);
    }

    fn cached_definition(&self, name: &str) -> Option<Option<crate::definition::Target>> {
        self.def_cache.get(name).filter(|(at, _)| at.elapsed() < Duration::from_secs(30)).map(|(_, t)| t.clone())
    }

    /// Looks `name` up across the project off the UI thread, then does `then` with it.
    fn search_definition(&mut self, name: String, then: Then, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        if matches!(then, Then::Underline) && !self.def_pending.insert(name.clone()) {
            return;
        }
        let skip = self.path.clone();
        // In JS/TS a name that isn't imported or declared here can only come from an export
        // elsewhere (anything else is a parameter or a local we couldn't place).
        let exported_only = complete::flavor(self.lang.or_else(|| self.path.as_deref().and_then(highlight::detect))).is_some();
        if matches!(then, Then::Go) {
            cx.emit(EditorEvent::Notice(format!("Looking for {name}…")));
        }
        cx.spawn(async move |this, cx| {
            let n = name.clone();
            let found = cx.background_spawn(async move { crate::definition::search_project(&root, &n, skip.as_deref(), exported_only) }).await;
            this.update(cx, |ed, cx| {
                ed.def_pending.remove(&name);
                ed.def_cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(30));
                ed.def_cache.insert(name.clone(), (std::time::Instant::now(), found.clone()));
                match then {
                    Then::Go => match found {
                        Some(t) => ed.land(t, cx),
                        None => cx.emit(EditorEvent::Notice(format!("No definition found for {name}"))),
                    },
                    Then::Underline => {
                        ed.link_memo = None;
                        ed.update_link(cx);
                    }
                    // Only if that name is still what the card is wanted for.
                    Then::Hover(range, keyboard) => {
                        let wanted = if keyboard { range.contains(&ed.buf.sel.head) || range.end == ed.buf.sel.head } else { ed.hover_wait.as_ref() == Some(&range) };
                        if let Some(t) = found.filter(|_| wanted) {
                            ed.open_hover(range, t, keyboard, cx);
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Through the workspace even within this file, so ⌃- can come back.
    fn land(&mut self, t: crate::definition::Target, cx: &mut Context<Self>) {
        if self.path.is_some() {
            cx.emit(EditorEvent::OpenAt(t));
        } else {
            self.go_to(t.row, t.col, cx);
        }
    }

    fn go_to_definition_action(&mut self, _: &GoToDefinition, _: &mut Window, cx: &mut Context<Self>) {
        self.go_to_definition(self.buf.sel.head, cx);
    }

    /// Jumps to what's at `pos`: in this file directly, in another through the workspace, or
    /// after a project-wide search in the background.
    pub fn go_to_definition(&mut self, pos: usize, cx: &mut Context<Self>) {
        use crate::definition::Found;
        self.completion = None;
        match crate::definition::find(&self.buf, pos, self.path.as_deref(), self.root.as_deref()) {
            Found::At(t) => self.land(t, cx),
            Found::Nothing => {}
            Found::Search(name) => match self.cached_definition(&name) {
                Some(Some(t)) => self.land(t, cx),
                Some(None) => cx.emit(EditorEvent::Notice(format!("No definition found for {name}"))),
                None => self.search_definition(name, Then::Go, cx),
            },
        }
    }

    // ------------------------------------------------------------ completion

    fn flavor(&self) -> Option<Flavor> {
        if self.mode != EditorMode::Full {
            return None;
        }
        complete::flavor(self.lang.or_else(|| self.path.as_deref().and_then(highlight::detect)))
    }

    /// After a character typed at a single cursor: close tags, then update the suggestions.
    fn after_type(&mut self, text: &str) {
        let Some(fl) = self.flavor() else { return };
        if !self.buf.extra.is_empty() || !self.buf.sel.is_empty() {
            return;
        }
        let pos = self.buf.sel.head;
        let edit = crate::buffer::EditKind::Other;
        match text {
            ">" => {
                if let Some(close) = complete::close_tag_after_gt(&self.buf, pos, fl) {
                    self.buf.replace(pos..pos, &close, Some(Sel::at(pos)), edit);
                }
            }
            "/" => {
                if let Some(rest) = complete::close_tag_after_slash(&self.buf, pos, fl) {
                    let existing = format!("</{}>", rest.trim_end_matches('>'));
                    let end = (pos + existing.chars().count()).min(self.buf.len());
                    if self.buf.slice(pos..end) == existing {
                        // The closing tag `>` already added is right here: step over it instead.
                        self.buf.replace(pos - 2..pos, "", Some(Sel::at(end - 2)), edit);
                    } else {
                        let n = rest.chars().count() + usize::from(!rest.ends_with('>'));
                        self.buf.replace(pos..pos, &rest, Some(Sel::at(pos + n)), edit);
                    }
                }
            }
            _ => {}
        }
        self.refresh_completion(true);
    }

    /// Recomputes the suggestion list for the cursor. A new list only opens while typing;
    /// otherwise (backspace) the open one is refiltered or closed.
    fn refresh_completion(&mut self, typed: bool) {
        let old = self.completion.take();
        let Some(fl) = self.flavor() else { return };
        if !self.buf.extra.is_empty() || !self.buf.sel.is_empty() {
            return;
        }
        let Some(ctx) = complete::context(&self.buf, self.buf.sel.head, fl) else { return };
        let items = match old {
            Some(c) if c.ctx.same_source(&ctx) => c.items,
            _ if typed => complete::candidates(&self.buf, &ctx, fl, self.path.as_deref(), self.root.as_deref()),
            _ => return,
        };
        let shown = complete::filter(&items, ctx.prefix(), 60);
        if !shown.is_empty() {
            self.completion = Some(Completion { ctx, items, shown, sel: 0 });
        }
    }

    pub fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.completion.take() else { return };
        let item = c.items[c.shown[c.sel]].clone();
        let head = self.buf.sel.head;
        let start = head.saturating_sub(c.ctx.prefix().chars().count());
        let text = format!("{}{}", item.label, item.suffix);
        let cursor = start + text.chars().count() - item.back;
        let link = self.tag_link();
        self.buf.replace(start..head, &text, Some(Sel::at(cursor)), crate::buffer::EditKind::Other);
        self.sync_tag(link);
        self.after_edit(cx);
        // Into a folder: keep listing what's inside.
        if item.kind == Kind::Folder {
            self.refresh_completion(true);
        }
    }

    fn completion_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        const ROWS: usize = 10;
        let c = self.completion.as_ref()?;
        let l = self.layout.as_ref()?;
        let (row, col) = self.buf.pos_to_rc(self.buf.sel.head);
        let col = col.saturating_sub(c.ctx.prefix().chars().count());
        let st = self.seg_starts(row);
        let k = seg_index(st, col);
        let x = l.text_left - self.scroll_x + (self.buf.display_col(row, col) - self.buf.display_col(row, st[k])) as f32 * l.char_w;
        let y = l.bounds.top() / px(1.) + l.pad_top + (self.disp_of(row) + k + 1) as f32 * l.line_h - self.scroll_y;
        let first = (c.sel + 1).saturating_sub(ROWS);
        let rows = c.shown.iter().enumerate().skip(first).take(ROWS).map(|(i, &ix)| {
            let it = &c.items[ix];
            let color = match it.kind {
                Kind::Tag | Kind::Component => theme::accent(),
                Kind::Attr => theme::git_modified(),
                Kind::Type => theme::git_added(),
                Kind::Keyword => theme::c(0xc678dd),
                Kind::Folder | Kind::File => theme::fg_dim(),
                Kind::Word => theme::fg_muted(),
            };
            div()
                .id(i)
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(6.))
                .h(px(20.))
                .cursor_pointer()
                .when(i == c.sel, |d| d.bg(theme::selected_focused()).text_color(theme::fg_bright()))
                .child(div().w(px(16.)).flex_none().text_color(color).child(it.kind.glyph()))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(it.label.clone()))
                .child(div().flex_none().text_color(theme::fg_muted()).child(it.kind.label()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ed, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(c) = &mut ed.completion {
                            c.sel = i;
                        }
                        ed.accept_completion(cx);
                    }),
                )
        });
        Some(
            deferred(
                anchored().position(point(px(x), px(y))).snap_to_window().child(
                    div()
                        .id("completion")
                        .w(px(320.))
                        .py(px(2.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .rounded(px(4.))
                        .shadow_md()
                        .font_family(theme::MONO_FONT)
                        .text_size(px(12.))
                        .text_color(theme::fg())
                        .children(rows),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    // ------------------------------------------------------------ edit plumbing

    fn after_move(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.completion = None;
        if self.hover_diag.is_some_and(|(_, _, keyboard)| keyboard) {
            self.hover_diag = None;
        }
        if self.hover_info.as_ref().is_some_and(|h| h.keyboard) {
            self.hover_info = None;
        }
        self.autoscroll = Some(false);
        cx.notify();
    }

    fn after_edit(&mut self, cx: &mut Context<Self>) {
        let changes = self.buf.take_changes();
        self.splice_folds(&changes);
        self.splice_blame(&changes);
        self.splice_decorations(&changes);
        self.schedule_highlight(Duration::from_millis(30), cx);
        let delay = if self.diff_role == DiffRole::None { Duration::from_millis(120) } else { Duration::ZERO };
        self.schedule_marks(delay, cx);
        self.autoscroll = Some(false);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    /// A cursor motion, applied to every cursor.
    fn motion(&mut self, cx: &mut Context<Self>, f: impl FnMut(&mut Buffer)) {
        self.buf.each_cursor(false, f);
        if !self.folds.is_empty() {
            let (row, _) = self.cursor_rc();
            self.reveal_row(row);
        }
        self.after_move(cx);
    }

    /// Up/down over folded regions: hidden rows are skipped, not opened.
    fn vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        if self.wrapped_vertical(delta, extend) {
            let (row, _) = self.cursor_rc();
            self.reveal_row(row);
            return self.after_move(cx);
        }
        if self.folds.is_empty() || self.mode != EditorMode::Full {
            return self.motion(cx, |b| b.move_vertical(delta, extend));
        }
        let step = delta.signum();
        let mut left = delta.abs();
        let last = self.buf.line_count().saturating_sub(1);
        let (mut row, _) = self.cursor_rc();
        let mut moves = 0isize;
        while left > 0 {
            let mut next = row as isize + step;
            while next >= 0 && (next as usize) <= last && self.row_hidden(next as usize) {
                next += step;
            }
            if next < 0 || next as usize > last {
                moves += next - row as isize - step;
                break;
            }
            moves += next - row as isize;
            row = next as usize;
            left -= 1;
        }
        let moves = if moves == 0 { step } else { moves };
        self.buf.each_cursor(false, |b| b.move_vertical(moves, extend));
        self.after_move(cx);
    }

    /// An edit, applied at every cursor as one undo step.
    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnMut(&mut Buffer)) {
        if self.read_only {
            return;
        }
        let link = self.tag_link();
        self.buf.each_cursor(false, f);
        self.sync_tag(link);
        self.after_edit(cx);
    }

    /// An edit that must not drag a tag partner along (undo / redo restore both themselves).
    fn edit_unlinked(&mut self, cx: &mut Context<Self>, f: impl FnMut(&mut Buffer)) {
        if self.read_only {
            return;
        }
        self.buf.each_cursor(false, f);
        self.after_edit(cx);
    }

    /// A line edit (comment, indent, move…): once per line even with several cursors on it.
    fn edit_rows(&mut self, cx: &mut Context<Self>, f: impl FnMut(&mut Buffer)) {
        if self.read_only {
            return;
        }
        self.buf.each_cursor(true, f);
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
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) { self.buf.single_cursor(); self.buf.select_all(); cx.notify() }
    fn select_line(&mut self, _: &SelectLine, _: &mut Window, cx: &mut Context<Self>) { self.motion(cx, |b| b.select_line()) }
    fn select_next_occurrence(&mut self, _: &SelectNextOccurrence, _: &mut Window, cx: &mut Context<Self>) {
        let start = self.buf.extra.is_empty().then_some(self.scroll_y);
        let added = self.buf.select_next_occurrence();
        if let Some(y) = start {
            let first = self.buf.extra.first().unwrap_or(&self.buf.sel).range();
            self.occurrence_origin = Some((first, y));
        }
        if added {
            self.after_move(cx);
        }
        // ⌘D again once every occurrence is selected: go back to the one the run started on, at
        // the scroll it started at, ready to type.
        if !added && !self.buf.extra.is_empty() {
            if let Some((first, y)) = self.occurrence_origin.clone() {
                self.buf.make_primary(first);
                self.scroll_y = y;
                self.autoscroll = None;
                cx.notify();
            }
        }
    }
    fn select_all_occurrences(&mut self, _: &SelectAllOccurrences, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.select_all_occurrences();
        self.after_move(cx);
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.completion {
            c.sel = c.sel.checked_sub(1).unwrap_or(c.shown.len() - 1);
            return cx.notify();
        }
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Up);
        }
        self.vertical(-1, false, cx)
    }
    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.completion {
            c.sel = (c.sel + 1) % c.shown.len();
            return cx.notify();
        }
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Down);
        }
        self.vertical(1, false, cx)
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) { self.vertical(-1, true, cx) }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) { self.vertical(1, true, cx) }
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
    /// ⌃⌘↑ / ⌃⌘↓ (and ⌃⌥↑ / ⌃⌥↓): scroll the view a few lines, leaving the cursor where it is.
    fn scroll_line_up(&mut self, _: &ScrollLineUp, _: &mut Window, cx: &mut Context<Self>) {
        // In one-line inputs (finder, command line, search) this moves through the options.
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Up);
        }
        self.scroll_y = (self.scroll_y - self.line_height() * SCROLL_STEP).max(0.);
        cx.notify();
    }
    fn scroll_line_down(&mut self, _: &ScrollLineDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode == EditorMode::SingleLine {
            return cx.emit(EditorEvent::Down);
        }
        self.scroll_y += self.line_height() * SCROLL_STEP;
        cx.notify();
    }

    fn select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) { let n = self.page_rows(); self.motion(cx, |b| b.move_vertical(-n, true)) }
    fn select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) { let n = self.page_rows(); self.motion(cx, |b| b.move_vertical(n, true)) }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |b| b.backspace());
        if self.completion.is_some() {
            self.refresh_completion(false);
        }
    }
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
        if self.completion.is_some() {
            return self.accept_completion(cx);
        }
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
        if self.try_emmet(cx) {
            return;
        }
        if self.completion.is_some() {
            return self.accept_completion(cx);
        }
        match self.mode {
            EditorMode::SingleLine => cx.emit(EditorEvent::Tab),
            _ => self.edit_rows(cx, |b| b.indent_lines(false)),
        }
    }
    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode != EditorMode::SingleLine {
            self.edit_rows(cx, |b| b.indent_lines(true))
        }
    }
    fn move_line_up(&mut self, _: &MoveLineUp, _: &mut Window, cx: &mut Context<Self>) { self.edit_rows(cx, |b| b.move_lines(false)) }
    fn move_line_down(&mut self, _: &MoveLineDown, _: &mut Window, cx: &mut Context<Self>) { self.edit_rows(cx, |b| b.move_lines(true)) }
    fn duplicate_line_down(&mut self, _: &DuplicateLineDown, _: &mut Window, cx: &mut Context<Self>) { self.edit_rows(cx, |b| b.duplicate_lines(true)) }
    fn duplicate_line_up(&mut self, _: &DuplicateLineUp, _: &mut Window, cx: &mut Context<Self>) { self.edit_rows(cx, |b| b.duplicate_lines(false)) }
    fn delete_line(&mut self, _: &DeleteLine, _: &mut Window, cx: &mut Context<Self>) { self.edit_rows(cx, |b| b.delete_lines()) }
    fn toggle_comment(&mut self, _: &ToggleComment, _: &mut Window, cx: &mut Context<Self>) {
        match tools::comment_style(self.path.as_deref()) {
            tools::CommentStyle::Line(prefix) => self.edit_rows(cx, |b| b.toggle_comment(prefix)),
            tools::CommentStyle::Block(open, close) => self.edit_rows(cx, |b| b.toggle_block_comment(open, close)),
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) { self.buf.single_cursor(); self.edit_unlinked(cx, |b| { b.undo(); }) }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) { self.buf.single_cursor(); self.edit_unlinked(cx, |b| { b.redo(); }) }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.extra.is_empty() {
            // One line per cursor; pasting with the same number of cursors puts one at each.
            let parts: Vec<String> = self.buf.all_sels().iter().map(|s| self.buf.slice(s.range())).collect();
            self.line_copy = None;
            return cx.write_to_clipboard(ClipboardItem::new_string(parts.join("\n")));
        }
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
        if !self.buf.extra.is_empty() {
            return self.edit(cx, |b| b.insert(""));
        }
        if self.buf.sel.is_empty() {
            self.edit_rows(cx, |b| b.delete_lines())
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
        let cursors = self.buf.extra.len() + 1;
        let lines: Vec<&str> = text.split('\n').collect();
        if cursors > 1 && lines.len() == cursors {
            // Cursors are visited last-first, so hand out the lines from the end.
            let mut next = cursors;
            return self.edit(cx, |b| {
                next -= 1;
                b.insert(lines[next]);
            });
        }
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
        if self.completion.take().is_some() | self.hover_diag.take().is_some() | self.hover_info.take().is_some() {
            return cx.notify();
        }
        if !self.buf.extra.is_empty() {
            self.buf.single_cursor();
            return cx.notify();
        }
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

    fn disp_for_y(&self, y: Pixels) -> usize {
        let Some(l) = &self.layout else { return 0 };
        let rel = (y - l.bounds.top()) / px(1.) - l.pad_top + self.scroll_y;
        if rel < 0. {
            // Above the top edge: one row above the first visible row.
            ((self.scroll_y / l.line_h) as usize).saturating_sub(1)
        } else {
            (rel / l.line_h) as usize
        }
    }

    fn row_for_y(&self, y: Pixels) -> usize {
        self.line_at_disp(self.disp_for_y(y))
    }

    fn pos_for_point(&self, p: Point<Pixels>) -> usize {
        let Some(l) = &self.layout else { return self.buf.sel.head };
        if let Some(w) = self.wrap_data.as_ref().filter(|_| self.wrap_active()) {
            // Wrapped: find the segment under the pointer, then the column inside it.
            let disp = self.disp_for_y(p.y).min(w.decor.rows.len().saturating_sub(1));
            let (row, k) = match w.decor.rows.get(disp) {
                Some(DRow::Cont(r, k)) => (*r, *k),
                Some(DRow::Line(r)) => (*r, 0),
                _ => return self.buf.len(),
            };
            let st = &w.starts[row];
            let indent = if k == 0 { 0. } else { w.indent[row] as f32 * l.char_w };
            let x = p.x / px(1.) - l.text_left - indent;
            let col = match l.lines.iter().find(|(r, _, _)| *r == row) {
                Some((_, shaped, map)) => {
                    let x0 = shaped.x_for_index(map[st[k].min(map.len() - 1)]) / px(1.);
                    let byte = shaped.closest_index_for_x(px((x + x0).max(x0)));
                    map.iter().position(|b| *b >= byte).unwrap_or(map.len() - 1)
                }
                None => st[k],
            };
            let end = st.get(k + 1).map(|e| e - 1).unwrap_or(self.buf.line_len(row));
            return self.buf.rc_to_pos(row, col.clamp(st[k], end.max(st[k])));
        }
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
        self.completion = None;
        self.hover_info = None;
        self.hover_wait = None;
        if let Some((_, ix, action)) = self.conflict_hits.iter().find(|(b, _, _)| b.contains(&ev.position)).cloned() {
            return self.resolve_conflict(ix, action, cx);
        }
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
        if self.mode == EditorMode::Full && self.decor.is_none() && x >= l.text_left - 18. && x < l.text_left - 2. {
            // Fold chevron.
            let row = self.row_for_y(ev.position.y);
            if self.is_folded(row) || self.foldable_quick(row) {
                return self.toggle_fold(row, cx);
            }
        }
        let in_numbers = self.mode == EditorMode::Full && x < l.text_left;
        let pos = self.pos_for_point(ev.position);
        self.buf.goal_col = None;
        if !in_numbers && ev.modifiers.platform && ev.click_count <= 1 && self.mode == EditorMode::Full {
            self.link = None;
            return self.go_to_definition(pos, cx);
        }
        if !in_numbers && ev.modifiers.alt && ev.click_count <= 1 {
            if ev.modifiers.shift {
                // ⌥⇧-drag: box selection.
                let (row, col) = self.buf.pos_to_rc(pos);
                let dcol = self.buf.display_col(row, col);
                self.buf.single_cursor();
                self.buf.sel = Sel::at(pos);
                self.drag = Some(DragMode::Column(row, dcol));
            } else {
                // ⌥-click: add a cursor (or remove the one there).
                self.buf.toggle_cursor(pos);
                self.drag = None;
            }
            self.marked = None;
            return cx.notify();
        }
        self.buf.single_cursor();
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
            DragMode::Column(row0, col0) => {
                let row = self.row_for_y(p.y);
                let col = self.layout.as_ref().map(|l| ((p.x / px(1.) - l.text_left + self.scroll_x) / l.char_w).round().max(0.) as usize).unwrap_or(0);
                self.buf.select_columns((row0, col0), (row, col));
            }
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

    fn mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.last_mouse = ev.position;
        self.cmd_held = ev.modifiers.platform && !ev.dragging();
        self.update_link(cx);
        if !ev.dragging() {
            self.drag = None;
            self.scroll_drag = None;
            self.hover_problems(ev.position, cx);
            self.hover_word(ev.position, window, cx);
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
        self.hover_diag = None;
        self.hover_info = None;
        self.hover_wait = None;
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
        // Room between the numbers and the text for the fold chevrons.
        let text_pad = if full { 18. } else { 2. };
        let text_left = left + blame_w + numbers_w + text_pad;
        let text_w = (width - blame_w - numbers_w - text_pad - 10.).max(10.);
        let pad_top = if self.mode == EditorMode::SingleLine { ((height - line_h) / 2.).max(0.) } else { 0. };

        // Word wrap at the editor's width (monospace: a column count).
        let wrap_on = self.wrap_active();
        if wrap_on {
            self.ensure_wrap(((text_w - 6.) / char_w).floor().max(20.) as usize);
            self.scroll_x = 0.;
        } else {
            self.wrap_data = None;
        }

        // Scrolling.
        let disp_count = self.disp_count();
        let content_h = if full { (disp_count.saturating_sub(1)) as f32 * line_h + height } else { line_count as f32 * line_h };
        let max_scroll = (content_h - height).max(0.);
        let (row, col) = self.buf.pos_to_rc(self.buf.sel.head);
        let cursor_disp = self.disp_of(row) + seg_index(self.seg_starts(row), col);
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
            if !wrap_on && x < self.scroll_x {
                self.scroll_x = (x - char_w * 8.).max(0.);
            }
            if !wrap_on && x > self.scroll_x + text_w - char_w * 2. {
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

        let decor = self.view_decor().cloned();
        let first = (self.scroll_y / line_h) as usize;
        let visible = ((height / line_h).ceil() as usize + 1).min(disp_count.saturating_sub(first));
        let scroll_y = self.scroll_y;
        let y_disp = move |d: usize| top + pad_top + d as f32 * line_h - scroll_y;
        let y_for = |row: usize| y_disp(match &decor { Some(d) => disp_of(d, row), None => row });

        // `.env` values hidden except on the lines with a cursor; colour swatches elsewhere.
        let env_masks = if self.masks_env() { Some(self.env_masks_now()) } else { None };
        let revealed: std::collections::HashSet<usize> = if focused { self.buf.all_sels().iter().map(|s| self.buf.pos_to_rc(s.head).0).collect() } else { Default::default() };
        let mut swatches: Vec<(usize, usize, Hsla)> = Vec::new();
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
                Some(DRow::Cont(r, _)) if r < line_count => {
                    // A wrapped line's next segment: its line is shaped once (maybe here, if its
                    // first segment is scrolled off the top).
                    if lines.last().map(|l: &(usize, ShapedLine, Vec<usize>)| l.0) == Some(r) {
                        continue;
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
            if let Some(m) = env_masks.as_ref().and_then(|m| m.get(row).cloned().flatten()).filter(|_| !revealed.contains(&row)) {
                (display, map) = mask_cols(&display, &map, m.start, m.end);
            } else if full {
                for (col, color) in self.colors_in(row).into_iter().rev() {
                    (display, map) = insert_gap(&display, &map, col, 2);
                    swatches.push((row, col, color));
                }
            }
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
        let max_scroll_x = if wrap_on { 0. } else { (max_line_w - text_w + char_w * 8.).max(0.) };
        self.scroll_x = self.scroll_x.clamp(0., max_scroll_x.max(self.scroll_x.min(max_scroll_x + 1.)));
        if self.scroll_x > max_scroll_x && self.drag.is_none() {
            self.scroll_x = max_scroll_x;
        }

        let text_x = text_left - self.scroll_x;
        let x_for = |shaped: &ShapedLine, map: &Vec<usize>, col: usize| -> f32 {
            shaped.x_for_index(map[col.min(map.len() - 1)]) / px(1.)
        };
        // Wrap-aware geometry: a column's (x, y), and a column range split into on-screen pieces.
        // Without wrap every row is one segment and these are the plain x_for / y_for.
        let wd = self.wrap_data.clone().filter(|_| wrap_on);
        let starts = |r: usize| -> Vec<usize> { wd.as_ref().and_then(|w| w.starts.get(r).cloned()).unwrap_or_else(|| vec![0]) };
        let ind = |r: usize, k: usize| if k == 0 { 0. } else { wd.as_ref().and_then(|w| w.indent.get(r)).copied().unwrap_or(0) as f32 * char_w };
        let at = |r: usize, shaped: &ShapedLine, map: &Vec<usize>, col: usize| -> (f32, f32) {
            let st = starts(r);
            let k = seg_index(&st, col);
            (text_x + ind(r, k) + x_for(shaped, map, col) - x_for(shaped, map, st[k]), y_for(r) + k as f32 * line_h)
        };
        let spans = |r: usize, shaped: &ShapedLine, map: &Vec<usize>, c0: usize, c1: usize| -> Vec<(f32, f32, f32)> {
            let st = starts(r);
            let mut out = Vec::new();
            for k in 0..st.len() {
                let (s, e) = (st[k], st.get(k + 1).copied().unwrap_or(usize::MAX));
                let (a, b) = (c0.max(s), c1.min(e));
                if a > b || (a == b && c0 != c1) {
                    continue;
                }
                let base = text_x + ind(r, k) - x_for(shaped, map, s);
                out.push((base + x_for(shaped, map, a), base + x_for(shaped, map, b), y_for(r) + k as f32 * line_h));
            }
            out
        };
        let nsegs = |r: usize| starts(r).len();

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
                Bounds::new(point(px(left + blame_w), px(y_for(row))), size(px(width - blame_w), px(line_h * nsegs(row) as f32))),
                theme::line_highlight(),
            ));
        }

        // Find bar matches (the current one stronger).
        if !self.find_matches.is_empty() {
            for (row, shaped, map) in &lines {
                let rs = self.buf.line_start(*row);
                let re = rs + self.buf.line_len(*row);
                let first = self.find_matches.partition_point(|m| m.end <= rs);
                for (i, m) in self.find_matches.iter().enumerate().skip(first) {
                    if m.start > re {
                        break;
                    }
                    let c0 = m.start.max(rs) - rs;
                    let c1 = m.end.min(re) - rs;
                    let current = self.find_current == Some(i);
                    for (x0, x1, y) in spans(*row, shaped, map, c0, c1) {
                        let b = Bounds::new(point(px(x0), px(y)), size(px((x1 - x0).max(2.)), px(line_h)));
                        p.quads_under.push(if current {
                            fill(b, theme::ca(0xff9d0066)).border_widths(px(1.)).border_color(theme::c(0xffb340))
                        } else {
                            fill(b, theme::find_match())
                        });
                    }
                }
            }
        }
        // Old command-line search highlight.
        if let Some(q) = self.search.as_ref().filter(|q| !q.is_empty() && self.find_matches.is_empty()) {
            let lo = lines.first().map(|l| l.0).unwrap_or(0);
            let hi = lines.last().map(|l| l.0 + 1).unwrap_or(0);
            for (row, cols) in self.buf.find_all(q, lo..hi) {
                if let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| *r == row) {
                    for (x0, x1, y) in spans(row, shaped, map, cols.start, cols.end) {
                        p.quads_under.push(fill(Bounds::new(point(px(x0), px(y)), size(px((x1 - x0).max(2.)), px(line_h))), theme::find_match()));
                    }
                }
            }
        }

        // ⌘-hover: the name or path a click would follow.
        if let Some((link, color)) = self.link.clone() {
            let (row, c0) = self.buf.pos_to_rc(link.start);
            let c1 = c0 + (link.end - link.start);
            if let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| *r == row) {
                for (x0, x1, y) in spans(row, shaped, map, c0, c1) {
                    p.quads_under.push(fill(Bounds::new(point(px(x0), px(y + line_h - 2.)), size(px(x1 - x0), px(1.))), color));
                }
            }
        }

        // Selections (every cursor's).
        let color = if focused { theme::selection() } else { theme::selection_inactive() };
        // The current find match is drawn in its own colour, not hidden under the selection.
        let current_match = self.find_current.and_then(|i| self.find_matches.get(i)).cloned();
        for sel in self.buf.all_sels().into_iter().map(|s| s.range()).filter(|r| !r.is_empty() && Some(r) != current_match.as_ref()) {
            for (row, shaped, map) in &lines {
                let rs = self.buf.line_start(*row);
                let re = rs + self.buf.line_len(*row);
                if sel.start > re || sel.end < rs || (sel.end == rs && sel.start < rs) {
                    continue;
                }
                let s = sel.start.max(rs) - rs;
                let e = sel.end.min(re) - rs;
                let pieces = spans(*row, shaped, map, s, e);
                let n = pieces.len();
                for (i, (x0, mut x1, y)) in pieces.into_iter().enumerate() {
                    if sel.end > re && i + 1 == n {
                        x1 += char_w * 0.6;
                    }
                    if x1 <= x0 {
                        continue;
                    }
                    p.quads_under.push(fill(
                        Bounds::new(point(px(x0), px(y)), size(px(x1 - x0), px(line_h))),
                        color,
                    ));
                }
            }
        }

        // Merge conflicts: current (green) and incoming (blue) blocks, actions on the <<<<<<< line.
        self.conflict_hits.clear();
        if full {
            self.refresh_conflicts();
        }
        if full && !self.conflicts.is_empty() {
            let width_all = px(width);
            let (cur_head, cur_body) = (theme::ca(0x2ea0436e), theme::ca(0x2ea04326));
            let (inc_head, inc_body) = (theme::ca(0x3794ff6e), theme::ca(0x3794ff26));
            let base_tint = theme::ca(0x8b919c22);
            let first_row = lines.first().map(|l| l.0).unwrap_or(0);
            let last_row = lines.last().map(|l| l.0).unwrap_or(0);
            for (ix, c) in self.conflicts.clone().iter().enumerate() {
                if c.end < first_row || c.start > last_row {
                    continue;
                }
                for (row, shaped, map) in &lines {
                    let tint = if *row == c.start {
                        cur_head
                    } else if *row < c.base.unwrap_or(c.mid) && *row > c.start {
                        cur_body
                    } else if c.base.is_some_and(|b| *row >= b) && *row < c.mid {
                        base_tint
                    } else if *row == c.mid {
                        theme::ca(0x00000000)
                    } else if *row > c.mid && *row < c.end {
                        inc_body
                    } else if *row == c.end {
                        inc_head
                    } else {
                        continue;
                    };
                    p.quads_under.push(fill(Bounds::new(point(bounds.left(), px(y_for(*row))), size(width_all, px(line_h * nsegs(*row) as f32))), tint));
                    if *row == c.start {
                        let (end_x, end_y) = at(*row, shaped, map, self.buf.line_len(*row));
                        let mut x = end_x + char_w * 3.;
                        let y_row = end_y;
                        let actions = [
                            ("Accept Current", ConflictAction::Current),
                            ("Accept Incoming", ConflictAction::Incoming),
                            ("Accept Both", ConflictAction::Both),
                            ("Compare", ConflictAction::Compare),
                            ("Merge Editor", ConflictAction::MergeEditor),
                        ];
                        for (i, (label, action)) in actions.into_iter().enumerate() {
                            if i > 0 {
                                let sep = self.shape(window, "|".into(), theme::fg_muted());
                                p.overlay_text.push((point(px(x), px(y_row)), sep.clone()));
                                x += sep.width / px(1.) + char_w;
                            }
                            let shaped = self.shape(window, label.into(), theme::c(0xd7dae0));
                            let w = shaped.width / px(1.);
                            let hit = Bounds::new(point(px(x), px(y_row)), size(px(w), px(line_h)));
                            p.overlay_text.push((point(px(x), px(y_row)), shaped));
                            self.conflict_hits.push((hit, ix, action));
                            x += w + char_w;
                        }
                    }
                }
            }
        }

        if full {
            // Indent guides (blank lines take the indent of the next line with text).
            let unit = if self.buf.indent.starts_with('\t') { self.buf.tab_width } else { self.buf.indent.chars().count() }.max(1);
            for (row, _, _) in &lines {
                let w = self.buf.indent_width(*row).or_else(|| (row + 1..(row + 200).min(line_count)).find_map(|r| self.buf.indent_width(r))).unwrap_or(0);
                for k in 0..w / unit {
                    let x = text_x + (k * unit) as f32 * char_w;
                    if x >= text_left - 1. {
                        p.quads_under.push(fill(Bounds::new(point(px(x.round()), px(y_for(*row))), size(px(1.), px(line_h * nsegs(*row) as f32))), theme::indent_guide()));
                    }
                }
            }
            // Matching bracket.
            if focused
                && self.buf.extra.is_empty()
                && let Some((a, b)) = self.buf.matching_bracket(self.buf.sel.head)
            {
                for pos in [a, b] {
                    let (r, c) = self.buf.pos_to_rc(pos);
                    if let Some((_, shaped, map)) = lines.iter().find(|(lr, _, _)| *lr == r) {
                        let (x0, y0) = at(r, shaped, map, c);
                        let w = x_for(shaped, map, c + 1) - x_for(shaped, map, c);
                        let bb = Bounds::new(point(px(x0), px(y0)), size(px(w.max(2.)), px(line_h)));
                        p.quads_under.push(fill(bb, theme::bracket_match()).border_widths(px(1.)).border_color(theme::bracket_match_border()));
                    }
                }
            }
            // "⋯" after a folded line.
            for (row, shaped, map) in lines.iter().filter(|(r, _, _)| self.is_folded(*r)) {
                let (ex, ey) = at(*row, shaped, map, self.buf.line_len(*row));
                let x = ex + char_w;
                let label = self.shape(window, " ⋯ ".into(), theme::fg_dim());
                let bb = Bounds::new(point(px(x), px(ey + 2.)), size(label.width, px(line_h - 4.)));
                p.quads_under.push(fill(bb, theme::hover()).corner_radii(px(3.)));
                p.text.push((point(px(x), px(ey)), label));
            }
        }

        // Colour swatches, in the gap opened before each colour.
        let sw = (char_w * 1.5).min(line_h * 0.7).round();
        for (srow, scol, color) in &swatches {
            if let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| r == srow) {
                let (x, y) = at(*srow, shaped, map, *scol);
                let gap = 2. * char_w;
                let b = Bounds::new(point(px((x - gap + (gap - sw) / 2.).round()), px((y + (line_h - sw) / 2.).round())), size(px(sw), px(sw)));
                p.quads_under.push(fill(b, *color).corner_radii(px(2.)).border_widths(px(1.)).border_color(theme::fg_muted().opacity(0.7)));
            }
        }

        // Text.
        for (row, shaped, map) in &lines {
            let st = starts(*row);
            if st.len() == 1 {
                p.text.push((point(px(text_x), px(y_for(*row))), shaped.clone()));
                continue;
            }
            // Each segment paints the whole line shifted left, clipped to that segment's text.
            for k in 0..st.len() {
                let x0 = x_for(shaped, map, st[k]);
                let x1 = st.get(k + 1).map(|e| x_for(shaped, map, *e)).unwrap_or(shaped.width / px(1.));
                let y = y_for(*row) + k as f32 * line_h;
                let left_x = text_x + ind(*row, k);
                let clip = Bounds::new(point(px(left_x), px(y)), size(px(x1 - x0 + 1.), px(line_h)));
                p.text_clipped.push((point(px(left_x - x0), px(y)), shaped.clone(), clip));
            }
        }
        if self.buf.len() == 0 && !self.placeholder.is_empty() {
            let ph = self.shape(window, self.placeholder.to_string(), theme::fg_muted());
            p.text.push((point(px(text_left), px(top + pad_top)), ph));
        }

        // Extra cursors.
        if focused {
            for s in &self.buf.extra {
                let (r, c) = self.buf.pos_to_rc(s.head);
                if let Some((_, shaped, map)) = lines.iter().find(|(lr, _, _)| *lr == r) {
                    let (x, y) = at(r, shaped, map, c);
                    p.quads_over.push(fill(Bounds::new(point(px(x), px(y)), size(px(2.), px(line_h))), theme::cursor()));
                }
            }
        }

        // Cursor.
        if focused
            && let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| *r == row)
        {
            let (x, y) = at(row, shaped, map, col);
            p.cursor = Some(fill(Bounds::new(point(px(x), px(y)), size(px(2.), px(line_h))), theme::cursor()));
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

            // Fold chevrons: ▸ on folded lines, a faint ▾ where something is nested below.
            let chevron_x = left + blame_w + numbers_w + 1.;
            for (r, _, _) in lines.iter().filter(|_| self.decor.is_none()) {
                let folded = self.is_folded(*r);
                if !folded && !self.foldable_quick(*r) {
                    continue;
                }
                let (glyph, color) = if folded { ("▸", theme::fg()) } else { ("▾", theme::line_number()) };
                let label = self.shape(window, glyph.into(), color);
                p.overlay_text.push((point(px(chevron_x), px(y_for(*r))), label));
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
            let mut hits = Vec::new();
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
                    let color = d.severity.color();
                    for (x0, x1, sy) in spans(*r, shaped, map, c0, c1) {
                        let x1 = x1.max(x0 + char_w);
                        hits.push((Bounds::new(point(px(x0), px(sy)), size(px(x1 - x0), px(line_h))), d.row));
                        let mut pb = PathBuilder::stroke(px(1.));
                        let base = sy + line_h - 2.5;
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
                    }
                    if *r == d.row && diag_rows.insert(*r) {
                        p.quads_under.push(fill(Bounds::new(point(px(left + blame_w), px(y)), size(px(width - blame_w), px(line_h * nsegs(*r) as f32))), color.opacity(0.07)));
                        let more = self.diagnostics.iter().filter(|o| o.row == *r).count();
                        let text = tserror::label(&d.message, d.source == "ts");
                        let label = if more > 1 { format!("● {text}  (+{})", more - 1) } else { format!("● {text}") };
                        let shaped_label = self.shape(window, label, color.opacity(0.85));
                        let (ex, ey) = at(*r, shaped, map, len);
                        hits.push((Bounds::new(point(px(ex + char_w * 3.), px(ey)), size(shaped_label.width, px(line_h))), d.row));
                        p.text.push((point(px(ex + char_w * 3.), px(ey)), shaped_label));
                    }
                }
            }

            self.diag_hits = hits;
            if self.blame_gutter || diag_rows.contains(&row) {
            } else if focused
                && self.buf.sel.is_empty()
                && let Some(c) = self.blame.as_ref().and_then(|b| b.get(row)).and_then(|c| c.clone())
                && let Some((_, shaped, map)) = lines.iter().find(|(r, _, _)| *r == row)
            {
                // Inline blame for the current line, GitLens-style.
                let who = blame_author(&c, self.user_email.as_deref());
                let label = if c.is_uncommitted() {
                    format!("{who}, Uncommitted changes")
                } else {
                    format!("{who}, {} • {}", util::relative_time(c.time), c.summary)
                };
                let shaped_label = self.shape(window, label, theme::fg_muted());
                let (ex, ey) = at(row, shaped, map, self.buf.line_len(row));
                p.text.push((point(px(ex + char_w * 4.), px(ey)), shaped_label));
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

/// Which segment of a wrapped line a column is in.
/// Hides char columns `c0..c1` of a displayed line behind dots (one per char), keeping `map`
/// (char column → byte offset) right.
fn mask_cols(display: &str, map: &[usize], c0: usize, c1: usize) -> (String, Vec<usize>) {
    let last = map.len() - 1;
    let (c0, c1) = (c0.min(last), c1.min(last).max(c0.min(last)));
    let (b0, b1) = (map[c0], map[c1]);
    let dot = '•';
    let n = c1 - c0;
    let mut out = String::with_capacity(display.len() + n * 2);
    out.push_str(&display[..b0]);
    out.extend(std::iter::repeat_n(dot, n));
    out.push_str(&display[b1..]);
    let delta = (n * dot.len_utf8()) as isize - (b1 - b0) as isize;
    let map = map
        .iter()
        .enumerate()
        .map(|(k, &b)| if k < c0 { b } else if k <= c1 { b0 + (k - c0) * dot.len_utf8() } else { (b as isize + delta) as usize })
        .collect();
    (out, map)
}

/// Opens a gap of `n` spaces before char column `col` of a displayed line (for a swatch).
fn insert_gap(display: &str, map: &[usize], col: usize, n: usize) -> (String, Vec<usize>) {
    let col = col.min(map.len() - 1);
    let b = map[col];
    let mut out = String::with_capacity(display.len() + n);
    out.push_str(&display[..b]);
    out.extend(std::iter::repeat_n(' ', n));
    out.push_str(&display[b..]);
    let map = map.iter().enumerate().map(|(k, &x)| if k >= col { x + n } else { x }).collect();
    (out, map)
}

/// One problem in the hover card: severity, code or rule, and the message laid out (TypeScript
/// chains as indented steps, long types formatted).
fn problem_card(d: &Diag) -> AnyElement {
    let ts = d.source == "ts";
    let code = tserror::code(&d.message);
    let tag = match (&code, ts) {
        (Some(c), true) => c.clone(),
        (Some(c), false) => format!("{} · {c}", d.source),
        (None, _) => d.source.clone(),
    };
    let head = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().text_color(d.severity.color()).child("●"))
        .child(div().text_color(theme::fg_dim()).child(tag));
    let body: Vec<AnyElement> = if ts {
        tserror::steps(&d.message).iter().flat_map(step_elements).collect()
    } else {
        vec![div().child(tserror::body(&d.message)).into_any_element()]
    };
    div().flex().flex_col().gap(px(5.)).child(head).children(body).into_any_element()
}

/// A step of a TypeScript explanation: prose with the short types coloured inline, and long
/// types as their own formatted block.
fn step_elements(st: &tserror::Step) -> Vec<AnyElement> {
    let type_color = highlight::color(highlight_index("type"));
    let pad = px(st.depth as f32 * 14.);
    let mut out = Vec::new();
    let mut text = String::new();
    let mut hl: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    fn flush(text: &mut String, hl: &mut Vec<(Range<usize>, HighlightStyle)>, pad: Pixels, out: &mut Vec<AnyElement>) {
        if !text.trim().is_empty() {
            let styled = StyledText::new(std::mem::take(text)).with_highlights(std::mem::take(hl));
            out.push(div().pl(pad).child(styled).into_any_element());
        }
        text.clear();
        hl.clear();
    }
    if st.depth > 0 {
        text.push_str("↳ ");
    }
    for part in &st.parts {
        match part {
            tserror::Part::Text(t) => text.push_str(t),
            tserror::Part::Code(c) if c.chars().count() > 60 => {
                flush(&mut text, &mut hl, pad, &mut out);
                out.push(
                    div()
                        .ml(pad + px(14.))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(4.))
                        .bg(theme::ca(0xffffff0a))
                        .font_family(theme::MONO_FONT)
                        .text_color(type_color)
                        .child(tserror::format_type(c))
                        .into_any_element(),
                );
            }
            tserror::Part::Code(c) => {
                let start = text.len();
                text.push_str(c);
                hl.push((start..text.len(), HighlightStyle { color: Some(type_color), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }));
            }
        }
    }
    flush(&mut text, &mut hl, pad, &mut out);
    out
}

/// The index of a capture name, for its colour.
fn highlight_index(name: &str) -> u8 {
    (0..u8::MAX).find(|i| highlight::name(*i) == name).unwrap_or(0)
}

fn seg_index(starts: &[usize], col: usize) -> usize {
    starts.partition_point(|s| *s <= col).saturating_sub(1)
}

/// Where a line wraps at `cols` columns: after the last space that fits (else mid-word), with
/// continuation rows indented like the line itself (unless that leaves too little room).
fn wrap_points(buf: &Buffer, row: usize, cols: usize) -> (Vec<usize>, usize) {
    let line = buf.line(row);
    let chars: Vec<char> = line.chars().collect();
    let mut dcols = Vec::with_capacity(chars.len() + 1);
    let mut d = 0;
    for ch in &chars {
        dcols.push(d);
        d = if *ch == '\t' { (d / buf.tab_width + 1) * buf.tab_width } else { d + 1 };
    }
    dcols.push(d);
    let mut starts = vec![0];
    if d <= cols {
        return (starts, 0);
    }
    let lead = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
    let indent = if dcols[lead] * 2 < cols { dcols[lead] } else { 0 };
    let mut seg_start = 0;
    let mut room = cols;
    let mut last_break: Option<usize> = None;
    for i in 0..chars.len() {
        if dcols[i + 1] - dcols[seg_start] > room && i > seg_start {
            let b = last_break.filter(|b| *b > seg_start && *b <= i).unwrap_or(i);
            starts.push(b);
            seg_start = b;
            room = cols - indent;
            last_break = None;
        }
        if chars[i] == ' ' && i >= lead {
            last_break = Some(i + 1);
        }
    }
    (starts, indent)
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
        let range_utf16_none = range_utf16.is_none() && self.marked.is_none();
        let typed_at_cursors = range_utf16_none && !self.buf.extra.is_empty();
        let range = range_utf16
            .map(|r| self.chars_from_utf16(&r))
            .or(self.marked.clone())
            .unwrap_or(self.buf.sel.range());
        self.marked = None;
        let text = if self.mode == EditorMode::SingleLine { text.replace('\n', " ") } else { text.to_string() };
        let code = self.mode == EditorMode::Full && range_utf16_none;
        if typed_at_cursors {
            return self.edit(cx, |b| if code { b.type_char(&text) } else { b.insert(&text) });
        }
        self.buf.single_cursor();
        self.buf.sel = Sel { anchor: range.start, head: range.end };
        let link = self.tag_link();
        if code {
            self.buf.type_char(&text);
            self.sync_tag(link);
            self.after_type(&text);
        } else {
            self.buf.insert(&text);
            self.sync_tag(link);
        }
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

impl Editor {
    // ------------------------------------------------------------ linked tags, abbreviations

    /// The tag name the cursor is in, with the matching tag that should follow it (one cursor).
    fn tag_link(&self) -> Option<complete::TagLink> {
        let fl = self.flavor()?;
        if !self.buf.extra.is_empty() || self.read_only {
            return None;
        }
        complete::tag_link(&self.buf, self.buf.sel.range(), fl)
    }

    /// After an edit inside a tag name: the matching tag gets the same name, in the same undo step.
    fn sync_tag(&mut self, link: Option<complete::TagLink>) {
        if let Some(link) = link
            && let Some((range, name)) = complete::linked_rename(&self.buf, &link)
        {
            self.buf.replace_linked(range, &name);
        }
    }

    /// Tab right after an abbreviation (`View.card>Text*2`) in JSX or HTML: expands it.
    fn try_emmet(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(fl) = self.flavor() else { return false };
        if fl == Flavor::Script || self.read_only || !self.buf.extra.is_empty() || !self.buf.sel.is_empty() {
            return false;
        }
        let pos = self.buf.sel.head;
        let (row, col) = self.buf.pos_to_rc(pos);
        let line: Vec<char> = self.buf.line(row).chars().collect();
        if line.get(col).is_some_and(|c| !c.is_whitespace() && !matches!(c, ')' | '}' | ']' | ';' | ',')) {
            return false;
        }
        let before: String = line[..col.min(line.len())].iter().collect();
        let start_col = emmet::abbreviation_start(&before);
        if start_col >= col {
            return false;
        }
        let start = self.buf.line_start(row) + start_col;
        if complete::emmet_blocked(&self.buf, start) {
            return false;
        }
        // In scripts, only where markup can start (`const x = a.b` stays code).
        let lead: String = line[..start_col].iter().collect();
        let lead = lead.trim_end();
        let markup_spot = lead.is_empty() || lead.ends_with(['(', '>', '{', '?', ':']) || ["return", "&&", "||", "=>"].iter().any(|w| lead.ends_with(w));
        if fl == Flavor::Jsx && !markup_spot {
            return false;
        }
        let target = match fl {
            Flavor::Html => emmet::Target::Html,
            _ if complete::is_react_native(&self.buf) => emmet::Target::ReactNative,
            _ => emmet::Target::Jsx,
        };
        let abbr: String = line[start_col..col].iter().collect();
        // With suggestions open, Tab picks one, unless what's typed is clearly an abbreviation.
        if self.completion.is_some() && !abbr.contains(['.', '#', '>', '+', '*', '[', '{', '^', '(']) {
            return false;
        }
        let base: String = line.iter().take_while(|c| **c == ' ' || **c == '\t').collect();
        let Some((text, cursor)) = emmet::expand(&abbr, target, &self.buf.indent, &base) else { return false };
        self.completion = None;
        self.buf.replace(start..pos, &text, Some(Sel::at(start + cursor)), crate::buffer::EditKind::Other);
        self.after_edit(cx);
        true
    }

    // ------------------------------------------------------------ env values, colours

    /// The width drawn into last frame, and how far the text is scrolled sideways.
    #[cfg(test)]
    pub fn drawn_width(&self) -> (f32, f32) {
        (self.layout.as_ref().map_or(0., |l| l.bounds.size.width / px(1.)), self.scroll_x)
    }

    /// The open hover card's declaration and description.
    #[cfg(test)]
    pub fn hover_card(&self) -> Option<(String, String)> {
        self.hover_info.as_ref().map(|h| (h.info.sig.clone(), h.info.doc.text.clone()))
    }

    /// The middle of the character at `pos`, on screen.
    #[cfg(test)]
    pub fn point_of(&self, pos: usize) -> Option<Point<Pixels>> {
        let l = self.layout.as_ref()?;
        let a = self.anchor_below(pos)?;
        Some(point(a.x + px(l.char_w / 2.), a.y - px(l.line_h / 2.)))
    }

    /// Whether the problems card is open.
    #[cfg(test)]
    pub fn problem_card_open(&self) -> bool {
        self.hover_diag.is_some()
    }

    /// Where problems were drawn in the last layout (underlines, then labels).
    #[cfg(test)]
    pub fn problem_spots(&self) -> Vec<Bounds<Pixels>> {
        self.diag_hits.iter().map(|(b, _)| *b).collect()
    }

    /// The text drawn for a row in the last layout (masks and swatch gaps included).
    #[cfg(test)]
    pub fn shown_row(&self, row: usize) -> Option<String> {
        self.layout.as_ref()?.lines.iter().find(|(r, _, _)| *r == row).map(|(_, s, _)| s.text.to_string())
    }

    /// `.env` files show their values as dots, except on the line being edited.
    fn masks_env(&self) -> bool {
        self.mode == EditorMode::Full && self.path.as_deref().is_some_and(secrets::is_env_path)
    }

    fn env_masks_now(&mut self) -> Arc<Vec<Option<Range<usize>>>> {
        let version = self.buf.version();
        if let Some((v, m)) = &self.env_masks
            && *v == version
        {
            return m.clone();
        }
        let m = Arc::new(secrets::env_masks(&self.buf.text()));
        self.env_masks = Some((version, m.clone()));
        m
    }

    /// Colours written on a row: the column the swatch goes before (the opening quote when the
    /// colour is the whole string) and the colour.
    fn colors_in(&self, row: usize) -> Vec<(usize, Hsla)> {
        let Some(lang) = self.lang else { return Vec::new() };
        let line = self.buf.line(row);
        if line.len() > 3_000 || !(line.contains('#') || line.contains("rgb") || line.contains("hsl")) {
            return Vec::new();
        }
        let chars: Vec<char> = line.chars().collect();
        let spans = self.spans.as_ref().and_then(|s| s.get(row)).filter(|s| !s.is_empty());
        let in_string = |col: usize| match spans {
            Some(sp) => sp.iter().any(|(r, h)| r.contains(&col) && highlight::name(*h).starts_with("string")),
            None => col > 0 && matches!(chars.get(col - 1), Some('\'' | '"' | '`')),
        };
        colors::find(&line, matches!(lang, Lang::Css | Lang::Html), in_string)
            .into_iter()
            .map(|f| {
                let quote = f.cols.start.checked_sub(1).and_then(|i| chars.get(i)).filter(|q| matches!(q, '\'' | '"' | '`'));
                let whole = quote.is_some() && chars.get(f.cols.end) == quote;
                let [r, g, b, a] = f.rgba;
                (if whole { f.cols.start - 1 } else { f.cols.start }, Hsla::from(Rgba { r, g, b, a }))
            })
            .collect()
    }

    // ------------------------------------------------------------ problems card

    /// ⌘K ⌘I: the card for what's at the cursor: the name's declaration and docs, and the
    /// problems on the line.
    fn show_hover(&mut self, _: &ShowHover, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.buf.sel.head;
        let (row, _) = self.cursor_rc();
        if self.diagnostics.iter().any(|d| d.row == row)
            && let Some(at) = self.anchor_below(pos)
        {
            self.hover_diag = Some((row, at, true));
            cx.notify();
        }
        if let Some(range) = self.name_at(pos) {
            self.find_hover(range, true, cx);
        }
    }

    /// The name at `pos` worth a card (not a keyword, string, comment…).
    fn name_at(&self, pos: usize) -> Option<Range<usize>> {
        if self.mode != EditorMode::Full {
            return None;
        }
        match crate::definition::hit_at(&self.buf, pos)? {
            crate::definition::Hit::Name(_, r) if !self.inert(&r) => Some(r),
            _ => None,
        }
    }

    /// Where a card for the text at `pos` hangs: right under it.
    fn anchor_below(&self, pos: usize) -> Option<Point<Pixels>> {
        let l = self.layout.as_ref()?;
        let (row, col) = self.buf.pos_to_rc(pos);
        let st = self.seg_starts(row);
        let k = seg_index(st, col);
        let x = l.text_left - self.scroll_x + (self.buf.display_col(row, col) - self.buf.display_col(row, st[k])) as f32 * l.char_w;
        let y = l.bounds.top() / px(1.) + l.pad_top + (self.disp_of(row) + k + 1) as f32 * l.line_h - self.scroll_y;
        Some(point(px(x), px(y)))
    }

    /// The name right under the mouse, if any.
    fn word_at_point(&self, p: Point<Pixels>) -> Option<Range<usize>> {
        let l = self.layout.as_ref()?;
        if !l.bounds.contains(&p) || p.x / px(1.) < l.text_left {
            return None;
        }
        let range = self.name_at(self.pos_for_point(p))?;
        let (a, b) = (self.anchor_below(range.start)?, self.anchor_below(range.end)?);
        let (x, y) = (p.x / px(1.), p.y / px(1.));
        let bottom = a.y / px(1.);
        (x >= a.x / px(1.) - 1. && x <= b.x / px(1.) + 1. && y <= bottom && y >= bottom - l.line_h).then_some(range)
    }

    /// The mouse over a name: its card opens once it rests there a moment.
    fn hover_word(&mut self, p: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        const DELAY: Duration = Duration::from_millis(350);
        let range = self.word_at_point(p);
        if self.hover_info.as_ref().is_some_and(|h| Some(&h.range) == range.as_ref()) {
            return;
        }
        if self.hover_info.as_ref().is_some_and(|h| !h.keyboard) {
            self.hover_info = None;
            cx.notify();
        }
        if self.hover_wait == range {
            return;
        }
        self.hover_wait = range.clone();
        let Some(range) = range else { return };
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DELAY).await;
            this.update_in(cx, |ed, window, cx| {
                if ed.hover_wait.as_ref() == Some(&range) && ed.word_at_point(window.mouse_position()).as_ref() == Some(&range) {
                    ed.find_hover(range, false, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Finds where the name at `range` is declared (maybe across the project, in the background)
    /// and opens its card.
    fn find_hover(&mut self, range: Range<usize>, keyboard: bool, cx: &mut Context<Self>) {
        use crate::definition::Found;
        match crate::definition::find(&self.buf, range.start, self.path.as_deref(), self.root.as_deref()) {
            Found::At(t) => self.open_hover(range, t, keyboard, cx),
            Found::Nothing => {}
            Found::Search(name) => match self.cached_definition(&name) {
                Some(Some(t)) => self.open_hover(range, t, keyboard, cx),
                Some(None) => {}
                None => self.search_definition(name, Then::Hover(range, keyboard), cx),
            },
        }
    }

    fn open_hover(&mut self, range: Range<usize>, t: crate::definition::Target, keyboard: bool, cx: &mut Context<Self>) {
        let here = t.path.as_os_str().is_empty() || self.path.as_ref() == Some(&t.path);
        // A whole module (`import * as x`, or an export we couldn't place) has no declaration to show.
        if !here && (t.row, t.col) == (0, 0) {
            return;
        }
        let text = if here {
            self.buf.text()
        } else {
            match std::fs::read_to_string(&t.path) {
                Ok(s) => s,
                Err(_) => return,
            }
        };
        let path = if here { self.path.clone().unwrap_or_default() } else { t.path };
        let Some(info) = crate::signature::info(&text, &path, t.row, t.col) else { return };
        let Some(at) = self.anchor_below(range.start) else { return };
        self.hover_info = Some(HoverCard { range, at, keyboard, info });
        cx.notify();
    }

    /// The mouse on a problem's underline or label opens its card; moving off it closes it.
    fn hover_problems(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let hit = self.diag_hits.iter().find(|(b, _)| b.contains(&p)).map(|(b, row)| (*row, point(b.left(), b.bottom())));
        match (hit, self.hover_diag) {
            (Some((row, _)), Some((r, _, _))) if r == row => {}
            (Some((row, at)), _) => {
                self.hover_diag = Some((row, at, false));
                cx.notify();
            }
            (None, Some((_, _, false))) => {
                self.hover_diag = None;
                cx.notify();
            }
            _ => {}
        }
    }

    /// One card for both: the name's declaration and docs, then the line's problems.
    fn hover_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let info = self.hover_info.as_ref();
        let info_row = info.map(|h| self.buf.pos_to_rc(h.range.start).0);
        let diag = self.hover_diag.filter(|(row, _, _)| info_row.is_none_or(|r| r == *row));
        let mut diags: Vec<&Diag> = diag.map(|(row, _, _)| self.diagnostics.iter().filter(|d| d.row == row).collect()).unwrap_or_default();
        if info.is_none() && diags.is_empty() {
            return None;
        }
        diags.sort_by_key(|d| d.severity);
        let at = info.map(|h| h.at).or(diag.map(|d| d.1))?;
        let problems = !diags.is_empty();
        Some(
            deferred(
                anchored().position(at).snap_to_window().child(
                    div()
                        .id("problem-card")
                        .debug_selector(move || if problems { "problem-card" } else { "hover-card" }.into())
                        .occlude()
                        .max_w(px(580.))
                        .max_h(px(400.))
                        .overflow_y_scroll()
                        .p(px(10.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .rounded(px(6.))
                        .shadow_lg()
                        .font_family(theme::UI_FONT)
                        .text_size(px(12.))
                        .text_color(theme::fg())
                        .on_hover(cx.listener(|ed, hovered: &bool, _, cx| {
                            if *hovered {
                                return;
                            }
                            let mouse_diag = ed.hover_diag.is_some_and(|(_, _, keyboard)| !keyboard);
                            let mouse_info = ed.hover_info.as_ref().is_some_and(|h| !h.keyboard);
                            if mouse_diag {
                                ed.hover_diag = None;
                            }
                            if mouse_info {
                                ed.hover_info = None;
                                ed.hover_wait = None;
                            }
                            if mouse_diag || mouse_info {
                                cx.notify();
                            }
                        }))
                        .children(info.map(|h| self.info_card(h, cx)))
                        .when(info.is_some() && problems, |d| d.child(div().h(px(1.)).flex_none().bg(theme::border_input())))
                        .children(diags.into_iter().map(problem_card)),
                ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// The declaration in colour, its docs, and where it is (click to go there).
    fn info_card(&self, h: &HoverCard, cx: &mut Context<Self>) -> AnyElement {
        let i = &h.info;
        let code_color = highlight::color(highlight_index("type"));
        let rich = move |s: &str| {
            let (text, code) = crate::signature::inline_code(s);
            let style = HighlightStyle { color: Some(code_color), ..Default::default() };
            StyledText::new(text).with_highlights(code.into_iter().map(|r| (r, style)))
        };
        let sig = div().font_family(theme::MONO_FONT).flex().flex_col().children(i.sig.lines().enumerate().map(|(n, line)| {
            let spans = i.syntax.as_ref().and_then(|s| s.get(n)).map(|v| v.as_slice()).unwrap_or(&[]);
            div().min_h(px(16.)).child(crate::diff_view::styled_line(line, spans, &[]))
        }));
        let d = &i.doc;
        let label = |s: &str| div().flex_none().text_color(theme::fg_dim()).child(s.to_string());
        let docs = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(d.text.lines().map(|p| div().child(rich(p))))
            .children(d.params.iter().map(|(name, desc)| {
                div()
                    .flex()
                    .gap(px(6.))
                    .child(label("@param"))
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_color(theme::accent()).child(name.clone()))
                    .when(!desc.is_empty(), |r| r.child(div().flex_1().min_w_0().child(rich(desc))))
            }))
            .children(d.returns.iter().map(|r| div().flex().gap(px(6.)).child(label("@returns")).child(div().flex_1().min_w_0().child(rich(r)))))
            .children(d.tags.iter().map(|(tag, text)| {
                div()
                    .flex()
                    .gap(px(6.))
                    .child(label(&format!("@{tag}")).when(tag == "deprecated", |l| l.text_color(theme::git_modified())))
                    .child(div().flex_1().min_w_0().flex().flex_col().children(text.lines().map(|l| div().child(rich(l)))))
            }));
        let target = crate::definition::Target { path: i.path.clone(), row: i.row, col: i.col };
        let shown = self.root.as_deref().and_then(|r| i.path.strip_prefix(r).ok()).unwrap_or(&i.path).to_string_lossy().to_string();
        let place = (!shown.is_empty()).then(|| {
            div()
                .id("hover-card-place")
                .debug_selector(|| "hover-card-place".into())
                .text_size(px(11.))
                .text_color(theme::fg_muted())
                .cursor_pointer()
                .hover(|s| s.text_color(theme::accent()))
                .child(format!("{shown}:{}", i.row + 1))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ed, _, _, cx| {
                        cx.stop_propagation();
                        ed.hover_info = None;
                        ed.hover_wait = None;
                        ed.land(target.clone(), cx);
                    }),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(sig)
            .when(!d.is_empty(), |c| c.child(div().h(px(1.)).flex_none().bg(theme::border_input())).child(docs))
            .children(place)
            .into_any_element()
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
    /// Segments of wrapped lines: the line, where to paint it, and the part to show.
    text_clipped: Vec<(Point<Pixels>, ShapedLine, Bounds<Pixels>)>,
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
                for (origin, line, clip) in &p.text_clipped {
                    window.with_content_mask(Some(ContentMask { bounds: *clip }), |window| {
                        line.paint(*origin, p.line_h, window, cx).ok();
                    });
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
            .cursor(if self.link.is_some() { CursorStyle::PointingHand } else { CursorStyle::IBeam })
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_action(cx.listener(Self::go_to_definition_action))
            .on_action(cx.listener(Self::show_hover))
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
            .on_action(cx.listener(Self::select_next_occurrence))
            .on_action(cx.listener(Self::select_all_occurrences))
            .on_action(cx.listener(Self::fold))
            .on_action(cx.listener(Self::unfold))
            .on_action(cx.listener(Self::fold_all))
            .on_action(cx.listener(Self::unfold_all))
            .on_action(cx.listener(Self::next_conflict))
            .on_action(cx.listener(Self::prev_conflict))
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
            .children(self.completion_popup(cx))
            .children(self.hover_popup(cx))
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

    #[test]
    fn wraps_after_spaces_and_indents_continuations() {
        let b = crate::buffer::Buffer::new("aaaa bbbb cccc\n    xxxxxxxx yyyyyyyy\nshort");
        assert_eq!(super::wrap_points(&b, 0, 10), (vec![0, 10], 0));
        assert_eq!(super::wrap_points(&b, 0, 7), (vec![0, 5, 10], 0));
        let (st, ind) = super::wrap_points(&b, 1, 14);
        assert_eq!(ind, 4, "continuation rows line up with the line's indent");
        assert_eq!(st, vec![0, 13]);
        assert_eq!(super::wrap_points(&b, 2, 10), (vec![0], 0));
        let b = crate::buffer::Buffer::new("abcdefghijklmnop");
        assert_eq!(super::wrap_points(&b, 0, 6).0, vec![0, 6, 12], "no space: cut mid-word");
    }
}
