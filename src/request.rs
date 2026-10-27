//! Requests that panels send up to the workspace.

use crate::git::Rev;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct DiffSpec {
    pub path: String,
    pub old_path: Option<String>,
    pub left: Rev,
    pub right: Rev,
    /// The commit page this diff was opened from (shown as a link back).
    pub from_commit: Option<String>,
}

impl DiffSpec {
    pub fn key(&self) -> String {
        format!("{}|{:?}|{:?}", self.path, self.left, self.right)
    }
}

pub enum Request {
    OpenFile { path: PathBuf, line: Option<usize>, preview: bool },
    /// Open a file and select char columns `cols` on `line` (0-based).
    OpenAt { path: PathBuf, line: usize, cols: std::ops::Range<usize> },
    OpenDiff { spec: DiffSpec, preview: bool },
    OpenCommit(String),
    /// Compare the current branch with this one.
    Compare(String),
    /// A conflicted file (repo-relative) was resolved in the merge editor: stage it.
    MarkResolved(String),
    /// Opens the command line pre-filled with this text.
    CommandLine(String),
    /// A file or folder was renamed/moved: open tabs follow it.
    Moved { from: PathBuf, to: PathBuf },
    /// Files or folders were moved to the Trash: their tabs close.
    Deleted(Vec<PathBuf>),
    /// A terminal in this folder.
    OpenTerminal(PathBuf),
    Message(String),
    Error(String),
}
