//! Git backend. Everything goes through the `git` CLI so hooks, signing,
//! credentials and config behave exactly like they do in a terminal.

use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

#[path = "gh.rs"]
pub mod gh;
#[path = "git_draft.rs"]
pub mod draft;
#[path = "git_undo.rs"]
pub mod undo;

pub const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

fn git(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Git Credential Manager would wait for a sign-in window nobody sees.
        .env("GCM_INTERACTIVE", "never")
        .args(["-c", "core.quotepath=off", "-c", "color.ui=false"])
        .stdin(Stdio::null());
    // Hooks (husky, lint-staged, pre-commit…) need node, npx, python from the user's shell;
    // from the Dock the app's own PATH has none of them.
    if let Some(path) = crate::tools::shell_path_ready() {
        cmd.env("PATH", path);
    }
    cmd
}

pub fn run(root: &Path, args: &[&str]) -> Result<String> {
    let out = git(root).args(args).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn run_with_stdin(root: &Path, args: &[&str], input: &str) -> Result<String> {
    let mut child = git(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let input = input.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A plain `git push` (no remote or branch named) of a branch that tracks nothing yet: push it
/// to its remote and track it there (`-u`), as the terminal suggests, instead of failing.
/// The remote is the branch's pushRemote, else remote.pushDefault, else origin, else the only one.
pub fn push_args(root: &Path, args: &[String]) -> Vec<String> {
    let plain = args.first().is_some_and(|a| a == "push") && args[1..].iter().all(|a| a.starts_with('-'));
    if !plain || run(root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok() {
        return args.to_vec();
    }
    let Ok(branch) = run(root, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|b| b.trim().to_string()) else { return args.to_vec() };
    let remotes = remotes(root);
    let config = |key: String| run(root, &["config", "--get", &key]).ok().map(|v| v.trim().to_string()).filter(|v| remotes.contains(v));
    let remote = config(format!("branch.{branch}.pushRemote"))
        .or_else(|| config("remote.pushDefault".into()))
        .or_else(|| remotes.iter().find(|r| *r == "origin").cloned())
        .or_else(|| (remotes.len() == 1).then(|| remotes[0].clone()));
    let Some(remote) = remote.filter(|_| !branch.is_empty()) else { return args.to_vec() };
    let mut out = args.to_vec();
    out.extend(["-u".to_string(), remote, branch]);
    out
}

/// Runs git with arbitrary user args, returning combined output and success.
pub fn run_user(root: &Path, args: &[String]) -> (bool, String) {
    let args = push_args(root, args);
    match git(root).args(&args).output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), text)
        }
        Err(e) => (false, e.to_string()),
    }
}

pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|s| PathBuf::from(s.trim()))
}

pub fn user_email(root: &Path) -> Option<String> {
    run(root, &["config", "user.email"]).ok().map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------- status

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl FileStatus {
    fn from_code(c: char) -> Option<Self> {
        Some(match c {
            'M' => Self::Modified,
            'A' => Self::Added,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            'T' => Self::TypeChanged,
            'U' => Self::Conflicted,
            _ => return None,
        })
    }

    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: FileStatus,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub branch: Option<String>,
    pub head_oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<Change>,
    pub unstaged: Vec<Change>,
    pub conflicts: Vec<Change>,
    pub ignored: Vec<String>,
    /// A merge / rebase / cherry-pick / revert waiting to be finished.
    pub op: Option<OpState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpState {
    pub kind: OpKind,
    /// "Merging feature", "Rebasing main (2/5)"…
    pub label: String,
    /// The commit message git prepared (MERGE_MSG, or the rebased commit's message).
    pub message: String,
}

impl OpKind {
    pub fn continue_label(self) -> &'static str {
        match self {
            OpKind::Merge => "Commit Merge",
            OpKind::Rebase => "Continue Rebase",
            OpKind::CherryPick => "Continue Cherry-pick",
            OpKind::Revert => "Continue Revert",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            OpKind::Merge => "merge",
            OpKind::Rebase => "rebase",
            OpKind::CherryPick => "cherry-pick",
            OpKind::Revert => "revert",
        }
    }

    pub fn can_skip(self) -> bool {
        matches!(self, OpKind::Rebase | OpKind::CherryPick | OpKind::Revert)
    }
}

/// The repository's .git directory (follows `.git` files of worktrees and submodules).
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let p = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    Some(if p.is_absolute() { p } else { root.join(p) })
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// Message file contents without git's "#" comment lines.
fn clean_message(text: &str) -> String {
    text.lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n").trim().to_string()
}

pub fn op_state(root: &Path) -> Option<OpState> {
    let dir = git_dir(root)?;
    let short = |r: String| r.trim_start_matches("refs/heads/").to_string();
    for sub in ["rebase-merge", "rebase-apply"] {
        let d = dir.join(sub);
        if d.is_dir() {
            let (n, total) = if sub == "rebase-merge" { ("msgnum", "end") } else { ("next", "last") };
            let step = match (read_trim(&d.join(n)), read_trim(&d.join(total))) {
                (Some(a), Some(b)) => format!(" ({a}/{b})"),
                _ => String::new(),
            };
            let onto = read_trim(&d.join("onto")).map(|o| o[..o.len().min(7)].to_string());
            let branch = read_trim(&d.join("head-name")).map(short).unwrap_or_else(|| "HEAD".into());
            let message = read_trim(&d.join("message")).or_else(|| read_trim(&dir.join("MERGE_MSG"))).map(|m| clean_message(&m)).unwrap_or_default();
            let label = match onto {
                Some(o) => format!("Rebasing {branch} onto {o}{step}"),
                None => format!("Rebasing {branch}{step}"),
            };
            return Some(OpState { kind: OpKind::Rebase, label, message });
        }
    }
    let msg = || read_trim(&dir.join("MERGE_MSG")).map(|m| clean_message(&m)).unwrap_or_default();
    if dir.join("MERGE_HEAD").exists() {
        // "Merge branch 'feature' into main" → feature
        let m = msg();
        let what = m.split('\'').nth(1).map(String::from).unwrap_or_else(|| read_trim(&dir.join("MERGE_HEAD")).map(|s| s[..s.len().min(7)].to_string()).unwrap_or_default());
        return Some(OpState { kind: OpKind::Merge, label: format!("Merging {what}"), message: m });
    }
    for (file, kind, verb) in [("CHERRY_PICK_HEAD", OpKind::CherryPick, "Cherry-picking"), ("REVERT_HEAD", OpKind::Revert, "Reverting")] {
        if let Some(sha) = read_trim(&dir.join(file)) {
            return Some(OpState { kind, label: format!("{verb} {}", &sha[..sha.len().min(7)]), message: msg() });
        }
    }
    None
}

/// Finishes the operation with `message` (no editor pops up).
pub fn continue_op(root: &Path, kind: OpKind, message: &str) -> Result<String> {
    let dir = git_dir(root).ok_or_else(|| anyhow!("no .git directory"))?;
    match kind {
        OpKind::Merge => return run_with_stdin(root, &["commit", "-F", "-"], message),
        OpKind::Rebase => {
            if !message.is_empty() {
                let f = dir.join("rebase-merge/message");
                if f.exists() {
                    std::fs::write(f, format!("{message}\n"))?;
                }
            }
        }
        _ => {
            if !message.is_empty() {
                std::fs::write(dir.join("MERGE_MSG"), format!("{message}\n"))?;
            }
        }
    }
    let out = git(root).env("GIT_EDITOR", "true").args([kind.verb(), "--continue"]).output()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("{}", text.trim());
    }
    Ok(text)
}

