//! Source Control panel: commit box, staged/unstaged changes and the commit graph.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::{self, Change, FileStatus, Rev};
use crate::graph::GraphRow;
use crate::repo::Repo;
use crate::request::{DiffSpec, Request};
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
// Sizes follow the interface zoom.
use crate::ui::px;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

#[path = "git_refs.rs"]
pub mod refs;
#[path = "git_commands.rs"]
mod commands;
#[path = "git_view.rs"]
pub mod view;
#[path = "git_pr.rs"]
pub mod pr;
pub use commands::COMMANDS;

actions!(git_panel, [Down, Up, Open, ToggleStage, Discard, OpenFile, FocusCommit, Refresh, Top, Bottom, Expand, Collapse]);

pub fn bind_keys() {
    view::bind_keys();
    // Not while typing a commit message: s, d, c… are letters there.
    let c = Some("GitPanel && !typing");
    crate::keybindings::collect([
        crate::keybindings::kb("{down}", Down, c),
        crate::keybindings::kb("down", Down, c),
        crate::keybindings::kb("{up}", Up, c),
        crate::keybindings::kb("up", Up, c),
        crate::keybindings::kb("enter", Open, c),
        crate::keybindings::kb("space", ToggleStage, c),
        crate::keybindings::kb("s", ToggleStage, c),
        crate::keybindings::kb("d", Discard, c),
        crate::keybindings::kb("o", OpenFile, c),
        crate::keybindings::kb("c", FocusCommit, c),
        crate::keybindings::kb("r", Refresh, c),
        crate::keybindings::kb("g", Top, c),
        crate::keybindings::kb("shift-g", Bottom, c),
        // A commit in the graph: show or hide the files it changed.
        crate::keybindings::kb("right", Expand, c),
        crate::keybindings::kb("left", Collapse, c),
    ]);
}

#[derive(Clone, PartialEq, Eq)]
enum Item {
    File { staged: bool, path: String },
    Commit(usize),
    /// A file listed under an expanded commit: (commit, file).
    CommitFile(usize, usize),
}

#[derive(Clone)]
enum ListRow {
    Section { staged: bool, conflicts: bool, count: usize },
    /// `label` is the folder name, or a compacted chain like "src / lib" (VS Code style).
    Folder { staged: bool, key: String, label: String, depth: usize, open: bool, paths: Vec<String> },
    File { staged: bool, change: Change, depth: usize },
}

/// A row of the graph list: a commit, or one of the files an expanded commit changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphItem {
    Commit(usize),
    /// (commit, file index in its `CommitFiles`).
    File(usize, usize),
    /// An expanded commit whose files are still being read.
    Loading(usize),
}

/// What a commit changed, read when it's first expanded.
struct CommitFiles {
    parent: Option<String>,
    files: Vec<Change>,
}

/// Which loaded commits the checkout contains (HEAD and its ancestors), plus HEAD's row.
fn in_checkout(commits: &[git::Commit], head: &str) -> (Vec<bool>, Option<usize>) {
    let index: std::collections::HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();
    let mut seen = vec![false; commits.len()];
    let head_ix = index.get(head).copied();
    let mut stack: Vec<usize> = head_ix.into_iter().collect();
    while let Some(i) = stack.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        stack.extend(commits[i].parents.iter().filter_map(|p| index.get(p.as_str()).copied()));
    }
    (seen, head_ix)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscardWhat {
    All,
    TrackedOnly,
}

struct DiscardPrompt {
    message: String,
    detail: String,
    /// Buttons besides Cancel; the last one is the default.
    buttons: Vec<(String, DiscardWhat)>,
}

/// The confirmation VS Code shows for discarding, for tracked and/or untracked files.
fn discard_prompt(tracked: &[String], untracked: &[String]) -> Option<DiscardPrompt> {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let name = |p: &String| format!("'{}'", ui::file_name(p));
    const LOST: &str = "This is IRREVERSIBLE!\nYour current working set will be FOREVER LOST if you proceed.";
    let discard_q = match tracked {
        [one] => format!("Are you sure you want to discard changes in {}?", name(one)),
        many => format!("Are you sure you want to discard ALL changes in {} files?", many.len()),
    };
    let delete_q = match untracked {
        [one] => format!("Are you sure you want to DELETE {}?", name(one)),
        many => format!("Are you sure you want to DELETE the {} untracked files?", many.len()),
    };
    let trash_note = if untracked.len() == 1 { "You can restore this file from the Trash." } else { "You can restore these files from the Trash." };
    match (tracked.len(), untracked.len()) {
        (0, 0) => None,
        (t, 0) => Some(DiscardPrompt {
            message: discard_q,
            detail: LOST.into(),
            buttons: vec![(if t == 1 { "Discard File".into() } else { format!("Discard {t} Files") }, DiscardWhat::All)],
        }),
        (0, u) => Some(DiscardPrompt {
            message: delete_q,
            detail: trash_note.into(),
            buttons: vec![(if u == 1 { "Move to Trash".into() } else { format!("Move {u} Files to Trash") }, DiscardWhat::All)],
        }),
        (t, u) => Some(DiscardPrompt {
            message: format!("Are you sure you want to DELETE the {u} untracked file{}? {trash_note}", plural(u)),
            detail: format!("{discard_q}\n\n{LOST}"),
            buttons: vec![
                (format!("Discard All {} Files", t + u), DiscardWhat::All),
                (format!("Discard {t} Tracked File{}", plural(t)), DiscardWhat::TrackedOnly),
            ],
        }),
    }
}

/// Folder of the changes tree.
#[derive(Default)]
struct Node {
    dirs: std::collections::BTreeMap<String, Node>,
    files: Vec<Change>,
}

impl Node {
    fn all_paths(&self, out: &mut Vec<String>) {
        for d in self.dirs.values() {
            d.all_paths(out);
        }
        out.extend(self.files.iter().map(|c| c.path.clone()));
    }
}

/// Changes as a tree like the explorer: folders first (single-folder chains compacted into
/// one row), then files; a collapsed folder hides what's inside it.
fn tree_rows(changes: &[Change], staged: bool, section: &str, collapsed: &std::collections::HashSet<String>) -> Vec<ListRow> {
    let mut root = Node::default();
    for c in changes {
        let mut node = &mut root;
        let dir = ui::file_dir(&c.path);
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            node = node.dirs.entry(part.to_string()).or_default();
        }
        node.files.push(c.clone());
    }
    fn emit(node: &Node, prefix: &str, depth: usize, staged: bool, section: &str, collapsed: &std::collections::HashSet<String>, rows: &mut Vec<ListRow>) {
        for (name, child) in &node.dirs {
            let (mut label, mut full, mut child) = (name.clone(), if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") }, child);
            while child.files.is_empty() && child.dirs.len() == 1 {
                let (n, c) = child.dirs.iter().next().unwrap();
                label = format!("{label} / {n}");
                full = format!("{full}/{n}");
                child = c;
            }
            let key = format!("{section}:{full}");
            let open = !collapsed.contains(&key);
            let mut paths = Vec::new();
            child.all_paths(&mut paths);
            rows.push(ListRow::Folder { staged, key, label, depth, open, paths });
            if open {
                emit(child, &full, depth + 1, staged, section, collapsed, rows);
            }
        }
        let mut files: Vec<&Change> = node.files.iter().collect();
        files.sort_by(|a, b| ui::file_name(&a.path).cmp(ui::file_name(&b.path)));
        for c in files {
            rows.push(ListRow::File { staged, change: c.clone(), depth });
        }
    }
    let mut rows = Vec::new();
    emit(&root, "", 0, staged, section, collapsed, &mut rows);
    rows
}

/// Changes as a flat list: by name, the folder shown beside it.
fn flat_rows(changes: &[Change], staged: bool) -> Vec<ListRow> {
    let mut files: Vec<&Change> = changes.iter().collect();
    files.sort_by(|a, b| (ui::file_name(&a.path), &a.path).cmp(&(ui::file_name(&b.path), &b.path)));
    files.into_iter().map(|c| ListRow::File { staged, change: c.clone(), depth: 0 }).collect()
}

const LANE_W: f32 = 11.;
/// Opacity of graph colors for commits the checkout doesn't contain.
const MUTED: f32 = 0.38;
/// Changes tree: where depth 0 starts, and the step per level (same as the explorer).
const TREE_LEFT: f32 = 16.;
const TREE_INDENT: f32 = 12.;
const GRAPH_ROW_H: f32 = 20.;
/// The repository picker under the title (only with more than one repository).
const REPO_H: f32 = 30.;

pub struct GitPanel {
    focus: FocusHandle,
    repo: Entity<Repo>,
    pub commit_input: Entity<Editor>,
    pub drafter: git::draft::Drafter,
    changes_open: bool,
    graph_open: bool,
    staged_open: bool,
    unstaged_open: bool,
    selected: Option<Item>,
    amend: bool,
    split: f32,
    dragging_split: bool,
    panel_bounds: Rc<Cell<Bounds<Pixels>>>,
    graph_scroll: UniformListScrollHandle,
    changes_scroll: ScrollHandle,
    commit_task: Option<Task<()>>,
    menu_open: bool,
    /// The menu under the commit button's arrow: Commit, Commit (Amend), Push.
    commit_menu_open: bool,
    scope_open: bool,
    /// The filter's choices: local and remote branches, most useful first.
    scope_local: Vec<String>,
    scope_remote: Vec<String>,
    /// Last operation seen, to prefill the message once when one starts.
    last_op: Option<git::OpState>,
    /// Collapsed folders in the changes tree ("section:dir").
    collapsed: std::collections::HashSet<String>,
    /// The changes row under the mouse: only it shows its buttons (the others' names get
    /// that room). Tracked here, since a hover style can't change layout.
    hovered_row: Option<String>,
    /// Changes as a folder tree; off (the default), a flat list with the status letter on the left.
    as_tree: bool,
    /// `in_checkout` for the current log and HEAD, recomputed when either changes.
    reach: std::cell::RefCell<Option<(String, usize, Rc<(Vec<bool>, Option<usize>)>)>>,
    /// Commits opened in the graph to list their files (by sha), and those files once read.
    expanded: std::collections::HashSet<String>,
    commit_files: std::collections::HashMap<String, Rc<CommitFiles>>,
    /// Every repository in the workspace (the picker's choices) and whether the picker is open.
    repos: Vec<Entity<Repo>>,
    repo_menu: bool,
    /// Unsent commit messages of the repositories not shown.
    drafts: std::collections::HashMap<std::path::PathBuf, String>,
    repo_sub: Subscription,
    _repos_subs: Vec<Subscription>,
    _subs: Vec<Subscription>,
    /// The Repository section (branches, remotes, tags…), and its groups folded shut.
    refs_open: bool,
    /// Branches, tags, stashes…: also shows the menus, dialogs and rebase card of the graph.
    pub refs: Entity<refs::RefsTree>,
    search_open: bool,
    pub search_input: Entity<Editor>,
    /// Put `commit.template` in the empty commit box (at the start and after each commit).
    want_template: bool,
}

impl EventEmitter<Request> for GitPanel {}

