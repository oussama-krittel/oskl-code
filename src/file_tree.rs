//! Explorer: a lazily loaded file tree with git status colors.

use crate::repo::Repo;
use crate::request::Request;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

actions!(file_tree, [Down, Up, Expand, Collapse, Open, Top, Bottom, NewFile, NewFolder, Rename, CollapseAll]);

pub fn bind_keys(cx: &mut App) {
    let c = Some("FileTree");
    cx.bind_keys([
        KeyBinding::new("j", Down, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("k", Up, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("l", Expand, c),
        KeyBinding::new("right", Expand, c),
        KeyBinding::new("h", Collapse, c),
        KeyBinding::new("left", Collapse, c),
        KeyBinding::new("enter", Open, c),
        KeyBinding::new("o", Open, c),
        KeyBinding::new("space", Open, c),
        KeyBinding::new("g", Top, c),
        KeyBinding::new("shift-g", Bottom, c),
        KeyBinding::new("a", NewFile, c),
        KeyBinding::new("shift-a", NewFolder, c),
        KeyBinding::new("r", Rename, c),
        KeyBinding::new("f2", Rename, c),
        KeyBinding::new("shift-h", CollapseAll, c),
    ]);
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

#[derive(Clone)]
struct Row {
    entry: Entry,
    depth: usize,
    expanded: bool,
}

pub struct FileTree {
    focus: FocusHandle,
    root: PathBuf,
    repo: Entity<Repo>,
    expanded: HashSet<PathBuf>,
    children: HashMap<PathBuf, Vec<Entry>>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    _observe: Subscription,
}

impl EventEmitter<Request> for FileTree {}

impl FileTree {
    pub fn new(repo: Entity<Repo>, cx: &mut Context<Self>) -> Self {
        let root = repo.read(cx).root.clone();
        let observe = cx.observe(&repo, |_, _, cx| cx.notify());
        let mut tree = Self {
            focus: cx.focus_handle(),
            root: root.clone(),
            repo,
            expanded: HashSet::from([root]),
            children: HashMap::new(),
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            _observe: observe,
        };
        tree.rebuild();
        tree
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn load_dir(&mut self, dir: &Path) -> &Vec<Entry> {
        if !self.children.contains_key(dir) {
            let mut entries: Vec<Entry> = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            if name == ".git" || name == ".DS_Store" {
                                return None;
                            }
                            let path = e.path();
                            let is_dir = path.is_dir();
                            Some(Entry { path, name, is_dir })
                        })
                        .collect()
                })
                .unwrap_or_default();
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            self.children.insert(dir.to_path_buf(), entries);
        }
        &self.children[dir]
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.push_children(&root, 0, &mut rows);
        self.rows = rows;
    }

    fn push_children(&mut self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let entries = self.load_dir(dir).clone();
        for entry in entries {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row { entry, depth, expanded });
            if expanded {
                self.push_children(&path, depth + 1, rows);
            }
        }
    }

    /// Drops cached listings for these directories (after file system changes).
    pub fn invalidate(&mut self, dirs: &HashSet<PathBuf>, cx: &mut Context<Self>) {
        let mut changed = false;
        for d in dirs {
            changed |= self.children.remove(d).is_some();
        }
        if changed {
            self.rebuild();
            cx.notify();
        }
    }

    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let mut p = path.parent();
        while let Some(dir) = p {
            if !dir.starts_with(&self.root) {
                break;
            }
            self.expanded.insert(dir.to_path_buf());
            p = dir.parent();
        }
        self.selected = Some(path.to_path_buf());
        self.rebuild();
        if let Some(ix) = self.rows.iter().position(|r| r.entry.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn selected_ix(&self) -> Option<usize> {
        let sel = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.entry.path == sel)
    }

    fn select_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(ix) {
            self.selected = Some(row.entry.path.clone());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn activate(&mut self, ix: usize, preview: bool, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        self.selected = Some(row.entry.path.clone());
        if row.entry.is_dir {
            if !self.expanded.remove(&row.entry.path) {
                self.expanded.insert(row.entry.path.clone());
            }
            self.rebuild();
        } else {
            cx.emit(Request::OpenFile { path: row.entry.path.clone(), line: None, preview });
        }
        cx.notify();
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected_ix().map(|i| i + 1).unwrap_or(0);
        self.select_ix(ix.min(self.rows.len().saturating_sub(1)), cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected_ix().map(|i| i.saturating_sub(1)).unwrap_or(0);
        self.select_ix(ix, cx);
    }
    fn top(&mut self, _: &Top, _: &mut Window, cx: &mut Context<Self>) {
        self.select_ix(0, cx);
    }
    fn bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) {
        self.select_ix(self.rows.len().saturating_sub(1), cx);
    }
    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_ix() else { return };
        let row = self.rows[ix].clone();
        if row.entry.is_dir && !row.expanded {
            self.activate(ix, true, cx);
        } else if row.entry.is_dir {
            self.select_ix(ix + 1, cx);
        } else {
            self.activate(ix, true, cx);
        }
    }
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_ix() else { return };
        let row = self.rows[ix].clone();
        if row.entry.is_dir && row.expanded {
            self.activate(ix, true, cx);
        } else if let Some(parent) = row.entry.path.parent()
            && let Some(pix) = self.rows.iter().position(|r| r.entry.path == parent)
        {
            self.select_ix(pix, cx);
        }
    }
    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_ix() {
            self.activate(ix, false, cx);
        }
    }
    fn collapse_all(&mut self, _: &CollapseAll, _: &mut Window, cx: &mut Context<Self>) {
        self.expanded.retain(|p| p == &self.root);
        self.rebuild();
        cx.notify();
    }

    /// Directory new files go into: the selected folder, or the selected file's folder.
    fn target_dir(&self) -> String {
        let dir = match self.selected.as_ref().and_then(|s| self.rows.iter().find(|r| &r.entry.path == s)) {
            Some(r) if r.entry.is_dir => r.entry.path.clone(),
            Some(r) => r.entry.path.parent().map(Path::to_path_buf).unwrap_or(self.root.clone()),
            None => self.root.clone(),
        };
        let rel = dir.strip_prefix(&self.root).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        if rel.is_empty() { String::new() } else { format!("{rel}/") }
    }

    fn new_file(&mut self, _: &NewFile, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Request::CommandLine(format!("new {}", self.target_dir())));
    }
    fn new_folder(&mut self, _: &NewFolder, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Request::CommandLine(format!("mkdir {}", self.target_dir())));
    }
    fn rename(&mut self, _: &Rename, _: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = &self.selected else { return };
        let rel = sel.strip_prefix(&self.root).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        cx.emit(Request::CommandLine(format!("mv {rel} {rel}")));
    }

    fn render_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[ix];
        let repo = self.repo.read(cx);
        let rel = row.entry.path.strip_prefix(&self.root).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let ignored = repo.is_ignored(&rel);
        let status = if row.entry.is_dir { None } else { repo.status_of(&rel) };
        let dirty_dir = row.entry.is_dir && repo.dir_is_dirty(&rel);
        let name_color = match (status, ignored) {
            (Some(s), _) => ui::status_color(s),
            (None, true) => theme::git_ignored(),
            _ if dirty_dir => theme::git_modified(),
            _ => theme::fg(),
        };
        let selected = self.selected.as_ref() == Some(&row.entry.path);
        div()
            .id(("tree-row", ix))
            .flex()
            .items_center()
            .h(theme::row_h())
            .pl(px(8. + row.depth as f32 * 12.))
            .pr(px(10.))
            .gap(px(4.))
            .cursor_pointer()
            .when(selected, |d| {
                d.bg(if focused { theme::selected_focused() } else { theme::selected() })
                    .when(focused, |d| d.border_1().border_color(theme::focus_border()))
            })
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |tree, ev: &ClickEvent, window, cx| {
                window.focus(&tree.focus);
                tree.activate(ix, ev.click_count() < 2, cx);
            }))
            .when(!row.entry.is_dir, |d| {
                let drag = ui::DraggedFile { path: row.entry.path.clone(), label: row.entry.name.clone() };
                d.on_drag(drag, |f: &ui::DraggedFile, _, _, cx| cx.new(|_| ui::DragChip(f.label.clone())))
            })
            .child(if row.entry.is_dir {
                ui::small_icon(if row.expanded { "chevron-down" } else { "chevron-right" }, theme::fg())
            } else {
                ui::small_icon("dot", transparent_black())
            })
            .child(if row.entry.is_dir {
                ui::folder_icon(&row.entry.name, row.expanded)
            } else {
                ui::file_icon(&row.entry.name)
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(name_color)
                    .child(row.entry.name.clone()),
            )
            .when_some(status, |d, s| d.child(div().text_size(px(11.)).text_color(ui::status_color(s)).child(s.letter())))
            .when(dirty_dir && status.is_none(), |d| d.child(ui::small_icon("dot", theme::git_modified()).size(px(8.))))
            .into_any_element()
    }
}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let project = self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        div()
            .key_context("FileTree")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::top))
            .on_action(cx.listener(Self::bottom))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::rename))
            .on_action(cx.listener(Self::collapse_all))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .px(px(20.))
                    .text_size(px(10.5))
                    .child(div().flex_1().child("EXPLORER")),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(22.))
                    .pl(px(4.))
                    .pr(px(6.))
                    .gap(px(2.))
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(10.5))
                    .child(ui::small_icon("chevron-down", theme::fg()))
                    .child(div().flex_1().child(project.to_uppercase()))
                    .child(ui::icon_button("tree-new-file", "file").on_click(cx.listener(|t, _, w, cx| t.new_file(&NewFile, w, cx))))
                    .child(ui::icon_button("tree-new-folder", "folder").on_click(cx.listener(|t, _, w, cx| t.new_folder(&NewFolder, w, cx))))
                    .child(ui::icon_button("tree-refresh", "refresh").on_click(cx.listener(|t, _, _, cx| {
                        t.children.clear();
                        t.rebuild();
                        cx.notify();
                    })))
                    .child(ui::icon_button("tree-collapse", "minus").on_click(cx.listener(|t, _, w, cx| t.collapse_all(&CollapseAll, w, cx)))),
            )
            .child(
                uniform_list(
                    "file-tree",
                    self.rows.len(),
                    cx.processor(move |tree, range: Range<usize>, _window, cx| {
                        range.map(|ix| tree.render_row(ix, focused, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
    }
}
