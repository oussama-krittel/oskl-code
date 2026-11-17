//! Git features from the UI, on real temporary repositories: partial staging, the Repository
//! section (branches, remotes, tags, stashes, worktrees, submodules, LFS), reset, interactive
//! rebase, search, file history, comparing commits, patches, conflicts, git-flow, templates and
//! signatures.

use super::{Side, TabKind, Workspace};
use super::ui_tests::{git_out, merge_in_conflict, setup_with};
use crate::git::Rev;
use crate::request::{DiffSpec, Request};
use gpui::{Entity, Modifiers, MouseButton, VisualTestContext};
use std::path::{Path, PathBuf};

fn sh(dir: &Path, cmd: &str) -> String {
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(dir).env("GIT_EDITOR", "true").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string() + &String::from_utf8_lossy(&out.stderr)
}

const INIT: &str = "git init -q -b main && git config user.email t@t && git config user.name t && git config commit.gpgsign false";

fn sel(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn at(cx: &mut VisualTestContext, s: &str) -> gpui::Point<gpui::Pixels> {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(sel(s)).unwrap_or_else(|| panic!("{s} is on screen")).center()
}

fn click(cx: &mut VisualTestContext, s: &str) {
    let p = at(cx, s);
    cx.simulate_click(p, Modifiers::none());
    cx.run_until_parked();
}

fn right_click(cx: &mut VisualTestContext, s: &str) {
    let p = at(cx, s);
    cx.simulate_mouse_down(p, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(p, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
}

/// Right-clicks `row` and picks `item` in the menu that opens.
fn menu(cx: &mut VisualTestContext, row: &str, item: &str) {
    right_click(cx, row);
    click(cx, &format!("menu-{item}"));
}

fn show_git(ws: &Entity<Workspace>, cx: &mut VisualTestContext) {
    ws.update(cx, |ws, cx| {
        ws.side = Side::Git;
        ws.side_visible = true;
        cx.notify();
    });
    cx.run_until_parked();
}

fn command(ws: &Entity<Workspace>, cx: &mut VisualTestContext, line: &str) {
    ws.update_in(cx, |ws, window, cx| ws.run_command(line, window, cx));
    cx.run_until_parked();
}

/// Background git work and the refreshes after it are done.
fn settle(ws: &Entity<Workspace>, cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        ws.update(cx, |ws, cx| ws.git.update(cx, |r, cx| r.refresh_all(cx)));
        cx.run_until_parked();
    }
}

fn root(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> PathBuf {
    ws.read_with(cx, |ws, cx| ws.git.read(cx).root.clone())
}

fn subjects(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    ws.read_with(cx, |ws, cx| ws.git.read(cx).commits.iter().map(|c| c.subject.clone()).collect())
}

fn short(dir: &Path, rev: &str) -> String {
    git_out(dir, &["rev-parse", "--short=7", rev])
}

fn open_repository(ws: &Entity<Workspace>, cx: &mut VisualTestContext) {
    show_git(ws, cx);
    settle(ws, cx);
    click(cx, "section-repository");
}

#[gpui::test]
fn hunks_and_lines_are_staged_unstaged_and_discarded(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "seq 1 10 > a.txt && git add . && git commit -qm base");
        sh(dir, "printf '1\\ntwo\\n3\\n4\\n5\\n6\\n7\\n8\\nnine\\n10\\n' > a.txt");
    });
    cx.run_until_parked();
    let dir = root(&ws, cx);
    let diff_edit = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.update_in(cx, |ws, window, cx| {
            let spec = DiffSpec { path: "a.txt".into(), old_path: None, left: Rev::Index, right: Rev::Working, back: None };
            ws.handle_request(&Request::OpenDiff { spec, preview: false }, window, cx)
        });
        cx.run_until_parked();
        ws.read_with(cx, |ws, _| match &ws.tabs[ws.active.unwrap()].kind {
            TabKind::DiffEdit(d) => d.clone(),
            _ => panic!("an editable diff"),
        })
    };
    let d = diff_edit(&ws, cx);
    // The change at the cursor (line 2) only.
    d.update(cx, |d, cx| d.right.update(cx, |r, cx| r.go_to(1, 0, cx)));
    click(cx, "de-stage");
    let staged = git_out(&dir, &["diff", "--cached"]);
    assert!(staged.contains("+two") && !staged.contains("+nine"), "{staged}");

    // Discarding line 9's change puts it back as staged, in the file too.
    d.update(cx, |d, cx| d.right.update(cx, |r, cx| r.go_to(8, 0, cx)));
    click(cx, "de-discard");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "1\ntwo\n3\n4\n5\n6\n7\n8\n9\n10\n");

    // The staged diff takes it back out of the index.
    ws.update_in(cx, |ws, window, cx| {
        let spec = DiffSpec { path: "a.txt".into(), old_path: None, left: Rev::Commit("HEAD".into()), right: Rev::Index, back: None };
        ws.handle_request(&Request::OpenDiff { spec, preview: false }, window, cx)
    });
    cx.run_until_parked();
    click(cx, "unstage-hunk");
    assert_eq!(git_out(&dir, &["diff", "--cached"]), "", "nothing staged any more");

    // Selected lines: only the second of two added lines.
    sh(&dir, "git checkout -q a.txt && printf 'x\\ny\\n' >> a.txt");
    let d = diff_edit(&ws, cx);
    d.update(cx, |d, cx| {
        d.right.update(cx, |r, cx| {
            r.reload_if_clean(cx);
            let (a, b) = (r.buf.rc_to_pos(11, 0), r.buf.rc_to_pos(11, 1));
            r.select_range(a..b, cx);
        })
    });
    click(cx, "de-stage");
    let staged = git_out(&dir, &["diff", "--cached"]);
    assert!(staged.contains("+y") && !staged.contains("+x"), "{staged}");
}

#[gpui::test]
fn branches_are_renamed_pushed_merged_tracked_and_deleted(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && git remote add origin ../$(basename $PWD)-remote.git && git push -q -u origin main");
        sh(dir, "git branch feature && git switch -qc topic && echo t > t.txt && git add . && git commit -qm topic && git switch -q main");
    });
    let dir = root(&ws, cx);
    open_repository(&ws, cx);
    assert!(cx.debug_bounds("ref-local-feature").is_some(), "local branches listed");

    // Rename: the menu prefills the command line, the new name finishes it.
    menu(cx, "ref-local-feature", "Rename…");
    cx.simulate_input("renamed");
    cx.simulate_keystrokes("enter");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--list", "renamed"]), "renamed");

    menu(cx, "ref-local-renamed", "Push to origin and track it");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "--abbrev-ref", "renamed@{upstream}"]), "origin/renamed");

    menu(cx, "ref-local-topic", "Merge topic into main (always a merge commit)");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%P"]).split_whitespace().count(), 2, "a merge commit");

    menu(cx, "ref-local-topic", "Delete topic…");
    click(cx, "dialog-button-Delete");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--list", "topic"]), "", "merged, so deleted");

    menu(cx, "ref-local-renamed", "Stop tracking origin/renamed");
    settle(&ws, cx);
    assert!(git_out(&dir, &["config", "branch.renamed.remote"]).is_empty());

    // A remote branch, deleted on the server.
    click(cx, "refs-group-remotes");
    menu(cx, "ref-remote-branch-origin/renamed", "Delete renamed on origin…");
    click(cx, "dialog-button-Delete on Remote");
    settle(&ws, cx);
    assert!(!git_out(&dir, &["ls-remote", "--heads", "origin"]).contains("renamed"));

    // Upstream set again from the command line, with the remote branch completed.
    command(&ws, cx, "upstream renamed origin/main");
    assert_eq!(git_out(&dir, &["rev-parse", "--abbrev-ref", "renamed@{upstream}"]), "origin/main");
}

#[gpui::test]
fn tags_remotes_and_stashes(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && git remote add origin ../$(basename $PWD)-remote.git && git push -q origin main");
    });
    let dir = root(&ws, cx);
    open_repository(&ws, cx);

    command(&ws, cx, "tag v1 First release");
    assert_eq!(git_out(&dir, &["cat-file", "-t", "v1"]), "tag", "a message makes an annotated tag");
    settle(&ws, cx);
    click(cx, "refs-group-tags");
    menu(cx, "ref-tag-v1", "Push to origin");
    settle(&ws, cx);
    assert!(git_out(&dir, &["ls-remote", "--tags", "origin"]).contains("v1"));
    menu(cx, "ref-tag-v1", "Delete v1…");
    click(cx, "dialog-button-Delete Here and on origin");
    settle(&ws, cx);
    assert!(git_out(&dir, &["tag"]).is_empty() && !git_out(&dir, &["ls-remote", "--tags", "origin"]).contains("v1"));

    let other = format!("{}-remote.git", dir.display());
    command(&ws, cx, &format!("remote-add second {other}"));
    settle(&ws, cx);
    assert!(git_out(&dir, &["remote"]).contains("second"));
    click(cx, "refs-group-remotes");
    menu(cx, "ref-remote-second", "Remove second…");
    click(cx, "dialog-button-Remove");
    settle(&ws, cx);
    assert!(!git_out(&dir, &["remote"]).contains("second"));

    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    command(&ws, cx, "stash-save my work");
    settle(&ws, cx);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "a\n", "stashed away");
    assert!(git_out(&dir, &["stash", "list"]).contains("my work"));
    menu(cx, "ref-stash-stash@{0}", "Apply");
    settle(&ws, cx);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "changed\n", "applied, and kept");
    menu(cx, "ref-stash-stash@{0}", "Drop stash@{0}…");
    click(cx, "dialog-button-Drop");
    settle(&ws, cx);
    assert!(git_out(&dir, &["stash", "list"]).is_empty());
}

