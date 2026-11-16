//! The window shell from the UI: title bar, breadcrumb, start screens, zen mode, the ⌘K hints
//! and narrow windows, on real temporary repositories.

use super::super::ui_tests::setup_with;
use super::super::{CmdMode, Side, TabKind, Workspace};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use std::path::Path;

fn sh(dir: &Path, cmd: &str) {
    std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
}

/// A repository with two committed files, `src/a/b.rs` modified afterwards.
fn repo(dir: &Path) {
    sh(dir, "git init -q -b main && git config user.email t@t && git config user.name t && git config commit.gpgsign false");
    sh(dir, "mkdir -p src/a && echo 'fn main() {}' > src/a/b.rs && printf '# Title\\n' > notes.md && git add . && git commit -qm init && echo '// more' >> src/a/b.rs");
}

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

fn shown(cx: &mut VisualTestContext, s: &str) -> bool {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(sel(s)).is_some()
}

fn open(ws: &Entity<Workspace>, cx: &mut VisualTestContext, rel: &str) {
    let root = ws.read_with(cx, |ws, cx| ws.root(cx));
    ws.update_in(cx, |ws, window, cx| ws.open_file(root.join(rel), None, false, window, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn title_bar_opens_the_command_line_and_toggles_panels(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    cx.run_until_parked();
    assert!(shown(cx, "title-bar"));
    click(cx, "title-cmd");
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode == Some(CmdMode::Files)));
    // A ":" at the start makes it the command line.
    ws.update(cx, |ws, cx| ws.cmd.update(cx, |c, cx| c.set_text(":zen", cx)));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode == Some(CmdMode::Command)));
    assert_eq!(ws.read_with(cx, |ws, cx| ws.cmd.read(cx).text()), "zen");
    ws.update_in(cx, |ws, window, cx| ws.cancel_cmd(window, cx));

    let side = ws.read_with(cx, |ws, _| ws.side_visible);
    click(cx, "title-sidebar");
    assert_eq!(ws.read_with(cx, |ws, _| ws.side_visible), !side);
    let out = ws.read_with(cx, |ws, _| ws.output_visible);
    click(cx, "title-panel");
    assert_eq!(ws.read_with(cx, |ws, _| ws.output_visible), !out);
    click(cx, "title-split");
    assert_eq!(ws.read_with(cx, |ws, _| ws.panes.len()), 2);
}

#[gpui::test]
fn project_switcher_lists_recent_folders_and_closes_on_an_outside_click(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    // Not under the temp folder, which the lists leave out.
    let recent = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    ws.update(cx, |ws, _| ws.settings.recent_folders = vec![recent]);
    cx.run_until_parked();
    assert!(!shown(cx, "project-menu"));
    click(cx, "title-project");
    for id in ["project-menu", "project-recent-0", "project-open", "project-new-window"] {
        assert!(shown(cx, id), "{id}");
    }
    // Clicking the button again closes it (the press is not an outside click that reopens it).
    click(cx, "title-project");
    assert!(!shown(cx, "project-menu"));
    click(cx, "title-project");
    let pane = at(cx, "start-screen-0");
    cx.simulate_click(pane + gpui::point(gpui::px(0.), gpui::px(-200.)), Modifiers::none());
    cx.run_until_parked();
    assert!(!shown(cx, "project-menu"), "an outside click closes it");
}

#[gpui::test]
fn breadcrumb_shows_the_path_and_the_files_actions(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    cx.run_until_parked();
    assert!(!shown(cx, "crumb-0"), "nothing open: no breadcrumb");
    open(&ws, cx, "src/a/b.rs");
    assert!(shown(cx, "crumb-0") && shown(cx, "crumb-symbol-0"), "the path, and the slot for the symbol");
    assert!(!shown(cx, "crumb-preview-0"), "no preview for Rust");

    // Changed against the index: Changes opens the diff.
    click(cx, "crumb-changes-0");
    assert!(ws.read_with(cx, |ws, _| matches!(ws.tabs[ws.active.unwrap()].kind, TabKind::DiffEdit(_) | TabKind::Diff(_))));
    open(&ws, cx, "src/a/b.rs");

    click(cx, "crumb-blame-0");
    assert!(ws.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).blame_gutter));
    // The choice is remembered across windows: put it back for the other tests.
    click(cx, "crumb-blame-0");
    assert!(!ws.read_with(cx, |ws, cx| ws.active_editor().unwrap().read(cx).blame_gutter));
    click(cx, "crumb-find-0");
    assert!(ws.read_with(cx, |ws, _| ws.find_on.is_some()));
    click(cx, "crumb-history-0");
    assert!(ws.read_with(cx, |ws, _| ws.side == Side::Git));

    // An unchanged Markdown file: no Changes, but a preview.
    open(&ws, cx, "notes.md");
    assert!(!shown(cx, "crumb-changes-0") && shown(cx, "crumb-preview-0"));
    click(cx, "crumb-preview-0");
    assert!(ws.read_with(cx, |ws, _| matches!(ws.tabs[ws.active.unwrap()].kind, TabKind::Preview(_))));
    assert!(!shown(cx, "crumb-0"), "a preview has no breadcrumb");
}

