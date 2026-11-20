//! Branches, remotes, tags, stashes, worktrees, submodules and LFS as a tree (SourceGit's
//! sidebar), with everything git does to them: menus, names typed in place, small dialogs for
//! the rest, and the interactive rebase dialog. The Source Control panel and the Git tab each
//! hold one; their commit lists open their commit menus through it too.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::{self, Branch, Commit, RefKind, Stash, Submodule, Worktree};
use crate::repo::Repo;
use crate::request::Request;
use crate::ui::px;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

#[path = "git_rebase.rs"]
mod rebase;

/// What a menu item, a dialog or a name typed in place does.
#[derive(Clone)]
pub enum Act {
    None,
    /// `git <args>`, its output shown.
    Git(Vec<String>),
    /// Several git commands in a row, stopping at the first that fails.
    Steps(String, Vec<Vec<String>>),
    /// Asks first; each button runs its commands.
    Ask { title: String, detail: String, buttons: Vec<(String, Vec<Vec<String>>)> },
    /// A small dialog asks for what's needed.
    Form(FormKind),
    /// A name typed right in the tree.
    Inline(InlineKind),
    Commit(String),
    /// What this branch or commit has that the current branch doesn't.
    Compare(String),
    /// Two commits (or branches), whichever came first as the base.
    CompareTwo(String, String),
    Mark(String),
    Copy(String),
    Window(PathBuf),
    Rebase(String),
    /// Fold the staged changes into this commit; whether the user already accepted rewriting a pushed one.
    Fixup(String, bool),
    Flow(String, String, String),
    SavePatch(String, PathBuf),
    OpenFile(PathBuf),
    History(PathBuf),
    MergeEditor(PathBuf),
}


/// To the list showing the history: select this branch's, tag's or stash's commit.
pub struct Reveal(pub String);

/// Dialogs: what they ask for.
#[derive(Clone)]
pub enum FormKind {
    NewBranch { start: String },
    Tag { at: String },
    RemoteAdd,
    RemoteUrl { remote: String, url: String },
    Upstream { branch: String },
    StashBranch { stash: String },
    WorktreeAdd,
    SubmoduleAdd,
    FlowStart { kind: String },
    LfsTrack,
    PatchSave { commit: String },
    PatchApply,
}

/// Names typed in place: the row turns into a text box.
#[derive(Clone)]
pub enum InlineKind {
    RenameBranch(String),
    RenameRemote(String),
    /// A new row under Branches: a branch at HEAD, checked out.
    NewBranch,
    /// A new row under Stashes: the message; Enter stashes everything.
    StashSave,
}