#[gpui::test]
fn reset_and_interactive_rebase_from_the_graph(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        for i in 1..=5 {
            sh(dir, &format!("echo {i} > f{i}.txt && git add . && git commit -qm c{i}"));
        }
    });
    let dir = root(&ws, cx);
    show_git(&ws, cx);
    settle(&ws, cx);
    let row = |dir: &Path, rev: &str| format!("commit-row-{}", short(dir, rev));

    // Soft: the commits after it come back as staged changes.
    menu(cx, &row(&dir, "HEAD~1"), "Reset main here, keeping changes staged");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%s"]), "c4");
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "f5.txt");
    sh(&dir, "git commit -qm c5");
    settle(&ws, cx);

    // Interactive: c3 kept but reworded, c4 squashed into it, c5 dropped.
    menu(cx, &row(&dir, "HEAD~2"), "Interactive rebase from here…");
    click(cx, "rebase-message-0");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("c3 and c4");
    for _ in 0..2 {
        click(cx, "rebase-action-1");
    }
    for _ in 0..4 {
        click(cx, "rebase-action-2");
    }
    click(cx, "rebase-start");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c3 and c4", "c2", "c1"], "{}", git_out(&dir, &["log", "-3", "--format=%B--"]));
    assert!(dir.join("f4.txt").exists() && !dir.join("f5.txt").exists());

    // Hard asks first, then drops everything after it.
    std::fs::write(dir.join("f1.txt"), "dirty\n").unwrap();
    menu(cx, &row(&dir, "HEAD~1"), "Reset main here, discarding changes…");
    click(cx, "dialog-button-Reset");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%s"]), "c2");
    assert_eq!(git_out(&dir, &["status", "--porcelain"]), "", "clean");
}

#[gpui::test]
fn the_graph_is_searched_and_shows_a_files_history(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo 1 > f.txt && git add . && git -c user.name=Ann commit -qm 'fix login'");
        sh(dir, "echo x > other.txt && git add . && git -c user.name=Bob commit -qm unrelated");
        sh(dir, "echo 2 > f.txt && git commit -qam 'edit f'");
        sh(dir, "git mv f.txt g.txt && git commit -qm rename && echo 3 > g.txt && git commit -qam 'edit g'");
    });
    let dir = root(&ws, cx);
    show_git(&ws, cx);
    settle(&ws, cx);

    click(cx, "graph-search");
    cx.simulate_input("LOGIN");
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx), ["fix login"], "message, any case");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("bob");
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx), ["unrelated"], "author");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input(&short(&dir, "HEAD~1"));
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx), ["rename"], "hash");
    cx.simulate_keystrokes("escape");
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx).len(), 5, "everything again");

    // History of g.txt follows it back to f.txt.
    ws.update_in(cx, |ws, window, cx| ws.handle_request(&Request::FileHistory(dir.join("g.txt")), window, cx));
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx), ["edit g", "rename", "edit f", "fix login"]);
    click(cx, &format!("commit-row-{}", short(&dir, "HEAD~2")));
    let spec = ws.read_with(cx, |ws, cx| match &ws.tabs[ws.active.unwrap()].kind {
        TabKind::Diff(d) => Some(d.read(cx).spec.clone()),
        _ => None,
    });
    let spec = spec.expect("the file's diff in that commit");
    assert_eq!(spec.path, "f.txt", "under the name it had then");
    click(cx, "history-close");
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx).len(), 5);
}

#[gpui::test]
fn commits_are_compared_and_saved_as_patches(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        for i in 1..=3 {
            sh(dir, &format!("echo {i} > f{i}.txt && git add . && git commit -qm c{i}"));
        }
    });
    let dir = root(&ws, cx);
    show_git(&ws, cx);
    settle(&ws, cx);
    let (c1, c3) = (short(&dir, "HEAD~2"), short(&dir, "HEAD"));

    // Mark the newer one, compare with the older: the older is the base either way.
    menu(cx, &format!("commit-row-{c3}"), "Mark to compare with another commit");
    menu(cx, &format!("commit-row-{c1}"), &format!("Compare with {c3}"));
    let page = ws.read_with(cx, |ws, cx| match &ws.tabs[ws.active.unwrap()].kind {
        TabKind::Commit(c) => Some((c.read(cx).sha.clone(), c.read(cx).ahead_behind())),
        _ => None,
    });
    let full = |r: &str| git_out(&dir, &["rev-parse", r]);
    assert_eq!(page, Some((format!("{}...{}", full("HEAD~2"), full("HEAD")), Some((2, 0)))));

    command(&ws, cx, &format!("patch-save {c3} patches"));
    let patch = std::fs::read_dir(dir.join("patches")).unwrap().next().unwrap().unwrap().path();
    assert!(patch.to_string_lossy().ends_with("-c3.patch"));
    sh(&dir, "git reset -q --hard HEAD~1");
    let rel = patch.strip_prefix(&dir).unwrap().to_string_lossy().to_string();
    command(&ws, cx, &format!("patch-apply {rel}"));
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%s"]), "c3", "a mail patch comes back as its commit");
}

#[gpui::test]
fn a_conflicted_file_takes_a_whole_side(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, merge_in_conflict);
    settle(&ws, cx);
    let dir = root(&ws, cx);
    command(&ws, cx, "take theirs a.txt");
    settle(&ws, cx);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "theirs\nkeep\n");
    assert!(ws.read_with(cx, |ws, cx| ws.git.read(cx).status.conflicts.is_empty()), "resolved and staged");
}

#[gpui::test]
fn worktrees_submodules_flow_lfs_templates_and_signatures(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "mkdir -p ../$(basename $PWD)-sub && cd ../$(basename $PWD)-sub && git init -q -b main && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m sub");
        sh(dir, INIT);
        sh(dir, "printf 'Subject here\\n\\n# a comment\\n' > .gitmessage && git config commit.template .gitmessage");
        sh(dir, "echo '*.bin filter=lfs diff=lfs merge=lfs -text' > .gitattributes && git add . && git commit -qm one");
        // Deinit keeps the clone in .git/modules, so setting it up again needs no transport.
        sh(dir, "git -c protocol.file.allow=always submodule -q add ../$(basename $PWD)-sub sub && git commit -qm sub && git submodule -q deinit -f sub");
    });
    let dir = root(&ws, cx);
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    open_repository(&ws, cx);

    // The commit box starts from commit.template (comments left out).
    let msg = ws.read_with(cx, |ws, cx| ws.git_panel.read(cx).commit_input.read(cx).text());
    assert_eq!(msg, "Subject here");

    // Worktrees: added from the command line, removed from the menu.
    command(&ws, cx, &format!("worktree-add ../{name}-wt wt"));
    settle(&ws, cx);
    assert!(dir.with_file_name(format!("{name}-wt")).join(".git").exists());
    menu(cx, &format!("ref-worktree-{name}-wt"), "Remove…");
    click(cx, "dialog-button-Remove");
    settle(&ws, cx);
    assert!(!dir.with_file_name(format!("{name}-wt")).exists());

    // Submodules: set up again from its menu.
    assert!(git_out(&dir, &["submodule", "status"]).starts_with('-'), "not set up yet");
    menu(cx, "ref-submodule-sub", "Update to the recorded commit");
    settle(&ws, cx);
    let status = git_out(&dir, &["submodule", "status"]);
    assert!(!status.starts_with('-') && status.contains("sub"), "set up: {status}");

    // LFS: the group shows for a repository that tracks files with it.
    right_click(cx, "refs-group-lfs");
    assert!(cx.debug_bounds("menu-Pull LFS files").is_some());
    cx.simulate_keystrokes("escape");

    // git-flow, without the extension.
    command(&ws, cx, "flow init");
    command(&ws, cx, "flow feature start login");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "feature/login");
    sh(&dir, "echo l > login.txt && git add login.txt && git commit -qm login");
    settle(&ws, cx);
    menu(cx, "ref-local-feature/login", "Git Flow: finish this feature");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "develop");
    assert!(dir.join("login.txt").exists() && git_out(&dir, &["branch", "--list", "feature/login"]).is_empty());

    // Signing: `sign on` turns it on; a signed commit says so on its page.
    command(&ws, cx, "sign on");
    assert_eq!(git_out(&dir, &["config", "commit.gpgsign"]), "true");
    sh(&dir, "ssh-keygen -q -t ed25519 -N '' -f .key && echo \"t@t $(cat .key.pub)\" > .signers");
    sh(&dir, "git config gpg.format ssh && git config user.signingkey \"$PWD/.key\" && git config gpg.ssh.allowedSignersFile \"$PWD/.signers\"");
    sh(&dir, "git commit -q --allow-empty -m signed");
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%G?"]), "G", "signed with the ssh key");
    let sha = git_out(&dir, &["rev-parse", "HEAD"]);
    ws.update_in(cx, |ws, window, cx| ws.handle_request(&Request::OpenCommit(sha), window, cx));
    settle(&ws, cx);
    let _ = at(cx, "commit-signature");
}