pub fn abort_op(root: &Path, kind: OpKind) -> Result<String> {
    run(root, &[kind.verb(), "--abort"])
}

pub fn skip_op(root: &Path, kind: OpKind) -> Result<String> {
    let out = git(root).env("GIT_EDITOR", "true").args([kind.verb(), "--skip"]).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pulls (rebase or merge), then pushes: what to do after a push was rejected.
pub fn sync(root: &Path, rebase: bool) -> Result<String> {
    run(root, &["pull", if rebase { "--rebase" } else { "--no-rebase" }, "--no-edit"])?;
    let args: Vec<String> = push_args(root, &["push".to_string()]);
    run(root, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Two branches side by side: what `other` changed since it split from `base`, and how far
/// apart they are.
pub struct Comparison {
    /// Commit-like summary of `other`'s changes (the "parent" is the merge base).
    pub details: CommitDetails,
    /// Commits in `other` that `base` doesn't have.
    pub ahead: usize,
    /// Commits in `base` that `other` doesn't have.
    pub behind: usize,
}

pub fn compare(root: &Path, base: &str, other: &str) -> Result<Comparison> {
    let mb = run(root, &["merge-base", base, other])?.trim().to_string();
    let tip = run(root, &["rev-parse", other])?.trim().to_string();
    let files = run(root, &["diff", "-z", "-M", "--name-status", &mb, &tip])?;
    let count = |range: String| run(root, &["rev-list", "--count", &range]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let (ahead, behind) = (count(format!("{mb}..{tip}")), count(format!("{tip}..{base}")));
    let details = CommitDetails {
        sha: tip,
        parents: vec![mb],
        author: String::new(),
        email: String::new(),
        date: String::new(),
        time: 0,
        committer: String::new(),
        signature: 'N',
        refs: Vec::new(),
        message: format!("{other} compared with {base}"),
        files: parse_name_status(&files),
    };
    Ok(Comparison { details, ahead, behind })
}

impl Status {
    pub fn is_initial(&self) -> bool {
        self.head_oid.as_deref() == Some("(initial)")
    }

    pub fn change_count(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.conflicts.len()
    }
}

pub fn status(root: &Path) -> Result<Status> {
    let out = run(
        root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    let mut st = parse_status(&out);
    st.op = op_state(root);
    Ok(st)
}

fn parse_status(out: &str) -> Status {
    let mut st = Status::default();
    let mut records = out.split('\0');
    while let Some(rec) = records.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(header) = rec.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => st.head_oid = Some(value.to_string()),
                "branch.head" if value != "(detached)" => st.branch = Some(value.to_string()),
                "branch.upstream" => st.upstream = Some(value.to_string()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            st.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            st.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = rec.as_bytes()[0];
        match kind {
            b'1' | b'2' => {
                let field_count = if kind == b'1' { 9 } else { 10 };
                let fields: Vec<&str> = rec.splitn(field_count, ' ').collect();
                if fields.len() < field_count {
                    continue;
                }
                let xy: Vec<char> = fields[1].chars().collect();
                let path = fields[field_count - 1].to_string();
                let orig_path = if kind == b'2' {
                    records.next().map(|s| s.to_string())
                } else {
                    None
                };
                if let Some(s) = xy.first().and_then(|c| FileStatus::from_code(*c)) {
                    st.staged.push(Change { path: path.clone(), orig_path: orig_path.clone(), status: s });
                }
                if let Some(s) = xy.get(1).and_then(|c| FileStatus::from_code(*c)) {
                    st.unstaged.push(Change { path, orig_path: None, status: s });
                }
            }
            b'u' => {
                let fields: Vec<&str> = rec.splitn(11, ' ').collect();
                if let Some(path) = fields.get(10) {
                    st.conflicts.push(Change {
                        path: path.to_string(),
                        orig_path: None,
                        status: FileStatus::Conflicted,
                    });
                }
            }
            b'?' => st.unstaged.push(Change {
                path: rec[2..].to_string(),
                orig_path: None,
                status: FileStatus::Untracked,
            }),
            b'!' => st.ignored.push(rec[2..].trim_end_matches('/').to_string()),
            _ => {}
        }
    }
    let by_path = |a: &Change, b: &Change| a.path.cmp(&b.path);
    st.staged.sort_by(by_path);
    st.unstaged.sort_by(by_path);
    st
}

// ---------------------------------------------------------------- log

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug)]
pub struct Ref {
    pub name: String,
    pub kind: RefKind,
    pub is_head: bool,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub time: i64,
    pub refs: Vec<Ref>,
    pub subject: String,
    /// The message after its first line (shown when hovering the commit in the graph).
    pub body: String,
}

impl Commit {
    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }
}

pub fn remotes(root: &Path) -> Vec<String> {
    run(root, &["remote"])
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn parse_refs(decoration: &str, remotes: &[String]) -> Vec<Ref> {
    let mut refs = Vec::new();
    for part in decoration.split(", ").filter(|p| !p.is_empty()) {
        if let Some(branch) = part.strip_prefix("HEAD -> ") {
            refs.push(Ref { name: branch.to_string(), kind: RefKind::Local, is_head: true });
        } else if let Some(tag) = part.strip_prefix("tag: ") {
            refs.push(Ref { name: tag.to_string(), kind: RefKind::Tag, is_head: false });
        } else if part == "HEAD" || part.ends_with("/HEAD") {
            continue;
        } else if remotes.iter().any(|r| part.starts_with(&format!("{r}/"))) {
            refs.push(Ref { name: part.to_string(), kind: RefKind::Remote, is_head: false });
        } else {
            refs.push(Ref { name: part.to_string(), kind: RefKind::Local, is_head: false });
        }
    }
    // Local branches first, like VS Code.
    refs.sort_by_key(|r| match r.kind {
        RefKind::Local => 0,
        RefKind::Remote => 1,
        RefKind::Tag => 2,
    });
    refs
}

const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1f%b%x1e";

/// History for the graph. `all = false` mirrors VS Code's "Auto" mode:
/// the current branch, its upstream and the remote's default branch.
pub fn log(root: &Path, limit: usize, all: bool, only: &[String]) -> Result<Vec<Commit>> {
    let remotes = remotes(root);
    let limit_arg = format!("-n{limit}");
    let mut args: Vec<String> = vec!["log".into(), "--topo-order".into(), limit_arg, LOG_FORMAT.into()];
    if !only.is_empty() {
        // An explicit pick of branches from the graph's filter.
        args.extend(only.iter().cloned());
    } else if all {
        // Stashes are listed on their own, not as commits of the history.
        args.extend(["--exclude=refs/stash".into(), "--all".into()]);
    } else {
        if run(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
            return Ok(Vec::new());
        }
        args.push("HEAD".into());
        if let Ok(up) = run(root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]) {
            args.push(up.trim().to_string());
        }
        for remote in &remotes {
            if let Ok(base) = run(root, &["rev-parse", "--abbrev-ref", &format!("{remote}/HEAD")]) {
                let base = base.trim().to_string();
                if !base.is_empty() && !args.contains(&base) {
                    args.push(base);
                }
                break;
            }
        }
    }
    args.push("--".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut commits = parse_log(&run(root, &arg_refs)?, &remotes);
    if !only.is_empty() {
        // Branches picked: only theirs are labelled (and a picked branch's remote twin, its
        // cloud), not every other branch that happens to point at a shared commit. Tags stay:
        // they're on the picked history.
        let picked = |r: &Ref| match r.kind {
            RefKind::Tag => true,
            RefKind::Local => only.contains(&r.name),
            RefKind::Remote => only.contains(&r.name) || r.name.split_once('/').is_some_and(|(_, n)| only.iter().any(|o| o == n)),
        };
        for c in &mut commits {
            c.refs.retain(picked);
        }
    }
    Ok(commits)
}

fn parse_log(out: &str, remotes: &[String]) -> Vec<Commit> {
    let mut commits = Vec::new();
    for rec in out.split('\x1e') {
        let rec = rec.trim_start_matches('\n');
        if rec.is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.splitn(8, '\x1f').collect();
        if f.len() < 8 {
            continue;
        }
        commits.push(Commit {
            sha: f[0].to_string(),
            parents: f[1].split(' ').filter(|p| !p.is_empty()).map(String::from).collect(),
            author: f[2].to_string(),
            time: f[4].parse().unwrap_or(0),
            refs: parse_refs(f[5], remotes),
            subject: f[6].to_string(),
            body: f[7].trim().to_string(),
        });
    }
    commits
}

// ---------------------------------------------------------------- commit details

#[derive(Clone, Debug)]
pub struct CommitDetails {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub date: String,
    pub time: i64,
    pub committer: String,
    /// `%G?`: G good, U good but untrusted, B bad, E can't be checked, N none (and X, Y, R expired or revoked).
    pub signature: char,
    pub refs: Vec<Ref>,
    pub message: String,
    pub files: Vec<Change>,
}

impl CommitDetails {
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }

    pub fn body(&self) -> &str {
        self.message
            .split_once('\n')
            .map(|(_, rest)| rest.trim())
            .unwrap_or("")
    }
}

pub fn commit_details(root: &Path, sha: &str) -> Result<CommitDetails> {
    let out = run(
        root,
        &[
            "show",
            "-s",
            "--date=format-local:%a %b %d %Y, %H:%M",
            "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%ad%x1f%at%x1f%cn%x1f%G?%x1f%D%x1f%B",
            sha,
            "--",
        ],
    )?;
    let f: Vec<&str> = out.splitn(10, '\x1f').collect();
    if f.len() < 10 {
        bail!("unexpected git show output");
    }
    let parents: Vec<String> = f[1].split(' ').filter(|p| !p.is_empty()).map(String::from).collect();
    let mut args = vec!["diff-tree", "-r", "-z", "-M", "--name-status", "--no-commit-id"];
    if parents.is_empty() {
        args.push("--root");
    } else {
        args.push(&parents[0]);
    }
    args.push(f[0]);
    let files_out = run(root, &args)?;
    Ok(CommitDetails {
        sha: f[0].to_string(),
        parents,
        author: f[2].to_string(),
        email: f[3].to_string(),
        date: f[4].to_string(),
        time: f[5].parse().unwrap_or(0),
        committer: f[6].to_string(),
        signature: f[7].chars().next().unwrap_or('N'),
        refs: parse_refs(f[8], &remotes(root)),
        message: f[9].trim_end().to_string(),
        files: parse_name_status(&files_out),
    })
}

pub(crate) fn parse_name_status(out: &str) -> Vec<Change> {
    let mut files = Vec::new();
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = it.next() {
        let Some(status) = code.chars().next().and_then(FileStatus::from_code) else {
            continue;
        };
        if matches!(status, FileStatus::Renamed | FileStatus::Copied) {
            let (Some(old), Some(new)) = (it.next(), it.next()) else { break };
            files.push(Change { path: new.to_string(), orig_path: Some(old.to_string()), status });
        } else if let Some(path) = it.next() {
            files.push(Change { path: path.to_string(), orig_path: None, status });
        }
    }
    files
}

// ---------------------------------------------------------------- file contents

/// Revision a side of a diff is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rev {
    Commit(String),
    Index,
    Working,
    Empty,
    /// A merge stage: 2 = current (ours), 3 = incoming (theirs).
    Stage(u8),
}

impl Rev {
    pub fn label(&self) -> String {
        match self {
            Rev::Commit(sha) => sha[..sha.len().min(7)].to_string(),
            Rev::Index => "Index".into(),
            Rev::Working => "Working Tree".into(),
            Rev::Empty => "Empty".into(),
            Rev::Stage(2) => "Current (ours)".into(),
            Rev::Stage(3) => "Incoming (theirs)".into(),
            Rev::Stage(n) => format!("Stage {n}"),
        }
    }
}

pub fn read_rev(root: &Path, rev: &Rev, path: &str) -> Option<String> {
    let bytes = match rev {
        Rev::Empty => return Some(String::new()),
        Rev::Working => std::fs::read(root.join(path)).ok()?,
        Rev::Index => git(root).args(["show", &format!(":{path}")]).output().ok().filter(|o| o.status.success())?.stdout,
        Rev::Stage(n) => git(root).args(["show", &format!(":{n}:{path}")]).output().ok().filter(|o| o.status.success())?.stdout,
        Rev::Commit(sha) => git(root).args(["show", &format!("{sha}:{path}")]).output().ok().filter(|o| o.status.success())?.stdout,
    };
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

/// A file as it is in a revision, on disk (to show pictures, PDFs and documents): the file
/// itself for the working tree, otherwise a copy kept under the temp folder, named by its
/// content so each version is written once.
pub fn rev_file(root: &Path, rev: &Rev, path: &str) -> Option<PathBuf> {
    let spec = match rev {
        Rev::Empty => return None,
        Rev::Working => return Some(root.join(path)).filter(|p| p.is_file()),
        Rev::Index => format!(":{path}"),
        Rev::Stage(n) => format!(":{n}:{path}"),
        Rev::Commit(sha) => format!("{sha}:{path}"),
    };
    let out = git(root).args(["rev-parse", &spec]).output().ok().filter(|o| o.status.success())?;
    let blob = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let name = Path::new(path).file_name()?;
    let file = std::env::temp_dir().join("oskl-rev").join(&blob).join(name);
    if !file.is_file() {
        let bytes = git(root).args(["cat-file", "blob", &blob]).output().ok().filter(|o| o.status.success())?.stdout;
        std::fs::create_dir_all(file.parent()?).ok()?;
        std::fs::write(&file, bytes).ok()?;
    }
    Some(file)
}

// ---------------------------------------------------------------- blame

#[derive(Debug)]
pub struct BlameCommit {
    pub sha: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub summary: String,
}

impl BlameCommit {
    pub fn is_uncommitted(&self) -> bool {
        self.sha == UNCOMMITTED
    }
}

#[derive(Debug, Default)]
pub struct Blame {
    pub lines: Vec<Option<Arc<BlameCommit>>>,
}

/// Blame `path`. With `rev`, blames that revision; with `contents`, blames
/// that text as if it were the working copy (so unsaved edits line up).
pub fn blame(root: &Path, path: &str, rev: Option<&str>, contents: Option<&str>) -> Result<Blame> {
    let mut args = vec!["blame", "--porcelain"];
    if contents.is_some() {
        args.extend(["--contents", "-"]);
    }
    if let Some(rev) = rev {
        args.push(rev);
    }
    args.extend(["--", path]);
    let out = match contents {
        Some(text) => run_with_stdin(root, &args, text)?,
        None => run(root, &args)?,
    };
    Ok(parse_blame(&out))
}

fn parse_blame(out: &str) -> Blame {
    #[derive(Default)]
    struct Partial {
        author: String,
        email: String,
        time: i64,
        summary: String,
    }
    let mut partial: HashMap<String, Partial> = HashMap::new();
    let mut done: HashMap<String, Arc<BlameCommit>> = HashMap::new();
    let mut entries: Vec<(usize, String)> = Vec::new();
    let mut current: Option<String> = None;

    for line in out.lines() {
        if line.starts_with('\t') {
            continue;
        }
        let mut words = line.split(' ');
        let first = words.next().unwrap_or("");
        if first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) {
            let _orig = words.next();
            if let Some(final_line) = words.next().and_then(|n| n.parse::<usize>().ok()) {
                entries.push((final_line.saturating_sub(1), first.to_string()));
            }
            partial.entry(first.to_string()).or_default();
            current = Some(first.to_string());
            continue;
        }
        let Some(sha) = current.as_ref() else { continue };
        let Some(p) = partial.get_mut(sha) else { continue };
        let value = line.split_once(' ').map(|(_, v)| v).unwrap_or("");
        match first {
            "author" => p.author = value.to_string(),
            "author-mail" => p.email = value.trim_matches(|c| c == '<' || c == '>').to_string(),
            "author-time" => p.time = value.parse().unwrap_or(0),
            "summary" => p.summary = value.to_string(),
            _ => {}
        }
    }

    let count = entries.iter().map(|(l, _)| l + 1).max().unwrap_or(0);
    let mut blame = Blame { lines: vec![None; count] };
    for (line, sha) in entries {
        let commit = done
            .entry(sha.clone())
            .or_insert_with(|| {
                let p = partial.remove(&sha).unwrap_or_default();
                Arc::new(BlameCommit { sha: sha.clone(), author: p.author, email: p.email, time: p.time, summary: p.summary })
            })
            .clone();
        blame.lines[line] = Some(commit);
    }
    blame
}

// ---------------------------------------------------------------- partial staging

/// `from` with `to`'s version of the changes at `rows`, and `from` everywhere else: what staging,
/// unstaging or discarding a hunk or some lines leaves. `rows` are lines of `to` when `rows_in_to`,
/// else of `from`. `whole`: a change `rows` touches is taken entirely; otherwise only its added lines
/// inside `rows` are (its removed lines always go).
pub fn mix(from: &str, to: &str, rows: std::ops::Range<usize>, rows_in_to: bool, whole: bool) -> String {
    let diff = similar::TextDiff::from_lines(from, to);
    let (old, new) = (diff.old_slices(), diff.new_slices());
    // An empty range (a pure deletion or insertion) is hit by the line it sits before.
    let hit = |r: &std::ops::Range<usize>| r.start < rows.end && rows.start < r.end.max(r.start + 1);
    let mut out = String::new();
    for op in diff.ops() {
        let (o, n) = (op.old_range(), op.new_range());
        let touched = op.tag() != similar::DiffTag::Equal && hit(if rows_in_to { &n } else { &o });
        if !touched {
            out.extend(old[o].iter().copied());
        } else if whole || !rows_in_to {
            out.extend(new[n].iter().copied());
        } else {
            out.extend(n.filter(|i| rows.contains(i)).map(|i| new[i]));
        }
    }
    out
}

/// Makes `text` the staged content of `path` (keeping its mode, and CRLF if the staged file has it).
pub fn write_index(root: &Path, path: &str, text: &str) -> Result<()> {
    let staged = run(root, &["ls-files", "-s", "--", path])?;
    let mode = staged.split_whitespace().next().map(String::from).unwrap_or_else(|| {
        use std::os::unix::fs::PermissionsExt;
        let exec = std::fs::metadata(root.join(path)).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
        if exec { "100755" } else { "100644" }.into()
    });
    let crlf = git(root).args(["show", &format!(":{path}")]).output().is_ok_and(|o| o.stdout.windows(2).any(|w| w == b"\r\n"));
    let text = if crlf { text.replace('\n', "\r\n") } else { text.to_string() };
    // shortcut: clean/smudge filters (LFS, autocrlf) are skipped; such files stage whole from the Changes list.
    let sha = run_with_stdin(root, &["hash-object", "-w", "--no-filters", "--stdin"], &text)?;
    run(root, &["update-index", "--add", "--cacheinfo", &format!("{mode},{},{path}", sha.trim())]).map(|_| ())
}

// ---------------------------------------------------------------- actions

pub fn stage(root: &Path, paths: &[String]) -> Result<()> {
    let mut args = vec!["add", "-A", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run(root, &args).map(|_| ())
}

pub fn unstage(root: &Path, paths: &[String], initial: bool) -> Result<()> {
    let mut args = if initial {
        vec!["rm", "--cached", "-r", "-q", "--"]
    } else {
        vec!["restore", "--staged", "--"]
    };
    args.extend(paths.iter().map(|s| s.as_str()));
    run(root, &args).map(|_| ())
}

/// Reverts tracked files to their index state. Untracked files are left alone.
pub fn discard(root: &Path, paths: &[String]) -> Result<()> {
    let mut args = vec!["restore", "--worktree", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run(root, &args).map(|_| ())
}

/// A commit git refused, with everything it and its hooks printed (stdout and stderr, colors
/// removed), so what a pre-commit hook found can be shown in full.
#[derive(Debug, Clone)]
pub struct CommitFailed {
    pub output: String,
    /// The repository runs commit hooks (husky, pre-commit…): most likely one of them said no.
    pub hook: bool,
}

impl std::fmt::Display for CommitFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.output.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("git commit failed").trim())
    }
}