impl InlineKind {
    /// The row it replaces (or the group it opens under).
    fn key(&self) -> String {
        match self {
            InlineKind::RenameBranch(b) => format!("local:{b}"),
            InlineKind::RenameRemote(r) => format!("remote:{r}"),
            InlineKind::NewBranch => "new:local".into(),
            InlineKind::StashSave => "new:stashes".into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Style {
    Normal,
    Danger,
    /// Greyed: a heading (e.g. the commit the menu is about).
    Title,
}

#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub act: Act,
    pub style: Style,
}

pub fn item(label: impl Into<String>, act: Act) -> MenuItem {
    MenuItem { label: label.into(), act, style: Style::Normal }
}

fn danger(label: impl Into<String>, act: Act) -> MenuItem {
    MenuItem { label: label.into(), act, style: Style::Danger }
}

fn title(label: impl Into<String>) -> MenuItem {
    MenuItem { label: label.into(), act: Act::None, style: Style::Title }
}

pub fn sep() -> MenuItem {
    item("", Act::None)
}

pub fn git_act<const N: usize>(args: [&str; N]) -> Act {
    Act::Git(args.iter().map(|s| s.to_string()).collect())
}

pub fn cmd<const N: usize>(args: [&str; N]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

fn ask(title: String, detail: &str, buttons: Vec<(String, Vec<Vec<String>>)>) -> Act {
    Act::Ask { title, detail: detail.into(), buttons }
}

/// "origin/main" → ("origin", "main"): remote names can hold "/", so they're matched.
fn split_remote<'a>(full: &'a str, remotes: &[(String, Vec<String>)]) -> Option<(&'a str, &'a str)> {
    let (r, _) = remotes.iter().find(|(r, _)| full.starts_with(&format!("{r}/")))?;
    Some((&full[..r.len()], &full[r.len() + 1..]))
}

/// "On main: notes" → "notes"; git's own "WIP on main: 1a2b3c4 subject" → "WIP: subject".
fn stash_label(subject: &str) -> String {
    match subject.split_once(": ") {
        Some((head, rest)) if head.starts_with("WIP on ") => format!("WIP: {}", rest.split_once(' ').map(|(_, s)| s).unwrap_or(rest)),
        Some((_, rest)) => rest.to_string(),
        None => subject.to_string(),
    }
}

/// A dialog's field, which must not be empty.
fn need(v: &str, what: &str) -> Result<String, String> {
    if v.trim().is_empty() { Err(format!("Type {what} first")) } else { Ok(v.trim().to_string()) }
}

/// A row of the tree.
#[derive(Clone)]
enum Row {
    Group { key: &'static str, title: &'static str, count: usize, open: bool },
    Local(Branch),
    Remote(String, usize),
    RemoteBranch(String, String),
    Tag(String),
    Stash(Stash),
    Worktree(Worktree),
    Submodule(Submodule),
    More(usize),
    /// A line of text in a group (an error, "none open").
    Note(String),
    /// Where a new name is typed (under its group).
    New(String),
}

/// Long lists stop here (the graph's filter and search reach the rest).
const MAX_ROWS: usize = 300;

struct Form {
    title: String,
    fields: Vec<(String, Entity<Editor>)>,
    checks: Vec<(String, bool)>,
    submit: String,
    /// From the fields' text and the boxes ticked to what to run, or what's missing.
    run: Rc<dyn Fn(&[String], &[bool]) -> Result<Act, String>>,
}

struct Inline {
    kind: InlineKind,
    input: Entity<Editor>,
}

pub struct RefsTree {
    pub repo: Entity<Repo>,
    folded: HashSet<String>,
    menu: Option<(Vec<MenuItem>, Point<Pixels>)>,
    form: Option<Form>,
    form_error: Option<String>,
    inline: Option<Inline>,
    /// A commit marked to be compared with the next one picked.
    pub marked: Option<String>,
    rebase: Option<rebase::Plan>,
    /// Draw the tree; off, only the menus, dialogs and rebase dialog show (the owner's list is
    /// what was clicked).
    pub show_tree: bool,
    /// The tree's height in the sidebar; None fills its place (the Git tab).
    pub max_h: Option<f32>,
    /// The row last clicked (highlighted).
    selected: Option<String>,
    /// The open pull requests (`gh` reads them; the Git tab's chip for this branch's).
    prs: Option<Result<Vec<git::gh::Pr>, String>>,
    prs_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for RefsTree {}
impl EventEmitter<Reveal> for RefsTree {}

impl RefsTree {
    pub fn new(repo: Entity<Repo>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.observe(&repo, |_, _, cx| cx.notify())];
        Self {
            repo,
            folded: ["remotes", "tags"].map(String::from).into(),
            menu: None,
            form: None,
            form_error: None,
            inline: None,
            marked: None,
            rebase: None,
            show_tree: true,
            max_h: None,
            selected: None,
            prs: None,
            prs_task: None,
            _subs: subs,
        }
    }

    /// Reads the open pull requests again (in the background).
    pub fn load_prs(&mut self, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        let task = cx.background_executor().spawn(async move { git::gh::list(&root, "open").map_err(|e| e.to_string()) });
        self.prs_task = Some(cx.spawn(async move |t, cx| {
            let prs = task.await;
            t.update(cx, |t, cx| {
                t.prs = Some(prs);
                cx.notify();
            })
            .ok();
        }));
    }

    /// The open pull requests read so far.
    pub fn prs(&self) -> Vec<git::gh::Pr> {
        self.prs.clone().and_then(|p| p.ok()).unwrap_or_default()
    }

    pub fn set_repo(&mut self, repo: Entity<Repo>, cx: &mut Context<Self>) {
        if repo != self.repo {
            self._subs = vec![cx.observe(&repo, |_, _, cx| cx.notify())];
            self.repo = repo;
            self.menu = None;
            self.form = None;
            self.inline = None;
            self.rebase = None;
            self.marked = None;
            cx.notify();
        }
    }

    /// One of its text boxes has the keyboard (the owner's single-letter keys must wait).
    pub fn typing(&self, window: &Window, cx: &App) -> bool {
        let rebase = self.rebase.iter().flat_map(|p| &p.steps).map(|s| &s.message);
        let form = self.form.iter().flat_map(|f| &f.fields).map(|f| &f.1);
        self.inline.iter().map(|i| &i.input).chain(form).chain(rebase).any(|e| e.read(cx).focus_handle(cx).contains_focused(window, cx))
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let refs = self.repo.read(cx).refs.clone();
        let mut rows = Vec::new();
        let new_under = |key: &str| self.inline.as_ref().is_some_and(|i| i.kind.key() == format!("new:{key}"));
        let group = |rows: &mut Vec<Row>, key: &'static str, title: &'static str, count: usize, always: bool| -> bool {
            if count == 0 && !always && !new_under(key) {
                return false;
            }
            let open = !self.folded.contains(key) || new_under(key);
            rows.push(Row::Group { key, title, count, open });
            if open && new_under(key) {
                rows.push(Row::New(key.to_string()));
            }
            open
        };
        if group(&mut rows, "local", "Branches", refs.local.len(), true) {
            rows.extend(refs.local.iter().take(MAX_ROWS).cloned().map(Row::Local));
        }
        if group(&mut rows, "remotes", "Remotes", refs.remotes.len(), true) {
            if refs.remotes.is_empty() {
                rows.push(Row::Note("No remote yet. Right-click Remotes to add one, then push.".into()));
            }
            for (r, branches) in &refs.remotes {
                rows.push(Row::Remote(r.clone(), branches.len()));
                if !self.folded.contains(&format!("remote:{r}")) {
                    rows.extend(branches.iter().take(MAX_ROWS).map(|b| Row::RemoteBranch(r.clone(), b.clone())));
                    if branches.len() > MAX_ROWS {
                        rows.push(Row::More(branches.len() - MAX_ROWS));
                    }
                }
            }
        }
        if group(&mut rows, "tags", "Tags", refs.tags.len(), true) {
            rows.extend(refs.tags.iter().take(MAX_ROWS).cloned().map(Row::Tag));
            if refs.tags.len() > MAX_ROWS {
                rows.push(Row::More(refs.tags.len() - MAX_ROWS));
            }
        }
        if group(&mut rows, "stashes", "Stashes", refs.stashes.len(), true) {
            rows.extend(refs.stashes.iter().cloned().map(Row::Stash));
            if refs.stashes.is_empty() {
                rows.push(Row::Note("No stashes. Stash puts your changes aside.".into()));
            }
        }
        // One worktree is the repository itself: the group shows once there's another.
        if group(&mut rows, "worktrees", "Worktrees", refs.worktrees.len(), refs.worktrees.len() > 1) {
            rows.extend(refs.worktrees.iter().cloned().map(Row::Worktree));
        }
        if group(&mut rows, "submodules", "Submodules", refs.submodules.len(), false) {
            rows.extend(refs.submodules.iter().cloned().map(Row::Submodule));
        }
        if refs.lfs {
            group(&mut rows, "lfs", "LFS", 0, true);
        }
        rows
    }

    fn first_remote(&self, cx: &App) -> Option<String> {
        let refs = self.repo.read(cx).refs.clone();
        refs.remotes.iter().map(|(r, _)| r.clone()).find(|r| r == "origin").or_else(|| refs.remotes.first().map(|(r, _)| r.clone()))
    }

    // ------------------------------------------------------------ menus

    fn group_menu(&self, key: &str, cx: &App) -> Vec<MenuItem> {
        let remote = self.first_remote(cx).unwrap_or_else(|| "origin".into());
        let flow = |kind: &str| item(format!("Git Flow: start a {kind}…"), Act::Form(FormKind::FlowStart { kind: kind.into() }));
        match key {
            "local" => vec![item("New branch…", Act::Inline(InlineKind::NewBranch)), sep(), item("Git Flow: set up", Act::Flow(String::new(), "init".into(), String::new())), flow("feature"), flow("release"), flow("hotfix")],
            "remotes" => vec![item("Add remote…", Act::Form(FormKind::RemoteAdd)), item("Fetch all (prune)", git_act(["fetch", "--all", "--prune"]))],
            "tags" => vec![item("New tag on HEAD…", Act::Form(FormKind::Tag { at: "HEAD".into() })), item(format!("Push all tags to {remote}"), git_act(["push", &remote, "--tags"]))],
            "stashes" => vec![
                item("Stash changes…", Act::Inline(InlineKind::StashSave)),
                item("Stash only the staged changes", git_act(["stash", "push", "--staged"])),
                item("Apply a patch file…", Act::Form(FormKind::PatchApply)),
                sep(),
                danger("Clear all stashes…", ask("Drop every stash?".into(), "Their changes are gone for good.", vec![("Drop All".into(), vec![cmd(["stash", "clear"])])])),
            ],
            "worktrees" => vec![item("New worktree…", Act::Form(FormKind::WorktreeAdd)), item("Prune missing worktrees", git_act(["worktree", "prune"]))],
            "submodules" => vec![item("Update all", git_act(["submodule", "update", "--init", "--recursive"])), item("Add submodule…", Act::Form(FormKind::SubmoduleAdd))],
            "lfs" => vec![
                item("Pull LFS files", git_act(["lfs", "pull"])),
                item("Fetch LFS files", git_act(["lfs", "fetch"])),
                item(format!("Push LFS files to {remote}"), git_act(["lfs", "push", "--all", &remote])),
                item("Prune old LFS files", git_act(["lfs", "prune"])),
                item("Track files with LFS…", Act::Form(FormKind::LfsTrack)),
                item("Show locks", git_act(["lfs", "locks"])),
            ],
            _ => Vec::new(),
        }
    }

    pub fn local_menu(&self, b: &Branch, cx: &App) -> Vec<MenuItem> {
        let refs = self.repo.read(cx).refs.clone();
        let current = self.repo.read(cx).branch_label();
        let n = b.name.as_str();
        let mut v = vec![title(n)];
        if !b.head {
            v.push(item(format!("Checkout {n}"), git_act(["switch", n])));
        }
        match b.upstream.as_deref().and_then(|u| split_remote(u, &refs.remotes).map(|s| (u, s))) {
            Some((up, (r, rb))) => {
                if b.head {
                    v.push(item(format!("Pull {up}"), git_act(["pull"])));
                } else {
                    v.push(item(format!("Fast-forward to {up}"), git_act(["fetch", r, &format!("{rb}:{n}")])));
                }
                v.push(item(format!("Push to {up}"), git_act(["push", r, &format!("{n}:{rb}")])));
            }
            None => {
                if let Some(r) = self.first_remote(cx) {
                    v.push(item(format!("Push to {r} and track it"), git_act(["push", "-u", &r, n])));
                }
            }
        }
        if !b.head {
            v.extend([
                sep(),
                item(format!("Merge {n} into {current}"), git_act(["merge", "--no-edit", n])),
                item(format!("Merge {n} into {current} (always a merge commit)"), git_act(["merge", "--no-ff", "--no-edit", n])),
                item(format!("Squash {n} into {current} (then commit)"), git_act(["merge", "--squash", n])),
                item(format!("Rebase {current} onto {n}"), git_act(["rebase", n])),
                item(format!("Compare with {current}"), Act::Compare(n.into())),
            ]);
        }
        v.extend([
            sep(),
            item(format!("New branch from {n}…"), Act::Form(FormKind::NewBranch { start: n.into() })),
            item("Tag its tip…", Act::Form(FormKind::Tag { at: n.into() })),
            item("Set upstream…", Act::Form(FormKind::Upstream { branch: n.into() })),
        ]);
        if let Some(up) = &b.upstream {
            v.push(item(format!("Stop tracking {up}"), git_act(["branch", "--unset-upstream", n])));
        }
        v.push(item("Rename…", Act::Inline(InlineKind::RenameBranch(n.into()))));
        for kind in ["feature", "release", "hotfix"] {
            if let Some(short) = n.strip_prefix(&format!("{kind}/")) {
                v.push(item(format!("Git Flow: finish this {kind}"), Act::Flow(kind.into(), "finish".into(), short.into())));
            }
        }
        v.push(item("Copy name", Act::Copy(n.into())));
        if !b.head {
            v.push(sep());
            v.push(danger(
                format!("Delete {n}…"),
                ask(
                    format!("Delete the branch {n}?"),
                    "Delete stops if it has commits no other branch has; Delete Anyway doesn't.",
                    vec![("Delete".into(), vec![cmd(["branch", "-d", n])]), ("Delete Anyway".into(), vec![cmd(["branch", "-D", n])])],
                ),
            ));
        }
        v
    }

    fn remote_menu(&self, r: &str, cx: &App) -> Vec<MenuItem> {
        let url = git::run(&self.repo.read(cx).root, &["remote", "get-url", r]).map(|s| s.trim().to_string()).unwrap_or_default();
        vec![
            title(url.clone()),
            item(format!("Fetch {r}"), git_act(["fetch", r, "--prune"])),
            item("Prune branches deleted there", git_act(["remote", "prune", r])),
            sep(),
            item("Change URL…", Act::Form(FormKind::RemoteUrl { remote: r.into(), url: url.clone() })),
            item("Rename…", Act::Inline(InlineKind::RenameRemote(r.into()))),
            item("Copy URL", Act::Copy(url)),
            sep(),
            danger(format!("Remove {r}…"), ask(format!("Remove the remote {r}?"), "Its branches go from here; nothing changes on the server.", vec![("Remove".into(), vec![cmd(["remote", "remove", r])])])),
        ]
    }

    pub fn remote_branch_menu(&self, r: &str, b: &str, cx: &App) -> Vec<MenuItem> {
        let full = format!("{r}/{b}");
        let current = self.repo.read(cx).branch_label();
        let has_local = self.repo.read(cx).refs.local.iter().any(|l| l.name == b);
        vec![
            title(full.clone()),
            item(format!("Checkout {b}"), if has_local { git_act(["switch", b]) } else { git_act(["switch", "--track", &full]) }),
            item("Checkout detached", git_act(["switch", "--detach", &full])),
            sep(),
            item(format!("Merge into {current}"), git_act(["merge", "--no-edit", &full])),
            item(format!("Rebase {current} onto this"), git_act(["rebase", &full])),
            item(format!("Compare with {current}"), Act::Compare(full.clone())),
            item("New branch from here…", Act::Form(FormKind::NewBranch { start: full.clone() })),
            item("Copy name", Act::Copy(full.clone())),
            sep(),
            danger(format!("Delete {b} on {r}…"), ask(format!("Delete {b} on {r}?"), "The branch is deleted on the server, for everyone.", vec![("Delete on Remote".into(), vec![cmd(["push", r, "--delete", b])])])),
        ]
    }

    pub fn tag_menu(&self, t: &str, cx: &App) -> Vec<MenuItem> {
        let current = self.repo.read(cx).branch_label();
        let mut v = vec![title(t), item("Checkout (detached)", git_act(["switch", "--detach", t])), item(format!("Compare with {current}"), Act::Compare(t.into())), item("New branch from it…", Act::Form(FormKind::NewBranch { start: t.into() }))];
        let mut buttons = vec![("Delete".to_string(), vec![cmd(["tag", "-d", t])])];
        if let Some(r) = self.first_remote(cx) {
            let refname = format!("refs/tags/{t}");
            v.push(item(format!("Push to {r}"), git_act(["push", &r, &refname])));
            buttons.push((format!("Delete Here and on {r}"), vec![cmd(["tag", "-d", t]), cmd(["push", &r, "--delete", &refname])]));
        }
        v.extend([item("Copy name", Act::Copy(t.into())), sep(), danger(format!("Delete {t}…"), ask(format!("Delete the tag {t}?"), "", buttons))]);
        v
    }

    fn stash_menu(&self, s: &Stash) -> Vec<MenuItem> {
        let n = s.name.as_str();
        vec![
            title(s.subject.clone()),
            item("Show its changes", Act::Commit(s.sha.clone())),
            item("Apply", git_act(["stash", "apply", n])),
            item("Pop (apply and drop)", git_act(["stash", "pop", n])),
            item("New branch from it…", Act::Form(FormKind::StashBranch { stash: n.into() })),
            sep(),
            danger(format!("Drop {n}…"), ask(format!("Drop {n}?"), "Its changes are gone for good.", vec![("Drop".into(), vec![cmd(["stash", "drop", n])])])),
        ]
    }

    fn worktree_menu(&self, w: &Worktree) -> Vec<MenuItem> {
        let p = w.path.to_string_lossy().to_string();
        let mut v = vec![title(p.clone()), item("Open in a new window", Act::Window(w.path.clone())), item("Copy path", Act::Copy(p.clone()))];
        if !w.main {
            v.push(if w.locked { item("Unlock", git_act(["worktree", "unlock", &p])) } else { item("Lock (keep it from being pruned)", git_act(["worktree", "lock", &p])) });
            v.extend([
                sep(),
                danger(
                    "Remove…",
                    ask(
                        format!("Remove the worktree at {p}?"),
                        "Its folder is deleted; its branch stays.",
                        vec![("Remove".into(), vec![cmd(["worktree", "remove", &p])]), ("Remove With Its Changes".into(), vec![cmd(["worktree", "remove", "--force", &p])])],
                    ),
                ),
            ]);
        }
        v
    }

    fn submodule_menu(&self, m: &Submodule, cx: &App) -> Vec<MenuItem> {
        let root = self.repo.read(cx).root.clone();
        let p = m.path.as_str();
        vec![
            title(p),
            item("Open in a new window", Act::Window(root.join(p))),
            item("Update to the recorded commit", git_act(["submodule", "update", "--init", "--recursive", "--", p])),
            item("Update to its remote's latest", git_act(["submodule", "update", "--init", "--remote", "--", p])),
            item("Copy path", Act::Copy(p.into())),
        ]
    }

    /// A branch or tag label on a commit.
    pub fn ref_menu(&self, r: &git::Ref, cx: &App) -> Vec<MenuItem> {
        let refs = self.repo.read(cx).refs.clone();
        match r.kind {
            RefKind::Local => refs.local.iter().find(|b| b.name == r.name).map(|b| self.local_menu(b, cx)).unwrap_or_default(),
            RefKind::Remote => split_remote(&r.name, &refs.remotes).map(|(rem, b)| self.remote_branch_menu(rem, b, cx)).unwrap_or_default(),
            RefKind::Tag => self.tag_menu(&r.name, cx),
        }
    }

    /// Everything a commit in a history list can do.
    pub fn commit_menu(&self, c: &Commit, in_checkout: bool, cx: &App) -> Vec<MenuItem> {
        let sha = c.sha.as_str();
        let short = c.short().to_string();
        let current = self.repo.read(cx).branch_label();
        let head = self.repo.read(cx).status.head_oid.clone().unwrap_or_default();
        let mut v = vec![title(format!("{short} {}", c.subject)), item("Open commit", Act::Commit(sha.into()))];
        let locals: Vec<&git::Ref> = c.refs.iter().filter(|r| r.kind == RefKind::Local && !r.is_head).collect();
        if !locals.is_empty() {
            v.push(sep());
            v.extend(locals.iter().map(|r| item(format!("Checkout {}", r.name), git_act(["switch", &r.name]))));
        }
        v.extend([
            sep(),
            item(format!("Checkout {short} (detached)"), git_act(["switch", "--detach", sha])),
            item("New branch here…", Act::Form(FormKind::NewBranch { start: sha.into() })),
            item("Tag this commit…", Act::Form(FormKind::Tag { at: sha.into() })),
            sep(),
            item(format!("Cherry-pick onto {current}"), git_act(["cherry-pick", sha])),
            item("Cherry-pick without committing", git_act(["cherry-pick", "-n", sha])),
            item("Revert (a new commit undoing it)", git_act(["revert", "--no-edit", sha])),
            sep(),
            item(format!("Reset {current} here, keeping changes staged"), git_act(["reset", "--soft", sha])),
            item(format!("Reset {current} here, keeping changes unstaged"), git_act(["reset", "--mixed", sha])),
            danger(
                format!("Reset {current} here, discarding changes…"),
                ask(format!("Reset {current} to {short} and discard every change?"), "Uncommitted changes and the commits after it are dropped (the reflog still has the commits).", vec![("Reset".into(), vec![cmd(["reset", "--hard", sha])])]),
            ),
        ]);
        if in_checkout && sha != head {
            v.push(item("Interactive rebase from here…", Act::Rebase(sha.into())));
        }
        if git::undo::fixup_problem(&self.repo.read(cx).root, sha).is_none() {
            v.push(item("Fix up this commit with the staged changes", Act::Fixup(sha.into(), false)));
        }
        v.extend([sep(), item("Save as a patch…", Act::Form(FormKind::PatchSave { commit: sha.into() }))]);
        if let Some(m) = self.marked.clone().filter(|m| m != sha) {
            v.push(item(format!("Compare with {}", &m[..m.len().min(7)]), Act::CompareTwo(m, sha.into())));
        }
        v.extend([
            item("Mark to compare with another commit", Act::Mark(sha.into())),
            item("Compare with HEAD", Act::CompareTwo(sha.into(), "HEAD".into())),
            item("Copy hash", Act::Copy(sha.into())),
        ]);
        v
    }

    // ------------------------------------------------------------ doing it

    pub fn open_menu(&mut self, items: Vec<MenuItem>, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some((items, at));
        cx.notify();
    }

    pub fn run_act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.menu.take().map(|m| m.1);
        let repo = self.repo.clone();
        match act {
            Act::None => {}
            Act::Git(args) => repo.update(cx, |r, cx| r.run_git(args, cx)),
            Act::Steps(label, steps) => repo.update(cx, |r, cx| {
                let kind = steps.first().and_then(|a| git::undo::kind_of_args(a));
                r.run_op_as(kind, &label, cx, move |root| {
                    let mut out = String::new();
                    for args in &steps {
                        out = git::run(root, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
                    }
                    Ok(out)
                })
            }),
            Act::Ask { title, detail, buttons } => {
                let tree = cx.entity().downgrade();
                let steps: Vec<Vec<Vec<String>>> = buttons.iter().map(|b| b.1.clone()).collect();
                let label = title.trim_end_matches('?').to_string();
                cx.emit(Request::Ask(crate::dialog::Ask {
                    title,
                    detail,
                    buttons: buttons.into_iter().map(|b| b.0).collect(),
                    danger: true,
                    cancel_default: false,
                    cancel_label: None,
                    output: None,
                    on_answer: Rc::new(move |ix, window, cx| {
                        let Some(s) = steps.get(ix).cloned() else { return };
                        let label = label.clone();
                        tree.update(cx, |t, cx| t.run_act(Act::Steps(label, s), window, cx)).ok();
                    }),
                }));
            }
            Act::Form(kind) => self.open_form(kind, at.unwrap_or_else(|| window.bounds().center()), window, cx),
            Act::Inline(kind) => self.start_inline(kind, window, cx),
            Act::Commit(sha) => cx.emit(Request::OpenCommit(sha)),
            Act::Compare(other) => cx.emit(Request::Compare { base: None, other }),
            Act::CompareTwo(a, b) => {
                // The older one is the base, so the comparison shows what the newer one adds.
                let root = repo.read(cx).root.clone();
                let a_is_newer = git::run(&root, &["merge-base", "--is-ancestor", &b, &a]).is_ok();
                let (base, other) = if a_is_newer { (b, a) } else { (a, b) };
                cx.emit(Request::Compare { base: Some(base), other });
            }
            Act::Mark(sha) => {
                cx.emit(Request::Message(format!("{} marked: right-click another commit to compare them", &sha[..sha.len().min(7)])));
                self.marked = Some(sha);
            }
            Act::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.emit(Request::Message("Copied".into()));
            }
            Act::Window(path) => crate::open_window(path, cx),
            Act::Rebase(sha) => self.open_rebase(&sha, cx),
            Act::Fixup(sha, confirmed) => {
                let r = repo.read(cx);
                let root = r.root.clone();
                if r.status.staged.is_empty() {
                    cx.emit(Request::Error("Stage the changes you want to fold in first".into()));
                } else if !confirmed && git::undo::on_remote(&root, &sha) {
                    let tree = cx.entity().downgrade();
                    let short = sha[..sha.len().min(7)].to_string();
                    cx.emit(Request::Ask(crate::dialog::Ask {
                        title: format!("{short} is already on a remote. Fold into it anyway?"),
                        detail: "This rewrites it and every commit after it. Pushing the result takes a force push, and anyone who has the old commits is affected. Undo puts it back here.".into(),
                        buttons: vec!["Fold In Anyway".into()],
                        danger: true,
                        cancel_default: true,
                        cancel_label: None,
                        output: None,
                        on_answer: Rc::new(move |_, window, cx| {
                            tree.update(cx, |t, cx| t.run_act(Act::Fixup(sha.clone(), true), window, cx)).ok();
                        }),
                    }));
                } else {
                    repo.update(cx, |r, cx| r.run_op("Fix up", cx, move |root| git::undo::fixup(root, &sha)));
                }
            }
            Act::Flow(kind, action, name) => repo.update(cx, |r, cx| r.run_op("Git Flow", cx, move |root| git::flow(root, &kind, &action, &name))),
            Act::OpenFile(path) => cx.emit(Request::OpenFile { path, line: None, preview: false }),
            Act::History(path) => cx.emit(Request::FileHistory(path)),
            Act::MergeEditor(path) => cx.emit(Request::OpenMergeEditor(path)),
            Act::SavePatch(commit, dir) => repo.update(cx, |r, cx| {
                r.run_op("Save patch", cx, move |root| {
                    let out = git::run(root, &["format-patch", "-1", &commit, "-o", &dir.to_string_lossy()])?;
                    Ok(format!("Saved {}", out.trim()))
                })
            }),
        }
        cx.notify();
    }

    fn input(text: &str, placeholder: &str, cx: &mut Context<Self>) -> Entity<Editor> {
        cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.font_size = 12.;
            ed.placeholder = placeholder.to_string().into();
            ed.set_text(text, cx);
            // Typing replaces what's there (a name being renamed).
            let len = ed.buf.len();
            ed.select_range(0..len, cx);
            ed
        })
    }

    fn start_inline(&mut self, kind: InlineKind, window: &mut Window, cx: &mut Context<Self>) {
        let (text, hint) = match &kind {
            InlineKind::RenameBranch(b) => (b.clone(), "New name"),
            InlineKind::RenameRemote(r) => (r.clone(), "New name"),
            InlineKind::NewBranch => (String::new(), "New branch name, then Enter"),
            InlineKind::StashSave => (String::new(), "What you're putting aside, then Enter"),
        };
        if let InlineKind::NewBranch = kind {
            self.folded.remove("local");
        }
        let input = Self::input(&text, hint, cx);
        self._subs.push(cx.subscribe_in(&input, window, |t, _, ev: &EditorEvent, window, cx| match ev {
            EditorEvent::Submit => t.finish_inline(window, cx),
            EditorEvent::Cancel => {
                t.inline = None;
                cx.notify();
            }
            _ => {}
        }));
        window.focus(&input.read(cx).focus_target(cx), cx);
        self.inline = Some(Inline { kind, input });
        cx.notify();
    }

    fn finish_inline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Inline { kind, input }) = self.inline.take() else { return };
        let text = input.read(cx).text().trim().to_string();
        let act = match (&kind, text.is_empty()) {
            (InlineKind::StashSave, true) => git_act(["stash", "push", "--include-untracked"]),
            (_, true) => Act::None,
            (InlineKind::RenameBranch(b), _) if *b == text => Act::None,
            (InlineKind::RenameBranch(b), _) => git_act(["branch", "-m", b, &text]),
            (InlineKind::RenameRemote(r), _) => git_act(["remote", "rename", r, &text]),
            (InlineKind::NewBranch, _) => git_act(["switch", "-c", &text]),
            (InlineKind::StashSave, _) => git_act(["stash", "push", "--include-untracked", "-m", &text]),
        };
        self.run_act(act, window, cx);
    }

    fn open_form(&mut self, kind: FormKind, _at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let remote = self.first_remote(cx);
        let root = self.repo.read(cx).root.clone();
        let short = |s: &str| if s.len() == 40 { s[..7].to_string() } else { s.to_string() };
        // (title, fields as (label, starting text, hint), check boxes, button, what it runs)
        type Run = Rc<dyn Fn(&[String], &[bool]) -> Result<Act, String>>;
        let (title, fields, checks, submit, run): (String, Vec<(&str, String, &str)>, Vec<(String, bool)>, &str, Run) = match kind {
            FormKind::NewBranch { start } => {
                let s = start.clone();
                (
                    format!("New branch from {}", short(&start)),
                    vec![("Name", String::new(), "feature/…")],
                    vec![("Check it out".into(), true)],
                    "Create Branch",
                    Rc::new(move |v, c| {
                        let name = need(&v[0], "a name")?;
                        Ok(if c[0] { git_act(["switch", "-c", &name, &s]) } else { git_act(["branch", &name, &s]) })
                    }),
                )
            }
            FormKind::Tag { at: target } => {
                let r = remote.clone();
                let t = target.clone();
                let mut checks = Vec::new();
                if let Some(r) = &remote {
                    checks.push((format!("Push it to {r}"), false));
                }
                (
                    format!("Tag {}", short(&target)),
                    vec![("Name", String::new(), "v1.0.0"), ("Message", String::new(), "Optional: makes an annotated tag")],
                    checks,
                    "Create Tag",
                    Rc::new(move |v, c| {
                        let name = need(&v[0], "a name")?;
                        let mut steps = vec![if v[1].trim().is_empty() { cmd(["tag", &name, &t]) } else { cmd(["tag", "-a", &name, "-m", v[1].trim(), &t]) }];
                        if let (Some(r), Some(true)) = (&r, c.first()) {
                            steps.push(cmd(["push", r, &format!("refs/tags/{name}")]));
                        }
                        Ok(Act::Steps(format!("Tag {name}"), steps))
                    }),
                )
            }
            FormKind::RemoteAdd => (
                "Add a remote".into(),
                vec![("Name", if remote.is_none() { "origin".into() } else { String::new() }, "origin"), ("URL", String::new(), "git@github.com:you/repo.git")],
                vec![("Fetch it now".into(), true)],
                "Add Remote",
                Rc::new(|v, c| {
                    let (name, url) = (need(&v[0], "a name")?, need(&v[1], "a URL")?);
                    let mut steps = vec![cmd(["remote", "add", &name, &url])];
                    if c[0] {
                        steps.push(cmd(["fetch", &name]));
                    }
                    Ok(Act::Steps(format!("Add {name}"), steps))
                }),
            ),
            FormKind::RemoteUrl { remote: r, url } => (
                format!("{r}'s URL"),
                vec![("URL", url, "")],
                vec![],
                "Save",
                Rc::new(move |v, _| Ok(git_act(["remote", "set-url", &r, &need(&v[0], "a URL")?]))),
            ),
            FormKind::Upstream { branch } => {
                let guess = remote.as_ref().map(|r| format!("{r}/{branch}")).unwrap_or_default();
                (
                    format!("Upstream of {branch}"),
                    vec![("Remote branch", guess, "origin/main")],
                    vec![],
                    "Track It",
                    Rc::new(move |v, _| Ok(git_act(["branch", "--set-upstream-to", &need(&v[0], "a remote branch")?, &branch]))),
                )
            }
            FormKind::StashBranch { stash } => (
                format!("New branch from {stash}"),
                vec![("Name", String::new(), "")],
                vec![],
                "Create Branch",
                Rc::new(move |v, _| Ok(git_act(["stash", "branch", &need(&v[0], "a name")?, &stash]))),
            ),
            FormKind::WorktreeAdd => {
                let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let refs = self.repo.read(cx).refs.clone();
                (
                    "New worktree".into(),
                    vec![("Folder", format!("../{name}-"), "../folder"), ("Branch", String::new(), "Optional: an existing or new branch")],
                    vec![],
                    "Add Worktree",
                    Rc::new(move |v, _| {
                        let dir = need(&v[0], "a folder")?;
                        let b = v[1].trim();
                        Ok(match b {
                            "" => git_act(["worktree", "add", &dir]),
                            b if refs.local.iter().any(|l| l.name == b) => git_act(["worktree", "add", &dir, b]),
                            b => git_act(["worktree", "add", "-b", b, &dir]),
                        })
                    }),
                )
            }
            FormKind::SubmoduleAdd => (
                "Add a submodule".into(),
                vec![("URL", String::new(), "https://…"), ("Folder", String::new(), "Optional")],
                vec![],
                "Add Submodule",
                Rc::new(|v, _| {
                    let url = need(&v[0], "a URL")?;
                    Ok(if v[1].trim().is_empty() { git_act(["submodule", "add", &url]) } else { git_act(["submodule", "add", &url, v[1].trim()]) })
                }),
            ),
            FormKind::FlowStart { kind } => (
                format!("Start a {kind}"),
                vec![("Name", String::new(), if kind == "feature" { "login-page" } else { "1.2.0" })],
                vec![],
                "Start",
                Rc::new(move |v, _| Ok(Act::Flow(kind.clone(), "start".into(), need(&v[0], "a name")?))),
            ),
            FormKind::LfsTrack => ("Track files with LFS".into(), vec![("Pattern", String::new(), "*.psd")], vec![], "Track", Rc::new(|v, _| Ok(git_act(["lfs", "track", &need(&v[0], "a pattern")?])))),
            FormKind::PatchSave { commit } => {
                let root = root.clone();
                (
                    format!("Save {} as a patch", short(&commit)),
                    vec![("Folder", root.to_string_lossy().to_string(), "")],
                    vec![],
                    "Save",
                    Rc::new(move |v, _| Ok(Act::SavePatch(commit.clone(), root.join(need(&v[0], "a folder")?)))),
                )
            }
            FormKind::PatchApply => {
                let root = root.clone();
                (
                    "Apply a patch".into(),
                    vec![("File", String::new(), "changes.patch")],
                    vec![],
                    "Apply",
                    Rc::new(move |v, _| {
                        let file = root.join(need(&v[0], "a file")?);
                        // A mail-style patch (git format-patch) becomes a commit; a plain diff only changes files.
                        let mail = std::fs::read_to_string(&file).map_err(|e| format!("Can't read it: {e}"))?.starts_with("From ");
                        let f = file.to_string_lossy().to_string();
                        Ok(if mail { git_act(["am", "--3way", &f]) } else { git_act(["apply", "--3way", &f]) })
                    }),
                )
            }
        };
        let fields: Vec<(String, Entity<Editor>)> = fields.into_iter().map(|(label, text, hint)| (label.to_string(), Self::input(&text, hint, cx))).collect();
        for (_, ed) in &fields {
            self._subs.push(cx.subscribe_in(ed, window, |t, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => t.submit_form(window, cx),
                EditorEvent::Cancel => {
                    t.form = None;
                    cx.notify();
                }
                _ => {}
            }));
        }
        if let Some((_, first)) = fields.first() {
            window.focus(&first.read(cx).focus_target(cx), cx);
        }
        self.form_error = None;
        self.form = Some(Form { title, fields, checks, submit: submit.to_string(), run });
        cx.notify();
    }

    pub fn submit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let values: Vec<String> = form.fields.iter().map(|(_, e)| e.read(cx).text()).collect();
        let checks: Vec<bool> = form.checks.iter().map(|c| c.1).collect();
        match (form.run)(&values, &checks) {
            Ok(act) => {
                self.form = None;
                self.run_act(act, window, cx);
            }
            Err(e) => {
                self.form_error = Some(e);
                cx.notify();
            }
        }
    }

    // ------------------------------------------------------------ drawing

    fn render_menu(&self, items: Vec<MenuItem>, vp: Size<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("ref-menu")
            .occlude()
            .w(px(300.))
            .max_h(vp.height * 0.9)
            .overflow_y_scroll()
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(8.))
            .shadow_lg()
            .text_size(px(12.))
            .on_mouse_down_out(cx.listener(|t, _, _, cx| {
                t.menu = None;
                cx.notify();
            }))
            .children(items.into_iter().enumerate().map(|(i, it)| {
                if it.label.is_empty() {
                    return div().my(px(4.)).h(px(1.)).bg(theme::border()).into_any_element();
                }
                if it.style == Style::Title {
                    return div().px(px(14.)).py(px(4.)).text_color(theme::fg_muted()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(it.label).into_any_element();
                }
                let selector = format!("menu-{}", it.label);
                let act = it.act.clone();
                div()
                    .id(("menu-item", i))
                    .debug_selector(move || selector)
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .px(px(10.))
                    .mx(px(4.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .hover(|s| s.bg(theme::selected_focused()))
                    .when(it.style == Style::Danger, |d| d.text_color(theme::git_deleted()))
                    .on_click(cx.listener(move |t, _, w, cx| t.run_act(act.clone(), w, cx)))
                    .child(it.label)
                    .into_any_element()
            }))
    }

    fn render_form(&self, vp: Size<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(form) = &self.form else { return div().into_any_element() };
        let button = |id: &'static str, label: String, primary: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_none()
                .px(px(12.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .when(primary, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).font_weight(FontWeight::SEMIBOLD).hover(|s| s.bg(theme::accent_hover())))
                .when(!primary, |d| d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())))
                .child(label)
        };
        let dialog = div()
            .id("git-form")
            .occlude()
            .w(px(420.))
            .max_w(vp.width - gpui::px(24.))
            .max_h(vp.height * 0.82)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(16.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(10.))
            .shadow_lg()
            .text_size(px(12.))
            .on_mouse_down_out(cx.listener(|t, _, _, cx| {
                t.form = None;
                cx.notify();
            }))
            .child(div().text_size(px(14.)).text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).overflow_hidden().text_ellipsis().whitespace_nowrap().child(form.title.clone()))
            .children(form.fields.iter().enumerate().map(|(i, (label, ed))| {
                let selector = format!("form-field-{label}");
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_color(theme::fg_muted()).child(label.clone()))
                    .child(
                        div()
                            .id(("form-field", i))
                            .debug_selector(move || selector)
                            .px(px(8.))
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(theme::bg_input())
                            .border_1()
                            .border_color(theme::border_input())
                            .overflow_hidden()
                            .child(ed.clone()),
                    )
            }))
            .children(form.checks.iter().enumerate().map(|(i, (label, on))| {
                let selector = format!("form-check-{label}");
                div()
                    .id(("form-check", i))
                    .debug_selector(move || selector)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |t, _, _, cx| {
                        if let Some(c) = t.form.as_mut().and_then(|f| f.checks.get_mut(i)) {
                            c.1 = !c.1;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .border_1()
                            .border_color(theme::border_input())
                            .when(*on, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).child("✓")),
                    )
                    .child(label.clone())
            }))
            .when_some(self.form_error.clone(), |d, e| d.child(div().text_color(theme::git_deleted()).child(e)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(6.))
                    .child(button("form-cancel", "Cancel".into(), false).on_click(cx.listener(|t, _, _, cx| {
                        t.form = None;
                        cx.notify();
                    })))
                    .child(button("form-submit", form.submit.clone(), true).on_click(cx.listener(|t, _, w, cx| t.submit_form(w, cx)))),
            );
        ui::dialog_scrim(vp, dialog).into_any_element()
    }

    fn render_row(&self, i: usize, row: Row, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.clone();
        let base = |id: String, key: String, depth: usize| {
            let selector = id.clone();
            let on = selected.as_deref() == Some(&key);
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .group("ref-row")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .h(px(22.))
                .pl(px(10. + depth as f32 * 12.))
                .pr(px(8.))
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected()))
                .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                .overflow_hidden()
                .whitespace_nowrap()
        };
        let name = |text: String| div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(text);
        // Branch, remote and tag names are code, not chrome.
        let mono = |text: String| div().flex_1().min_w_0().overflow_hidden().text_ellipsis().font_family(theme::MONO_FONT).child(text);
        let detail = |text: String| div().flex_none().text_size(px(10.)).text_color(theme::fg_muted()).child(text);
        type MenuFn = Rc<dyn Fn(&RefsTree, &App) -> Vec<MenuItem>>;
        // A "…" on hover opens the same menu as a right-click.
        let with_menu = |el: Stateful<Div>, key: String, menu: MenuFn, cx: &mut Context<Self>| {
            let (m2, k2) = (menu.clone(), key.clone());
            el.on_mouse_down(MouseButton::Right, cx.listener(move |t, ev: &MouseDownEvent, _, cx| {
                t.selected = Some(k2.clone());
                let items = m2(t, cx);
                t.open_menu(items, ev.position, cx);
            }))
            .child(
                div().id("ref-more").flex_none().invisible().group_hover("ref-row", |s| s.visible()).child(ui::icon_button(("ref-more-btn", i), "ellipsis").size(px(18.)).on_click(cx.listener(
                    move |t, ev: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        t.selected = Some(key.clone());
                        let items = menu(t, cx);
                        t.open_menu(items, ev.position(), cx);
                    },
                ))),
            )
        };
        // The row being renamed, or a new name's row: a text box in its place.
        let editing = |key: &str| self.inline.as_ref().filter(|i| i.kind.key() == key).map(|i| i.input.clone());
        let edit_row = |id: String, depth: usize, icon: &str, input: Entity<Editor>| {
            div()
                .id(SharedString::from(id.clone()))
                .debug_selector(move || id)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .h(px(24.))
                .pl(px(10. + depth as f32 * 12.))
                .pr(px(8.))
                .child(ui::small_icon(icon, theme::fg_dim()))
                .child(div().flex_1().min_w_0().h(px(20.)).px(px(4.)).flex().items_center().rounded(px(3.)).bg(theme::bg_input()).border_1().border_color(theme::focus_border()).overflow_hidden().child(input))
                .into_any_element()
        };
        match row {
            Row::New(group) => {
                let input = self.inline.as_ref().map(|i| i.input.clone()).expect("a new row has its input");
                return edit_row(format!("ref-new-{group}"), 1, if group == "stashes" { "paperclip" } else { "git" }, input);
            }
            Row::Group { key, title, count, open } => {
                let menu: MenuFn = Rc::new(move |t, cx| t.group_menu(key, cx));
                let add: Option<(&str, Act)> = match key {
                    "local" => Some(("New branch", Act::Inline(InlineKind::NewBranch))),
                    "remotes" => Some(("Add remote", Act::Form(FormKind::RemoteAdd))),
                    "tags" => Some(("New tag on HEAD", Act::Form(FormKind::Tag { at: "HEAD".into() }))),
                    "stashes" => Some(("Stash changes", Act::Inline(InlineKind::StashSave))),
                    "worktrees" => Some(("New worktree", Act::Form(FormKind::WorktreeAdd))),
                    _ => None,
                };
                with_menu(
                    base(format!("refs-group-{key}"), format!("group:{key}"), 0)
                        .on_click(cx.listener(move |t, _, _, cx| {
                            if !t.folded.remove(key) {
                                t.folded.insert(key.into());
                            }
                            cx.notify();
                        }))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::fg_dim())
                        .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_dim()).size(px(12.)))
                        .child(name(title.to_string()))
                        .when(key != "lfs", |d| d.child(detail(count.to_string())))
                        .when_some(add, |d, (tip, act)| {
                            d.child(
                                ui::icon_button(SharedString::from(format!("refs-add-{key}")), "plus")
                                    .debug_selector(move || format!("refs-add-{key}"))
                                    .size(px(18.))
                                    .tooltip(ui::tooltip(tip))
                                    .on_click(cx.listener(move |t, ev: &ClickEvent, w, cx| {
                                        cx.stop_propagation();
                                        t.menu = Some((Vec::new(), ev.position()));
                                        t.run_act(act.clone(), w, cx);
                                    })),
                            )
                        }),
                    format!("group:{key}"),
                    menu,
                    cx,
                )
            }
            Row::Local(b) => {
                let key = format!("local:{}", b.name);
                if let Some(input) = editing(&key) {
                    return edit_row(format!("ref-rename-{}", b.name), 1, "git", input);
                }
                let n = b.name.clone();
                let sync = match (b.gone, b.ahead, b.behind) {
                    (true, ..) => "gone".to_string(),
                    (_, 0, 0) => String::new(),
                    (_, a, 0) => format!("↑{a}"),
                    (_, 0, z) => format!("↓{z}"),
                    (_, a, z) => format!("↑{a} ↓{z}"),
                };
                let tip = b.upstream.clone().map(|u| format!("{n} → {u} · double-click to check out")).unwrap_or_else(|| format!("{n} (no upstream) · double-click to check out"));
                let b2 = b.clone();
                let menu: MenuFn = Rc::new(move |t, cx| t.local_menu(&b2, cx));
                let k2 = key.clone();
                with_menu(
                    base(format!("ref-local-{n}"), key.clone(), 1)
                        .tooltip(ui::tooltip_text(tip))
                        .when(b.head, |d| d.font_weight(FontWeight::BOLD).text_color(theme::fg_bright()))
                        .on_click(cx.listener(move |t, ev: &ClickEvent, w, cx| {
                            t.selected = Some(k2.clone());
                            if ev.click_count() >= 2 && !b.head {
                                t.run_act(git_act(["switch", &n]), w, cx);
                            } else {
                                cx.emit(Reveal(format!("refs/heads/{n}")));
                            }
                            cx.notify();
                        }))
                        .child(ui::small_icon(if b.head { "target" } else { "git" }, theme::ref_local()))
                        .child(mono(b.name.clone()))
                        .child(detail(sync)),
                    key,
                    menu,
                    cx,
                )
            }
            Row::Remote(r, count) => {
                let key = format!("remote:{r}");
                if let Some(input) = editing(&key) {
                    return edit_row(format!("ref-rename-{r}"), 1, "cloud", input);
                }
                let open = !self.folded.contains(&key);
                let r2 = r.clone();
                let menu: MenuFn = Rc::new(move |t, cx| t.remote_menu(&r2, cx));
                let k2 = key.clone();
                with_menu(
                    base(format!("ref-remote-{r}"), key.clone(), 1)
                        .on_click(cx.listener(move |t, _, _, cx| {
                            if !t.folded.remove(&k2) {
                                t.folded.insert(k2.clone());
                            }
                            cx.notify();
                        }))
                        .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_dim()).size(px(12.)))
                        .child(ui::small_icon("cloud", theme::ref_remote()))
                        .child(mono(r.clone()))
                        .child(detail(count.to_string())),
                    key,
                    menu,
                    cx,
                )
            }
            Row::RemoteBranch(r, b) => {
                let (r2, b2) = (r.clone(), b.clone());
                let menu: MenuFn = Rc::new(move |t, cx| t.remote_branch_menu(&r2, &b2, cx));
                let full = format!("{r}/{b}");
                let key = format!("rb:{full}");
                let k2 = key.clone();
                with_menu(
                    base(format!("ref-remote-branch-{full}"), key.clone(), 2)
                        .tooltip(ui::tooltip_text(format!("{full} · double-click to compare with the current branch")))
                        .on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| {
                            t.selected = Some(k2.clone());
                            if ev.click_count() >= 2 {
                                cx.emit(Request::Compare { base: None, other: full.clone() });
                            } else {
                                cx.emit(Reveal(format!("refs/remotes/{full}")));
                            }
                            cx.notify();
                        }))
                        .child(ui::small_icon("git", theme::ref_remote()))
                        .child(mono(b)),
                    key,
                    menu,
                    cx,
                )
            }
            Row::Tag(t) => {
                let t2 = t.clone();
                let menu: MenuFn = Rc::new(move |tr, cx| tr.tag_menu(&t2, cx));
                let t3 = t.clone();
                let key = format!("tag:{t}");
                let k2 = key.clone();
                with_menu(
                    base(format!("ref-tag-{t}"), key.clone(), 1)
                        .on_click(cx.listener(move |tr, ev: &ClickEvent, _, cx| {
                            tr.selected = Some(k2.clone());
                            if ev.click_count() >= 2 {
                                cx.emit(Request::OpenCommit(t3.clone()));
                            } else {
                                cx.emit(Reveal(format!("refs/tags/{t3}^{{commit}}")));
                            }
                            cx.notify();
                        }))
                        .child(ui::small_icon("tag", theme::ref_tag()))
                        .child(mono(t)),
                    key,
                    menu,
                    cx,
                )
            }
            Row::Stash(s) => {
                let s2 = s.clone();
                let menu: MenuFn = Rc::new(move |t, _| t.stash_menu(&s2));
                let sha = s.sha.clone();
                let key = format!("stash:{}", s.name);
                let k2 = key.clone();
                with_menu(
                    base(format!("ref-stash-{}", s.name), key.clone(), 1)
                        .tooltip(ui::tooltip_text(format!("{}: {} · double-click to see it", s.name, s.subject)))
                        .on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| {
                            t.selected = Some(k2.clone());
                            if ev.click_count() >= 2 {
                                cx.emit(Request::OpenCommit(sha.clone()));
                            }
                            cx.notify();
                        }))
                        .child(ui::small_icon("paperclip", theme::fg_dim()))
                        .child(name(stash_label(&s.subject)))
                        .child(detail(s.name.clone())),
                    key,
                    menu,
                    cx,
                )
            }
            Row::Worktree(w) => {
                let w2 = w.clone();
                let menu: MenuFn = Rc::new(move |t, _| t.worktree_menu(&w2));
                let folder = w.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                let what = match (&w.branch, w.main) {
                    (_, true) => "this folder".to_string(),
                    (Some(b), _) => b.clone(),
                    (None, _) => w.sha[..w.sha.len().min(7)].to_string(),
                };
                let path = w.path.clone();
                let key = format!("wt:{folder}");
                with_menu(
                    base(format!("ref-worktree-{folder}"), key.clone(), 1)
                        .tooltip(ui::tooltip_text(format!("{} · double-click to open it", w.path.display())))
                        .on_click(cx.listener(move |_, ev: &ClickEvent, _, cx| {
                            if ev.click_count() >= 2 {
                                crate::open_window(path.clone(), cx);
                            }
                        }))
                        .child(ui::small_icon("folder", theme::fg_dim()))
                        .child(name(folder))
                        .child(detail(if w.locked { format!("{what} · locked") } else { what })),
                    key,
                    menu,
                    cx,
                )
            }
            Row::Submodule(m) => {
                let m2 = m.clone();
                let menu: MenuFn = Rc::new(move |t, cx| t.submodule_menu(&m2, cx));
                let state = match m.state {
                    '-' => "not set up",
                    '+' => "moved",
                    'U' => "conflict",
                    _ => "",
                };
                let key = format!("sub:{}", m.path);
                with_menu(
                    base(format!("ref-submodule-{}", m.path), key.clone(), 1)
                        .child(ui::small_icon("repo", theme::fg_dim()))
                        .child(name(m.path.clone()))
                        .child(detail(if state.is_empty() { m.sha[..m.sha.len().min(7)].to_string() } else { state.to_string() })),
                    key,
                    menu,
                    cx,
                )
            }
            Row::Note(text) => base(format!("ref-note-{i}"), String::new(), 1).text_color(theme::fg_muted()).child(name(text)),
            Row::More(n) => base(format!("ref-more-{i}"), String::new(), 2).text_color(theme::fg_muted()).child(format!("…and {n} more (the graph's filter and search reach them)")),
        }
        .into_any_element()
    }
}

