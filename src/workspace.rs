//! The window: activity bar, sidebar, tabs, output panel and the command line.

use crate::commit_view::CommitView;
use crate::diff_edit::DiffEditView;
use crate::diff_view::DiffView;
use crate::keymap::{self, KeysPanel};
use crate::diagnostics::FileDiags;
use crate::editor::{Diag, Severity};
use crate::search::{SearchEvent, SearchPanel};
use crate::settings::Settings;
use crate::terminal::{TerminalEvent, TerminalView};
use std::cell::RefCell;
use std::collections::HashMap;
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::file_tree::FileTree;
use crate::git::{self, Rev};
use crate::git_panel::GitPanel;
use crate::repo::{Repo, RepoEvent};
use crate::request::{DiffSpec, Request};
use crate::{theme, tools, ui, util};
use futures::StreamExt;
use gpui::prelude::*;
use gpui::*;
use std::cell::Cell;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

actions!(
    workspace,
    [
        FindFile, CommandPalette, FindInFile, FindNext, FindPrev, ShowFiles, ShowGit, ToggleSidebar,
        ToggleOutput, FocusEditor, FocusSidebar, Save, CloseTab, NextTab, PrevTab, FormatFile,
        ToggleBlame, ZoomIn, ZoomOut, Quit, GoToLine, RunShell, FocusCommit, ShowKeys, Lint,
        OutputDown, OutputUp, OutputPageDown, OutputPageUp, OutputTop, OutputBottom, OutputOpen, OutputLeave,
        OpenFolder, OpenFolderNewWindow, NextWindow, SearchAll, ShowKeysPanel, ToggleSidebarSide,
        SplitRight, SplitDown, ClosePane, ZoomPane, FocusLeft, FocusRight, FocusUp, FocusDown,
        NewTerminal, ToggleTerminal, PaneBuffers,
        GoBack, GoForward,
        MoveItemLeft, MoveItemRight, MoveItemUp, MoveItemDown,
        Buffer1, Buffer2, Buffer3, Buffer4, Buffer5, Buffer6, Buffer7, Buffer8, Buffer9,
    ]
);

pub fn bind_keys() {
    crate::keybindings::collect([
        crate::keybindings::kb("cmd-p", FindFile, None),
        crate::keybindings::kb("cmd-shift-p", CommandPalette, None),
        crate::keybindings::kb("cmd-;", CommandPalette, None),
        crate::keybindings::kb("cmd-f", FindInFile, None),
        crate::keybindings::kb("cmd-g", FindNext, None),
        crate::keybindings::kb("cmd-shift-g", FindPrev, None),
        crate::keybindings::kb("cmd-shift-e", ShowFiles, None),
        crate::keybindings::kb("ctrl-shift-g", ShowGit, None),
        crate::keybindings::kb("cmd-b", ToggleSidebar, None),
        crate::keybindings::kb("cmd-j", ToggleOutput, None),
        crate::keybindings::kb("ctrl-`", ToggleTerminal, None),
        crate::keybindings::kb("cmd-1", Buffer1, None),
        crate::keybindings::kb("cmd-2", Buffer2, None),
        crate::keybindings::kb("cmd-3", Buffer3, None),
        crate::keybindings::kb("cmd-4", Buffer4, None),
        crate::keybindings::kb("cmd-5", Buffer5, None),
        crate::keybindings::kb("cmd-6", Buffer6, None),
        crate::keybindings::kb("cmd-7", Buffer7, None),
        crate::keybindings::kb("cmd-8", Buffer8, None),
        crate::keybindings::kb("cmd-9", Buffer9, None),
        crate::keybindings::kb("cmd-alt-enter", ZoomPane, None),
        crate::keybindings::kb("cmd-0", FocusSidebar, None),
        // Directions: arrows, or j ← · k ↓ · l ↑ · ; → (home row).
        crate::keybindings::kb("cmd-alt-{left}", FocusLeft, None),
        crate::keybindings::kb("cmd-alt-{down}", FocusDown, None),
        crate::keybindings::kb("cmd-alt-{up}", FocusUp, None),
        crate::keybindings::kb("cmd-alt-{right}", FocusRight, None),
        crate::keybindings::kb("cmd-alt-shift-left", MoveItemLeft, None),
        crate::keybindings::kb("cmd-alt-shift-right", MoveItemRight, None),
        crate::keybindings::kb("cmd-alt-shift-up", MoveItemUp, None),
        crate::keybindings::kb("cmd-alt-shift-down", MoveItemDown, None),
        crate::keybindings::kb("cmd-alt-shift-{left}", MoveItemLeft, None),
        crate::keybindings::kb("cmd-alt-shift-{down}", MoveItemDown, None),
        crate::keybindings::kb("cmd-alt-shift-{up}", MoveItemUp, None),
        crate::keybindings::kb("cmd-alt-shift-{right}", MoveItemRight, None),
        crate::keybindings::kb("ctrl-cmd-left", PrevTab, None),
        crate::keybindings::kb("ctrl-cmd-right", NextTab, None),
        crate::keybindings::kb("ctrl-cmd-{left}", PrevTab, None),
        crate::keybindings::kb("ctrl-cmd-{right}", NextTab, None),
        crate::keybindings::kb("cmd-alt-up", FocusUp, None),
        crate::keybindings::kb("cmd-alt-down", FocusDown, None),
        crate::keybindings::kb("cmd-d", SplitRight, None),
        crate::keybindings::kb("cmd-shift-d", SplitDown, None),
        crate::keybindings::kb("cmd-shift-w", ClosePane, None),
        crate::keybindings::kb("cmd-t", NewTerminal, None),
        crate::keybindings::kb("cmd-e", PaneBuffers, None),
        crate::keybindings::kb("cmd-s", Save, None),
        crate::keybindings::kb("cmd-w", CloseTab, None),
        crate::keybindings::kb("ctrl-tab", NextTab, None),
        crate::keybindings::kb("ctrl-shift-tab", PrevTab, None),
        crate::keybindings::kb("cmd-shift-]", NextTab, None),
        crate::keybindings::kb("cmd-shift-[", PrevTab, None),
        crate::keybindings::kb("cmd-alt-right", FocusRight, None),
        crate::keybindings::kb("cmd-alt-left", FocusLeft, None),
        crate::keybindings::kb("shift-alt-f", FormatFile, None),
        crate::keybindings::kb("cmd-alt-b", ToggleBlame, None),
        crate::keybindings::kb("cmd-=", ZoomIn, None),
        crate::keybindings::kb("cmd-+", ZoomIn, None),
        crate::keybindings::kb("cmd--", ZoomOut, None),
        crate::keybindings::kb("cmd-q", Quit, None),
        crate::keybindings::kb("ctrl-g", GoToLine, None),
        crate::keybindings::kb("ctrl-shift-1", RunShell, None),
        crate::keybindings::kb("cmd-k cmd-s", ShowKeysPanel, None),
        crate::keybindings::kb("cmd-o", OpenFolder, None),
        crate::keybindings::kb("cmd-shift-n", OpenFolderNewWindow, None),
        crate::keybindings::kb("ctrl--", GoBack, None),
        crate::keybindings::kb("ctrl-shift--", GoForward, None),
        crate::keybindings::kb("cmd-`", NextWindow, None),
        crate::keybindings::kb("cmd-shift-f", SearchAll, None),
        crate::keybindings::kb("cmd-k cmd-b", ToggleSidebarSide, None),
    ]);
    let o = Some("Output");
    crate::keybindings::collect([
        crate::keybindings::kb("j", OutputDown, o),
        crate::keybindings::kb("down", OutputDown, o),
        crate::keybindings::kb("k", OutputUp, o),
        crate::keybindings::kb("up", OutputUp, o),
        crate::keybindings::kb("space", OutputPageDown, o),
        crate::keybindings::kb("pagedown", OutputPageDown, o),
        crate::keybindings::kb("pageup", OutputPageUp, o),
        crate::keybindings::kb("g", OutputTop, o),
        crate::keybindings::kb("home", OutputTop, o),
        crate::keybindings::kb("shift-g", OutputBottom, o),
        crate::keybindings::kb("end", OutputBottom, o),
        crate::keybindings::kb("enter", OutputOpen, o),
        crate::keybindings::kb("escape", OutputLeave, o),
    ]);
    // In the non-text panes, ":" opens the command line and "/" finds files, like a terminal UI.
    for ctx in ["FileTree", "GitPanel", "DiffView", "CommitView", "Output"] {
        crate::keybindings::collect([
            crate::keybindings::kb("shift-;", CommandPalette, Some(ctx)),
            crate::keybindings::kb(":", CommandPalette, Some(ctx)),
            crate::keybindings::kb("/", FindFile, Some(ctx)),
            crate::keybindings::kb("!", RunShell, Some(ctx)),
            crate::keybindings::kb("shift-1", RunShell, Some(ctx)),
        ]);
    }
}

const ACTIVITY_W: f32 = 40.;
const STATUS_H: f32 = 22.;
const TITLE_H: f32 = 30.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Files,
    Search,
    Git,
    Keys,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CmdMode {
    Command,
    Search,
    Files,
    Buffers,
}

struct CmdItem {
    label: String,
    detail: String,
    value: String,
    positions: Vec<usize>,
    exec: bool,
}

enum TabKind {
    Editor(Entity<Editor>),
    Diff(Entity<DiffView>),
    DiffEdit(Entity<DiffEditView>),
    Commit(Entity<CommitView>),
    Terminal(Entity<TerminalView>),
}

/// Tiling layout of panes, like zellij / tmux / iTerm splits.
#[derive(Clone, Debug)]
enum Layout {
    Pane(usize),
    Split { vertical: bool, ratio: f32, a: Box<Layout>, b: Box<Layout> },
}

impl Layout {
    fn panes(&self, out: &mut Vec<usize>) {
        match self {
            Layout::Pane(id) => out.push(*id),
            Layout::Split { a, b, .. } => {
                a.panes(out);
                b.panes(out);
            }
        }
    }

    fn replace_pane(&mut self, id: usize, new: Layout) -> bool {
        match self {
            Layout::Pane(p) if *p == id => {
                *self = new;
                true
            }
            Layout::Pane(_) => false,
            Layout::Split { a, b, .. } => a.replace_pane(id, new.clone()) || b.replace_pane(id, new),
        }
    }

    /// Removes a pane; its sibling takes the parent's place.
    fn remove_pane(&mut self, id: usize) -> bool {
        let Layout::Split { a, b, .. } = self else { return false };
        if matches!(**a, Layout::Pane(p) if p == id) {
            *self = (**b).clone();
            return true;
        }
        if matches!(**b, Layout::Pane(p) if p == id) {
            *self = (**a).clone();
            return true;
        }
        a.remove_pane(id) || b.remove_pane(id)
    }

    /// How many times the area holding `id` has been halved (0 = whole area).
    fn depth(&self, id: usize) -> Option<usize> {
        match self {
            Layout::Pane(p) => (*p == id).then_some(0),
            Layout::Split { a, b, .. } => a.depth(id).or_else(|| b.depth(id)).map(|d| d + 1),
        }
    }

    fn max_depth(&self) -> usize {
        match self {
            Layout::Pane(_) => 0,
            Layout::Split { a, b, .. } => 1 + a.max_depth().max(b.max_depth()),
        }
    }

    fn at_path(&mut self, path: &str) -> Option<&mut Layout> {
        let mut node = self;
        for step in path.chars() {
            node = match node {
                Layout::Split { a, b, .. } => if step == 'a' { a } else { b },
                Layout::Pane(_) => return None,
            };
        }
        Some(node)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DropZone {
    /// Which part of a pane the cursor is over: the outer quarter on each side splits.
    fn at(bounds: Bounds<Pixels>, p: Point<Pixels>) -> Self {
        let x = (p.x - bounds.left()) / bounds.size.width;
        let y = (p.y - bounds.top()) / bounds.size.height;
        let edge = 0.25;
        let candidates = [(x, DropZone::Left), (1. - x, DropZone::Right), (y, DropZone::Top), (1. - y, DropZone::Bottom)];
        candidates
            .into_iter()
            .filter(|(d, _)| *d < edge)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, z)| z)
            .unwrap_or(DropZone::Center)
    }

    fn from_dir(dx: i32, dy: i32) -> Self {
        match (dx.signum(), dy.signum()) {
            (-1, _) => DropZone::Left,
            (1, _) => DropZone::Right,
            (_, -1) => DropZone::Top,
            _ => DropZone::Bottom,
        }
    }
}

/// Splits are limited by size, not count: an area can be halved until it is 1/8 of the
/// window (half → quarter → eighth), which also caps the layout at 8 splits.
const MAX_SPLIT_DEPTH: usize = 3;

struct Pane {
    id: usize,
    /// Tab ids open in this pane (its "buffers").
    items: Vec<usize>,
    active: Option<usize>,
    /// What was shown before / after (⌃- and ⌃⇧-), newest last.
    back: Vec<usize>,
    fwd: Vec<usize>,
}

struct Tab {
    id: usize,
    kind: TabKind,
    preview: bool,
    _sub: Subscription,
}

struct OutputLine {
    text: SharedString,
    loc: Option<(String, usize, usize)>,
    err: bool,
}

const COMMANDS: &[(&str, &str, bool)] = &[
    ("w", "Save the current file", false),
    ("q", "Close the current tab (q! discards changes)", false),
    ("wq", "Save and close", false),
    ("qa", "Quit OSKL Code (qa! discards changes)", false),
    ("e", "Open a file: e <path>[:line]", true),
    ("new", "Create a file: new <path>", true),
    ("mkdir", "Create a folder: mkdir <path>", true),
    ("mv", "Rename/move: mv <from> <to>", true),
    ("cd", "Open a folder in a new window: cd <dir>", true),
    ("format", "Format the current file (⇧⌥F)", false),
    ("lint", "Lint the current file; lint . for the whole project", false),
    ("blame", "Toggle the blame gutter (⌥⌘B)", false),
    ("diff", "Diff the current file against the index", false),
    ("stage", "Stage a file (current file by default)", false),
    ("unstage", "Unstage a file (current file by default)", false),
    ("discard", "Discard working-tree changes to files", true),
    ("commit", "Commit: commit [-a] [--amend] <message>", true),
    ("checkout", "Switch branch: checkout <branch>", true),
    ("branch", "Create and switch to a branch: branch <name>", true),
    ("fetch", "git fetch --all --prune", false),
    ("pull", "git pull", false),
    ("push", "git push", false),
    ("stash", "git stash (stash pop to restore)", false),
    ("graph", "Graph scope: graph all | graph auto", true),
    ("open", "Open a folder in this window: open <dir> (⌘O for a picker)", true),
    ("window", "Open a project in a new window: window [dir] (⌘⇧N)", true),
    ("recent", "Open a recent folder", true),
    ("search", "Search in all files: search <text> (⇧⌘F)", true),
    ("sidebar", "Sidebar side: sidebar left | right", true),
    ("autosave", "Auto save: autosave on | off (default on)", true),
    ("formatonsave", "Format on ⌘S: formatonsave on | off (default on)", true),
    ("problems", "List lint and TypeScript problems (click to jump)", false),
    ("check", "Lint/type-check on save: check on | off | now", true),
    ("vsplit", "Split right (⌘D), optionally opening a file: vsplit <path>", true),
    ("split", "Split down (⌘⇧D), optionally opening a file: split <path>", true),
    ("term", "Terminal in this pane (⌘T); term <cmd> runs a command", true),
    ("close", "Close this pane (⌘⇧W)", false),
    ("only", "Close all other panes", false),
    ("max", "Maximize / restore this pane (⌘⌥Enter)", false),
    ("buffers", "Files open in this pane (⌘E)", false),
    ("git", "Run any git command: git <args>", true),
    ("!", "Run a shell command: !<command>", true),
    ("files", "Show the explorer (⇧⌘E)", false),
    ("scm", "Show source control (⌃⇧G)", false),
    ("output", "Toggle the output panel (⌘J)", false),
    ("zoom", "zoom in | out | reset", true),
    ("keys", "Show key bindings", false),
];

