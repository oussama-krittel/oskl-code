//! Shared git state for the open project. Views observe it and re-render
//! when status or history changes.

use crate::git::undo::{self, Undo};
use crate::git::{self, FileStatus};
use crate::graph::{self, GraphRow};
use crate::theme;
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Commits read at a time for the history.
pub const HISTORY_PAGE: usize = 800;

pub enum RepoEvent {
    Message(String),
    Error(String),
    Output { title: String, text: String, ok: bool },
    /// The remote has commits we don't: offer pull-then-push.
    PushRejected,
    /// Something was done that `Repo::undo` can take back (the text says what).
    Undoable(String),
}

impl EventEmitter<RepoEvent> for Repo {}

pub struct Repo {
    pub root: PathBuf,
    pub is_git: bool,
    pub status: Arc<git::Status>,
    pub commits: Arc<Vec<git::Commit>>,
    pub graph: Arc<Vec<GraphRow>>,
    pub graph_all: bool,
    /// Branches picked in the graph filter; empty means Auto / All.
    pub graph_only: Vec<String>,
    /// The graph's search: only commits whose message, author or hash match.
    pub graph_search: Option<String>,
    /// The graph shows one file's history: its path, and the name it had in each commit.
    pub graph_file: Option<(String, Arc<HashMap<String, String>>)>,
    /// How many commits the history reads; lists ask for more as you scroll to their end.
    pub log_limit: usize,
    /// `commit.template`'s text, put in an empty commit box.
    pub template: Option<String>,
    /// Branches, remotes, tags, stashes, worktrees and submodules (read with the history).
    pub refs: Arc<git::RepoRefs>,
    pub user_email: Option<String>,
    pub files: Arc<Vec<String>>,
    file_status: HashMap<String, FileStatus>,
    dirty_dirs: HashSet<String>,
    pub busy: Option<String>,
    /// How to take back the last action done from the app (only the last one is kept).
    pub undo: Option<Undo>,
    status_task: Option<Task<()>>,
    log_task: Option<Task<()>>,
    files_task: Option<Task<()>>,
    op_task: Option<Task<()>>,
    status_running: bool,
    status_again: bool,
    log_running: bool,
    log_again: bool,
    files_running: bool,
    files_again: bool,
}

