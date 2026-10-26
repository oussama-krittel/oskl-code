//! The shortcuts manual: one list of every binding and command, shown as a sidebar tab.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::{theme, ui, util};
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;

/// (section, keys, what it does)
pub const KEYS: &[(&str, &str, &str)] = &[
    ("General", "⌘P", "Find file by name (fuzzy). Add :line to jump, e.g. app.ts:42"),
    ("General", "⌘⇧P  ·  ⌘;", "Command line (type :keys-style commands, Tab completes)"),
    ("General", "⌘O", "Open folder in this window"),
    ("General", "⌘⇧N  ·  title bar +", "Open another project in a new window (several at once)"),
    ("General", "⌘`", "Switch between open project windows"),
    ("General", "⌘⇧F", "Search & replace in all files"),
    ("General", "⇧⌘E", "Show explorer"),
    ("General", "⌃⇧G", "Show source control"),
    ("General", "⌘K ⌘S", "Show this shortcuts manual"),
    ("General", "⌘B", "Toggle sidebar"),
    ("General", "⌘K ⌘B  ·  title bar ◫", "Move sidebar left / right  ·  :sidebar left|right"),
    ("General", "⌘J", "Toggle output panel (lint/format/git output); it gets keyboard focus"),
    ("Output panel", "j  ·  k  ·  ↑  ·  ↓", "Move line by line"),
    ("Output panel", "Space  ·  PgDn  ·  PgUp", "Page down / up"),
    ("Output panel", "g  ·  G  (Home / End)", "Top / bottom"),
    ("Output panel", "Enter", "Open the file:line on the selected line"),
    ("Output panel", "Esc", "Back to your split"),
        ("General", "⌘0", "Focus sidebar"),
    ("General", "Status bar ✓ Auto Save", "Auto save 1s after typing (on by default) · :autosave on|off"),
    ("General", "⌘S", "Save — formats first with your project's formatter (:formatonsave off to disable)"),
    ("Panes & splits", "⌘D  ·  ⌘⇧D", "Split right / down (then pick a file, or ⌘T for a terminal)"),
    ("Panes & splits", "Split limit", "A split can be halved until it is 1/8 of the window (max 8 splits)"),
    ("Panes & splits", "⌘⌥ ←↓↑→  ·  ⌘⌥ J K L ;", "Move between splits (j ← · k ↓ · l ↑ · ; →; wraps at the edges)"),
    ("Panes & splits", "Click a pane", "Focus it (green frame = focused pane)"),
    ("Panes & splits", "Drag the gap between panes", "Resize"),
    ("Panes & splits", "⌘⌥⇧ ←↓↑→  ·  ⌘⌥⇧ J K L ;", "Move the shown file/terminal to the split that way (or split one off there); the old split stays even if empty"),
    ("Panes & splits", "Drag a numbered item", "Drop on a pane to move it there; drop near an edge to split that side"),
    ("Panes & splits", "Drag a file from the explorer", "Open it in that pane (edge = new split)"),
    ("Panes & splits", "Small ✕ on an item", "Close it (also middle-click, ⌘W for the shown one)"),
    ("Panes & splits", "⌘⌥Enter  ·  double-click title", "Maximize the pane / back to all splits"),
    ("Panes & splits", "⌘1 … ⌘9", "Show item 1…9 of the focused pane (numbers in its title bar; ⌘9 = last)"),
    ("Panes & splits", "⌘⇧W", "Close pane"),
    ("Panes & splits", "⌘W", "Close the file shown in the pane"),
    ("Panes & splits", "⌘E  ·  click a number in the title bar", "List / switch files open in this pane (middle-click closes)"),
    ("Panes & splits", "⌃⌘ ← →  ·  ⌃⌘ J ;  ·  ⌃Tab  ·  ⌘⇧] [", "Previous / next file in this split"),
    ("Panes & splits", "⌘P  ·  explorer  ·  git panel", "Open into the focused pane"),
    ("Terminal", "⌃`", "Jump to a terminal (opens one at the bottom); again jumps back"),
    ("Terminal", "⌘T", "New terminal in the focused pane"),
    ("Terminal", "⌘C  ·  ⌘V", "Copy selection / paste (bracketed paste)"),
    ("Terminal", "⌘K", "Clear screen and scrollback"),
    ("Terminal", "Drag  ·  double / triple click", "Select text, word, line (⇧ forces selection in mouse apps)"),
    ("Terminal", "⌘Click", "Open a file:line printed in the terminal"),
    ("Terminal", "⌥ + key", "Acts as Meta (⌥B / ⌥F word jumps in the shell)"),
    ("Terminal", "Scroll", "Scrollback; in vim/less/htop scrolls the app"),
    ("Terminal", "⇧PgUp  ·  ⇧PgDn", "Scroll history a page (keys go to vim/less when they're open)"),
    ("Terminal", "⇧⌘↑  ·  ⇧⌘↓  (⇧Home / ⇧End)", "Top / bottom of the history"),
    ("Terminal", "exit  ·  ⌃D", "Closes the terminal (and its pane if empty)"),
    ("General", "⌃G", "Go to line"),
    ("General", "⌘=  ·  ⌘-", "Zoom editor font"),
    ("General", "⌘Q", "Quit"),
    ("Editing", "Type / ⌫ / ⌦", "Insert / delete (⌫ in indentation removes one indent level)"),
    ("Editing", "Enter", "New line keeping indentation; between {} opens a block"),
    ("Editing", "⌘Enter  ·  ⌘⇧Enter", "New line below / above"),
    ("Editing", "Tab  ·  ⇧Tab  ·  ⌘]  ·  ⌘[", "Indent / outdent line or selection"),
    ("Editing", "⌥↑  ·  ⌥↓", "Move line(s) up / down"),
    ("Editing", "⇧⌥↑  ·  ⇧⌥↓", "Duplicate line(s) up / down"),
    ("Editing", "⌘⇧K", "Delete line(s)"),
    ("Editing", "⌘/", "Toggle comment in the file's language: // · # (.env, YAML, shell, Python) · <!-- --> (HTML, Markdown) · /* */ (CSS) · -- (SQL)"),
    ("Editing", "⌘C  ·  ⌘X", "Copy / cut (whole line when nothing is selected)"),
    ("Editing", "⌘V", "Paste (a copied whole line pastes above the cursor)"),
    ("Editing", "⌘Z  ·  ⌘⇧Z", "Undo / redo"),
    ("Editing", "⇧⌥F", "Format file (Biome / Prettier / rustfmt / gofmt / ruff …)"),
    ("Editing", "⌃⌘Space", "Emoji & symbols"),
    ("Moving & selecting", "←→↑↓", "Move (add ⇧ to select)"),
    ("Moving & selecting", "⌥←  ·  ⌥→", "Word left / right (add ⇧ to select)"),
    ("Moving & selecting", "⌘←  ·  ⌘→  ·  Home  ·  End", "Line start (smart) / end (add ⇧ to select)"),
    ("Moving & selecting", "⌘↑  ·  ⌘↓", "File start / end (add ⇧ to select)"),
    ("Moving & selecting", "PgUp  ·  PgDn", "Page up / down (add ⇧ to select)"),
    ("Moving & selecting", "⌃⌥ ↑ ↓  ·  ⌃⌥ L K", "Scroll the view without moving the cursor"),
    ("Moving & selecting", "⌘A  ·  ⌘L", "Select all / select line (repeat to extend)"),
    ("Terminal keys (readline)", "⌃A  ·  ⌃E", "Line start / end"),
    ("Terminal keys (readline)", "⌃F  ·  ⌃B  ·  ⌃N  ·  ⌃P", "Right / left / down / up"),
    ("Terminal keys (readline)", "⌥F  ·  ⌥B", "Word forward / back"),
    ("Terminal keys (readline)", "⌃K", "Kill to end of line (copies it)"),
    ("Terminal keys (readline)", "⌃U  ·  ⌘⌫", "Delete to line start"),
    ("Terminal keys (readline)", "⌃W  ·  ⌥⌫", "Delete word back"),
    ("Terminal keys (readline)", "⌥⌦", "Delete word forward"),
    ("Terminal keys (readline)", "⌃D  ·  ⌃H", "Delete forward / back"),
    ("Mouse & trackpad", "Click  ·  drag", "Place cursor / select (drag past the edge to auto-scroll)"),
    ("Mouse & trackpad", "Double-click  ·  triple-click", "Select word / line"),
    ("Mouse & trackpad", "⇧Click", "Extend selection"),
    ("Mouse & trackpad", "Click / drag line numbers", "Select whole lines"),
    ("Mouse & trackpad", "Two-finger scroll", "Smooth scroll (horizontal too); drag the scrollbar"),
    ("Mouse & trackpad", "Click blame gutter", "Open that commit"),
    ("Mouse & trackpad", "Middle-click tab", "Close tab; double-click a tab to keep it open"),
    ("Search in file", "⌘F", "Search in file (live, case-insensitive unless you type capitals)"),
    ("Search in file", "⌘G  ·  ⌘⇧G", "Next / previous match"),
    ("Search in file", "Enter  ·  Esc", "Keep result / cancel and go back"),
    ("Search in files", "⌘⇧F", "Open search (selected text is used as the query)"),
    ("Search in files", "⌥⌘C  ·  ⌥⌘W  ·  ⌥⌘R", "Match case / whole word / regex"),
    ("Search in files", "Enter", "In search box: search now · in replace box: replace all"),
    ("Search in files", "↓  (from the search box)", "Jump into results"),
    ("Search in files", "j  ·  k  ·  Enter", "Move / open result (file rows fold)"),
    ("Search in files", "x  ·  ⌫", "Dismiss a result or a whole file — it will not be replaced"),
    ("Search in files", "r", "Replace this result (or this file)"),
    ("Search in files", "h  ·  l", "Fold / unfold file"),
    ("Search in files", "i", "Back to the search box"),
    ("Search in files", "…", "Files to include / exclude (globs: *.ts, src/**, dist)"),
    ("Source control", "j  ·  k  ·  g  ·  G", "Move through changes and the graph"),
    ("Source control", "Enter", "Open diff (editable for unstaged files) / open commit"),
    ("Source control", "Space  ·  s", "Stage / unstage file"),
    ("Source control", "d", "Discard changes (confirm on the command line with Enter)"),
    ("Source control", "o", "Open the file"),
    ("Source control", "c", "Write commit message"),
    ("Source control", "⌘Enter (in message)", "Commit (amend via the ⌄ next to Commit)"),
    ("Source control", "r", "Refresh"),
    ("Source control", "⌥⌘B", "Toggle blame gutter (editor and diffs)"),
    ("Source control", "…  (panel header)", "Pull, push, fetch, stash, stage/unstage/discard all, undo last commit, amend…"),
    ("Source control", "Graph filter icon", "Show Auto / All branches / pick local and origin/… branches"),
    ("Problems", "Status bar ⊗ 2 ⚠ 3  ·  :problems", "List lint + TypeScript problems; click a line to jump"),
    ("Problems", "Save a file", "Lints it (ESLint or Biome) and type-checks its TS project (tsc --noEmit)"),
    ("Problems", ":check now|off", "Re-run checks / turn them off"),
    ("Problems", "JSON / YAML", "Syntax errors and duplicate keys shown as you type (tsconfig & .vscode allow comments)"),
    ("Editing", "⇧⌥F on JSON / YAML", "Biome or Prettier; JSON falls back to a built-in formatter (keeps key order)"),
    ("Problems", "Squiggles", "Red = error, yellow = warning; message shown at the end of the line"),
    ("Diff (editable)", "F7  ·  ⇧F7", "Next / previous change"),
    ("Diff (editable)", "⌘K I", "Inline ↔ side-by-side"),
    ("Diff (editable)", "⌘S", "Save edits made in the diff"),
    ("Diff (history)", "j  ·  k  ·  n  ·  p", "Move by line / change"),
    ("Diff (history)", "b  ·  i", "Toggle blame / inline"),
    ("Diff (history)", "Enter  ·  o", "Open file at this line"),
    ("Commit view", "j  ·  k  ·  Enter", "Pick a file / open its diff"),
    ("Commit view", "y  ·  ⌘C", "Copy commit hash"),
    ("Explorer", "j  ·  k  ·  g  ·  G", "Move"),
    ("Explorer", "h  ·  l  ·  ←  ·  →", "Collapse / expand"),
    ("Explorer", "Enter  ·  o  ·  Space", "Open (single click previews, double click keeps)"),
    ("Explorer", "a  ·  ⇧A", "New file / folder (on the command line)"),
    ("Explorer", "r  ·  F2", "Rename / move"),
    ("Explorer", "⇧H", "Collapse all"),
    ("Panes (terminal style)", ":", "Command line from any pane"),
    ("Panes (terminal style)", "/", "Find file from any pane"),
    ("Panes (terminal style)", "!", "Run a shell command"),
    ("Commands (:)", ":w  ·  :q  ·  :wq  ·  :q!  ·  :qa", "Save / close / save+close / discard / quit"),
    ("Commands (:)", ":42", "Go to line 42"),
    ("Commands (:)", ":e path[:line]", "Open file"),
    ("Commands (:)", ":open dir  ·  :recent  ·  :window dir", "Open folder here / recent folder / new window"),
    ("Commands (:)", ":new path  ·  :mkdir path  ·  :mv a b", "Create file / folder, rename"),
    ("Commands (:)", ":format  ·  :lint  ·  :lint .", "Format, lint file, lint project"),
    ("Commands (:)", ":blame  ·  :diff", "Blame gutter / diff current file"),
    ("Commands (:)", ":stage  ·  :unstage  ·  :discard [files]", "Index operations (current file by default)"),
    ("Commands (:)", ":commit [-a] [--amend] msg", "Commit"),
    ("Commands (:)", ":checkout b  ·  :branch b", "Switch / create branch (Tab completes)"),
    ("Commands (:)", ":fetch  ·  :pull  ·  :push  ·  :stash", "Remote and stash operations"),
    ("Commands (:)", ":git any args", "Run any git command"),
    ("Commands (:)", ":graph all|auto", "Graph shows all branches or just yours"),
    ("Commands (:)", ":!cmd", "Run in your shell, output is clickable"),
    ("Commands (:)", ":search text", "Search in all files"),
    ("Commands (:)", ":sidebar left|right", "Sidebar side"),
    ("Commands (:)", ":vsplit [path]  ·  :split [path]", "Split right / down, optionally opening a file"),
    ("Commands (:)", ":term [cmd]", "Terminal in this pane, optionally running a command"),
    ("Commands (:)", ":close  ·  :only  ·  :max  ·  :ls", "Close pane / close others / maximize / pane files"),
    ("Commands (:)", ":autosave on|off", "Auto save (default on)"),
    ("Commands (:)", ":zoom in|out|reset  ·  :keys", "Font size / this manual"),
];

