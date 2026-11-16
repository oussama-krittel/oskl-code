//! Go to symbol, the problems panel and the change peek, on real temporary repositories.

use super::super::ui_tests::setup_with;
use super::super::{CmdMode, TabKind, Workspace};
use crate::editor::{Diag, Editor, Severity};
use gpui::{Entity, Modifiers, Point, TestAppContext, VisualTestContext};
use std::path::Path;
use std::time::Duration;

const LIB: &str = "fn alpha() {\n    1\n}\n\nstruct Beta;\n\nimpl Beta {\n    fn gamma(&self) {\n        2\n    }\n}\n";

fn sh(dir: &Path, cmd: &str) {
    std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
}

fn out(dir: &Path, cmd: &str) -> String {
    String::from_utf8_lossy(&std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap().stdout).to_string()
}

/// `lib.rs` committed, then two separate changes made in the working tree (rows 1 and 8).
fn repo(dir: &Path) {
    sh(dir, "git init -q -b main && git config user.email t@t && git config user.name t && git config commit.gpgsign false");
    std::fs::write(dir.join("lib.rs"), LIB).unwrap();
    sh(dir, "git add . && git commit -qm init");
    std::fs::write(dir.join("lib.rs"), LIB.replace("    1\n", "    10\n").replace("        2\n", "        20\n")).unwrap();
}

fn sel(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn shown(cx: &mut VisualTestContext, s: &str) -> bool {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(sel(s)).is_some()
}

fn click(cx: &mut VisualTestContext, s: &str) {
    assert!(shown(cx, s), "{s} is on screen");
    let p = cx.debug_bounds(sel(s)).unwrap().center();
    cx.simulate_click(p, Modifiers::none());
    cx.run_until_parked();
}

fn open(ws: &Entity<Workspace>, cx: &mut VisualTestContext, rel: &str) -> Entity<Editor> {
    let root = ws.read_with(cx, |ws, cx| ws.root(cx));
    ws.update_in(cx, |ws, window, cx| ws.open_file(root.join(rel), None, false, window, cx));
    cx.run_until_parked();
    ws.read_with(cx, |ws, _| ws.active_editor()).unwrap()
}

/// Where the change mark of `row` is, once the index has been read and the editor drawn.
fn mark(cx: &mut VisualTestContext, ed: &Entity<Editor>, row: usize) -> Point<gpui::Pixels> {
    for _ in 0..200 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if let Some(p) = ed.read_with(cx, |e, _| e.mark_spot(row)) {
            return p;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("no change mark on row {row}");
}

fn text(cx: &mut VisualTestContext, ed: &Entity<Editor>) -> String {
    ed.read_with(cx, |e, _| e.text())
}

#[gpui::test]
fn clicking_a_change_mark_peeks_at_the_old_lines_and_esc_closes_it(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let ed = open(&ws, cx, "lib.rs");
    let spot = mark(cx, &ed, 1);
    assert!(!shown(cx, "peek-card"));
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    assert!(ed.read_with(cx, |e, _| e.peek_open()) && shown(cx, "peek-card"), "the card opens under the line");
    for id in ["peek-stage", "peek-revert", "peek-diff", "peek-old", "peek-new"] {
        assert!(shown(cx, id), "{id}");
    }
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!ed.read_with(cx, |e, _| e.peek_open()), "Esc closes it");

    // The same mark again: open, then closed by a second click on it.
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    assert!(ed.read_with(cx, |e, _| e.peek_open()));
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    assert!(!ed.read_with(cx, |e, _| e.peek_open()), "the mark again closes it");

    // A click in the text closes it too, and a line without a change has no card.
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    let in_text = spot + gpui::point(gpui::px(120.), gpui::px(0.));
    cx.simulate_click(in_text, Modifiers::none());
    cx.run_until_parked();
    assert!(!ed.read_with(cx, |e, _| e.peek_open()), "clicking elsewhere closes it");
    assert!(ed.read_with(cx, |e, _| e.mark_spot(2)).is_none(), "row 2 has no mark");
}

#[gpui::test]
fn the_peek_shows_the_new_lines_and_hangs_under_the_last_changed_line(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, |dir| {
        repo(dir);
        // Row 1 becomes four lines.
        std::fs::write(dir.join("lib.rs"), LIB.replace("    1\n", "    a\n    b\n    c\n    d\n")).unwrap();
    });
    let ed = open(&ws, cx, "lib.rs");
    let spot = mark(cx, &ed, 1);
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    assert!(shown(cx, "peek-new") && shown(cx, "peek-old"));
    let last = ed.read_with(cx, |e, _| e.mark_spot(4)).expect("a mark on the last new line");
    let card = cx.debug_bounds("peek-card").unwrap();
    assert!(card.top() >= last.y, "the card starts under the last changed line, not over it");
    let new_rows = cx.debug_bounds("peek-new").unwrap().size.height;
    let old_rows = cx.debug_bounds("peek-old").unwrap().size.height;
    assert!(new_rows > old_rows, "four added rows against one removed");
}

