//! Undo for the git actions the app runs (commit, amend, reset, rebase, merge, pull, cherry-pick,
//! revert, stash, pop, branch delete, discard, fix up). Before an action the repository's marks are read (HEAD, its
//! branch, the stashes, the branches); after it succeeds, what changed says how to put it back.
//! The old commit is also in git's reflog, so nothing here is the only copy of anything.
//! Only the last action per repository is kept, and an undo that would lose work asks first
//! or stops: it never throws away commits or uncommitted changes.

use super::{git, git_dir, run};
use anyhow::{Result, bail};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Soft,
    Mixed,
    Hard,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Commit,
    Fixup,
    Reset(Mode),
    Rebase,
    Merge,
    Pull,
    CherryPick,
    Revert,
    Discard,
    Stash,
    /// Pop or drop: the word for the toast.
    StashOut(&'static str),
    BranchDelete,
}

/// What `git <args>` is, when it is something Undo covers.
pub fn kind_of_args(args: &[String]) -> Option<Kind> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let flag = |f: &str| a.contains(&f);
    let going_on = || ["--continue", "--abort", "--skip", "--quit", "--edit-todo"].iter().any(|f| flag(f));
    match a.as_slice() {
        ["commit", ..] => Some(Kind::Commit),
        ["reset", ..] => Some(Kind::Reset(if flag("--hard") { Mode::Hard } else if flag("--soft") { Mode::Soft } else { Mode::Mixed })),
        ["rebase", ..] if !going_on() => Some(Kind::Rebase),
        ["merge", ..] if !going_on() => Some(Kind::Merge),
        ["pull", ..] => Some(Kind::Pull),
        ["cherry-pick", ..] if !going_on() => Some(Kind::CherryPick),
        ["revert", ..] if !going_on() => Some(Kind::Revert),
        ["stash"] | ["stash", "push" | "save", ..] => Some(Kind::Stash),
        ["stash", "pop", ..] => Some(Kind::StashOut("Popped")),
        ["stash", "drop", ..] => Some(Kind::StashOut("Dropped")),
        ["branch", d, ..] if matches!(*d, "-d" | "-D" | "-df" | "-fd" | "--delete") => Some(Kind::BranchDelete),
        _ => None,
    }
}

/// The names `Repo::run_op` is given for the actions that have no git command of their own.
pub fn kind_of_label(label: &str) -> Option<Kind> {
    match label {
        "Commit" => Some(Kind::Commit),
        "Discard" => Some(Kind::Discard),
        "Interactive rebase" => Some(Kind::Rebase),
        "Fix up" => Some(Kind::Fixup),
        _ => None,
    }
}

/// The repository before an action.
pub struct Mark {
    head: Option<String>,
    branch: Option<String>,
    stashes: Vec<(String, String)>,
    branches: Vec<(String, String)>,
    /// A rebase, merge or cherry-pick was already under way: the action is part of it.
    midway: bool,
    /// A copy of the changes in the files, made before an action that would throw them away.
    saved: Option<String>,
}

impl Mark {
    pub fn read(root: &Path, kind: Kind) -> Mark {
        let line = |args: &[&str]| run(root, args).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let midway = git_dir(root).is_some_and(|d| ["rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"].iter().any(|f| d.join(f).exists()));
        Mark {
            head: line(&["rev-parse", "--verify", "-q", "HEAD"]),
            branch: line(&["symbolic-ref", "-q", "--short", "HEAD"]),
            stashes: stashes(root),
            branches: run(root, &["for-each-ref", "--format=%(objectname)\t%(refname:short)", "refs/heads"])
                .map(|s| s.lines().filter_map(|l| l.split_once('\t')).map(|(sha, name)| (name.to_string(), sha.to_string())).collect())
                .unwrap_or_default(),
            midway,
            saved: matches!(kind, Kind::Discard | Kind::Reset(Mode::Hard)).then(|| line(&["stash", "create"])).flatten(),
        }
    }
}

/// (sha, subject) of each stash, newest first.
fn stashes(root: &Path) -> Vec<(String, String)> {
    run(root, &["stash", "list", "--format=%H\t%gs"]).map(|s| s.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect()).unwrap_or_default()
}