pub fn as_text() -> String {
    let mut out = String::new();
    let mut section = "";
    for (s, k, d) in KEYS {
        if *s != section {
            out.push_str(&format!("\n{}\n", s.to_uppercase()));
            section = s;
        }
        out.push_str(&format!("  {k:<38} {d}\n"));
    }
    out
}

enum Line {
    Header(&'static str),
    Key(usize),
}

pub struct KeysPanel {
    focus: FocusHandle,
    filter: Entity<Editor>,
    lines: Vec<Line>,
    _sub: Subscription,
}

impl KeysPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.placeholder = "Filter shortcuts (e.g. blame, ⌘, delete)".into();
            ed
        });
        let sub = cx.subscribe_in(&filter, window, |p, _, ev: &EditorEvent, _, cx| {
            if let EditorEvent::Changed = ev {
                p.rebuild(cx);
            }
        });
        let mut p = Self { focus: cx.focus_handle(), filter, lines: Vec::new(), _sub: sub };
        p.rebuild(cx);
        p
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.filter.read(cx).focus(window);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let q = self.filter.read(cx).text().to_lowercase();
        self.lines.clear();
        let mut section = "";
        for (ix, (s, k, d)) in KEYS.iter().enumerate() {
            let hay = format!("{s} {k} {d}").to_lowercase();
            if !q.is_empty() && !q.split_whitespace().all(|w| hay.contains(w)) && util::fuzzy(&q, d).is_none() {
                continue;
            }
            if *s != section {
                self.lines.push(Line::Header(s));
                section = s;
            }
            self.lines.push(Line::Key(ix));
        }
        cx.notify();
    }
}