impl Repo {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let git_root = git::repo_root(&root);
        if std::env::var_os("OSKL_DEBUG").is_some() {
            eprintln!("[oskl] open {} -> git root {:?}", root.display(), git_root);
        }
        let is_git = git_root.is_some();
        Self::build(git_root.unwrap_or(root), is_git, cx)
    }

    /// Exactly this folder (a folder added to the workspace, or a repo found inside the
    /// project): git only if the folder is itself a repository's top.
    pub fn at(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let is_git = root.join(".git").exists();
        Self::build(root, is_git, cx)
    }

    fn build(root: PathBuf, is_git: bool, cx: &mut Context<Self>) -> Self {
        let user_email = if is_git { git::user_email(&root) } else { None };
        let template = if is_git { git::commit_template(&root) } else { None };
        let mut repo = Self {
            root,
            is_git,
            status: Arc::default(),
            commits: Arc::default(),
            graph: Arc::default(),
            graph_all: false,
            graph_only: Vec::new(),
            graph_search: None,
            graph_file: None,
            template,
            log_limit: HISTORY_PAGE,
            refs: Arc::default(),
            user_email,
            files: Arc::default(),
            file_status: HashMap::new(),
            dirty_dirs: HashSet::new(),
            busy: None,
            undo: None,
            status_task: None,
            log_task: None,
            files_task: None,
            op_task: None,
            status_running: false,
            status_again: false,
            log_running: false,
            log_again: false,
            files_running: false,
            files_again: false,
        };
        repo.refresh_all(cx);
        repo
    }

    pub fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        self.refresh_log(cx);
        self.refresh_files(cx);
    }

    /// Refreshes never cancel each other: if one is running, exactly one more runs after it.
    /// (Busy repos change constantly — shell prompts, dev servers, saves — and cancelling
    /// in-flight refreshes meant none ever finished.)
    pub fn refresh_status(&mut self, cx: &mut Context<Self>) {
        if !self.is_git {
            return;
        }
        if self.status_running {
            self.status_again = true;
            return;
        }
        self.status_running = true;
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move { git::status(&root) });
        self.status_task = Some(cx.spawn(async move |this, cx| {
            let status = task.await;
            if std::env::var_os("OSKL_DEBUG").is_some() {
                eprintln!("[oskl] status: {:?}", status.as_ref().map(|s| (s.branch.clone(), s.staged.len(), s.unstaged.len())));
            }
            this.update(cx, |repo, cx| {
                repo.status_running = false;
                match status {
                    Ok(status) => repo.set_status(status),
                    Err(e) => cx.emit(RepoEvent::Error(format!("git status failed: {e}"))),
                }
                if std::mem::take(&mut repo.status_again) {
                    repo.refresh_status(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_status(&mut self, status: git::Status) {
        self.file_status.clear();
        self.dirty_dirs.clear();
        for c in status.staged.iter().chain(&status.unstaged).chain(&status.conflicts) {
            let entry = self.file_status.entry(c.path.clone()).or_insert(c.status);
            // Working-tree state wins over index state for coloring.
            if c.status == FileStatus::Untracked || c.status == FileStatus::Conflicted {
                *entry = c.status;
            }
            let mut p = Path::new(&c.path).parent();
            while let Some(dir) = p {
                if dir.as_os_str().is_empty() {
                    break;
                }
                self.dirty_dirs.insert(dir.to_string_lossy().to_string());
                p = dir.parent();
            }
        }
        self.status = Arc::new(status);
    }

    /// The history is being read.
    pub fn log_loading(&self) -> bool {
        self.log_running
    }

    pub fn refresh_log(&mut self, cx: &mut Context<Self>) {
        if !self.is_git {
            return;
        }
        if self.log_running {
            self.log_again = true;
            return;
        }
        self.log_running = true;
        let root = self.root.clone();
        let all = self.graph_all;
        let only = self.graph_only.clone();
        let limit = self.log_limit;
        let search = self.graph_search.clone().filter(|s| !s.trim().is_empty());
        let file = self.graph_file.as_ref().map(|(f, _)| f.clone());
        let task = cx.background_executor().spawn(async move {
            let mut names = None;
            let commits = match (&file, &search) {
                (Some(f), _) => git::log_file(&root, f, limit).map(|(c, n)| {
                    names = Some(n);
                    c
                }),
                (None, Some(q)) => git::search_log(&root, q.trim(), 300),
                (None, None) => git::log(&root, limit, all, &only),
            };
            let rows = commits.as_ref().map(|c| graph::layout(c, theme::lane_colors().len())).unwrap_or_default();
            (commits, rows, git::repo_refs(&root), names)
        });
        self.log_task = Some(cx.spawn(async move |this, cx| {
            let (commits, rows, refs, names) = task.await;
            if std::env::var_os("OSKL_DEBUG").is_some() {
                eprintln!("[oskl] log: {:?}", commits.as_ref().map(|c| c.len()));
            }
            this.update(cx, |repo, cx| {
                repo.log_running = false;
                if *repo.refs != refs {
                    repo.refs = Arc::new(refs);
                }
                if let (Some((_, n)), Some(names)) = (&mut repo.graph_file, names) {
                    *n = Arc::new(names);
                }
                match commits {
                    Ok(commits) => {
                        repo.commits = Arc::new(commits);
                        repo.graph = Arc::new(rows);
                    }
                    Err(e) => cx.emit(RepoEvent::Error(format!("git log failed: {e}"))),
                }
                if std::mem::take(&mut repo.log_again) {
                    repo.refresh_log(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn refresh_files(&mut self, cx: &mut Context<Self>) {
        if self.files_running {
            self.files_again = true;
            return;
        }
        self.files_running = true;
        let root = self.root.clone();
        let is_git = self.is_git;
        let task = cx.background_executor().spawn(async move {
            let mut files = if is_git { git::list_files(&root) } else { walk_files(&root, 100_000) };
            // Env files are usually git-ignored (and hidden), but you still want to find them.
            files.extend(env_files(&root));
            files.sort();
            files.dedup();
            files
        });
        self.files_task = Some(cx.spawn(async move |this, cx| {
            let files = task.await;
            this.update(cx, |repo, cx| {
                repo.files_running = false;
                repo.files = Arc::new(files);
                if std::mem::take(&mut repo.files_again) {
                    repo.refresh_files(cx);
                }
            })
            .ok();
        }));
    }

    /// A list showed rows up to `shown`: near the end of a full page, read the next page.
    pub fn want_more(&mut self, shown: usize, cx: &mut Context<Self>) {
        let n = self.commits.len();
        if n >= self.log_limit && shown + 40 >= n && !self.log_running {
            self.log_limit += HISTORY_PAGE;
            self.refresh_log(cx);
        }
    }

    /// The folder's name, as shown in pickers.
    pub fn name(&self) -> String {
        self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| self.root.to_string_lossy().to_string())
    }

    pub fn status_of(&self, rel: &str) -> Option<FileStatus> {
        self.file_status.get(rel).copied()
    }

    pub fn dir_is_dirty(&self, rel: &str) -> bool {
        self.dirty_dirs.contains(rel)
    }

    pub fn is_ignored(&self, rel: &str) -> bool {
        self.status.ignored.iter().any(|ig| rel == ig || rel.starts_with(&format!("{ig}/")))
    }

    pub fn branch_label(&self) -> String {
        if !self.is_git {
            return String::new();
        }
        match (&self.status.branch, &self.status.head_oid) {
            (Some(b), _) => b.clone(),
            (None, Some(oid)) if oid.len() >= 7 => format!("({})", &oid[..7]),
            _ => "HEAD".into(),
        }
    }

    /// Runs a git operation in the background, then refreshes everything.
    pub fn run_op(&mut self, label: &str, cx: &mut Context<Self>, op: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static) {
        self.run_op_as(undo::kind_of_label(label), label, cx, op)
    }

    /// `run_op` for an action Undo covers, when its name doesn't say which.
    pub fn run_op_as(&mut self, kind: Option<undo::Kind>, label: &str, cx: &mut Context<Self>, op: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static) {
        let root = self.root.clone();
        self.busy = Some(label.to_string());
        cx.notify();
        let label = label.to_string();
        let task = cx.background_executor().spawn(async move { undo::track(&root, kind, || op(&root), |r| r.is_ok()) });
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let (result, undo) = task.await;
            this.update(cx, |repo, cx| {
                repo.busy = None;
                match result {
                    Ok(msg) => {
                        let msg = msg.trim();
                        if !repo.remember(undo, cx) {
                            cx.emit(RepoEvent::Message(if msg.is_empty() { format!("{label}: done") } else { msg.lines().last().unwrap_or("").to_string() }));
                        }
                    }
                    Err(e) => cx.emit(RepoEvent::Error(format!("{label} failed: {e}"))),
                }
                repo.refresh_status(cx);
                repo.refresh_log(cx);
            })
            .ok();
        }));
    }

    /// Runs `git <args>` and reports the full output.
    pub fn run_git(&mut self, args: Vec<String>, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let title = format!("git {}", args.join(" "));
        self.busy = Some(title.clone());
        cx.notify();
        let kind = undo::kind_of_args(&args);
        let task = cx.background_executor().spawn(async move { undo::track(&root, kind, || git::run_user(&root, &args), |r| r.0) });
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let ((ok, text), undo) = task.await;
            this.update(cx, |repo, cx| {
                repo.busy = None;
                let rejected = !ok && title.starts_with("git push") && (text.contains("rejected") || text.contains("fetch first") || text.contains("non-fast-forward"));
                cx.emit(RepoEvent::Output { title, text, ok });
                repo.remember(undo, cx);
                if rejected {
                    cx.emit(RepoEvent::PushRejected);
                }
                repo.refresh_status(cx);
                repo.refresh_log(cx);
            })
            .ok();
        }));
    }

    /// Keeps `undo` as the last action's (when there is one) and tells the views.
    pub fn remember(&mut self, undo: Option<Undo>, cx: &mut Context<Self>) -> bool {
        let Some(u) = undo else { return false };
        cx.emit(RepoEvent::Undoable(u.label.clone()));
        self.undo = Some(u);
        true
    }

    /// What Undo would do, and whether it replaces files that have uncommitted changes (so it must ask).
    pub fn undo_state(&self) -> Option<(String, bool)> {
        let u = self.undo.as_ref()?;
        let changed = self.status.staged.iter().chain(&self.status.unstaged).chain(&self.status.conflicts).any(|c| c.status != FileStatus::Untracked);
        Some((u.label.clone(), u.replaces_files() && changed))
    }

    /// Takes back the last action. `stash_first`: put uncommitted changes in a stash before that.
    pub fn undo_last(&mut self, stash_first: bool, cx: &mut Context<Self>) {
        let Some(u) = self.undo.clone() else { return cx.emit(RepoEvent::Message("Nothing to undo yet: commit, discard or stash something first".into())) };
        let root = self.root.clone();
        self.busy = Some("Undo".into());
        cx.notify();
        let task = cx.background_executor().spawn(async move { undo::apply(&root, &u, stash_first) });
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |repo, cx| {
                repo.busy = None;
                match result {
                    // Undone once: the record is spent, and the old action can't be undone twice.
                    Ok(msg) => {
                        repo.undo = None;
                        cx.emit(RepoEvent::Message(msg));
                    }
                    Err(e) => cx.emit(RepoEvent::Error(format!("Undo failed: {e}"))),
                }
                repo.refresh_status(cx);
                repo.refresh_log(cx);
            })
            .ok();
        }));
    }
}

/// `.env`, `.env.local`, `prod.env`… a few folders deep (not in dependencies or build output).
pub fn env_files(root: &Path) -> Vec<String> {
    const SKIP: &[&str] = &["node_modules", "target", "build", "dist", "Pods", "vendor", "DerivedData"];
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    let mut seen = 0;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            seen += 1;
            if seen > 30_000 {
                return out;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < 4 && !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                    stack.push((entry.path(), depth + 1));
                }
            } else if (crate::secrets::is_env_file(&name) || name.starts_with(".env"))
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
    out
}

/// The files of a folder that isn't a repository itself. A repository inside it (a folder of
/// several projects) lists its own files, as git does: what it ignores (build output, coverage,
/// dependencies) isn't listed.
pub fn walk_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir != root && dir.join(".git").exists() {
            let rel = dir.strip_prefix(root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
            out.extend(git::list_files(&dir).into_iter().map(|f| format!("{rel}/{f}")));
            if out.len() >= limit {
                out.truncate(limit);
                return out;
            }
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().to_string());
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

/// Git repositories inside `root` (not `root` itself), a few levels down: a folder holding
/// several projects, or a monorepo with nested checkouts. Skips dependencies and build output.
pub fn find_repos(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    const SKIP: &[&str] = &["node_modules", "target", "build", "dist", "Pods", "vendor", "DerivedData"];
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    let mut seen = 0;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            if path.join(".git").exists() {
                out.push(path.clone());
            }
            if depth + 1 < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_env_files_but_not_in_dependencies() {
        let d = std::env::temp_dir().join(format!("oskl-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for f in [".env", ".env.example", "apps/api/.env.local", "node_modules/x/.env", ".git/.env", "src/a.ts"] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let mut found = super::env_files(&d);
        found.sort();
        assert_eq!(found, vec![".env", ".env.example", "apps/api/.env.local"]);
        std::fs::remove_dir_all(&d).ok();
    }
}