#[gpui::test]
fn empty_pane_and_split_say_what_to_do_next(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    cx.run_until_parked();
    assert!(shown(cx, "start-screen-0"));
    click(cx, "start-split-0");
    let panes: Vec<usize> = ws.read_with(cx, |ws, _| ws.panes.iter().map(|p| p.id).collect());
    assert_eq!(panes.len(), 2);
    for id in &panes {
        assert!(shown(cx, &format!("start-screen-{id}")), "an empty split offers the same ways to start");
    }
    click(cx, &format!("start-file-{}", panes[1]));
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode == Some(CmdMode::Files) && ws.focused_pane == panes[1]));
}

#[gpui::test]
fn search_and_source_control_say_so_when_empty(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        repo(dir);
        sh(dir, "git checkout -q -- src/a/b.rs");
    });
    cx.run_until_parked();
    ws.update_in(cx, |ws, window, cx| ws.search_all(&super::super::SearchAll, window, cx));
    assert!(shown(cx, "search-empty"), "before a search: what to type");
    ws.update(cx, |ws, cx| {
        ws.search.update(cx, |s, cx| {
            s.set_query("zzzz-not-there", cx);
        })
    });
    for _ in 0..20 {
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }
    assert!(shown(cx, "search-empty"), "no results: what to try");

    ws.update_in(cx, |ws, window, cx| ws.show_git(&super::super::ShowGit, window, cx));
    cx.run_until_parked();
    assert!(shown(cx, "git-clean"), "a clean tree says nothing is to be committed");
    let root = ws.read_with(cx, |ws, cx| ws.root(cx));
    std::fs::write(root.join("new.txt"), "x").unwrap();
    ws.update(cx, |ws, cx| ws.repo.update(cx, |r, cx| r.refresh_all(cx)));
    for _ in 0..20 {
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }
    assert!(!shown(cx, "git-clean"));
}

#[gpui::test]
fn zen_mode_leaves_only_the_editor_and_esc_brings_it_back(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    open(&ws, cx, "src/a/b.rs");
    for id in ["title-bar", "act-files", "side-panel", "status-branch"] {
        assert!(shown(cx, id), "{id} before");
    }
    cx.simulate_keystrokes("cmd-k z");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.shell.zen));
    for id in ["title-bar", "act-files", "side-panel", "status-branch"] {
        assert!(!shown(cx, id), "{id} hidden");
    }
    assert!(shown(cx, "tab-row-0") && shown(cx, "zen-note"), "the editor's tabs stay, and a note says how to leave");
    // Esc in the editor leaves it.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!ws.read_with(cx, |ws, _| ws.shell.zen));
    assert!(shown(cx, "title-bar") && shown(cx, "act-files"));

    // With the keyboard somewhere else, Esc still does; and the :zen command is the same switch.
    ws.update_in(cx, |ws, window, cx| ws.run_command("zen", window, cx));
    assert!(ws.read_with(cx, |ws, _| ws.shell.zen));
    ws.update_in(cx, |ws, window, cx| window.focus(&ws.focus, cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!ws.read_with(cx, |ws, _| ws.shell.zen));
    cx.simulate_keystrokes("cmd-k z cmd-k z");
    assert!(!ws.read_with(cx, |ws, _| ws.shell.zen), "the same keys bring it back");
}