#[gpui::test]
fn the_repository_section_fits_a_narrow_sidebar(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "git commit -q --allow-empty -m one && git branch a-very-long-branch-name-that-cannot-fit-in-a-narrow-sidebar-at-all");
    });
    ws.update(cx, |ws, _| ws.side_w = 180.);
    open_repository(&ws, cx);
    let header = cx.debug_bounds("section-repository").expect("header");
    let row = cx.debug_bounds("ref-local-a-very-long-branch-name-that-cannot-fit-in-a-narrow-sidebar-at-all").expect("row");
    assert!(row.right() <= header.right() + gpui::px(1.), "the row ends inside the panel: {row:?} vs {header:?}");
    assert_eq!(row.size.height, gpui::px(22.) * crate::ui::scale(), "one line");
}

fn git_tab(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<crate::git_panel::view::GitView> {
    ws.read_with(cx, |ws, _| match &ws.tabs[ws.active.unwrap()].kind {
        TabKind::Git(v) => v.clone(),
        _ => panic!("the Git tab is showing"),
    })
}

#[gpui::test]
fn the_git_tab_shows_history_details_diffs_and_asks_in_place(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && echo b > b.txt && git add . && git commit -qm 'add b'");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    let view = git_tab(&ws, cx);
    let head = short(&dir, "HEAD");

    // A commit's details below the table, and a file's diff beside them.
    click(cx, &format!("git-row-{head}"));
    assert!(view.read_with(cx, |v, _| v.details.is_some()));
    click(cx, "commit-file-b.txt");
    assert!(view.read_with(cx, |v, _| v.diff.is_some()), "the diff opens in the tab");

    // A new branch, named right in the tree.
    click(cx, "git-branch");
    cx.simulate_input("topic");
    cx.simulate_keystrokes("enter");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "topic");

    // A branch from a commit, in a small dialog (checked out unless unticked).
    menu(cx, &format!("git-row-{}", short(&dir, "HEAD~1")), "New branch here…");
    cx.simulate_input("from-dialog");
    click(cx, "form-check-Check it out");
    click(cx, "form-submit");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--list", "from-dialog"]), "from-dialog");
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "topic", "not checked out");

    // A tag with a message is an annotated tag.
    menu(cx, &format!("git-row-{head}"), "Tag this commit…");
    cx.simulate_input("v1");
    click(cx, "form-field-Message");
    cx.simulate_input("First");
    click(cx, "form-submit");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["cat-file", "-t", "v1"]), "tag");

    // An empty field doesn't close the dialog: it says what's missing.
    click(cx, "refs-add-remotes");
    click(cx, "form-submit");
    assert!(cx.debug_bounds("form-submit").is_some(), "still asking");
    let url = format!("{}-remote.git", dir.display());
    click(cx, "form-field-URL");
    cx.simulate_input(&url);
    click(cx, "form-submit");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["remote"]), "origin", "the name starts as origin");

    // Stash from the toolbar: the message is typed in the tree.
    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    click(cx, "git-stash");
    cx.simulate_input("half done");
    cx.simulate_keystrokes("enter");
    settle(&ws, cx);
    assert!(git_out(&dir, &["stash", "list"]).contains("half done"));

    // Search filters the table.
    click(cx, "git-search");
    cx.simulate_input("add b");
    settle(&ws, cx);
    assert_eq!(subjects(&ws, cx), ["add b"]);
}

#[gpui::test]
fn the_git_tab_fits_a_narrow_window(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "git commit -q --allow-empty -m 'a commit with a long message that cannot fit on a narrow screen at all, really'");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    ws.update(cx, |ws, cx| {
        ws.side_visible = false;
        cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(620.), gpui::px(700.)));
    settle(&ws, cx);
    let _ = at(cx, "git-view");
    let tab = cx.debug_bounds("git-view").unwrap();
    let inside = |cx: &mut VisualTestContext, s: &str| {
        let b = cx.debug_bounds(sel(s)).unwrap_or_else(|| panic!("{s} on screen"));
        assert!(b.right() <= tab.right() + gpui::px(1.), "{s} ends inside the tab: {b:?} vs {tab:?}");
    };
    inside(cx, &format!("git-row-{}", short(&dir, "HEAD")));
    inside(cx, "git-search");
    inside(cx, "git-refresh");
    assert!(cx.debug_bounds("git-author-0").is_none(), "no author column when narrow");
    cx.simulate_resize(gpui::size(gpui::px(1600.), gpui::px(900.)));
    settle(&ws, cx);
    let _ = at(cx, "git-author-0");
    let _ = at(cx, "ref-local-main");
}

#[gpui::test]
fn the_git_tab_stages_commits_amends_and_moves_by_keyboard(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && echo b > a.txt && echo new > b.txt");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    let view = git_tab(&ws, cx);

    click(cx, "git-local");
    assert!(view.read_with(cx, |v, _| v.local));
    click(cx, "unstaged-a.txt");
    assert!(view.read_with(cx, |v, _| v.edit.is_some()), "an unstaged change opens as an editable diff");
    click(cx, "unstaged-all");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "a.txt\nb.txt");
    click(cx, "staged-b.txt");
    assert!(view.read_with(cx, |v, _| v.diff.is_some()), "a staged change opens its diff");

    click(cx, "git-commit-msg");
    cx.simulate_input("Two files");
    click(cx, "git-commit");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%s"]), "Two files");
    assert_eq!(git_out(&dir, &["status", "--porcelain"]), "", "all committed");

    // Amend starts from the last message and folds the new change in.
    std::fs::write(dir.join("a.txt"), "c\n").unwrap();
    settle(&ws, cx);
    click(cx, "git-local");
    click(cx, "unstaged-all");
    settle(&ws, cx);
    click(cx, "git-amend");
    assert_eq!(view.read_with(cx, |v, cx| v.commit_msg.read(cx).text()), "Two files");
    click(cx, "git-commit");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-list", "--count", "HEAD"]), "2", "amended, not a new commit");
    assert_eq!(git_out(&dir, &["show", "HEAD:a.txt"]), "c");

    // ↓ / ↑ move through the history.
    let first = git_out(&dir, &["rev-parse", "HEAD"]);
    let second = git_out(&dir, &["rev-parse", "HEAD~1"]);
    click(cx, &format!("git-row-{}", short(&dir, "HEAD")));
    cx.simulate_keystrokes("down");
    let shown = |cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.details.as_ref().map(|d| d.read(cx).sha.clone()));
    assert_eq!(shown(cx), Some(second));
    cx.simulate_keystrokes("up");
    assert_eq!(shown(cx), Some(first));
}

#[gpui::test]
fn the_git_tab_finishes_or_aborts_a_merge(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, merge_in_conflict);
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    // Continue is there but waits for the last conflict.
    let _ = at(cx, "git-op-continue");
    let _ = at(cx, "git-op-detail");
    click(cx, "git-op-continue");
    assert!(ws.read_with(cx, |ws, cx| ws.git.read(cx).status.op.is_some()), "not while a file is conflicted");
    click(cx, "git-local");
    let _ = at(cx, "conflicts-a.txt");

    // Resolved and staged: Commit Merge appears and finishes it.
    sh(&dir, "printf 'both\\nkeep\\n' > a.txt && git add a.txt");
    settle(&ws, cx);
    click(cx, "git-op-continue");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%P"]).split_whitespace().count(), 2, "a merge commit");
    assert!(cx.debug_bounds("git-op-abort").is_none(), "the banner is gone");

    // Another merge, aborted.
    sh(&dir, "git switch -qc other HEAD~1 && printf 'other\\nkeep\\n' > a.txt && git commit -qam other && git switch -q main && git merge other");
    settle(&ws, cx);
    click(cx, "git-op-abort");
    settle(&ws, cx);
    assert!(ws.read_with(cx, |ws, cx| ws.git.read(cx).status.op.is_none()));
}

#[gpui::test]
fn the_tree_selects_commits_and_changed_files_have_menus(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && git -c tag.gpgSign=false tag v1");
        sh(dir, "git switch -qc side && echo s > s.txt && git add . && git commit -qm side && git switch -q main");
        sh(dir, "echo b > b.txt && git add . && git commit -qm two && echo changed > a.txt");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);

    // The sidebar's graph follows a click in its tree.
    open_repository(&ws, cx);
    click(cx, "refs-group-tags");
    click(cx, "ref-tag-v1");
    let picked = ws.read_with(cx, |ws, cx| ws.git_panel.read(cx).selected_commit().and_then(|ix| ws.git.read(cx).commits.get(ix).map(|c| c.sha.clone())));
    assert_eq!(picked, Some(git_out(&dir, &["rev-parse", "v1"])));

    command(&ws, cx, "repo");
    let view = git_tab(&ws, cx);
    let shown = |cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.details.as_ref().map(|d| d.read(cx).sha.clone()));
    // `side` isn't in the history shown: it shows every branch, then selects it.
    assert!(!ws.read_with(cx, |ws, cx| ws.git.read(cx).graph_all));
    click(cx, "ref-local-side");
    settle(&ws, cx);
    assert_eq!(shown(cx), Some(git_out(&dir, &["rev-parse", "side"])));
    assert!(ws.read_with(cx, |ws, cx| ws.git.read(cx).graph_all), "all branches on");
    click(cx, "git-all");
    settle(&ws, cx);
    assert!(!ws.read_with(cx, |ws, cx| ws.git.read(cx).graph_all), "and off again from the toolbar");

    // A changed file's menu stages and unstages it.
    click(cx, "git-local");
    menu(cx, "unstaged-a.txt", "Stage");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "a.txt");
    menu(cx, "staged-a.txt", "Unstage");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "");
}