impl GitPanel {
    pub fn new(repo: Entity<Repo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commit_input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::MultiLine, cx);
            ed.font_size = 12.;
            ed.wrap = true;
            ed
        });
        let repo_sub = Self::observe_repo(&repo, cx);
        let refs = cx.new(|cx| {
            let mut t = refs::RefsTree::new(repo.clone(), cx);
            t.max_h = Some(300.);
            t.show_tree = false;
            t
        });
        let search_input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.font_size = 12.;
            ed.placeholder = "Message, author or hash".into();
            ed
        });
        let subs = vec![
            cx.subscribe(&refs, |p, _, refs::Reveal(rev), cx| p.reveal(rev, cx)),
            cx.subscribe_in(&commit_input, window, |panel, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => panel.commit(None, false, panel.amend, window, cx),
                EditorEvent::Cancel => window.focus(&panel.focus_target(cx), cx),
                _ => {}
            }),
            cx.subscribe_in(&search_input, window, |panel, input, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed | EditorEvent::Submit => {
                    let q = input.read(cx).text();
                    panel.search_graph(q.trim(), window, cx);
                }
                EditorEvent::Cancel => {
                    panel.search_open = false;
                    panel.search_graph("", window, cx);
                    window.focus(&panel.focus_target(cx), cx);
                }
                _ => {}
            }),
        ];
        Self {
            focus: cx.focus_handle(),
            repos: vec![repo.clone()],
            repo,
            commit_input,
            drafter: Default::default(),
            changes_open: true,
            graph_open: true,
            staged_open: true,
            unstaged_open: true,
            selected: None,
            amend: false,
            split: 0.42,
            dragging_split: false,
            panel_bounds: Rc::new(Cell::new(Bounds::default())),
            graph_scroll: UniformListScrollHandle::new(),
            changes_scroll: ScrollHandle::new(),
            commit_task: None,
            menu_open: false,
            commit_menu_open: false,
            scope_open: false,
            scope_local: Vec::new(),
            scope_remote: Vec::new(),
            last_op: None,
            collapsed: Default::default(),
            hovered_row: None,
            as_tree: false,
            reach: Default::default(),
            expanded: Default::default(),
            commit_files: Default::default(),
            repo_menu: false,
            drafts: Default::default(),
            repo_sub,
            _repos_subs: Vec::new(),
            _subs: subs,
            refs_open: false,
            refs,
            search_open: false,
            search_input,
            want_template: true,
        }
    }

    fn observe_repo(repo: &Entity<Repo>, cx: &mut Context<Self>) -> Subscription {
        cx.observe(repo, |panel, repo, cx| {
                let op = repo.read(cx).status.op.clone();
                if op.as_ref().map(|o| (o.kind, &o.message)) != panel.last_op.as_ref().map(|o| (o.kind, &o.message)) {
                    // A merge/rebase step started: put git's prepared message in the box.
                    let msg = op.as_ref().map(|o| o.message.clone()).unwrap_or_default();
                    let current = panel.commit_input.read(cx).text();
                    let was_prefilled = panel.last_op.as_ref().is_some_and(|o| o.message == current);
                    if current.trim().is_empty() || was_prefilled {
                        panel.commit_input.update(cx, |ed, cx| ed.set_text(&msg, cx));
                    }
                    panel.last_op = op;
                }
                if panel.want_template
                    && let Some(t) = repo.read(cx).template.clone().filter(|t| !t.is_empty())
                    && repo.read(cx).status.op.is_none()
                {
                    panel.want_template = false;
                    if panel.commit_input.read(cx).text().trim().is_empty() {
                        panel.commit_input.update(cx, |ed, cx| ed.set_text(&t, cx));
                    }
                }
                let branch = repo.read(cx).branch_label();
                panel.commit_input.update(cx, |ed, cx| {
                    ed.placeholder = format!("Message (⌘Enter to commit on \"{branch}\")").into();
                    cx.notify();
                });
                cx.notify();
        })
    }

    /// The workspace's repositories changed (found, added, removed) or another one was picked.
    pub fn set_repos(&mut self, repos: Vec<Entity<Repo>>, active: Entity<Repo>, cx: &mut Context<Self>) {
        self._repos_subs = repos.iter().filter(|r| **r != active).map(|r| cx.observe(r, |_, _, cx| cx.notify())).collect();
        self.repos = repos;
        if active != self.repo {
            let old = self.repo.read(cx).root.clone();
            let draft = self.commit_input.read(cx).text();
            self.drafts.insert(old, draft);
            let draft = self.drafts.remove(&active.read(cx).root).unwrap_or_default();
            self.commit_input.update(cx, |ed, cx| ed.set_text(&draft, cx));
            self.repo_sub = Self::observe_repo(&active, cx);
            self.repo = active;
            self.selected = None;
            self.last_op = None;
            self.amend = false;
            self.collapsed.clear();
            self.expanded.clear();
            self.commit_files.clear();
            self.reach.replace(None);
            self.scope_open = false;
            let active = self.repo.clone();
            self.refs.update(cx, |t, cx| t.set_repo(active, cx));
            self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
            // Show its state (branch in the message box, prepared merge message) right away.
            self.repo.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn repo_name(&self, cx: &App) -> String {
        self.repo.read(cx).name()
    }

    /// Title bar plus the repository picker, if shown.
    fn head_h(&self) -> f32 {
        35. + if self.repos.len() > 1 { REPO_H } else { 0. }
    }

    fn render_repo_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        repo_picker("repo-picker", &self.repo, self.repo_menu, cx).on_click(cx.listener(|p, _, _, cx| {
            p.repo_menu = !p.repo_menu;
            cx.notify();
        }))
    }

    fn render_repo_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let close = |p: &mut Self, cx: &mut Context<Self>| {
            p.repo_menu = false;
            cx.notify();
        };
        repo_menu("repo", &self.repos, &self.repo, 35., cx).on_mouse_down_out(cx.listener(move |p, _, _, cx| close(p, cx))).on_click(cx.listener(move |p, _, _, cx| close(p, cx)))
    }

    #[cfg(test)]
    pub fn changes_scroll_max(&self) -> f32 {
        self.changes_scroll.max_offset().y / gpui::px(1.)
    }

    pub fn focus_target(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub fn focus_commit_input(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.commit_input.read(cx).focus_target(cx), cx);
    }

    // ------------------------------------------------------------ git actions

    pub fn commit(&mut self, message: Option<String>, all: bool, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_with(message, all, amend, false, window, cx);
    }

    /// `confirmed`: the user already chose to commit what the secrets check found.
    fn commit_with(&mut self, message: Option<String>, all: bool, amend: bool, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let message = message.unwrap_or_else(|| self.commit_input.read(cx).text()).trim().to_string();
        let repo = self.repo.read(cx);
        if let Some(op) = repo.status.op.clone() {
            return self.continue_op(op, message, window, cx);
        }
        if message.is_empty() && !amend {
            self.focus_commit_input(window, cx);
            return cx.emit(Request::Error("Write a commit message first".into()));
        }
        if repo.status.staged.is_empty() && !all && !amend && !confirmed {
            if repo.status.unstaged.is_empty() {
                return cx.emit(Request::Error("Nothing to commit".into()));
            }
            // Smart commit: let the user confirm "stage everything" on the command line.
            return cx.emit(Request::CommandLine(format!("commit -a {message}")));
        }
        let root = repo.root.clone();
        let retry = message.clone();
        let task = cx.background_executor().spawn(async move {
            if all {
                git::run(&root, &["add", "-A"])?;
            }
            // Env files or key-shaped strings in what's about to be committed: ask first.
            if !confirmed {
                let findings = crate::secrets::scan_staged(&root);
                if !findings.is_empty() {
                    return Ok((Err(findings), None));
                }
            }
            let (done, undo) = git::undo::track(
                &root,
                Some(git::undo::Kind::Commit),
                || git::commit(&root, &message, amend),
                |r| r.is_ok(),
            );
            done.map(|out| (Ok(out), undo))
        });
        self.commit_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |panel, _, cx| {
                match result {
                    Ok((Err(findings), _)) => panel.ask_secrets(findings, retry, amend, cx),
                    Ok((Ok(out), undo)) => {
                        panel.commit_input.update(cx, |ed, cx| ed.set_text("", cx));
                        panel.amend = false;
                        panel.want_template = true;
                        let summary = out.lines().next().unwrap_or("Committed").to_string();
                        if !panel.repo.update(cx, |r, cx| r.remember(undo, cx)) {
                            cx.emit(Request::Message(summary));
                        }
                    }
                    Err(e) => match e.downcast::<git::CommitFailed>() {
                        Ok(failed) => cx.emit(Request::CommitFailed(failed)),
                        Err(e) => cx.emit(Request::Error(format!("Commit failed: {e}"))),
                    },
                }
                panel.repo.update(cx, |repo, cx| {
                    repo.refresh_status(cx);
                    repo.refresh_log(cx);
                });
            })
            .ok();
        }));
    }

    /// What the secrets check found, in the themed dialog: Cancel by default, or commit anyway.
    fn ask_secrets(&mut self, findings: Vec<crate::secrets::Finding>, message: String, amend: bool, cx: &mut Context<Self>) {
        let panel = cx.entity().downgrade();
        let mut lines: Vec<String> = findings.iter().take(6).map(|f| format!("• {}", f.describe())).collect();
        if findings.len() > 6 {
            lines.push(format!("…and {} more", findings.len() - 6));
        }
        lines.push(String::new());
        lines.push("Unstage them first, or commit anyway if they're meant to be shared.".into());
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: "This commit may include secrets".into(),
            detail: lines.join("\n"),
            buttons: vec!["Commit Anyway".into()],
            danger: true,
            cancel_default: true,
            cancel_label: None,
            output: None,
            on_answer: Rc::new(move |_, window, cx| {
                let message = message.clone();
                panel.update(cx, |p, cx| p.commit_with(Some(message), false, amend, true, window, cx)).ok();
            }),
        }));
    }

    /// Commit Merge / Continue Rebase / Continue Cherry-pick, without an editor popping up.
    fn continue_op(&mut self, op: git::OpState, message: String, window: &mut Window, cx: &mut Context<Self>) {
        let st = self.repo.read(cx).status.clone();
        if !st.conflicts.is_empty() {
            let n = st.conflicts.len();
            return cx.emit(Request::Error(format!("Resolve {n} conflicted file{} first (Merge changes)", if n == 1 { "" } else { "s" })));
        }
        if op.kind == git::OpKind::Merge && message.is_empty() {
            self.focus_commit_input(window, cx);
            return cx.emit(Request::Error("Write a commit message first".into()));
        }
        if !st.unstaged.iter().all(|c| c.status == FileStatus::Untracked) && op.kind != git::OpKind::Merge {
            // Rebase/cherry-pick need resolved files staged; stage the tracked changes like VS Code.
            let paths: Vec<String> = st.unstaged.iter().filter(|c| c.status != FileStatus::Untracked).map(|c| c.path.clone()).collect();
            let root = self.repo.read(cx).root.clone();
            git::stage(&root, &paths).ok();
        }
        let kind = op.kind;
        self.commit_input.update(cx, |ed, cx| ed.set_text("", cx));
        self.last_op = None;
        self.repo.update(cx, |r, cx| r.run_op(kind.continue_label(), cx, move |root| git::continue_op(root, kind, &message)));
    }

    fn abort_op(&mut self, cx: &mut Context<Self>) {
        let Some(op) = self.repo.read(cx).status.op.clone() else { return };
        self.commit_input.update(cx, |ed, cx| ed.set_text("", cx));
        self.repo.update(cx, |r, cx| r.run_op("Abort", cx, move |root| git::abort_op(root, op.kind)));
    }

    fn skip_op(&mut self, cx: &mut Context<Self>) {
        let Some(op) = self.repo.read(cx).status.op.clone() else { return };
        self.repo.update(cx, |r, cx| r.run_op("Skip", cx, move |root| git::skip_op(root, op.kind)));
    }

    fn render_op_banner(&self, op: &git::OpState, conflicts: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(8.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .border_1()
                .border_color(theme::border_input())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let detail = if conflicts > 0 {
            format!("{conflicts} conflicted file{} left: open {} to resolve, save to mark resolved", if conflicts == 1 { "" } else { "s" }, if conflicts == 1 { "it" } else { "them" })
        } else {
            format!("No conflicts left: {}", op.kind.continue_label())
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(8.))
            .rounded(px(3.))
            .bg(theme::ca(0xe5c07b1a))
            .border_1()
            .border_color(theme::ca(0xe5c07b66))
            .child(div().flex().items_center().gap(px(6.)).child(ui::small_icon("warning", theme::c(0xe5c07b))).child(div().text_color(theme::fg_bright()).child(op.label.clone())))
            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(detail))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .child(button("op-abort", "Abort").on_click(cx.listener(|p, _, _, cx| p.abort_op(cx))))
                    .when(op.kind.can_skip(), |d| d.child(button("op-skip", "Skip this commit").on_click(cx.listener(|p, _, _, cx| p.skip_op(cx))))),
            )
    }

    pub fn stage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.repo.update(cx, |repo, cx| repo.run_op("Stage", cx, move |root| git::stage(root, &paths).map(|_| String::new())));
    }

    pub fn unstage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let initial = self.repo.read(cx).status.is_initial();
        self.repo.update(cx, |repo, cx| repo.run_op("Unstage", cx, move |root| git::unstage(root, &paths, initial).map(|_| String::new())));
    }

    /// Asks like VS Code before discarding: tracked changes are lost for good, untracked
    /// files go to the Trash. With both, you can discard everything or only the tracked ones.
    pub(crate) fn ask_discard(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let st = self.repo.read(cx).status.clone();
        let is_untracked = |p: &String| st.unstaged.iter().any(|c| &c.path == p && c.status == FileStatus::Untracked);
        let (untracked, tracked): (Vec<String>, Vec<String>) = paths.into_iter().partition(is_untracked);
        let Some(ask) = discard_prompt(&tracked, &untracked) else { return };
        let _ = window;
        let panel = cx.entity().downgrade();
        let choices = ask.buttons.clone();
        cx.emit(Request::Ask(crate::dialog::Ask {
            cancel_default: false,
            cancel_label: None,
            output: None,
            title: ask.message,
            detail: ask.detail,
            buttons: ask.buttons.into_iter().map(|(label, _)| label).collect(),
            danger: true,
            on_answer: std::rc::Rc::new(move |ix, _, cx| {
                let Some((_, what)) = choices.get(ix).cloned() else { return };
                let (tracked, untracked) = (tracked.clone(), if what == DiscardWhat::TrackedOnly { Vec::new() } else { untracked.clone() });
                panel.update(cx, |panel, cx| panel.run_discard(tracked, untracked, cx)).ok();
            }),
        }));
    }

    fn run_discard(&mut self, tracked: Vec<String>, untracked: Vec<String>, cx: &mut Context<Self>) {
        let n = tracked.len() + untracked.len();
        self.repo.update(cx, |repo, cx| {
            repo.run_op("Discard", cx, move |root| {
                if !tracked.is_empty() {
                    git::discard(root, &tracked)?;
                }
                for p in &untracked {
                    trash::delete(root.join(p)).map_err(|e| anyhow::anyhow!("couldn't move {p} to the Trash: {e}"))?;
                }
                Ok(format!("Discarded {n} file{}", if n == 1 { "" } else { "s" }))
            })
        });
    }

    fn set_hovered_row(&mut self, key: &str, on: bool, cx: &mut Context<Self>) {
        if on {
            if self.hovered_row.as_deref() != Some(key) {
                self.hovered_row = Some(key.to_string());
                cx.notify();
            }
        } else if self.hovered_row.as_deref() == Some(key) {
            self.hovered_row = None;
            cx.notify();
        }
    }

    fn open_change(&self, staged: bool, change: &Change, preview: bool, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        if matches!(change.status, FileStatus::Untracked | FileStatus::Conflicted) {
            let path = root.join(&change.path);
            return cx.emit(if preview { Request::ShowFile(path) } else { Request::OpenFile { path, line: None, preview } });
        }
        let (left, right) = if staged {
            let left = if change.status == FileStatus::Added { Rev::Empty } else { Rev::Commit("HEAD".into()) };
            let right = if change.status == FileStatus::Deleted { Rev::Empty } else { Rev::Index };
            (left, right)
        } else {
            let right = if change.status == FileStatus::Deleted { Rev::Empty } else { Rev::Working };
            (Rev::Index, right)
        };
        cx.emit(Request::OpenDiff {
            spec: DiffSpec { path: change.path.clone(), old_path: change.orig_path.clone(), left, right, back: None },
            preview,
        });
    }

    // ------------------------------------------------------------ list model

    fn list_rows(&self, cx: &App) -> Vec<ListRow> {
        let repo = self.repo.read(cx);
        let st = &repo.status;
        let mut rows = Vec::new();
        let collapsed = &self.collapsed;
        let tree = self.as_tree;
        let group = |rows: &mut Vec<ListRow>, staged: bool, section: &str, changes: &[Change]| {
            rows.extend(if tree { tree_rows(changes, staged, section, collapsed) } else { flat_rows(changes, staged) })
        };
        if !st.conflicts.is_empty() {
            rows.push(ListRow::Section { staged: false, conflicts: true, count: st.conflicts.len() });
            group(&mut rows, false, "merge", &st.conflicts);
        }
        if !st.staged.is_empty() {
            rows.push(ListRow::Section { staged: true, conflicts: false, count: st.staged.len() });
            if self.staged_open {
                group(&mut rows, true, "staged", &st.staged);
            }
        }
        rows.push(ListRow::Section { staged: false, conflicts: false, count: st.unstaged.len() });
        if self.unstaged_open {
            group(&mut rows, false, "changes", &st.unstaged);
        }
        rows
    }

    /// Everything keyboard navigation can land on, in order.
    fn items(&self, cx: &App) -> Vec<Item> {
        let mut items = Vec::new();
        if self.changes_open {
            for row in self.list_rows(cx) {
                if let ListRow::File { staged, change, .. } = row {
                    items.push(Item::File { staged, path: change.path });
                }
            }
        }
        if self.graph_open {
            items.extend(self.graph_items(cx).into_iter().filter_map(|i| match i {
                GraphItem::Commit(ix) => Some(Item::Commit(ix)),
                GraphItem::File(ix, fi) => Some(Item::CommitFile(ix, fi)),
                GraphItem::Loading(_) => None,
            }));
        }
        items
    }

    fn change_for(&self, staged: bool, path: &str, cx: &App) -> Option<Change> {
        let st = &self.repo.read(cx).status;
        let list = if staged { &st.staged } else { &st.unstaged };
        list.iter().chain(st.conflicts.iter()).find(|c| c.path == path).cloned()
    }

    fn move_sel(&mut self, delta: isize, cx: &mut Context<Self>) {
        let items = self.items(cx);
        if items.is_empty() {
            return;
        }
        let cur = self.selected.as_ref().and_then(|s| items.iter().position(|i| i == s));
        let next = match cur {
            Some(c) => (c as isize + delta).clamp(0, items.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(items[next].clone(), cx);
    }

    fn select(&mut self, item: Item, cx: &mut Context<Self>) {
        let in_graph = match item {
            Item::Commit(ix) => Some(GraphItem::Commit(ix)),
            Item::CommitFile(ix, fi) => Some(GraphItem::File(ix, fi)),
            Item::File { .. } => None,
        };
        if let Some(target) = in_graph
            && let Some(row) = self.graph_items(cx).iter().position(|i| *i == target)
        {
            self.graph_scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
        self.selected = Some(item);
        cx.notify();
    }

    /// (changes, graph) sections open (tests).
    #[cfg(test)]
    pub fn sections_open(&self) -> (bool, bool) { (self.changes_open, self.graph_open) }

    #[cfg(test)]
    pub fn expanded_count(&self) -> usize { self.expanded.len() }

    /// The commit selected in the graph (tests).
    #[cfg(test)]
    pub fn selected_commit(&self) -> Option<usize> {
        match self.selected {
            Some(Item::Commit(ix)) => Some(ix),
            _ => None,
        }
    }

    /// The graph's rows: every commit, with an expanded one's files right under it.
    fn graph_items(&self, cx: &App) -> Vec<GraphItem> {
        let commits = &self.repo.read(cx).commits;
        let mut items = Vec::with_capacity(commits.len());
        for (ix, c) in commits.iter().enumerate() {
            items.push(GraphItem::Commit(ix));
            if self.expanded.contains(&c.sha) {
                match self.commit_files.get(&c.sha) {
                    Some(f) => items.extend((0..f.files.len()).map(|fi| GraphItem::File(ix, fi))),
                    None => items.push(GraphItem::Loading(ix)),
                }
            }
        }
        items
    }

    /// Opens or closes a commit in the graph; its files are read the first time.
    fn toggle_commit(&mut self, sha: &str, cx: &mut Context<Self>) {
        if self.expanded.remove(sha) {
            return cx.notify();
        }
        self.expanded.insert(sha.to_string());
        if !self.commit_files.contains_key(sha) {
            let (root, s) = (self.repo.read(cx).root.clone(), sha.to_string());
            let load = cx.background_executor().spawn(async move { git::commit_details(&root, &s) });
            let sha = sha.to_string();
            cx.spawn(async move |panel, cx| {
                let result = load.await;
                panel
                    .update(cx, |p, cx| {
                        match result {
                            Ok(d) => {
                                p.commit_files.insert(sha, Rc::new(CommitFiles { parent: d.parents.first().cloned(), files: d.files }));
                            }
                            Err(_) => {
                                p.expanded.remove(&sha);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// In a file's history: that file's change in the commit at `ix` (under the name it had then).
    fn open_history_diff(&self, ix: usize, preview: bool, cx: &mut Context<Self>) {
        let repo = self.repo.read(cx);
        let (Some((file, names)), Some(c)) = (repo.graph_file.as_ref(), repo.commits.get(ix)) else { return };
        let path = names.get(&c.sha).cloned().unwrap_or(file.clone());
        let older = repo.commits.get(ix + 1).and_then(|o| names.get(&o.sha)).filter(|o| **o != path).cloned();
        // The oldest commit listed is where the file was added.
        let left = match c.parents.first() {
            Some(p) if ix + 1 < repo.commits.len() => Rev::Commit(p.clone()),
            _ => Rev::Empty,
        };
        let spec = DiffSpec { path, old_path: older, left, right: Rev::Commit(c.sha.clone()), back: None };
        cx.emit(Request::OpenDiff { spec, preview });
    }

    /// A file under an expanded commit: its diff against the commit's first parent.
    fn open_commit_file(&self, ix: usize, fi: usize, preview: bool, cx: &mut Context<Self>) {
        let Some(sha) = self.repo.read(cx).commits.get(ix).map(|c| c.sha.clone()) else { return };
        let Some(f) = self.commit_files.get(&sha) else { return };
        let Some(file) = f.files.get(fi) else { return };
        let left = match (&f.parent, file.status) {
            (_, FileStatus::Added) | (None, _) => Rev::Empty,
            (Some(p), _) => Rev::Commit(p.clone()),
        };
        let right = if file.status == FileStatus::Deleted { Rev::Empty } else { Rev::Commit(sha.clone()) };
        cx.emit(Request::OpenDiff {
            spec: DiffSpec { path: file.path.clone(), old_path: file.orig_path.clone(), left, right, back: Some(crate::request::Back::Commit(sha)) },
            preview,
        });
    }

    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Item::Commit(ix)) = self.selected
            && let Some(sha) = self.repo.read(cx).commits.get(ix).map(|c| c.sha.clone())
            && !self.expanded.contains(&sha)
        {
            self.toggle_commit(&sha, cx);
        }
    }

    /// On a commit: hide its files. On one of those files: hide them and go back to the commit.
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let ix = match self.selected {
            Some(Item::Commit(ix) | Item::CommitFile(ix, _)) => ix,
            _ => return,
        };
        if let Some(sha) = self.repo.read(cx).commits.get(ix).map(|c| c.sha.clone())
            && self.expanded.contains(&sha)
        {
            self.toggle_commit(&sha, cx);
            self.selected = Some(Item::Commit(ix));
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) { self.move_sel(1, cx) }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) { self.move_sel(-1, cx) }
    fn top(&mut self, _: &Top, _: &mut Window, cx: &mut Context<Self>) { self.move_sel(isize::MIN / 2, cx) }
    fn bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) { self.move_sel(isize::MAX / 2, cx) }

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        match self.selected.clone() {
            Some(Item::File { staged, path }) => {
                if let Some(change) = self.change_for(staged, &path, cx) {
                    self.open_change(staged, &change, false, cx);
                }
            }
            Some(Item::Commit(ix)) => {
                if let Some(c) = self.repo.read(cx).commits.get(ix) {
                    cx.emit(Request::OpenCommit(c.sha.clone()));
                }
            }
            Some(Item::CommitFile(ix, fi)) => self.open_commit_file(ix, fi, false, cx),
            None => {}
        }
    }

    fn toggle_stage(&mut self, _: &ToggleStage, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Item::File { staged, path }) = self.selected.clone() {
            // Keep the cursor on the next row, like lazygit.
            self.move_sel(1, cx);
            if staged { self.unstage(vec![path], cx) } else { self.stage(vec![path], cx) }
        }
    }

    fn discard(&mut self, _: &Discard, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Item::File { staged: false, path }) = self.selected.clone() {
            self.ask_discard(vec![path], window, cx);
        }
    }

    fn open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Item::File { path, .. }) = self.selected.clone() {
            let root = self.repo.read(cx).root.clone();
            cx.emit(Request::OpenFile { path: root.join(path), line: None, preview: false });
        }
    }

    fn on_focus_commit(&mut self, _: &FocusCommit, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_commit_input(window, cx);
    }

    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.repo.update(cx, |r, cx| r.refresh_all(cx));
    }

    fn run_menu(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        let st = self.repo.read(cx).status.clone();
        let git = |p: &mut Self, args: &[&str], cx: &mut Context<Self>| {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            p.repo.update(cx, |r, cx| r.run_git(args, cx));
        };
        match ix {
            0 => git(self, &["pull"], cx),
            1 => git(self, &["push"], cx),
            2 => git(self, &["fetch", "--all", "--prune"], cx),
            3 => git(self, &["pull", "--rebase"], cx),
            4 => {
                let paths = st.unstaged.iter().map(|c| c.path.clone()).collect();
                self.stage(paths, cx)
            }
            5 => {
                let paths = st.staged.iter().map(|c| c.path.clone()).collect();
                self.unstage(paths, cx)
            }
            6 => {
                let all: Vec<String> = st.unstaged.iter().map(|c| c.path.clone()).collect();
                self.ask_discard(all, window, cx)
            }
            7 => {
                self.amend = !self.amend;
                self.focus_commit_input(window, cx);
            }
            8 => cx.emit(Request::CommandLine("commit --amend ".into())),
            9 => git(self, &["reset", "--soft", "HEAD~1"], cx),
            10 => git(self, &["stash", "push", "--include-untracked"], cx),
            11 => git(self, &["stash", "pop"], cx),
            12 => cx.emit(Request::CommandLine("checkout ".into())),
            13 => cx.emit(Request::CommandLine("branch ".into())),
            14 => self.repo.update(cx, |r, cx| {
                r.graph_all = !r.graph_all;
                r.refresh_log(cx);
            }),
            15 => cx.emit(Request::CommandLine("git ".into())),
            17 => cx.emit(Request::CommandLine("stash-save ".into())),
            18 => cx.emit(Request::CommandLine("patch-apply ".into())),
            19 => {
                self.search_open = true;
                window.focus(&self.search_input.read(cx).focus_target(cx), cx);
            }
            20 => self.toggle_refs(cx),
            _ => self.repo.update(cx, |r, cx| r.refresh_all(cx)),
        }
        cx.notify();
    }

    /// A popup this wide, hung from the panel's right edge, stays inside the window.
    /// Selects, in the graph, the commit a branch or tag of the tree points at.
    fn reveal(&mut self, rev: &str, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        let Ok(sha) = git::run(&root, &["rev-parse", "--verify", "-q", rev]).map(|s| s.trim().to_string()) else { return };
        let Some(ix) = self.repo.read(cx).commits.iter().position(|c| c.sha == sha) else {
            return cx.emit(Request::Message("Not in the graph: choose more branches with its filter".into()));
        };
        self.graph_open = true;
        self.selected = Some(Item::Commit(ix));
        if let Some(pos) = self.graph_items(cx).iter().position(|i| *i == GraphItem::Commit(ix)) {
            self.graph_scroll.scroll_to_item(pos, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Right-click on a commit of the graph.
    fn open_commit_menu(&mut self, ix: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(commit) = self.repo.read(cx).commits.get(ix).cloned() else { return };
        let in_checkout = self.reachability(cx).0.get(ix).copied().unwrap_or(false);
        let items = self.refs.read(cx).commit_menu(&commit, in_checkout, cx);
        self.refs.update(cx, |t, cx| t.open_menu(items, at, cx));
    }

    fn toggle_refs(&mut self, cx: &mut Context<Self>) {
        self.refs_open = !self.refs_open;
        let open = self.refs_open;
        self.refs.update(cx, |t, cx| {
            t.show_tree = open;
            cx.notify();
        });
        cx.notify();
    }

    /// The Repository section: its header, and the tree when open (closed, the tree still
    /// draws the graph's menus and dialogs).
    fn render_refs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let header = ui::section_header("refs-header", "Repository", self.refs_open, cx.listener(|p, _, _, cx| p.toggle_refs(cx))).child(
            div()
                .flex()
                .gap(px(1.))
                .child(ui::icon_button("refs-open-tab", "maximize").debug_selector(|| "refs-open-tab".into()).tooltip(ui::tooltip("Open the Git tab: history, branches and changes side by side")).on_click(cx.listener(|p, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(Request::OpenGitTab(p.repo.read(cx).root.clone()));
                })))
                .child(ui::icon_button("refs-fetch", "fetch").tooltip(ui::tooltip("Fetch all remotes")).on_click(cx.listener(|p, _, _, cx| {
                    cx.stop_propagation();
                    p.repo.update(cx, |r, cx| r.run_git(vec!["fetch".into(), "--all".into(), "--prune".into()], cx));
                }))),
        );
        div().flex().flex_col().flex_none().child(header).child(self.refs.clone())
    }

    /// A text box of the panel has the keyboard: letters type instead of acting on the list.
    fn typing(&self, window: &Window, cx: &App) -> bool {
        [&self.commit_input, &self.search_input].into_iter().any(|e| e.read(cx).focus_handle(cx).contains_focused(window, cx)) || self.refs.read(cx).typing(window, cx)
    }

    fn popup_fits(&self, w: f32) -> bool {
        self.panel_bounds.get().right() - px(8.) - px(w) >= px(0.)
    }

    /// Under the commit button's arrow: commit, commit amending the last one, or push the
    /// current branch (setting its upstream when it has none, like the terminal's advice).
    fn render_commit_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items: [(&'static str, &'static str); 3] = [("commit-menu-commit", "Commit"), ("commit-menu-amend", "Commit (Amend)"), ("commit-menu-push", "Push")];
        div()
            .id("commit-menu-popup")
            .occlude()
            .absolute()
            .top(px(28.))
            .right_0()
            .w(px(180.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_color(theme::fg())
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.commit_menu_open = false;
                cx.notify();
            }))
            .children(items.into_iter().enumerate().map(|(ix, (id, label))| {
                div()
                    .id(id)
                    .debug_selector(move || id.to_string())
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(10.))
                    .mx(px(4.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::selected_focused()))
                    .when(ix == 2, |d| d.mt(px(4.)).border_t_1().border_color(theme::border()))
                    .on_click(cx.listener(move |p, _, window, cx| {
                        p.commit_menu_open = false;
                        match ix {
                            0 | 1 => {
                                p.amend = ix == 1;
                                p.commit(None, false, ix == 1, window, cx);
                            }
                            _ => p.repo.update(cx, |r, cx| r.run_git(vec!["push".into()], cx)),
                        }
                        cx.notify();
                    }))
                    .child(label)
            }))
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let graph_all = self.repo.read(cx).graph_all;
        let items: [(&str, &str); 21] = [
            ("Pull", ":pull"),
            ("Push", ":push"),
            ("Fetch (all, prune)", ":fetch"),
            ("Pull (rebase)", ""),
            ("Stage all changes", ""),
            ("Unstage all", ""),
            ("Discard all changes…", ""),
            (if self.amend { "Amend mode: on" } else { "Amend mode: off" }, ""),
            ("Amend last commit with message…", ""),
            ("Undo last commit (keep changes)", ""),
            ("Stash all (incl. untracked)", ":stash"),
            ("Pop stash", ""),
            ("Checkout branch…", ":checkout"),
            ("Create branch…", ":branch"),
            (if graph_all { "Graph: show all branches ✓" } else { "Graph: show all branches" }, "filter ▾"),
            ("Run git command…", ":git"),
            ("Refresh", "r"),
            ("Stash with a message…", ":stash-save"),
            ("Apply a patch file…", ":patch-apply"),
            ("Search the graph…", ":find"),
            (if self.refs_open { "Hide branches, tags, stashes…" } else { "Show branches, tags, stashes…" }, ""),
        ];
        div()
            .id("scm-menu-popup")
            .max_h(self.panel_bounds.get().size.height.max(gpui::px(240.)) - gpui::px(40.))
            .overflow_y_scroll()
            .occlude()
            .absolute()
            .top(px(32.))
            // Under its button; from the panel's left edge when the panel is too narrow for it.
            .map(|d| if self.popup_fits(250.) { d.right(px(8.)) } else { d.left(px(8.)) })
            .w(px(250.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.menu_open = false;
                cx.notify();
            }))
            .children(items.into_iter().enumerate().map(|(ix, (label, hint))| {
                div()
                    .id(("scm-menu-item", ix))
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .px(px(10.))
                    .mx(px(4.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::selected_focused()))
                    .when(matches!(ix, 4 | 7 | 10 | 12 | 14 | 17), |d| d.mt(px(4.)).border_t_1().border_color(theme::border()))
                    .on_click(cx.listener(move |p, _, w, cx| p.run_menu(ix, w, cx)))
                    .child(div().flex_1().child(label.to_string()))
                    .child(div().text_color(theme::fg_muted()).child(hint.to_string()))
            }))
    }

    /// Graph filter: Auto, All, or any pick of local and remote branches.
    fn render_scope(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let (all, only) = (repo.graph_all, repo.graph_only.clone());
        let auto = !all && only.is_empty();
        // Real pixels: the measured height plus the zoomed header.
        let top = self.panel_bounds.get().size.height / gpui::px(1.) * if self.changes_open && self.graph_open { self.split } else { 0. } + (self.head_h() + 25.) * ui::scale();
        let option = |id: &'static str, label: &str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(22.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(3.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::selected_focused()))
                .child(div().w(px(12.)).text_color(theme::frame_focused()).child(if on { "●" } else { "○" }))
                .child(label.to_string())
        };
        div()
            .id("graph-scope-popup")
            .occlude()
            .absolute()
            .top(gpui::px(top))
            .map(|d| if self.popup_fits(270.) { d.right(px(8.)) } else { d.left(px(8.)) })
            .w(px(270.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.scope_open = false;
                cx.notify();
            }))
            .child(div().px(px(14.)).py(px(4.)).text_color(theme::fg_muted()).child("Show in graph"))
            .child(option("scope-auto", "Auto — current, upstream, default", auto).on_click(cx.listener(|p, _, _, cx| {
                p.repo.update(cx, |r, cx| {
                    r.graph_all = false;
                    r.graph_only.clear();
                    r.refresh_log(cx);
                });
                cx.notify();
            })))
            .child(option("scope-all", "All branches", all && only.is_empty()).on_click(cx.listener(|p, _, _, cx| {
                p.repo.update(cx, |r, cx| {
                    r.graph_all = true;
                    r.graph_only.clear();
                    r.refresh_log(cx);
                });
                cx.notify();
            })))
            .child(self.render_scope_list("scope-local", "Local", &self.scope_local, false, &only, cx))
            .child(self.render_scope_list("scope-remote", "Remote", &self.scope_remote, true, &only, cx))
    }

    /// One part of the filter (local or remote branches): a title and its own scrolling list.
    fn render_scope_list(&self, id: &'static str, title: &str, names: &[String], remote: bool, only: &[String], cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, color) = if remote { ("cloud", theme::ref_remote()) } else { ("git", theme::ref_local()) };
        div()
            .flex()
            .flex_col()
            .mt(px(4.))
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .px(px(14.))
                    .py(px(4.))
                    .text_color(theme::fg_muted())
                    .child(title.to_string())
                    .child(div().text_color(theme::fg_dim()).child(names.len().to_string())),
            )
            .child(
                div().id(id).max_h(px(150.)).overflow_y_scroll().children(names.iter().enumerate().map(|(ix, b)| {
                    let on = only.contains(b);
                    let name = b.clone();
                    let selector = format!("{id}-{b}");
                    div()
                        .id((id, ix))
                        .debug_selector(move || selector)
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(22.))
                        .px(px(10.))
                        .mx(px(4.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::selected_focused()))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            let name = name.clone();
                            p.repo.update(cx, |r, cx| {
                                if let Some(i) = r.graph_only.iter().position(|x| *x == name) {
                                    r.graph_only.remove(i);
                                } else {
                                    r.graph_only.push(name);
                                }
                                r.refresh_log(cx);
                            });
                            cx.notify();
                        }))
                        .child(div().w(px(12.)).text_color(theme::frame_focused()).child(if on { "☑" } else { "☐" }))
                        .child(ui::small_icon(icon, color).size(px(12.)))
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(b.clone()))
                })),
            )
    }

    // ------------------------------------------------------------ rendering

    fn render_list_row(&self, row: &ListRow, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        match row {
            ListRow::Section { staged, conflicts, count } => {
                let (staged, count) = (*staged, *count);
                let open = if staged { self.staged_open } else { self.unstaged_open };
                let title = if *conflicts { "Merge changes" } else if staged { "Staged changes" } else { "Changes" };
                let paths: Vec<String> = {
                    let st = &self.repo.read(cx).status;
                    (if staged { &st.staged } else { &st.unstaged }).iter().map(|c| c.path.clone()).collect()
                };
                // Discard-all covers untracked files too (the dialog asks about them).
                let all: Vec<String> = self.repo.read(cx).status.unstaged.iter().map(|c| c.path.clone()).collect();
                let actions = div().flex().gap(px(2.)).invisible().group_hover("section", |s| s.visible());
                let actions = if staged {
                    let p = paths.clone();
                    actions.child(ui::icon_button(("unstage-all", ix), "minus").on_click(cx.listener(move |panel, _, _, cx| {
                        cx.stop_propagation();
                        panel.unstage(p.clone(), cx);
                    })))
                } else {
                    let p = paths.clone();
                    actions
                        .child(ui::icon_button(("discard-all", ix), "discard").on_click(cx.listener(move |panel, _, window, cx| {
                            cx.stop_propagation();
                            panel.ask_discard(all.clone(), window, cx);
                        })))
                        .child(ui::icon_button(("stage-all", ix), "plus").on_click(cx.listener(move |panel, _, _, cx| {
                            cx.stop_propagation();
                            panel.stage(p.clone(), cx);
                        })))
                };
                let section_sel = format!("changes-section-{ix}");
                div()
                    .id(("section", ix))
                    .debug_selector(move || section_sel)
                    .group("section")
                    .flex()
                    .items_center()
                    .h(theme::row_h())
                    .pl(px(10.))
                    .pr(px(8.))
                    .gap(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        if staged {
                            panel.staged_open = !panel.staged_open;
                        } else {
                            panel.unstaged_open = !panel.unstaged_open;
                        }
                        cx.notify();
                    }))
                    .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg()))
                    .child(div().flex_1().child(title))
                    .child(actions)
                    .child(ui::badge(count.to_string(), theme::accent()))
                    .into_any_element()
            }
            ListRow::Folder { staged, key, label, depth, open, paths } => {
                let (staged, paths, open, key) = (*staged, paths.clone(), *open, key.clone());
                let hover_key = format!("folder:{key}");
                let hovered = self.hovered_row.as_deref() == Some(hover_key.as_str());
                let name = label.rsplit(" / ").next().unwrap_or(label).to_string();
                let row_selector = format!("folder-row-{name}");
                div()
                    .id(("folder", ix))
                    .debug_selector(move || row_selector)
                    .group("folder")
                    .on_hover(cx.listener(move |p, on: &bool, _, cx| p.set_hovered_row(&hover_key, *on, cx)))
                    .flex()
                    .items_center()
                    .h(theme::row_h())
                    .pl(px(TREE_LEFT + *depth as f32 * TREE_INDENT))
                    .pr(px(8.))
                    .gap(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        if !panel.collapsed.remove(&key) {
                            panel.collapsed.insert(key.clone());
                        }
                        cx.notify();
                    }))
                    .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg()))
                    .child(ui::folder_icon(&name, open))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme::fg()).child(label.clone()))
                    .child({
                        let discard = paths.clone();
                        let selector = format!("folder-discard-{name}");
                        div()
                            .flex()
                            .gap(px(1.))
                            // Out of the way until hovered, so the name gets the room.
                            .when(!hovered, |d| d.hidden())
                            // Discard everything in the folder (asks first, like a file), like VS Code.
                            .when(!staged, |d| {
                                d.child(
                                    ui::icon_button(("folder-discard", ix), "discard")
                                        .debug_selector(move || selector)
                                        .tooltip(ui::tooltip("Discard changes in this folder"))
                                        .on_click(cx.listener(move |panel, _, window, cx| {
                                            cx.stop_propagation();
                                            panel.ask_discard(discard.clone(), window, cx);
                                        })),
                                )
                            })
                            .child(
                                ui::icon_button(("folder-stage", ix), if staged { "minus" } else { "plus" })
                                    .tooltip(ui::tooltip(if staged { "Unstage this folder" } else { "Stage this folder" }))
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        cx.stop_propagation();
                                        if staged { panel.unstage(paths.clone(), cx) } else { panel.stage(paths.clone(), cx) }
                                    })),
                            )
                    })
                    .child(ui::small_icon("dot", theme::git_modified()).size(px(8.)))
                    .into_any_element()
            }
            ListRow::File { staged, change, depth } => {
                let staged = *staged;
                let name = ui::file_name(&change.path).to_string();
                let dir = ui::file_dir(&change.path).to_string();
                let item = Item::File { staged, path: change.path.clone() };
                let selected = self.selected.as_ref() == Some(&item);
                let color = ui::status_color(change.status);
                let (c1, c2, c3) = (change.clone(), change.clone(), change.clone());
                let root = self.repo.read(cx).root.clone();
                let path_open = root.join(&change.path);
                let selector = format!("change-row-{}", c1.path);
                let tree = self.as_tree;
                let hovered = self.hovered_row.as_deref() == Some(&format!("file:{staged}:{}", change.path));
                let buttons = div()
                    .flex()
                    .gap(px(1.))
                    .child(ui::icon_button(("open", ix), "open").size(px(20.)).tooltip(ui::tooltip("Open the file")).on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(Request::OpenFile { path: path_open.clone(), line: None, preview: false });
                    })))
                    .when(!staged, |d| {
                        d.child(ui::icon_button(("discard", ix), "discard").size(px(20.)).tooltip(ui::tooltip("Discard changes")).on_click(cx.listener(move |panel, _, window, cx| {
                            cx.stop_propagation();
                            panel.ask_discard(vec![c2.path.clone()], window, cx);
                        })))
                    })
                    .child(ui::icon_button(("stage", ix), if staged { "minus" } else { "plus" }).size(px(20.)).tooltip(ui::tooltip(if staged { "Unstage" } else { "Stage" })).on_click(cx.listener(move |panel, _, _, cx| {
                        cx.stop_propagation();
                        if staged { panel.unstage(vec![c3.path.clone()], cx) } else { panel.stage(vec![c3.path.clone()], cx) }
                    })));
                div()
                    .id(("file", ix))
                    .debug_selector(move || selector)
                    .group("file")
                    .on_hover(cx.listener({
                        let k = format!("file:{staged}:{}", change.path);
                        move |p, on: &bool, _, cx| p.set_hovered_row(&k, *on, cx)
                    }))
                    .flex()
                    .items_center()
                    .h(theme::row_h())
                    .pl(px(if tree { TREE_LEFT + *depth as f32 * TREE_INDENT } else { 10. }))
                    .pr(px(8.))
                    .gap(px(4.))
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(if focused { theme::selected_focused() } else { theme::selected() })
                            .when(focused, |d| d.border_1().border_color(theme::focus_border()))
                    })
                    .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
                    .on_click(cx.listener(move |panel, ev: &ClickEvent, window, cx| {
                        window.focus(&panel.focus, cx);
                        panel.selected = Some(Item::File { staged, path: c1.path.clone() });
                        panel.open_change(staged, &c1, ev.click_count() < 2, cx);
                        cx.notify();
                    }))
                    .map(|d| if tree {
                        d
                            // Lines up with the folder icons, like the explorer.
                            .child(div().w(px(14.)).flex_none())
                            .child(ui::file_icon(&name))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(6.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child({
                                        let name_sel = format!("change-name-{}", change.path);
                                        div()
                                            .debug_selector(move || name_sel)
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_color(color)
                                            .when(change.status == FileStatus::Deleted, |d| d.line_through())
                                            .child(name.clone())
                                    })
                                    .when(*depth == 0 && !dir.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(dir.clone()))),
                            )
                            .child(div().flex().gap(px(1.)).when(!selected && !hovered, |d| d.hidden()).child(buttons))
                            .child(div().w(px(14.)).flex_none().text_color(color).text_size(px(11.)).child(change.status.letter()))
                    } else {
                        d.relative()
                            .child(ui::change_cells(change.status, &change.path, Some(format!("change-name-{}", change.path))))
                            .child(ui::hover_actions("file", selected, theme::bg_side()).child(buttons))
                    })
                    .into_any_element()
            }
        }
    }

    fn render_changes(&self, focused: bool, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let branch = repo.branch_label();
        let busy = repo.busy.clone();
        let input_focused = self.commit_input.read(cx).is_focused(window);
        let op = repo.status.op.clone();
        let conflicts = repo.status.conflicts.len();
        let commit_label = match &op {
            Some(op) => op.kind.continue_label(),
            None if self.amend => "Commit (Amend)",
            None => "Commit",
        };
        let rows = self.list_rows(cx);
        let st = &repo.status;
        let clean = !branch.is_empty() && st.conflicts.is_empty() && st.staged.is_empty() && st.unstaged.is_empty();
        div()
            .id("changes-scroll")
            .flex()
            .flex_col()
            // Fill the space it's given and scroll inside it (otherwise it just grows).
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.changes_scroll)
            .child(
                div()
                    .px(px(12.))
                    .pt(px(4.))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .when_some(op.as_ref(), |d, op| d.child(self.render_op_banner(op, conflicts, cx)))
                    .child(
                        div()
                            .p(px(4.))
                            .rounded(px(2.))
                            .bg(theme::bg_input())
                            .border_1()
                            .border_color(if input_focused { theme::focus_border() } else { theme::border_input() })
                            // Draft sits inside the box, top right, beside the message.
                            .debug_selector(|| "commit-box".into())
                            .flex()
                            .items_start()
                            .gap(px(4.))
                            .child(div().flex_1().min_w_0().debug_selector(|| "commit-text".into()).child(self.commit_input.clone()))
                            .child(git::draft::icon("commit-draft", self.drafter.running()).on_click(cx.listener(|panel, _, _, cx| {
                                let root = panel.repo.read(cx).root.clone();
                                panel.drafter.toggle(root, &panel.commit_input, cx);
                            }))),
                    )
                    .child(
                        div()
                            .flex()
                            .h(px(26.))
                            .rounded(px(2.))
                            .bg(theme::accent())
                            .text_color(theme::on(theme::accent()))
                            .child(
                                div()
                                    .id("commit-button")
                                    .flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(6.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::accent_hover()))
                                    .on_click(cx.listener(|panel, _, window, cx| {
                                        let amend = panel.amend;
                                        panel.commit(None, false, amend, window, cx)
                                    }))
                                    .child(ui::icon("check", theme::on(theme::accent())))
                                    .child(busy.unwrap_or(commit_label.to_string())),
                            )
                            .child(div().w(px(1.)).my(px(4.)).bg(theme::ca(0xffffff55)))
                            .child(
                                div()
                                    .id("commit-menu")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(26.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::accent_hover()))
                                    .debug_selector(|| "commit-menu".into())
                                    .on_click(cx.listener(|panel, _, _, cx| {
                                        panel.commit_menu_open = !panel.commit_menu_open;
                                        cx.notify();
                                    }))
                                    .child(ui::small_icon("chevron-down", theme::on(theme::accent()))),
                            )
                            .relative()
                            .when(self.commit_menu_open, |d| d.child(deferred(self.render_commit_menu(cx)).with_priority(1))),
                    )
                    .when(branch.is_empty(), |d| d.child(div().text_color(theme::fg_muted()).child("Not a git repository"))),
            )
            .when(clean, |d| {
                d.child(
                    div()
                        .id("git-clean")
                        .debug_selector(|| "git-clean".into())
                        // In the middle of the space the changes would take.
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .px(px(20.))
                        .py(px(12.))
                        .text_center()
                        .text_size(px(12.))
                        .text_color(theme::fg_dim())
                        .child(ui::icon("check", theme::accent()).size(px(22.)))
                        .child(div().text_size(px(14.)).text_color(theme::fg_bright()).child("Nothing to commit"))
                        .child(format!("The working tree is clean on {branch}. Changes you make show up here."))
                )
            })
            .when(!clean, |d| d.children(rows.iter().enumerate().map(|(ix, row)| self.render_list_row(row, ix, focused, cx))))
    }

    fn reachability(&self, cx: &App) -> Rc<(Vec<bool>, Option<usize>)> {
        let repo = self.repo.read(cx);
        let head = repo.status.head_oid.clone().unwrap_or_default();
        let key = std::sync::Arc::as_ptr(&repo.commits) as usize;
        let mut cache = self.reach.borrow_mut();
        if let Some((h, k, r)) = cache.as_ref()
            && *h == head
            && *k == key
        {
            return r.clone();
        }
        let r = Rc::new(in_checkout(&repo.commits, &head));
        *cache = Some((head, key, r.clone()));
        r
    }

    fn render_graph_row(&self, ix: usize, row: &GraphRow, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let reach = self.reachability(cx);
        let repo = self.repo.read(cx);
        let Some(commit) = repo.commits.get(ix) else { return div().into_any_element() };
        let head_oid = repo.status.head_oid.clone().unwrap_or_default();
        let is_head = commit.sha == head_oid;
        // Commits the checkout doesn't have (newer than HEAD, unmerged branches) are muted.
        let contained = reach.0.get(ix).copied().unwrap_or(true) || reach.1.is_none();
        let above_head = reach.1.is_some_and(|h| ix < h);
        let detached = is_head && !commit.refs.iter().any(|r| r.is_head);
        let selected = self.selected == Some(Item::Commit(ix));
        let colors = theme::lane_colors();
        let is_merge = commit.parents.len() > 1;
        let bg = if selected { if focused { theme::selected_focused() } else { theme::selected() } } else { theme::bg_side() };

        let graph = graph_cell(row, is_head, is_merge, contained, above_head, bg);

        // A local branch and its same-named remote twin show as one chip with a cloud mark.
        let badges = ui::ref_labels(&commit.refs).into_iter().map(|(r, cloud, ri)| {
            let menu_ref = r.clone();
            ui::ref_chip(r, cloud, 18.)
                .id(SharedString::from(format!("ref-{ix}-{ri}")))
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_mouse_down(MouseButton::Right, cx.listener(move |panel, ev: &MouseDownEvent, _, cx| {
                    // Right-click a branch/tag label: checkout, detached checkout or delete.
                    cx.stop_propagation();
                    let items = panel.refs.read(cx).ref_menu(&menu_ref, cx);
                    panel.refs.update(cx, |t, cx| t.open_menu(items, ev.position, cx));
                }))
        });

        let lane_color = colors[row.color % colors.len()];
        let sha = commit.sha.clone();
        let short = commit.short().to_string();
        let card = CommitCard {
            subject: commit.subject.clone().into(),
            body: commit.body.clone().into(),
            author: commit.author.clone().into(),
            when: crate::util::relative_time(commit.time).into(),
            short: short.clone().into(),
        };
        div()
            .id(("commit", ix))
            .debug_selector(move || format!("commit-row-{short}"))
            .tooltip(move |_, cx| cx.new(|_| card.clone()).into())
            .w_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .h(px(GRAPH_ROW_H))
            .pl(px(4.))
            .pr(px(8.))
            .gap(px(4.))
            .cursor_pointer()
            .bg(bg)
            .when(is_head && !selected, |d| d.bg(lane_color.opacity(0.12)))
            .when(selected && focused, |d| d.border_1().border_color(theme::focus_border()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |panel, ev: &ClickEvent, window, cx| {
                window.focus(&panel.focus, cx);
                panel.selected = Some(Item::Commit(ix));
                if panel.repo.read(cx).graph_file.is_some() {
                    // A file's history: each commit opens that file's change.
                    panel.open_history_diff(ix, ev.click_count() < 2, cx);
                    return cx.notify();
                }
                // One click lists the commit's files under it; the second click of a
                // double-click puts that back and opens the commit page instead.
                panel.toggle_commit(&sha, cx);
                if ev.click_count() >= 2 {
                    cx.emit(Request::OpenCommit(sha.clone()));
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |panel, ev: &MouseDownEvent, _, cx| {
                // Right-click any commit: checkout, branch from it, cherry-pick, revert…
                panel.selected = Some(Item::Commit(ix));
                panel.open_commit_menu(ix, ev.position, cx);
            }))
            .child(graph)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    // A long subject ends in "…" (the whole message shows on hover).
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(if is_head { theme::fg_bright() } else if contained { theme::fg() } else { theme::fg_muted() })
                            .when(is_head, |d| d.font_weight(FontWeight::BOLD))
                            .child(commit.subject.clone()),
                    )
                    // The author is small and gives way first, so the message stays readable.
                    .child(
                        div()
                            .min_w_0()
                            .max_w(relative(0.28))
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(px(9.5))
                            .text_color(theme::fg_muted())
                            .when(!contained, |d| d.opacity(0.6))
                            .map(|mut d| {
                                d.style().flex_shrink = Some(6.);
                                d
                            })
                            .child(commit.author.clone()),
                    ),
            )
            .when(detached, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .px(px(6.))
                        .h(px(16.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(lane_color)
                        .text_color(lane_color)
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .child("HEAD"),
                )
            })
            .child(div().flex().flex_none().gap(px(3.)).children(badges))
            .into_any_element()
    }

    /// A row under an expanded commit: one changed file (`file` is None while they're read).
    /// The graph's lanes run straight through it, so the history stays connected.
    fn render_commit_file_row(&self, list_ix: usize, ix: usize, file: Option<usize>, row: &GraphRow, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let reach = self.reachability(cx);
        let repo = self.repo.read(cx);
        let Some(commit) = repo.commits.get(ix) else { return div().into_any_element() };
        let is_head = repo.status.head_oid.as_deref() == Some(commit.sha.as_str());
        let muted = reach.1.is_some_and(|h| ix < h) && !is_head;
        let colors = theme::lane_colors();
        let lanes: Vec<(usize, Hsla)> = row
            .bottom
            .iter()
            .map(|s| {
                let c = colors[s.color % colors.len()];
                (s.to, if muted { c.opacity(MUTED) } else { c })
            })
            .collect();
        let graph_w = row.width.max(1) as f32 * LANE_W + 8.;
        let lines = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                for (lane, color) in &lanes {
                    let x = bounds.left() + px(6. + *lane as f32 * LANE_W);
                    let mut pb = PathBuilder::stroke(px(1.6));
                    pb.move_to(point(x, bounds.top()));
                    pb.line_to(point(x, bounds.bottom()));
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, *color);
                    }
                }
            },
        )
        .w(px(graph_w))
        .h(px(GRAPH_ROW_H))
        .flex_none();

        let base = div()
            .w_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .h(px(GRAPH_ROW_H))
            .pl(px(4.))
            .pr(px(8.))
            .gap(px(4.))
            .child(lines);
        let Some(fi) = file else {
            return base.child(div().pl(px(8.)).text_color(theme::fg_muted()).child("Loading…")).into_any_element();
        };
        let Some(change) = self.commit_files.get(&commit.sha).and_then(|f| f.files.get(fi)).cloned() else {
            return base.into_any_element();
        };
        let selected = self.selected == Some(Item::CommitFile(ix, fi));
        let name = ui::file_name(&change.path).to_string();
        let dir = ui::file_dir(&change.path).to_string();
        let selector = format!("commit-file-{}", change.path);
        base.id(("commit-file", list_ix))
            .debug_selector(move || selector)
            .cursor_pointer()
            .when(selected, |d| {
                d.bg(if focused { theme::selected_focused() } else { theme::selected() })
                    .when(focused, |d| d.border_1().border_color(theme::focus_border()))
            })
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |panel, ev: &ClickEvent, window, cx| {
                // Like the changes list: one click shows the diff and keeps the keyboard here,
                // a double-click opens it for editing.
                window.focus(&panel.focus, cx);
                panel.selected = Some(Item::CommitFile(ix, fi));
                panel.open_commit_file(ix, fi, ev.click_count() < 2, cx);
                cx.notify();
            }))
            .child(div().w(px(4.)).flex_none())
            .child(ui::file_icon(&name))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .text_color(ui::status_color(change.status))
                            .when(change.status == FileStatus::Deleted, |d| d.line_through())
                            .child(name),
                    )
                    .when(!dir.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(dir))),
            )
            .child(div().flex_none().text_size(px(11.)).text_color(ui::status_color(change.status)).child(change.status.letter()))
            .into_any_element()
    }
}