#[gpui::test]
fn the_peek_stages_only_its_change(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let root = ws.read_with(cx, |ws, cx| ws.root(cx));
    let ed = open(&ws, cx, "lib.rs");
    let spot = mark(cx, &ed, 1);
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    click(cx, "peek-stage");
    assert!(!ed.read_with(cx, |e, _| e.peek_open()), "staging closes the card");
    let staged = out(&root, "git diff --cached");
    assert!(staged.contains("+    10") && !staged.contains("20"), "only the first change is staged: {staged}");
    let unstaged = out(&root, "git diff");
    assert!(unstaged.contains("+        20") && !unstaged.contains("+    10"), "the other stays unstaged: {unstaged}");
    assert_eq!(text(cx, &ed), LIB.replace("    1\n", "    10\n").replace("        2\n", "        20\n"), "the file itself is untouched");
}

#[gpui::test]
fn the_peek_reverts_its_change_as_an_edit_that_can_be_undone(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let ed = open(&ws, cx, "lib.rs");
    let both = text(cx, &ed);
    let spot = mark(cx, &ed, 8);
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    click(cx, "peek-revert");
    assert_eq!(text(cx, &ed), both.replace("        20\n", "        2\n"), "the old line is back, the other change stays");
    assert!(!ed.read_with(cx, |e, _| e.peek_open()));
    ed.update(cx, |e, _| assert!(e.buf.undo()));
    assert_eq!(text(cx, &ed), both, "undo brings the change back");
}

#[gpui::test]
fn the_peek_opens_the_editable_diff(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let ed = open(&ws, cx, "lib.rs");
    let spot = mark(cx, &ed, 1);
    cx.simulate_click(spot, Modifiers::none());
    cx.run_until_parked();
    click(cx, "peek-diff");
    assert!(ws.read_with(cx, |ws, _| matches!(ws.tabs[ws.active.unwrap()].kind, TabKind::DiffEdit(_))), "Index against Working, editable");
}

#[gpui::test]
fn go_to_symbol_lists_filters_and_jumps(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let ed = open(&ws, cx, "lib.rs");
    cx.simulate_keystrokes("cmd-shift-o");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode) == Some(CmdMode::Symbols), "⌘⇧O opens the symbol list");
    let labels = |cx: &mut VisualTestContext| ws.read_with(cx, |ws, _| ws.cmd_items.iter().map(|i| i.label.clone()).collect::<Vec<_>>());
    assert_eq!(labels(cx), ["alpha", "Beta", "Beta", "gamma"], "in file order");
    ws.update(cx, |ws, cx| ws.cmd.update(cx, |c, cx| c.set_text("gm", cx)));
    cx.run_until_parked();
    assert_eq!(labels(cx), ["gamma"], "fuzzy filtering");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode) == None);
    assert_eq!(ed.read_with(cx, |e, _| e.cursor_rc()), (7, 7), "the cursor is on the name");

    // "@" first in the file finder is the same list.
    ws.update_in(cx, |ws, window, cx| ws.open_cmd(CmdMode::Files, "", window, cx));
    ws.update(cx, |ws, cx| ws.cmd.update(cx, |c, cx| c.set_text("@alp", cx)));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode) == Some(CmdMode::Symbols));
    assert_eq!(ws.read_with(cx, |ws, cx| ws.cmd.read(cx).text()), "alp");
    assert_eq!(labels(cx), ["alpha"]);
    ws.update_in(cx, |ws, window, cx| ws.cancel_cmd(window, cx));
}

