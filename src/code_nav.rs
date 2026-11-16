//! Moving around code: go to a symbol of the file (⌘⇧O, or @ in the file finder), the problems
//! panel, and the editable diff the change peek opens. A child of `workspace`, so it works on
//! the same private state.

use super::*;
use crate::editor::{Diag, Severity};

impl Workspace {
    // ------------------------------------------------------------------ go to symbol

    pub(super) fn go_to_symbol(&mut self, _: &GoToSymbol, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_editor().is_none() {
            return self.flash("Open a file to list its symbols".into(), false, cx);
        }
        self.open_cmd(CmdMode::Symbols, "", window, cx);
    }

    /// The active file's symbols that match: in file order until something is typed, then best first.
    pub(super) fn symbol_items(&self, query: &str, cx: &App) -> Vec<CmdItem> {
        let Some(editor) = self.active_editor() else { return Vec::new() };
        let symbols = editor.read(cx).symbols();
        let mut hits: Vec<(i64, usize, Vec<usize>)> = symbols.iter().enumerate().filter_map(|(i, s)| util::fuzzy(query, &s.name).map(|(score, pos)| (score, i, pos))).collect();
        if !query.is_empty() {
            hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        hits.into_iter()
            .take(300)
            .map(|(_, i, positions)| {
                let s = &symbols[i];
                CmdItem { label: s.name.clone(), detail: format!("{} · line {}", s.kind, s.row + 1), value: format!("{}:{}", s.row, s.col), positions, exec: true }
            })
            .collect()
    }

    pub(super) fn go_to_symbol_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.cmd_items.get(self.cmd_sel).and_then(|i| i.value.split_once(':')).and_then(|(r, c)| Some((r.parse::<usize>().ok()?, c.parse::<usize>().ok()?)));
        self.close_cmd(window, cx);
        if let (Some((row, col)), Some(ed)) = (at, self.active_editor()) {
            ed.update(cx, |e, cx| e.go_to(row, col, cx));
            window.focus(&ed.read(cx).focus_target(cx), cx);
        }
    }

    // ------------------------------------------------------------------ problems

    /// Every problem the linters, the type checker and the data-file checks found: errors first,
    /// then by file and line.
    pub(super) fn problem_rows(&self) -> Vec<(PathBuf, Diag)> {
        let mut rows: Vec<(PathBuf, Diag)> = self.lint_diags.iter().chain(self.ts_diags.iter()).chain(self.data_diags.iter()).flat_map(|(p, ds)| ds.iter().map(move |d| (p.clone(), d.clone()))).collect();
        rows.sort_by(|a, b| (a.1.severity, &a.0, a.1.row, a.1.col).cmp(&(b.1.severity, &b.0, b.1.row, b.1.col)));
        rows
    }

    /// The status bar's counts and `:problems` both come here: the bottom panel, on its Problems tab.
    pub(super) fn show_problems(&mut self, cx: &mut Context<Self>) {
        self.shell.problems = true;
        self.output_visible = true;
        self.output_sel = 0;
        self.output_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// The status bar's counts: open the panel, or close it when it already shows the problems.
    pub(super) fn toggle_problems(&mut self, cx: &mut Context<Self>) {
        if self.shell.problems && self.output_visible {
            self.output_visible = false;
            cx.notify();
        } else {
            self.show_problems(cx);
        }
    }

    pub(super) fn open_problem(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, d)) = self.problem_rows().into_iter().nth(ix) else { return };
        self.output_sel = ix;
        self.open_file(path, Some(d.row + 1), false, window, cx);
        if let Some(ed) = self.active_editor() {
            ed.update(cx, |e, cx| e.go_to(d.row, d.col, cx));
        }
    }

    /// The panel's rows: severity, message, and where it is.
    pub(super) fn render_problems(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = Rc::new(self.problem_rows());
        if rows.is_empty() {
            return div()
                .id("problems-empty")
                .debug_selector(|| "problems-empty".into())
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(16.))
                .text_size(px(12.))
                .text_color(theme::fg_muted())
                .child("No problems found. Lint runs when a file opens or saves; TypeScript is checked when a .ts or .tsx file saves.")
                .into_any_element();
        }
        let root = self.root(cx);
        uniform_list(
            "problems",
            rows.len(),
            cx.processor(move |ws, range: Range<usize>, window, cx| {
                range
                    .filter_map(|ix| {
                        let (path, d) = rows.get(ix)?;
                        let selected = ix == ws.output_sel && ws.output_focus.is_focused(window);
                        let place = format!("{}:{}", path.strip_prefix(&root).unwrap_or(path).to_string_lossy(), d.row + 1);
                        let sel = format!("problem-{ix}");
                        Some(
                            div()
                                .id(("problem", ix))
                                .debug_selector(move || sel)
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .w_full()
                                .min_w_0()
                                .overflow_hidden()
                                .h(px(24.))
                                .px(px(12.))
                                .text_size(px(12.))
                                .text_color(theme::fg())
                                .cursor_pointer()
                                .when(selected, |r| r.bg(theme::selected_focused()))
                                .when(!selected, |r| r.hover(|s| s.bg(theme::hover())))
                                .on_click(cx.listener(move |ws, _, window, cx| ws.open_problem(ix, window, cx)))
                                .child(ui::small_icon(if d.severity == Severity::Error { "error" } else { "warning" }, d.severity.color()))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(crate::tserror::label(&d.message, d.source == "ts")))
                                .child(div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).child(d.source.clone()))
                                .child(div().flex_none().max_w(relative(0.5)).overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_dim()).child(place))
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.output_scroll)
        .flex_1()
        .into_any_element()
    }

    // ------------------------------------------------------------------ the editable diff of a file

    /// The change peek's "Open diff": the file's changes against the index, editable.
    pub(super) fn open_changes_of(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = editor.read(cx).path.clone() else { return };
        let repo = self.repo_for(&path, cx);
        let root = repo.read(cx).root.clone();
        let Some(rel) = path.strip_prefix(&root).ok().map(|p| p.to_string_lossy().to_string()) else { return };
        if self.git_root(cx) != root {
            self.select_repo(&root, window, cx);
        }
        self.open_diff(DiffSpec { path: rel, old_path: None, left: Rev::Index, right: Rev::Working, back: None }, false, window, cx);
    }
}

#[cfg(test)]
#[path = "code_nav_tests.rs"]
mod tests;
