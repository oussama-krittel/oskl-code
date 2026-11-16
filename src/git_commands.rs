//! The `:` commands for git (rename, upstream, tags, remotes, stashes, history, search,
//! patches, worktrees, git-flow…) for keyboard use; the menus and dialogs do the same.

use super::GitPanel;
use super::refs::{Act, cmd, git_act};
use crate::git;
use crate::request::Request;
use gpui::*;
use std::path::PathBuf;

/// The commands below, for the command line's list and completions: (name, help, takes args).
pub const COMMANDS: &[(&str, &str, bool)] = &[
    ("repo", "The Git tab: history, branches, tags and stashes, and each commit's changes", false),
    ("branch-rename", "Rename a branch: branch-rename <old> <new> (one name renames the current one)", true),
    ("upstream", "A branch's upstream: upstream <branch> <remote/branch>, or upstream <branch> none", true),
    ("tag", "Tag HEAD: tag <name> [message] (a message makes an annotated tag)", true),
    ("tag-at", "Tag a commit: tag-at <commit> <name> [message]", true),
    ("remote-add", "Add a remote and fetch it: remote-add <name> <url>", true),
    ("remote-rename", "Rename a remote: remote-rename <old> <new>", true),
    ("remote-url", "Change a remote's URL: remote-url <name> <url>", true),
    ("stash-save", "Stash with a message: stash-save [--staged | --keep-index] <message>", true),
    ("stash-branch", "A branch from a stash: stash-branch <stash@{n}> <name>", true),
    ("history", "History of a file in the graph: history [path] (the current file; history off to go back)", true),
    ("find", "Search the graph by message, author or hash: find <text> (find alone shows everything again)", true),
    ("rebase-i", "Interactive rebase from a commit: rebase-i <commit>", true),
    ("patch-save", "Save a commit as a .patch file: patch-save <commit> [folder]", true),
    ("patch-apply", "Apply a .patch or .diff file: patch-apply <file>", true),
    ("take", "Resolve a conflicted file with one side: take ours|theirs <path>", true),
    ("worktree-add", "A worktree: worktree-add <folder> [branch] (a new branch if it doesn't exist)", true),
    ("submodule-add", "Add a submodule: submodule-add <url> [folder]", true),
    ("flow", "git-flow: flow init · flow feature|release|hotfix start|finish <name>", true),
    ("lfs-track", "Track files with Git LFS: lfs-track <pattern>", true),
    ("sign", "Sign commits and tags in this repository: sign on | off", true),
];

impl GitPanel {
    /// Runs one of `COMMANDS`. Returns whether the Source Control panel should show (the
    /// command changed what the graph lists).
    pub fn run_command(&mut self, name: &str, rest: &str, active: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let words: Vec<&str> = rest.split_whitespace().collect();
        let tail = |n: usize| words.get(n..).map(|w| w.join(" ")).unwrap_or_default();
        let usage = |cx: &mut Context<Self>| {
            let help = COMMANDS.iter().find(|c| c.0 == name).map(|c| c.1).unwrap_or("");
            cx.emit(Request::Error(help.to_string()));
            false
        };
        let root = self.repo.read(cx).root.clone();
        let act = match (name, &words[..]) {
            ("branch-rename", [new]) => git_act(["branch", "-m", new]),
            ("branch-rename", [old, new]) => git_act(["branch", "-m", old, new]),
            ("upstream", [b, "none"]) => git_act(["branch", "--unset-upstream", b]),
            ("upstream", [b, up]) => git_act(["branch", "--set-upstream-to", up, b]),
            ("tag", [t]) => git_act(["tag", t]),
            ("tag", [t, ..]) => git_act(["tag", "-a", t, "-m", &tail(1)]),
            ("tag-at", [at, t]) => git_act(["tag", t, at]),
            ("tag-at", [at, t, ..]) => git_act(["tag", "-a", t, "-m", &tail(2), at]),
            ("remote-add", [r, url]) => Act::Steps(format!("Add {r}"), vec![cmd(["remote", "add", r, url]), cmd(["fetch", r])]),
            ("remote-rename", [a, b]) => git_act(["remote", "rename", a, b]),
            ("remote-url", [r, url]) => git_act(["remote", "set-url", r, url]),
            ("stash-save", [flag @ ("--staged" | "--keep-index"), ..]) => git_act(["stash", "push", flag, "-m", &tail(1)]),
            ("stash-save", _) => git_act(["stash", "push", "--include-untracked", "-m", rest.trim()]),
            ("stash-branch", [s, b]) => git_act(["stash", "branch", b, s]),
            ("take", [side @ ("ours" | "theirs"), path]) => Act::Steps(format!("Take {side}"), vec![cmd(["checkout", &format!("--{side}"), "--", path]), cmd(["add", "--", path])]),
            ("worktree-add", [dir]) => git_act(["worktree", "add", dir]),
            ("worktree-add", [dir, b]) => {
                let exists = self.repo.read(cx).refs.local.iter().any(|l| l.name == *b);
                if exists { git_act(["worktree", "add", dir, b]) } else { git_act(["worktree", "add", "-b", b, dir]) }
            }
            ("submodule-add", [url]) => git_act(["submodule", "add", url]),
            ("submodule-add", [url, dir]) => git_act(["submodule", "add", url, dir]),
            ("lfs-track", [_, ..]) => git_act(["lfs", "track", rest.trim()]),
            ("sign", [on @ ("on" | "off")]) => {
                let v = if *on == "on" { "true" } else { "false" };
                Act::Steps(format!("Signing {on}"), vec![cmd(["config", "commit.gpgsign", v]), cmd(["config", "tag.gpgsign", v])])
            }
            ("flow", ["init"]) => Act::Flow(String::new(), "init".into(), String::new()),
            ("flow", [kind, action, n]) => Act::Flow(kind.to_string(), action.to_string(), n.to_string()),
            ("rebase-i", [c]) => Act::Rebase(c.to_string()),
            ("patch-save", [c, ..]) => {
                let dir = words.get(1).map(|d| root.join(d)).unwrap_or(root.clone());
                let c = c.to_string();
                self.repo.update(cx, |r, cx| {
                    r.run_op("Save patch", cx, move |root| {
                        let out = git::run(root, &["format-patch", "-1", &c, "-o", &dir.to_string_lossy()])?;
                        Ok(format!("Saved {}", out.trim()))
                    })
                });
                return false;
            }
            ("patch-apply", [_, ..]) => {
                let file = root.join(rest.trim());
                // A mail-style patch (git format-patch) becomes a commit; a plain diff only changes files.
                let mail = std::fs::read_to_string(&file).is_ok_and(|t| t.starts_with("From "));
                let f = file.to_string_lossy().to_string();
                if mail { git_act(["am", "--3way", &f]) } else { git_act(["apply", "--3way", &f]) }
            }
            ("history", ["off"]) => return self.show_history(None, cx),
            ("history", []) => {
                let rel = active.and_then(|p| p.strip_prefix(&root).ok().map(|r| r.to_string_lossy().to_string()));
                match rel {
                    Some(rel) => return self.show_history(Some(rel), cx),
                    None => {
                        cx.emit(Request::Error("Open a file of this repository, or say which: history <path>".into()));
                        return false;
                    }
                }
            }
            ("history", _) => return self.show_history(Some(rest.trim().to_string()), cx),
            ("find", _) => return self.search_graph(rest.trim(), window, cx),
            ("repo", _) => {
                cx.emit(Request::OpenGitTab(root));
                return false;
            }
            _ => return usage(cx),
        };
        self.refs.update(cx, |r, cx| r.run_act(act, window, cx));
        false
    }

