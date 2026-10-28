//! Source Control panel: commit box, staged/unstaged changes and the commit graph.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::{self, Change, FileStatus, RefKind, Rev};
use crate::graph::GraphRow;
use crate::repo::Repo;
use crate::request::{DiffSpec, Request};
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

actions!(git_panel, [Down, Up, Open, ToggleStage, Discard, OpenFile, FocusCommit, Refresh, Top, Bottom]);

pub fn bind_keys() {
    let c = Some("GitPanel");
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
    ]);
}

#[derive(Clone, PartialEq, Eq)]
enum Item {
    File { staged: bool, path: String },
    Commit(usize),
}

#[derive(Clone)]
enum ListRow {
    Section { staged: bool, conflicts: bool, count: usize },
    /// `label` is the folder name, or a compacted chain like "src / lib" (VS Code style).
    Folder { staged: bool, key: String, label: String, depth: usize, open: bool, paths: Vec<String> },
    File { staged: bool, change: Change, depth: usize },
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
    branch_menu: Option<(git::Ref, Point<Pixels>)>,
    commit_menu: Option<(usize, Point<Pixels>)>,
    scope_open: bool,
    scope_branches: Vec<String>,
    scope_remotes: Vec<String>,
    /// Last operation seen, to prefill the message once when one starts.
    last_op: Option<git::OpState>,
    /// Collapsed folders in the changes tree ("section:dir").
    collapsed: std::collections::HashSet<String>,
    /// `in_checkout` for the current log and HEAD, recomputed when either changes.
    reach: std::cell::RefCell<Option<(String, usize, Rc<(Vec<bool>, Option<usize>)>)>>,
    /// Every repository in the workspace (the picker's choices) and whether the picker is open.
    repos: Vec<Entity<Repo>>,
    repo_menu: bool,
    /// Unsent commit messages of the repositories not shown.
    drafts: std::collections::HashMap<std::path::PathBuf, String>,
    repo_sub: Subscription,
    _repos_subs: Vec<Subscription>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for GitPanel {}

impl GitPanel {
    pub fn new(repo: Entity<Repo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commit_input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::MultiLine, cx);
            ed.font_size = 12.;
            ed
        });
        let repo_sub = Self::observe_repo(&repo, cx);
        let subs = vec![
            cx.subscribe_in(&commit_input, window, |panel, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => panel.commit(None, false, panel.amend, window, cx),
                EditorEvent::Cancel => panel.focus(window),
                _ => {}
            }),
        ];
        Self {
            focus: cx.focus_handle(),
            repos: vec![repo.clone()],
            repo,
            commit_input,
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
            branch_menu: None,
            commit_menu: None,
            scope_open: false,
            scope_branches: Vec::new(),
            scope_remotes: Vec::new(),
            last_op: None,
            collapsed: Default::default(),
            reach: Default::default(),
            repo_menu: false,
            drafts: Default::default(),
            repo_sub,
            _repos_subs: Vec::new(),
            _subs: subs,
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
            self.reach.replace(None);
            self.scope_open = false;
            self.branch_menu = None;
            self.commit_menu = None;
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
        let repo = self.repo.read(cx);
        let (name, branch) = (repo.name(), repo.branch_label());
        div()
            .id("repo-picker")
            .debug_selector(|| "repo-picker".into())
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
            .border_color(if self.repo_menu { theme::focus_border() } else { theme::border_input() })
            .cursor_pointer()
            .hover(|s| s.bg(theme::ca(0xffffff17)))
            .tooltip(ui::tooltip("Repository shown here"))
            .on_click(cx.listener(|p, _, _, cx| {
                p.repo_menu = !p.repo_menu;
                cx.notify();
            }))
            .child(ui::small_icon("repo", theme::fg_dim()))
            .child(div().flex_none().text_color(theme::fg_bright()).child(name))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme::fg_dim()).child(branch))
            .child(ui::small_icon("chevron-down", theme::fg_dim()))
    }

    fn render_repo_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let common = common_parent(self.repos.iter().map(|r| r.read(cx).root.clone()));
        div()
            .id("repo-menu")
            .occlude()
            .absolute()
            .top(px(35. + REPO_H - 4.))
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
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.repo_menu = false;
                cx.notify();
            }))
            .children(self.repos.iter().enumerate().map(|(ix, r)| {
                let active = *r == self.repo;
                let repo = r.read(cx);
                let root = repo.root.clone();
                let rel = common.as_ref().and_then(|c| root.parent()?.strip_prefix(c).ok()).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let changes = repo.status.change_count();
                let name = repo.name();
                div()
                    .id(("repo-item", ix))
                    .debug_selector(move || format!("repo-item-{name}"))
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
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.repo_menu = false;
                        cx.emit(Request::SelectRepo(root.clone()));
                        cx.notify();
                    }))
                    .child(div().w(px(10.)).text_color(theme::frame_focused()).child(if active { "●" } else { "" }))
                    .child(div().flex_none().text_color(if active { theme::fg_bright() } else { theme::fg() }).child(repo.name()))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme::fg_muted()).child(rel))
                    .child(div().flex_none().flex().items_center().gap(px(3.)).text_color(theme::fg_dim()).child(ui::small_icon("git", theme::fg_dim()).size(px(11.))).child(repo.branch_label()))
                    .when(changes > 0, |d| d.child(ui::badge(changes.to_string(), theme::accent()).text_size(px(10.)).h(px(16.)).min_w(px(16.))))
            }))
    }

    #[cfg(test)]
    pub fn changes_scroll_max(&self) -> f32 {
        self.changes_scroll.max_offset().height / px(1.)
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn focus_commit_input(&self, window: &mut Window, cx: &App) {
        self.commit_input.read(cx).focus(window);
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
                    return Ok(Err(findings));
                }
            }
            if amend && message.is_empty() {
                return git::run(&root, &["commit", "--amend", "--no-edit"]).map(Ok);
            }
            git::commit(&root, &message, amend).map(Ok)
        });
        self.commit_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |panel, _, cx| {
                match result {
                    Ok(Err(findings)) => panel.ask_secrets(findings, retry, amend, cx),
                    Ok(Ok(out)) => {
                        panel.commit_input.update(cx, |ed, cx| ed.set_text("", cx));
                        panel.amend = false;
                        let summary = out.lines().next().unwrap_or("Committed").to_string();
                        cx.emit(Request::Message(summary));
                    }
                    Err(e) => cx.emit(Request::Error(format!("Commit failed: {e}"))),
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
            return cx.emit(Request::Error(format!("Resolve {n} conflicted file{} first (Merge Changes)", if n == 1 { "" } else { "s" })));
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
            format!("{conflicts} conflicted file{} — open to resolve, save to mark resolved", if conflicts == 1 { "" } else { "s" })
        } else {
            format!("All conflicts resolved — {}", op.kind.continue_label())
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

    fn open_change(&self, staged: bool, change: &Change, preview: bool, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        if matches!(change.status, FileStatus::Untracked | FileStatus::Conflicted) {
            return cx.emit(Request::OpenFile { path: root.join(&change.path), line: None, preview });
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
            spec: DiffSpec { path: change.path.clone(), old_path: change.orig_path.clone(), left, right, from_commit: None },
            preview,
        });
    }

    // ------------------------------------------------------------ list model

    fn list_rows(&self, cx: &App) -> Vec<ListRow> {
        let repo = self.repo.read(cx);
        let st = &repo.status;
        let mut rows = Vec::new();
        let collapsed = &self.collapsed;
        let group = |rows: &mut Vec<ListRow>, staged: bool, section: &str, changes: &[Change]| rows.extend(tree_rows(changes, staged, section, collapsed));
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
            items.extend((0..self.repo.read(cx).commits.len()).map(Item::Commit));
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
        if let Item::Commit(ix) = item {
            self.graph_scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
        self.selected = Some(item);
        cx.notify();
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
            _ => self.repo.update(cx, |r, cx| r.refresh_all(cx)),
        }
        cx.notify();
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let graph_all = self.repo.read(cx).graph_all;
        let items: [(&str, &str); 17] = [
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
        ];
        div()
            .id("scm-menu-popup")
            .occlude()
            .absolute()
            .top(px(32.))
            .right(px(8.))
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
                    .when(matches!(ix, 4 | 7 | 10 | 12 | 14), |d| d.mt(px(4.)).border_t_1().border_color(theme::border()))
                    .on_click(cx.listener(move |p, _, w, cx| p.run_menu(ix, w, cx)))
                    .child(div().flex_1().child(label.to_string()))
                    .child(div().text_color(theme::fg_muted()).child(hint.to_string()))
            }))
    }

    /// Right-click menu for any commit in the graph.
    fn render_commit_menu(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let Some(commit) = repo.commits.get(ix).cloned() else { return div().into_any_element() };
        let sha = commit.sha.clone();
        let short = commit.short().to_string();
        let item = |id: SharedString, label: String| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(3.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::selected_focused()))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label)
        };
        let run = |args: Vec<String>| {
            move |p: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                p.commit_menu = None;
                let args = args.clone();
                p.repo.update(cx, |repo, cx| repo.run_git(args, cx));
                cx.notify();
            }
        };
        let request = |req: fn(&str) -> Request, arg: String| {
            move |p: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                p.commit_menu = None;
                cx.emit(req(&arg));
                cx.notify();
            }
        };
        let sep = || div().my(px(4.)).h(px(1.)).bg(theme::border());
        let mut menu = div()
            .id("commit-menu")
            .occlude()
            .w(px(290.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(11.))
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.commit_menu = None;
                cx.notify();
            }))
            .child(div().px(px(14.)).py(px(4.)).text_color(theme::fg_muted()).overflow_hidden().whitespace_nowrap().child(format!("{short} {}", commit.subject)))
            .child(item("cm-open".into(), "Open commit".into()).on_click(cx.listener(request(|s| Request::OpenCommit(s.to_string()), sha.clone()))));
        let locals: Vec<&git::Ref> = commit.refs.iter().filter(|r| r.kind == RefKind::Local && !r.is_head).collect();
        if !locals.is_empty() {
            menu = menu.child(sep());
            for (i, r) in locals.iter().enumerate() {
                menu = menu.child(item(SharedString::from(format!("cm-co-{i}")), format!("Checkout {}", r.name)).on_click(cx.listener(run(vec!["switch".into(), r.name.clone()]))));
            }
        }
        menu = menu
            .child(sep())
            .child(item("cm-detach".into(), format!("Checkout {short} (detached)")).on_click(cx.listener(run(vec!["switch".into(), "--detach".into(), sha.clone()]))))
            .child(item("cm-branch".into(), "Create branch here…".into()).on_click(cx.listener(request(|s| Request::CommandLine(format!("branch-from {s} ")), short.clone()))))
            .child(sep())
            .child(item("cm-pick".into(), "Cherry-pick onto current branch".into()).on_click(cx.listener(run(vec!["cherry-pick".into(), sha.clone()]))))
            .child(item("cm-revert".into(), "Revert (new commit undoing it)".into()).on_click(cx.listener(run(vec!["revert".into(), "--no-edit".into(), sha.clone()]))))
            .child(sep())
            .child(item("cm-copy".into(), "Copy hash".into()).on_click(cx.listener(move |p, _, _, cx| {
                p.commit_menu = None;
                cx.write_to_clipboard(ClipboardItem::new_string(sha.clone()));
                cx.emit(Request::Message("Commit hash copied".into()));
                cx.notify();
            })));
        menu.into_any_element()
    }

    /// The menu for a branch/tag label in the graph.
    fn render_branch_menu(&self, r: git::Ref, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |id: &'static str, label: String, enabled: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(3.))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::selected_focused())))
                .when(!enabled, |d| d.text_color(theme::fg_muted()))
                .child(label)
        };
        let name = r.name.clone();
        let git = |panel: &mut Self, args: Vec<String>, cx: &mut Context<Self>| {
            panel.branch_menu = None;
            panel.repo.update(cx, |repo, cx| repo.run_git(args, cx));
            cx.notify();
        };
        let mut menu = div()
            .id("branch-menu")
            .occlude()
            .w(px(260.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(11.))
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.branch_menu = None;
                cx.notify();
            }))
            .child(div().px(px(14.)).py(px(4.)).text_color(theme::fg_muted()).overflow_hidden().whitespace_nowrap().child(name.clone()));
        match r.kind {
            git::RefKind::Local => {
                let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
                menu = menu
                    .child(item("bm-checkout", if r.is_head { "Checked out".into() } else { format!("Checkout {name}") }, !r.is_head).when(!r.is_head, |d| {
                        d.on_click(cx.listener(move |p, _, _, cx| git(p, vec!["switch".into(), n1.clone()], cx)))
                    }))
                    .child(item("bm-detach", "Checkout detached".into(), true).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["switch".into(), "--detach".into(), n2.clone()], cx))))
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::border()))
                    .child(
                        item("bm-delete", if r.is_head { "Can't delete the checked-out branch".into() } else { format!("Delete {name}") }, !r.is_head)
                            .when(!r.is_head, |d| d.text_color(theme::git_deleted()).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["branch".into(), "-d".into(), n3.clone()], cx)))),
                    );
            }
            git::RefKind::Remote => {
                let short = name.split_once('/').map(|(_, b)| b.to_string()).unwrap_or(name.clone());
                let (n1, n2) = (short.clone(), name.clone());
                menu = menu
                    .child(item("bm-checkout", format!("Checkout {short} (tracks {name})"), true).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["switch".into(), n1.clone()], cx))))
                    .child(item("bm-detach", "Checkout detached".into(), true).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["switch".into(), "--detach".into(), n2.clone()], cx))))
                    .child(div().px(px(14.)).py(px(4.)).text_size(px(10.)).text_color(theme::fg_muted()).child("Remote branches can't be deleted from here"));
            }
            git::RefKind::Tag => {
                let (n1, n2) = (name.clone(), name.clone());
                menu = menu
                    .child(item("bm-detach", format!("Checkout {name} (detached)"), true).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["switch".into(), "--detach".into(), n1.clone()], cx))))
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::border()))
                    .child(item("bm-delete", format!("Delete tag {name}"), true).text_color(theme::git_deleted()).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["tag".into(), "-d".into(), n2.clone()], cx))));
            }
        }
        if !r.is_head {
            let current = self.repo.read(cx).branch_label();
            let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
            menu = menu
                .child(div().my(px(4.)).h(px(1.)).bg(theme::border()))
                .child(item("bm-compare", format!("Compare with {current}"), true).on_click(cx.listener(move |p, _, _, cx| {
                    p.branch_menu = None;
                    cx.emit(Request::Compare(n1.clone()));
                    cx.notify();
                })))
                .child(item("bm-merge", format!("Merge into {current}"), true).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["merge".into(), "--no-edit".into(), n2.clone()], cx))))
                .child(item("bm-rebase", format!("Rebase {current} onto this"), true).on_click(cx.listener(move |p, _, _, cx| git(p, vec!["rebase".into(), n3.clone()], cx))));
        }
        menu
    }

    /// Graph filter: Auto, All, or any pick of local and remote branches.
    fn render_scope(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let (all, only) = (repo.graph_all, repo.graph_only.clone());
        let auto = !all && only.is_empty();
        let top = self.panel_bounds.get().size.height / px(1.) * if self.changes_open && self.graph_open { self.split } else { 0. } + self.head_h() + 25.;
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
            .top(px(top))
            .right(px(8.))
            .w(px(270.))
            .max_h(px(420.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .overflow_y_scroll()
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.scope_open = false;
                cx.notify();
            }))
            .child(div().px(px(14.)).py(px(4.)).text_color(theme::fg_muted()).child("SHOW IN GRAPH"))
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
            .child(div().mt(px(4.)).border_t_1().border_color(theme::border()).px(px(14.)).py(px(4.)).text_color(theme::fg_muted()).child("OR PICK BRANCHES"))
            .children(self.scope_branches.iter().enumerate().map(|(ix, b)| {
                let on = only.contains(b);
                let is_remote = self.scope_remotes.iter().any(|r| b.starts_with(&format!("{r}/")));
                let name = b.clone();
                div()
                    .id(("scope-branch", ix))
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
                    .child(ui::small_icon(if is_remote { "cloud" } else { "git" }, if is_remote { theme::ref_remote() } else { theme::ref_local() }).size(px(12.)))
                    .child(div().overflow_hidden().whitespace_nowrap().child(b.clone()))
            }))
    }

    // ------------------------------------------------------------ rendering

    fn render_list_row(&self, row: &ListRow, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        match row {
            ListRow::Section { staged, conflicts, count } => {
                let (staged, count) = (*staged, *count);
                let open = if staged { self.staged_open } else { self.unstaged_open };
                let title = if *conflicts { "Merge Changes" } else if staged { "Staged Changes" } else { "Changes" };
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
                div()
                    .id(("section", ix))
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
                let name = label.rsplit(" / ").next().unwrap_or(label).to_string();
                div()
                    .id(("folder", ix))
                    .group("folder")
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
                    .child(
                        div().invisible().group_hover("folder", |s| s.visible()).child(
                            ui::icon_button(("folder-stage", ix), if staged { "minus" } else { "plus" }).on_click(cx.listener(move |panel, _, _, cx| {
                                cx.stop_propagation();
                                if staged { panel.unstage(paths.clone(), cx) } else { panel.stage(paths.clone(), cx) }
                            })),
                        ),
                    )
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
                let untracked = change.status == FileStatus::Untracked;
                div()
                    .id(("file", ix))
                    .group("file")
                    .flex()
                    .items_center()
                    .h(theme::row_h())
                    .pl(px(TREE_LEFT + *depth as f32 * TREE_INDENT))
                    .pr(px(8.))
                    .gap(px(4.))
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(if focused { theme::selected_focused() } else { theme::selected() })
                            .when(focused, |d| d.border_1().border_color(theme::focus_border()))
                    })
                    .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
                    .on_click(cx.listener(move |panel, ev: &ClickEvent, window, cx| {
                        window.focus(&panel.focus);
                        panel.selected = Some(Item::File { staged, path: c1.path.clone() });
                        panel.open_change(staged, &c1, ev.click_count() < 2, cx);
                        cx.notify();
                    }))
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
                            .child(
                                div()
                                    .text_color(color)
                                    .when(change.status == FileStatus::Deleted, |d| d.line_through())
                                    .child(name),
                            )
                            .when(*depth == 0 && !dir.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(dir))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(1.))
                            .invisible()
                            .group_hover("file", |s| s.visible())
                            .child(ui::icon_button(("open", ix), "open").on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(Request::OpenFile { path: path_open.clone(), line: None, preview: false });
                            })))
                            .when(!staged, |d| {
                                d.child(ui::icon_button(("discard", ix), "discard").on_click(cx.listener(move |panel, _, window, cx| {
                                    cx.stop_propagation();
                                    panel.ask_discard(vec![c2.path.clone()], window, cx);
                                })))
                            })
                            .child(ui::icon_button(("stage", ix), if staged { "minus" } else { "plus" }).on_click(cx.listener(move |panel, _, _, cx| {
                                cx.stop_propagation();
                                if staged { panel.unstage(vec![c3.path.clone()], cx) } else { panel.stage(vec![c3.path.clone()], cx) }
                            }))),
                    )
                    .child(div().w(px(14.)).flex_none().text_color(color).text_size(px(11.)).child(change.status.letter()))
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
                            .child(self.commit_input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .h(px(26.))
                            .rounded(px(2.))
                            .bg(theme::accent())
                            .text_color(theme::c(0xffffff))
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
                                    .child(ui::icon("check", theme::c(0xffffff)))
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
                                    .on_click(cx.listener(|panel, _, _, cx| {
                                        panel.amend = !panel.amend;
                                        cx.emit(Request::Message(if panel.amend { "Amend mode on: the next commit amends HEAD".into() } else { "Amend mode off".into() }));
                                        cx.notify();
                                    }))
                                    .child(ui::small_icon("chevron-down", theme::c(0xffffff))),
                            ),
                    )
                    .when(branch.is_empty(), |d| d.child(div().text_color(theme::fg_muted()).child("Not a git repository"))),
            )
            .children(rows.iter().enumerate().map(|(ix, row)| self.render_list_row(row, ix, focused, cx)))
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
        let row2 = row.clone();
        let is_merge = commit.parents.len() > 1;
        let graph_w = row.width.max(1) as f32 * LANE_W + 8.;
        let bg = if selected { if focused { theme::selected_focused() } else { theme::selected() } } else { theme::bg_side() };

        let graph = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let x = |lane: usize| bounds.left() + px(6. + lane as f32 * LANE_W);
                let top = bounds.top();
                let mid = bounds.top() + px(GRAPH_ROW_H / 2.);
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
        .w(px(graph_w))
        .h(px(GRAPH_ROW_H))
        .flex_none();

        // Every branch pointer is shown: local (blue) and its remote origin/… (orange) separately.
        let badges = commit.refs.iter().enumerate().filter_map(|(ri, r)| {
            let menu_ref = r.clone();
            let (color, icon_name) = match r.kind {
                RefKind::Local => (theme::ref_local(), if r.is_head { "target" } else { "git" }),
                RefKind::Remote => (theme::ref_remote(), "cloud"),
                RefKind::Tag => (theme::ref_tag(), "tag"),
            };
            Some(
                div()
                    .id(SharedString::from(format!("ref-{ix}-{ri}")))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(3.))
                    .pl(px(5.))
                    .pr(px(7.))
                    .h(px(18.))
                    .rounded(px(9.))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |panel, ev: &MouseDownEvent, _, cx| {
                        // Right-click a branch/tag label: checkout, detached checkout or delete.
                        cx.stop_propagation();
                        panel.commit_menu = None;
                        panel.branch_menu = Some((menu_ref.clone(), ev.position));
                        cx.notify();
                    }))
                    .bg(color)
                    .text_color(theme::c(0xffffff))
                    .text_size(px(10.5))
                    .child(ui::small_icon(icon_name, theme::c(0xffffff)).size(px(12.)))
                    .child(r.name.clone()),
            )
        });

        let lane_color = colors[row.color % colors.len()];
        let sha = commit.sha.clone();
        div()
            .id(("commit", ix))
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
            .on_click(cx.listener(move |panel, _, window, cx| {
                window.focus(&panel.focus);
                panel.selected = Some(Item::Commit(ix));
                cx.emit(Request::OpenCommit(sha.clone()));
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |panel, ev: &MouseDownEvent, _, cx| {
                // Right-click any commit: checkout, branch from it, cherry-pick, revert…
                panel.selected = Some(Item::Commit(ix));
                panel.branch_menu = None;
                panel.commit_menu = Some((ix, ev.position));
                cx.notify();
            }))
            .child(graph)
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
                            .text_color(if is_head { theme::fg_bright() } else if contained { theme::fg() } else { theme::fg_muted() })
                            .when(is_head, |d| d.font_weight(FontWeight::BOLD))
                            .child(commit.subject.clone()),
                    )
                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).when(!contained, |d| d.opacity(0.6)).child(commit.author.clone())),
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

        let changes_header = ui::section_header("changes-header", "Changes", self.changes_open)
            .group("changes-header")
            .on_click(cx.listener(|p, _, _, cx| {
                p.changes_open = !p.changes_open;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .gap(px(1.))
                    .child(ui::icon_button("hdr-commit", "check").on_click(cx.listener(|p, _, w, cx| {
                        cx.stop_propagation();
                        let amend = p.amend;
                        p.commit(None, false, amend, w, cx);
                    })))
                    .child(ui::icon_button("hdr-refresh", "refresh").on_click(cx.listener(|p, _, _, cx| {
                        cx.stop_propagation();
                        p.repo.update(cx, |r, cx| r.refresh_all(cx));
                    }))),
            );

        let graph_header = ui::section_header("graph-header", "Graph", self.graph_open)
            .on_click(cx.listener(|p, _, _, cx| {
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
                        ui::icon_button("graph-scope", "filter")
                            .tooltip(ui::tooltip("Choose branches shown in the graph"))
                            .when(graph_all || !graph_only_empty, |b| b.bg(theme::selected()))
                            .on_click(cx.listener(|p, _, _, cx| {
                                cx.stop_propagation();
                                p.scope_open = !p.scope_open;
                                if p.scope_open {
                                    let root = p.repo.read(cx).root.clone();
                                    p.scope_branches = git::branches(&root);
                                    p.scope_remotes = git::remotes(&root);
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
            .key_context("GitPanel")
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
            .on_mouse_move(cx.listener(|p, ev: &MouseMoveEvent, _, cx| {
                if p.dragging_split {
                    if !ev.dragging() {
                        p.dragging_split = false;
                        return;
                    }
                    let b = p.panel_bounds.get();
                    let h = b.size.height / px(1.);
                    if h > 0. {
                        let head = p.head_h();
                        p.split = (((ev.position.y - b.top()) / px(1.) - head) / (h - head)).clamp(0.12, 0.88);
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
            .when_some(self.commit_menu, |d, (ix, pos)| d.child(deferred(anchored().position(pos).snap_to_window().child(self.render_commit_menu(ix, cx))).with_priority(2)))
            .when_some(self.branch_menu.clone(), |d, (r, pos)| d.child(deferred(anchored().position(pos).snap_to_window().child(self.render_branch_menu(r, cx))).with_priority(2)))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .px(px(20.))
                    .text_size(px(10.5))
                    .child(div().flex_1().child("SOURCE CONTROL"))
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .when(self.graph_open, |d| d.flex_1())
                    .when(!self.graph_open, |d| d.flex_none())
                    .child(graph_header)
                    .when(self.graph_open, |d| {
                        d.child(
                            uniform_list(
                                "graph",
                                graph.len(),
                                cx.processor(move |panel, range: Range<usize>, _window, cx| {
                                    range.map(|ix| panel.render_graph_row(ix, &graph[ix], focused, cx)).collect::<Vec<_>>()
                                }),
                            )
                            .track_scroll(self.graph_scroll.clone())
                            .flex_1(),
                        )
                    }),
            )
    }
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
    use super::{ListRow, tree_rows};
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
            email: String::new(),
            time: 0,
            refs: Vec::new(),
            subject: String::new(),
        };
        // newer (not checked out) → head → merge of side into base
        let commits = vec![c("new", &["head"]), c("other", &["base"]), c("head", &["base", "side"]), c("side", &["base"]), c("base", &[])];
        let (seen, head) = super::in_checkout(&commits, "head");
        assert_eq!(head, Some(2));
        assert_eq!(seen, vec![false, false, true, true, true]);
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
