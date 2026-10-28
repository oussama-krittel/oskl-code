//! Explorer: a lazily loaded file tree with git status colors, and the usual file operations
//! (new, rename, delete to Trash, cut/copy/paste, duplicate, reveal in Finder) from the
//! right-click menu or the keyboard.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::repo::Repo;
use crate::request::Request;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

actions!(
    file_tree,
    [
        Down, Up, Expand, Collapse, Open, Top, Bottom, NewFile, NewFolder, Rename, CollapseAll, Delete, CopyFile, CutFile, PasteFile,
        Duplicate, CopyPath, CopyRelativePath, RevealInFinder,
    ]
);

pub fn bind_keys() {
    // Off while a name box is open, so typing a name doesn't trigger the one-letter keys.
    let c = Some("FileTree && !naming");
    crate::keybindings::collect([
        crate::keybindings::kb("{down}", Down, c),
        crate::keybindings::kb("down", Down, c),
        crate::keybindings::kb("{up}", Up, c),
        crate::keybindings::kb("up", Up, c),
        crate::keybindings::kb("{right}", Expand, c),
        crate::keybindings::kb("right", Expand, c),
        crate::keybindings::kb("{left}", Collapse, c),
        crate::keybindings::kb("left", Collapse, c),
        crate::keybindings::kb("enter", Open, c),
        crate::keybindings::kb("o", Open, c),
        crate::keybindings::kb("space", Open, c),
        crate::keybindings::kb("g", Top, c),
        crate::keybindings::kb("shift-g", Bottom, c),
        crate::keybindings::kb("a", NewFile, c),
        crate::keybindings::kb("shift-a", NewFolder, c),
        crate::keybindings::kb("r", Rename, c),
        crate::keybindings::kb("f2", Rename, c),
        crate::keybindings::kb("shift-h", CollapseAll, c),
        crate::keybindings::kb("cmd-backspace", Delete, c),
        crate::keybindings::kb("delete", Delete, c),
        crate::keybindings::kb("cmd-c", CopyFile, c),
        crate::keybindings::kb("cmd-x", CutFile, c),
        crate::keybindings::kb("cmd-v", PasteFile, c),
        crate::keybindings::kb("cmd-d", Duplicate, c),
        crate::keybindings::kb("alt-cmd-c", CopyPath, c),
        crate::keybindings::kb("alt-cmd-shift-c", CopyRelativePath, c),
        crate::keybindings::kb("alt-cmd-r", RevealInFinder, c),
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
    /// The name box of a file/folder being created.
    pending: bool,
}

#[derive(Clone, PartialEq)]
enum EditKind {
    NewFile,
    NewFolder,
    Rename(PathBuf),
}

/// The name box shown in the tree while creating or renaming.
struct Inline {
    kind: EditKind,
    dir: PathBuf,
    input: Entity<Editor>,
    _subs: Vec<Subscription>,
}

type MenuAct = fn(&mut FileTree, &mut Window, &mut Context<FileTree>);
type MenuItem = (&'static str, &'static str, bool, MenuAct);

pub struct FileTree {
    focus: FocusHandle,
    root: PathBuf,
    repo: Entity<Repo>,
    /// Folders added to the workspace: more roots under the project's.
    extra: Vec<PathBuf>,
    /// Every repository in the workspace (a file's git colors come from the innermost one).
    repos: Vec<Entity<Repo>>,
    _repo_subs: Vec<Subscription>,
    expanded: HashSet<PathBuf>,
    children: HashMap<PathBuf, Vec<Entry>>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    editing: Option<Inline>,
    /// Files copied (false) or cut (true) for the next paste.
    clip: Option<(Vec<PathBuf>, bool)>,
    /// Right-click menu: where, and for which entry (the root for empty space).
    menu: Option<(Point<Pixels>, PathBuf)>,
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
            repos: vec![repo.clone()],
            repo,
            extra: Vec::new(),
            _repo_subs: Vec::new(),
            expanded: HashSet::from([root]),
            children: HashMap::new(),
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            editing: None,
            clip: None,
            menu: None,
            _observe: observe,
        };
        tree.rebuild();
        tree
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    #[cfg(test)]
    pub(crate) fn select(&mut self, path: PathBuf) {
        self.selected = Some(path);
    }

    #[cfg(test)]
    pub(crate) fn row_names(&self) -> Vec<(usize, String)> {
        self.rows.iter().map(|r| (r.depth, r.entry.name.clone())).collect()
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

    /// The workspace's folders or repositories changed.
    pub fn set_workspace(&mut self, extra: Vec<PathBuf>, repos: Vec<Entity<Repo>>, cx: &mut Context<Self>) {
        for r in extra.iter().filter(|r| !self.extra.contains(r)) {
            self.expanded.insert(r.clone());
        }
        self._repo_subs = repos.iter().filter(|r| **r != self.repo).map(|r| cx.observe(r, |_, _, cx| cx.notify())).collect();
        self.repos = repos;
        self.extra = extra;
        self.rebuild();
        cx.notify();
    }

    /// The project folder, then the added ones.
    fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.root.clone()).chain(self.extra.iter().cloned()).collect()
    }

    fn is_root(&self, p: &Path) -> bool {
        p == self.root || self.extra.iter().any(|r| r == p)
    }

    /// The root (project or added folder) a path is in.
    fn root_of(&self, p: &Path) -> PathBuf {
        self.roots().into_iter().filter(|r| p.starts_with(r)).max_by_key(|r| r.components().count()).unwrap_or_else(|| self.root.clone())
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for root in self.roots() {
            let expanded = self.expanded.contains(&root);
            let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.to_string_lossy().to_string());
            rows.push(Row { entry: Entry { path: root.clone(), name, is_dir: true }, depth: 0, expanded, pending: false });
            if expanded {
                self.push_children(&root, 1, &mut rows);
            }
        }
        self.rows = rows;
    }

    fn push_children(&mut self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        if let Some(ed) = self.editing.as_ref().filter(|e| e.dir == dir && matches!(e.kind, EditKind::NewFile | EditKind::NewFolder)) {
            let is_dir = ed.kind == EditKind::NewFolder;
            rows.push(Row { entry: Entry { path: dir.join("\u{0}new"), name: String::new(), is_dir }, depth, expanded: false, pending: true });
        }
        let entries = self.load_dir(dir).clone();
        for entry in entries {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row { entry, depth, expanded, pending: false });
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

    /// Re-reads these folders now (after our own changes, before the watcher catches up).
    fn refresh_dirs<'a>(&mut self, dirs: impl IntoIterator<Item = &'a Path>) {
        for d in dirs {
            self.children.remove(d);
        }
        self.rebuild();
    }

    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let mut p = path.parent();
        while let Some(dir) = p {
            if !self.roots().iter().any(|r| dir.starts_with(r)) {
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
        if row.pending {
            return;
        }
        self.selected = Some(row.entry.path.clone());
        if row.entry.is_dir {
            if !self.expanded.remove(&row.entry.path) {
                self.expanded.insert(row.entry.path.clone());
            }
            self.rebuild();
        } else if preview {
            // Single click: show it, but keep the keyboard here (⌘C, ⌘⌫… act on the file).
            cx.emit(Request::ShowFile(row.entry.path.clone()));
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
        let roots = self.roots();
        self.expanded.retain(|p| roots.contains(p));
        self.rebuild();
        cx.notify();
    }

    /// The selected entry, if it's a real file or folder (not the project root).
    fn target(&self) -> Option<PathBuf> {
        self.selected.clone().filter(|p| !self.is_root(p) && self.roots().iter().any(|r| p.starts_with(r)) && p.exists())
    }

    /// Folder new files and pastes go into: the selected folder, or the selected file's folder.
    fn target_dir(&self) -> PathBuf {
        match self.selected.as_ref().filter(|p| p.exists()) {
            Some(p) if p.is_dir() => p.clone(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or(self.root.clone()),
            None => self.root.clone(),
        }
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(self.root_of(p)).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string_lossy().to_string())
    }

    // ------------------------------------------------------------ name box (new / rename)

    fn start_inline(&mut self, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let (dir, text) = match &kind {
            EditKind::Rename(p) => (p.parent().map(Path::to_path_buf).unwrap_or(self.root.clone()), p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
            _ => (self.target_dir(), String::new()),
        };
        let input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.set_text(&text, cx);
            // Renaming selects the name without its extension, like Finder.
            let stem = match text.rfind('.') {
                Some(i) if i > 0 => i,
                _ => text.chars().count(),
            };
            ed.select_range(0..text[..stem].chars().count(), cx);
            ed
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |tree, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => tree.commit_inline(window, cx),
                EditorEvent::Cancel => tree.cancel_inline(window, cx),
                _ => {}
            }),
            // Clicking elsewhere keeps what was typed, like VS Code.
            cx.on_blur(&input.focus_handle(cx), window, |tree, window, cx| {
                if tree.editing.is_some() {
                    tree.commit_inline(window, cx);
                }
            }),
        ];
        if !matches!(kind, EditKind::Rename(_)) {
            self.expanded.insert(dir.clone());
        }
        self.editing = Some(Inline { kind, dir, input: input.clone(), _subs: subs });
        self.rebuild();
        let ix = self.rows.iter().position(|r| r.pending || self.editing.as_ref().is_some_and(|e| e.kind == EditKind::Rename(r.entry.path.clone())));
        if let Some(ix) = ix {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        input.read(cx).focus(window);
        // And again once the box is on screen: focus given to something not yet drawn can be lost.
        let again = input.clone();
        cx.on_next_frame(window, move |tree, window, cx| {
            if tree.editing.as_ref().is_some_and(|e| e.input == again) {
                again.read(cx).focus(window);
            }
        });
        cx.notify();
    }

    fn cancel_inline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.rebuild();
        window.focus(&self.focus);
        cx.notify();
    }

    fn commit_inline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.editing.take() else { return };
        let name = ed.input.read(cx).text().trim().to_string();
        window.focus(&self.focus);
        if name.is_empty() || name.contains('\0') {
            self.rebuild();
            return cx.notify();
        }
        let result = match &ed.kind {
            EditKind::NewFile => {
                let path = ed.dir.join(&name);
                if path.exists() {
                    Err(format!("{name} already exists"))
                } else {
                    let made = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, ""));
                    made.map(|_| {
                        cx.emit(Request::OpenFile { path: path.clone(), line: None, preview: false });
                        path
                    })
                    .map_err(|e| format!("Couldn't create {name}: {e}"))
                }
            }
            EditKind::NewFolder => {
                let path = ed.dir.join(&name);
                if path.exists() {
                    Err(format!("{name} already exists"))
                } else {
                    std::fs::create_dir_all(&path).map(|_| path).map_err(|e| format!("Couldn't create {name}: {e}"))
                }
            }
            EditKind::Rename(from) => {
                let to = ed.dir.join(&name);
                if &to == from {
                    Ok(to)
                } else if to.exists() && !same_file_other_case(from, &to) {
                    Err(format!("{name} already exists"))
                } else {
                    let made = to.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::rename(from, &to));
                    made.map(|_| {
                        cx.emit(Request::Moved { from: from.clone(), to: to.clone() });
                        to
                    })
                    .map_err(|e| format!("Couldn't rename: {e}"))
                }
            }
        };
        match result {
            Ok(path) => {
                let dirs: Vec<PathBuf> = [Some(ed.dir.clone()), path.parent().map(Path::to_path_buf)].into_iter().flatten().collect();
                self.refresh_dirs(dirs.iter().map(PathBuf::as_path));
                self.reveal(&path, cx);
            }
            Err(e) => {
                self.rebuild();
                cx.emit(Request::Error(e));
            }
        }
        cx.notify();
    }

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        self.start_inline(EditKind::NewFile, window, cx);
    }
    fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.start_inline(EditKind::NewFolder, window, cx);
    }
    fn rename(&mut self, _: &Rename, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.target() {
            self.start_inline(EditKind::Rename(p), window, cx);
        }
    }

    // ------------------------------------------------------------ delete, clipboard

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(path) = self.target() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let what = if path.is_dir() { format!("the folder '{name}' and its contents") } else { format!("'{name}'") };
        let _ = window;
        let tree = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            cancel_default: false,
            title: format!("Are you sure you want to delete {what}?"),
            detail: "You can restore it from the Trash.".into(),
            buttons: vec!["Move to Trash".into()],
            danger: true,
            on_answer: std::rc::Rc::new(move |_, _, cx| {
                let path = path.clone();
                tree.update(cx, |tree, cx| tree.trash(path, cx)).ok();
            }),
        }));
    }

    pub(crate) fn trash(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // The next row (or the one before) stays selected.
        let next = self.selected_ix().and_then(|ix| {
            let depth = self.rows[ix].depth;
            let after = self.rows[ix + 1..].iter().position(|r| r.depth <= depth).map(|i| ix + 1 + i).filter(|&i| self.rows[i].depth == depth);
            after.or(ix.checked_sub(1)).map(|i| self.rows[i].entry.path.clone())
        });
        match trash::delete(&path) {
            Ok(()) => {
                cx.emit(Request::Deleted(vec![path.clone()]));
                if let Some(parent) = path.parent() {
                    self.refresh_dirs([parent]);
                }
                self.selected = next;
                self.clip = self.clip.take().filter(|(ps, _)| !ps.contains(&path));
            }
            Err(e) => cx.emit(Request::Error(format!("Couldn't move to Trash: {e}"))),
        }
        cx.notify();
    }

    fn copy_file(&mut self, _: &CopyFile, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(p) = self.target() {
            cx.emit(Request::Message(format!("Copied {} — ⌘V in a folder to paste", self.rel(&p))));
            self.clip = Some((vec![p], false));
            cx.notify();
        }
    }

    fn cut_file(&mut self, _: &CutFile, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(p) = self.target() {
            cx.emit(Request::Message(format!("Cut {} — ⌘V in a folder to move it there", self.rel(&p))));
            self.clip = Some((vec![p], true));
            cx.notify();
        }
    }

    fn paste_file(&mut self, _: &PasteFile, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some((paths, cut)) = self.clip.clone() else { return };
        let dir = self.target_dir();
        let mut last = None;
        let mut touched: Vec<PathBuf> = vec![dir.clone()];
        for from in paths.into_iter().filter(|p| p.exists()) {
            let Some(name) = from.file_name() else { continue };
            if cut {
                if dir.starts_with(&from) {
                    cx.emit(Request::Error("Can't move a folder into itself".into()));
                    continue;
                }
                if from.parent() == Some(dir.as_path()) {
                    last = Some(from);
                    continue;
                }
                let to = unique_path(&dir.join(name));
                match std::fs::rename(&from, &to) {
                    Ok(()) => {
                        touched.extend(from.parent().map(Path::to_path_buf));
                        cx.emit(Request::Moved { from, to: to.clone() });
                        last = Some(to);
                    }
                    Err(e) => cx.emit(Request::Error(format!("Couldn't move: {e}"))),
                }
            } else {
                if dir.starts_with(&from) && from.is_dir() {
                    cx.emit(Request::Error("Can't copy a folder into itself".into()));
                    continue;
                }
                let to = unique_path(&dir.join(name));
                match copy_recursive(&from, &to) {
                    Ok(()) => last = Some(to),
                    Err(e) => cx.emit(Request::Error(format!("Couldn't copy: {e}"))),
                }
            }
        }
        if cut {
            self.clip = None;
        }
        self.expanded.insert(dir);
        self.refresh_dirs(touched.iter().map(PathBuf::as_path));
        if let Some(p) = last {
            self.reveal(&p, cx);
        }
        cx.notify();
    }

    fn duplicate(&mut self, _: &Duplicate, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(from) = self.target() else { return };
        let to = unique_path(&from);
        match copy_recursive(&from, &to) {
            Ok(()) => {
                if let Some(parent) = to.parent() {
                    self.refresh_dirs([parent]);
                }
                self.reveal(&to, cx);
            }
            Err(e) => cx.emit(Request::Error(format!("Couldn't duplicate: {e}"))),
        }
    }

    fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let p = self.selected.clone().unwrap_or(self.root.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(p.to_string_lossy().to_string()));
        cx.emit(Request::Message("Copied path".into()));
    }

    fn copy_relative_path(&mut self, _: &CopyRelativePath, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let p = self.selected.clone().unwrap_or(self.root.clone());
        let rel = if self.is_root(&p) { ".".to_string() } else { self.rel(&p) };
        cx.write_to_clipboard(ClipboardItem::new_string(rel));
        cx.emit(Request::Message("Copied relative path".into()));
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let p = self.selected.clone().filter(|p| p.exists()).unwrap_or(self.root.clone());
        if let Err(e) = std::process::Command::new("open").arg("-R").arg(&p).spawn() {
            cx.emit(Request::Error(format!("Couldn't open Finder: {e}")));
        }
        cx.notify();
    }

    fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        cx.emit(Request::OpenTerminal(self.target_dir()));
        cx.notify();
    }

    // ------------------------------------------------------------ rendering

    fn open_menu(&mut self, path: PathBuf, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.commit_inline(window, cx);
        }
        window.focus(&self.focus);
        self.selected = Some(path.clone());
        self.menu = Some((at, path));
        cx.notify();
    }

    /// The right-click menu for `path`: (label, keys, enabled, action), None for a separator.
    fn menu_items(&self, path: &Path) -> Vec<Option<MenuItem>> {
        let is_root = self.is_root(path);
        let added = self.extra.iter().any(|r| r == path);
        let can_paste = self.clip.is_some();
        let mut items: Vec<Option<MenuItem>> = vec![
            Some(("New File…", "A", true, |t, w, cx| t.new_file(&NewFile, w, cx))),
            Some(("New Folder…", "⇧A", true, |t, w, cx| t.new_folder(&NewFolder, w, cx))),
            None,
            Some(("Reveal in Finder", "⌥⌘R", true, |t, w, cx| t.reveal_in_finder(&RevealInFinder, w, cx))),
        ];
        // A terminal opens in a folder, so only folders offer it.
        if path.is_dir() {
            items.push(Some(("Open in Integrated Terminal", "", true, |t, _, cx| t.open_in_terminal(cx))));
        }
        items.push(None);
        if !is_root {
            items.push(Some(("Cut", "⌘X", true, |t, w, cx| t.cut_file(&CutFile, w, cx))));
            items.push(Some(("Copy", "⌘C", true, |t, w, cx| t.copy_file(&CopyFile, w, cx))));
        }
        items.push(Some(("Paste", "⌘V", can_paste, |t, w, cx| t.paste_file(&PasteFile, w, cx))));
        if !is_root {
            items.push(Some(("Duplicate", "⌘D", true, |t, w, cx| t.duplicate(&Duplicate, w, cx))));
        }
        items.push(None);
        items.push(Some(("Copy Path", "⌥⌘C", true, |t, w, cx| t.copy_path(&CopyPath, w, cx))));
        items.push(Some(("Copy Relative Path", "⌥⇧⌘C", true, |t, w, cx| t.copy_relative_path(&CopyRelativePath, w, cx))));
        if !is_root {
            items.push(None);
            items.push(Some(("Rename…", "F2", true, |t, w, cx| t.rename(&Rename, w, cx))));
            items.push(Some(("Delete", "⌘⌫", true, |t, w, cx| t.delete(&Delete, w, cx))));
        }
        // Workspace folders are managed from the roots (or the empty space below the files).
        if is_root {
            items.push(None);
            items.push(Some(("Add Folder to Workspace…", "", true, |t, _, cx| {
                t.menu = None;
                cx.emit(Request::AddFolder);
                cx.notify();
            })));
        }
        if added {
            items.push(Some(("Remove Folder from Workspace", "", true, |t, _, cx| {
                if let Some((_, p)) = t.menu.take() {
                    cx.emit(Request::RemoveFolder(p));
                }
                cx.notify();
            })));
        }
        items
    }

    #[cfg(test)]
    pub(crate) fn menu_labels(&self, path: &Path) -> Vec<&'static str> {
        self.menu_items(path).into_iter().flatten().map(|i| i.0).collect()
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, path) = self.menu.clone()?;
        let items = self.menu_items(&path);
        let list = div()
            .id("tree-menu")
            .occlude()
            .w(px(240.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(12.))
            .on_mouse_down_out(cx.listener(|t, _, _, cx| {
                t.menu = None;
                cx.notify();
            }))
            .children(items.into_iter().enumerate().map(|(ix, item)| match item {
                None => div().my(px(4.)).h(px(1.)).bg(theme::border()).into_any_element(),
                Some((label, keys, enabled, act)) => div()
                    .id(("tree-menu-item", ix))
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .px(px(10.))
                    .mx(px(4.))
                    .rounded(px(3.))
                    .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::selected_focused())).on_click(cx.listener(move |t, _, w, cx| act(t, w, cx))))
                    .when(!enabled, |d| d.text_color(theme::fg_muted()))
                    .child(div().flex_1().child(label))
                    .child(div().text_color(theme::fg_muted()).child(keys))
                    .into_any_element(),
            }));
        Some(deferred(anchored().position(at).snap_to_window().child(list)).with_priority(1).into_any_element())
    }

    fn render_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[ix];
        let is_root = row.depth == 0;
        let owner = self
            .repos
            .iter()
            .filter(|r| row.entry.path.starts_with(&r.read(cx).root))
            .max_by_key(|r| r.read(cx).root.components().count())
            .unwrap_or(&self.repo);
        let repo = owner.read(cx);
        let rel = row.entry.path.strip_prefix(&repo.root).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let ignored = !is_root && !rel.is_empty() && repo.is_ignored(&rel);
        let status = if row.entry.is_dir || row.pending { None } else { repo.status_of(&rel) };
        let dirty_dir = row.entry.is_dir && !is_root && !rel.is_empty() && repo.dir_is_dirty(&rel);
        let name_color = match (status, ignored) {
            (Some(s), _) => ui::status_color(s),
            (None, true) => theme::git_ignored(),
            _ if dirty_dir => theme::git_modified(),
            _ => theme::fg(),
        };
        let cut = self.clip.as_ref().is_some_and(|(ps, cut)| *cut && ps.contains(&row.entry.path));
        let selected = !row.pending && self.selected.as_ref() == Some(&row.entry.path);
        let input = self.editing.as_ref().filter(|e| row.pending || e.kind == EditKind::Rename(row.entry.path.clone())).map(|e| e.input.clone());
        let path = row.entry.path.clone();
        let selector = format!("tree-row-{}", row.entry.name);
        div()
            .id(("tree-row", ix))
            .debug_selector(move || selector)
            .flex()
            .items_center()
            .h(theme::row_h())
            .pl(px(if is_root { 4. } else { 8. + (row.depth - 1) as f32 * 12. }))
            .pr(px(10.))
            .gap(px(4.))
            .cursor_pointer()
            .when(cut, |d| d.opacity(0.5))
            .when(selected, |d| {
                d.bg(if focused { theme::selected_focused() } else { theme::selected() })
                    .when(focused, |d| d.border_1().border_color(theme::focus_border()))
            })
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |tree, ev: &ClickEvent, window, cx| {
                tree.menu = None;
                window.focus(&tree.focus);
                tree.activate(ix, ev.click_count() < 2, cx);
            }))
            .when(!row.pending, |d| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |tree, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        tree.open_menu(path.clone(), ev.position, window, cx);
                    }),
                )
            })
            .when(!row.entry.is_dir && !row.pending, |d| {
                let drag = ui::DraggedFile { path: row.entry.path.clone(), label: row.entry.name.clone() };
                d.on_drag(drag, |f: &ui::DraggedFile, _, _, cx| cx.new(|_| ui::DragChip(f.label.clone())))
            })
            .child(if row.entry.is_dir && !row.pending {
                ui::small_icon(if row.expanded { "chevron-down" } else { "chevron-right" }, theme::fg())
            } else {
                ui::small_icon("dot", transparent_black())
            })
            .when(!is_root, |d| {
                d.child(if row.entry.is_dir {
                    ui::folder_icon(&row.entry.name, row.expanded)
                } else if row.pending {
                    ui::small_icon("file", theme::fg_dim())
                } else {
                    ui::file_icon(&row.entry.name)
                })
            })
            .child(match input {
                Some(input) => div().flex_1().min_w_0().h(px(20.)).border_1().border_color(theme::focus_border()).child(input).into_any_element(),
                None => div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(name_color)
                    .when(is_root, |d| d.font_weight(FontWeight::BOLD).text_size(px(10.5)))
                    .child(if is_root { row.entry.name.to_uppercase() } else { row.entry.name.clone() })
                    .into_any_element(),
            })
            .when_some(status, |d, s| d.child(div().text_size(px(11.)).text_color(ui::status_color(s)).child(s.letter())))
            .when(dirty_dir && status.is_none(), |d| d.child(ui::small_icon("dot", theme::git_modified()).size(px(8.))))
            .into_any_element()
    }
}