/// The whole message of a commit, shown when the pointer rests on it in the graph.
#[derive(Clone)]
struct CommitCard {
    subject: SharedString,
    body: SharedString,
    author: SharedString,
    when: SharedString,
    short: SharedString,
}

impl Render for CommitCard {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A set width (from the longest line, up to 420): with only a maximum, the height is
        // measured for the text on one line and the wrapped lines run out below the card.
        let longest = self.subject.chars().count().max(self.body.lines().map(|l| l.chars().count()).max().unwrap_or(0)).max(self.author.chars().count() + self.when.chars().count() + 12);
        let width = (longest as f32 * 7. + 24.).clamp(200., 420.);
        div()
            .debug_selector(|| "commit-card".into())
            .flex()
            .flex_col()
            .gap(px(6.))
            .w(px(width))
            .px(px(10.))
            .py(px(8.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_md()
            .text_size(px(12.))
            .text_color(theme::fg())
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .text_color(theme::fg_muted())
                    .child(div().text_color(theme::fg()).font_weight(FontWeight::BOLD).child(self.author.clone()))
                    .child(self.when.clone())
                    .child(div().font_family(theme::MONO_FONT).child(self.short.clone())),
            )
            .child(div().w_full().text_color(theme::fg_bright()).debug_selector(|| "commit-card-subject".into()).child(self.subject.clone()))
            .when(!self.body.is_empty(), |d| d.child(div().w_full().text_color(theme::fg_dim()).debug_selector(|| "commit-card-body".into()).child(self.body.clone())))
            .with_animation("commit-card", Animation::new(std::time::Duration::from_millis(120)).with_easing(ease_out_quint()), |d, t| d.opacity(t))
    }
}