#[gpui::test]
fn cmd_k_lists_what_can_follow_and_stops_when_done(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    open(&ws, cx, "src/a/b.rs");
    cx.simulate_keystrokes("cmd-k");
    cx.run_until_parked();
    assert!(!shown(cx, "chord-hints"), "a quick chord stays quiet");
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    cx.run_until_parked();
    assert!(shown(cx, "chord-hints"));
    // Read from the real bindings.
    let rows = cx.update(|window, _| crate::keybindings::follow_ups(window));
    assert!(rows.iter().any(|(k, d)| k == "Z" && d.contains("Zen")), "{rows:?}");
    assert!(rows.iter().any(|(k, _)| k == "V"), "{rows:?}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown(cx, "chord-hints"), "cancelled");
    cx.simulate_keystrokes("cmd-k");
    cx.executor().advance_clock(std::time::Duration::from_millis(600));
    cx.run_until_parked();
    assert!(shown(cx, "chord-hints"));
    cx.simulate_keystrokes("z");
    cx.run_until_parked();
    assert!(!shown(cx, "chord-hints"), "completed");
    assert!(ws.read_with(cx, |ws, _| ws.shell.zen));
}

#[gpui::test]
fn narrow_window_lays_the_sidebar_over_the_editor_and_stacks_splits(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    open(&ws, cx, "src/a/b.rs");
    ws.update(cx, |ws, _| ws.side_visible = true);
    cx.simulate_resize(gpui::size(gpui::px(1100.), gpui::px(700.)));
    cx.run_until_parked();
    let (side, pane) = (cx.debug_bounds("side-panel").unwrap(), ws.read_with(cx, |ws, _| ws.pane_bounds.borrow()[&0]));
    assert!(pane.left() >= side.right(), "wide: beside the editor");

    cx.simulate_resize(gpui::size(gpui::px(700.), gpui::px(700.)));
    cx.run_until_parked();
    cx.update(|w, _| w.refresh());
    cx.run_until_parked();
    let (side, pane) = (cx.debug_bounds("side-panel").unwrap(), ws.read_with(cx, |ws, _| ws.pane_bounds.borrow()[&0]));
    assert!(pane.left() < side.right() && side.right() <= gpui::px(700.), "narrow: over the editor, inside the window");
    // A click outside closes it.
    cx.simulate_click(gpui::point(gpui::px(650.), gpui::px(300.)), Modifiers::none());
    cx.run_until_parked();
    assert!(!ws.read_with(cx, |ws, _| ws.side_visible));

    // Splits stack when narrower still.
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(700.)));
    click(cx, "title-split");
    cx.update(|w, _| w.refresh());
    cx.run_until_parked();
    let sides = ws.read_with(cx, |ws, _| ws.pane_bounds.borrow().values().copied().collect::<Vec<_>>());
    assert_eq!(sides.len(), 2);
    assert!(sides[0].top() == sides[1].top(), "side by side while there is room");
    cx.simulate_resize(gpui::size(gpui::px(620.), gpui::px(700.)));
    cx.run_until_parked();
    cx.update(|w, _| w.refresh());
    cx.run_until_parked();
    let b = ws.read_with(cx, |ws, _| ws.pane_bounds.borrow().values().copied().collect::<Vec<_>>());
    assert!((b[0].left() - b[1].left()).abs() < gpui::px(1.) && (b[0].top() - b[1].top()).abs() > gpui::px(50.), "stacked: {b:?}");
}

#[gpui::test]
fn window_color_is_picked_in_the_switcher_typed_as_hex_and_kept_with_the_project(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    cx.run_until_parked();
    click(cx, "title-project");
    assert!(shown(cx, "window-color-none") && shown(cx, "window-color-other"));
    click(cx, "window-color-teal");
    assert_eq!(ws.read_with(cx, |ws, _| ws.shell.color), Some(0x3f8f8a));
    assert_ne!(ws.read_with(cx, |ws, _| ws.chrome_bg()), crate::theme::bg_side(), "the title and status bars take the tint");
    let root = ws.read_with(cx, |ws, cx| ws.root(cx));
    assert_eq!(crate::session::load(&root).and_then(|s| s.window_color), Some(0x3f8f8a), "saved with the project");
    // Custom… opens a hex field in the menu: the bars follow it, Esc puts the old color back.
    let input = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| ws.read_with(cx, |ws, _| ws.shell.color_input.as_ref().map(|(i, _, _)| i.clone())).expect("the hex field is open");
    let emit = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, ev: crate::editor::EditorEvent| {
        let i = input(ws, cx);
        i.update(cx, |_, cx| cx.emit(ev));
        cx.run_until_parked();
    };
    click(cx, "window-color-other");
    assert!(shown(cx, "window-color-input"));
    assert_eq!(input(&ws, cx).read_with(cx, |e, _| e.text()), "#3f8f8a");
    input(&ws, cx).update(cx, |e, cx| e.set_text("#123456", cx));
    emit(&ws, cx, crate::editor::EditorEvent::Changed);
    assert_eq!(ws.read_with(cx, |ws, _| ws.shell.color), Some(0x123456), "the bars preview it while typing");
    emit(&ws, cx, crate::editor::EditorEvent::Cancel);
    assert_eq!(ws.read_with(cx, |ws, _| ws.shell.color), Some(0x3f8f8a), "Esc puts the old color back");
    click(cx, "title-project");
    click(cx, "window-color-other");
    input(&ws, cx).update(cx, |e, cx| e.set_text("#a1b2c3", cx));
    emit(&ws, cx, crate::editor::EditorEvent::Submit);
    assert_eq!(ws.read_with(cx, |ws, _| ws.shell.color), Some(0xa1b2c3));
    assert!(ws.read_with(cx, |ws, _| ws.shell.color_input.is_none()), "Enter keeps it and closes the field");
    assert_eq!(crate::session::load(&root).and_then(|s| s.window_color), Some(0xa1b2c3));
    click(cx, "title-project");
    assert!(shown(cx, "window-color-custom"), "a custom color gets its own swatch");
    ws.update_in(cx, |ws, window, cx| ws.run_command("window-color nope", window, cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.shell.color), Some(0xa1b2c3), "a bad color changes nothing");
    ws.update_in(cx, |ws, window, cx| ws.run_command("window-color off", window, cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.shell.color), None);
}
