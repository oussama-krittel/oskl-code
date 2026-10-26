//! Git backend. Everything goes through the `git` CLI so hooks, signing,
//! credentials and config behave exactly like they do in a terminal.

use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

pub const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

fn git(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.quotepath=off", "-c", "color.ui=false"])
        .stdin(Stdio::null());
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

/// Runs git with arbitrary user args, returning combined output and success.
pub fn run_user(root: &Path, args: &[String]) -> (bool, String) {
    match git(root).args(args).output() {
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
    Ok(parse_status(&out))
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
    pub email: String,
    pub time: i64,
    pub refs: Vec<Ref>,
    pub subject: String,
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

const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1e";

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
        args.push("--all".into());
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
    let out = run(root, &arg_refs)?;
    let mut commits = Vec::new();
    for rec in out.split('\x1e') {
        let rec = rec.trim_start_matches('\n');
        if rec.is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.splitn(7, '\x1f').collect();
        if f.len() < 7 {
            continue;
        }
        commits.push(Commit {
            sha: f[0].to_string(),
            parents: f[1].split(' ').filter(|p| !p.is_empty()).map(String::from).collect(),
            author: f[2].to_string(),
            email: f[3].to_string(),
            time: f[4].parse().unwrap_or(0),
            refs: parse_refs(f[5], &remotes),
            subject: f[6].to_string(),
        });
    }
    Ok(commits)
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
            "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%ad%x1f%at%x1f%cn%x1f%D%x1f%B",
            sha,
            "--",
        ],
    )?;
    let f: Vec<&str> = out.splitn(9, '\x1f').collect();
    if f.len() < 9 {
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
        refs: parse_refs(f[7], &remotes(root)),
        message: f[8].trim_end().to_string(),
        files: parse_name_status(&files_out),
    })
}

fn parse_name_status(out: &str) -> Vec<Change> {
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
}

impl Rev {
    pub fn label(&self) -> String {
        match self {
            Rev::Commit(sha) => sha[..sha.len().min(7)].to_string(),
            Rev::Index => "Index".into(),
            Rev::Working => "Working Tree".into(),
            Rev::Empty => "Empty".into(),
        }
    }
}

pub fn read_rev(root: &Path, rev: &Rev, path: &str) -> Option<String> {
    let bytes = match rev {
        Rev::Empty => return Some(String::new()),
        Rev::Working => std::fs::read(root.join(path)).ok()?,
        Rev::Index => git(root).args(["show", &format!(":{path}")]).output().ok().filter(|o| o.status.success())?.stdout,
        Rev::Commit(sha) => git(root).args(["show", &format!("{sha}:{path}")]).output().ok().filter(|o| o.status.success())?.stdout,
    };
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
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

    pub fn short(&self) -> &str {
        &self.sha[..7]
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

pub fn commit(root: &Path, message: &str, amend: bool) -> Result<String> {
    let mut args = vec!["commit", "-F", "-"];
    if amend {
        args.push("--amend");
    }
    run_with_stdin(root, &args, message)
}

pub fn branches(root: &Path) -> Vec<String> {
    run(root, &["for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes"])
        .map(|s| s.lines().filter(|l| !l.ends_with("/HEAD")).map(String::from).collect())
        .unwrap_or_default()
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
        let Ok(dir) = std::env::var("KITE_REPO") else { return };
        let root = super::repo_root(std::path::Path::new(&dir)).expect("repo root");
        let st = super::status(&root).expect("status");
        println!("branch={:?} staged={} unstaged={}", st.branch, st.staged.len(), st.unstaged.len());
        let log = super::log(&root, 800, false, &[]);
        println!("log auto: {:?}", log.as_ref().map(|l| l.len()));
        let all = super::log(&root, 800, true, &[]).expect("log all");
        println!("log all: {}", all.len());
        assert!(!log.unwrap().is_empty());
    }
}