impl Focusable for GitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let repo = self.repo.read(cx);
        let graph = repo.graph.clone();
        let commit_count = repo.commits.len();
        let graph_all = repo.graph_all;
        let graph_only_empty = repo.graph_only.is_empty();
        let bounds_cell = self.panel_bounds.clone();

        let changes_header = ui::section_header("changes-header", "Changes", self.changes_open, cx.listener(|p, _, _, cx| {
                p.changes_open = !p.changes_open;
                cx.notify();
            }))
            .group("changes-header")
            .child(
                div()
                    .flex()
                    .gap(px(1.))
                    // Staged and unstaged changes alike, as a tree or a list.
                    .child({
                        let tree = self.as_tree;
                        ui::icon_button("changes-view", if tree { "list-flat" } else { "list-tree" })
                            .debug_selector(|| "changes-view".into())
                            .tooltip(ui::tooltip(if tree { "View as list" } else { "View as tree" }))
                            .on_click(cx.listener(|panel, _, _, cx| {
                                cx.stop_propagation();
                                panel.as_tree = !panel.as_tree;
                                cx.notify();
                            }))
                    })
                    .child(ui::icon_button("hdr-refresh", "refresh").on_click(cx.listener(|p, _, _, cx| {
                        cx.stop_propagation();
                        p.repo.update(cx, |r, cx| r.refresh_all(cx));
                    }))),
            );

        let graph_header = ui::section_header("graph-header", "Graph", self.graph_open, cx.listener(|p, _, _, cx| {
                p.graph_open = !p.graph_open;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(1.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .mr(px(4.))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme::fg_dim())
                            .child(ui::small_icon("git", theme::fg_dim()))
                            .child(format!("{commit_count} Items")),
                    )
                    .child(
                        ui::icon_button("graph-search", "search").debug_selector(|| "graph-search".into())
                            .tooltip(ui::tooltip("Search commits by message, author or hash"))
                            .when(self.search_open, |b| b.bg(theme::selected()))
                            .on_click(cx.listener(|p, _, w, cx| {
                                cx.stop_propagation();
                                p.search_open = !p.search_open;
                                if p.search_open {
                                    w.focus(&p.search_input.read(cx).focus_target(cx), cx);
                                } else {
                                    p.search_graph("", w, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::icon_button("graph-scope", "filter").debug_selector(|| "graph-scope".into())
                            .tooltip(ui::tooltip("Choose branches shown in the graph"))
                            .when(graph_all || !graph_only_empty, |b| b.bg(theme::selected()))
                            .on_click(cx.listener(|p, _, _, cx| {
                                cx.stop_propagation();
                                p.scope_open = !p.scope_open;
                                if p.scope_open {
                                    let root = p.repo.read(cx).root.clone();
                                    (p.scope_local, p.scope_remote) = git::branches_split(&root);
                                }
                                cx.notify();
                            })),
                    )
                    .child(ui::icon_button("graph-fetch", "fetch").on_click(cx.listener(|p, _, _, cx| {
                        cx.stop_propagation();
                        p.repo.update(cx, |r, cx| r.run_git(vec!["fetch".into(), "--all".into(), "--prune".into()], cx));
                    })))
                    .child(ui::icon_button("graph-pull", "arrow-down").on_click(cx.listener(|p, _, _, cx| {
                        cx.stop_propagation();
                        p.repo.update(cx, |r, cx| r.run_git(vec!["pull".into()], cx));
                    })))
                    .child(ui::icon_button("graph-push", "arrow-up").on_click(cx.listener(|p, _, _, cx| {
                        cx.stop_propagation();
                        p.repo.update(cx, |r, cx| r.run_git(vec!["push".into()], cx));
                    })))
                    .child(ui::icon_button("graph-refresh", "refresh").on_click(cx.listener(|p, _, _, cx| {
                        cx.stop_propagation();
                        p.repo.update(cx, |r, cx| r.refresh_log(cx));
                    }))),
            );

        let both_open = self.changes_open && self.graph_open;

        div()
            .key_context(if self.typing(window, cx) { "GitPanel typing" } else { "GitPanel" })
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::top))
            .on_action(cx.listener(Self::bottom))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::toggle_stage))
            .on_action(cx.listener(Self::discard))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::on_focus_commit))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_mouse_move(cx.listener(|p, ev: &MouseMoveEvent, _, cx| {
                if p.dragging_split {
                    if !ev.dragging() {
                        p.dragging_split = false;
                        return;
                    }
                    let b = p.panel_bounds.get();
                    let h = b.size.height / gpui::px(1.);
                    if h > 0. {
                        let head = p.head_h() * ui::scale();
                        p.split = (((ev.position.y - b.top()) / gpui::px(1.) - head) / (h - head)).clamp(0.12, 0.88);
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|p, _, _, _| p.dragging_split = false))
            .relative()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {}).absolute().size_full())
            .when(self.menu_open, |d| d.child(deferred(self.render_menu(cx)).with_priority(1)))
            .when(self.scope_open, |d| d.child(deferred(self.render_scope(cx)).with_priority(1)))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .px(px(20.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::fg_bright())
                    .child(div().flex_1().child("Source control"))
                    .child(ui::icon_button("scm-menu", "ellipsis").on_click(cx.listener(|p, _, _, cx| {
                        p.menu_open = !p.menu_open;
                        cx.notify();
                    }))),
            )
            .when(self.repos.len() > 1, |d| d.child(self.render_repo_picker(cx)))
            .when(self.repo_menu && self.repos.len() > 1, |d| d.child(deferred(self.render_repo_menu(cx)).with_priority(2)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    // Both open: a resizable split. Only changes open: it takes all the room
                    // and the Graph header stays pinned at the bottom so it can be reopened.
                    .when(both_open, |d| d.flex_none().h(relative(self.split)))
                    .when(self.changes_open && !self.graph_open, |d| d.flex_1())
                    .when(!self.changes_open, |d| d.flex_none())
                    .child(changes_header)
                    .when(self.changes_open, |d| d.child(div().flex().flex_col().flex_1().min_h_0().child(self.render_changes(focused, window, cx)))),
            )
            .when(both_open, |d| {
                d.child(
                    div()
                        .id("split")
                        .flex_none()
                        .h(px(4.))
                        .border_t_1()
                        .border_color(theme::border())
                        .cursor(CursorStyle::ResizeUpDown)
                        .hover(|s| s.bg(theme::focus_border()))
                        .on_mouse_down(MouseButton::Left, cx.listener(|p, _, _, cx| {
                            p.dragging_split = true;
                            cx.stop_propagation();
                        })),
                )
            })
            .when(!both_open, |d| d.child(div().flex_none().h(px(1.)).bg(theme::border())))
            // Under the split, so dragging it still follows the mouse.
            .when(self.repo.read(cx).is_git, |d| d.child(self.render_refs(cx)).child(div().flex_none().h(px(1.)).bg(theme::border())))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .when(self.graph_open, |d| d.flex_1())
                    .when(!self.graph_open, |d| d.flex_none())
                    .child(graph_header)
                    .when(self.graph_open && self.search_open, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(6.))
                                .mx(px(8.))
                                .mb(px(4.))
                                .px(px(6.))
                                .h(px(24.))
                                .rounded(px(3.))
                                .bg(theme::bg_input())
                                .border_1()
                                .border_color(theme::border_input())
                                .child(ui::small_icon("search", theme::fg_muted()))
                                .child(div().flex_1().min_w_0().overflow_hidden().child(self.search_input.clone())),
                        )
                    })
                    .when_some(self.repo.read(cx).graph_file.as_ref().map(|f| f.0.clone()), |d, file| {
                        d.child(
                            div()
                                .id("history-chip")
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(6.))
                                .mx(px(8.))
                                .mb(px(4.))
                                .px(px(8.))
                                .h(px(22.))
                                .rounded(px(3.))
                                .bg(theme::selected())
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(ui::small_icon("history", theme::fg()))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(format!("History of {file}")))
                                .child(ui::icon_button("history-close", "close").debug_selector(|| "history-close".into()).size(px(18.)).tooltip(ui::tooltip("Back to the whole graph")).on_click(cx.listener(|p, _, _, cx| {
                                    p.show_history(None, cx);
                                }))),
                        )
                    })
                    .when(self.graph_open, |d| {
                        let items = Rc::new(self.graph_items(cx));
                        d.child(
                            uniform_list(
                                "graph",
                                items.len(),
                                cx.processor(move |panel, range: Range<usize>, _window, cx| {
                                    let repo = panel.repo.downgrade();
                                    let end = range.end;
                                    cx.defer(move |cx| {
                                        repo.update(cx, |r, cx| r.want_more(end, cx)).ok();
                                    });
                                    range
                                        .filter_map(|i| {
                                            Some(match items[i] {
                                                GraphItem::Commit(ix) => panel.render_graph_row(ix, graph.get(ix)?, focused, cx),
                                                GraphItem::File(ix, fi) => panel.render_commit_file_row(i, ix, Some(fi), graph.get(ix)?, focused, cx),
                                                GraphItem::Loading(ix) => panel.render_commit_file_row(i, ix, None, graph.get(ix)?, focused, cx),
                                            })
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .track_scroll(&self.graph_scroll)
                            .flex_1(),
                        )
                    }),
            )
    }
}

/// One row's lanes and dot, for any history list (the panel's graph, the Git tab's table).
pub(crate) fn graph_cell(row: &GraphRow, is_head: bool, is_merge: bool, contained: bool, above_head: bool, bg: Hsla) -> impl IntoElement {
    graph_cell_h(row, is_head, is_merge, contained, above_head, bg, GRAPH_ROW_H)
}

/// `graph_cell` for rows `h` tall (the lines run the whole height, so rows touch).
pub(crate) fn graph_cell_h(row: &GraphRow, is_head: bool, is_merge: bool, contained: bool, above_head: bool, bg: Hsla, h: f32) -> impl IntoElement {
    let colors = theme::lane_colors();
    let row2 = row.clone();
    canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let x = |lane: usize| bounds.left() + px(6. + lane as f32 * LANE_W);
                let top = bounds.top();
                let mid = bounds.top() + px(h / 2.);
                let bottom = bounds.bottom();
                let mut stroke = |a: Point<Pixels>, b: Point<Pixels>, ctrl: Option<Point<Pixels>>, color: Hsla| {
                    let mut pb = PathBuilder::stroke(px(1.6));
                    pb.move_to(a);
                    match ctrl {
                        Some(c) => pb.curve_to(b, c),
                        None => pb.line_to(b),
                    }
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, color);
                    }
                };
                let line_color = |i: usize| {
                    let c = colors[i % colors.len()];
                    if above_head { c.opacity(MUTED) } else { c }
                };
                for s in &row2.top {
                    let (a, b) = (point(x(s.from), top), point(x(s.to), mid));
                    stroke(a, b, (s.from != s.to).then(|| point(x(s.from), mid)), line_color(s.color));
                }
                for s in &row2.bottom {
                    let (a, b) = (point(x(s.from), mid), point(x(s.to), bottom));
                    // Below HEAD's dot the line is the checkout's own history again.
                    let c = if is_head { colors[s.color % colors.len()] } else { line_color(s.color) };
                    stroke(a, b, (s.from != s.to).then(|| point(x(s.to), mid)), c);
                }
                let full = colors[row2.color % colors.len()];
                let color = if contained { full } else { full.opacity(MUTED) };
                let c = point(x(row2.lane), mid);
                let circle = |r: f32, color: Hsla| fill(Bounds::new(point(c.x - px(r), c.y - px(r)), size(px(r * 2.), px(r * 2.))), color).corner_radii(px(r));
                if is_head {
                    // Current commit: a glowing ring around a solid dot with a bright core.
                    window.paint_quad(circle(9., full.opacity(0.16)));
                    window.paint_quad(circle(7., full.opacity(0.35)));
                    window.paint_quad(circle(5.5, full));
                    window.paint_quad(circle(2.2, theme::c(0xffffff)));
                } else if is_merge {
                    window.paint_quad(circle(5., color));
                    window.paint_quad(circle(3.2, bg));
                    window.paint_quad(circle(1.8, color));
                } else {
                    window.paint_quad(circle(4., color));
                }
            },
        )
    .w(px(row.width.max(1) as f32 * LANE_W + 8.))
    .h(px(h))
    .flex_none()
}