#[gpui::test]
fn conflicted_files_open_the_merge_editor_or_take_a_side(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, merge_in_conflict);
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    click(cx, "git-local");
    click(cx, "conflicts-a.txt");
    assert!(ws.read_with(cx, |ws, _| matches!(ws.tabs[ws.active.unwrap()].kind, TabKind::Merge(_))), "the merge editor opens");
    command(&ws, cx, "repo");
    menu(cx, "conflicts-a.txt", "Take theirs (the whole file)");
    settle(&ws, cx);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "theirs\nkeep\n");
    assert!(ws.read_with(cx, |ws, cx| ws.git.read(cx).status.conflicts.is_empty()));
}

#[gpui::test]
fn long_histories_load_more_as_you_scroll(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        // 900 commits at once (fast-import), more than one page of history.
        sh(dir, "(for i in $(seq 1 900); do printf 'commit refs/heads/main\\ncommitter t <t@t> %s +0000\\ndata <<END\\nc%s\\nEND\\n\\n' $((1700000000 + i)) $i; done) | git fast-import --quiet && git reset -q --hard main");
    });
    settle(&ws, cx);
    command(&ws, cx, "repo");
    let count = |cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.git.read(cx).commits.len());
    assert_eq!(count(cx), crate::repo::HISTORY_PAGE);
    let dir = root(&ws, cx);
    let table = at(cx, &format!("git-row-{}", short(&dir, "HEAD")));
    for _ in 0..60 {
        cx.simulate_event(gpui::ScrollEvent { position: table, delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-2000.))), ..Default::default() });
        cx.run_until_parked();
    }
    settle(&ws, cx);
    assert_eq!(count(cx), 900, "the rest came in at the end of the list");
}

#[gpui::test]
fn the_git_tab_fetches_quietly_and_counts_the_subject(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "git commit -q --allow-empty -m one && git remote add origin ../$(basename $PWD)-remote.git && git push -q -u origin main");
        sh(dir, "git clone -q ../$(basename $PWD)-remote.git ../$(basename $PWD)-other && cd ../$(basename $PWD)-other && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m 'from elsewhere' && git push -q");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    let view = git_tab(&ws, cx);
    view.update(cx, |v, cx| v.fetch_quietly(cx));
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%s", "origin/main"]), "from elsewhere");
    assert_eq!(ws.read_with(cx, |ws, cx| ws.git.read(cx).status.behind), 1, "the counts know");
    let _ = at(cx, "git-pull");

    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    settle(&ws, cx);
    click(cx, "git-local");
    click(cx, "git-commit-msg");
    cx.simulate_input(&"a".repeat(80));
    let _ = at(cx, "git-subject-len");
}

/// A stand-in for `gh`: pull request #7 (branch `login` into `main`) with one line comment;
/// every call is written to `gh.log` beside it.
const FAKE_GH: &str = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/gh.log"
case "$1 $2" in
  "pr list") case "$*" in *"--head newwork"*) echo '[]' ; exit 0 ;; esac; echo '[{"number":7,"title":"Login page","author":{"login":"ann"},"headRefName":"login","baseRefName":"main","isDraft":false,"reviewDecision":"REVIEW_REQUIRED","statusCheckRollup":[{"name":"ci","conclusion":"SUCCESS"}],"updatedAt":"2026-11-14T10:00:00Z"}]' ;;
  "pr view") echo '{"number":7,"title":"Login page","body":"Adds the login page.","author":{"login":"ann"},"state":"OPEN","url":"https://example.invalid/pr/7","headRefName":"login","baseRefName":"main","headRefOid":"'"$(git rev-parse refs/oskl/pr/7 2>/dev/null)"'","additions":1,"deletions":0,"isDraft":false,"reviewDecision":"REVIEW_REQUIRED","mergeable":"MERGEABLE","comments":[{"id":"IC_1","author":{"login":"bob"},"body":"Thanks!","createdAt":"2026-11-14T11:00:00Z"},{"id":"IC_2","author":{"login":"me"},"body":"Mine","createdAt":"2026-11-14T11:30:00Z"}],"reviews":[{"id":"R_1","author":{"login":"bob"},"state":"CHANGES_REQUESTED","body":"Needs work","submittedAt":"2026-11-14T12:00:00Z"}],"statusCheckRollup":[{"name":"build","workflowName":"CI","conclusion":"SUCCESS","status":"COMPLETED","detailsUrl":"https://example.invalid/run/1"},{"name":"lint","workflowName":"CI","conclusion":"","status":"IN_PROGRESS","detailsUrl":"https://example.invalid/run/2"}],"commits":[{"oid":"'"$(git --git-dir=../$(basename $PWD)-remote.git rev-parse refs/pull/7/head)"'","messageHeadline":"Login page","authors":[{"login":"ann"}],"committedDate":"2026-11-14T10:00:00Z"}]}' ;;
  "api repos/{owner}/{repo}/pulls/7/comments?per_page=100") echo '[{"id":101,"pull_request_review_id":55,"path":"login.txt","line":1,"body":"Nice","user":{"login":"bob"},"created_at":"2026-11-14T12:00:00Z","diff_hunk":"@@ -0,0 +1,3 @@\\n+login"}]' ;;
  "api repos/{owner}/{repo}/pulls/7/reviews?per_page=100") echo '[{"id":55,"node_id":"R_1"}]' ;;
  "api graphql") case "$*" in *reviewThreads*) echo '{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"id":"T_1","isResolved":false,"comments":{"nodes":[{"databaseId":101}]}}]},"timelineItems":{"nodes":[{"actor":{"login":"ann"},"beforeCommit":{"abbreviatedOid":"aaaaaaa"},"afterCommit":{"abbreviatedOid":"bbbbbbb"},"createdAt":"2026-11-14T13:00:00Z"}]}}}}}' ;; *) echo '{}' ;; esac ;;
  "api user") echo me ;;
  "pr edit") echo ok ;;
  "pr close"|"pr reopen") echo ok ;;
  "api --method") case "$*" in *--input*) for a; do last=$a; done; cat "$last" >> "$(dirname "$0")/gh.log"; echo >> "$(dirname "$0")/gh.log" ;; esac; echo '{}' ;;
  "pr review"|"pr comment") echo ok ;;
  "pr create") echo "https://example.invalid/pr/8" ;;
  "pr checkout") git switch -q -c login refs/oskl/pr/7 ;;
  *) echo "unknown: $*" >&2; exit 1 ;;
esac
"#;

/// The fake gh, one for the whole run: `OSKL_GH` is process-wide, so tests running at the same
/// time share it (it answers any repository) and its log.
fn fake_gh() -> std::path::PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join(format!("oskl-fake-gh-{}", std::process::id()));
    ONCE.call_once(|| {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gh"), FAKE_GH).unwrap();
        sh(&dir, "chmod +x gh");
        unsafe { std::env::set_var("OSKL_GH", dir.join("gh")) };
    });
    dir.join("gh")
}

#[gpui::test]
fn the_graph_filter_applies_at_once_and_labels_only_the_picked_branches(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm base && git tag v1 && git branch stale");
        sh(dir, "git switch -qc feature && echo f > f.txt && git add . && git commit -qm feat && git tag v2");
        sh(dir, "git switch -qc other main && echo o > o.txt && git add . && git commit -qm other-work");
        sh(dir, "git switch -q main && echo m > m.txt && git add . && git commit -qm main-work");
    });
    settle(&ws, cx);
    show_git(&ws, cx);
    settle(&ws, cx);
    let shown = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.repo.read(cx).commits.iter().map(|c| (c.subject.clone(), c.refs.iter().map(|r| r.name.clone()).collect::<Vec<_>>())).collect::<Vec<_>>())
    };
    click(cx, "graph-scope");
    settle(&ws, cx);
    click(cx, "scope-local-feature");
    settle(&ws, cx);
    // Only feature's history, labelled with feature and its tags: not with stale, another
    // branch on the shared commit.
    let picked = shown(&ws, cx);
    let subjects: Vec<&str> = picked.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(subjects, ["feat", "base"], "{picked:?}");
    let labels: Vec<&String> = picked.iter().flat_map(|(_, r)| r).collect();
    assert_eq!(labels, ["feature", "v2", "v1"].map(String::from).iter().collect::<Vec<_>>(), "{picked:?}");
    // A second pick adds its branch at once.
    click(cx, "scope-local-other");
    settle(&ws, cx);
    let picked = shown(&ws, cx);
    assert!(picked.iter().any(|(s, _)| s == "other-work") && !picked.iter().any(|(s, _)| s == "main-work"), "{picked:?}");
    let mut labels: Vec<String> = picked.iter().flat_map(|(_, r)| r.clone()).collect();
    labels.sort();
    assert_eq!(labels, ["feature", "other", "v1", "v2"]);
}