impl Focusable for KeysPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KeysPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.filter.read(cx).is_focused(window);
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex().flex_none().items_center().h(px(35.)).px(px(20.)).text_size(px(10.5)).child("SHORTCUTS"))
            .child(
                div().flex_none().px(px(12.)).pb(px(8.)).child(
                    div()
                        .h(px(26.))
                        .px(px(4.))
                        .bg(theme::bg_input())
                        .border_1()
                        .border_color(if focused { theme::focus_border() } else { theme::border_input() })
                        .child(self.filter.clone()),
                ),
            )
            .child(
                uniform_list(
                    "keys",
                    self.lines.len(),
                    cx.processor(|p, range: Range<usize>, _, _| {
                        range
                            .map(|ix| match p.lines[ix] {
                                Line::Header(s) => div()
                                    .flex()
                                    .items_end()
                                    .h(px(30.))
                                    .px(px(14.))
                                    .pb(px(4.))
                                    .text_size(px(10.5))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::accent())
                                    .child(s.to_uppercase())
                                    .into_any_element(),
                                Line::Key(k) => {
                                    let (_, keys, desc) = KEYS[k];
                                    div()
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .h(px(40.))
                                        .px(px(14.))
                                        .hover(|s| s.bg(theme::hover()))
                                        .child(div().overflow_hidden().whitespace_nowrap().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_bright()).child(keys))
                                        .child(div().overflow_hidden().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_dim()).child(desc))
                                        .into_any_element()
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(14.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(px(10.5))
                    .text_color(theme::fg_muted())
                    .flex()
                    .gap(px(6.))
                    .child(ui::small_icon("terminal", theme::fg_muted()))
                    .child(":keys prints this list to the output panel"),
            )
    }
}