#[gpui::test]
fn the_breadcrumb_names_the_symbol_the_cursor_is_in(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let ed = open(&ws, cx, "lib.rs");
    let width = |cx: &mut VisualTestContext| {
        assert!(shown(cx, "crumb-symbol-0"));
        cx.debug_bounds("crumb-symbol-0").unwrap().size.width
    };
    ed.update(cx, |e, cx| e.go_to(8, 0, cx));
    assert!(width(cx) > gpui::px(40.), "inside gamma: its name is there");
    ed.update(cx, |e, cx| e.go_to(3, 0, cx));
    assert!(width(cx) < gpui::px(2.), "between items: nothing");
    ed.update(cx, |e, cx| e.go_to(8, 0, cx));
    // A click on it opens the list.
    click(cx, "crumb-symbol-0");
    assert!(ws.read_with(cx, |ws, _| ws.cmd_mode) == Some(CmdMode::Symbols));
}

fn diag(row: usize, col: usize, severity: Severity, message: &str, source: &str) -> Diag {
    Diag { row, col, end_row: row, end_col: 12, severity, message: message.into(), source: source.into() }
}

#[gpui::test]
fn the_problems_panel_lists_problems_and_the_status_counts_and_command_open_it(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let root = ws.read_with(cx, |ws, cx| ws.root(cx));
    let ed = open(&ws, cx, "lib.rs");
    ws.update(cx, |ws, cx| {
        ws.lint_diags.insert(root.join("lib.rs"), vec![diag(7, 7, Severity::Warning, "'gamma' is never used", "eslint")]);
        ws.ts_diags.insert(root.join("lib.rs"), vec![diag(1, 3, Severity::Error, "Cannot find name 'x'. [TS2304]", "ts")]);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!ws.read_with(cx, |ws, _| ws.output_visible && ws.shell.problems));

    click(cx, "status-problems");
    assert!(ws.read_with(cx, |ws, _| ws.output_visible && ws.shell.problems), "the status counts open the Problems panel");
    assert!(shown(cx, "problems-tab") && shown(cx, "problem-0") && shown(cx, "problem-1") && !shown(cx, "problem-2"));
    // Errors come first; a click jumps to the line and column.
    click(cx, "problem-0");
    assert_eq!(ed.read_with(cx, |e, _| e.cursor_rc()), (1, 3));
    click(cx, "problem-1");
    assert_eq!(ed.read_with(cx, |e, _| e.cursor_rc()), (7, 7));

    // The Output tab and back; the counts close it when it is already showing the problems.
    click(cx, "output-tab");
    assert!(!ws.read_with(cx, |ws, _| ws.shell.problems) && !shown(cx, "problem-0"));
    click(cx, "problems-tab");
    assert!(shown(cx, "problem-0"));
    click(cx, "status-problems");
    assert!(!ws.read_with(cx, |ws, _| ws.output_visible));

    // `:problems` opens that same panel.
    ws.update_in(cx, |ws, window, cx| ws.run_command("problems", window, cx));
    assert!(ws.read_with(cx, |ws, _| ws.output_visible && ws.shell.problems));
    assert!(shown(cx, "problem-0"));

    // Nothing found: it says so.
    ws.update(cx, |ws, cx| {
        ws.lint_diags.clear();
        ws.ts_diags.clear();
        cx.notify();
    });
    assert!(shown(cx, "problems-empty") && !shown(cx, "problem-0"));
}

#[gpui::test]
fn symbol_and_problems_stay_on_screen_in_a_narrow_window(cx: &mut TestAppContext) {
    let (ws, cx) = setup_with(cx, repo);
    let ed = open(&ws, cx, "lib.rs");
    ed.update(cx, |e, cx| e.go_to(8, 0, cx));
    ws.update(cx, |ws, cx| {
        let p = ws.root(cx).join("lib.rs");
        ws.lint_diags.insert(p, vec![diag(7, 7, Severity::Warning, "A message long enough that it has to be cut to fit in a narrow window and not push the place off", "eslint")]);
        ws.show_problems(cx);
    });
    cx.simulate_resize(gpui::size(gpui::px(620.), gpui::px(600.)));
    for id in ["crumb-symbol-0", "problems-tab", "problem-0"] {
        assert!(shown(cx, id), "{id}");
        let b = cx.debug_bounds(sel(id)).unwrap();
        assert!(b.left() >= gpui::px(0.) && b.right() <= gpui::px(620.), "{id} inside the window: {b:?}");
    }
}