#[gpui::test]
fn a_pull_request_is_opened_from_the_sidebar_or_source_control(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && git remote add origin ../$(basename $PWD)-remote.git && git push -q -u origin main");
        sh(dir, "git switch -qc newwork && echo b > b.txt && git add . && git commit -qm 'Add the b file' -m 'Because b matters.'");
        fake_gh();
    });
    let dir = root(&ws, cx);
    let log = || std::fs::read_to_string(fake_gh().with_file_name("gh.log")).unwrap_or_default();
    settle(&ws, cx);
    let form = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Commit(c) if c.read(cx).create.is_some() => Some(c.clone()),
            _ => None,
        }))
    };

    // Source Control, the work committed: a button proposes the branch.
    show_git(&ws, cx);
    settle(&ws, cx);
    click(cx, "git-new-pr");
    settle(&ws, cx);
    let view = form(&ws, cx).expect("the form opens in a tab");
    assert_eq!(view.read_with(cx, |v, _| v.compare.clone()), Some(("main".into(), "newwork".into())), "from this branch into the default one");
    // One commit: its subject and body to start from.
    let title = cx.debug_bounds("pr-new-title").expect("the title box");
    let _ = title;
    // Preview shows the description as it will read on GitHub, Write the box again.
    click(cx, "md-tab-preview");
    let _ = at(cx, "md-preview");
    assert!(cx.debug_bounds("pr-new-body").is_none());
    click(cx, "md-tab-write");
    let _ = at(cx, "pr-new-body");
    assert_eq!(view.read_with(cx, |v, cx| v.form_text(cx)), Some(("Add the b file".into(), "Because b matters.".into())));
    // The sidebar's + opens the same form, not a second one.
    click(cx, "act-prs");
    settle(&ws, cx);
    click(cx, "pr-new");
    settle(&ws, cx);
    assert_eq!(ws.read_with(cx, |ws, cx| ws.tabs.iter().filter(|t| matches!(&t.kind, TabKind::Commit(c) if c.read(cx).create.is_some())).count()), 1);

    // Create: the branch is pushed, gh opens it, and its review takes the form's place.
    click(cx, "pr-new-create");
    settle(&ws, cx);
    assert!(log().contains("pr create --base main --head newwork --title Add the b file --body Because b matters."), "{}", log());
    assert!(git_out(&dir, &["ls-remote", "origin", "newwork"]).contains("refs/heads/newwork"), "pushed first");
    assert!(form(&ws, cx).is_none(), "the form closed");
    assert_eq!(ws.read_with(cx, |ws, cx| match &ws.tabs[ws.active.unwrap()].kind {
        TabKind::Pr(v) => Some(v.read(cx).number),
        _ => None,
    }), Some(8));
}

#[gpui::test]
fn pull_requests_have_their_own_sidebar_and_are_reviewed_in_a_tab(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && git remote add origin ../$(basename $PWD)-remote.git && git push -q -u origin main");
        sh(dir, "git switch -qc login && printf 'login\\nform\\nbutton\\n' > login.txt && git add . && git commit -qm 'Login page' && git push -q origin login:refs/pull/7/head && git switch -q main && git branch -qD login");
        fake_gh();
    });
    let dir = root(&ws, cx);
    let log = || std::fs::read_to_string(fake_gh().with_file_name("gh.log")).unwrap_or_default();
    settle(&ws, cx);
    let pr_tab = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| match &ws.tabs[ws.active.unwrap()].kind {
            TabKind::Pr(v) => Some(v.clone()),
            _ => None,
        })
    };

    // Their own sidebar item, apart from Source Control; a state filter asks gh for those.
    click(cx, "act-prs");
    settle(&ws, cx);
    assert!(ws.read_with(cx, |ws, _| ws.side == Side::Prs));
    assert!(log().contains("pr list --state open"), "{}", log());
    click(cx, "pr-filter-merged");
    settle(&ws, cx);
    assert!(log().contains("pr list --state merged"), "{}", log());
    click(cx, "pr-filter-open");
    // Back to a state already read: its list shows at once while gh reads it again.
    cx.run_until_parked();
    let _ = at(cx, "pr-row-7");
    settle(&ws, cx);

    click(cx, "pr-row-7");
    settle(&ws, cx);
    let pr = pr_tab(&ws, cx).expect("the review opens in its own tab");
    assert!(pr.read_with(cx, |p, _| matches!(&p.detail, Some(Ok(_)))), "read with gh");
    let _ = at(cx, "pr-conversation");

    // Its files, from its commits fetched here, open their diff.
    click(cx, "pr-tab-files");
    settle(&ws, cx);
    click(cx, "pr-file-login.txt");
    settle(&ws, cx);
    assert!(pr.read_with(cx, |p, _| p.diff.is_some()));

    // A commit picked in Commits opens there: its files and their diff, until Back.
    click(cx, "pr-tab-commits");
    let sha = git_out(&dir, &["rev-parse", "--short=7", "refs/oskl/pr/7"]);
    click(cx, &format!("pr-commit-{sha}"));
    settle(&ws, cx);
    assert!(pr.read_with(cx, |p, _| p.commit.is_some() && p.tab == crate::git_panel::pr::Tab::Commits && matches!(p.shown_files(), Some(Ok(f)) if f.list.len() == 1)));
    click(cx, "pr-file-login.txt");
    settle(&ws, cx);
    assert!(pr.read_with(cx, |p, _| p.diff.is_some()));
    click(cx, "pr-commit-back");
    assert!(pr.read_with(cx, |p, _| p.commit.is_none() && p.diff.is_none()));
    let _ = at(cx, &format!("pr-commit-{sha}"));
    click(cx, "pr-tab-files");

    // The filter lists matching files flat.
    click(cx, "pr-files-filter");
    cx.simulate_input("login");
    let _ = at(cx, "pr-file-login.txt");
    click(cx, "pr-file-login.txt");
    settle(&ws, cx);

    // The + on a line opens a box under it, below the comment already there: a single
    // comment goes at once.
    let _ = at(cx, "pr-thread-login.txt-1");
    click(cx, "diff-comment-new-1");
    settle(&ws, cx);
    click(cx, "pr-line-input");
    cx.simulate_input("Looks right");
    click(cx, "pr-line-single");
    settle(&ws, cx);
    assert!(log().contains("path=login.txt") && log().contains("line=1") && log().contains("body=Looks right"), "{}", log());
    // Start a review on lines 1 to 3 (Shift-click a second +): the comment waits, shown under
    // them, and kept on disk ...
    click(cx, "diff-comment-new-1");
    settle(&ws, cx);
    // Shift-click a second + ...
    let third = at(cx, "diff-comment-new-3");
    cx.simulate_click(third, gpui::Modifiers::shift());
    settle(&ws, cx);
    assert_eq!(pr.read_with(cx, |p, _| p.line_box.as_ref().map(|b| (b.start, b.line))), Some((Some(1), 3)));
    // ... or press one + and drag down over the others.
    click(cx, "pr-line-cancel");
    let (first, third) = (at(cx, "diff-comment-new-1"), at(cx, "diff-comment-new-3"));
    cx.simulate_mouse_down(first, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(third, Some(gpui::MouseButton::Left), gpui::Modifiers::none());
    cx.simulate_mouse_up(third, gpui::MouseButton::Left, gpui::Modifiers::none());
    settle(&ws, cx);
    assert_eq!(pr.read_with(cx, |p, _| p.line_box.as_ref().map(|b| (b.start, b.line))), Some((Some(1), 3)));
    assert_eq!(pr.read_with(cx, |p, cx| p.diff.as_ref().and_then(|d| d.read(cx).marked)), Some((true, 1, 3)), "the lines are tinted");
    click(cx, "pr-line-input");
    cx.simulate_input("Rename this");
    click(cx, "pr-line-review");
    settle(&ws, cx);
    assert_eq!(pr.read_with(cx, |p, _| p.pending.len()), 1);
    let _ = at(cx, "pr-pending-del-0");
    let kept = git_out(&dir, &["rev-parse", "--git-path", "oskl/review-7.json"]);
    assert!(std::fs::read_to_string(dir.join(&kept)).unwrap().contains("Rename this"));
    // ... until Finish your review sends it with the approval.
    click(cx, "pr-review");
    let _ = at(cx, "pr-pending-0");
    click(cx, "pr-kind-approve");
    click(cx, "pr-submit-review");
    settle(&ws, cx);
    assert!(log().contains("pulls/7/reviews --input"), "{}", log());
    assert!(log().contains(r#""event":"APPROVE""#) && log().contains(r#""body":"Rename this""#) && log().contains(r#""side":"RIGHT""#), "{}", log());
    assert!(log().contains(r#""start_line":1"#) && log().contains(r#""line":3"#), "{}", log());
    assert_eq!(pr.read_with(cx, |p, _| p.pending.len()), 0);
    assert!(!dir.join(&kept).exists(), "nothing pending left");

    // The conversation: its checks (each a link to its run), whether it merges, a comment box
    // with its preview.
    click(cx, "pr-tab-conversation");
    settle(&ws, cx);
    let _ = at(cx, "pr-check-CI / build");
    let _ = at(cx, "pr-check-CI / lint");
    let _ = at(cx, "pr-merge-state");
    // (scrolled down to the box first)
    let page = at(cx, "pr-conversation");
    cx.simulate_event(gpui::ScrollEvent { position: page, delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-5000.))), ..Default::default() });
    cx.run_until_parked();
    click(cx, "pr-comment-input");
    cx.simulate_input("Ship it");
    click(cx, "md-tab-preview");
    let _ = at(cx, "md-preview");
    // ⌘Enter twice (or two clicks) sends it once.
    pr.update(cx, |p, cx| {
        p.post_comment(cx);
        p.post_comment(cx);
    });
    settle(&ws, cx);
    assert_eq!(log().matches("pr comment 7 --body Ship it").count(), 1, "{}", log());

    // Commits grouped, the force push, bob's review with his thread under it (back up top).
    cx.simulate_event(gpui::ScrollEvent { position: page, delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(5000.))), ..Default::default() });
    cx.run_until_parked();
    let _ = at(cx, &format!("pr-timeline-commit-{sha}"));
    let _ = at(cx, "pr-force-push-0");
    // Resolve the thread; edit my own comment (Cancel leaves it, Update comment sends it).
    click(cx, "pr-resolve-101");
    settle(&ws, cx);
    assert!(log().contains("resolveReviewThread") && log().contains("id=T_1"), "{}", log());
    assert!(cx.debug_bounds("pr-edit-IC_1").is_none(), "not mine");
    click(cx, "pr-edit-IC_2");
    let _ = at(cx, "pr-edit-input");
    click(cx, "pr-edit-cancel");
    assert!(pr.read_with(cx, |p, _| p.editing.is_none()));
    click(cx, "pr-edit-IC_2");
    click(cx, "pr-edit-save");
    settle(&ws, cx);
    assert!(log().contains("updateIssueComment") && log().contains("id=IC_2"), "{}", log());

    // Close pull request, beside Comment.
    let page = at(cx, "pr-conversation");
    cx.simulate_event(gpui::ScrollEvent { position: page, delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-5000.))), ..Default::default() });
    cx.run_until_parked();
    click(cx, "pr-close");
    settle(&ws, cx);
    assert!(log().contains("pr close 7"), "{}", log());

    // Checked out, the Git tab shows its chip, which comes back to this review's tab.
    click(cx, "pr-checkout");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "login");
    command(&ws, cx, "repo");
    settle(&ws, cx);
    let _ = git_tab(&ws, cx);
    click(cx, "git-pr-chip");
    settle(&ws, cx);
    assert_eq!(pr_tab(&ws, cx).map(|p| p.entity_id()), Some(pr.entity_id()), "the same tab, not a second one");
}