/// The folder all these paths sit in (to show each repository by where it is).
fn common_parent(paths: impl Iterator<Item = std::path::PathBuf>) -> Option<std::path::PathBuf> {
    let mut common: Option<std::path::PathBuf> = None;
    for p in paths {
        let parent = p.parent().map(|x| x.to_path_buf()).unwrap_or(p);
        common = Some(match common {
            None => parent,
            Some(c) => c.ancestors().find(|a| parent.starts_with(a)).map(|a| a.to_path_buf()).unwrap_or_default(),
        });
    }
    common
}

#[cfg(test)]
mod tree_tests {
    use super::{ListRow, flat_rows, tree_rows};
    use crate::git::{Change, FileStatus};

    fn change(p: &str) -> Change {
        Change { path: p.into(), orig_path: None, status: FileStatus::Modified }
    }

    fn outline(rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                ListRow::Folder { label, depth, .. } => format!("{}{label}/", "  ".repeat(*depth)),
                ListRow::File { change, depth, .. } => format!("{}{}", "  ".repeat(*depth), crate::ui::file_name(&change.path)),
                ListRow::Section { .. } => String::new(),
            })
            .collect()
    }

    #[test]
    fn discard_prompt_wording() {
        use super::{DiscardWhat, discard_prompt};
        let t = vec!["docs/sprint-new-features3.md".to_string()];
        let u = vec!["a.txt".to_string(), "b.txt".to_string()];
        let both = discard_prompt(&t, &u).unwrap();
        assert_eq!(both.message, "Are you sure you want to DELETE the 2 untracked files? You can restore these files from the Trash.");
        assert!(both.detail.starts_with("Are you sure you want to discard changes in 'sprint-new-features3.md'?"));
        assert_eq!(both.buttons, vec![("Discard All 3 Files".into(), DiscardWhat::All), ("Discard 1 Tracked File".into(), DiscardWhat::TrackedOnly)]);
        let only_u = discard_prompt(&[], &u[..1]).unwrap();
        assert_eq!(only_u.message, "Are you sure you want to DELETE 'a.txt'?");
        assert_eq!(only_u.buttons[0].0, "Move to Trash");
        let only_t = discard_prompt(&t, &[]).unwrap();
        assert_eq!(only_t.buttons[0].0, "Discard File");
        assert!(discard_prompt(&[], &[]).is_none());
    }

    #[test]
    fn checkout_contains_head_and_ancestors_only() {
        let c = |sha: &str, parents: &[&str]| crate::git::Commit {
            sha: sha.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: String::new(),
            time: 0,
            refs: Vec::new(),
            subject: String::new(),
            body: String::new(),
        };
        // newer (not checked out) → head → merge of side into base
        let commits = vec![c("new", &["head"]), c("other", &["base"]), c("head", &["base", "side"]), c("side", &["base"]), c("base", &[])];
        let (seen, head) = super::in_checkout(&commits, "head");
        assert_eq!(head, Some(2));
        assert_eq!(seen, vec![false, false, true, true, true]);
    }

    #[test]
    fn the_flat_list_has_no_folders_and_goes_by_name() {
        let changes = [change("src/lib/b.ts"), change("README.md"), change("a/a.ts")];
        assert_eq!(outline(&flat_rows(&changes, false)), vec!["README.md", "a.ts", "b.ts"]);
    }

    #[test]
    fn nested_and_compacted_like_vscode() {
        let changes = [change("src/lib/api.test.ts"), change("src/lib/auth/auth.types.ts"), change("README.md")];
        let rows = tree_rows(&changes, false, "changes", &Default::default());
        assert_eq!(outline(&rows), vec!["src / lib/", "  auth/", "    auth.types.ts", "  api.test.ts", "README.md"]);
        let collapsed = ["changes:src/lib/auth".to_string()].into_iter().collect();
        let rows = tree_rows(&changes, false, "changes", &collapsed);
        assert_eq!(outline(&rows), vec!["src / lib/", "  auth/", "  api.test.ts", "README.md"]);
    }
}