    /// The graph lists one file's history (None: the usual graph again).
    pub fn show_history(&mut self, path: Option<String>, cx: &mut Context<Self>) -> bool {
        self.graph_open = true;
        self.expanded.clear();
        self.repo.update(cx, |r, cx| {
            r.graph_file = path.map(|p| (p, Default::default()));
            r.refresh_log(cx);
        });
        cx.notify();
        true
    }

    /// The graph lists only commits matching `query` (empty: everything again).
    pub fn search_graph(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.graph_open = true;
        self.search_open = !query.is_empty() || self.search_open;
        if self.search_input.read(cx).text() != query {
            self.search_input.update(cx, |e, cx| e.set_text(query, cx));
        }
        let _ = window;
        self.repo.update(cx, |r, cx| {
            r.graph_search = (!query.is_empty()).then(|| query.to_string());
            r.refresh_log(cx);
        });
        cx.notify();
        true
    }

    /// Completions for the commands' arguments: (label, the whole command line, runs as is).
    pub fn command_items(&self, name: &str, rest: &str, cx: &App) -> Vec<(String, String, bool)> {
        let refs = self.repo.read(cx).refs.clone();
        let words: Vec<&str> = rest.split(' ').collect();
        let line = |v: &str| format!("{name} {}", words[..words.len() - 1].iter().chain([&v]).copied().collect::<Vec<_>>().join(" "));
        let locals = || refs.local.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
        let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The choices for the word being typed, and whether it's the command's last word.
        let (list, last): (Vec<String>, bool) = match (name, words.len()) {
            ("branch-rename" | "upstream", 1) => (locals(), false),
            ("upstream", 2) => (refs.remotes.iter().flat_map(|(r, bs)| bs.iter().map(move |b| format!("{r}/{b}"))).chain(["none".to_string()]).collect(), true),
            ("remote-rename" | "remote-url", 1) => (refs.remotes.iter().map(|r| r.0.clone()).collect(), false),
            ("stash-branch", 1) => (refs.stashes.iter().map(|s| s.name.clone()).collect(), false),
            ("take", 1) => (strs(&["ours", "theirs"]), false),
            ("take", 2) => (self.repo.read(cx).status.conflicts.iter().map(|c| c.path.clone()).collect(), true),
            ("sign", 1) => (strs(&["on", "off"]), true),
            ("history", 1) => (strs(&["off"]), true),
            ("flow", 1) => (strs(&["init", "feature", "release", "hotfix"]), false),
            ("flow", 2) if words[0] != "init" => (strs(&["start", "finish"]), false),
            ("flow", 3) if words[1] == "finish" => (refs.local.iter().filter_map(|b| b.name.strip_prefix(&format!("{}/", words[0])).map(String::from)).collect(), true),
            _ => (Vec::new(), false),
        };
        list.into_iter().map(|v| { let done = last || v == "init"; (v.clone(), if done { line(&v) } else { format!("{} ", line(&v)) }, done) }).collect()
    }
}
