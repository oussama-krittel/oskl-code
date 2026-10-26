//! The shortcuts manual: one list of every binding and command, shown as a sidebar tab.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::keybindings as kb;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;

/// (section, keys, what it does)
pub const KEYS: &[(&str, &str, &str)] = &[
    ("Arrows on letters", "⌃⌥ + I K J L", "↑ ↓ ← → everywhere — editor, search boxes, popups, terminal (your direction letters)"),
    ("Arrows on letters", "⌃⌥⇧ + I K J L", "Select (⇧ + arrows)"),
    ("Arrows on letters", "⌥ + J L  ·  ⌥⇧ + J L", "Word left / right · select word"),
    ("Arrows on letters", "⌥ + I K  ·  ⌥⇧ + I K", "Move line up / down · duplicate line"),
    ("Arrows on letters", "⌃⌥⌘ + I K J L", "File start / end · line start / end (⌘ + arrows)"),
    ("Arrows on letters", "⌃⌥⌘⇧ + I K J L", "Select to file / line start and end"),
    ("Arrows on letters", "⌃⌘ + I K  ·  ⌃⌘ + J L", "Scroll the editor view (cursor stays) · previous / next item in the split"),
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
    ("Moving & selecting", "⌃⌘ ↑ ↓  ·  ⌃⌘ I K  ·  ⌃⌥ ↑ ↓", "Scroll the view without moving the cursor"),
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
    ("Mouse & trackpad", "Click a branch label in the graph", "Checkout · checkout detached · delete (local branches and tags only)"),
    ("Mouse & trackpad", "Drag a split's item / an explorer file", "Move it to another split; drop near an edge to split"),
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

/// Sections of `KEYS` that aren't rebindable shortcuts (mouse, built-in terminal keys, commands).
const REFERENCE: &[&str] = &["Arrows on letters", "Mouse & trackpad", "Terminal", "Commands (:)", "Problems"];

/// `:keys` — every shortcut as currently bound, then the reference list.
pub fn as_text() -> String {
    let cfg = kb::Config::load();
    let mut out = String::new();
    let mut section = String::new();
    for e in kb::entries() {
        if e.section != section {
            out.push_str(&format!("\n{}\n", e.section.to_uppercase()));
            section = e.section.clone();
        }
        let keys: Vec<String> = cfg.effective(&e).iter().map(|k| kb::label(k)).collect();
        out.push_str(&format!("  {:<34} {}\n", keys.join("  ·  "), e.desc));
    }
    let mut section = "";
    for (s, k, d) in KEYS.iter().filter(|(s, _, _)| REFERENCE.contains(s)) {
        if *s != section {
            out.push_str(&format!("\n{}\n", s.to_uppercase()));
            section = s;
        }
        out.push_str(&format!("  {k:<34} {d}\n"));
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Rec {
    /// Recording a key for an entry; `None` slot means "add another key".
    Key { entry: usize, slot: Option<usize> },
    /// Recording a direction letter (0 ← · 1 ↓ · 2 ↑ · 3 →).
    Dir(usize),
}

pub struct KeysPanel {
    focus: FocusHandle,
    filter: Entity<Editor>,
    entries: Vec<kb::Entry>,
    cfg: kb::Config,
    recording: Option<Rec>,
    message: Option<(String, bool)>,
    scroll: ScrollHandle,
    _sub: Subscription,
}

impl KeysPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.placeholder = "Filter (e.g. split, blame, ⌘D)".into();
            ed
        });
        let sub = cx.subscribe_in(&filter, window, |_, _, ev: &EditorEvent, _, cx| {
            if let EditorEvent::Changed = ev {
                cx.notify();
            }
        });
        Self {
            focus: cx.focus_handle(),
            filter,
            entries: kb::entries(),
            cfg: kb::Config::load(),
            recording: None,
            message: None,
            scroll: ScrollHandle::new(),
            _sub: sub,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.filter.read(cx).focus(window);
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    fn start(&mut self, rec: Rec, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(rec);
        self.message = Some(("Press the new shortcut… (Esc cancels)".into(), false));
        window.focus(&self.focus);
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>, msg: String) {
        self.cfg.save();
        kb::apply(cx);
        self.recording = None;
        self.message = Some((msg, false));
        cx.notify();
    }

    fn fail(&mut self, msg: String, cx: &mut Context<Self>) {
        self.recording = None;
        self.message = Some((msg, true));
        cx.notify();
    }

    fn set_keys(&mut self, ix: usize, keys: Vec<String>, cx: &mut Context<Self>, msg: String) {
        let e = &self.entries[ix];
        let defaults: Vec<String> = e.defaults.iter().map(|k| self.cfg.resolve(k)).collect();
        if keys == defaults {
            self.cfg.overrides.remove(&e.id);
        } else {
            self.cfg.overrides.insert(e.id.clone(), keys);
        }
        self.commit(cx, msg);
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.recording else { return };
        cx.stop_propagation();
        let mut ks = ev.keystroke.clone();
        if ks.key == "escape" && !ks.modifiers.modified() {
            self.recording = None;
            self.message = None;
            return cx.notify();
        }
        // fn is implied by Page Up/Down, Home/End and ⌦ on Mac keyboards.
        ks.modifiers.function = false;
        let key = ks.unparse();
        match rec {
            Rec::Dir(i) => self.record_direction(i, &ks, cx),
            Rec::Key { entry, slot } => self.record_key(entry, slot, key, cx),
        }
    }

    fn record_key(&mut self, ix: usize, slot: Option<usize>, key: String, cx: &mut Context<Self>) {
        let Some(key) = kb::normalize(&key) else { return self.fail(format!("Can't use \"{key}\""), cx) };
        let e = &self.entries[ix];
        let label = kb::label(&key);
        if let Some(why) = kb::reserved(&key)
            && !(e.id.ends_with("Quit") && key == "cmd-q")
        {
            return self.fail(format!("{label} is used by {why} — pick another"), cx);
        }
        if kb::blocks_typing(&key, e.ctx.as_deref()) {
            return self.fail(format!("{label} alone would stop you typing — add ⌘, ⌃ or ⌥"), cx);
        }
        if kb::is_arrow_alias(&key) {
            return self.fail(format!("{label} is an arrow key on your direction letters — pick another"), cx);
        }
        if let Some(other) = kb::conflict(&self.cfg, &self.entries, &key, e.ctx.as_deref(), &e.id) {
            return self.fail(format!("{label} is already used by {} › {}", other.section, other.desc), cx);
        }
        let mut keys = self.cfg.effective(e);
        match slot {
            Some(i) if i < keys.len() => keys[i] = key,
            _ if keys.contains(&key) => {}
            _ => keys.push(key),
        }
        let desc = e.desc.clone();
        self.set_keys(ix, keys, cx, format!("{desc}: {label}"));
    }

    fn record_direction(&mut self, i: usize, ks: &Keystroke, cx: &mut Context<Self>) {
        let m = &ks.modifiers;
        if m.control || m.alt || m.platform || ks.key.chars().count() != 1 {
            return self.fail("Direction keys are single letters or symbols, without modifiers".into(), cx);
        }
        let letter = ks.key.to_lowercase();
        if self.cfg.dirs.iter().enumerate().any(|(j, d)| j != i && *d == letter) {
            return self.fail(format!("\"{letter}\" is already another direction"), cx);
        }
        let mut next = self.cfg.clone();
        next.dirs[i] = letter.clone();
        // Every shortcut built from direction letters must stay valid with the new letter.
        for e in self.entries.iter().filter(|e| !next.is_custom(e) && e.defaults.iter().any(|k| k.contains('{'))) {
            for k in next.effective(e) {
                if let Some(why) = kb::reserved(&k) {
                    return self.fail(format!("{} would be {} ({why})", e.desc, kb::label(&k)), cx);
                }
                if let Some(other) = kb::conflict(&next, &self.entries, &k, e.ctx.as_deref(), &e.id) {
                    return self.fail(format!("{} → {} clashes with {} › {}", e.desc, kb::label(&k), other.section, other.desc), cx);
                }
            }
        }
        self.cfg = next;
        let arrow = ["←", "↓", "↑", "→"][i];
        self.commit(cx, format!("{arrow} is now \"{letter}\""));
    }

    fn chip(&self, id: SharedString, text: String, active: bool) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .h(px(20.))
            .rounded(px(4.))
            .border_1()
            .border_color(if active { theme::frame_focused() } else { theme::border_input() })
            .bg(if active { theme::frame_focused().opacity(0.15) } else { theme::bg_input() })
            .text_color(theme::fg_bright())
            .font_family(theme::MONO_FONT)
            .text_size(px(10.5))
            .cursor_pointer()
            .hover(|s| s.border_color(theme::frame_focused()))
            .child(text)
    }

    fn render_entry(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let e = &self.entries[ix];
        let keys = self.cfg.effective(e);
        let custom = self.cfg.is_custom(e);
        let mut chips = div().flex().flex_wrap().gap(px(4.)).items_center();
        for (slot, k) in keys.iter().enumerate() {
            let rec = Rec::Key { entry: ix, slot: Some(slot) };
            let active = self.recording == Some(rec);
            let text = if active { "press keys…".to_string() } else { kb::label(k) };
            chips = chips.child(
                self.chip(SharedString::from(format!("k-{ix}-{slot}")), text, active)
                    .group(SharedString::from(format!("kg-{ix}-{slot}")))
                    .tooltip(ui::tooltip("Click to change"))
                    .on_click(cx.listener(move |p, _, w, cx| p.start(rec, w, cx)))
                    .child(
                        div()
                            .id(SharedString::from(format!("kx-{ix}-{slot}")))
                            .invisible()
                            .group_hover(SharedString::from(format!("kg-{ix}-{slot}")), |s| s.visible())
                            .text_color(theme::fg_muted())
                            .hover(|s| s.text_color(theme::git_deleted()))
                            .on_click(cx.listener(move |p, _, _, cx| {
                                cx.stop_propagation();
                                let mut keys = p.cfg.effective(&p.entries[ix]);
                                if slot < keys.len() {
                                    let removed = keys.remove(slot);
                                    let desc = p.entries[ix].desc.clone();
                                    p.set_keys(ix, keys, cx, format!("Removed {} from {desc}", kb::label(&removed)));
                                }
                            }))
                            .child("✕"),
                    ),
            );
        }
        let adding = self.recording == Some(Rec::Key { entry: ix, slot: None });
        chips = chips.child(
            self.chip(SharedString::from(format!("k-{ix}-add")), if adding { "press keys…".into() } else { "+".into() }, adding)
                .tooltip(ui::tooltip("Add another shortcut"))
                .on_click(cx.listener(move |p, _, w, cx| p.start(Rec::Key { entry: ix, slot: None }, w, cx))),
        );
        if custom {
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("k-{ix}-reset")))
                    .px(px(4.))
                    .text_color(theme::fg_muted())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::fg_bright()))
                    .tooltip(ui::tooltip("Reset to default"))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        let id = p.entries[ix].id.clone();
                        p.cfg.overrides.remove(&id);
                        let desc = p.entries[ix].desc.clone();
                        p.commit(cx, format!("{desc}: back to default"));
                    }))
                    .child("↺"),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(12.))
            .py(px(5.))
            .hover(|s| s.bg(theme::hover()))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(div().text_color(if custom { theme::frame_focused() } else { theme::fg() }).child(e.desc.clone()))
                    .when(custom, |d| d.child(div().text_color(theme::fg_muted()).child("(custom)"))),
            )
            .child(chips)
            .into_any_element()
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
        let q = self.filter.read(cx).text().to_lowercase();
        let matches = |hay: &str| q.is_empty() || q.split_whitespace().all(|w| hay.to_lowercase().contains(w));

        // Direction letters.
        let mut dirs = div().flex().items_center().gap(px(6.)).px(px(12.)).pb(px(8.)).child(div().text_color(theme::fg_muted()).child("Directions"));
        for (i, arrow) in ["←", "↓", "↑", "→"].iter().enumerate() {
            let active = self.recording == Some(Rec::Dir(i));
            let text = if active { "…".to_string() } else { format!("{arrow} {}", self.cfg.dirs[i].to_uppercase()) };
            dirs = dirs.child(
                self.chip(SharedString::from(format!("dir-{i}")), text, active)
                    .tooltip(ui::tooltip("Letter used for this direction in ⌘⌥ / ⌘⌥⇧ / ⌃⌘ / ⌃⌥ shortcuts"))
                    .on_click(cx.listener(move |p, _, w, cx| p.start(Rec::Dir(i), w, cx))),
            );
        }

        let mut list = div().flex().flex_col();
        let mut section = String::new();
        for ix in 0..self.entries.len() {
            let e = &self.entries[ix];
            let keys_text: String = self.cfg.effective(e).iter().map(|k| format!("{} {k} ", kb::label(k))).collect();
            if !matches(&format!("{} {} {}", e.section, e.desc, keys_text)) {
                continue;
            }
            if e.section != section {
                section = e.section.clone();
                list = list.child(div().px(px(12.)).pt(px(12.)).pb(px(4.)).text_size(px(10.)).font_weight(FontWeight::BOLD).text_color(theme::accent()).child(section.to_uppercase()));
            }
            list = list.child(self.render_entry(ix, cx));
        }
        let mut ref_section = "";
        for (s, k, d) in KEYS.iter().filter(|(s, _, _)| REFERENCE.contains(s)) {
            if !matches(&format!("{s} {k} {d}")) {
                continue;
            }
            if *s != ref_section {
                ref_section = s;
                list = list.child(div().px(px(12.)).pt(px(12.)).pb(px(4.)).text_size(px(10.)).font_weight(FontWeight::BOLD).text_color(theme::fg_muted()).child(format!("{} (built in)", s.to_uppercase())));
            }
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(4.))
                    .flex()
                    .flex_col()
                    .child(div().font_family(theme::MONO_FONT).text_color(theme::fg_dim()).child(*k))
                    .child(div().text_color(theme::fg_muted()).child(*d)),
            );
        }

        let custom_count = self.cfg.overrides.len();
        div()
            .key_context("ShortcutsPanel")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(11.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .px(px(12.))
                    .child(div().flex_1().text_size(px(10.5)).child("SHORTCUTS"))
                    .when(custom_count > 0, |d| {
                        d.child(
                            div()
                                .id("keys-reset-all")
                                .px(px(6.))
                                .rounded(px(3.))
                                .text_color(theme::fg_muted())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::hover()).text_color(theme::fg_bright()))
                                .on_click(cx.listener(|p, _, _, cx| {
                                    p.cfg = kb::Config::default();
                                    p.commit(cx, "All shortcuts reset to defaults".into());
                                }))
                                .child(format!("Reset all ({custom_count})")),
                        )
                    }),
            )
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
            .child(dirs)
            .when_some(self.message.clone(), |d, (msg, err)| {
                d.child(div().flex_none().mx(px(12.)).mb(px(6.)).px(px(8.)).py(px(5.)).rounded(px(4.)).bg(if err { theme::git_deleted().opacity(0.15) } else { theme::frame_focused().opacity(0.12) }).text_color(if err { theme::git_deleted() } else { theme::fg() }).child(msg))
            })
            .child(div().id("keys-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(list))
    }
}