#[gpui::test]
fn the_rebase_dialog_counts_reorders_by_dragging_and_refuses_a_first_squash(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        for i in 1..=4 {
            sh(dir, &format!("echo {i} > f{i}.txt && git add . && git commit -qm c{i}"));
        }
    });
    let dir = root(&ws, cx);
    show_git(&ws, cx);
    settle(&ws, cx);
    menu(cx, &format!("commit-row-{}", short(&dir, "HEAD~2")), "Interactive rebase from here…");
    let refs = ws.read_with(cx, |ws, cx| ws.git_panel.read(cx).refs.clone());
    let state = |cx: &mut VisualTestContext| refs.read_with(cx, |t, cx| t.rebase_state(cx)).expect("the dialog is open");
    let dialog = at(cx, "rebase-plan");
    let window = cx.debug_bounds("rebase-plan").unwrap();
    assert!((dialog.x - cx.update(|w, _| w.viewport_size().width / 2.)).abs() < gpui::px(2.), "centered in the window");
    assert_eq!(state(cx), ("3 commits become 3".to_string(), vec!["c2".into(), "c3".into(), "c4".into()]));
    assert!(window.size.width <= gpui::px(640.) + gpui::px(1.));

    // Squashing the second row folds it into the first.
    for _ in 0..2 {
        click(cx, "rebase-action-1");
    }
    assert_eq!(state(cx).0, "3 commits become 2");

    // Dragging the second row's handle onto the first puts it first: a squash with nothing before it.
    let (grip, target) = (at(cx, "rebase-grip-1"), at(cx, "rebase-row-0"));
    cx.simulate_mouse_down(grip, MouseButton::Left, Modifiers::none());
    for step in 1..=4 {
        let t = step as f32 / 4.;
        cx.simulate_mouse_move(gpui::point(grip.x + (target.x - grip.x) * t, grip.y + (target.y - grip.y) * t), MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(state(cx).1, ["c3", "c2", "c4"]);
    let _ = at(cx, "rebase-bad");
    click(cx, "rebase-start");
    assert!(cx.debug_bounds("rebase-plan").is_some(), "Start waits until the first commit kept can stand alone");
    assert_eq!(git_out(&dir, &["log", "-1", "--format=%s"]), "c4", "nothing was rebased");

    // The arrows still move rows.
    click(cx, "rebase-down-0");
    assert_eq!(state(cx).1, ["c2", "c3", "c4"]);
    assert!(cx.debug_bounds("rebase-bad").is_none());
    click(cx, "rebase-cancel");
    assert!(cx.debug_bounds("rebase-plan").is_none());
}

#[gpui::test]
fn the_git_tab_explains_empty_histories_searches_and_clean_trees(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    command(&ws, cx, "repo");
    let _ = at(cx, "git-no-commits");

    sh(&dir, "echo 1 > a.txt && git add . && git commit -qm one && echo 2 >> a.txt");
    settle(&ws, cx);
    let _ = at(cx, "git-nothing-picked");
    assert!((0..40).any(|i| cx.debug_bounds(sel(&format!("ref-note-{i}"))).is_some()), "no remote: the tree says so");
    click(cx, "git-search");
    cx.simulate_input("zzzzzz");
    settle(&ws, cx);
    let _ = at(cx, "git-no-commits");
    click(cx, "git-clear-search");
    settle(&ws, cx);
    assert!(cx.debug_bounds("git-no-commits").is_none(), "the history is back");

    // Local changes, then committed: the pane says there is nothing left.
    click(cx, "git-local");
    sh(&dir, "git commit -qam two");
    settle(&ws, cx);
    let _ = at(cx, "git-tab-clean");
}

fn toast(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds("undo-toast").is_some()
}

fn flashed(ws: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    ws.read_with(cx, |ws, _| ws.message.clone().map(|m| m.0).unwrap_or_default())
}

#[gpui::test]
fn undo_takes_back_a_commit_an_amend_and_a_hard_reset(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && echo b > a.txt && echo new > b.txt");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    assert!(!toast(cx), "nothing done yet");
    command(&ws, cx, "undo-git");
    assert!(flashed(&ws, cx).starts_with("Nothing to undo"), "{}", flashed(&ws, cx));
    command(&ws, cx, "repo");
    click(cx, "git-local");
    click(cx, "unstaged-all");
    settle(&ws, cx);
    click(cx, "git-commit-msg");
    cx.simulate_input("Two files");
    click(cx, "git-commit");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-list", "--count", "HEAD"]), "2");

    // The toast offers Undo: the commit goes, its changes stay staged.
    assert!(toast(cx), "a toast with Undo");
    click(cx, "undo-button");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "a.txt\nb.txt");
    assert!(!toast(cx), "the toast goes");
    assert!(ws.read_with(cx, |ws, cx| ws.git.read(cx).undo.is_none()), "undone once");
    command(&ws, cx, "undo-git");
    assert!(flashed(&ws, cx).starts_with("Nothing to undo"), "not twice");

    // An amend, taken back by the command: the amended changes are staged again.
    sh(&dir, "git commit -qm two");
    settle(&ws, cx);
    let two = git_out(&dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("a.txt"), "c\n").unwrap();
    settle(&ws, cx);
    click(cx, "git-local");
    click(cx, "unstaged-all");
    settle(&ws, cx);
    click(cx, "git-amend");
    click(cx, "git-commit");
    settle(&ws, cx);
    assert_ne!(git_out(&dir, &["rev-parse", "HEAD"]), two);
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), two, "the old commit is back");
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "a.txt");

    // A hard reset throws the changes away; Undo brings the commit and the changes back.
    sh(&dir, "git commit -qm three && echo dirty > b.txt");
    settle(&ws, cx);
    let (three, target) = (git_out(&dir, &["rev-parse", "HEAD"]), git_out(&dir, &["rev-parse", "HEAD~1"]));
    show_git(&ws, cx);
    menu(cx, &format!("commit-row-{}", short(&dir, "HEAD~1")), "Reset main here, discarding changes…");
    click(cx, "dialog-button-Reset");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), target, "reset");
    assert_eq!(git_out(&dir, &["status", "--porcelain"]), "");
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), three);
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "dirty\n", "the discarded change is back");
}

#[gpui::test]
fn undo_of_a_merge_asks_before_it_replaces_changed_files_and_stops_when_the_branch_moved(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && echo s > s.txt && git add . && git commit -qm one && git switch -qc feature && echo f > f.txt && git add . && git commit -qm feat && git switch -q main");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);
    let before = git_out(&dir, &["rev-parse", "HEAD"]);
    command(&ws, cx, "merge feature");
    settle(&ws, cx);
    assert!(dir.join("f.txt").exists());
    assert!(toast(cx));

    // Uncommitted changes: it asks, and keeps them in a stash.
    std::fs::write(dir.join("s.txt"), "mine\n").unwrap();
    settle(&ws, cx);
    command(&ws, cx, "undo-git");
    let title = ws.read_with(cx, |ws, _| ws.ask.as_ref().map(|a| a.ask.title.clone())).expect("asks first");
    assert!(title.contains("Merged"), "{title}");
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), git_out(&dir, &["rev-parse", "feature"]), "nothing happened yet");
    click(cx, "dialog-button-Stash and Undo");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), before);
    assert!(!dir.join("f.txt").exists());
    assert!(git_out(&dir, &["stash", "list"]).contains("Before undoing"), "the changes are kept");
    assert_eq!(git_out(&dir, &["stash", "show", "-p", "stash@{0}"]).matches("+mine").count(), 1);

    // A newer commit on the branch: undoing the old action would drop it, so it stops.
    command(&ws, cx, "merge feature");
    settle(&ws, cx);
    sh(&dir, "echo n > n.txt && git add . && git commit -qm newer");
    settle(&ws, cx);
    let head = git_out(&dir, &["rev-parse", "HEAD"]);
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), head, "left alone");
    assert!(flashed(&ws, cx).contains("moved on"), "{}", flashed(&ws, cx));
}