impl std::error::Error for CommitFailed {}

/// Commits the index with `message`. An amend with no message keeps the last one.
/// Waits for the shell's PATH first, so hooks find their tools; a refusal is a `CommitFailed`.
pub fn commit(root: &Path, message: &str, amend: bool) -> Result<String> {
    crate::tools::init_shell_path();
    let mut args = vec!["commit"];
    if amend && message.is_empty() {
        args.extend(["--amend", "--no-edit"]);
    } else {
        args.extend(["-F", "-"]);
        if amend {
            args.push("--amend");
        }
    }
    let mut child = git(root)
        .args(&args)
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let input = message.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if !out.status.success() {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        let err = String::from_utf8_lossy(&out.stderr);
        if !text.is_empty() && !text.ends_with('\n') && !err.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
        return Err(CommitFailed { output: strip_ansi(&text).trim().to_string(), hook: has_commit_hooks(root) }.into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a commit runs hooks: pre-commit, prepare-commit-msg or commit-msg in the hooks
/// folder git uses (core.hooksPath, as husky sets it, or .git/hooks).
pub fn has_commit_hooks(root: &Path) -> bool {
    let Ok(dir) = run(root, &["rev-parse", "--git-path", "hooks"]) else { return false };
    let dir = root.join(dir.trim());
    ["pre-commit", "prepare-commit-msg", "commit-msg"].iter().any(|h| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(dir.join(h)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// Removes terminal color and cursor codes (ESC [ … letter) from tool output.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(n) = chars.next() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

pub fn branches(root: &Path) -> Vec<String> {
    run(root, &["for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes"])
        .map(|s| s.lines().filter(|l| !l.ends_with("/HEAD")).map(String::from).collect())
        .unwrap_or_default()
}

/// Local and remote branches for the graph's filter, most useful first: the current branch,
/// then the repo's default (what GitHub shows first: origin/HEAD, else main/master/develop/dev),
/// then the rest by name. Remote branches list origin's before other remotes'.
pub fn branches_split(root: &Path) -> (Vec<String>, Vec<String>) {
    let current = run(root, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
    let remotes = remotes(root);
    let default = default_branch(root, &remotes);
    let refs = run(root, &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"]).unwrap_or_default();
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for r in refs.lines() {
        if let Some(b) = r.strip_prefix("refs/heads/") {
            local.push(b.to_string());
        } else if let Some(b) = r.strip_prefix("refs/remotes/")
            && !b.ends_with("/HEAD")
        {
            remote.push(b.to_string());
        }
    }
    order_branches(&mut local, &mut remote, &current, default.as_deref(), &remotes);
    (local, remote)
}

/// The repo's main branch name ("main"), as its remote says, else a usual name that exists here.
fn default_branch(root: &Path, remotes: &[String]) -> Option<String> {
    let first = remotes.iter().find(|r| *r == "origin").or(remotes.first());
    if let Some(remote) = first
        && let Ok(head) = run(root, &["symbolic-ref", "--short", "-q", &format!("refs/remotes/{remote}/HEAD")])
        && let Some(name) = head.trim().strip_prefix(&format!("{remote}/"))
    {
        return Some(name.to_string());
    }
    ["main", "master", "develop", "dev"]
        .into_iter()
        .find(|n| run(root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{n}")]).is_ok())
        .map(String::from)
}

fn order_branches(local: &mut [String], remote: &mut [String], current: &str, default: Option<&str>, remotes: &[String]) {
    let rank = |name: &str| if name == current { 0 } else if Some(name) == default { 1 } else { 2 };
    local.sort_by(|a, b| (rank(a), a.as_str()).cmp(&(rank(b), b.as_str())));
    // "origin/main" → remote "origin", branch "main" (remotes are matched, since names can hold "/").
    let split = |full: &str| -> (usize, String) {
        let pos = remotes.iter().position(|r| full.starts_with(&format!("{r}/")));
        let name = pos.map(|p| full[remotes[p].len() + 1..].to_string()).unwrap_or(full.to_string());
        let order = match pos {
            Some(p) if remotes[p] == "origin" => 0,
            Some(p) => 1 + p,
            None => usize::MAX,
        };
        (order, name)
    };
    remote.sort_by(|a, b| {
        let ((ra, na), (rb, nb)) = (split(a), split(b));
        (ra, rank(&na), na, a.as_str()).cmp(&(rb, rank(&nb), nb, b.as_str()))
    });
}

// ---------------------------------------------------------------- repository sidebar

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Branch {
    pub name: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream was deleted on the remote.
    pub gone: bool,
    pub head: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stash {
    /// "stash@{0}"
    pub name: String,
    pub sha: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub sha: String,
    /// The repository's own folder (it can't be removed).
    pub main: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Submodule {
    pub path: String,
    pub sha: String,
    /// ' ' up to date, '-' not set up, '+' at another commit than recorded, 'U' conflicted.
    pub state: char,
}

/// What SourceGit's sidebar lists: branches, remotes, tags, stashes, worktrees, submodules.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoRefs {
    pub local: Vec<Branch>,
    /// Each remote with its branches (without the "remote/" prefix).
    pub remotes: Vec<(String, Vec<String>)>,
    /// Newest first.
    pub tags: Vec<String>,
    pub stashes: Vec<Stash>,
    pub worktrees: Vec<Worktree>,
    pub submodules: Vec<Submodule>,
    /// The repository tracks files with Git LFS (its `.gitattributes` says so).
    pub lfs: bool,
}

pub fn repo_refs(root: &Path) -> RepoRefs {
    let lines = |args: &[&str]| run(root, args).unwrap_or_default();
    let mut refs = RepoRefs::default();
    for l in lines(&["for-each-ref", "--format=%(refname:short)%1f%(upstream:short)%1f%(upstream:track)%1f%(HEAD)", "refs/heads"]).lines() {
        let f: Vec<&str> = l.split('\x1f').collect();
        let [name, up, track, head] = f[..] else { continue };
        let num = |key: &str| track.split(key).nth(1).and_then(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok()).unwrap_or(0);
        refs.local.push(Branch { name: name.into(), upstream: (!up.is_empty()).then(|| up.into()), ahead: num("ahead"), behind: num("behind"), gone: track.contains("gone"), head: head == "*" });
    }
    let current = refs.local.iter().find(|b| b.head).map(|b| b.name.clone()).unwrap_or_default();
    let names: Vec<String> = refs.local.iter().map(|b| b.name.clone()).collect();
    let remote_names = remotes(root);
    let default = default_branch(root, &remote_names);
    let mut local = names.clone();
    order_branches(&mut local, &mut [], &current, default.as_deref(), &remote_names);
    refs.local.sort_by_key(|b| local.iter().position(|n| *n == b.name));
    let all_remote = lines(&["for-each-ref", "--format=%(refname:short)", "refs/remotes"]);
    for r in &remote_names {
        let prefix = format!("{r}/");
        let branches = all_remote.lines().filter_map(|l| l.strip_prefix(&prefix)).filter(|b| *b != "HEAD").map(String::from).collect();
        refs.remotes.push((r.clone(), branches));
    }
    refs.tags = lines(&["for-each-ref", "--sort=-creatordate", "--format=%(refname:short)", "refs/tags"]).lines().map(String::from).collect();
    refs.stashes = lines(&["stash", "list", "--format=%gd%x1f%H%x1f%s"])
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\x1f');
            Some(Stash { name: f.next()?.into(), sha: f.next()?.into(), subject: f.next().unwrap_or("").into() })
        })
        .collect();
    refs.worktrees = parse_worktrees(&lines(&["worktree", "list", "--porcelain"]));
    refs.submodules = lines(&["submodule", "status"])
        .lines()
        .filter_map(|l| {
            let state = l.chars().next()?;
            let mut f = l[state.len_utf8()..].split_whitespace();
            Some(Submodule { sha: f.next()?.into(), path: f.next()?.into(), state })
        })
        .collect();
    refs.lfs = std::fs::read_to_string(root.join(".gitattributes")).is_ok_and(|a| a.contains("filter=lfs"));
    refs
}

fn parse_worktrees(out: &str) -> Vec<Worktree> {
    out.split("\n\n")
        .filter_map(|block| {
            let mut w = Worktree { path: PathBuf::new(), branch: None, sha: String::new(), main: false, locked: false };
            for l in block.lines() {
                let (k, v) = l.split_once(' ').unwrap_or((l, ""));
                match k {
                    "worktree" => w.path = PathBuf::from(v),
                    "HEAD" => w.sha = v.into(),
                    "branch" => w.branch = Some(v.trim_start_matches("refs/heads/").into()),
                    "locked" => w.locked = true,
                    _ => {}
                }
            }
            (!w.path.as_os_str().is_empty()).then_some(w)
        })
        .enumerate()
        .map(|(i, w)| Worktree { main: i == 0, ..w })
        .collect()
}

/// One file's history, following renames: its commits, and the name it had in each (by sha).
pub fn log_file(root: &Path, path: &str, limit: usize) -> Result<(Vec<Commit>, HashMap<String, String>)> {
    let n = format!("-n{limit}");
    let mut commits = parse_log(&run(root, &["log", "--follow", &n, LOG_FORMAT, "--", path])?, &remotes(root));
    // --follow turns off parent rewriting: link each change to the one before it, so the graph
    // draws one line (the file is the same in the commits skipped between them).
    let shas: Vec<String> = commits.iter().map(|c| c.sha.clone()).collect();
    for (c, older) in commits.iter_mut().zip(shas.iter().skip(1).map(Some).chain([None])) {
        c.parents = older.cloned().into_iter().collect();
    }
    let names = run(root, &["log", "--follow", &n, "--name-only", "--format=%x1e%H", "--", path])?
        .split('\x1e')
        .filter_map(|rec| {
            let mut l = rec.lines().filter(|l| !l.is_empty());
            Some((l.next()?.to_string(), l.next()?.to_string()))
        })
        .collect();
    Ok((commits, names))
}

/// Commits on any branch whose message or author contains `query`, or whose hash starts with
/// it; newest first.
pub fn search_log(root: &Path, query: &str, limit: usize) -> Result<Vec<Commit>> {
    let n = format!("-n{limit}");
    let mut shas: Vec<String> = Vec::new();
    // Together, --grep and --author would both have to match.
    for by in ["--grep", "--author"] {
        let out = run(root, &["log", "--exclude=refs/stash", "--all", "-i", "--fixed-strings", &n, "--format=%H", &format!("{by}={query}")])?;
        shas.extend(out.lines().map(String::from));
    }
    if query.len() >= 4
        && query.bytes().all(|b| b.is_ascii_hexdigit())
        && let Ok(sha) = run(root, &["rev-parse", "--verify", "-q", &format!("{query}^{{commit}}")])
    {
        shas.push(sha.trim().to_string());
    }
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["log", "--no-walk=sorted", LOG_FORMAT];
    args.extend(shas.iter().map(String::as_str));
    args.push("--");
    Ok(parse_log(&run(root, &args)?, &remotes(root)))
}

/// git-flow without the extension: the branches, merges and tags `git flow` makes, read from
/// and written to its config (`gitflow.branch.*`, `gitflow.prefix.*`). `kind` is feature,
/// release or hotfix; `action` init, start or finish.
pub fn flow(root: &Path, kind: &str, action: &str, name: &str) -> Result<String> {
    let conf = |key: &str| run(root, &["config", "--get", key]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let main = conf("gitflow.branch.master").or_else(|| default_branch(root, &remotes(root))).unwrap_or_else(|| "main".into());
    let develop = conf("gitflow.branch.develop").unwrap_or_else(|| "develop".into());
    if action == "init" {
        if run(root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{develop}")]).is_err() {
            run(root, &["branch", &develop, &main])?;
        }
        for (k, v) in [("branch.master", main.as_str()), ("branch.develop", &develop), ("prefix.feature", "feature/"), ("prefix.release", "release/"), ("prefix.hotfix", "hotfix/"), ("prefix.versiontag", "")] {
            if conf(&format!("gitflow.{k}")).is_none() {
                run(root, &["config", &format!("gitflow.{k}"), v])?;
            }
        }
        return Ok(format!("git-flow ready: {main} for releases, {develop} for development"));
    }
    if !matches!(kind, "feature" | "release" | "hotfix") || name.is_empty() {
        bail!("say feature, release or hotfix, then a name");
    }
    let prefix = conf(&format!("gitflow.prefix.{kind}")).unwrap_or_else(|| format!("{kind}/"));
    let short = name.strip_prefix(&prefix).unwrap_or(name);
    let branch = format!("{prefix}{short}");
    match action {
        "start" => {
            let base = if kind == "hotfix" { &main } else { &develop };
            run(root, &["switch", "-c", &branch, base])?;
            Ok(format!("Started {branch} from {base}"))
        }
        "finish" => {
            let merge = |into: &str| -> Result<()> {
                run(root, &["switch", into])?;
                run(root, &["merge", "--no-ff", "--no-edit", &branch]).map(|_| ())
            };
            if kind == "feature" {
                merge(&develop)?;
            } else {
                merge(&main)?;
                let tag = format!("{}{short}", conf("gitflow.prefix.versiontag").unwrap_or_default());
                run(root, &["tag", "-a", &tag, "-m", &tag])?;
                merge(&develop)?;
            }
            run(root, &["branch", "-d", &branch])?;
            Ok(format!("Finished {branch}"))
        }
        _ => bail!("say start or finish"),
    }
}

/// Commits from `first` up to HEAD, oldest first, for an interactive rebase: (sha, subject).
/// Merges are left out, as `git rebase -i` leaves them out.
pub fn rebase_commits(root: &Path, first: &str) -> Result<Vec<(String, String)>> {
    let first = run(root, &["rev-parse", "--verify", &format!("{first}^{{commit}}")])?.trim().to_string();
    let has_parent = run(root, &["rev-parse", "--verify", "-q", &format!("{first}^")]).is_ok();
    let range = if has_parent { format!("{first}^..HEAD") } else { "HEAD".into() };
    let out = run(root, &["rev-list", "--reverse", "--no-merges", "--format=%s", &range])?;
    // rev-list --format prints "commit <sha>" then the subject.
    let mut lines = out.lines();
    let mut v = Vec::new();
    while let (Some(head), Some(subject)) = (lines.next(), lines.next()) {
        v.push((head.trim_start_matches("commit ").to_string(), subject.to_string()));
    }
    if v.first().map(|c| &c.0) != Some(&first) {
        bail!("{} isn't in the current branch's history", &first[..7]);
    }
    Ok(v)
}

/// Runs `git rebase -i` from `first`'s parent with this todo: (action, sha, new message for a
/// reword). No editor opens: squashes keep both messages, rewords get theirs.
pub fn rebase_interactive(root: &Path, first: &str, steps: &[(String, String, Option<String>)]) -> Result<String> {
    let dir = git_dir(root).ok_or_else(|| anyhow!("no .git directory"))?;
    let mut todo = String::new();
    for (i, (action, sha, message)) in steps.iter().enumerate() {
        match (action.as_str(), message) {
            ("reword", Some(m)) => {
                let body = run(root, &["log", "-1", "--format=%b", sha]).unwrap_or_default();
                let file = dir.join(format!("oskl-reword-{i}"));
                std::fs::write(&file, if body.trim().is_empty() { format!("{m}\n") } else { format!("{m}\n\n{}\n", body.trim()) })?;
                // shortcut: a path with a single quote in it breaks this exec line.
                todo.push_str(&format!("pick {sha}\nexec git commit --amend --only -q --no-verify -F '{}'\n", file.display()));
            }
            (a, _) => todo.push_str(&format!("{a} {sha}\n")),
        }
    }
    let todo_file = dir.join("oskl-rebase-todo");
    std::fs::write(&todo_file, todo)?;
    let has_parent = run(root, &["rev-parse", "--verify", "-q", &format!("{first}^")]).is_ok();
    let base = if has_parent { format!("{first}^") } else { "--root".into() };
    let out = git(root)
        .env("GIT_SEQUENCE_EDITOR", format!("cp '{}'", todo_file.display()))
        .env("GIT_EDITOR", "true")
        .args(["rebase", "-i", "--autostash", &base])
        .output()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("{}", text.trim());
    }
    Ok(text)
}

/// The text of `commit.template`, if one is set and readable.
pub fn commit_template(root: &Path) -> Option<String> {
    let path = run(root, &["config", "--path", "commit.template"]).ok()?;
    let path = PathBuf::from(path.trim());
    let text = std::fs::read_to_string(if path.is_absolute() { path } else { root.join(path) }).ok()?;
    Some(clean_message(&text))
}

pub fn list_files(root: &Path) -> Vec<String> {
    run(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .map(|s| {
            let mut v: Vec<String> = s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn filter_reads_local_and_remote_branches_of_a_repo() {
        let base = std::env::temp_dir().join(format!("oskl-branches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sh = |cmd: &str| {
            std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(&base).output().unwrap();
        };
        sh("git init -q -b main up && cd up && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m one && git branch alpha && git branch zed");
        sh("git clone -q up work && cd work && git switch -qc topic");
        let (local, remote) = super::branches_split(&base.join("work"));
        assert_eq!(local, ["topic", "main"]);
        assert_eq!(remote, ["origin/main", "origin/alpha", "origin/zed"], "origin/HEAD is left out, the default comes first");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn filter_lists_current_then_default_then_the_rest_origin_first() {
        let mut local: Vec<String> = ["zeta", "main", "auth", "feature"].map(String::from).to_vec();
        let mut remote: Vec<String> = ["upstream/main", "origin/auth", "origin/feature", "origin/main", "fork/x"].map(String::from).to_vec();
        let remotes: Vec<String> = ["fork", "origin", "upstream"].map(String::from).to_vec();
        super::order_branches(&mut local, &mut remote, "feature", Some("main"), &remotes);
        assert_eq!(local, ["feature", "main", "auth", "zeta"]);
        assert_eq!(remote, ["origin/feature", "origin/main", "origin/auth", "fork/x", "upstream/main"]);
    }

    use super::*;

    #[test]
    fn parses_status_v2() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
                   1 .M N... 100644 100644 100644 a b src/a.ts\0\
                   1 M. N... 100644 100644 100644 a b src/b.ts\0\
                   2 R. N... 100644 100644 100644 a b R100 src/new.ts\0src/old.ts\0\
                   ? notes.md\0! node_modules/\0";
        let st = parse_status(out);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!((st.ahead, st.behind), (2, 1));
        assert_eq!(st.unstaged.len(), 2);
        assert_eq!(st.staged.len(), 2);
        let renamed = st.staged.iter().find(|c| c.status == FileStatus::Renamed).unwrap();
        assert_eq!(renamed.orig_path.as_deref(), Some("src/old.ts"));
        assert_eq!(st.ignored, vec!["node_modules"]);
    }

    #[test]
    fn parses_refs() {
        let refs = parse_refs("HEAD -> sessions, origin/main, tag: v1", &["origin".into()]);
        assert_eq!(refs[0].name, "sessions");
        assert!(refs[0].is_head);
        assert_eq!(refs[1].kind, RefKind::Remote);
        assert_eq!(refs[2].kind, RefKind::Tag);
    }

    #[test]
    fn mixes_hunks_and_lines() {
        let (index, work) = ("a\nb\nc\nd\n", "a\nB\nc\nd\nx\ny\n");
        assert_eq!(mix(index, work, 1..2, true, true), "a\nB\nc\nd\n", "the hunk at line 1 only");
        assert_eq!(mix(index, work, 4..5, true, true), "a\nb\nc\nd\nx\ny\n", "a whole insertion");
        assert_eq!(mix(index, work, 5..6, true, false), "a\nb\nc\nd\ny\n", "one added line");
        assert_eq!(mix(work, index, 1..2, false, true), "a\nb\nc\nd\nx\ny\n", "discard: lines of `from`");
        assert_eq!(mix("a\nb\nc\n", "a\nc\n", 1..2, true, true), "a\nc\n", "a deletion above the cursor line");
        assert_eq!(mix(index, work, 2..3, true, true), index, "untouched lines take nothing");
    }

    #[test]
    fn parses_blame_porcelain() {
        let sha = "a".repeat(40);
        let out = format!(
            "{sha} 1 1 2\nauthor Ann\nauthor-mail <ann@x>\nauthor-time 100\nsummary First\nfilename f\n\tline one\n{sha} 2 2\n\tline two\n"
        );
        let b = parse_blame(&out);
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.lines[1].as_ref().unwrap().author, "Ann");
        assert_eq!(b.lines[0].as_ref().unwrap().summary, "First");
    }
}

#[cfg(test)]
mod live {
    #[test]
    fn reads_real_repo() {
        let Ok(dir) = std::env::var("OSKL_REPO") else { return };
        let root = super::repo_root(std::path::Path::new(&dir)).expect("repo root");
        let st = super::status(&root).expect("status");
        println!("branch={:?} staged={} unstaged={}", st.branch, st.staged.len(), st.unstaged.len());
        let log = super::log(&root, 800, false, &[]);
        println!("log auto: {:?}", log.as_ref().map(|l| l.len()));
        let all = super::log(&root, 800, true, &[]).expect("log all");
        println!("log all: {}", all.len());
        assert!(!log.unwrap().is_empty());
    }

    use crate::git::{OpKind, abort_op, compare, continue_op, run, stage, status};
    use std::path::PathBuf;
    use std::process::Command;

    /// A repo where `main` and `feature` both changed line 1 of a.txt.
    fn conflicting_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oskl-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |cmd: &str| {
            let out = Command::new("sh").arg("-c").arg(cmd).current_dir(&dir).env("GIT_EDITOR", "true").output().unwrap();
            String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
        };
        sh("git init -q -b main && git config user.email t@t && git config user.name t && git config commit.gpgsign false");
        sh("printf 'base\\nkeep\\n' > a.txt && git add . && git commit -qm base");
        sh("git switch -qc feature && printf 'theirs\\nkeep\\n' > a.txt && git commit -qam feat");
        sh("git switch -q main && printf 'ours\\nkeep\\n' > a.txt && git commit -qam mine");
        dir
    }

    #[test]
    fn merge_conflict_state_and_continue() {
        let dir = conflicting_repo("merge");
        assert!(run(&dir, &["merge", "feature"]).is_err(), "conflicts");
        let st = status(&dir).unwrap();
        assert_eq!(st.conflicts.len(), 1);
        let op = st.op.expect("merge in progress");
        assert_eq!(op.kind, OpKind::Merge);
        assert_eq!(op.label, "Merging feature");
        assert!(op.message.starts_with("Merge branch 'feature'"), "{}", op.message);
        std::fs::write(dir.join("a.txt"), "ours\ntheirs\nkeep\n").unwrap();
        stage(&dir, &["a.txt".into()]).unwrap();
        continue_op(&dir, OpKind::Merge, &op.message).unwrap();
        let st = status(&dir).unwrap();
        assert!(st.op.is_none() && st.conflicts.is_empty());
        assert!(run(&dir, &["log", "-1", "--format=%P"]).unwrap().split_whitespace().count() == 2, "merge commit");
    }

    #[test]
    fn rebase_conflict_continue_and_abort() {
        let dir = conflicting_repo("rebase");
        assert!(run(&dir, &["rebase", "feature"]).is_err());
        let op = status(&dir).unwrap().op.expect("rebase in progress");
        assert_eq!(op.kind, OpKind::Rebase);
        assert!(op.label.starts_with("Rebasing main"), "{}", op.label);
        assert_eq!(op.message, "mine");
        std::fs::write(dir.join("a.txt"), "both\nkeep\n").unwrap();
        stage(&dir, &["a.txt".into()]).unwrap();
        continue_op(&dir, OpKind::Rebase, "mine, merged").unwrap();
        assert!(status(&dir).unwrap().op.is_none());
        assert_eq!(run(&dir, &["log", "-1", "--format=%s"]).unwrap().trim(), "mine, merged");
        // Abort leaves things as they were.
        let dir = conflicting_repo("abort");
        assert!(run(&dir, &["merge", "feature"]).is_err());
        abort_op(&dir, OpKind::Merge).unwrap();
        let st = status(&dir).unwrap();
        assert!(st.op.is_none() && st.conflicts.is_empty());
    }

    #[test]
    fn compare_branches_lists_their_changes() {
        let dir = conflicting_repo("compare");
        let c = compare(&dir, "main", "feature").unwrap();
        assert_eq!(c.details.files.len(), 1);
        assert_eq!(c.details.files[0].path, "a.txt");
        assert_eq!((c.ahead, c.behind), (1, 1), "each branch has one commit the other lacks");
        let back = compare(&dir, "feature", "main").unwrap();
        assert_eq!((back.ahead, back.behind), (1, 1));
    }
}
