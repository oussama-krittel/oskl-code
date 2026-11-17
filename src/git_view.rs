//! The Git tab, a git client in the editor area: a toolbar, the branches tree on the left,
//! the history as a table in the middle, and the picked commit's details and diff below it.
//! "Local changes" at the top of the history stages, discards and commits, like the sidebar.

use super::refs::{Act, FormKind, InlineKind, RefsTree, Reveal, cmd, git_act, item, sep};
use super::{graph_cell_h, in_checkout};
use crate::commit_view::CommitView;
use crate::diff_edit::DiffEditView;
use crate::diff_view::DiffView;
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::{self, Change, FileStatus, Rev};
use crate::request::DiffSpec;
use crate::repo::Repo;
use crate::request::Request;
use crate::ui::px;
use crate::{theme, ui, util};
use gpui::prelude::*;
use gpui::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

actions!(git_view, [Up, Down, Open]);

pub fn bind_keys() {
    let c = Some("GitView");
    crate::keybindings::collect([
        crate::keybindings::kb("up", Up, c),
        crate::keybindings::kb("down", Down, c),
        crate::keybindings::kb("enter", Open, c),
    ]);
}

/// While the tab is open, remotes are fetched this often (quietly), so ahead/behind stay true.
const AUTO_FETCH: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Narrower than this, the table drops its author and date columns.
const WIDE: f32 = 640.;
/// A history row's height; the graph's lines run the whole of it.
const ROW_H: f32 = 28.;
/// The author and date columns of the history.
const AUTHOR_W: f32 = 128.;
const DATE_W: f32 = 104.;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Left,
    Bottom,
}

pub struct GitView {
    focus: FocusHandle,
    pub repo: Entity<Repo>,
    pub refs: Entity<RefsTree>,
    search: Entity<Editor>,
    selected: Option<usize>,
    /// "Local changes" is picked instead of a commit.
    pub local: bool,
    pub details: Option<Entity<CommitView>>,
    pub diff: Option<Entity<DiffView>>,
    /// An unstaged change's diff: editable, its hunks and lines staged from there.
    pub edit: Option<Entity<DiffEditView>>,
    /// The change shown: (staged, path).
    change: Option<(bool, String)>,
    pub commit_msg: Entity<Editor>,
    pub drafter: git::draft::Drafter,
    amend: bool,
    /// A commit to select once the history (just widened to every branch) has it.
    pending: Option<String>,
    _auto_fetch: Task<()>,
    fetch_task: Option<Task<()>>,
    pub font_size: f32,
    left_w: f32,
    /// The details' share of the height under the toolbar.
    bottom: f32,
    drag: Option<Drag>,
    scroll: UniformListScrollHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _subs: Vec<Subscription>,
    _detail_subs: Vec<Subscription>,
}

impl EventEmitter<Request> for GitView {}