/// `name copy.ext`, `name copy 2.ext`… for a path that's taken (Finder's naming).
pub fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let dir = p.parent().unwrap_or(Path::new("."));
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !p.is_dir() => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let stem = stem.trim_end_matches(" copy").to_string();
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") }))
        .find(|c| !c.exists())
        .expect("a free name")
}

/// Copies a file, or a folder with everything in it.
pub fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Renaming `a.ts` to `A.ts` on a case-insensitive disk: the "existing" file is the same one.
fn same_file_other_case(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let head_button = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).tooltip(ui::tooltip(tip));
        div()
            .key_context(if self.editing.is_some() { "FileTree naming" } else { "FileTree" })
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
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::copy_file))
            .on_action(cx.listener(Self::cut_file))
            .on_action(cx.listener(Self::paste_file))
            .on_action(cx.listener(Self::duplicate))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::copy_relative_path))
            .on_action(cx.listener(Self::reveal_in_finder))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .pl(px(20.))
                    .pr(px(6.))
                    .gap(px(1.))
                    .text_size(px(10.5))
                    .child(div().flex_1().child("EXPLORER"))
                    .child(head_button("tree-new-file", "new-file", "New File… (A)").debug_selector(|| "tree-new-file".into()).on_click(cx.listener(|t, _, w, cx| t.new_file(&NewFile, w, cx))))
                    .child(head_button("tree-new-folder", "new-folder", "New Folder… (⇧A)").debug_selector(|| "tree-new-folder".into()).on_click(cx.listener(|t, _, w, cx| t.new_folder(&NewFolder, w, cx))))
                    .child(head_button("tree-refresh", "refresh", "Refresh").on_click(cx.listener(|t, _, _, cx| {
                        t.children.clear();
                        t.rebuild();
                        cx.notify();
                    })))
                    .child(head_button("tree-collapse", "collapse-all", "Collapse Folders (⇧H)").on_click(cx.listener(|t, _, w, cx| t.collapse_all(&CollapseAll, w, cx)))),
            )
            .child(
                div()
                    .id("tree-body")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    // Empty space below the files: the project root (paste / new file there).
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|tree, _, window, cx| {
                            tree.menu = None;
                            tree.selected = Some(tree.root.clone());
                            window.focus(&tree.focus);
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|tree, ev: &MouseDownEvent, window, cx| {
                            let root = tree.root.clone();
                            tree.open_menu(root, ev.position, window, cx);
                        }),
                    )
                    .child(
                        uniform_list(
                            "file-tree",
                            self.rows.len(),
                            cx.processor(move |tree, range: Range<usize>, _window, cx| range.map(|ix| tree.render_row(ix, focused, cx)).collect::<Vec<_>>()),
                        )
                        .track_scroll(self.scroll.clone())
                        .flex_1(),
                    ),
            )
            .children(self.render_menu(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_recursive, unique_path};

    #[test]
    fn copies_get_finder_names() {
        let d = std::env::temp_dir().join(format!("kite-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("a.ts"), "x").unwrap();
        std::fs::write(d.join("src/b.ts"), "y").unwrap();
        assert_eq!(unique_path(&d.join("a.ts")), d.join("a copy.ts"));
        std::fs::write(d.join("a copy.ts"), "").unwrap();
        assert_eq!(unique_path(&d.join("a.ts")), d.join("a copy 2.ts"));
        assert_eq!(unique_path(&d.join("src")), d.join("src copy"));
        copy_recursive(&d.join("src"), &d.join("src copy")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("src copy/b.ts")).unwrap(), "y");
        std::fs::remove_dir_all(&d).ok();
    }
}