pub struct Workspace {
    focus: FocusHandle,
    repo: Entity<Repo>,
    tree: Entity<FileTree>,
    git_panel: Entity<GitPanel>,
    search: Entity<SearchPanel>,
    keys: Entity<KeysPanel>,
    settings: Settings,
    tabs: Vec<Tab>,
    active: Option<usize>,
    side: Side,
    side_visible: bool,
    side_w: f32,
    resizing_side: bool,
    output: Vec<OutputLine>,
    output_title: String,
    output_visible: bool,
    output_h: f32,
    resizing_output: bool,
    output_scroll: UniformListScrollHandle,
    output_focus: FocusHandle,
    output_sel: usize,
    cmd: Entity<Editor>,
    cmd_mode: Option<CmdMode>,
    cmd_items: Vec<CmdItem>,
    cmd_sel: usize,
    cmd_nav: bool,
    branches: Vec<String>,
    history: Vec<String>,
    hist_ix: Option<usize>,
    return_focus: Option<FocusHandle>,
    search_origin: Option<usize>,
    last_search: Option<String>,
    message: Option<(String, bool)>,
    message_task: Option<Task<()>>,
    recent: Vec<PathBuf>,
    font_size: f32,
    blame_default: bool,
    window_bounds: Rc<Cell<Bounds<Pixels>>>,
    tool_task: Option<Task<()>>,
    format_save_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
    _subs: Vec<Subscription>,
    diff_rights: Vec<(Entity<DiffEditView>, Entity<Editor>)>,
    autosave_tasks: std::collections::HashMap<EntityId, Task<()>>,
    lint_diags: FileDiags,
    ts_diags: FileDiags,
    data_diags: FileDiags,
    data_tasks: HashMap<EntityId, Task<()>>,
    lint_tasks: HashMap<PathBuf, Task<()>>,
    tsc_task: Option<Task<()>>,
    tsc_running: bool,
    tsc_again: Option<PathBuf>,
    /// False when the app was opened without a folder (start screen with Open Folder / Recent).
    has_project: bool,
    panes: Vec<Pane>,
    layout: Layout,
    focused_pane: usize,
    prev_pane: usize,
    next_id: usize,
    zoomed: bool,
    pane_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    split_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    dragging_split: Option<String>,
    navigating: bool,
    drop_hint: Option<(usize, DropZone)>,
    extra_subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let repo = cx.new(|cx| Repo::new(root, cx));
        let tree = cx.new(|cx| FileTree::new(repo.clone(), cx));
        let git_panel = cx.new(|cx| GitPanel::new(repo.clone(), window, cx));
        let cmd = cx.new(|cx| Editor::new(EditorMode::SingleLine, cx));
        let search = cx.new(|cx| SearchPanel::new(repo.clone(), window, cx));
        let keys = cx.new(|cx| KeysPanel::new(window, cx));
        let has_project = root_is_project(&repo.read(cx).root);
        let mut settings = Settings::load();
        if has_project {
            settings.add_recent(repo.read(cx).root.clone());
            settings.save();
        }
        let subs = vec![
            cx.subscribe_in(&search, window, |ws, _, req: &Request, window, cx| ws.handle_request(req, window, cx)),
            cx.subscribe_in(&search, window, |ws, _, ev: &SearchEvent, window, cx| match ev {
                SearchEvent::Replace(plan) => ws.apply_replace(plan, window, cx),
            }),
            cx.subscribe_in(&tree, window, |ws, _, req: &Request, window, cx| ws.handle_request(req, window, cx)),
            cx.subscribe_in(&git_panel, window, |ws, _, req: &Request, window, cx| ws.handle_request(req, window, cx)),
            cx.subscribe_in(&repo, window, |ws, _, ev: &RepoEvent, _, cx| match ev {
                RepoEvent::Message(m) => ws.flash(m.clone(), false, cx),
                RepoEvent::Error(e) => ws.flash(e.clone(), true, cx),
                RepoEvent::Output { title, text, ok } => {
                    ws.show_output(title.clone(), text, !*ok, cx);
                    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(if *ok { "done" } else { "failed" });
                    ws.flash(format!("{title}: {last}"), !*ok, cx);
                }
            }),
            cx.subscribe_in(&cmd, window, |ws, _, ev: &EditorEvent, window, cx| ws.on_cmd_event(ev, window, cx)),
            cx.observe(&repo, |ws, repo, cx| {
                for tab in &ws.tabs {
                    if let TabKind::Editor(e) = &tab.kind {
                        let email = repo.read(cx).user_email.clone();
                        e.update(cx, |e, _| e.user_email = email);
                    }
                }
                cx.notify();
            }),
        ];
        let project = repo.read(cx).root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        window.set_window_title(&if root_is_project(&repo.read(cx).root) { format!("{project} — OSKL Code") } else { "OSKL Code".into() });
        let mut ws = Self {
            focus: cx.focus_handle(),
            repo,
            tree,
            git_panel,
            search,
            keys,
            tabs: Vec::new(),
            active: None,
            side: Side::Files,
            side_visible: has_project,
            side_w: 300.,
            resizing_side: false,
            output: Vec::new(),
            output_title: String::new(),
            output_visible: false,
            output_h: 220.,
            resizing_output: false,
            output_scroll: UniformListScrollHandle::new(),
            output_focus: cx.focus_handle(),
            output_sel: 0,
            cmd,
            cmd_mode: None,
            cmd_items: Vec::new(),
            cmd_sel: 0,
            cmd_nav: false,
            branches: Vec::new(),
            history: Vec::new(),
            hist_ix: None,
            return_focus: None,
            search_origin: None,
            last_search: None,
            message: None,
            message_task: None,
            recent: Vec::new(),
            font_size: settings.font_size,
            blame_default: settings.blame_default,
            settings,
            window_bounds: Rc::new(Cell::new(Bounds::default())),
            tool_task: None,
            format_save_task: None,
            watch_task: None,
            _watcher: None,
            _subs: subs,
            diff_rights: Vec::new(),
            autosave_tasks: Default::default(),
            lint_diags: Default::default(),
            ts_diags: Default::default(),
            data_diags: Default::default(),
            data_tasks: Default::default(),
            lint_tasks: Default::default(),
            tsc_task: None,
            tsc_running: false,
            tsc_again: None,
            has_project,
            panes: vec![Pane { id: 0, items: Vec::new(), active: None, back: Vec::new(), fwd: Vec::new() }],
            layout: Layout::Pane(0),
            focused_pane: 0,
            prev_pane: 0,
            next_id: 1,
            zoomed: false,
            pane_bounds: Default::default(),
            split_bounds: Default::default(),
            dragging_split: None,
            navigating: false,
            drop_hint: None,
            extra_subs: Vec::new(),
        };
        ws.start_watcher(cx);
        ws.tree.read(cx).focus(window);
        ws
    }

    fn root(&self, cx: &App) -> PathBuf {
        self.repo.read(cx).root.clone()
    }

    fn rel(&self, path: &Path, cx: &App) -> String {
        path.strip_prefix(self.root(cx)).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string_lossy().to_string())
    }

    fn flash(&mut self, msg: String, err: bool, cx: &mut Context<Self>) {
        self.message = Some((msg, err));
        self.message_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(if err { 8 } else { 4 })).await;
            this.update(cx, |ws, cx| {
                ws.message = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn show_output(&mut self, title: String, text: &str, err: bool, cx: &mut Context<Self>) {
        self.output_title = title;
        self.output = text
            .lines()
            .take(20_000)
            .map(|l| OutputLine { text: l.to_string().into(), loc: util::parse_location(l), err: false })
            .collect();
        // ESLint's default output lists the file once, then "line:col" rows under it.
        let mut current_file: Option<String> = None;
        for line in self.output.iter_mut() {
            let t = line.text.to_string();
            if !t.starts_with(' ') && (t.starts_with('/') || t.contains('.')) && !t.contains(' ') {
                current_file = Some(t.trim().to_string());
            } else if line.loc.is_none()
                && let Some(file) = &current_file
            {
                let mut parts = t.split_whitespace().next().unwrap_or("").split(':');
                if let (Some(Ok(r)), Some(Ok(c))) = (parts.next().map(str::parse::<usize>), parts.next().map(str::parse::<usize>)) {
                    line.loc = Some((file.clone(), r, c));
                }
            }
            line.err = err && line.loc.is_none() && !t.trim().is_empty();
        }
        if self.output.is_empty() {
            self.output.push(OutputLine { text: "(no output)".into(), loc: None, err: false });
        }
        self.output_visible = true;
        self.output_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.output_sel = 0;
        cx.notify();
    }

    // ------------------------------------------------------------ requests

    fn handle_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) {
        match req {
            Request::OpenFile { path, line, preview } => self.open_file(path.clone(), *line, *preview, window, cx),
            Request::OpenAt { path, line, cols } => {
                self.open_file(path.clone(), None, true, window, cx);
                if let Some(ed) = self.active_editor() {
                    let (line, cols) = (*line, cols.clone());
                    ed.update(cx, |ed, cx| {
                        let start = ed.buf.rc_to_pos(line, cols.start);
                        let end = ed.buf.rc_to_pos(line, cols.end);
                        ed.select_range(start..end, cx);
                    });
                }
            }
            Request::OpenDiff { spec, preview } => self.open_diff(spec.clone(), *preview, window, cx),
            Request::OpenCommit(sha) => self.open_commit(sha.clone(), window, cx),
            Request::CommandLine(text) => self.open_cmd(CmdMode::Command, text, window, cx),
            Request::Message(m) => self.flash(m.clone(), false, cx),
            Request::Error(e) => self.flash(e.clone(), true, cx),
        }
    }

    // ------------------------------------------------------------ tabs

    fn pane(&self, id: usize) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    fn pane_mut(&mut self, id: usize) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn tab_ix(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn pane_of_tab(&self, tab_id: usize) -> Option<usize> {
        self.panes.iter().find(|p| p.items.contains(&tab_id)).map(|p| p.id)
    }

    /// `active` mirrors the focused pane's visible item (index into `tabs`).
    fn sync_active(&mut self) {
        self.active = self.pane(self.focused_pane).and_then(|p| p.active).and_then(|id| self.tab_ix(id));
    }

    fn set_focused_pane(&mut self, id: usize) {
        if self.focused_pane != id {
            self.prev_pane = self.focused_pane;
            self.focused_pane = id;
        }
        self.sync_active();
    }

    fn add_tab(&mut self, kind: TabKind, sub: Subscription, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id += 1;
        if self.pane(self.focused_pane).is_none() {
            self.focused_pane = self.panes[0].id;
        }
        // A preview item in this pane is replaced instead of piling up.
        let old_preview = if preview {
            self.pane(self.focused_pane)
                .and_then(|p| p.items.iter().copied().find(|t| self.tab_ix(*t).is_some_and(|ix| self.tabs[ix].preview)))
        } else {
            None
        };
        self.tabs.push(Tab { id, kind, preview, _sub: sub });
        let fp = self.focused_pane;
        if let Some(pane) = self.pane_mut(fp) {
            let pos = pane.active.and_then(|a| pane.items.iter().position(|x| *x == a)).map(|p| p + 1).unwrap_or(pane.items.len());
            pane.items.insert(pos, id);
            if let Some(prev) = pane.active {
                pane.back.push(prev);
                pane.fwd.clear();
            }
            pane.active = Some(id);
        }
        if let Some(old) = old_preview {
            self.drop_tab(old);
        }
        self.sync_active();
        if let Some(ix) = self.active {
            self.activate(ix, window, cx);
        }
    }

    /// Removes a tab everywhere (no dirty check).
    fn drop_tab(&mut self, id: usize) {
        for pane in &mut self.panes {
            pane.back.retain(|t| *t != id);
            pane.fwd.retain(|t| *t != id);
            if let Some(pos) = pane.items.iter().position(|x| *x == id) {
                pane.items.remove(pos);
                if pane.active == Some(id) {
                    pane.active = pane.items.get(pos.min(pane.items.len().saturating_sub(1))).copied();
                }
            }
        }
        if let Some(ix) = self.tab_ix(id) {
            let removed = self.tabs.remove(ix);
            if let TabKind::DiffEdit(d) = &removed.kind {
                self.diff_rights.retain(|(v, _)| v != d);
            }
        }
        self.sync_active();
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.get(ix).map(|t| t.id) else { return };
        match self.pane_of_tab(id) {
            Some(p) => {
                self.set_focused_pane(p);
                let navigating = self.navigating;
                if let Some(pane) = self.pane_mut(p) {
                    if let Some(prev) = pane.active.filter(|prev| *prev != id && !navigating) {
                        pane.back.push(prev);
                        pane.fwd.clear();
                    }
                    pane.active = Some(id);
                }
            }
            None => {
                let fp = self.focused_pane;
                if let Some(pane) = self.pane_mut(fp) {
                    pane.items.push(id);
                    pane.active = Some(id);
                }
            }
        }
        self.sync_active();
        self.focus_active(window, cx);
        if let TabKind::Editor(e) = &self.tabs[ix].kind
            && let Some(path) = e.read(cx).path.clone()
        {
            self.tree.update(cx, |t, cx| t.reveal(&path, cx));
        }
        cx.notify();
    }

    // ------------------------------------------------------------ panes

    fn can_split(&self, pane: usize) -> bool {
        self.layout.depth(pane).is_some_and(|d| d < MAX_SPLIT_DEPTH)
    }

    fn split_limit_message(&mut self, cx: &mut Context<Self>) {
        self.flash("This split is already at the smallest size (1/8 of the window) — split a bigger one, or close/merge splits".into(), true, cx);
    }

    fn split(&mut self, vertical: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_split(self.focused_pane) {
            return self.split_limit_message(cx);
        }
        let new = self.next_id;
        self.next_id += 1;
        self.panes.push(Pane { id: new, items: Vec::new(), active: None, back: Vec::new(), fwd: Vec::new() });
        let fp = self.focused_pane;
        let node = Layout::Split { vertical, ratio: 0.5, a: Box::new(Layout::Pane(fp)), b: Box::new(Layout::Pane(new)) };
        if !self.layout.replace_pane(fp, node.clone()) {
            self.layout = node;
        }
        self.zoomed = false;
        self.set_focused_pane(new);
        window.focus(&self.focus);
        // A new pane asks what to show, like a fresh terminal split.
        self.open_cmd(CmdMode::Files, "", window, cx);
        self.flash("New pane — pick a file (Esc for an empty pane, ⌘T for a terminal)".into(), false, cx);
        cx.notify();
    }

    fn close_pane(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(items) = self.pane(id).map(|p| p.items.clone()) else { return };
        let dirty = items.iter().any(|t| self.tab_ix(*t).is_some_and(|ix| self.tab_dirty(ix, cx)));
        if dirty {
            return self.flash("This pane has unsaved files — save them first (:w) or close them with :q!".into(), true, cx);
        }
        for t in items {
            self.drop_tab(t);
        }
        if self.panes.len() > 1 {
            self.layout.remove_pane(id);
            self.panes.retain(|p| p.id != id);
            let mut ids = Vec::new();
            self.layout.panes(&mut ids);
            let next = if ids.contains(&self.prev_pane) { self.prev_pane } else { ids[0] };
            self.focused_pane = next;
        }
        self.zoomed = false;
        self.sync_active();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn tab_dirty(&self, ix: usize, cx: &App) -> bool {
        match &self.tabs[ix].kind {
            TabKind::Editor(e) => e.read(cx).buf.is_dirty(),
            TabKind::DiffEdit(d) => d.read(cx).right.read(cx).buf.is_dirty(),
            _ => false,
        }
    }

    /// The nearest pane in a direction from the focused one.
    fn neighbor(&self, dx: i32, dy: i32) -> Option<usize> {
        let bounds = self.pane_bounds.borrow().clone();
        let cur = bounds.get(&self.focused_pane)?;
        let c = cur.center();
        let mut best: Option<(f32, usize)> = None;
        for (id, b) in &bounds {
            if *id == self.focused_pane || self.pane(*id).is_none() {
                continue;
            }
            let o = b.center();
            let (ddx, ddy) = ((o.x - c.x) / px(1.), (o.y - c.y) / px(1.));
            let ok = (dx > 0 && ddx > 1.) || (dx < 0 && ddx < -1.) || (dy > 0 && ddy > 1.) || (dy < 0 && ddy < -1.);
            if !ok {
                continue;
            }
            let along = if dx != 0 { ddx.abs() } else { ddy.abs() };
            let across = if dx != 0 { ddy.abs() } else { ddx.abs() };
            let score = along + across * 2.;
            if best.is_none_or(|(s, _)| score < s) {
                best = Some((score, *id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Moves focus to the nearest pane in a direction (wraps around at the edges).
    fn focus_dir(&mut self, dx: i32, dy: i32, window: &mut Window, cx: &mut Context<Self>) {
        let best = self.neighbor(dx, dy).map(|id| ((), id));
        match best {
            Some(((), id)) => {
                self.set_focused_pane(id);
                self.focus_active(window, cx);
            }
            None => {
                // Nothing that way: cycle through panes in layout order (wraps around).
                let mut ids = Vec::new();
                self.layout.panes(&mut ids);
                if ids.len() > 1 {
                    let cur = ids.iter().position(|p| *p == self.focused_pane).unwrap_or(0);
                    let forward = dx > 0 || dy > 0;
                    let next = if forward { (cur + 1) % ids.len() } else { (cur + ids.len() - 1) % ids.len() };
                    self.set_focused_pane(ids[next]);
                    self.focus_active(window, cx);
                }
            }
        }
        cx.notify();
    }

    /// ⌘T: a terminal in the focused pane.
    fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_terminal_view(None, window, cx);
    }

    /// ⌘⌥⇧ + arrow: move the shown item to the pane that way, or split off a new pane there.
    fn move_item_dir(&mut self, dx: i32, dy: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tid) = self.pane(self.focused_pane).and_then(|p| p.active) else {
            return self.flash("Nothing to move in this pane".into(), true, cx);
        };
        match self.neighbor(dx, dy) {
            Some(target) => self.move_tab(tid, target, DropZone::Center, window, cx),
            None => self.move_tab(tid, self.focused_pane, DropZone::from_dir(dx, dy), window, cx),
        }
    }

    /// Moves an item into `target` (Center) or into a new split on one side of it.
    fn move_tab(&mut self, tid: usize, target: usize, zone: DropZone, window: &mut Window, cx: &mut Context<Self>) {
        let Some(src) = self.pane_of_tab(tid) else { return };
        if self.pane(target).is_none() {
            return;
        }
        let src_len = self.pane(src).map(|p| p.items.len()).unwrap_or(0);
        let dest = if zone == DropZone::Center {
            if src == target {
                return;
            }
            target
        } else {
            if !self.can_split(target) {
                return self.split_limit_message(cx);
            }
            let _ = src_len;
            let new = self.next_id;
            self.next_id += 1;
            self.panes.push(Pane { id: new, items: Vec::new(), active: None, back: Vec::new(), fwd: Vec::new() });
            let vertical = matches!(zone, DropZone::Left | DropZone::Right);
            let before = matches!(zone, DropZone::Left | DropZone::Top);
            let (a, b) = if before { (Layout::Pane(new), Layout::Pane(target)) } else { (Layout::Pane(target), Layout::Pane(new)) };
            self.layout.replace_pane(target, Layout::Split { vertical, ratio: 0.5, a: Box::new(a), b: Box::new(b) });
            new
        };
        if let Some(p) = self.pane_mut(src)
            && let Some(pos) = p.items.iter().position(|x| *x == tid)
        {
            p.items.remove(pos);
            if p.active == Some(tid) {
                p.active = p.items.get(pos.min(p.items.len().saturating_sub(1))).copied();
            }
        }
        if let Some(p) = self.pane_mut(dest) {
            p.items.push(tid);
            p.active = Some(tid);
        }
        // The split it came from stays, even when empty (close it with ⌘⇧W).
        self.zoomed = false;
        self.drop_hint = None;
        self.set_focused_pane(dest);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// A file dropped from the explorer onto a pane.
    fn drop_file(&mut self, path: PathBuf, pane: usize, zone: DropZone, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_hint = None;
        let existing = self.tabs.iter().find(|t| matches!(&t.kind, TabKind::Editor(e) if e.read(cx).path.as_ref() == Some(&path))).map(|t| t.id);
        if let Some(tid) = existing {
            // Already open somewhere: move it there instead of opening it twice.
            if self.pane_of_tab(tid) == Some(pane) && zone == DropZone::Center {
                if let Some(ix) = self.tab_ix(tid) {
                    self.activate(ix, window, cx);
                }
                return;
            }
            return self.move_tab(tid, pane, zone, window, cx);
        }
        let dest = if zone == DropZone::Center {
            pane
        } else {
            if !self.can_split(pane) {
                return self.split_limit_message(cx);
            }
            let new = self.next_id;
            self.next_id += 1;
            self.panes.push(Pane { id: new, items: Vec::new(), active: None, back: Vec::new(), fwd: Vec::new() });
            let vertical = matches!(zone, DropZone::Left | DropZone::Right);
            let before = matches!(zone, DropZone::Left | DropZone::Top);
            let (a, b) = if before { (Layout::Pane(new), Layout::Pane(pane)) } else { (Layout::Pane(pane), Layout::Pane(new)) };
            self.layout.replace_pane(pane, Layout::Split { vertical, ratio: 0.5, a: Box::new(a), b: Box::new(b) });
            new
        };
        self.zoomed = false;
        self.set_focused_pane(dest);
        self.open_file(path, None, false, window, cx);
    }

    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused_is_term = self.active.is_some_and(|a| matches!(self.tabs[a].kind, TabKind::Terminal(_)));
        if focused_is_term {
            let prev = self.prev_pane;
            if self.pane(prev).is_some() && prev != self.focused_pane {
                self.set_focused_pane(prev);
            }
            self.focus_active(window, cx);
            return cx.notify();
        }
        // Focus a terminal that's already on screen.
        let visible_term = self.panes.iter().find_map(|p| {
            let t = p.active?;
            let ix = self.tab_ix(t)?;
            matches!(self.tabs[ix].kind, TabKind::Terminal(_)).then_some(p.id)
        });
        if let Some(p) = visible_term {
            self.set_focused_pane(p);
            self.focus_active(window, cx);
            return cx.notify();
        }
        // Otherwise open one in a new pane along the bottom.
        let new = self.next_id;
        self.next_id += 1;
        self.panes.push(Pane { id: new, items: Vec::new(), active: None, back: Vec::new(), fwd: Vec::new() });
        if self.layout.max_depth() >= MAX_SPLIT_DEPTH {
            // Wrapping everything would make some splits smaller than allowed:
            // split the focused one instead, or just open the terminal in it.
            self.panes.retain(|p| p.id != new);
            if self.can_split(self.focused_pane) {
                let fp = self.focused_pane;
                self.panes.push(Pane { id: new, items: Vec::new(), active: None, back: Vec::new(), fwd: Vec::new() });
                self.layout.replace_pane(fp, Layout::Split { vertical: false, ratio: 0.6, a: Box::new(Layout::Pane(fp)), b: Box::new(Layout::Pane(new)) });
                self.set_focused_pane(new);
            }
            return self.open_terminal_view(None, window, cx);
        }
        let old = std::mem::replace(&mut self.layout, Layout::Pane(new));
        self.layout = Layout::Split { vertical: false, ratio: 0.68, a: Box::new(old), b: Box::new(Layout::Pane(new)) };
        self.zoomed = false;
        self.set_focused_pane(new);
        self.open_terminal_view(None, window, cx);
    }

    fn open_terminal_view(&mut self, command: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root(cx);
        let started = match crate::terminal::start(&root) {
            Ok(s) => s,
            Err(e) => return self.flash(format!("Could not start terminal: {e}"), true, cx),
        };
        let font = self.font_size;
        let view = cx.new(|cx| TerminalView::new(started, root, font, cx));
        if let Some(cmd) = command {
            view.read(cx).run(&cmd);
        }
        let sub = cx.subscribe_in(&view, window, |ws, view, ev: &TerminalEvent, window, cx| match ev {
            TerminalEvent::Exited => {
                if let Some(ix) = ws.tabs.iter().position(|t| matches!(&t.kind, TabKind::Terminal(v) if v == view)) {
                    let id = ws.tabs[ix].id;
                    let pane = ws.pane_of_tab(id);
                    ws.drop_tab(id);
                    // Like a shell exiting in zellij: its pane goes away when it's empty.
                    if let Some(p) = pane
                        && ws.pane(p).is_some_and(|p| p.items.is_empty())
                        && ws.panes.len() > 1
                    {
                        ws.close_pane(p, window, cx);
                    } else {
                        ws.focus_active(window, cx);
                    }
                    cx.notify();
                }
            }
            TerminalEvent::Title => cx.notify(),
        });
        self.extra_subs.push(cx.subscribe_in(&view, window, |ws, _, req: &Request, window, cx| ws.handle_request(req, window, cx)));
        self.add_tab(TabKind::Terminal(view), sub, false, window, cx);
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active.and_then(|a| self.tabs.get(a)) else {
            // Empty pane: keep keys working (⌘P, ⌘T, : …).
            return window.focus(&self.focus);
        };
        match &tab.kind {
            TabKind::Editor(e) => e.read(cx).focus(window),
            TabKind::Diff(d) => d.read(cx).focus(window),
            TabKind::DiffEdit(d) => d.read(cx).focus(window, cx),
            TabKind::Commit(c) => c.read(cx).focus(window),
            TabKind::Terminal(t) => t.read(cx).focus(window),
        }
    }

    fn active_editor(&self) -> Option<Entity<Editor>> {
        match &self.tabs.get(self.active?)?.kind {
            TabKind::Editor(e) => Some(e.clone()),
            TabKind::DiffEdit(_) => self.diff_edit_right(self.active?),
            _ => None,
        }
    }

    fn diff_edit_right(&self, ix: usize) -> Option<Entity<Editor>> {
        match &self.tabs.get(ix)?.kind {
            // The right side of an editable diff behaves exactly like a file editor.
            TabKind::DiffEdit(d) => self.diff_rights.iter().find(|(v, _)| v == d).map(|(_, e)| e.clone()),
            _ => None,
        }
    }

    pub fn open_file(&mut self, path: PathBuf, line: Option<usize>, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        if path.is_dir() {
            return self.flash(format!("{} is a folder", path.display()), true, cx);
        }
        let existing = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Editor(e) if e.read(cx).path.as_ref() == Some(&path)));
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path.clone());
        self.recent.truncate(30);
        if let Some(ix) = existing {
            if !preview {
                self.tabs[ix].preview = false;
            }
            self.activate(ix, window, cx);
        } else {
            if !path.exists() {
                return self.flash(format!("No such file: {}", path.display()), true, cx);
            }
            let root = self.root(cx);
            let (font_size, blame, email) = (self.font_size, self.blame_default, self.repo.read(cx).user_email.clone());
            let editor = cx.new(|cx| {
                let mut ed = Editor::open(path.clone(), Some(root), cx);
                ed.font_size = font_size;
                ed.blame_gutter = blame;
                ed.user_email = email;
                ed
            });
            let sub = cx.subscribe_in(&editor, window, |ws, editor, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed => {
                    if let Some(tab) = ws.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::Editor(e) if e == editor)) {
                        tab.preview = false;
                    }
                    ws.schedule_autosave(editor.clone(), cx);
                    ws.check_data_file(editor.clone(), Duration::from_millis(250), cx);
                    cx.notify();
                }
                EditorEvent::OpenCommit(sha) => ws.open_commit(sha.clone(), window, cx),
                EditorEvent::Saved => {
                    if let Some(p) = editor.read(cx).path.clone() {
                        ws.check_file(p, cx);
                    }
                }
                _ => {}
            });
            self.add_tab(TabKind::Editor(editor.clone()), sub, preview, window, cx);
            self.push_diagnostics(cx);
            self.check_file(path.clone(), cx);
            self.check_data_file(editor, Duration::ZERO, cx);
        }
        if let (Some(line), Some(ed)) = (line, self.active_editor()) {
            ed.update(cx, |ed, cx| ed.go_to(line.saturating_sub(1), 0, cx));
        }
    }

    fn open_diff(&mut self, spec: DiffSpec, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = spec.key();
        if spec.right == Rev::Working && self.root(cx).join(&spec.path).is_file() {
            if let Some(ix) = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::DiffEdit(d) if d.read(cx).spec.key() == key)) {
                if !preview {
                    self.tabs[ix].preview = false;
                }
                return self.activate(ix, window, cx);
            }
            let root = self.root(cx);
            let email = self.repo.read(cx).user_email.clone();
            let (blame, font) = (self.blame_default, self.font_size);
            let view = cx.new(|cx| DiffEditView::new(root, spec, email, blame, font, window, cx));
            let right = view.read(cx).right.clone();
            self.diff_rights.push((view.clone(), right.clone()));
            let sub = cx.subscribe_in(&view, window, |ws, _, req: &Request, window, cx| ws.handle_request(req, window, cx));
            let sub2 = cx.subscribe_in(&right, window, |ws, editor, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed => {
                    if let Some(tab) = ws.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::DiffEdit(d) if &d.read(cx).right == editor)) {
                        tab.preview = false;
                    }
                    ws.schedule_autosave(editor.clone(), cx);
                    ws.check_data_file(editor.clone(), Duration::from_millis(250), cx);
                    cx.notify();
                }
                EditorEvent::OpenCommit(sha) => ws.open_commit(sha.clone(), window, cx),
                EditorEvent::Saved => {
                    if let Some(p) = editor.read(cx).path.clone() {
                        ws.check_file(p, cx);
                    }
                }
                _ => {}
            });
            self.extra_subs.push(sub2);
            self.push_diagnostics(cx);
            return self.add_tab(TabKind::DiffEdit(view), sub, preview, window, cx);
        }
        if let Some(ix) = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Diff(d) if d.read(cx).spec.key() == key)) {
            if !preview {
                self.tabs[ix].preview = false;
            }
            if let TabKind::Diff(d) = &self.tabs[ix].kind {
                d.update(cx, |d, cx| d.reload(cx));
            }
            return self.activate(ix, window, cx);
        }
        let root = self.root(cx);
        let email = self.repo.read(cx).user_email.clone();
        let blame = self.blame_default;
        let view = cx.new(|cx| DiffView::new(root, spec, email, blame, cx));
        let sub = cx.subscribe_in(&view, window, |ws, _, req: &Request, window, cx| ws.handle_request(req, window, cx));
        self.add_tab(TabKind::Diff(view), sub, preview, window, cx);
    }

    fn open_commit(&mut self, sha: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Commit(c) if c.read(cx).sha == sha)) {
            return self.activate(ix, window, cx);
        }
        let root = self.root(cx);
        let view = cx.new(|cx| CommitView::new(root, sha, cx));
        let sub = cx.subscribe_in(&view, window, |ws, view, req: &Request, window, cx| {
            // Opening a file from a commit keeps the commit page (it was a preview, which
            // the diff would otherwise replace) so you can go back to it.
            if let Request::OpenDiff { .. } = req
                && let Some(tab) = ws.tabs.iter_mut().find(|t| matches!(&t.kind, TabKind::Commit(c) if c == view))
            {
                tab.preview = false;
            }
            ws.handle_request(req, window, cx)
        });
        self.add_tab(TabKind::Commit(view), sub, true, window, cx);
    }

    fn close_tab(&mut self, ix: usize, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        if self.tab_dirty(ix, cx) && !force {
            return self.flash("Unsaved changes — :w to save, :q! to discard".into(), true, cx);
        }
        let id = self.tabs[ix].id;
        let pane = self.pane_of_tab(id);
        self.drop_tab(id);
        if let Some(p) = pane {
            if self.pane(p).is_some_and(|p| p.items.is_empty()) && self.panes.len() > 1 {
                return self.close_pane(p, window, cx);
            }
            self.set_focused_pane(p);
        }
        if self.active.is_some() {
            self.focus_active(window, cx);
        } else {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    fn tab_title(&self, tab: &Tab, cx: &App) -> (String, String, bool) {
        match &tab.kind {
            TabKind::Editor(e) => {
                let e = e.read(cx);
                let name = e.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                (name.clone(), name, e.buf.is_dirty())
            }
            TabKind::Diff(d) => {
                let d = d.read(cx);
                (d.title(), d.spec.path.clone(), false)
            }
            TabKind::DiffEdit(d) => {
                let d = d.read(cx);
                (d.title(), d.spec.path.clone(), d.right.read(cx).buf.is_dirty())
            }
            TabKind::Commit(c) => (c.read(cx).title(), String::new(), false),
            TabKind::Terminal(t) => {
                let t = t.read(cx);
                (format!("$ {}", t.title), String::new(), false)
            }
        }
    }

    // ------------------------------------------------------------ actions

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let format = self.settings.format_on_save;
        self.save_active(format, cx);
    }

    /// ⌘S: format with the project's formatter (when there is one), then save.
    /// Autosave never formats, so text doesn't jump around while typing.
    fn save_active(&mut self, format: bool, cx: &mut Context<Self>) {
        let Some(ed) = self.active_editor() else { return };
        let path = ed.read(cx).path.clone();
        let (Some(path), true) = (path, format) else { return self.finish_save(ed, cx) };
        let root = self.root(cx);
        let text = ed.read(cx).text();
        let version = ed.read(cx).buf.version();
        let input = text.clone();
        let task = cx.background_executor().spawn(async move { tools::format(&root, &path, &input) });
        self.format_save_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |ws, cx| {
                // Only apply if nothing was typed meanwhile; formatting is one undo step.
                if let Ok((formatted, _)) = result
                    && formatted != text
                    && ed.read(cx).buf.version() == version
                {
                    ed.update(cx, |e, cx| e.apply_format(&formatted, cx));
                }
                ws.finish_save(ed, cx);
            })
            .ok();
        }));
    }

    fn finish_save(&mut self, ed: Entity<Editor>, cx: &mut Context<Self>) {
        match ed.update(cx, |ed, cx| ed.save(cx)) {
            Ok(()) => {
                let name = ed.read(cx).path.as_ref().map(|p| self.rel(p, cx)).unwrap_or_default();
                self.flash(format!("Saved {name}"), false, cx);
                self.repo.update(cx, |r, cx| r.refresh_status(cx));
            }
            Err(e) => self.flash(format!("Save failed: {e}"), true, cx),
        }
    }

    fn close_active(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            self.close_tab(a, false, window, cx);
        }
    }

    fn cycle_buffer(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.pane(self.focused_pane) else { return };
        if pane.items.is_empty() {
            return;
        }
        let n = pane.items.len() as isize;
        let cur = pane.active.and_then(|a| pane.items.iter().position(|x| *x == a)).unwrap_or(0) as isize;
        let next = pane.items[((cur + delta) % n + n) as usize % n as usize];
        if let Some(ix) = self.tab_ix(next) {
            self.activate(ix, window, cx);
        }
    }

    /// ⌃- / ⌃⇧-: back / forward through what this split showed.
    fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let fp = self.focused_pane;
        let Some(pane) = self.pane_mut(fp) else { return };
        let (from, to) = if back { (&mut pane.back, &mut pane.fwd) } else { (&mut pane.fwd, &mut pane.back) };
        let Some(target) = from.pop() else {
            return self.flash(if back { "Nothing to go back to".into() } else { "Nothing to go forward to".into() }, false, cx);
        };
        if let Some(cur) = pane.active {
            to.push(cur);
        }
        let Some(ix) = self.tab_ix(target) else { return };
        self.navigating = true;
        self.activate(ix, window, cx);
        self.navigating = false;
    }

    /// ⌘1…⌘9: show the Nth item of the focused pane (⌘9 = last, like browsers).
    fn show_buffer(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.pane(self.focused_pane) else { return };
        let Some(&tid) = (if n == usize::MAX { pane.items.last() } else { pane.items.get(n) }) else { return };
        if let Some(ix) = self.tab_ix(tid) {
            self.activate(ix, window, cx);
        }
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_buffer(1, window, cx);
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_buffer(-1, window, cx);
    }

    fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.side_visible = true;
        match self.side {
            Side::Files => self.tree.read(cx).focus(window),
            Side::Git => self.git_panel.read(cx).focus(window),
            Side::Search => {
                let query = self.search.read(cx).query.clone();
                query.update(cx, |q, cx| {
                    q.buf.select_all();
                    cx.notify();
                });
                query.read(cx).focus(window);
            }
            Side::Keys => self.keys.read(cx).focus(window, cx),
        }
        cx.notify();
    }

    fn show_files(&mut self, _: &ShowFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.side = Side::Files;
        self.focus_sidebar(window, cx);
    }

    fn show_git(&mut self, _: &ShowGit, window: &mut Window, cx: &mut Context<Self>) {
        self.side = Side::Git;
        self.focus_sidebar(window, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        self.side_visible = !self.side_visible;
        if !self.side_visible {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// ⌘J: open the output panel and put keyboard focus in it; again to close it.
    fn toggle_output(&mut self, _: &ToggleOutput, window: &mut Window, cx: &mut Context<Self>) {
        self.output_visible = !self.output_visible;
        if self.output_visible {
            window.focus(&self.output_focus);
        } else if self.output_focus.is_focused(window) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn output_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.output.len();
        if n == 0 {
            return;
        }
        self.output_sel = (self.output_sel as isize + delta).clamp(0, n as isize - 1) as usize;
        self.output_scroll.scroll_to_item(self.output_sel, ScrollStrategy::Top);
        cx.notify();
    }

    fn output_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((file, row, _)) = self.output.get(self.output_sel).and_then(|l| l.loc.clone()) else { return };
        let root = self.root(cx);
        self.open_file(resolve(&root, &file), Some(row), false, window, cx);
    }

    fn on_focus_editor(&mut self, _: &FocusEditor, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active(window, cx);
    }

    fn on_focus_sidebar(&mut self, _: &FocusSidebar, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_sidebar(window, cx);
    }

    fn on_focus_commit(&mut self, _: &FocusCommit, window: &mut Window, cx: &mut Context<Self>) {
        self.side = Side::Git;
        self.side_visible = true;
        self.git_panel.read(cx).focus_commit_input(window, cx);
        cx.notify();
    }

    fn toggle_blame(&mut self, _: &ToggleBlame, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active.and_then(|a| self.tabs.get(a)) else { return };
        match &tab.kind {
            TabKind::Editor(e) => {
                let on = e.update(cx, |e, cx| {
                    e.blame_gutter = !e.blame_gutter;
                    cx.notify();
                    e.blame_gutter
                });
                self.blame_default = on;
            }
            TabKind::Diff(d) => {
                let on = d.update(cx, |d, cx| {
                    d.toggle_blame(cx);
                    d.show_blame
                });
                self.blame_default = on;
            }
            TabKind::DiffEdit(d) => {
                let on = !d.read(cx).right.read(cx).blame_gutter;
                d.update(cx, |d, cx| d.set_blame(on, cx));
                self.blame_default = on;
            }
            TabKind::Commit(_) | TabKind::Terminal(_) => {}
        }
        self.settings.blame_default = self.blame_default;
        self.settings.save();
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.font_size = if delta == 0. { 12. } else { (self.font_size + delta).clamp(8., 32.) };
        let size = self.font_size;
        for tab in &self.tabs {
            let eds: Vec<Entity<Editor>> = match &tab.kind {
                TabKind::Editor(e) => vec![e.clone()],
                TabKind::DiffEdit(d) => vec![d.read(cx).left.clone(), d.read(cx).right.clone()],
                TabKind::Terminal(t) => {
                    t.update(cx, |t, cx| {
                        t.font_size = size;
                        cx.notify();
                    });
                    vec![]
                }
                _ => vec![],
            };
            for e in eds {
                e.update(cx, |e, cx| {
                    e.font_size = size;
                    cx.notify();
                });
            }
        }
        self.settings.font_size = size;
        self.settings.save();
        self.flash(format!("Font size {size}"), false, cx);
    }

    fn format_file(&mut self, _: &FormatFile, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.active_editor() else { return self.flash("Open a file to format it".into(), true, cx) };
        let (Some(path), text) = (ed.read(cx).path.clone(), ed.read(cx).text()) else { return };
        let root = self.root(cx);
        let task = cx.background_executor().spawn(async move { tools::format(&root, &path, &text) });
        self.flash("Formatting…".into(), false, cx);
        self.tool_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |ws, cx| match result {
                Ok((formatted, tool)) => {
                    ed.update(cx, |ed, cx| ed.apply_format(&formatted, cx));
                    ws.flash(format!("Formatted with {tool}"), false, cx);
                }
                Err(e) => {
                    let first = e.lines().next().unwrap_or("").to_string();
                    ws.show_output("format".into(), &e, true, cx);
                    ws.flash(first, true, cx);
                }
            })
            .ok();
        }));
    }

    fn lint(&mut self, whole_project: bool, cx: &mut Context<Self>) {
        let root = self.root(cx);
        let target = if whole_project { None } else { self.active_editor().and_then(|e| e.read(cx).path.clone()) };
        if let Some(ed) = self.active_editor()
            && ed.read(cx).buf.is_dirty()
        {
            ed.update(cx, |e, cx| e.save(cx)).ok();
        }
        self.flash("Linting…".into(), false, cx);
        let task = cx.background_executor().spawn(async move { tools::lint(&root, target.as_deref()) });
        self.tool_task = Some(cx.spawn(async move |this, cx| {
            let out = task.await;
            this.update(cx, |ws, cx| {
                let text = format!("{}{}", out.stdout, out.stderr);
                let problems = text.lines().filter(|l| util::parse_location(l).is_some() || l.trim_start().split(':').next().is_some_and(|n| n.parse::<usize>().is_ok())).count();
                ws.show_output(out.tool.clone(), &text, !out.ok, cx);
                ws.flash(if out.ok && problems == 0 { format!("{}: no problems", out.tool) } else { format!("{}: {problems} problem lines", out.tool) }, !out.ok, cx);
            })
            .ok();
        }));
    }

    fn run_shell(&mut self, line: String, cx: &mut Context<Self>) {
        let root = self.root(cx);
        self.flash(format!("$ {line}"), false, cx);
        let task = cx.background_executor().spawn(async move { tools::shell(&root, &line) });
        self.tool_task = Some(cx.spawn(async move |this, cx| {
            let out = task.await;
            this.update(cx, |ws, cx| {
                let text = format!("{}{}", out.stdout, out.stderr);
                ws.show_output(format!("$ {}", out.tool), &text, !out.ok, cx);
                ws.flash(format!("$ {} — {}", out.tool, if out.ok { "ok" } else { "failed" }), !out.ok, cx);
                ws.repo.update(cx, |r, cx| r.refresh_status(cx));
            })
            .ok();
        }));
    }

    fn show_keys(&mut self, cx: &mut Context<Self>) {
        self.show_output("Key bindings (also in the ⌨ sidebar tab)".into(), &keymap::as_text(), false, cx);
    }

    fn select_side(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        if self.side == side && self.side_visible {
            self.side_visible = false;
            self.focus_active(window, cx);
            return cx.notify();
        }
        self.side = side;
        self.focus_sidebar(window, cx);
    }

    fn search_all(&mut self, _: &SearchAll, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.active_editor().and_then(|e| {
            let e = e.read(cx);
            let t = e.buf.slice(e.buf.sel.range());
            (!t.is_empty() && !t.contains('\n')).then_some(t)
        });
        self.side = Side::Search;
        self.side_visible = true;
        if let Some(q) = selected {
            self.search.update(cx, |s, cx| s.set_query(&q, cx));
        }
        self.focus_sidebar(window, cx);
    }

    fn show_keys_panel(&mut self, _: &ShowKeysPanel, window: &mut Window, cx: &mut Context<Self>) {
        self.side = Side::Keys;
        self.focus_sidebar(window, cx);
    }

    fn toggle_sidebar_side(&mut self, _: &ToggleSidebarSide, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.sidebar_right = !self.settings.sidebar_right;
        self.settings.save();
        cx.notify();
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_folder(false, window, cx);
    }

    /// ⌘⇧N: another project, side by side in its own window.
    fn open_folder_new_window(&mut self, _: &OpenFolderNewWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_folder(true, window, cx);
    }

    fn pick_folder(&mut self, new_window: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = if new_window { "Open in New Window" } else { "Open" };
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some(prompt.into()) });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                this.update_in(cx, |ws, window, cx| {
                    for (i, dir) in paths.into_iter().enumerate() {
                        if new_window || i > 0 {
                            ws.settings.add_recent(dir.clone());
                            ws.settings.save();
                            crate::open_window(dir.canonicalize().unwrap_or(dir), cx);
                        } else {
                            ws.switch_project(dir, window, cx);
                        }
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// ⌘`: cycle through open project windows.
    fn next_window(&mut self, _: &NextWindow, window: &mut Window, cx: &mut Context<Self>) {
        let windows = cx.windows();
        if windows.len() < 2 {
            return self.flash("Only one project open — ⌘⇧N opens another in a new window".into(), false, cx);
        }
        let me = window.window_handle();
        let ix = windows.iter().position(|w| *w == me).unwrap_or(0);
        let next = windows[(ix + 1) % windows.len()];
        next.update(cx, |_, window, _| window.activate_window()).ok();
    }

    /// Replaces this window's project with another folder.
    fn switch_project(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let dirty = self.tabs.iter().any(|t| match &t.kind {
            TabKind::Editor(e) => e.read(cx).buf.is_dirty(),
            TabKind::DiffEdit(d) => d.read(cx).right.read(cx).buf.is_dirty(),
            _ => false,
        });
        if dirty {
            return self.flash("Save or close unsaved files before switching folders".into(), true, cx);
        }
        let dir = dir.canonicalize().unwrap_or(dir);
        window.defer(cx, move |window, cx| {
            window.replace_root(cx, |window, cx| Workspace::new(dir, window, cx));
        });
    }

    fn apply_replace(&mut self, plan: &crate::search::ReplacePlan, _: &mut Window, cx: &mut Context<Self>) {
        let mut replaced = Vec::new();
        let mut total = 0;
        let mut failed = Vec::new();
        for (path, content, count) in &plan.files {
            let open: Option<Entity<Editor>> = self.tabs.iter().find_map(|t| match &t.kind {
                TabKind::Editor(e) if e.read(cx).path.as_ref() == Some(path) => Some(e.clone()),
                TabKind::DiffEdit(d) if d.read(cx).right.read(cx).path.as_ref() == Some(path) => Some(d.read(cx).right.clone()),
                _ => None,
            });
            let result = match open {
                // Open files are edited in place (undoable with ⌘Z) and saved.
                Some(ed) if !ed.read(cx).buf.is_dirty() => ed.update(cx, |ed, cx| {
                    ed.apply_format(content, cx);
                    ed.save(cx)
                }),
                Some(_) => Err(anyhow::anyhow!("has unsaved changes")),
                None => std::fs::write(path, content).map_err(Into::into),
            };
            match result {
                Ok(()) => {
                    replaced.push(path.clone());
                    total += count;
                }
                Err(e) => failed.push(format!("{}: {e}", self.rel(path, cx))),
            }
        }
        self.search.update(cx, |s, cx| s.after_replace(&replaced, cx));
        let mut msg = format!("Replaced {total} occurrence{} in {} file{}", if total == 1 { "" } else { "s" }, replaced.len(), if replaced.len() == 1 { "" } else { "s" });
        let skipped: Vec<String> = plan.skipped.iter().cloned().chain(failed).collect();
        if !skipped.is_empty() {
            msg.push_str(&format!(" — skipped {} (changed since search or unsaved): {}", skipped.len(), skipped.join(", ")));
        }
        self.flash(msg, !skipped.is_empty(), cx);
        self.repo.update(cx, |r, cx| r.refresh_status(cx));
    }

    /// Auto save: one second after the last edit, like VS Code's "afterDelay".
    fn schedule_autosave(&mut self, editor: Entity<Editor>, cx: &mut Context<Self>) {
        if !self.settings.autosave || !editor.read(cx).buf.is_dirty() {
            return;
        }
        let id = editor.entity_id();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1000)).await;
            this.update(cx, |ws, cx| {
                ws.autosave_tasks.remove(&id);
                if !ws.settings.autosave || !editor.read(cx).buf.is_dirty() {
                    return;
                }
                match editor.update(cx, |e, cx| e.save(cx)) {
                    Ok(()) => ws.repo.update(cx, |r, cx| r.refresh_status(cx)),
                    Err(e) => ws.flash(format!("Auto save failed: {e}"), true, cx),
                }
                cx.notify();
            })
            .ok();
        });
        self.autosave_tasks.insert(id, task);
    }

    fn set_autosave(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.autosave = on;
        self.settings.save();
        if !on {
            self.autosave_tasks.clear();
        }
        self.flash(format!("Auto save {}", if on { "on" } else { "off" }), false, cx);
    }

    // ------------------------------------------------------------ diagnostics

    /// Lints a saved/opened file, and type-checks its TypeScript project.
    fn check_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if std::env::var_os("KITE_DEBUG").is_some() {
            eprintln!("[kite] check_file {} (checks on: {})", path.display(), self.settings.checks);
        }
        if !self.settings.checks {
            return;
        }
        let root = self.root(cx);
        let (r, p) = (root.clone(), path.clone());
        let task = cx.background_executor().spawn(async move { crate::diagnostics::lint_file(&r, &p) });
        let key = path.clone();
        self.lint_tasks.insert(
            key.clone(),
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if std::env::var_os("KITE_DEBUG").is_some() {
                    eprintln!("[kite] lint {}: {:?}", key.display(), result.as_ref().map(|r| r.as_ref().map(|(t, d)| (t.clone(), d.len()))));
                }
                this.update(cx, |ws, cx| {
                    ws.lint_tasks.remove(&key);
                    match result {
                        Some(Ok((_, diags))) => {
                            let key = key.canonicalize().unwrap_or(key.clone());
                            ws.lint_diags.insert(key, diags);
                            ws.push_diagnostics(cx);
                        }
                        Some(Err(e)) => ws.flash(e, true, cx),
                        None => {}
                    }
                })
                .ok();
            }),
        );
        if crate::diagnostics::is_typescript(&path) {
            self.typecheck(path, cx);
        }
    }

    /// JSON / YAML: checked as you type (built in, no tools needed).
    fn check_data_file(&mut self, editor: Entity<Editor>, delay: Duration, cx: &mut Context<Self>) {
        let Some(path) = editor.read(cx).path.clone() else { return };
        let Some(kind) = crate::datafiles::kind(&path) else { return };
        let id = editor.entity_id();
        let text = editor.read(cx).text();
        let task = cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let diags = cx.background_executor().spawn(async move { crate::datafiles::check(kind, &text) }).await;
            this.update(cx, |ws, cx| {
                ws.data_tasks.remove(&id);
                let key = path.canonicalize().unwrap_or(path.clone());
                ws.data_diags.insert(key, diags);
                ws.push_diagnostics(cx);
            })
            .ok();
        });
        self.data_tasks.insert(id, task);
    }

    fn typecheck(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.tsc_running {
            // One check at a time; run once more afterwards with the latest file.
            self.tsc_again = Some(path);
            return;
        }
        self.tsc_running = true;
        cx.notify();
        let root = self.root(cx);
        let task = cx.background_executor().spawn(async move { crate::diagnostics::typecheck(&root, &path) });
        self.tsc_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            if std::env::var_os("KITE_DEBUG").is_some() {
                eprintln!("[kite] tsc: {:?}", result.as_ref().map(|r| r.as_ref().map(|m| m.iter().map(|(p, d)| (p.display().to_string(), d.len())).collect::<Vec<_>>())));
            }
            this.update(cx, |ws, cx| {
                ws.tsc_running = false;
                match result {
                    Some(Ok(map)) => {
                        ws.ts_diags = map;
                        ws.push_diagnostics(cx);
                    }
                    Some(Err(e)) => ws.flash(e, true, cx),
                    None => {}
                }
                if let Some(next) = ws.tsc_again.take() {
                    ws.typecheck(next, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn diags_for(&self, path: &Path) -> Vec<Diag> {
        let key = path.canonicalize().unwrap_or(path.to_path_buf());
        let mut out: Vec<Diag> = self.lint_diags.get(&key).into_iter().chain(self.ts_diags.get(&key)).chain(self.data_diags.get(&key)).flatten().cloned().collect();
        out.sort_by_key(|d| (d.row, d.col, d.severity));
        out
    }

    fn push_diagnostics(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<Entity<Editor>> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::Editor(e) => Some(e.clone()),
                TabKind::DiffEdit(d) => Some(d.read(cx).right.clone()),
                _ => None,
            })
            .collect();
        for e in editors {
            let Some(path) = e.read(cx).path.clone() else { continue };
            let diags = self.diags_for(&path);
            e.update(cx, |e, cx| e.set_diagnostics(diags, cx));
        }
        cx.notify();
    }

    fn problem_counts(&self) -> (usize, usize) {
        let all = self.lint_diags.values().chain(self.ts_diags.values()).chain(self.data_diags.values()).flatten();
        all.fold((0, 0), |(e, w), d| match d.severity {
            Severity::Error => (e + 1, w),
            _ => (e, w + 1),
        })
    }

    fn show_problems(&mut self, cx: &mut Context<Self>) {
        let root = self.root(cx);
        let mut rows: Vec<(PathBuf, Diag)> = self
            .lint_diags
            .iter()
            .chain(self.ts_diags.iter())
            .chain(self.data_diags.iter())
            .flat_map(|(p, ds)| ds.iter().map(move |d| (p.clone(), d.clone())))
            .collect();
        rows.sort_by(|a, b| (a.1.severity, &a.0, a.1.row).cmp(&(b.1.severity, &b.0, b.1.row)));
        let text = rows
            .iter()
            .map(|(p, d)| {
                let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_string();
                let sev = match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warn ",
                    Severity::Info => "info ",
                };
                format!("{rel}:{}:{}  {sev}  {}  ({})", d.row + 1, d.col + 1, d.message.lines().next().unwrap_or(""), d.source)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (e, w) = self.problem_counts();
        let body = if text.is_empty() { "No problems found. (Lint runs on open/save, TypeScript checks on save of .ts/.tsx.)".to_string() } else { text };
        self.show_output(format!("Problems — {e} errors, {w} warnings"), &body, false, cx);
    }

    // ------------------------------------------------------------ command line

    fn command_palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.open_cmd(CmdMode::Command, "", window, cx);
    }

    fn go_to_line(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
        self.open_cmd(CmdMode::Command, "", window, cx);
        self.flash("Type a line number and press Enter".into(), false, cx);
    }

    fn on_run_shell(&mut self, _: &RunShell, window: &mut Window, cx: &mut Context<Self>) {
        self.open_cmd(CmdMode::Command, "!", window, cx);
    }

    fn find_file(&mut self, _: &FindFile, window: &mut Window, cx: &mut Context<Self>) {
        self.open_cmd(CmdMode::Files, "", window, cx);
    }

    fn find_in_file(&mut self, _: &FindInFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.active_editor() else { return self.flash("Open a file to search in it".into(), true, cx) };
        let (origin, selected) = {
            let e = ed.read(cx);
            let r = e.buf.sel.range();
            let text = e.buf.slice(r.clone());
            (r.start, if !text.is_empty() && !text.contains('\n') { Some(text) } else { None })
        };
        self.search_origin = Some(origin);
        let initial = selected.or(self.last_search.clone()).unwrap_or_default();
        self.open_cmd(CmdMode::Search, &initial, window, cx);
        self.cmd.update(cx, |c, cx| {
            c.buf.select_all();
            cx.notify();
        });
    }

    fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(query) = self.last_search.clone() else { return };
        let Some(ed) = self.active_editor() else { return };
        ed.update(cx, |ed, cx| {
            let r = ed.buf.sel.range();
            let from = if forward { r.end } else { r.start };
            ed.search = Some(query.clone());
            if let Some(m) = ed.buf.find(&query, from, forward) {
                ed.select_range(m, cx);
            }
        });
    }

    fn open_cmd(&mut self, mode: CmdMode, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmd_mode.is_none() {
            self.return_focus = window.focused(cx);
        }
        self.cmd_mode = Some(mode);
        self.hist_ix = None;
        if mode == CmdMode::Command {
            let root = self.root(cx);
            self.branches = git::branches(&root);
        }
        self.cmd.update(cx, |c, cx| {
            c.placeholder = match mode {
                CmdMode::Command => "command — try w, format, commit <msg>, checkout <branch>, !<shell>, 42 (go to line)".into(),
                CmdMode::Search => "search in file".into(),
                CmdMode::Files => "file name — append :line to jump".into(),
                CmdMode::Buffers => "files open in this pane".into(),
            };
            c.set_text(text, cx);
            c.focus(window);
        });
        self.update_items(cx);
        cx.notify();
    }

    fn close_cmd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cmd_mode = None;
        self.cmd_items.clear();
        if let Some(f) = self.return_focus.take() {
            window.focus(&f);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn on_cmd_event(&mut self, ev: &EditorEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.cmd_mode else { return };
        match ev {
            EditorEvent::Changed => {
                self.update_items(cx);
                if mode == CmdMode::Search {
                    self.incremental_search(cx);
                }
                cx.notify();
            }
            EditorEvent::Submit => self.submit_cmd(window, cx),
            EditorEvent::Cancel => self.cancel_cmd(window, cx),
            EditorEvent::Up | EditorEvent::Down => {
                let up = matches!(ev, EditorEvent::Up);
                if !self.cmd_items.is_empty() {
                    let n = self.cmd_items.len();
                    self.cmd_sel = if up { (self.cmd_sel + n - 1) % n } else { (self.cmd_sel + 1) % n };
                    self.cmd_nav = true;
                } else if mode == CmdMode::Command && !self.history.is_empty() {
                    let ix = match (self.hist_ix, up) {
                        (None, true) => Some(self.history.len() - 1),
                        (None, false) => None,
                        (Some(i), true) => Some(i.saturating_sub(1)),
                        (Some(i), false) => (i + 1 < self.history.len()).then_some(i + 1),
                    };
                    self.hist_ix = ix;
                    let text = ix.map(|i| self.history[i].clone()).unwrap_or_default();
                    self.cmd.update(cx, |c, cx| c.set_text(&text, cx));
                    self.cmd_items.clear();
                }
                cx.notify();
            }
            EditorEvent::Tab => {
                if let Some(item) = self.cmd_items.get(self.cmd_sel) {
                    let mut value = item.value.clone();
                    if mode == CmdMode::Command && !item.exec && !value.ends_with('/') && !value.ends_with(' ') && !value.contains(' ') {
                        value.push(' ');
                    }
                    self.cmd.update(cx, |c, cx| c.set_text(&value, cx));
                }
            }
            EditorEvent::OpenCommit(_) | EditorEvent::Saved => {}
        }
    }

    /// Esc anywhere (or a click outside) closes the command line / finder.
    fn cancel_cmd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmd_mode == Some(CmdMode::Search)
            && let (Some(ed), Some(origin)) = (self.active_editor(), self.search_origin)
        {
            ed.update(cx, |ed, cx| {
                ed.search = None;
                ed.select_range(origin..origin, cx);
            });
        }
        self.close_cmd(window, cx);
    }

    fn incremental_search(&mut self, cx: &mut Context<Self>) {
        let query = self.cmd.read(cx).text();
        let Some(ed) = self.active_editor() else { return };
        let origin = self.search_origin.unwrap_or(0);
        ed.update(cx, |ed, cx| {
            ed.search = (!query.is_empty()).then(|| query.clone());
            match ed.buf.find(&query, origin, true) {
                Some(m) => ed.select_range(m, cx),
                None => ed.select_range(origin..origin, cx),
            }
        });
    }

    fn update_items(&mut self, cx: &mut Context<Self>) {
        let text = self.cmd.read(cx).text();
        self.cmd_sel = 0;
        self.cmd_nav = false;
        self.cmd_items = match self.cmd_mode {
            Some(CmdMode::Files) => self.file_items(&text, cx),
            Some(CmdMode::Buffers) => self.buffer_items(&text, cx),
            Some(CmdMode::Command) => self.command_items(&text, cx),
            _ => Vec::new(),
        };
    }

    fn buffer_items(&self, query: &str, cx: &App) -> Vec<CmdItem> {
        let Some(pane) = self.pane(self.focused_pane) else { return Vec::new() };
        pane.items
            .iter()
            .filter_map(|id| {
                let ix = self.tab_ix(*id)?;
                let (title, path, dirty) = self.tab_title(&self.tabs[ix], cx);
                let (_, positions) = util::fuzzy(query, &title)?;
                let label = if dirty { format!("{title} ●") } else { title };
                Some(CmdItem { label, detail: path, value: id.to_string(), positions, exec: true })
            })
            .collect()
    }

    fn file_items(&self, query: &str, cx: &App) -> Vec<CmdItem> {
        let (query, _) = split_line_suffix(query);
        let root = self.root(cx);
        if query.is_empty() {
            return self
                .recent
                .iter()
                .filter(|p| self.active_editor().and_then(|e| e.read(cx).path.clone()).as_ref() != Some(*p))
                .take(15)
                .map(|p| {
                    let rel = self.rel(p, cx);
                    CmdItem { label: ui::file_name(&rel).to_string(), detail: ui::file_dir(&rel).to_string(), value: rel, positions: vec![], exec: true }
                })
                .collect();
        }
        let files = self.repo.read(cx).files.clone();
        let mut scored: Vec<(i64, &String, Vec<usize>)> = files.iter().filter_map(|f| util::fuzzy(query, f).map(|(s, p)| (s, f, p))).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
        let _ = root;
        scored
            .into_iter()
            .take(60)
            .map(|(_, f, positions)| CmdItem { label: f.clone(), detail: String::new(), value: f.clone(), positions, exec: true })
            .collect()
    }

    fn command_items(&self, text: &str, cx: &App) -> Vec<CmdItem> {
        if text.is_empty() || text.starts_with('!') || text.chars().all(|c| c.is_ascii_digit()) {
            return Vec::new();
        }
        let Some((head, rest)) = text.split_once(' ') else {
            let mut items: Vec<(i64, CmdItem)> = COMMANDS
                .iter()
                .filter_map(|(name, detail, takes_arg)| {
                    let (score, positions) = util::fuzzy(text, name)?;
                    let bonus = if name.starts_with(text) { 100 } else { 0 };
                    Some((score + bonus, CmdItem { label: name.to_string(), detail: detail.to_string(), value: name.to_string(), positions, exec: !takes_arg }))
                })
                .collect();
            items.sort_by(|a, b| b.0.cmp(&a.0));
            return items.into_iter().map(|(_, i)| i).take(12).collect();
        };
        let root = self.root(cx);
        match head {
            "e" | "open" | "edit" | "new" | "mkdir" | "cd" | "lint" | "window" | "vsplit" | "vs" | "split" | "sp" => path_items(&root, head, "", rest),
            "mv" => {
                let (first, last) = rest.rsplit_once(' ').map(|(a, b)| (format!("{a} "), b)).unwrap_or((String::new(), rest));
                path_items(&root, head, &first, last)
            }
            "checkout" | "co" | "switch" | "merge" | "rebase" => {
                let mut v: Vec<(i64, CmdItem)> = self
                    .branches
                    .iter()
                    .filter_map(|b| {
                        let (s, positions) = util::fuzzy(rest, b)?;
                        Some((s, CmdItem { label: b.clone(), detail: String::new(), value: format!("{head} {b}"), positions, exec: true }))
                    })
                    .collect();
                v.sort_by(|a, b| b.0.cmp(&a.0));
                v.into_iter().map(|(_, i)| i).take(12).collect()
            }
            "stage" | "add" | "unstage" | "discard" => {
                let st = &self.repo.read(cx).status;
                let list = if head == "unstage" { &st.staged } else { &st.unstaged };
                list.iter()
                    .filter_map(|c| {
                        let (_, positions) = util::fuzzy(rest, &c.path)?;
                        Some(CmdItem { label: c.path.clone(), detail: c.status.letter().into(), value: format!("{head} {}", c.path), positions, exec: true })
                    })
                    .take(12)
                    .collect()
            }
            "recent" => self
                .settings
                .recent_folders
                .iter()
                .filter_map(|f| {
                    let label = f.to_string_lossy().to_string();
                    let (_, positions) = util::fuzzy(rest, &label)?;
                    Some(CmdItem { label: label.clone(), detail: String::new(), value: format!("recent {label}"), positions, exec: true })
                })
                .take(12)
                .collect(),
            "autosave" | "formatonsave" | "fos" => ["on", "off"]
                .iter()
                .map(|s| CmdItem { label: s.to_string(), detail: String::new(), value: format!("{head} {s}"), positions: vec![], exec: true })
                .collect(),
            "sidebar" => ["left", "right"]
                .iter()
                .map(|s| CmdItem { label: s.to_string(), detail: String::new(), value: format!("sidebar {s}"), positions: vec![], exec: true })
                .collect(),
            "graph" => ["all", "auto"]
                .iter()
                .map(|s| CmdItem { label: s.to_string(), detail: String::new(), value: format!("graph {s}"), positions: vec![], exec: true })
                .collect(),
            "zoom" => ["in", "out", "reset"]
                .iter()
                .map(|s| CmdItem { label: s.to_string(), detail: String::new(), value: format!("zoom {s}"), positions: vec![], exec: true })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn submit_cmd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.cmd_mode else { return };
        let text = self.cmd.read(cx).text();
        match mode {
            CmdMode::Buffers => {
                let id = self.cmd_items.get(self.cmd_sel).and_then(|i| i.value.parse::<usize>().ok());
                self.close_cmd(window, cx);
                if let Some(ix) = id.and_then(|id| self.tab_ix(id)) {
                    self.activate(ix, window, cx);
                }
            }
            CmdMode::Files => {
                let (_, line) = split_line_suffix(&text);
                let Some(item) = self.cmd_items.get(self.cmd_sel) else { return self.close_cmd(window, cx) };
                let path = self.root(cx).join(&item.value);
                self.close_cmd(window, cx);
                self.open_file(path, line, false, window, cx);
            }
            CmdMode::Search => {
                self.last_search = (!text.is_empty()).then_some(text);
                self.return_focus = None;
                self.close_cmd(window, cx);
            }
            CmdMode::Command => {
                let mut line = text.trim().to_string();
                if self.cmd_nav
                    && let Some(item) = self.cmd_items.get(self.cmd_sel)
                {
                    if !item.exec || item.value.ends_with('/') {
                        let mut v = item.value.clone();
                        if !v.ends_with('/') && !v.contains(' ') {
                            v.push(' ');
                        }
                        self.cmd.update(cx, |c, cx| c.set_text(&v, cx));
                        return;
                    }
                    line = item.value.clone();
                } else if !line.contains(' ')
                    && !line.starts_with('!')
                    && !COMMANDS.iter().any(|(n, _, _)| *n == line.trim_end_matches('!'))
                    && line.parse::<usize>().is_err()
                    && let Some(item) = self.cmd_items.first()
                {
                    // Typed a prefix like "fo": run the best match ("format").
                    if item.exec {
                        line = item.value.clone();
                    }
                }
                if line.is_empty() {
                    return self.close_cmd(window, cx);
                }
                self.history.retain(|h| h != &line);
                self.history.push(line.clone());
                self.close_cmd(window, cx);
                self.run_command(&line, window, cx);
            }
        }
    }

    fn run_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) {
        let line = line.trim();
        if let Ok(n) = line.parse::<usize>() {
            if let Some(ed) = self.active_editor() {
                ed.update(cx, |ed, cx| ed.go_to(n.saturating_sub(1), 0, cx));
                ed.read(cx).focus(window);
            }
            return;
        }
        if let Some(shell) = line.strip_prefix('!') {
            return self.run_shell(shell.trim().to_string(), cx);
        }
        let (cmd, rest) = line.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((line, ""));
        let root = self.root(cx);
        let active_rel = self.active_editor().and_then(|e| e.read(cx).rel_path());
        let args_or_active = |rest: &str| -> Vec<String> {
            if rest.is_empty() { active_rel.clone().into_iter().collect() } else { rest.split_whitespace().map(String::from).collect() }
        };
        match cmd {
            "w" | "write" => self.save(&Save, window, cx),
            "wq" | "x" => {
                self.save_active(false, cx);
                self.close_active(&CloseTab, window, cx);
            }
            "q" => self.close_active(&CloseTab, window, cx),
            "q!" => {
                if let Some(a) = self.active {
                    self.close_tab(a, true, window, cx);
                }
            }
            "qa" | "quit" => self.quit(false, cx),
            "qa!" => self.quit(true, cx),
            "open" if rest.is_empty() => self.open_folder(&OpenFolder, window, cx),
            "open" | "cd" if resolve(&root, rest).is_dir() => self.switch_project(resolve(&root, rest), window, cx),
            "e" | "edit" | "open" => {
                let (path, line) = split_line_suffix(rest);
                self.open_file(resolve(&root, path), line, false, window, cx);
            }
            "new" | "touch" => {
                let path = resolve(&root, rest);
                if rest.is_empty() || rest.ends_with('/') {
                    return self.flash("Usage: new <path/to/file>".into(), true, cx);
                }
                if !path.exists() {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    if let Err(e) = std::fs::write(&path, "") {
                        return self.flash(format!("Could not create file: {e}"), true, cx);
                    }
                }
                self.open_file(path, None, false, window, cx);
            }
            "mkdir" => match std::fs::create_dir_all(resolve(&root, rest)) {
                Ok(()) => self.flash(format!("Created {rest}"), false, cx),
                Err(e) => self.flash(format!("mkdir failed: {e}"), true, cx),
            },
            "mv" => {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() != 2 {
                    return self.flash("Usage: mv <from> <to>".into(), true, cx);
                }
                let (from, to) = (resolve(&root, parts[0]), resolve(&root, parts[1]));
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                match std::fs::rename(&from, &to) {
                    Ok(()) => {
                        for tab in &self.tabs {
                            if let TabKind::Editor(e) = &tab.kind
                                && e.read(cx).path.as_ref() == Some(&from)
                            {
                                e.update(cx, |e, cx| {
                                    e.path = Some(to.clone());
                                    cx.notify();
                                });
                            }
                        }
                        self.flash(format!("Renamed to {}", parts[1]), false, cx);
                    }
                    Err(e) => self.flash(format!("mv failed: {e}"), true, cx),
                }
            }
            "cd" => self.flash(format!("Not a folder: {rest}"), true, cx),
            "window" if rest.is_empty() => self.pick_folder(true, window, cx),
            "window" => crate::open_window(resolve(&root, rest), cx),
            "recent" => {
                if rest.is_empty() {
                    return self.open_cmd(CmdMode::Command, "recent ", window, cx);
                }
                let dir = PathBuf::from(rest);
                if dir.is_dir() { self.switch_project(dir, window, cx) } else { self.flash(format!("Not a folder: {rest}"), true, cx) }
            }
            "search" | "grep" | "rg" => {
                let q = rest.to_string();
                self.side = Side::Search;
                self.side_visible = true;
                self.search.update(cx, |s, cx| {
                    s.set_query(&q, cx);
                    s.run_search(cx);
                });
                self.focus_sidebar(window, cx);
            }
            "vsplit" | "vs" | "split" | "sp" => {
                let vertical = cmd.starts_with('v');
                self.split(vertical, window, cx);
                if !rest.is_empty() {
                    self.close_cmd(window, cx);
                    let (path, line) = split_line_suffix(rest);
                    self.open_file(resolve(&root, path), line, false, window, cx);
                }
            }
            "term" | "terminal" => {
                let command = (!rest.is_empty()).then(|| rest.to_string());
                self.open_terminal_view(command, window, cx);
            }
            "close" => self.close_pane(self.focused_pane, window, cx),
            "only" => {
                let others: Vec<usize> = self.panes.iter().map(|p| p.id).filter(|p| *p != self.focused_pane).collect();
                for p in others {
                    self.close_pane(p, window, cx);
                }
            }
            "max" | "maximize" => {
                self.zoomed = !self.zoomed;
                cx.notify();
            }
            "buffers" | "ls" => self.open_cmd(CmdMode::Buffers, "", window, cx),
            "problems" | "errors" => self.show_problems(cx),
            "check" | "tsc" => match rest {
                "off" => {
                    self.settings.checks = false;
                    self.settings.save();
                    self.lint_diags.clear();
                    self.ts_diags.clear();
                    self.push_diagnostics(cx);
                    self.flash("Checks off".into(), false, cx);
                }
                _ => {
                    self.settings.checks = true;
                    self.settings.save();
                    if let Some(p) = self.active_editor().and_then(|e| e.read(cx).path.clone()) {
                        self.check_file(p, cx);
                    }
                    self.flash("Checking…".into(), false, cx);
                }
            },
            "formatonsave" | "fos" => {
                self.settings.format_on_save = match rest {
                    "on" => true,
                    "off" => false,
                    _ => !self.settings.format_on_save,
                };
                self.settings.save();
                let on = self.settings.format_on_save;
                self.flash(format!("Format on save {}", if on { "on (⌘S formats, autosave doesn't)" } else { "off" }), false, cx);
            }
            "autosave" => {
                let on = match rest {
                    "on" => true,
                    "off" => false,
                    _ => !self.settings.autosave,
                };
                self.set_autosave(on, cx);
            }
            "sidebar" => {
                self.settings.sidebar_right = match rest {
                    "right" => true,
                    "left" => false,
                    _ => !self.settings.sidebar_right,
                };
                self.settings.save();
                cx.notify();
            }
            "format" | "fmt" => self.format_file(&FormatFile, window, cx),
            "lint" => self.lint(rest == "." || self.active_editor().is_none(), cx),
            "blame" => self.toggle_blame(&ToggleBlame, window, cx),
            "diff" => match active_rel {
                Some(rel) => self.open_diff(DiffSpec { path: rel, old_path: None, left: Rev::Index, right: Rev::Working, from_commit: None }, false, window, cx),
                None => self.flash("Open a file to diff it".into(), true, cx),
            },
            "stage" | "add" => {
                let paths = args_or_active(rest);
                self.git_panel.update(cx, |p, cx| p.stage(paths, cx));
            }
            "unstage" | "reset" => {
                let paths = args_or_active(rest);
                self.git_panel.update(cx, |p, cx| p.unstage(paths, cx));
            }
            "discard" | "restore" => {
                let paths = args_or_active(rest);
                if paths.is_empty() {
                    return self.flash("Nothing to discard".into(), true, cx);
                }
                let n = paths.len();
                self.repo.update(cx, |r, cx| {
                    r.run_op("Discard", cx, move |root| git::discard(root, &paths).map(|_| format!("Discarded changes in {n} file(s)")))
                });
            }
            "commit" | "ci" => {
                let mut all = false;
                let mut amend = false;
                let mut words: Vec<&str> = rest.split(' ').collect();
                while let Some(w) = words.first() {
                    match *w {
                        "-a" | "--all" => all = true,
                        "--amend" => amend = true,
                        "-am" | "-ma" => all = true,
                        "-m" => {}
                        _ => break,
                    }
                    words.remove(0);
                }
                let msg = words.join(" ").trim().trim_matches('"').to_string();
                if msg.is_empty() && !amend {
                    return self.on_focus_commit(&FocusCommit, window, cx);
                }
                self.git_panel.update(cx, |p, cx| p.commit(Some(msg), all, amend, window, cx));
            }
            "amend" => {
                let msg = rest.to_string();
                self.git_panel.update(cx, |p, cx| p.commit(Some(msg), false, true, window, cx));
            }
            "checkout" | "co" | "switch" => {
                if rest.is_empty() {
                    return self.flash("Usage: checkout <branch>".into(), true, cx);
                }
                let branch = rest.strip_prefix("origin/").filter(|_| rest.starts_with("origin/")).unwrap_or(rest).to_string();
                self.repo.update(cx, |r, cx| r.run_git(vec!["switch".into(), branch], cx));
            }
            "branch-from" => {
                // branch-from <commit> <name>: create a branch at that commit and switch to it.
                let mut parts = rest.split_whitespace();
                match (parts.next(), parts.next()) {
                    (Some(rev), Some(name)) => {
                        let args = vec!["switch".into(), "-c".into(), name.to_string(), rev.to_string()];
                        self.repo.update(cx, |r, cx| r.run_git(args, cx));
                    }
                    _ => self.flash("Type the new branch name after the commit, then Enter".into(), true, cx),
                }
            }
            "branch" => {
                if rest.is_empty() {
                    return self.repo.update(cx, |r, cx| r.run_git(vec!["branch".into(), "-vv".into()], cx));
                }
                self.repo.update(cx, |r, cx| r.run_git(vec!["switch".into(), "-c".into(), rest.to_string()], cx));
            }
            "fetch" => self.repo.update(cx, |r, cx| r.run_git(vec!["fetch".into(), "--all".into(), "--prune".into()], cx)),
            "pull" | "push" | "stash" | "merge" | "rebase" => {
                let mut args = vec![cmd.to_string()];
                args.extend(rest.split_whitespace().map(String::from));
                self.repo.update(cx, |r, cx| r.run_git(args, cx));
            }
            "git" => {
                let args = shell_words(rest);
                self.repo.update(cx, |r, cx| r.run_git(args, cx));
            }
            "graph" | "log" => {
                self.side = Side::Git;
                self.side_visible = true;
                if rest == "all" || rest == "auto" {
                    let all = rest == "all";
                    self.repo.update(cx, |r, cx| {
                        r.graph_all = all;
                        r.refresh_log(cx);
                    });
                }
                self.focus_sidebar(window, cx);
            }
            "files" | "explorer" => self.show_files(&ShowFiles, window, cx),
            "scm" | "sc" => self.show_git(&ShowGit, window, cx),
            "output" => self.toggle_output(&ToggleOutput, window, cx),
            "zoom" => match rest {
                "in" | "+" => self.zoom(1., cx),
                "out" | "-" => self.zoom(-1., cx),
                _ => self.zoom(0., cx),
            },
            "keys" | "help" => {
                self.show_keys(cx);
                self.side = Side::Keys;
                self.side_visible = true;
                cx.notify();
            }
            _ => self.flash(format!("Unknown command: {cmd} (try :keys)"), true, cx),
        }
    }

    fn quit(&mut self, force: bool, cx: &mut Context<Self>) {
        let dirty = self
            .tabs
            .iter()
            .filter(|t| match &t.kind {
                TabKind::Editor(e) => e.read(cx).buf.is_dirty(),
                TabKind::DiffEdit(d) => d.read(cx).right.read(cx).buf.is_dirty(),
                _ => false,
            })
            .count();
        if dirty > 0 && !force {
            return self.flash(format!("{dirty} unsaved file(s) — :w them or :qa! to quit anyway"), true, cx);
        }
        cx.quit();
    }

    // ------------------------------------------------------------ file watching

    fn start_watcher(&mut self, cx: &mut Context<Self>) {
        use notify::{RecursiveMode, Watcher};
        let root = self.root(cx);
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && !matches!(ev.kind, notify::EventKind::Access(_))
            {
                let _ = tx.unbounded_send(ev.paths);
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if watcher.watch(&root, RecursiveMode::Recursive).is_err() {
            return;
        }
        self._watcher = Some(watcher);
        self.watch_task = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut paths: HashSet<PathBuf> = first.into_iter().collect();
                cx.background_executor().timer(Duration::from_millis(150)).await;
                while let Ok(Some(more)) = rx.try_next() {
                    paths.extend(more);
                }
                if this.update(cx, |ws, cx| ws.on_fs_change(paths, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn on_fs_change(&mut self, paths: HashSet<PathBuf>, cx: &mut Context<Self>) {
        let root = self.root(cx);
        let (mut git_meta, mut worktree) = (false, false);
        let mut dirs = HashSet::new();
        let mut changed_files = HashSet::new();
        for p in paths {
            let Ok(rel) = p.strip_prefix(&root) else { continue };
            let rel_s = rel.to_string_lossy();
            if rel.starts_with(".git") {
                if rel_s.ends_with(".lock") || rel_s.contains("/objects/") || rel_s.contains("/logs/") {
                    continue;
                }
                git_meta = true;
                continue;
            }
            if rel.components().any(|c| c.as_os_str() == "node_modules" || c.as_os_str() == "target") {
                continue;
            }
            worktree = true;
            if let Some(parent) = p.parent() {
                dirs.insert(parent.to_path_buf());
            }
            changed_files.insert(p);
        }
        if !(git_meta || worktree) {
            return;
        }
        for tab in &self.tabs {
            match &tab.kind {
                TabKind::Editor(e) => {
                    let path = e.read(cx).path.clone();
                    if path.as_ref().is_some_and(|p| changed_files.contains(p)) {
                        e.update(cx, |e, cx| e.reload_if_clean(cx));
                    } else if git_meta {
                        e.update(cx, |e, cx| e.refresh_git(cx));
                    }
                }
                TabKind::Diff(d) => {
                    let live = {
                        let s = &d.read(cx).spec;
                        matches!(s.left, Rev::Index | Rev::Working) || matches!(s.right, Rev::Index | Rev::Working)
                    };
                    if live {
                        d.update(cx, |d, cx| d.reload(cx));
                    }
                }
                TabKind::DiffEdit(d) => {
                    let path = self.root(cx).join(&d.read(cx).spec.path);
                    if git_meta || changed_files.contains(&path) {
                        d.update(cx, |d, cx| d.refresh(cx));
                    }
                }
                TabKind::Commit(_) | TabKind::Terminal(_) => {}
            }
        }
        self.repo.update(cx, |r, cx| {
            r.refresh_status(cx);
            if git_meta {
                r.refresh_log(cx);
            }
            if worktree {
                r.refresh_files(cx);
            }
        });
        if worktree {
            self.tree.update(cx, |t, cx| t.invalidate(&dirs, cx));
        }
    }

    // ------------------------------------------------------------ rendering

    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let project = repo.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        div()
            .id("titlebar")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(TITLE_H))
            .bg(theme::bg_side())
            .border_b_1()
            .border_color(theme::border())
            .on_click(|ev, window, _| {
                if ev.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(
                div()
                    .id("command-center")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .w(px(380.))
                    .h(px(22.))
                    .rounded(px(6.))
                    .bg(theme::ca(0xffffff0d))
                    .border_1()
                    .border_color(theme::border_input())
                    .text_color(theme::fg_dim())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::ca(0xffffff17)))
                    .on_click(cx.listener(|ws, _, window, cx| ws.find_file(&FindFile, window, cx)))
                    .child(ui::small_icon("search", theme::fg_dim()))
                    .child(project),
            )
            .child(
                div()
                    .absolute()
                    .right(px(10.))
                    .top(px(4.))
                    .flex()
                    .gap(px(2.))
                    .child(ui::icon_button("tb-open", "folder").tooltip(ui::tooltip("Open Folder here (⌘O)")).on_click(cx.listener(|ws, _, w, cx| ws.open_folder(&OpenFolder, w, cx))))
                    .child(ui::icon_button("tb-open-new", "plus").tooltip(ui::tooltip("Open another project in a new window (⌘⇧N) · ⌘` switches")).on_click(cx.listener(|ws, _, w, cx| ws.pick_folder(true, w, cx))))
                    .child(ui::icon_button("tb-side", "split").tooltip(ui::tooltip("Move sidebar left/right (⌘K ⌘B)")).on_click(cx.listener(|ws, _, w, cx| ws.toggle_sidebar_side(&ToggleSidebarSide, w, cx))))
                    .child(ui::icon_button("tb-panel", "inline").tooltip(ui::tooltip("Toggle output panel (⌘J)")).on_click(cx.listener(|ws, _, w, cx| ws.toggle_output(&ToggleOutput, w, cx)))),
            )
    }

    fn render_activity_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let changes = self.repo.read(cx).status.change_count();
        let right = self.settings.sidebar_right;
        let item = |id: &'static str, icon: &'static str, side: Side, tip: &'static str, ws: &Self, cx: &mut Context<Self>| {
            let active = ws.side_visible && ws.side == side;
            div()
                .id(id)
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .w(px(ACTIVITY_W))
                .h(px(40.))
                .cursor_pointer()
                .when(active && !right, |d| d.border_l_2().border_color(theme::fg_bright()))
                .when(active && right, |d| d.border_r_2().border_color(theme::fg_bright()))
                .tooltip(ui::tooltip(tip))
                .on_click(cx.listener(move |ws, _, window, cx| ws.select_side(side, window, cx)))
                .child(ui::icon(icon, if active { theme::fg_bright() } else { theme::fg_muted() }).size(px(19.)))
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(ACTIVITY_W))
            .h_full()
            .bg(theme::bg_side())
            .when(!right, |d| d.border_r_1())
            .when(right, |d| d.border_l_1())
            .border_color(theme::border())
            .child(item("act-files", "files", Side::Files, "Explorer (⇧⌘E)", self, cx))
            .child(item("act-search", "search", Side::Search, "Search (⇧⌘F)", self, cx))
            .child(item("act-git", "git", Side::Git, "Source Control (⌃⇧G)", self, cx).when(changes > 0, |d| {
                d.child(div().absolute().bottom(px(8.)).right(px(6.)).child(ui::badge(changes.to_string(), theme::accent()).text_size(px(10.)).h(px(16.)).min_w(px(16.))))
            }))
            .child(div().flex_1())
            .child(item("act-keys", "keyboard", Side::Keys, "Shortcuts (⌘K ⌘S)", self, cx))
    }

    fn render_layout(&self, node: &Layout, path: String, focused_area: bool, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Layout::Pane(id) => self.render_pane(*id, focused_area, cx),
            Layout::Split { vertical, ratio, a, b } => {
                let vertical = *vertical;
                let cell = self.split_bounds.clone();
                let key = path.clone();
                let first = div()
                    .flex()
                    .flex_none()
                    .min_w_0()
                    .min_h_0()
                    .when(vertical, |d| d.w(relative(*ratio)).h_full())
                    .when(!vertical, |d| d.h(relative(*ratio)).w_full())
                    .child(self.render_layout(a, format!("{path}a"), focused_area, cx));
                let divider = div()
                    .id(SharedString::from(format!("split-{path}")))
                    .flex_none()
                    .when(vertical, |d| d.w(px(4.)).h_full().cursor(CursorStyle::ResizeLeftRight))
                    .when(!vertical, |d| d.h(px(4.)).w_full().cursor(CursorStyle::ResizeUpDown))
                    .hover(|s| s.bg(theme::frame_focused().opacity(0.4)))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |ws, _, _, cx| {
                        ws.dragging_split = Some(key.clone());
                        cx.stop_propagation();
                    }));
                let key = path.clone();
                div()
                    .relative()
                    .flex()
                    .when(!vertical, |d| d.flex_col())
                    .size_full()
                    .child(canvas(move |b, _, _| { cell.borrow_mut().insert(key, b); }, |_, _, _, _| {}).absolute().size_full())
                    .child(first)
                    .child(divider)
                    .child(div().flex().flex_1().min_w_0().min_h_0().child(self.render_layout(b, format!("{path}b"), focused_area, cx)))
                    .into_any_element()
            }
        }
    }

    /// One pane: a rounded frame with its title in the top bar (zellij-style), and its current item.
    fn render_pane(&self, id: usize, focused_area: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(pane) = self.pane(id) else { return div().into_any_element() };
        let focused = focused_area && id == self.focused_pane;
        let active_ix = pane.active.and_then(|t| self.tab_ix(t));
        let count = pane.items.len();
        let pos = pane.active.and_then(|a| pane.items.iter().position(|x| *x == a)).map(|p| p + 1).unwrap_or(0);
        let (title, icon_name, dirty) = active_ix.map(|ix| self.tab_title(&self.tabs[ix], cx)).unwrap_or_else(|| ("empty".into(), String::new(), false));
        let is_term = active_ix.is_some_and(|ix| matches!(self.tabs[ix].kind, TabKind::Terminal(_)));
        let frame = if focused { theme::frame_focused() } else { theme::frame() };
        let cell = self.pane_bounds.clone();
        let content: AnyElement = match active_ix.map(|ix| &self.tabs[ix].kind) {
            Some(TabKind::Editor(e)) => e.clone().into_any_element(),
            Some(TabKind::Diff(d)) => d.clone().into_any_element(),
            Some(TabKind::DiffEdit(d)) => d.clone().into_any_element(),
            Some(TabKind::Commit(c)) => c.clone().into_any_element(),
            Some(TabKind::Terminal(t)) => t.clone().into_any_element(),
            None if self.panes.len() == 1 => self.render_welcome(cx),
            None => self.render_empty_pane(),
        };
        let glyph = |gid: &'static str, icon: &'static str, tip: &'static str| {
            div()
                .id(SharedString::from(format!("{gid}-{id}")))
                .flex()
                .items_center()
                .justify_center()
                .size(px(18.))
                .rounded(px(3.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .tooltip(ui::tooltip(tip))
                .child(ui::small_icon(icon, theme::fg_dim()).size(px(12.)))
        };
        let header = div()
            .id(SharedString::from(format!("pane-header-{id}")))
            .on_click(cx.listener(move |ws, ev: &ClickEvent, _, cx| {
                // Double-click a pane's title bar to maximize / restore it.
                if ev.click_count() == 2 {
                    ws.set_focused_pane(id);
                    ws.zoomed = !ws.zoomed;
                    cx.notify();
                }
            }))
            .flex()
            .flex_none()
            .items_center()
            .h(px(24.))
            .pl(px(8.))
            .pr(px(4.))
            .gap(px(6.))
            .text_size(px(11.))
            .border_b_1()
            .border_color(frame)
            .child(div().text_color(frame).child("─"))
            .child(
                // tmux-style list of what's open in this pane: "1 app.ts  2 $ zsh" (⌘1…⌘9 or click).
                div().flex().items_center().gap(px(2.)).min_w_0().overflow_hidden().children(pane.items.iter().enumerate().filter_map(|(i, tid)| {
                    let ix = self.tab_ix(*tid)?;
                    let (title, icon_name, dirty) = self.tab_title(&self.tabs[ix], cx);
                    let active = pane.active == Some(*tid);
                    let term = matches!(self.tabs[ix].kind, TabKind::Terminal(_));
                    let preview = self.tabs[ix].preview;
                    Some(
                        div()
                            .id(SharedString::from(format!("chip-{id}-{tid}")))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(4.))
                            .px(px(6.))
                            .h(px(18.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .when(active, |d| d.bg(frame.opacity(0.22)).text_color(if focused { theme::fg_bright() } else { theme::fg() }))
                            .when(!active, |d| d.text_color(theme::fg_muted()).hover(|s| s.bg(theme::hover()).text_color(theme::fg())))
                            .on_click(cx.listener({
                                let tid = *tid;
                                move |ws, _, w, cx| {
                                if let Some(ix) = ws.tab_ix(tid) {
                                    ws.activate(ix, w, cx);
                                }
                            }}))
                            .on_mouse_down(MouseButton::Middle, cx.listener({
                                let tid = *tid;
                                move |ws, _, w, cx| {
                                    if let Some(ix) = ws.tab_ix(tid) {
                                        ws.close_tab(ix, false, w, cx);
                                    }
                                }
                            }))
                            .child(div().text_color(if active { theme::frame_focused() } else { theme::fg_muted() }).child(if i < 9 { format!("{}", i + 1) } else { "·".into() }))
                            .child(if term { ui::small_icon("terminal", theme::frame_focused()).size(px(12.)) } else { ui::file_icon(&icon_name).size(px(12.)) })
                            .child(div().max_w(px(180.)).overflow_hidden().whitespace_nowrap().when(preview, |d| d.italic()).child(title.clone()))
                            .when(dirty, |d| d.child(div().text_color(theme::git_modified()).child("●")))
                            .child(
                                div()
                                    .id(SharedString::from(format!("chip-x-{id}-{tid}")))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(14.))
                                    .rounded(px(3.))
                                    .when(!active, |d| d.invisible().group_hover(SharedString::from(format!("chip-{id}-{tid}")), |s| s.visible()))
                                    .hover(|s| s.bg(theme::ca(0xffffff1f)))
                                    .tooltip(ui::tooltip("Close (⌘W · middle-click)"))
                                    .on_click(cx.listener({
                                        let tid = *tid;
                                        move |ws, _, w, cx| {
                                            cx.stop_propagation();
                                            if let Some(ix) = ws.tab_ix(tid) {
                                                ws.close_tab(ix, false, w, cx);
                                            }
                                        }
                                    }))
                                    .child(ui::small_icon("close", theme::fg_dim()).size(px(10.))),
                            )
                            .group(SharedString::from(format!("chip-{id}-{tid}")))
                            .cursor_grab()
                            .on_drag(ui::DraggedItem { tab_id: *tid, label: title }, |d: &ui::DraggedItem, _, _, cx| cx.new(|_| ui::DragChip(d.label.clone()))),
                    )
                })),
            )
            .child(div().flex_1().h(px(1.)).bg(frame.opacity(0.5)))
            .when(count > 1, |d| {
                d.child(glyph("prev", "chevron-left", "Previous file in pane (⌃⇧Tab)").on_click(cx.listener(move |ws, _, w, cx| {
                    ws.set_focused_pane(id);
                    ws.cycle_buffer(-1, w, cx);
                })))
                .child(glyph("next", "chevron-right", "Next file in pane (⌃Tab)").on_click(cx.listener(move |ws, _, w, cx| {
                    ws.set_focused_pane(id);
                    ws.cycle_buffer(1, w, cx);
                })))
            })
            .child(glyph("split-r", "split", "Split right (⌘D)").on_click(cx.listener(move |ws, _, w, cx| {
                ws.set_focused_pane(id);
                ws.split(true, w, cx);
            })))
            .child(glyph("split-d", "inline", "Split down (⌘⇧D)").on_click(cx.listener(move |ws, _, w, cx| {
                ws.set_focused_pane(id);
                ws.split(false, w, cx);
            })))
            .child(glyph("term", "terminal", "Terminal here (⌘T)").on_click(cx.listener(move |ws, _, w, cx| {
                ws.set_focused_pane(id);
                ws.open_terminal_view(None, w, cx);
            })))
            .when(self.panes.len() > 1, |d| {
                d.child(glyph("max", "maximize", "Maximize / restore (⌘⌥Enter · double-click title)").on_click(cx.listener(move |ws, _, _, cx| {
                    ws.set_focused_pane(id);
                    ws.zoomed = !ws.zoomed;
                    cx.notify();
                })))
            })
            .when(active_ix.is_some(), |d| {
                d.child(glyph("close-item", "close", "Close file (⌘W)").on_click(cx.listener(move |ws, _, w, cx| {
                    ws.set_focused_pane(id);
                    if let Some(a) = ws.active {
                        ws.close_tab(a, false, w, cx);
                    }
                })))
            })
            .when(active_ix.is_none() && self.panes.len() > 1, |d| {
                d.child(glyph("close-pane", "close", "Close pane (⌘⇧W)").on_click(cx.listener(move |ws, _, w, cx| ws.close_pane(id, w, cx))))
            });
        div()
            .size_full()
            .p(px(2.))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(frame)
                    .bg(theme::bg_editor())
                    .overflow_hidden()
                    .capture_any_mouse_down(cx.listener(move |ws, _, _, cx| {
                        if ws.focused_pane != id {
                            ws.set_focused_pane(id);
                            cx.notify();
                        }
                    }))
                    .child(canvas(move |b, _, _| { cell.borrow_mut().insert(id, b); }, |_, _, _, _| {}).absolute().size_full())
                    .on_drag_move(cx.listener(move |ws, ev: &DragMoveEvent<ui::DraggedItem>, _, cx| ws.update_drop_hint(id, ev.bounds, ev.event.position, cx)))
                    .on_drag_move(cx.listener(move |ws, ev: &DragMoveEvent<ui::DraggedFile>, _, cx| ws.update_drop_hint(id, ev.bounds, ev.event.position, cx)))
                    .on_drop(cx.listener(move |ws, d: &ui::DraggedItem, w, cx| {
                        let zone = ws.drop_hint.filter(|(p, _)| *p == id).map(|(_, z)| z).unwrap_or(DropZone::Center);
                        ws.move_tab(d.tab_id, id, zone, w, cx);
                    }))
                    .on_drop(cx.listener(move |ws, d: &ui::DraggedFile, w, cx| {
                        let zone = ws.drop_hint.filter(|(p, _)| *p == id).map(|(_, z)| z).unwrap_or(DropZone::Center);
                        ws.drop_file(d.path.clone(), id, zone, w, cx);
                    }))
                    .child(header)
                    .child(div().flex_1().min_h_0().child(content))
                    .when_some(self.drop_hint.filter(|(p, _)| *p == id).map(|(_, z)| z), |d, zone| {
                        // Shows where the drop lands: the whole pane, or the half that becomes a new split.
                        let overlay = div().absolute().bg(theme::frame_focused().opacity(0.16)).border_2().border_color(theme::frame_focused()).rounded(px(4.));
                        let overlay = match zone {
                            DropZone::Center => overlay.inset_0(),
                            DropZone::Left => overlay.top_0().bottom_0().left_0().w(relative(0.5)),
                            DropZone::Right => overlay.top_0().bottom_0().right_0().w(relative(0.5)),
                            DropZone::Top => overlay.left_0().right_0().top_0().h(relative(0.5)),
                            DropZone::Bottom => overlay.left_0().right_0().bottom_0().h(relative(0.5)),
                        };
                        d.child(overlay)
                    }),
            )
            .into_any_element()
    }

    fn update_drop_hint(&mut self, id: usize, bounds: Bounds<Pixels>, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if !bounds.contains(&pos) {
            if self.drop_hint.is_some_and(|(p, _)| p == id) {
                self.drop_hint = None;
                cx.notify();
            }
            return;
        }
        let hint = Some((id, DropZone::at(bounds, pos)));
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    fn render_empty_pane(&self) -> AnyElement {
        let row = |keys: &str, label: &str| {
            div()
                .flex()
                .gap(px(12.))
                .child(div().w(px(70.)).flex().justify_end().text_color(theme::frame_focused()).child(keys.to_string()))
                .child(div().child(label.to_string()))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_color(theme::fg_muted())
            .child(row("⌘P", "open a file here"))
            .child(row("⌘T", "terminal here"))
            .child(row("⌘D ⌘⇧D", "split again"))
            .child(row(":e", "open by path"))
            .child(row("⌘⇧W", "close this pane"))
            .into_any_element()
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let screen = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(9.))
            .bg(theme::bg_editor())
            .text_color(theme::fg_muted());

        if self.has_project {
            // A project is open: just the keys to get going.
            let row = |label: &str, keys: &str| {
                div()
                    .flex()
                    .gap(px(16.))
                    .child(div().w(px(160.)).flex().justify_end().child(label.to_string()))
                    .child(div().w(px(130.)).text_color(theme::fg()).child(keys.to_string()))
            };
            return screen
                .child(row("Find File", "⌘ P"))
                .child(row("Command Line", "⌘ ⇧ P  or  :"))
                .child(row("Source Control", "⌃ ⇧ G"))
                .child(row("Search in Files", "⌘ ⇧ F"))
                .child(row("Terminal", "⌘ T  ·  ⌃ `"))
                .child(row("Split Right / Down", "⌘ D  ·  ⌘ ⇧ D"))
                .child(row("Toggle Blame", "⌥ ⌘ B"))
                .child(row("All Shortcuts", "⌘K ⌘S"))
                .into_any_element();
        }

        // No project yet: the logo, Open Folder and Recent.
        static WORDMARK: std::sync::OnceLock<Arc<gpui::Image>> = std::sync::OnceLock::new();
        let wordmark = WORDMARK
            .get_or_init(|| Arc::new(gpui::Image::from_bytes(ImageFormat::Png, include_bytes!("../packaging/wordmark.png").to_vec())))
            .clone();
        let home = std::env::var("HOME").unwrap_or_default();
        let is_temp = |f: &Path| f.starts_with(std::env::temp_dir()) || f.starts_with("/private/tmp") || f.starts_with("/tmp") || f.starts_with("/private/var/folders");
        let recent: Vec<PathBuf> = self.settings.recent_folders.iter().filter(|f| f.is_dir() && !is_temp(f)).take(8).cloned().collect();
        screen
            .child(img(wordmark).w(px(250.)).h(px(190.)))
            .child(div().mb(px(22.)).text_size(px(12.)).text_color(theme::fg_dim()).child("OSKL Code"))
            .child(
                div()
                    .id("welcome-open")
                    .px(px(16.))
                    .py(px(7.))
                    .rounded(px(5.))
                    .bg(theme::c(0x5ee7c4))
                    .text_color(theme::bg_app())
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .on_click(cx.listener(|ws, _, w, cx| ws.open_folder(&OpenFolder, w, cx)))
                    .child("Open Folder…   ⌘O"),
            )
            .when(!recent.is_empty(), |d| d.child(div().mt(px(22.)).mb(px(4.)).text_size(px(10.5)).child("RECENT")))
            .children(recent.into_iter().enumerate().map(|(i, f)| {
                let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let parent = f.parent().map(|p| p.to_string_lossy().replacen(&home, "~", 1)).unwrap_or_default();
                div()
                    .id(("recent", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .w(px(360.))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |ws, _, w, cx| ws.switch_project(f.clone(), w, cx)))
                    .child(ui::folder_icon(&name, false))
                    .child(div().text_color(theme::fg_bright()).child(name))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_muted()).child(parent))
            }))
            .into_any_element()
    }

    fn render_output(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.output.len();
        let page = ((self.output_h - 40.) / 18.).max(1.) as isize;
        div()
            .key_context("Output")
            .track_focus(&self.output_focus)
            .on_action(cx.listener(|ws, _: &OutputDown, _, cx| ws.output_move(1, cx)))
            .on_action(cx.listener(|ws, _: &OutputUp, _, cx| ws.output_move(-1, cx)))
            .on_action(cx.listener(move |ws, _: &OutputPageDown, _, cx| ws.output_move(page, cx)))
            .on_action(cx.listener(move |ws, _: &OutputPageUp, _, cx| ws.output_move(-page, cx)))
            .on_action(cx.listener(|ws, _: &OutputTop, _, cx| ws.output_move(isize::MIN / 2, cx)))
            .on_action(cx.listener(|ws, _: &OutputBottom, _, cx| ws.output_move(isize::MAX / 2, cx)))
            .on_action(cx.listener(|ws, _: &OutputOpen, w, cx| ws.output_open(w, cx)))
            .on_action(cx.listener(|ws, _: &OutputLeave, w, cx| ws.focus_active(w, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, w, _| w.focus(&ws.output_focus)))
            .flex()
            .flex_col()
            .flex_none()
            .h(px(self.output_h))
            .bg(theme::bg_editor())
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("output-resize")
                    .h(px(4.))
                    .mt(px(-2.))
                    .cursor(CursorStyle::ResizeUpDown)
                    .hover(|s| s.bg(theme::focus_border()))
                    .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, _, cx| {
                        ws.resizing_output = true;
                        cx.stop_propagation();
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .px(px(12.))
                    .gap(px(8.))
                    .text_size(px(10.5))
                    .child(div().border_b_1().border_color(theme::fg()).child("OUTPUT"))
                    .child(div().flex_1().text_color(theme::fg_dim()).overflow_hidden().whitespace_nowrap().child(self.output_title.clone()))
                    .child(ui::icon_button("output-close", "close").on_click(cx.listener(|ws, _, _, cx| {
                        ws.output_visible = false;
                        cx.notify();
                    }))),
            )
            .child(
                uniform_list(
                    "output",
                    count,
                    cx.processor(|ws, range: Range<usize>, _window, cx| {
                        range
                            .map(|ix| {
                                let line = &ws.output[ix];
                                let loc = line.loc.clone();
                                let selected = ix == ws.output_sel && ws.output_focus.is_focused(_window);
                                div()
                                    .id(("out", ix))
                                    .when(selected, |d| d.bg(theme::selected_focused()))
                                    .px(px(12.))
                                    .h(px(18.))
                                    .whitespace_nowrap()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(11.))
                                    .text_color(if line.err { theme::git_deleted() } else if loc.is_some() { theme::fg() } else { theme::fg_dim() })
                                    .when_some(loc, |d, (file, row, _col)| {
                                        d.cursor_pointer().hover(|s| s.underline()).on_click(cx.listener(move |ws, _, window, cx| {
                                            let root = ws.root(cx);
                                            ws.open_file(resolve(&root, &file), Some(row), false, window, cx);
                                        }))
                                    })
                                    .child(line.text.clone())
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.output_scroll.clone())
                .flex_1(),
            )
    }

    fn render_status(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let base = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(STATUS_H))
            .bg(theme::bg_side())
            .border_t_1()
            .border_color(theme::border())
            .text_size(px(11.));

        if let Some(mode) = self.cmd_mode {
            let prefix = match mode {
                CmdMode::Command => ":",
                CmdMode::Search => "/",
                CmdMode::Files => "›",
                CmdMode::Buffers => "≡",
            };
            let hint = match mode {
                CmdMode::Command => "Tab complete · ↑↓ history · Esc cancel",
                CmdMode::Search => "Enter keep · ⌘G next · Esc cancel",
                CmdMode::Files => "↑↓ select · Enter open here · Esc cancel",
                CmdMode::Buffers => "↑↓ select · Enter show · Esc cancel",
            };
            return base
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .h(px(STATUS_H + 4.))
                .bg(theme::bg_editor())
                .border_color(theme::focus_border())
                .child(div().w(px(24.)).flex().justify_center().font_family(theme::MONO_FONT).text_color(theme::accent()).child(prefix))
                .child(div().flex_1().min_w_0().font_family(theme::MONO_FONT).child(self.cmd.clone()))
                .child(div().px(px(10.)).text_color(theme::fg_muted()).child(hint));
        }

        let branch = repo.branch_label();
        let st = &repo.status;
        let sync = match (st.behind, st.ahead) {
            (0, 0) if st.upstream.is_some() => String::new(),
            (b, a) if st.upstream.is_some() => format!("{b}↓ {a}↑"),
            _ => String::new(),
        };
        let editor_info = self.active_editor().map(|e| {
            let e = e.read(cx);
            let (row, col) = e.cursor_rc();
            let sel = e.buf.sel.range().len();
            let indent = if e.buf.indent.starts_with('\t') { "Tabs".to_string() } else { format!("Spaces: {}", e.buf.indent.len()) };
            let lang = e.path.as_ref().and_then(|p| p.extension()).map(|x| x.to_string_lossy().to_uppercase()).unwrap_or("Plain Text".into());
            (
                if sel > 0 { format!("Ln {}, Col {} ({sel} selected)", row + 1, col + 1) } else { format!("Ln {}, Col {}", row + 1, col + 1) },
                indent,
                if e.buf.crlf { "CRLF" } else { "LF" },
                lang,
            )
        });
        let item = |id: &'static str| div().id(id).flex().items_center().gap(px(4.)).px(px(8.)).h_full().cursor_pointer().hover(|s| s.bg(theme::hover()));
        let _ = window;
        let chip = |label: String, bg: Hsla| div().flex().items_center().h_full().px(px(8.)).bg(bg).text_color(theme::bg_app()).font_weight(FontWeight::BOLD).child(label);
        let hint = |key: &str, label: &str| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(6.))
                .child(div().px(px(4.)).rounded(px(3.)).bg(theme::badge()).text_color(theme::fg_bright()).child(key.to_string()))
                .child(div().text_color(theme::fg_muted()).child(label.to_string()))
        };
        let pane_count = self.panes.len();
        base.child(chip(if self.zoomed { " oskl · MAX ".into() } else { " oskl ".into() }, theme::frame_focused()))
        .child(
            item("status-branch")
                .when(!branch.is_empty(), |d| d.child(ui::small_icon("git", theme::fg())).child(branch.clone()))
                .on_click(cx.listener(|ws, _, window, cx| ws.open_cmd(CmdMode::Command, "checkout ", window, cx))),
        )
        .when(!sync.is_empty(), |d| {
            d.child(
                item("status-sync")
                    .child(ui::small_icon("sync", theme::fg()))
                    .child(sync)
                    .on_click(cx.listener(|ws, _, _, cx| ws.repo.update(cx, |r, cx| r.run_git(vec!["pull".into()], cx)))),
            )
        })
        .when_some(repo.busy.clone(), |d, busy| d.child(div().px(px(8.)).text_color(theme::fg_dim()).child(format!("⟳ {busy}"))))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px(px(8.))
                .overflow_hidden()
                .whitespace_nowrap()
                .when_some(self.message.clone(), |d, (msg, err)| d.text_color(if err { theme::git_deleted() } else { theme::fg() }).child(msg)),
        )
        .when(self.message.is_none(), |d| {
            d.child(hint("⌘P", "file"))
                .child(hint("⌘D", "split"))
                .child(hint("⌘T", "term"))
                .child(hint(":", "cmd"))
                .when(pane_count > 1, |d| d.child(hint("⌘⌥ jkl;", "split")))
        })
        .child({
            let (e, w) = self.problem_counts();
            item("status-problems")
                .tooltip(ui::tooltip("Problems (:problems) — lint on save, TypeScript check on save"))
                .child(ui::small_icon("error", if e > 0 { Severity::Error.color() } else { theme::fg_muted() }))
                .child(e.to_string())
                .child(ui::small_icon("warning", if w > 0 { Severity::Warning.color() } else { theme::fg_muted() }))
                .child(w.to_string())
                .when(self.tsc_running, |d| d.child(div().text_color(theme::fg_muted()).child("tsc…")))
                .on_click(cx.listener(|ws, _, _, cx| ws.show_problems(cx)))
        })
        .when_some(editor_info, |d, (pos, indent, eol, lang)| {
            d.child(item("status-pos").child(pos).on_click(cx.listener(|ws, _, w, cx| ws.go_to_line(&GoToLine, w, cx))))
                .child(item("status-indent").child(indent))
                .child(item("status-enc").child("UTF-8"))
                .child(item("status-eol").child(eol))
                .child(item("status-lang").child(lang))
                .child(
                    item("status-blame")
                        .child(ui::small_icon("blame", theme::fg()))
                        .child("Blame")
                        .on_click(cx.listener(|ws, _, w, cx| ws.toggle_blame(&ToggleBlame, w, cx))),
                )
        })
        .child({
            let on = self.settings.autosave;
            item("status-autosave")
                .tooltip(ui::tooltip("Auto save after 1s (click to toggle, :autosave on|off)"))
                .child(ui::small_icon(if on { "check" } else { "close" }, if on { theme::git_added() } else { theme::fg_muted() }))
                .child("Auto Save")
                .on_click(cx.listener(|ws, _, _, cx| {
                    let on = !ws.settings.autosave;
                    ws.set_autosave(on, cx);
                }))
        })
        .child(
            item("status-keys")
                .child(ui::small_icon("terminal", theme::fg()))
                .on_click(cx.listener(|ws, _, w, cx| ws.open_cmd(CmdMode::Command, "", w, cx))),
        )
    }

    fn render_cmd_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.cmd_mode.is_none() || self.cmd_items.is_empty() {
            return None;
        }
        let max = 12;
        let start = self.cmd_sel.saturating_sub(max - 1);
        let rows = self.cmd_items.iter().enumerate().skip(start).take(max).map(|(ix, item)| {
            let selected = ix == self.cmd_sel;
            let highlights: Vec<(Range<usize>, HighlightStyle)> = item
                .positions
                .iter()
                .filter(|p| **p < item.label.len())
                .map(|&p| {
                    let end = p + item.label[p..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    (p..end, HighlightStyle { color: Some(theme::c(0x2aaaff)), font_weight: Some(FontWeight::BOLD), ..Default::default() })
                })
                .collect();
            let is_file = self.cmd_mode == Some(CmdMode::Files);
            div()
                .id(("cmd-item", ix))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(24.))
                .px(px(10.))
                .rounded(px(3.))
                .cursor_pointer()
                .when(selected, |d| d.bg(theme::selected_focused()))
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.cmd_sel = ix;
                    ws.cmd_nav = true;
                    ws.submit_cmd(window, cx);
                }))
                .when(is_file, |d| d.child(ui::file_icon(&item.label)))
                .child(div().flex_none().text_color(theme::fg_bright()).child(StyledText::new(item.label.clone()).with_highlights(highlights)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_muted()).child(item.detail.clone()))
        });
        Some(
            div()
                .absolute()
                .bottom(px(STATUS_H + 4.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("cmd-popup")
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(640.))
                        .p(px(4.))
                        .mb(px(4.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .rounded(px(6.))
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}

fn split_line_suffix(s: &str) -> (&str, Option<usize>) {
    match s.rsplit_once(':') {
        Some((p, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => (p, n.parse().ok()),
        _ => (s, None),
    }
}

fn resolve(root: &Path, p: &str) -> PathBuf {
    let p = p.trim();
    if let Some(home) = p.strip_prefix("~/") {
        return std::env::var("HOME").map(|h| PathBuf::from(h).join(home)).unwrap_or_else(|_| root.join(p));
    }
    let path = Path::new(p);
    if path.is_absolute() { path.to_path_buf() } else { root.join(path) }
}

fn path_items(root: &Path, head: &str, prefix_args: &str, arg: &str) -> Vec<CmdItem> {
    let (dir_part, name_prefix) = match arg.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n),
        None => (String::new(), arg),
    };
    let base = resolve(root, if dir_part.is_empty() { "." } else { &dir_part });
    let Ok(rd) = std::fs::read_dir(&base) else { return Vec::new() };
    let lower = name_prefix.to_lowercase();
    let mut entries: Vec<(bool, String)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if (name.starts_with('.') && !name_prefix.starts_with('.')) || name == ".git" {
                return None;
            }
            name.to_lowercase().starts_with(&lower).then(|| (e.path().is_dir(), name))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    entries
        .into_iter()
        .take(40)
        .map(|(is_dir, name)| {
            let path = format!("{dir_part}{name}{}", if is_dir { "/" } else { "" });
            CmdItem {
                label: format!("{name}{}", if is_dir { "/" } else { "" }),
                detail: dir_part.clone(),
                value: format!("{head} {prefix_args}{path}"),
                positions: (0..name_prefix.len()).collect(),
                exec: !is_dir,
            }
        })
        .collect()
}

/// Splits a command line into words, honoring simple quotes.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(ch),
            (None, ' ') => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds_cell = self.window_bounds.clone();
        let sidebar: AnyElement = match self.side {
            Side::Files => self.tree.clone().into_any_element(),
            Side::Git => self.git_panel.clone().into_any_element(),
            Side::Search => self.search.clone().into_any_element(),
            Side::Keys => self.keys.clone().into_any_element(),
        };
        let right = self.settings.sidebar_right;
        let side_block = self.side_visible.then(|| {
            let handle = div()
                .id("side-resize")
                .flex_none()
                .w(px(4.))
                .h_full()
                .when(!right, |d| d.ml(px(-2.)).border_l_1())
                .when(right, |d| d.mr(px(-2.)).border_r_1())
                .border_color(theme::border())
                .cursor(CursorStyle::ResizeLeftRight)
                .hover(|s| s.bg(theme::focus_border()))
                .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, _, cx| {
                    ws.resizing_side = true;
                    cx.stop_propagation();
                }));
            let panel = div().flex_none().w(px(self.side_w)).h_full().min_w_0().overflow_hidden().px(px(5.)).text_size(px(11.)).bg(theme::bg_side()).child(sidebar);
            if right { div().flex().h_full().child(handle).child(panel) } else { div().flex().h_full().child(panel).child(handle) }
        });
        let main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(div().flex().flex_1().min_h_0().p(px(2.)).child(if self.zoomed {
                self.render_pane(self.focused_pane, true, cx)
            } else {
                let layout = self.layout.clone();
                self.render_layout(&layout, String::new(), true, cx)
            }))
            .when(self.output_visible, |d| d.child(self.render_output(cx)))
            .into_any_element();
        let activity = self.render_activity_bar(cx).into_any_element();
        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" && ws.cmd_mode.is_some() {
                    ws.cancel_cmd(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, window, cx| {
                // Clicks inside the popup / command bar stop propagation; anything else dismisses it.
                // Leave focus wherever the click put it.
                let _ = window;
                if ws.cmd_mode.take().is_some() {
                    ws.cmd_items.clear();
                    ws.return_focus = None;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(Self::find_file))
            .on_action(cx.listener(Self::command_palette))
            .on_action(cx.listener(Self::find_in_file))
            .on_action(cx.listener(|ws, _: &FindNext, _, cx| ws.find_step(true, cx)))
            .on_action(cx.listener(|ws, _: &FindPrev, _, cx| ws.find_step(false, cx)))
            .on_action(cx.listener(Self::show_files))
            .on_action(cx.listener(Self::show_git))
            .on_action(cx.listener(Self::search_all))
            .on_action(cx.listener(Self::show_keys_panel))
            .on_action(cx.listener(Self::toggle_sidebar_side))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::open_folder_new_window))
            .on_action(cx.listener(Self::next_window))
            .on_action(cx.listener(|ws, _: &SplitRight, w, cx| ws.split(true, w, cx)))
            .on_action(cx.listener(|ws, _: &SplitDown, w, cx| ws.split(false, w, cx)))
            .on_action(cx.listener(|ws, _: &ClosePane, w, cx| ws.close_pane(ws.focused_pane, w, cx)))
            .on_action(cx.listener(|ws, _: &ZoomPane, _, cx| {
                ws.zoomed = !ws.zoomed;
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &GoBack, w, cx| ws.navigate(true, w, cx)))
            .on_action(cx.listener(|ws, _: &GoForward, w, cx| ws.navigate(false, w, cx)))
            .on_action(cx.listener(|ws, _: &MoveItemLeft, w, cx| ws.move_item_dir(-1, 0, w, cx)))
            .on_action(cx.listener(|ws, _: &MoveItemRight, w, cx| ws.move_item_dir(1, 0, w, cx)))
            .on_action(cx.listener(|ws, _: &MoveItemUp, w, cx| ws.move_item_dir(0, -1, w, cx)))
            .on_action(cx.listener(|ws, _: &MoveItemDown, w, cx| ws.move_item_dir(0, 1, w, cx)))
            .on_action(cx.listener(|ws, _: &FocusLeft, w, cx| ws.focus_dir(-1, 0, w, cx)))
            .on_action(cx.listener(|ws, _: &FocusRight, w, cx| ws.focus_dir(1, 0, w, cx)))
            .on_action(cx.listener(|ws, _: &FocusUp, w, cx| ws.focus_dir(0, -1, w, cx)))
            .on_action(cx.listener(|ws, _: &FocusDown, w, cx| ws.focus_dir(0, 1, w, cx)))
            .on_action(cx.listener(|ws, _: &NewTerminal, w, cx| ws.new_terminal(w, cx)))
            .on_action(cx.listener(|ws, _: &ToggleTerminal, w, cx| ws.toggle_terminal(w, cx)))
            .on_action(cx.listener(|ws, _: &PaneBuffers, w, cx| ws.open_cmd(CmdMode::Buffers, "", w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer1, w, cx| ws.show_buffer(0, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer2, w, cx| ws.show_buffer(1, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer3, w, cx| ws.show_buffer(2, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer4, w, cx| ws.show_buffer(3, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer5, w, cx| ws.show_buffer(4, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer6, w, cx| ws.show_buffer(5, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer7, w, cx| ws.show_buffer(6, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer8, w, cx| ws.show_buffer(7, w, cx)))
            .on_action(cx.listener(|ws, _: &Buffer9, w, cx| ws.show_buffer(usize::MAX, w, cx)))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_output))
            .on_action(cx.listener(Self::on_focus_editor))
            .on_action(cx.listener(Self::on_focus_sidebar))
            .on_action(cx.listener(Self::on_focus_commit))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_active))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::format_file))
            .on_action(cx.listener(Self::toggle_blame))
            .on_action(cx.listener(Self::go_to_line))
            .on_action(cx.listener(Self::on_run_shell))
            .on_action(cx.listener(|ws, _: &ZoomIn, _, cx| ws.zoom(1., cx)))
            .on_action(cx.listener(|ws, _: &ZoomOut, _, cx| ws.zoom(-1., cx)))
            .on_action(cx.listener(|ws, _: &ShowKeys, _, cx| ws.show_keys(cx)))
            .on_action(cx.listener(|ws, _: &Lint, _, cx| ws.lint(false, cx)))
            .on_action(cx.listener(|ws, _: &Quit, _, cx| ws.quit(false, cx)))
            .on_mouse_move(cx.listener(|ws, ev: &MouseMoveEvent, _, cx| {
                if let Some(path) = ws.dragging_split.clone() {
                    if !ev.dragging() {
                        ws.dragging_split = None;
                        return;
                    }
                    let Some(b) = ws.split_bounds.borrow().get(&path).copied() else { return };
                    if let Some(Layout::Split { vertical, ratio, .. }) = ws.layout.at_path(&path) {
                        let r = if *vertical {
                            (ev.position.x - b.left()) / b.size.width
                        } else {
                            (ev.position.y - b.top()) / b.size.height
                        };
                        *ratio = r.clamp(0.1, 0.9);
                        cx.notify();
                    }
                    return;
                }
                if !(ws.resizing_side || ws.resizing_output) {
                    return;
                }
                if !ev.dragging() {
                    ws.resizing_side = false;
                    ws.resizing_output = false;
                    return;
                }
                let b = ws.window_bounds.get();
                if ws.resizing_side {
                    let w = if ws.settings.sidebar_right { (b.right() - ev.position.x) / px(1.) } else { (ev.position.x - b.left()) / px(1.) };
                    ws.side_w = (w - ACTIVITY_W).clamp(170., 900.);
                }
                if ws.resizing_output {
                    ws.output_h = ((b.bottom() - ev.position.y) / px(1.) - STATUS_H).clamp(80., (b.size.height / px(1.)) * 0.8);
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|ws, _, _, _| {
                ws.drop_hint = None;
                ws.dragging_split = None;
                ws.resizing_side = false;
                ws.resizing_output = false;
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .text_color(theme::fg())
            .font_family(theme::UI_FONT)
            .text_size(theme::ui_text())
            .child(canvas(move |b, _, _| bounds_cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(self.render_titlebar(cx))
            .child(if right {
                div().flex().flex_1().min_h_0().child(main).children(side_block).child(activity)
            } else {
                div().flex().flex_1().min_h_0().child(activity).children(side_block).child(main)
            })
            .child(self.render_status(window, cx))
            .children(self.render_cmd_popup(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{DropZone, Layout};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn drop_zones_follow_the_cursor() {
        let b = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(200.)));
        assert_eq!(DropZone::at(b, point(px(200.), px(100.))), DropZone::Center);
        assert_eq!(DropZone::at(b, point(px(20.), px(100.))), DropZone::Left);
        assert_eq!(DropZone::at(b, point(px(390.), px(100.))), DropZone::Right);
        assert_eq!(DropZone::at(b, point(px(200.), px(10.))), DropZone::Top);
        assert_eq!(DropZone::at(b, point(px(200.), px(190.))), DropZone::Bottom);
    }

    #[test]
    fn split_depth_follows_halving() {
        use super::MAX_SPLIT_DEPTH;
        // half | (quarter / quarter): quarters are at depth 2, can be split once more.
        let l = Layout::Split {
            vertical: true,
            ratio: 0.5,
            a: Box::new(Layout::Pane(0)),
            b: Box::new(Layout::Split { vertical: false, ratio: 0.5, a: Box::new(Layout::Pane(1)), b: Box::new(Layout::Pane(2)) }),
        };
        assert_eq!(l.depth(0), Some(1));
        assert_eq!(l.depth(1), Some(2));
        assert!(l.depth(1).unwrap() < MAX_SPLIT_DEPTH);
        assert_eq!(l.max_depth(), 2);
    }

    #[test]
    fn layout_split_and_remove() {
        let mut l = Layout::Pane(0);
        l.replace_pane(0, Layout::Split { vertical: true, ratio: 0.5, a: Box::new(Layout::Pane(1)), b: Box::new(Layout::Pane(0)) });
        let mut ids = Vec::new();
        l.panes(&mut ids);
        assert_eq!(ids, vec![1, 0]);
        assert!(l.remove_pane(1));
        assert!(matches!(l, Layout::Pane(0)));
    }
}

/// An empty placeholder folder used when the app starts without a project.
pub fn no_project_dir() -> PathBuf {
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(".config/kite/no-project");
    std::fs::create_dir_all(&dir).ok();
    dir.canonicalize().unwrap_or(dir)
}

fn root_is_project(root: &Path) -> bool {
    root != no_project_dir()
}