impl Render for RefsTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vp = window.viewport_size();
        let rows = if self.show_tree { self.rows(cx) } else { Vec::new() };
        div()
            .id("refs-tree")
            .flex()
            .flex_col()
            .when(self.show_tree, |d| d.size_full())
            .when_some(self.max_h.filter(|_| self.show_tree), |d, h| d.max_h(px(h)))
            .overflow_y_scroll()
            .text_size(px(12.))
            .children(rows.into_iter().enumerate().map(|(i, row)| self.render_row(i, row, cx)))
            .when_some(self.menu.clone().filter(|m| !m.0.is_empty()), |d, (items, pos)| d.child(deferred(anchored().position(pos).snap_to_window().child(self.render_menu(items, vp, cx))).with_priority(3)))
            .when(self.form.is_some(), |d| d.child(deferred(anchored().position(Point::default()).child(self.render_form(vp, cx))).with_priority(3)))
            .when(self.rebase.is_some(), |d| d.child(deferred(anchored().position(Point::default()).child(self.render_rebase(vp, cx))).with_priority(2)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stash_labels_drop_the_branch_and_hash() {
        assert_eq!(super::stash_label("On main: notes"), "notes");
        assert_eq!(super::stash_label("WIP on main: 1a2b3c4 Cart starts"), "WIP: Cart starts");
        assert_eq!(super::stash_label("odd"), "odd");
    }
}
