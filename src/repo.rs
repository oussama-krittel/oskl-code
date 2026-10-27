//! Shared git state for the open project. Views observe it and re-render
//! when status or history changes.

use crate::git::{self, FileStatus};
use crate::graph::{self, GraphRow};
use crate::theme;
use gpui::prelude::*;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub enum RepoEvent {
    Message(String),
    Error(String),
    Output { title: String, text: String, ok: bool },
    /// The remote has commits we don't: offer pull-then-push.
    PushRejected,
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
    pub user_email: Option<String>,
    pub files: Arc<Vec<String>>,
    file_status: HashMap<String, FileStatus>,
    dirty_dirs: HashSet<String>,
    pub busy: Option<String>,
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
        if std::env::var_os("KITE_DEBUG").is_some() {
            eprintln!("[kite] open {} -> git root {:?}", root.display(), git_root);
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
        let mut repo = Self {
            root,
            is_git,
            status: Arc::default(),
            commits: Arc::default(),
            graph: Arc::default(),
            graph_all: false,
            graph_only: Vec::new(),
            user_email,
            files: Arc::default(),
            file_status: HashMap::new(),
            dirty_dirs: HashSet::new(),
            busy: None,
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
            if std::env::var_os("KITE_DEBUG").is_some() {
                eprintln!("[kite] status: {:?}", status.as_ref().map(|s| (s.branch.clone(), s.staged.len(), s.unstaged.len())));
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
        let task = cx.background_executor().spawn(async move {
            let commits = git::log(&root, 800, all, &only);
            let rows = commits.as_ref().map(|c| graph::layout(c, theme::lane_colors().len())).unwrap_or_default();
            (commits, rows)
        });
        self.log_task = Some(cx.spawn(async move |this, cx| {
            let (commits, rows) = task.await;
            if std::env::var_os("KITE_DEBUG").is_some() {
                eprintln!("[kite] log: {:?}", commits.as_ref().map(|c| c.len()));
            }
            this.update(cx, |repo, cx| {
                repo.log_running = false;
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
            if is_git { git::list_files(&root) } else { walk_files(&root, 20_000) }
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
    pub fn run_op(
        &mut self,
        label: &str,
        cx: &mut Context<Self>,
        op: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static,
    ) {
        let root = self.root.clone();
        self.busy = Some(label.to_string());
        cx.notify();
        let label = label.to_string();
        let task = cx.background_executor().spawn(async move { op(&root) });
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |repo, cx| {
                repo.busy = None;
                match result {
                    Ok(msg) => {
                        let msg = msg.trim();
                        cx.emit(RepoEvent::Message(if msg.is_empty() { format!("{label}: done") } else { msg.lines().last().unwrap_or("").to_string() }));
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
        let task = cx.background_executor().spawn(async move { git::run_user(&root, &args) });
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let (ok, text) = task.await;
            this.update(cx, |repo, cx| {
                repo.busy = None;
                let rejected = !ok && title.starts_with("git push") && (text.contains("rejected") || text.contains("fetch first") || text.contains("non-fast-forward"));
                cx.emit(RepoEvent::Output { title, text, ok });
                if rejected {
                    cx.emit(RepoEvent::PushRejected);
                }
                repo.refresh_status(cx);
                repo.refresh_log(cx);
            })
            .ok();
        }));
    }
}

fn walk_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
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
