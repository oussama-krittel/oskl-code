//! Requests that panels send up to the workspace.

use crate::git::Rev;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct DiffSpec {
    pub path: String,
    pub old_path: Option<String>,
    pub left: Rev,
    pub right: Rev,
    /// The page this diff was opened from (shown as a link back).
    pub back: Option<Back>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Back {
    Commit(String),
    /// A branch comparison: (base, compared).
    Compare(String, String),
}

impl DiffSpec {
    pub fn key(&self) -> String {
        format!("{}|{:?}|{:?}", self.path, self.left, self.right)
    }
}

pub enum Request {
    OpenFile { path: PathBuf, line: Option<usize>, preview: bool },
    /// Show a file as a preview but leave the keyboard where it is (explorer single click).
    ShowFile(PathBuf),
    /// Open a file and select char columns `cols` on `line` (0-based).
    OpenAt { path: PathBuf, line: usize, cols: std::ops::Range<usize> },
    OpenDiff { spec: DiffSpec, preview: bool },
    OpenCommit(String),
    /// What `other` changed since it split from `base` (the current branch when None).
    Compare { base: Option<String>, other: String },
    /// A conflicted file (repo-relative) was resolved in the merge editor: stage it.
    MarkResolved(String),
    /// Opens the command line pre-filled with this text.
    CommandLine(String),
    /// A file or folder was renamed/moved: open tabs follow it.
    Moved { from: PathBuf, to: PathBuf },
    /// Files or folders were moved to the Trash: their tabs close.
    Deleted(Vec<PathBuf>),
    /// Ask in the in-app dialog (themed, not the macOS alert).
    Ask(crate::dialog::Ask),
    /// A terminal in this folder.
    OpenTerminal(PathBuf),
    /// Show this repository in the Source Control panel.
    SelectRepo(PathBuf),
    /// Pick a folder to add to the workspace.
    AddFolder,
    /// Take this added folder out of the workspace.
    RemoveFolder(PathBuf),
    Message(String),
    Error(String),
}