/// The repository shown, with its branch: a click opens the menu of the others. Source Control
/// and Pull requests both show it, for the same repository.
pub(crate) fn repo_picker(id: &'static str, repo: &Entity<Repo>, open: bool, cx: &App) -> Stateful<Div> {
    let repo = repo.read(cx);
    let (name, branch) = (repo.name(), repo.branch_label());
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(REPO_H - 6.))
        .mx(px(10.))
        .mb(px(6.))
        .px(px(8.))
        .rounded(px(4.))
        .bg(theme::ca(0xffffff0d))
        .border_1()
        .border_color(if open { theme::focus_border() } else { theme::border_input() })
        .cursor_pointer()
        .hover(|s| s.bg(theme::ca(0xffffff17)))
        .tooltip(ui::tooltip("Repository shown in Source Control and Pull requests"))
        .child(ui::small_icon("repo", theme::fg_dim()))
        .child(div().flex_none().text_color(theme::fg_bright()).child(name))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme::fg_dim()).child(branch))
        .child(ui::small_icon("chevron-down", theme::fg_dim()))
}

/// The repositories to pick from, under the picker (`top`: where the picker's row starts);
/// picking one asks the workspace to show it everywhere (Source Control and Pull requests).
pub(crate) fn repo_menu<V: EventEmitter<Request>>(prefix: &'static str, repos: &[Entity<Repo>], current: &Entity<Repo>, top: f32, cx: &mut Context<V>) -> Stateful<Div> {
    let common = common_parent(repos.iter().map(|r| r.read(cx).root.clone()));
    let items: Vec<AnyElement> = repos
        .iter()
        .enumerate()
        .map(|(ix, r)| {
            let active = r == current;
            let repo = r.read(cx);
            let root = repo.root.clone();
            let rel = common.as_ref().and_then(|c| root.parent()?.strip_prefix(c).ok()).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let changes = repo.status.change_count();
            let name = repo.name();
            let (label, branch) = (repo.name(), repo.branch_label());
            div()
                .id((SharedString::from(format!("{prefix}-item")), ix))
                .debug_selector(move || format!("{prefix}-item-{name}"))
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(26.))
                .px(px(8.))
                .mx(px(4.))
                .rounded(px(3.))
                .cursor_pointer()
                .when(active, |d| d.bg(theme::selected()))
                .hover(|s| s.bg(theme::selected_focused()))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::SelectRepo(root.clone()))))
                .child(div().w(px(10.)).text_color(theme::frame_focused()).child(if active { "●" } else { "" }))
                .child(div().flex_none().text_color(if active { theme::fg_bright() } else { theme::fg() }).child(label))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme::fg_muted()).child(rel))
                .child(div().flex_none().flex().items_center().gap(px(3.)).text_color(theme::fg_dim()).child(ui::small_icon("git", theme::fg_dim()).size(px(11.))).child(branch))
                .when(changes > 0, |d| d.child(ui::badge(changes.to_string(), theme::accent()).text_size(px(10.)).h(px(16.)).min_w(px(16.))))
                .into_any_element()
        })
        .collect();
    div()
        .id(SharedString::from(format!("{prefix}-menu")))
        .occlude()
        .absolute()
        .top(px(top + REPO_H - 4.))
        .left(px(10.))
        .right(px(10.))
        .max_h(px(420.))
        .py(px(4.))
        .bg(theme::bg_popup())
        .border_1()
        .border_color(theme::border_input())
        .rounded(px(6.))
        .shadow_lg()
        .overflow_y_scroll()
        .children(items)
}