#[gpui::test]
fn undo_takes_back_the_first_commit_a_pull_a_cherry_pick_and_a_revert(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "git remote add origin ../$(basename $PWD)-remote.git && echo a > a.txt && git add .");
    });
    let dir = root(&ws, cx);
    settle(&ws, cx);

    // The first commit of a branch: the branch goes back to not existing, the files stay staged.
    command(&ws, cx, "git commit -qm first");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-list", "--count", "HEAD"]), "1");
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--list"]), "", "no commits again");
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "a.txt");
    sh(&dir, "git commit -qm first && git push -q -u origin main && echo s > s.txt && git switch -qc side && git add . && git commit -qm side && git switch -q main");
    settle(&ws, cx);

    // A pull: the branch is where it was, and the files with it.
    let before = git_out(&dir, &["rev-parse", "HEAD"]);
    sh(&dir, "b=$(basename $PWD) && git clone -q ../$b-remote.git ../$b-other && cd ../$b-other && git config user.email t@t && git config user.name t && echo n > n.txt && git add . && git commit -qm remote && git push -q origin main");
    command(&ws, cx, "pull");
    settle(&ws, cx);
    assert!(dir.join("n.txt").exists(), "pulled");
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), before);
    assert!(!dir.join("n.txt").exists());

    // A cherry-pick and a revert: their commit goes, the changes stay staged.
    command(&ws, cx, "git cherry-pick side");
    settle(&ws, cx);
    assert!(dir.join("s.txt").exists());
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), before);
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "s.txt");
    sh(&dir, "git commit -qm picked");
    let picked = git_out(&dir, &["rev-parse", "HEAD"]);
    command(&ws, cx, "git revert --no-edit HEAD");
    settle(&ws, cx);
    assert!(!dir.join("s.txt").exists(), "reverted");
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), picked);
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "s.txt", "the revert's changes are staged");
}

#[gpui::test]
fn undo_puts_back_stashes_deleted_branches_and_discarded_files(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one && git switch -qc topic && echo t > t.txt && git add . && git commit -qm topic && git switch -q main");
    });
    let dir = root(&ws, cx);
    open_repository(&ws, cx);
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();

    // Stash, then Undo: the changes come back and the stash entry goes.
    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    command(&ws, cx, "stash-save my work");
    settle(&ws, cx);
    assert_eq!(read("a.txt"), "a\n");
    assert!(toast(cx));
    click(cx, "undo-button");
    settle(&ws, cx);
    assert_eq!(read("a.txt"), "changed\n");
    assert_eq!(git_out(&dir, &["stash", "list"]), "");

    // Pop, then Undo: the stash is back in the list.
    command(&ws, cx, "stash-save again");
    settle(&ws, cx);
    command(&ws, cx, "stash pop");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["stash", "list"]), "");
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert!(git_out(&dir, &["stash", "list"]).contains("again"), "the popped stash is listed again");
    assert_eq!(read("a.txt"), "a\n", "and its changes are out of the files");

    // A pop whose files were edited since keeps them, and says so.
    command(&ws, cx, "stash pop");
    settle(&ws, cx);
    std::fs::write(dir.join("a.txt"), "edited since\n").unwrap();
    settle(&ws, cx);
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert!(git_out(&dir, &["stash", "list"]).contains("again"));
    assert_eq!(read("a.txt"), "edited since\n");
    assert!(flashed(&ws, cx).contains("changed since"), "{}", flashed(&ws, cx));
    sh(&dir, "git checkout -q -- . && git stash drop -q");

    // A branch deleted with Delete Anyway comes back at the same commit.
    let topic = git_out(&dir, &["rev-parse", "topic"]);
    settle(&ws, cx);
    menu(cx, "ref-local-topic", "Delete topic…");
    click(cx, "dialog-button-Delete Anyway");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["branch", "--list", "topic"]), "");
    assert!(toast(cx));
    click(cx, "undo-button");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "topic"]), topic);

    // A discard comes back too; a file edited since is left as it is.
    std::fs::write(dir.join("a.txt"), "precious\n").unwrap();
    settle(&ws, cx);
    command(&ws, cx, "discard a.txt");
    settle(&ws, cx);
    assert_eq!(read("a.txt"), "a\n");
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(read("a.txt"), "precious\n");
    command(&ws, cx, "discard a.txt");
    settle(&ws, cx);
    std::fs::write(dir.join("a.txt"), "newer\n").unwrap();
    command(&ws, cx, "undo-git");
    settle(&ws, cx);
    assert_eq!(read("a.txt"), "newer\n", "not overwritten");
    assert!(flashed(&ws, cx).contains("changed those files"), "{}", flashed(&ws, cx));
}

#[gpui::test]
fn a_commit_is_fixed_up_with_the_staged_changes_and_undone(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, "git init -q --bare ../$(basename $PWD)-remote.git");
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm c1 && echo b > b.txt && git add . && git commit -qm c2 && git remote add origin ../$(basename $PWD)-remote.git && git push -q origin main");
        sh(dir, "echo c > c.txt && git add . && git commit -qm c3 && git switch -qc side HEAD~1 && echo s > s.txt && git add . && git commit -qm side && git switch -q main && git merge -q --no-ff -m merged side");
    });
    let dir = root(&ws, cx);
    show_git(&ws, cx);
    settle(&ws, cx);
    let row = |rev: &str| format!("commit-row-{}", short(&dir, rev));
    let item = "menu-Fix up this commit with the staged changes";
    let on_menu = |cx: &mut VisualTestContext, rev: &str| {
        right_click(cx, &row(rev));
        cx.debug_bounds(sel(item)).is_some()
    };

    assert!(!on_menu(cx, "HEAD"), "not for a merge commit");
    cx.simulate_keystrokes("escape");
    assert!(on_menu(cx, "HEAD^1"));
    click(cx, item);
    assert!(flashed(&ws, cx).starts_with("Stage the changes"), "{}", flashed(&ws, cx));

    // Staged changes go into c3 (not pushed): no question.
    std::fs::write(dir.join("a.txt"), "a2\n").unwrap();
    sh(&dir, "git add a.txt");
    settle(&ws, cx);
    let before = git_out(&dir, &["rev-parse", "HEAD"]);
    menu(cx, &row("HEAD^1"), "Fix up this commit with the staged changes");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["log", "--format=%s", "--first-parent"]).lines().collect::<Vec<_>>(), ["merged", "c3", "c2", "c1"], "no fixup! commit is left");
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(git_out(&dir, &["show", "HEAD^1:a.txt"]), "a2", "folded into c3");
    assert_eq!(git_out(&dir, &["show", "HEAD^1~2:a.txt"]), "a", "c1 is as it was");
    assert!(toast(cx));
    click(cx, "undo-button");
    settle(&ws, cx);
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), before, "the history is as it was");
    assert_eq!(git_out(&dir, &["diff", "--cached", "--name-only"]), "a.txt", "and the changes are staged again");

    // c2 is on the remote: it asks, and does nothing until told to.
    menu(cx, &row("HEAD^1~1"), "Fix up this commit with the staged changes");
    let title = ws.read_with(cx, |ws, _| ws.ask.as_ref().map(|a| a.ask.title.clone())).expect("asks about a pushed commit");
    assert!(title.contains("already on a remote"), "{title}");
    assert_eq!(git_out(&dir, &["rev-parse", "HEAD"]), before);
    click(cx, "dialog-button-Fold In Anyway");
    settle(&ws, cx);
    assert_ne!(git_out(&dir, &["rev-parse", "HEAD"]), before, "rewritten");
    assert_eq!(git_out(&dir, &["show", "HEAD^1~1:a.txt"]), "a2");
}

/// A stand-in for `claude -p`: what it is asked is on its input (the staged diff).
const FAKE_CLAUDE: &str = r#"#!/bin/sh
in=$(cat)
case "$in" in
  *FAILME*) echo "not signed in" >&2; exit 1 ;;
  *SLOWME*) exec sleep 30 ;;
esac
[ "$1" = "-p" ] || exit 2
echo "Update $(echo "$in" | grep -m1 '^+++ b/' | cut -c7-)"
echo
echo "Because the tests say so."
"#;

