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

pub fn bind_keys(cx: &mut App) {
    let c = Some("GitPanel");
    cx.bind_keys([
        KeyBinding::new("j", Down, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("k", Up, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("enter", Open, c),
        KeyBinding::new("space", ToggleStage, c),
        KeyBinding::new("s", ToggleStage, c),
        KeyBinding::new("d", Discard, c),
        KeyBinding::new("o", OpenFile, c),
        KeyBinding::new("c", FocusCommit, c),
        KeyBinding::new("r", Refresh, c),
        KeyBinding::new("g", Top, c),
        KeyBinding::new("shift-g", Bottom, c),
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
    Folder { staged: bool, dir: String, paths: Vec<String> },
    File { staged: bool, change: Change, nested: bool },
}

const LANE_W: f32 = 11.;
const GRAPH_ROW_H: f32 = 20.;

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
    scope_open: bool,
    scope_branches: Vec<String>,
    scope_remotes: Vec<String>,
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
        let subs = vec![
            cx.observe(&repo, |panel, repo, cx| {
                let branch = repo.read(cx).branch_label();
                panel.commit_input.update(cx, |ed, cx| {
                    ed.placeholder = format!("Message (⌘Enter to commit on \"{branch}\")").into();
                    cx.notify();
                });
                cx.notify();
            }),
            cx.subscribe_in(&commit_input, window, |panel, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => panel.commit(None, false, panel.amend, window, cx),
                EditorEvent::Cancel => panel.focus(window),
                _ => {}
            }),
        ];
        Self {
            focus: cx.focus_handle(),
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
            scope_open: false,
            scope_branches: Vec::new(),
            scope_remotes: Vec::new(),
            _subs: subs,
        }
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn focus_commit_input(&self, window: &mut Window, cx: &App) {
        self.commit_input.read(cx).focus(window);
    }

    // ------------------------------------------------------------ git actions

    pub fn commit(&mut self, message: Option<String>, all: bool, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let message = message.unwrap_or_else(|| self.commit_input.read(cx).text()).trim().to_string();
        let repo = self.repo.read(cx);
        if message.is_empty() && !amend {
            self.focus_commit_input(window, cx);
            return cx.emit(Request::Error("Write a commit message first".into()));
        }
        if repo.status.staged.is_empty() && !all && !amend {
            if repo.status.unstaged.is_empty() {
                return cx.emit(Request::Error("Nothing to commit".into()));
            }
            // Smart commit: let the user confirm "stage everything" on the command line.
            return cx.emit(Request::CommandLine(format!("commit -a {message}")));
        }
        let root = repo.root.clone();
        let task = cx.background_executor().spawn(async move {
            if all {
                git::run(&root, &["add", "-A"])?;
            }
            if amend && message.is_empty() {
                return git::run(&root, &["commit", "--amend", "--no-edit"]);
            }
            git::commit(&root, &message, amend)
        });
        self.commit_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |panel, _, cx| {
                match result {
                    Ok(out) => {
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

    pub fn stage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.repo.update(cx, |repo, cx| repo.run_op("Stage", cx, move |root| git::stage(root, &paths).map(|_| String::new())));
    }

    pub fn unstage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let initial = self.repo.read(cx).status.is_initial();
        self.repo.update(cx, |repo, cx| repo.run_op("Unstage", cx, move |root| git::unstage(root, &paths, initial).map(|_| String::new())));
    }

    fn ask_discard(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        // Confirmation happens on the command line: press Enter to run it.
        cx.emit(Request::CommandLine(format!("discard {}", paths.join(" "))));
    }

    fn open_change(&self, staged: bool, change: &Change, preview: bool, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        if change.status == FileStatus::Untracked {
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
            spec: DiffSpec { path: change.path.clone(), old_path: change.orig_path.clone(), left, right },
            preview,
        });
    }

    // ------------------------------------------------------------ list model

    fn list_rows(&self, cx: &App) -> Vec<ListRow> {
        let repo = self.repo.read(cx);
        let st = &repo.status;
        let mut rows = Vec::new();
        let mut group = |rows: &mut Vec<ListRow>, staged: bool, changes: &[Change]| {
            let mut i = 0;
            while i < changes.len() {
                let dir = ui::file_dir(&changes[i].path).to_string();
                let mut j = i;
                while j < changes.len() && ui::file_dir(&changes[j].path) == dir {
                    j += 1;
                }
                if !dir.is_empty() {
                    rows.push(ListRow::Folder { staged, dir: dir.clone(), paths: changes[i..j].iter().map(|c| c.path.clone()).collect() });
                }
                for c in &changes[i..j] {
                    rows.push(ListRow::File { staged, change: c.clone(), nested: !dir.is_empty() });
                }
                i = j;
            }
        };
        if !st.conflicts.is_empty() {
            rows.push(ListRow::Section { staged: false, conflicts: true, count: st.conflicts.len() });
            group(&mut rows, false, &st.conflicts);
        }
        if !st.staged.is_empty() {
            rows.push(ListRow::Section { staged: true, conflicts: false, count: st.staged.len() });
            if self.staged_open {
                group(&mut rows, true, &st.staged);
            }
        }
        rows.push(ListRow::Section { staged: false, conflicts: false, count: st.unstaged.len() });
        if self.unstaged_open {
            group(&mut rows, false, &st.unstaged);
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

    fn discard(&mut self, _: &Discard, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Item::File { staged: false, path }) = self.selected.clone() {
            self.ask_discard(vec![path], cx);
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
                let tracked: Vec<String> = st.unstaged.iter().filter(|c| c.status != FileStatus::Untracked).map(|c| c.path.clone()).collect();
                self.ask_discard(tracked, cx)
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

    /// Graph filter: Auto, All, or any pick of local and remote branches.
    fn render_scope(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.repo.read(cx);
        let (all, only) = (repo.graph_all, repo.graph_only.clone());
        let auto = !all && only.is_empty();
        let top = self.panel_bounds.get().size.height / px(1.) * if self.changes_open && self.graph_open { self.split } else { 0. } + 60.;
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
                let tracked: Vec<String> = {
                    let st = &self.repo.read(cx).status;
                    st.unstaged.iter().filter(|c| c.status != FileStatus::Untracked).map(|c| c.path.clone()).collect()
                };
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
                        .child(ui::icon_button(("discard-all", ix), "discard").on_click(cx.listener(move |panel, _, _, cx| {
                            cx.stop_propagation();
                            panel.ask_discard(tracked.clone(), cx);
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
            ListRow::Folder { staged, dir, paths } => {
                let (staged, paths) = (*staged, paths.clone());
                div()
                    .id(("folder", ix))
                    .group("folder")
                    .flex()
                    .items_center()
                    .h(theme::row_h())
                    .pl(px(22.))
                    .pr(px(8.))
                    .gap(px(4.))
                    .hover(|s| s.bg(theme::hover()))
                    .child(ui::small_icon("chevron-down", theme::fg()))
                    .child(ui::folder_icon(dir.rsplit('/').next().unwrap_or(dir), true))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme::fg()).child(dir.replace('/', " / ")))
                    .child(
                        div().invisible().group_hover("folder", |s| s.visible()).child(
                            ui::icon_button(("folder-stage", ix), if staged { "minus" } else { "plus" }).on_click(cx.listener(move |panel, _, _, cx| {
                                if staged { panel.unstage(paths.clone(), cx) } else { panel.stage(paths.clone(), cx) }
                            })),
                        ),
                    )
                    .child(ui::small_icon("dot", theme::git_modified()).size(px(8.)))
                    .into_any_element()
            }
            ListRow::File { staged, change, nested } => {
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
                    .pl(px(if *nested { 40. } else { 26. }))
                    .pr(px(8.))
                    .gap(px(5.))
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
                            .when(!*nested && !dir.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(dir))),
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
                            .when(!staged && !untracked, |d| {
                                d.child(ui::icon_button(("discard", ix), "discard").on_click(cx.listener(move |panel, _, _, cx| {
                                    cx.stop_propagation();
                                    panel.ask_discard(vec![c2.path.clone()], cx);
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
        let commit_label = if self.amend { "Commit (Amend)" } else { "Commit" };
        let rows = self.list_rows(cx);
        div()
            .id("changes-scroll")
            .flex()
            .flex_col()
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

    fn render_graph_row(&self, ix: usize, row: &GraphRow, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let repo = self.repo.read(cx);
        let Some(commit) = repo.commits.get(ix) else { return div().into_any_element() };
        let head_oid = repo.status.head_oid.clone().unwrap_or_default();
        let is_head = commit.sha == head_oid;
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
                for s in &row2.top {
                    let (a, b) = (point(x(s.from), top), point(x(s.to), mid));
                    stroke(a, b, (s.from != s.to).then(|| point(x(s.from), mid)), colors[s.color % colors.len()]);
                }
                for s in &row2.bottom {
                    let (a, b) = (point(x(s.from), mid), point(x(s.to), bottom));
                    stroke(a, b, (s.from != s.to).then(|| point(x(s.to), mid)), colors[s.color % colors.len()]);
                }
                let color = colors[row2.color % colors.len()];
                let c = point(x(row2.lane), mid);
                let circle = |r: f32, color: Hsla| fill(Bounds::new(point(c.x - px(r), c.y - px(r)), size(px(r * 2.), px(r * 2.))), color).corner_radii(px(r));
                if is_head {
                    window.paint_quad(circle(5.5, color));
                    window.paint_quad(circle(3.3, bg));
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
        let badges = commit.refs.iter().filter_map(|r| {
            let (color, icon_name) = match r.kind {
                RefKind::Local => (theme::ref_local(), if r.is_head { "target" } else { "git" }),
                RefKind::Remote => (theme::ref_remote(), "cloud"),
                RefKind::Tag => (theme::ref_tag(), "tag"),
            };
            Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(3.))
                    .pl(px(5.))
                    .pr(px(7.))
                    .h(px(18.))
                    .rounded(px(9.))
                    .bg(color)
                    .text_color(theme::c(0xffffff))
                    .text_size(px(10.5))
                    .child(ui::small_icon(icon_name, theme::c(0xffffff)).size(px(12.)))
                    .child(r.name.clone()),
            )
        });

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
            .when(selected && focused, |d| d.border_1().border_color(theme::focus_border()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |panel, _, window, cx| {
                window.focus(&panel.focus);
                panel.selected = Some(Item::Commit(ix));
                cx.emit(Request::OpenCommit(sha.clone()));
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
                    .child(div().text_color(theme::fg()).when(is_head, |d| d.font_weight(FontWeight::BOLD)).child(commit.subject.clone()))
                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(commit.author.clone())),
            )
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
                        p.split = (((ev.position.y - b.top()) / px(1.) - 35.) / (h - 35.)).clamp(0.12, 0.88);
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
                    .text_size(px(10.5))
                    .child(div().flex_1().child("SOURCE CONTROL"))
                    .child(ui::icon_button("scm-menu", "ellipsis").on_click(cx.listener(|p, _, _, cx| {
                        p.menu_open = !p.menu_open;
                        cx.notify();
                    }))),
            )
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
                    .when(self.changes_open, |d| d.child(div().flex_1().min_h_0().child(self.render_changes(focused, window, cx)))),
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