#[derive(Clone, Debug)]
enum Step {
    /// Move the branch back, with the changes kept as `mode` says.
    /// `to` is `None` for the first commit of a branch: the branch goes back to not existing yet.
    Move { branch: Option<String>, from: String, to: Option<String>, mode: Mode, saved: Option<String> },
    /// Apply this stash and drop its entry.
    Unstash { sha: String },
    /// Put this stash back in the list.
    Restash { sha: String, subject: String, popped: Option<Popped> },
    Recreate(Vec<(String, String)>),
    /// Put these files back as the copy `saved` had them.
    Restore { saved: String, files: Vec<String> },
}

/// What a stash pop left in the files, to take out again when they still match.
#[derive(Clone, Debug)]
struct Popped {
    /// A copy of the files right after the pop, and the commit they were on.
    saved: String,
    head: String,
    files: Vec<String>,
}

/// How to undo the last action of a repository.
#[derive(Clone, Debug)]
pub struct Undo {
    /// What happened, for the toast: "Committed 1a2b3c4".
    pub label: String,
    step: Step,
}

impl Undo {
    /// Undoing replaces the files (not only what is staged): uncommitted changes need care.
    pub fn replaces_files(&self) -> bool {
        matches!(self.step, Step::Move { mode: Mode::Hard, .. })
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Runs `f` (the action) between reading the marks and finding out what it did.
/// `ok` says whether its result is a success; nothing is recorded for a failure.
pub fn track<T>(root: &Path, kind: Option<Kind>, f: impl FnOnce() -> T, ok: impl Fn(&T) -> bool) -> (T, Option<Undo>) {
    let before = kind.map(|k| Mark::read(root, k));
    let out = f();
    let undo = if ok(&out) { kind.zip(before).and_then(|(k, m)| record(root, k, &m)) } else { None };
    (out, undo)
}

pub fn record(root: &Path, kind: Kind, before: &Mark) -> Option<Undo> {
    if before.midway {
        return None;
    }
    let after = Mark::read(root, Kind::Stash);
    let moved = |mode: Mode, label: String, saved: Option<String>| {
        let (to, from) = (before.head.clone(), after.head.clone()?);
        // Only a commit can start a branch; for the rest an empty branch has nothing to go back to.
        if to.is_none() && kind != Kind::Commit {
            return None;
        }
        (to.as_ref() != Some(&from) && before.branch == after.branch).then(|| Undo { label, step: Step::Move { branch: after.branch.clone(), from, to, mode, saved } })
    };
    let at = after.head.as_deref().map(short).unwrap_or("");
    match kind {
        Kind::Commit => {
            let amended = before.head.as_ref().zip(after.head.as_ref()).is_some_and(|(b, a)| run(root, &["merge-base", "--is-ancestor", b, a]).is_err());
            moved(Mode::Soft, format!("{} {at}", if amended { "Amended to" } else { "Committed" }), None)
        }
        Kind::Fixup => moved(Mode::Soft, "Folded the staged changes into an earlier commit".into(), None),
        // A hard reset that stays where it is throws the changes away: that is a discard.
        Kind::Reset(mode) => moved(mode, format!("Reset to {at}"), before.saved.clone()).or_else(|| if mode == Mode::Hard { restore(root, before) } else { None }),
        Kind::Rebase => moved(Mode::Hard, format!("Rebased {}", after.branch.as_deref().unwrap_or("HEAD")), None),
        Kind::Merge => moved(Mode::Hard, "Merged".into(), None),
        Kind::Pull => moved(Mode::Hard, "Pulled".into(), None),
        // Their commits are taken back; the changes stay staged, as with a commit.
        Kind::CherryPick => moved(Mode::Soft, "Cherry-picked".into(), None),
        Kind::Revert => moved(Mode::Soft, "Reverted".into(), None),
        Kind::Discard => restore(root, before),
        Kind::Stash => {
            let top = after.stashes.first().filter(|(sha, _)| !before.stashes.iter().any(|(b, _)| b == sha))?;
            Some(Undo { label: "Put your changes in a stash".into(), step: Step::Unstash { sha: top.0.clone() } })
        }
        Kind::StashOut(verb) => {
            let (sha, subject) = before.stashes.iter().find(|(sha, _)| !after.stashes.iter().any(|(a, _)| a == sha))?;
            let popped = (verb == "Popped").then(|| popped(root, sha, &after)).flatten();
            Some(Undo { label: format!("{verb} the stash"), step: Step::Restash { sha: sha.clone(), subject: subject.clone(), popped } })
        }
        Kind::BranchDelete => {
            let gone: Vec<(String, String)> = before.branches.iter().filter(|(n, _)| !after.branches.iter().any(|(a, _)| a == n)).cloned().collect();
            let label = match gone.as_slice() {
                [] => return None,
                [(n, _)] => format!("Deleted the branch {n}"),
                g => format!("Deleted {} branches", g.len()),
            };
            Some(Undo { label, step: Step::Recreate(gone) })
        }
    }
}

/// The files a pop changed, when it is safe to put them back later (no untracked files in the stash).
fn popped(root: &Path, stash: &str, after: &Mark) -> Option<Popped> {
    // The third parent holds the untracked files (empty unless the stash took some).
    if run(root, &["ls-tree", "-r", "--name-only", &format!("{stash}^3")]).is_ok_and(|s| !s.trim().is_empty()) {
        return None;
    }
    let files: Vec<String> = run(root, &["diff", "--name-only", "-z", &format!("{stash}^1"), stash]).ok()?.split('\0').filter(|f| !f.is_empty()).map(String::from).collect();
    let saved = run(root, &["stash", "create"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())?;
    let head = after.head.clone()?;
    (!files.is_empty()).then_some(Popped { saved, head, files })
}

/// The files that differ from the copy made before: what the action threw away.
fn restore(root: &Path, before: &Mark) -> Option<Undo> {
    let saved = before.saved.clone()?;
    let files: Vec<String> = run(root, &["diff", "--name-only", "-z", &saved]).ok()?.split('\0').filter(|f| !f.is_empty()).map(String::from).collect();
    let label = format!("Discarded changes in {} file{}", files.len(), if files.len() == 1 { "" } else { "s" });
    (!files.is_empty()).then_some(Undo { label, step: Step::Restore { saved, files } })
}

/// Whether tracked files have changes right now.
fn dirty(root: &Path) -> bool {
    run(root, &["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.trim().is_empty()).unwrap_or(true)
}

/// Puts things back. Stops (the record stays) when that would lose work: the branch moved on,
/// a file was edited since, or there are uncommitted changes and `stash_first` isn't set.
pub fn apply(root: &Path, undo: &Undo, stash_first: bool) -> Result<String> {
    let done = |extra: &str| Ok(format!("Undone: {}{extra}", undo.label));
    match &undo.step {
        Step::Move { branch, from, to, mode, saved } => {
            let head = run(root, &["rev-parse", "HEAD"])?.trim().to_string();
            let on = run(root, &["symbolic-ref", "-q", "--short", "HEAD"]).ok().map(|s| s.trim().to_string());
            if head != *from || on != *branch {
                bail!("{} has moved on since: undoing now would drop newer commits", branch.as_deref().unwrap_or("HEAD"));
            }
            let mut extra = "";
            if *mode == Mode::Hard && dirty(root) {
                if !stash_first {
                    bail!("You have uncommitted changes. Commit or stash them, then undo again");
                }
                run(root, &["stash", "push", "-m", &format!("Before undoing: {}", undo.label)])?;
                extra = ". Your later changes are in a stash";
            }
            match to {
                Some(to) => run(root, &["reset", match mode { Mode::Soft => "--soft", Mode::Mixed => "--mixed", Mode::Hard => "--hard" }, to])?,
                // The files stay staged, as after a soft reset.
                None => run(root, &["update-ref", "-d", "HEAD"])?,
            };
            if let Some(saved) = saved {
                run(root, &["stash", "apply", saved])?;
            }
            done(extra)
        }
        Step::Unstash { sha } => {
            let at = stashes(root).iter().position(|(s, _)| s == sha).ok_or_else(|| anyhow::anyhow!("That stash is gone"))?;
            run(root, &["stash", "apply", sha])?;
            run(root, &["stash", "drop", &format!("stash@{{{at}}}")])?;
            done("")
        }
        Step::Restash { sha, subject, popped } => {
            if !stashes(root).iter().any(|(s, _)| s == sha) {
                run(root, &["stash", "store", "-m", subject, sha])?;
            }
            let Some(p) = popped else { return done(". Its changes also stay in your files") };
            // Only files still as the pop left them (on the same commit) are taken out: later edits are never lost.
            let same = run(root, &["rev-parse", "HEAD"]).is_ok_and(|h| h.trim() == p.head)
                && run(root, &["diff", "--quiet", &p.saved, "--"]).is_ok();
            if !same {
                return done(". Its changes also stay in your files, which you changed since");
            }
            let (present, absent): (Vec<&String>, Vec<&String>) = p.files.iter().partition(|f| run(root, &["cat-file", "-e", &format!("HEAD:{f}")]).is_ok());
            if !present.is_empty() {
                let mut args = vec!["restore", "--source", "HEAD", "--staged", "--worktree", "--"];
                args.extend(present.iter().map(|f| f.as_str()));
                run(root, &args)?;
            }
            // Files the stash had added.
            for f in absent {
                run(root, &["rm", "-q", "-f", "--cached", "--ignore-unmatch", "--", f])?;
                std::fs::remove_file(root.join(f)).ok();
            }
            done(". Its changes are out of your files again")
        }
        Step::Recreate(gone) => {
            if let Some((n, _)) = gone.iter().find(|(n, _)| run(root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{n}")]).is_ok()) {
                bail!("A branch named {n} exists now");
            }
            for (n, sha) in gone {
                run(root, &["branch", n, sha])?;
            }
            done("")
        }
        Step::Restore { saved, files } => {
            // Right after the discard each file equals the index; one that differs was edited since.
            let edited: Vec<String> = run(root, &["diff", "--name-only", "-z", "--"]).map(|s| s.split('\0').map(String::from).collect()).unwrap_or_default();
            let (skip, back): (Vec<&String>, Vec<&String>) = files.iter().partition(|f| edited.contains(*f));
            let (present, absent): (Vec<&String>, Vec<&String>) = back.into_iter().partition(|f| run(root, &["cat-file", "-e", &format!("{saved}:{f}")]).is_ok());
            if !present.is_empty() {
                let mut args = vec!["restore", "--source", saved.as_str(), "--worktree", "--"];
                args.extend(present.iter().map(|f| f.as_str()));
                run(root, &args)?;
            }
            // The copy had no such file: the discard had brought it back.
            for f in absent {
                std::fs::remove_file(root.join(f)).ok();
            }
            if skip.len() == files.len() {
                bail!("You've changed those files since, so they were left as they are");
            }
            done(&if skip.is_empty() { String::new() } else { format!(". {} edited since, left as {} are", skip.len(), if skip.len() == 1 { "it is" } else { "they are" }) })
        }
    }
}

/// Whether a commit is on a remote branch (rewriting it takes a force push).
pub fn on_remote(root: &Path, sha: &str) -> bool {
    run(root, &["branch", "-r", "--contains", sha]).is_ok_and(|s| !s.trim().is_empty())
}

/// Why `sha` can't be fixed up from here (it must be a single-parent commit in this branch's history).
pub fn fixup_problem(root: &Path, sha: &str) -> Option<&'static str> {
    if run(root, &["merge-base", "--is-ancestor", sha, "HEAD"]).is_err() {
        return Some("That commit isn't in the checked-out branch's history");
    }
    let parents = run(root, &["rev-list", "--parents", "-n", "1", sha]).unwrap_or_default();
    (parents.split_whitespace().count() > 2).then_some("A merge commit can't be fixed up")
}

/// Commits the staged changes as a fixup of `sha`, then folds it in without an editor.
/// When the fold stops on a conflict it is aborted and the staged changes come back as they were.
pub fn fixup(root: &Path, sha: &str) -> Result<String> {
    if let Some(why) = fixup_problem(root, sha) {
        bail!("{why}");
    }
    if run(root, &["diff", "--cached", "--quiet"]).is_ok() {
        bail!("Stage the changes you want to fold in first");
    }
    let before = run(root, &["rev-parse", "HEAD"])?.trim().to_string();
    run(root, &["commit", "--fixup", sha])?;
    let base = if run(root, &["rev-parse", "--verify", "-q", &format!("{sha}^")]).is_ok() { format!("{sha}^") } else { "--root".into() };
    let out = git(root).env("GIT_SEQUENCE_EDITOR", "true").env("GIT_EDITOR", "true").args(["rebase", "-i", "--autosquash", "--autostash", "--rebase-merges", &base]).output()?;
    if !out.status.success() {
        run(root, &["rebase", "--abort"]).ok();
        run(root, &["reset", "--soft", &before])?;
        bail!("The staged changes don't fit into that commit (they conflict with later ones). Nothing was changed");
    }
    Ok(format!("Folded the staged changes into {}", short(sha)))
}