#[gpui::test]
fn claude_drafts_the_commit_message_and_can_be_stopped(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "echo a > a.txt && git add . && git commit -qm one");
        let fake = dir.with_file_name(format!("{}-claude", dir.file_name().unwrap().to_string_lossy()));
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::write(fake.join("claude"), FAKE_CLAUDE).unwrap();
        sh(&fake, "chmod +x claude");
        // shortcut: process-wide; only this test reads it.
        unsafe { std::env::set_var("OSKL_CLAUDE", fake.join("claude")) };
    });
    let dir = root(&ws, cx);
    let wait = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, done: &dyn Fn(&mut VisualTestContext) -> bool| {
        for _ in 0..200 {
            settle(ws, cx);
            if done(cx) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("took too long");
    };
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    settle(&ws, cx);
    command(&ws, cx, "repo");
    let view = git_tab(&ws, cx);
    click(cx, "git-local");
    let msg = |view: &Entity<crate::git_panel::view::GitView>, cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.commit_msg.read(cx).text());
    let sidebar_drafting = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.git_panel.read(cx).drafter.running());

    // Nothing staged: it says so, without running Claude.
    click(cx, "git-draft");
    wait(&ws, cx, &|cx| flashed(&ws, cx).contains("Nothing is staged"));
    assert_eq!(msg(&view, cx), "");

    // Staged: the message fills the box, which stays editable.
    sh(&dir, "git add b.txt");
    settle(&ws, cx);
    click(cx, "git-draft");
    wait(&ws, cx, &|cx| !view.read_with(cx, |v, cx| v.commit_msg.read(cx).text().is_empty()));
    assert_eq!(msg(&view, cx), "Update b.txt\n\nBecause the tests say so.");
    click(cx, "git-commit-msg");
    cx.simulate_input("!");
    assert!(msg(&view, cx).contains('!'), "editable");

    // A failure is said plainly, and the message is left alone.
    std::fs::write(dir.join("c.txt"), "FAILME\n").unwrap();
    sh(&dir, "git add c.txt");
    settle(&ws, cx);
    let kept = msg(&view, cx);
    click(cx, "git-draft");
    wait(&ws, cx, &|cx| flashed(&ws, cx).contains("not signed in"));
    assert!(flashed(&ws, cx).contains("couldn't draft"), "{}", flashed(&ws, cx));
    assert_eq!(msg(&view, cx), kept);

    // A slow one shows progress and stops when clicked; the sidebar's button does the same.
    sh(&dir, "git reset -q c.txt");
    std::fs::write(dir.join("d.txt"), "SLOWME\n").unwrap();
    sh(&dir, "git add d.txt");
    show_git(&ws, cx);
    settle(&ws, cx);
    let _ = at(cx, "commit-draft");
    // Straight to what the button does: a click settles, which would wait for Claude to finish.
    let press = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.update(cx, |ws, cx| {
            let root = ws.git.read(cx).root.clone();
            ws.git_panel.update(cx, |p, cx| p.drafter.toggle(root, &p.commit_input, cx))
        })
    };
    press(&ws, cx);
    assert!(sidebar_drafting(&ws, cx), "it is working");
    press(&ws, cx);
    assert!(!sidebar_drafting(&ws, cx), "stopped");
    assert_eq!(flashed(&ws, cx), "Stopped drafting");
    assert_eq!(ws.read_with(cx, |ws, cx| ws.git_panel.read(cx).commit_input.read(cx).text()), "");

    // Claude Code missing: the plain reason.
    unsafe { std::env::set_var("OSKL_CLAUDE", dir.join("no-such-claude")) };
    click(cx, "commit-draft");
    wait(&ws, cx, &|cx| flashed(&ws, cx).contains("Couldn't run Claude Code"));

    // The button stays inside a narrow sidebar.
    ws.update(cx, |ws, _| ws.side_w = 180.);
    settle(&ws, cx);
    let draft = cx.debug_bounds("commit-draft").expect("the button");
    let edge = gpui::px((super::ACTIVITY_W + 180.) * crate::ui::scale());
    assert!(draft.right() <= edge && draft.left() >= gpui::px(0.), "{draft:?} inside {edge:?}");
    // Inside the message box, on its right, beside the message rather than under it.
    let bx = cx.debug_bounds("commit-box").expect("the message box");
    assert!(draft.left() > bx.left() && draft.right() <= bx.right() && draft.top() >= bx.top() && draft.bottom() <= bx.bottom(), "{draft:?} in {bx:?}");
    assert!(draft.right() > bx.right() - gpui::px(12.), "at the right edge: {draft:?} in {bx:?}");
    // A long message wraps onto more lines (the box grows down, never sideways).
    ws.update(cx, |ws, cx| ws.git_panel.update(cx, |p, cx| p.commit_input.update(cx, |e, cx| e.set_text(&"word ".repeat(60), cx))));
    settle(&ws, cx);
    cx.update(|window, _| window.refresh());
    settle(&ws, cx);
    let tall = cx.debug_bounds("commit-box").expect("the message box");
    assert!(tall.size.height > bx.size.height * 2., "grew: {tall:?} from {bx:?}");
    assert_eq!(tall.right(), bx.right(), "same width");
    // The whole width is used: as many letters as fit stay on one line, one more wraps.
    ws.update(cx, |ws, _| ws.side_w = 320.);
    let set = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, n: usize| {
        ws.update(cx, |ws, cx| ws.git_panel.update(cx, |p, cx| p.commit_input.update(cx, |e, cx| e.set_text(&"x".repeat(n), cx))));
        settle(ws, cx);
        cx.update(|window, _| window.refresh());
        settle(ws, cx);
        cx.debug_bounds("commit-box").expect("the message box").size.height
    };
    let one = set(&ws, cx, 1);
    let text_w = cx.debug_bounds("commit-text").expect("the text").size.width / gpui::px(1.);
    let cw = ws.read_with(cx, |ws, cx| ws.git_panel.read(cx).commit_input.read(cx).char_width()).expect("measured") * crate::ui::scale();
    let fit = ((text_w - 4. * crate::ui::scale()) / cw).floor() as usize;
    assert!(fit > 25, "{fit} letters in {text_w}");
    assert_eq!(set(&ws, cx, fit), one, "{fit} letters fit on one line");
    assert!(set(&ws, cx, fit + 1) > one, "one more wraps");
}

#[gpui::test]
fn the_editable_diff_can_show_only_the_changes(cx: &mut gpui::TestAppContext) {
    use crate::editor::DRow;
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "seq 1 40 > a.txt && git add . && git commit -qm base");
        sh(dir, "seq 1 40 | sed -e 's/^2$/two/' -e 's/^30$/thirty/' > a.txt");
    });
    cx.run_until_parked();
    ws.update_in(cx, |ws, window, cx| {
        let spec = DiffSpec { path: "a.txt".into(), old_path: None, left: Rev::Index, right: Rev::Working, back: None };
        ws.handle_request(&Request::OpenDiff { spec, preview: false }, window, cx)
    });
    cx.run_until_parked();
    let d = ws.read_with(cx, |ws, _| match &ws.tabs[ws.active.unwrap()].kind {
        TabKind::DiffEdit(d) => d.clone(),
        _ => panic!("an editable diff"),
    });
    let gaps = |cx: &mut VisualTestContext| {
        d.read_with(cx, |d, cx| {
            let side = |e: &Entity<crate::editor::Editor>| e.read(cx).decor.as_ref().map(|x| x.rows.iter().filter_map(|r| if let DRow::Gap(n) = r { Some(*n) } else { None }).collect::<Vec<_>>()).unwrap_or_default();
            let rows = |e: &Entity<crate::editor::Editor>| e.read(cx).decor.as_ref().map_or(0, |x| x.rows.len());
            (side(&d.left), side(&d.right), rows(&d.left), rows(&d.right))
        })
    };
    assert_eq!(gaps(cx).1, Vec::<usize>::new(), "the whole file at first");
    click(cx, "de-changes-only");
    let (left, right, lrows, rrows) = gaps(cx);
    // Lines 1–5 stay (2 changed, 3 around it); 6–26 and 34–40 fold away.
    assert_eq!(right, vec![21, 7]);
    assert_eq!(left, right, "both sides fold the same lines, so they stay aligned");
    assert_eq!(lrows, rrows);
    click(cx, "de-changes-only");
    assert_eq!(gaps(cx).1, Vec::<usize>::new(), "the whole file again");
}

#[gpui::test]
fn every_diff_can_show_only_the_changes_open_ones_follow_and_it_is_saved(cx: &mut gpui::TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        sh(dir, INIT);
        sh(dir, "seq 1 40 > a.txt && seq 1 40 > b.txt && git add . && git commit -qm base");
        sh(dir, "seq 1 40 | sed -e 's/^2$/two/' -e 's/^30$/thirty/' > a.txt && git add a.txt");
        sh(dir, "seq 1 40 | sed -e 's/^20$/twenty/' > b.txt");
    });
    cx.run_until_parked();
    let open = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, path: &str, left: Rev, right: Rev| {
        ws.update_in(cx, |ws, window, cx| {
            let spec = DiffSpec { path: path.into(), old_path: None, left, right, back: None };
            ws.handle_request(&Request::OpenDiff { spec, preview: false }, window, cx)
        });
        cx.run_until_parked();
    };
    let diff = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| ws.read_with(cx, |ws, _| match &ws.tabs[ws.active.unwrap()].kind {
        TabKind::Diff(d) => (Some(d.clone()), None),
        TabKind::DiffEdit(d) => (None, Some(d.clone())),
        _ => panic!("a diff"),
    });
    // An editable diff stays open while a staged (read-only) one is switched.
    open(&ws, cx, "b.txt", Rev::Index, Rev::Working);
    let edit = diff(&ws, cx).1.unwrap();
    open(&ws, cx, "a.txt", Rev::Commit("HEAD".into()), Rev::Index);
    let staged = diff(&ws, cx).0.unwrap();
    assert!(staged.read_with(cx, |d, _| d.gap_rows()).is_empty(), "the whole file at first");
    click(cx, "toggle-changes-only");
    assert_eq!(staged.read_with(cx, |d, _| d.gap_rows()), vec![21, 7], "lines 6–26 and 34–40 fold away");
    assert!(edit.read_with(cx, |d, cx| d.right.read(cx).changes_only), "the diff already open follows");
    let saved = || std::fs::read_to_string(std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config/oskl/settings")).unwrap();
    assert!(saved().contains("changesonly = on"), "kept for the next launch");
    // Switched back from the other diff: both show the whole file again.
    ws.update_in(cx, |ws, window, cx| {
        let ix = ws.tabs.iter().position(|t| matches!(&t.kind, TabKind::DiffEdit(_))).unwrap();
        ws.activate(ix, window, cx);
    });
    click(cx, "de-changes-only");
    assert!(staged.read_with(cx, |d, _| d.gap_rows()).is_empty());
    assert!(!crate::editor::diff_changes_only() && saved().contains("changesonly = off"));
}