impl GitView {
    pub fn new(repo: Entity<Repo>, font_size: f32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let refs = cx.new(|cx| {
            let mut t = RefsTree::new(repo.clone(), cx);
            t.load_prs(cx);
            t
        });
        let search = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.font_size = 12.;
            ed.placeholder = "Search: message, author or hash".into();
            ed
        });
        let commit_msg = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::MultiLine, cx);
            ed.font_size = 12.;
            ed.placeholder = "Message (⌘Enter to commit)".into();
            ed.wrap = true;
            ed
        });
        let subs = vec![
            cx.observe(&repo, |v, _, cx| {
                if let Some(sha) = v.pending.clone()
                    && let Some(ix) = v.repo.read(cx).commits.iter().position(|c| c.sha == sha)
                {
                    v.pending = None;
                    v.select(ix, cx);
                }
                cx.notify();
            }),
            cx.subscribe(&refs, |v, _, Reveal(rev), cx| v.reveal(rev, cx)),
            cx.subscribe_in(&commit_msg, window, |v, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => v.commit(false, window, cx),
                EditorEvent::Changed => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&search, window, |v, input, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed | EditorEvent::Submit => {
                    let q = input.read(cx).text().trim().to_string();
                    v.selected = None;
                    v.repo.update(cx, |r, cx| {
                        r.graph_search = (!q.is_empty()).then_some(q);
                        r.refresh_log(cx);
                    });
                }
                EditorEvent::Cancel => {
                    input.update(cx, |e, cx| e.set_text("", cx));
                    window.focus(&v.focus, cx);
                }
                _ => {}
            }),
        ];
        Self {
            focus: cx.focus_handle(),
            repo,
            refs,
            search,
            selected: None,
            local: false,
            details: None,
            diff: None,
            edit: None,
            change: None,
            commit_msg,
            drafter: Default::default(),
            amend: false,
            pending: None,
            _auto_fetch: cx.spawn(async move |v, cx| loop {
                cx.background_executor().timer(AUTO_FETCH).await;
                if v.update(cx, |v, cx| v.fetch_quietly(cx)).is_err() {
                    break;
                }
            }),
            fetch_task: None,
            font_size,
            left_w: 240.,
            bottom: 0.42,
            drag: None,
            scroll: UniformListScrollHandle::new(),
            bounds: Default::default(),
            _subs: subs,
            _detail_subs: Vec::new(),
        }
    }

    pub fn title(&self, cx: &App) -> String {
        format!("Git: {}", self.repo.read(cx).name())
    }

    pub fn focus_target(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }

    /// Picks a commit: its details below, no file open yet.
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(sha) = self.repo.read(cx).commits.get(ix).map(|c| c.sha.clone()) else { return };
        self.selected = Some(ix);
        self.local = false;
        self.edit = None;
        self.change = None;
        let root = self.repo.read(cx).root.clone();
        let details = cx.new(|cx| {
            let mut v = CommitView::new(root, sha, cx);
            v.compact = true;
            v
        });
        // The commit's files open their diff beside it, not in another tab.
        self._detail_subs = vec![cx.subscribe(&details, |v, _, req: &Request, cx| match req {
            Request::OpenDiff { spec, .. } => {
                let (root, email) = (v.repo.read(cx).root.clone(), v.repo.read(cx).user_email.clone());
                let mut spec = spec.clone();
                spec.back = None;
                let font = v.font_size;
                v.diff = Some(cx.new(|cx| DiffView::new(root, spec, email, false, font, cx)));
                cx.notify();
            }
            Request::OpenCommit(sha) => cx.emit(Request::OpenCommit(sha.clone())),
            Request::Message(m) => cx.emit(Request::Message(m.clone())),
            Request::Error(e) => cx.emit(Request::Error(e.clone())),
            _ => {}
        })];
        self.details = Some(details);
        self.diff = None;
        self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        cx.notify();
    }

    /// Fetches every remote without a word (no output, no message), then refreshes.
    pub fn fetch_quietly(&mut self, cx: &mut Context<Self>) {
        let repo = self.repo.read(cx);
        if repo.busy.is_some() || repo.refs.remotes.is_empty() || self.fetch_task.is_some() {
            return;
        }
        let root = repo.root.clone();
        let fetch = cx.background_executor().spawn(async move { git::run(&root, &["fetch", "--all", "--prune", "--quiet"]) });
        self.fetch_task = Some(cx.spawn(async move |v, cx| {
            let _ = fetch.await;
            v.update(cx, |v, cx| {
                v.fetch_task = None;
                v.refs.update(cx, |t, cx| t.load_prs(cx));
                v.repo.update(cx, |r, cx| {
                    r.refresh_status(cx);
                    r.refresh_log(cx);
                });
            })
            .ok();
        }));
    }

    /// Selects the commit a branch, tag or stash points at.
    fn reveal(&mut self, rev: &str, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        let Ok(sha) = git::run(&root, &["rev-parse", "--verify", "-q", rev]).map(|s| s.trim().to_string()) else { return };
        let repo = self.repo.read(cx);
        if let Some(ix) = repo.commits.iter().position(|c| c.sha == sha) {
            return self.select(ix, cx);
        }
        if repo.graph_search.is_some() || repo.graph_file.is_some() {
            return cx.emit(Request::Message("Not in the commits listed: clear the search to see it".into()));
        }
        // Not in the branches shown: show them all, then select it.
        self.pending = Some(sha);
        self.repo.update(cx, |r, cx| {
            r.graph_all = true;
            r.graph_only.clear();
            r.refresh_log(cx);
        });
    }

    /// Right-click on a changed file.
    fn change_menu(&self, staged: bool, conflicted: bool, c: &Change, cx: &App) -> Vec<super::refs::MenuItem> {
        let root = self.repo.read(cx).root.clone();
        let (p, abs) = (c.path.as_str(), root.join(&c.path));
        let mut v = vec![item("Open the file", Act::OpenFile(abs.clone())), item("File history", Act::History(abs.clone()))];
        if conflicted {
            v.extend([
                sep(),
                item("Resolve in the merge editor", Act::MergeEditor(abs.clone())),
                item("Take ours (the whole file)", Act::Steps("Take ours".into(), vec![cmd(["checkout", "--ours", "--", p]), cmd(["add", "--", p])])),
                item("Take theirs (the whole file)", Act::Steps("Take theirs".into(), vec![cmd(["checkout", "--theirs", "--", p]), cmd(["add", "--", p])])),
                item("Mark as resolved", git_act(["add", "--", p])),
            ]);
        } else if staged {
            v.extend([sep(), item("Unstage", git_act(["restore", "--staged", "--", p]))]);
        } else {
            v.push(sep());
            v.push(item("Stage", git_act(["add", "--", p])));
            if c.status != FileStatus::Untracked {
                v.push(super::refs::MenuItem {
                    label: "Discard changes…".into(),
                    act: Act::Ask { title: format!("Discard the changes in {}?", ui::file_name(p)), detail: "They can't be brought back.".into(), buttons: vec![("Discard".into(), vec![cmd(["restore", "--worktree", "--", p])])] },
                    style: super::refs::Style::Danger,
                });
            }
        }
        v.extend([sep(), item("Copy path", Act::Copy(p.into()))]);
        v
    }

    fn open_commit_menu(&mut self, ix: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        let repo = self.repo.read(cx);
        let Some(commit) = repo.commits.get(ix).cloned() else { return };
        let head = repo.status.head_oid.clone().unwrap_or_default();
        let in_checkout = in_checkout(&repo.commits, &head).0.get(ix).copied().unwrap_or(false);
        let items = self.refs.read(cx).commit_menu(&commit, in_checkout, cx);
        self.refs.update(cx, |t, cx| t.open_menu(items, at, cx));
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.repo.read(cx).commits.len();
        if n == 0 {
            return;
        }
        let ix = match self.selected {
            Some(i) => (i as isize + delta).clamp(0, n as isize - 1) as usize,
            None => 0,
        };
        self.select(ix, cx);
    }

    fn open_selected(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(sha) = self.selected.and_then(|i| self.repo.read(cx).commits.get(i)).map(|c| c.sha.clone()) {
            cx.emit(Request::OpenCommit(sha));
        }
    }

    /// Picks "Local changes": what's staged and not, and the commit box.
    pub fn select_local(&mut self, cx: &mut Context<Self>) {
        self.local = true;
        self.selected = None;
        self.details = None;
        self.diff = None;
        self.edit = None;
        self.change = None;
        self._detail_subs.clear();
        cx.notify();
    }

    /// A change's diff beside the lists: editable when it isn't staged yet.
    pub fn open_change(&mut self, staged: bool, c: &Change, window: &mut Window, cx: &mut Context<Self>) {
        let (root, email) = (self.repo.read(cx).root.clone(), self.repo.read(cx).user_email.clone());
        self.change = Some((staged, c.path.clone()));
        self.diff = None;
        self.edit = None;
        if matches!(c.status, FileStatus::Untracked | FileStatus::Conflicted) {
            // Nothing to compare with yet (or markers to resolve): the file itself.
            cx.emit(Request::OpenFile { path: root.join(&c.path), line: None, preview: true });
            return cx.notify();
        }
        let left = if staged && c.status != FileStatus::Added { Rev::Commit("HEAD".into()) } else if staged { Rev::Empty } else { Rev::Index };
        let right = match (staged, c.status) {
            (_, FileStatus::Deleted) => Rev::Empty,
            (true, _) => Rev::Index,
            (false, _) => Rev::Working,
        };
        let spec = DiffSpec { path: c.path.clone(), old_path: c.orig_path.clone(), left, right: right.clone(), back: None };
        if right == Rev::Working {
            let font = self.font_size;
            let edit = cx.new(|cx| DiffEditView::new(root, spec, email, false, font, window, cx));
            self._detail_subs = vec![cx.subscribe_in(&edit, window, |_, _, req: &Request, _, cx| match req {
                Request::Message(m) => cx.emit(Request::Message(m.clone())),
                Request::Error(e) => cx.emit(Request::Error(e.clone())),
                Request::OpenFile { path, line, preview } => cx.emit(Request::OpenFile { path: path.clone(), line: *line, preview: *preview }),
                Request::FileHistory(p) => cx.emit(Request::FileHistory(p.clone())),
                _ => {}
            })];
            self.edit = Some(edit);
        } else {
            let font = self.font_size;
            self.diff = Some(cx.new(|cx| DiffView::new(root, spec, email, false, font, cx)));
        }
        cx.notify();
    }

    fn stage(&mut self, paths: Vec<String>, staged: bool, cx: &mut Context<Self>) {
        let initial = self.repo.read(cx).status.is_initial();
        self.repo.update(cx, |r, cx| {
            r.run_op(if staged { "Unstage" } else { "Stage" }, cx, move |root| {
                if staged { git::unstage(root, &paths, initial) } else { git::stage(root, &paths) }.map(|_| String::new())
            })
        });
    }

    /// Asks, then puts tracked files back as committed or staged; new files go to the Trash.
    fn discard(&mut self, c: &Change, cx: &mut Context<Self>) {
        let (path, untracked) = (c.path.clone(), c.status == FileStatus::Untracked);
        let repo = self.repo.downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: if untracked { format!("Move {} to the Trash?", ui::file_name(&path)) } else { format!("Discard the changes in {}?", ui::file_name(&path)) },
            detail: if untracked { "You can get it back from the Trash.".into() } else { "They can't be brought back.".into() },
            buttons: vec![if untracked { "Move to Trash".into() } else { "Discard".into() }],
            danger: true,
            cancel_default: false,
            cancel_label: None,
            on_answer: Rc::new(move |_, _, cx| {
                let p = path.clone();
                repo.update(cx, |r, cx| {
                    r.run_op("Discard", cx, move |root| {
                        if untracked {
                            trash::delete(root.join(&p)).map_err(|e| anyhow::anyhow!("couldn't move {p} to the Trash: {e}"))?;
                            Ok(String::new())
                        } else {
                            git::discard(root, &[p]).map(|_| String::new())
                        }
                    })
                })
                .ok();
            }),
        }));
    }

    /// Commits what's staged (asks first when it looks like it holds secrets).
    pub fn commit(&mut self, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let msg = self.commit_msg.read(cx).text().trim().to_string();
        let (amend, root) = (self.amend, self.repo.read(cx).root.clone());
        if msg.is_empty() && !amend {
            return cx.emit(Request::Error("Write a commit message first".into()));
        }
        if self.repo.read(cx).status.staged.is_empty() && !amend {
            return cx.emit(Request::Error("Nothing staged: stage changes first".into()));
        }
        let findings = if confirmed { Vec::new() } else { crate::secrets::scan_staged(&root) };
        if !findings.is_empty() {
            let me = cx.entity().downgrade();
            let lines: Vec<String> = findings.iter().take(6).map(|f| format!("• {}", f.describe())).collect();
            return cx.emit(Request::Ask(crate::dialog::Ask {
                title: "This commit may include secrets".into(),
                detail: lines.join("\n"),
                buttons: vec!["Commit Anyway".into()],
                danger: true,
                cancel_default: true,
                cancel_label: None,
                on_answer: Rc::new(move |_, window, cx| {
                    me.update(cx, |v, cx| v.commit(true, window, cx)).ok();
                }),
            }));
        }
        self.repo.update(cx, |r, cx| {
            r.run_op("Commit", cx, move |root| {
                if amend && msg.is_empty() {
                    git::run(root, &["commit", "--amend", "--no-edit"])
                } else {
                    git::commit(root, &msg, amend)
                }
            })
        });
        self.commit_msg.update(cx, |e, cx| e.set_text("", cx));
        self.amend = false;
        self.edit = None;
        self.diff = None;
        self.change = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn render_local_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let st = self.repo.read(cx).status.clone();
        let (u, s, c) = (st.unstaged.len(), st.staged.len(), st.conflicts.len());
        let mut parts = Vec::new();
        if c > 0 {
            parts.push(format!("{c} conflict{}", if c == 1 { "" } else { "s" }));
        }
        if s > 0 {
            parts.push(format!("{s} staged"));
        }
        if u > 0 {
            parts.push(format!("{u} not staged"));
        }
        div()
            .id("git-local")
            .debug_selector(|| "git-local".into())
            .flex()
            .flex_none()
            .w_full()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_H))
            .px(px(8.))
            .cursor_pointer()
            .bg(if self.local { theme::selected_focused() } else { theme::bg_editor() })
            .when(!self.local, |d| d.hover(|s| s.bg(theme::hover())))
            .border_b_1()
            .border_color(theme::border())
            .overflow_hidden()
            .whitespace_nowrap()
            .on_click(cx.listener(|v, _, _, cx| v.select_local(cx)))
            .child(div().flex_none().size(px(9.)).rounded(px(5.)).border_2().border_color(theme::git_modified()))
            .child(div().flex_none().text_color(theme::fg_bright()).font_weight(FontWeight::BOLD).child("Local changes"))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(theme::fg_muted()).child(parts.join(", ")))
    }

    /// Merge / rebase / cherry-pick stopped: what's going on, how many conflicts are left, and
    /// the ways out. Continue waits until none are.
    fn render_banner(&self, op: &git::OpState, conflicts: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_none()
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .when(primary, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).font_weight(FontWeight::SEMIBOLD).hover(|s| s.bg(theme::accent_hover())))
                .when(!primary, |d| d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())))
                .child(label)
        };
        let (kind, msg) = (op.kind, op.message.clone());
        let color = theme::git_conflict();
        div()
            .id("git-op-banner")
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .bg(color.opacity(0.12))
            .border_b_1()
            .border_color(color.opacity(0.3))
            .child(ui::small_icon("warning", color))
            .child(div().id("git-op-detail").debug_selector(|| "git-op-detail".into()).flex_1().min_w(px(120.)).text_color(theme::fg_muted()).child(div().flex().flex_wrap().gap(px(6.)).child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(op.label.clone())).child(if conflicts > 0 {
                format!("{conflicts} conflicted file{}: open {} from Local changes", if conflicts == 1 { "" } else { "s" }, if conflicts == 1 { "it" } else { "them" })
            } else {
                "No conflicts left".to_string()
            })))
            .child(
                button("git-op-continue", op.kind.continue_label(), true)
                    .when(conflicts > 0, |d| d.opacity(0.45).cursor_default().tooltip(ui::tooltip("Resolve the conflicts first")))
                    .when(conflicts == 0, |d| {
                        d.on_click(cx.listener(move |v, _, _, cx| {
                            let msg = msg.clone();
                            v.repo.update(cx, |r, cx| r.run_op("Continue", cx, move |root| git::continue_op(root, kind, &msg)));
                        }))
                    }),
            )
            .when(kind.can_skip(), |d| {
                d.child(button("git-op-skip", "Skip this commit", false).on_click(cx.listener(move |v, _, _, cx| v.repo.update(cx, |r, cx| r.run_op("Skip", cx, move |root| git::skip_op(root, kind))))))
            })
            .child(button("git-op-abort", "Abort", false).on_click(cx.listener(move |v, _, _, cx| v.repo.update(cx, |r, cx| r.run_op("Abort", cx, move |root| git::abort_op(root, kind))))))
    }

    fn render_changes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let st = self.repo.read(cx).status.clone();
        let current = self.change.clone();
        let staged_n = st.staged.len();
        let clean = st.conflicts.is_empty() && st.unstaged.is_empty() && st.staged.is_empty();
        let section = |id: &'static str, title: &str, list: &[Change], staged: bool, cx: &mut Context<Self>| {
            let paths: Vec<String> = list.iter().map(|c| c.path.clone()).collect();
            let (all_label, all_icon) = if staged { ("Unstage all", "minus") } else { ("Stage all", "plus") };
            div()
                .flex()
                .flex_col()
                .flex_none()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(26.))
                        .px(px(10.))
                        .gap(px(6.))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::fg_dim())
                        .child(div().flex_1().flex().items_center().gap(px(6.)).child(title.to_string()).child(div().font_family(theme::MONO_FONT).text_size(px(11.)).font_weight(FontWeight::NORMAL).text_color(theme::fg_muted()).child(list.len().to_string())))
                        .when(!list.is_empty() && id != "conflicts", |d| {
                            d.child(
                                ui::icon_button(SharedString::from(format!("{id}-all")), all_icon)
                                    .debug_selector(move || format!("{id}-all"))
                                    .size(px(20.))
                                    .tooltip(ui::tooltip(all_label))
                                    .on_click(cx.listener(move |v, _, _, cx| v.stage(paths.clone(), staged, cx))),
                            )
                        }),
                )
                .children(list.iter().map(|c| {
                    let on = current.as_ref().is_some_and(|(s, p)| *s == staged && *p == c.path);
                    let (c1, c2, c3, c4) = (c.clone(), c.clone(), c.clone(), c.clone());
                    let conflicted = id == "conflicts";
                    let sel = format!("{id}-{}", c.path);
                    div()
                        .id(SharedString::from(sel.clone()))
                        .debug_selector(move || sel)
                        .group("change-row")
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(24.))
                        .px(px(10.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(theme::selected()))
                        .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .on_click(cx.listener(move |v, _, w, cx| {
                            if conflicted {
                                let path = v.repo.read(cx).root.join(&c1.path);
                                cx.emit(Request::OpenMergeEditor(path));
                            } else {
                                v.open_change(staged, &c1, w, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                            let items = v.change_menu(staged, conflicted, &c4, cx);
                            v.refs.update(cx, |t, cx| t.open_menu(items, ev.position, cx));
                        }))
                        .child(ui::change_cells(c.status, &c.path, None))
                        .child(
                            ui::hover_actions("change-row", on, theme::bg_editor())
                                .when(!staged && id != "conflicts", |d| {
                                    d.child(ui::icon_button(SharedString::from(format!("discard-{}", c.path)), "discard").size(px(20.)).tooltip(ui::tooltip("Discard")).on_click(cx.listener(move |v, _, _, cx| {
                                        cx.stop_propagation();
                                        v.discard(&c2, cx);
                                    })))
                                })
                                .child(ui::icon_button(SharedString::from(format!("toggle-{id}-{}", c.path)), if staged { "minus" } else { "plus" }).size(px(20.)).tooltip(ui::tooltip(if staged { "Unstage" } else { "Stage" })).on_click(cx.listener(move |v, _, _, cx| {
                                    cx.stop_propagation();
                                    v.stage(vec![c3.path.clone()], staged, cx);
                                }))),
                        )
                }))
        };
        div()
            .id("git-changes")
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("git-change-lists")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(clean, |d| d.child(ui::empty_state("check", "Nothing to commit", "The working tree is clean.").id("git-tab-clean").debug_selector(|| "git-tab-clean".into())))
                    .when(!clean, |d| {
                        d.when(!st.conflicts.is_empty(), |d| d.child(section("conflicts", "Conflicts", &st.conflicts, false, cx)))
                            .child(section("unstaged", "Not staged", &st.unstaged, false, cx))
                            .child(section("staged", "Staged", &st.staged, true, cx))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(6.))
                    .p(px(8.))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(div().id("git-commit-msg").debug_selector(|| "git-commit-msg".into()).h(px(64.)).px(px(6.)).py(px(4.)).rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(self.commit_msg.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("git-amend")
                                    .debug_selector(|| "git-amend".into())
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.amend = !v.amend;
                                        if v.amend && v.commit_msg.read(cx).text().trim().is_empty() {
                                            // Amending starts from the last commit's message.
                                            let root = v.repo.read(cx).root.clone();
                                            let last = git::run(&root, &["log", "-1", "--format=%B"]).unwrap_or_default();
                                            v.commit_msg.update(cx, |e, cx| {
                                                e.set_text(last.trim(), cx);
                                                e.go_to(0, 0, cx);
                                            });
                                        }
                                        cx.notify();
                                    }))
                                    .child(div().size(px(14.)).flex().items_center().justify_center().rounded(px(3.)).border_1().border_color(theme::border_input()).when(self.amend, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).child("✓")))
                                    .child("Amend the last commit"),
                            )
                            .child(div().flex_1())
                            .child(git::draft::button("git-draft", self.drafter.running()).on_click(cx.listener(|v, _, _, cx| {
                                let root = v.repo.read(cx).root.clone();
                                v.drafter.toggle(root, &v.commit_msg, cx);
                            })))
                            .child({
                                // Git's advice: a subject of 50, at most 72, characters.
                                let n = self.commit_msg.read(cx).text().lines().next().unwrap_or("").chars().count();
                                div()
                                    .id("git-subject-len")
                                    .debug_selector(|| "git-subject-len".into())
                                    .flex_none()
                                    .text_size(px(10.5))
                                    .text_color(if n > 72 { theme::git_modified() } else { theme::fg_muted() })
                                    .tooltip(ui::tooltip("The subject's length: best under 50, at most 72"))
                                    .when(n > 0, |d| d.child(format!("{n}/72")))
                            })
                            .child(
                                div()
                                    .id("git-commit")
                                    .debug_selector(|| "git-commit".into())
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .px(px(14.))
                                    .h(px(26.))
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .bg(theme::accent())
                                    .text_color(theme::on(theme::accent()))
                                    .hover(|s| s.bg(theme::accent_hover()))
                                    .on_click(cx.listener(|v, _, w, cx| v.commit(false, w, cx)))
                                    .child(ui::small_icon("check", theme::on(theme::accent())))
                                    .child(if self.amend { "Amend".to_string() } else if staged_n > 0 { format!("Commit {staged_n}") } else { "Commit".to_string() }),
                            ),
                    ),
            )
    }

    fn run(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.refs.update(cx, |t, cx| t.run_act(act, window, cx));
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let st = repo.status.clone();
        let has_stash = !repo.refs.stashes.is_empty();
        let all = repo.graph_all;
        let busy = repo.busy.clone();
        let tool = |id: &'static str, icon: &'static str, label: &'static str, count: Option<String>, tip: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .h(px(26.))
                .px(px(8.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(theme::fg_dim())
                .hover(|s| s.bg(theme::hover()).text_color(theme::fg_bright()))
                .tooltip(ui::tooltip(tip))
                .child(ui::small_icon(icon, theme::fg_dim()))
                .child(label)
                .when_some(count, |d, n| d.child(div().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::accent()).child(n)))
        };
        // Fetch, pull and push; stash and branch; then how the history is shown.
        let divider = || div().flex_none().w(px(1.)).h(px(16.)).mx(px(4.)).bg(theme::border());
        let head = ui::small_icon("check", theme::on(theme::accent())).size(px(11.));
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .py(px(5.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .mr(px(6.))
                    .max_w(px(300.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().pl(px(4.)).text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(repo.name()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .min_w_0()
                            .h(px(18.))
                            .pl(px(5.))
                            .pr(px(7.))
                            .rounded(px(9.))
                            .bg(theme::accent())
                            .text_color(theme::on(theme::accent()))
                            .font_family(theme::MONO_FONT)
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(head)
                            .child(div().min_w_0().overflow_hidden().text_ellipsis().child(repo.branch_label())),
                    ),
            )
            .when_some(self.refs.read(cx).prs().into_iter().find(|p| Some(&p.head_ref_name) == st.branch.as_ref()), |d, p| {
                let n = p.number;
                let mark = match crate::git::gh::checks(&p.status_check_rollup) {
                    crate::git::gh::Checks::Passing => " ✓",
                    crate::git::gh::Checks::Failing => " ✗",
                    crate::git::gh::Checks::Pending => " •",
                    crate::git::gh::Checks::None => "",
                };
                d.child(divider()).child(
                    div()
                        .id("git-pr-chip")
                        .debug_selector(|| "git-pr-chip".into())
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(5.))
                        .h(px(26.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("This branch's pull request: review it"))
                        .child(ui::small_icon("git", theme::ref_local()))
                        .child(format!("PR #{n}{mark}"))
                        .on_click(cx.listener(move |v, _, _, cx| cx.emit(Request::OpenPr(v.repo.read(cx).root.clone(), n)))),
                )
            })
            .child(divider())
            .child(tool("git-fetch", "fetch", "Fetch", None, "Fetch every remote and prune deleted branches").on_click(cx.listener(|v, _, w, cx| v.run(git_act(["fetch", "--all", "--prune"]), w, cx))))
            .child(tool("git-pull", "arrow-down", "Pull", (st.behind > 0).then(|| format!("↓{}", st.behind)), "Pull the current branch").on_click(cx.listener(|v, _, w, cx| v.run(git_act(["pull"]), w, cx))))
            .child(tool("git-push", "arrow-up", "Push", (st.ahead > 0).then(|| format!("↑{}", st.ahead)), "Push the current branch").on_click(cx.listener(|v, _, w, cx| v.run(git_act(["push"]), w, cx))))
            .child(divider())
            .child(tool("git-stash", "paperclip", "Stash", None, "Put all changes aside, with a message").on_click(cx.listener(|v, _, w, cx| v.run(Act::Inline(InlineKind::StashSave), w, cx))))
            .when(has_stash, |d| d.child(tool("git-pop", "discard", "Pop", None, "Bring back the last stash").on_click(cx.listener(|v, _, w, cx| v.run(git_act(["stash", "pop"]), w, cx)))))
            .child(tool("git-branch", "plus", "Branch", None, "A new branch here, checked out").on_click(cx.listener(|v, _, w, cx| v.run(Act::Inline(InlineKind::NewBranch), w, cx))))
            .child(divider())
            .child(
                tool("git-all", "git", "All branches", None, "Show every branch in the history (off: this branch, its upstream and the main one)")
                    .when(all, |d| d.bg(theme::accent().opacity(0.13)).text_color(theme::accent()))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.repo.update(cx, |r, cx| {
                            r.graph_all = !r.graph_all;
                            r.graph_only.clear();
                            r.refresh_log(cx);
                        })
                    })),
            )
            .child(tool("git-patch", "paperclip", "Apply patch", None, "Apply a .patch or .diff file").on_click(cx.listener(|v, ev: &ClickEvent, w, cx| {
                v.refs.update(cx, |t, cx| t.open_menu(Vec::new(), ev.position(), cx));
                v.run(Act::Form(FormKind::PatchApply), w, cx);
            })))
            // The search box and refresh are one item: they wrap together, or not at all.
            .child(
                div().flex().flex_1().min_w(px(150.)).max_w(px(350.)).ml_auto().items_center().gap(px(2.)).child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .h(px(26.))
                    .rounded(px(6.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(theme::border_input())
                    .child(ui::small_icon("search", theme::fg_muted()))
                    .child(div().id("git-search").debug_selector(|| "git-search".into()).flex_1().min_w_0().overflow_hidden().child(self.search.clone())),
                )
                .child(ui::icon_button("git-refresh", "refresh").debug_selector(|| "git-refresh".into()).tooltip(ui::tooltip("Refresh")).on_click(cx.listener(|v, _, _, cx| v.repo.update(cx, |r, cx| r.refresh_all(cx))))),
            )
            .when_some(busy, |d, b| d.child(div().flex_none().ml(px(6.)).text_color(theme::fg_muted()).max_w(px(200.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(b)))
    }

    fn render_row(&self, ix: usize, wide: bool, head: &str, reach: &(Vec<bool>, Option<usize>), cx: &mut Context<Self>) -> AnyElement {
        let repo = self.repo.read(cx);
        let (Some(c), Some(row)) = (repo.commits.get(ix).cloned(), repo.graph.get(ix).cloned()) else { return div().into_any_element() };
        let is_head = c.sha == head;
        let contained = reach.0.get(ix).copied().unwrap_or(true) || reach.1.is_none();
        let above_head = reach.1.is_some_and(|h| ix < h);
        let selected = self.selected == Some(ix);
        let bg = if selected { theme::selected_focused() } else { theme::bg_editor() };
        let badges = ui::ref_labels(&c.refs).into_iter().map(|(r, cloud, ri)| {
            let r2 = r.clone();
            ui::ref_chip(r, cloud, 16.)
                .id(SharedString::from(format!("git-ref-{ix}-{ri}")))
                .on_mouse_down(MouseButton::Right, cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let items = v.refs.read(cx).ref_menu(&r2, cx);
                    v.refs.update(cx, |t, cx| t.open_menu(items, ev.position, cx));
                }))
        });
        let muted = if contained { theme::fg() } else { theme::fg_muted() };
        let short = c.short().to_string();
        let sel = format!("git-row-{short}");
        div()
            .id(("git-row", ix))
            .debug_selector(move || sel)
            .w_full()
            .flex()
            .items_center()
            .h(px(ROW_H))
            .pr(px(10.))
            .gap(px(8.))
            .bg(bg)
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .cursor_pointer()
            .overflow_hidden()
            .whitespace_nowrap()
            .on_click(cx.listener(move |v, ev: &ClickEvent, w, cx| {
                window_focus(v, w, cx);
                if ev.click_count() >= 2 {
                    if let Some(sha) = v.repo.read(cx).commits.get(ix).map(|c| c.sha.clone()) {
                        cx.emit(Request::OpenCommit(sha));
                    }
                } else {
                    v.select(ix, cx);
                }
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                v.select(ix, cx);
                v.open_commit_menu(ix, ev.position, cx);
            }))
            .child(div().flex_none().pl(px(4.)).child(graph_cell_h(&row, is_head, c.parents.len() > 1, contained, above_head, bg, ROW_H)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(4.))
                    .overflow_hidden()
                    .children(badges)
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(if is_head { theme::fg_bright() } else { muted }).when(is_head, |d| d.font_weight(FontWeight::BOLD)).child(c.subject.clone())),
            )
            .when(wide, |d| {
                d.child(div().id(("git-author", ix)).debug_selector(move || format!("git-author-{ix}")).flex_none().w(px(AUTHOR_W)).overflow_hidden().text_ellipsis().text_color(theme::fg_dim()).child(c.author.clone()))
                    .child(div().flex_none().w(px(DATE_W)).overflow_hidden().text_ellipsis().text_color(theme::fg_dim()).child(util::relative_time(c.time)))
            })
            .child(div().flex_none().w(px(60.)).text_color(theme::fg_muted()).font_family(theme::MONO_FONT).child(short))
            .into_any_element()
    }

    fn render_table(&self, wide: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let count = repo.commits.len();
        let head = repo.status.head_oid.clone().unwrap_or_default();
        let reach = Rc::new(in_checkout(&repo.commits, &head));
        let has_local = repo.status.change_count() > 0 && repo.graph_file.is_none() && repo.graph_search.is_none();
        let header = |label: &'static str, w: Option<f32>| {
            div().flex_none().when_some(w, |d, w| d.w(px(w))).when(w.is_none(), |d| d.flex_1().pl(px(12.))).text_color(theme::fg_muted()).text_size(px(12.)).child(label)
        };
        let title = match (&repo.graph_file, &repo.graph_search) {
            (Some((f, _)), _) => format!("History of {f}"),
            (None, Some(q)) => format!("Commits matching \"{q}\""),
            _ => "Commits".into(),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(26.))
                    .pr(px(10.))
                    .border_b_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().flex_1().min_w_0().pl(px(14.)).overflow_hidden().text_ellipsis().text_color(theme::fg_muted()).text_size(px(12.)).child(title))
                    .when(wide, |d| d.child(header("Author", Some(AUTHOR_W))).child(header("Date", Some(DATE_W))))
                    .child(header("Hash", Some(60.))),
            )
            .when(has_local, |d| d.child(self.render_local_row(cx)))
            .child(if count == 0 && !has_local { self.render_no_commits(cx).into_any_element() } else {
                uniform_list(
                    "git-table",
                    count,
                    cx.processor(move |v, range: Range<usize>, _, cx| {
                        let (repo, end) = (v.repo.downgrade(), range.end);
                        cx.defer(move |cx| {
                            repo.update(cx, |r, cx| r.want_more(end, cx)).ok();
                        });
                        range.map(|ix| v.render_row(ix, wide, &head, &reach, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element()
            })
    }

    /// The history when no commit is listed: still reading, nothing matches, or none made yet.
    fn render_no_commits(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let state = if repo.log_loading() {
            ui::empty_state("refresh", "Loading the history", "Reading the commits.")
        } else if let Some(q) = &repo.graph_search {
            ui::empty_state("search", format!("No commit matches \"{q}\""), "The search looks at messages, authors and the start of hashes, on every branch.")
        } else if let Some((f, _)) = &repo.graph_file {
            ui::empty_state("commit", format!("No commit changed {f}"), "It may be new, or untracked.")
        } else {
            ui::empty_state("commit", "No commits yet", "Stage some changes and commit them to start the history.")
        };
        let searching = repo.graph_search.is_some();
        div().id("git-no-commits").debug_selector(|| "git-no-commits".into()).flex().flex_col().flex_1().items_center().justify_center().child(state.flex_none()).when(searching, |d| {
            d.child(
                div()
                    .id("git-clear-search")
                    .debug_selector(|| "git-clear-search".into())
                    .px(px(12.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_input())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(|v, _, _, cx| v.search.update(cx, |e, cx| e.set_text("", cx))))
                    .child("Show everything"),
            )
        })
    }

    fn render_details(&self, wide: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = || ui::empty_state("commit", "Pick a commit", "Click one above to see what it changed. Up and down move through the history, enter opens it.").id("git-nothing-picked").debug_selector(|| "git-nothing-picked".into());
        let side = |el: AnyElement| div().flex_1().min_w_0().min_h_0().overflow_hidden().child(el);
        let change_diff: Option<AnyElement> = self.edit.clone().map(|e| e.into_any_element()).or_else(|| self.diff.clone().filter(|_| self.local).map(|d| d.into_any_element()));
        if self.local {
            return div()
                .id("git-details")
                .flex()
                .when(!wide, |d| d.flex_col())
                .flex_1()
                .min_h_0()
                .child(div().flex_none().when(wide, |d| d.w(relative(0.36))).when(!wide, |d| d.h(relative(0.5))).min_w_0().min_h_0().overflow_hidden().child(self.render_changes(cx)))
                .child(div().flex_none().when(wide, |d| d.w(px(1.))).when(!wide, |d| d.h(px(1.))).bg(theme::border()))
                .child(match change_diff {
                    Some(d) => side(d),
                    None => side(ui::empty_state("file", "Pick a file", "Click one on the left to see its changes.").into_any_element()),
                })
                .into_any_element();
        }
        div()
            .id("git-details")
            .flex()
            .when(!wide, |d| d.flex_col())
            .flex_1()
            .min_h_0()
            .child(match &self.details {
                // With a diff open, the diff gets the room (the details are a list of files).
                Some(d) if self.diff.is_some() && wide => side(d.clone().into_any_element()).flex_none().w(relative(0.38)),
                Some(d) => side(d.clone().into_any_element()),
                None => side(empty().into_any_element()),
            })
            .when_some(self.diff.clone(), |d, diff| d.child(div().flex_none().when(wide, |d| d.w(px(1.))).when(!wide, |d| d.h(px(1.))).bg(theme::border())).child(side(diff.into_any_element())))
            .into_any_element()
    }
}

impl Focusable for GitView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GitView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let st = self.repo.read(cx).status.clone();
        let op = st.op.clone().map(|o| (o, st.conflicts.len()));
        let b = self.bounds.get();
        let width = if b.size.width > gpui::px(0.) { b.size.width / gpui::px(1.) } else { 1200. };
        let wide = width >= WIDE * ui::scale() + self.left_w;
        // A narrow tab hides the tree; it needs room for the history first.
        let show_tree = width >= 520. * ui::scale();
        let cell = self.bounds.clone();
        let handle = |id: &'static str, vertical: bool, which: Drag, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex_none()
                .when(vertical, |d| d.w(px(4.)).h_full().cursor(CursorStyle::ResizeLeftRight))
                .when(!vertical, |d| d.h(px(4.)).w_full().cursor(CursorStyle::ResizeUpDown))
                .bg(theme::border())
                .hover(|s| s.bg(theme::focus_border()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |v, _, _, cx| {
                    v.drag = Some(which);
                    cx.stop_propagation();
                }))
        };
        div()
            .id("git-view")
            .debug_selector(|| "git-view".into())
            .track_focus(&self.focus)
            .key_context("GitView")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .text_size(px(12.))
            .child(canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                let Some(drag) = v.drag else { return };
                if !ev.dragging() {
                    v.drag = None;
                    return;
                }
                let b = v.bounds.get();
                match drag {
                    Drag::Left => v.left_w = ((ev.position.x - b.left()) / gpui::px(1.) / ui::scale()).clamp(140., 520.),
                    Drag::Bottom => v.bottom = (1. - (ev.position.y - b.top()) / b.size.height).clamp(0.15, 0.85),
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, _| v.drag = None))
            .on_action(cx.listener(|v, _: &Up, _, cx| v.step(-1, cx)))
            .on_action(cx.listener(|v, _: &Down, _, cx| v.step(1, cx)))
            .on_action(cx.listener(Self::open_selected))
            .child(self.render_toolbar(cx))
            .when_some(op, |d, (op, n)| d.child(self.render_banner(&op, n, cx)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .when(show_tree, |d| {
                        d.child(div().flex_none().w(px(self.left_w)).h_full().pt(px(4.)).bg(theme::bg_side()).child(self.refs.clone())).child(handle("git-left-split", true, Drag::Left, cx))
                    })
                    .when(!show_tree, |d| d.child(div().size_0().child(self.refs.clone())))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(div().flex().flex_col().flex_1().min_h_0().min_w_0().overflow_hidden().child(self.render_table(wide, cx)))
                            .child(handle("git-bottom-split", false, Drag::Bottom, cx))
                            .child(div().flex().flex_none().h(relative(self.bottom)).min_h_0().min_w_0().overflow_hidden().child(self.render_details(wide, cx))),
                    ),
            )
    }
}

/// Clicking the table gives it the keyboard (↑ / ↓ then move from there).
fn window_focus(v: &GitView, window: &mut Window, cx: &mut Context<GitView>) {
    window.focus(&v.focus, cx);
}
