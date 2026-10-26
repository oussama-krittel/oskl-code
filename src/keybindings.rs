//! Every keyboard shortcut, overridable by the user.
//!
//! Modules declare their default bindings with `kb(..)` / `collect(..)`. At startup (and
//! whenever the user edits a shortcut) `apply` merges those defaults with the overrides in
//! `~/.config/kite/keys` and rebinds everything. Bindings that use direction letters are
//! written with `{left}` `{down}` `{up}` `{right}` and follow the configured letters.

use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, Keystroke};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Def {
    keys: String,
    action: Box<dyn Action>,
    ctx: Option<String>,
}

thread_local! {
    static DEFAULTS: RefCell<Vec<Def>> = const { RefCell::new(Vec::new()) };
}

/// A default binding (registered with `collect`).
pub fn kb<A: Action>(keys: &str, action: A, ctx: Option<&str>) -> Def {
    Def { keys: keys.to_string(), action: Box::new(action), ctx: ctx.map(String::from) }
}

pub fn collect(defs: impl IntoIterator<Item = Def>) {
    DEFAULTS.with(|d| d.borrow_mut().extend(defs));
}

/// One rebindable command: an action in a context, with its default keys.
pub struct Entry {
    pub id: String,
    pub ctx: Option<String>,
    pub section: String,
    pub desc: String,
    pub defaults: Vec<String>,
    action: Box<dyn Action>,
}

pub fn entries() -> Vec<Entry> {
    DEFAULTS.with(|d| {
        let mut out: Vec<Entry> = Vec::new();
        for def in d.borrow().iter() {
            let name = def.action.name();
            let id = format!("{}:{}", def.ctx.as_deref().unwrap_or("global"), name);
            if let Some(e) = out.iter_mut().find(|e| e.id == id) {
                e.defaults.push(def.keys.clone());
                continue;
            }
            out.push(Entry {
                section: section(def.ctx.as_deref(), name),
                desc: describe(name, def.ctx.as_deref()),
                id,
                ctx: def.ctx.clone(),
                defaults: vec![def.keys.clone()],
                action: def.action.boxed_clone(),
            });
        }
        out
    })
}

fn section(ctx: Option<&str>, name: &str) -> String {
    let ns = name.split("::").next().unwrap_or("");
    match (ctx, ns) {
        (Some("Editor"), _) => "Editor",
        (Some("FileTree"), _) => "Explorer",
        (Some("GitPanel"), _) => "Source control",
        (Some("DiffView"), _) => "Diff (history)",
        (Some("DiffEdit"), _) => "Diff (editable)",
        (Some("CommitView"), _) => "Commit view",
        (Some("SearchPanel" | "SearchResults"), _) => "Search in files",
        (Some("Output"), _) => "Output panel",
        (None, "workspace") if is_pane_action(name) => "Panes & splits",
        _ => "General",
    }
    .to_string()
}

fn is_pane_action(name: &str) -> bool {
    let n = name.rsplit("::").next().unwrap_or(name);
    n.starts_with("Focus") && !n.contains("Commit") && !n.contains("Sidebar") && !n.contains("Editor")
        || n.starts_with("MoveItem")
        || n.starts_with("Split")
        || n.starts_with("Buffer")
        || matches!(n, "GoBack" | "GoForward" | "ClosePane" | "ZoomPane" | "NextTab" | "PrevTab" | "CloseTab" | "PaneBuffers" | "NewTerminal" | "ToggleTerminal")
}

/// Human description from the action name ("MoveWordLeft" → "Move word left").
fn describe(name: &str, ctx: Option<&str>) -> String {
    let short = name.rsplit("::").next().unwrap_or(name);
    let custom = match short {
        "FindFile" => "Find file (fuzzy)",
        "CommandPalette" => "Command line",
        "FindInFile" => "Search in this file",
        "FindNext" | "FindPrev" if ctx.is_none() => if short == "FindNext" { "Next match" } else { "Previous match" },
        "ShowFiles" => "Show explorer",
        "ShowGit" => "Show source control",
        "ToggleOutput" => "Toggle output panel",
        "ToggleTerminal" => "Jump to terminal (opens one at the bottom)",
        "NewTerminal" => "New terminal in this split",
        "ToggleBlame" => "Toggle blame",
        "ZoomPane" => "Maximize split / back to all splits",
        "ClosePane" => "Close split",
        "CloseTab" => "Close the file shown in the split",
        "NextTab" => "Next file in this split",
        "PrevTab" => "Previous file in this split",
        "PaneBuffers" => "List files open in this split",
        "SplitRight" => "Split right",
        "SplitDown" => "Split down",
        "FocusLeft" | "FocusRight" | "FocusUp" | "FocusDown" => match short {
            "FocusLeft" => "Move to split on the left",
            "FocusRight" => "Move to split on the right",
            "FocusUp" => "Move to split above",
            _ => "Move to split below",
        },
        "MoveItemLeft" => "Move file/terminal to the split on the left",
        "MoveItemRight" => "Move file/terminal to the split on the right",
        "MoveItemUp" => "Move file/terminal to the split above",
        "MoveItemDown" => "Move file/terminal to the split below",
        "FormatFile" => "Format file",
        "GoBack" => "Go back (what this split showed before)",
        "GoForward" => "Go forward",
        "ShowKeysPanel" => "Show shortcuts",
        "ToggleSidebarSide" => "Move sidebar left / right",
        "OpenFolderNewWindow" => "Open a project in a new window",
        "NextWindow" => "Switch project window",
        "SearchAll" => "Search in all files",
        "GoToLine" => "Go to line",
        "RunShell" => "Run a shell command",
        "Save" => "Save (formats first)",
        "QuitApp" | "Quit" => "Quit",
        "KillLine" => "Delete to end of line (copies it)",
        "DeleteToLineStart" => "Delete to line start",
        "Submit" => if ctx == Some("Editor") { "New line below / submit" } else { "Submit" },
        "ScrollLineUp" => "Scroll view up (cursor stays) · previous option in lists",
        "ScrollLineDown" => "Scroll view down (cursor stays) · next option in lists",
        "ShowCharacterPalette" => "Emoji & symbols",
        _ => "",
    };
    if !custom.is_empty() {
        return custom.to_string();
    }
    if let Some(n) = short.strip_prefix("Buffer") {
        return if n == "9" { "Show the last item in the split".into() } else { format!("Show item {n} of the split") };
    }
    let mut out = String::new();
    for (i, ch) in short.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------- user config

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Letters for ← ↓ ↑ → in direction shortcuts.
    pub dirs: [String; 4],
    /// Entry id → keys (empty = unbound).
    pub overrides: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        // ← ↓ ↑ →
        Self { dirs: ["j".into(), "k".into(), "i".into(), "l".into()], overrides: BTreeMap::new() }
    }
}

fn config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/kite/keys"))
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let Some(text) = config_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return cfg };
        let mut version = 1;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let Some((k, v)) = line.split_once(" = ") else { continue };
            if k == "version" {
                version = v.parse().unwrap_or(1);
            } else if k == "directions" {
                let letters: Vec<String> = v.split_whitespace().map(String::from).collect();
                if let Ok(arr) = <[String; 4]>::try_from(letters) {
                    cfg.dirs = arr;
                }
            } else {
                let keys = v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
                cfg.overrides.insert(k.to_string(), keys);
            }
        }
        // v1's default letters (j k l ;) became j k i l: move people still on the old default.
        if version < 2 && cfg.dirs.join(" ") == "j k l ;" {
            cfg.dirs = Self::default().dirs;
        }
        cfg
    }

    pub fn save(&self) {
        let Some(p) = config_path() else { return };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let mut out = String::from("# OSKL Code shortcuts — edit them in the Shortcuts tab (⌘K ⌘S).\n");
        out.push_str("version = 2\n");
        out.push_str(&format!("directions = {}\n", self.dirs.join(" ")));
        for (id, keys) in &self.overrides {
            out.push_str(&format!("{id} = {}\n", keys.join(", ")));
        }
        std::fs::write(p, out).ok();
    }

    pub fn resolve(&self, keys: &str) -> String {
        keys.replace("{left}", &self.dirs[0]).replace("{down}", &self.dirs[1]).replace("{up}", &self.dirs[2]).replace("{right}", &self.dirs[3])
    }

    /// The keys an entry currently has (override, or defaults with direction letters filled in).
    pub fn effective(&self, e: &Entry) -> Vec<String> {
        match self.overrides.get(&e.id) {
            Some(keys) => keys.clone(),
            None => e.defaults.iter().map(|k| self.resolve(k)).collect(),
        }
    }

    pub fn is_custom(&self, e: &Entry) -> bool {
        self.overrides.contains_key(&e.id)
    }
}

thread_local! {
    static CURRENT: RefCell<Config> = RefCell::new(Config::default());
    static LABELS: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
}

/// Label of the first shortcut of a global action, as currently configured (e.g. "⌘;").
pub fn shortcut_label(action: &str) -> Option<String> {
    LABELS.with(|l| l.borrow().get(action).cloned())
}

/// The direction letters as typed, e.g. "jkl;".
pub fn direction_letters() -> String {
    CURRENT.with(|c| c.borrow().dirs.concat())
}

/// Rebinds everything from defaults + the user's config.
pub fn apply(cx: &mut App) {
    let cfg = Config::load();
    // Cache labels for UI hints (status bar) so rendering never touches disk.
    let mut labels = BTreeMap::new();
    for e in entries().iter().filter(|e| e.ctx.is_none()) {
        let short = e.id.rsplit("::").next().unwrap_or(&e.id).to_string();
        if let Some(first) = cfg.effective(e).first() {
            labels.insert(short, label(first));
        }
    }
    LABELS.with(|l| *l.borrow_mut() = labels);
    CURRENT.with(|c| *c.borrow_mut() = cfg.clone());
    cx.clear_key_bindings();
    let mapper = cx.keyboard_mapper().clone();
    let mut bindings = Vec::new();
    for e in entries() {
        let predicate = e.ctx.as_deref().and_then(|c| KeyBindingContextPredicate::parse(c).ok()).map(Rc::new);
        for keys in cfg.effective(&e) {
            // "⇧;" is typed as ":" on US layouts; bind both so either form matches.
            let mut variants = vec![keys.clone()];
            if keys.contains("shift-;") {
                variants.push(keys.replace("shift-;", ":"));
            }
            for k in variants {
                if let Ok(b) = KeyBinding::load(&k, e.action.boxed_clone(), predicate.clone(), false, None, mapper.as_ref()) {
                    bindings.push(b);
                }
            }
        }
    }
    if std::env::var_os("KITE_DEBUG").is_some() {
        eprintln!("[kite] key bindings applied: {}", bindings.len());
    }
    cx.bind_keys(bindings);
}

// ---------------------------------------------------------------- validation

/// Canonical form ("shift-cmd-P" → "cmd-shift-p"), or None if it doesn't parse.
pub fn normalize(keys: &str) -> Option<String> {
    let parts: Option<Vec<String>> = keys.split_whitespace().map(|k| Keystroke::parse(k).ok().map(|ks| ks.unparse())).collect();
    parts.map(|p| p.join(" "))
}

/// macOS shortcuts the system (or another app) owns.
pub fn reserved(keys: &str) -> Option<&'static str> {
    let k = normalize(keys)?;
    Some(match k.as_str() {
        "cmd-tab" | "cmd-shift-tab" => "macOS app switcher",
        "cmd-space" => "Spotlight",
        "cmd-alt-space" => "Finder search",
        "ctrl-space" | "ctrl-alt-space" => "macOS input source switch",
        "cmd-h" => "macOS Hide app",
        "cmd-alt-h" => "macOS Hide others",
        "cmd-m" | "cmd-alt-m" => "macOS Minimize",
        "cmd-alt-escape" => "macOS Force Quit",
        "cmd-shift-3" | "cmd-shift-4" | "cmd-shift-5" | "cmd-shift-6" | "ctrl-cmd-shift-3" | "ctrl-cmd-shift-4" => "macOS screenshots",
        "ctrl-cmd-q" => "macOS Lock Screen",
        "ctrl-cmd-f" => "macOS full screen",
        "cmd-alt-d" => "macOS show/hide Dock",
        "ctrl-up" | "ctrl-down" | "ctrl-left" | "ctrl-right" => "macOS Mission Control / Spaces",
        "cmd-shift-/" => "macOS Help menu",
        "cmd-," => "macOS app settings",
        "cmd-q" => "Quit",
        _ => return None,
    })
}

/// Plain typing keys can't be global or editor shortcuts — they'd stop you typing.
pub fn blocks_typing(keys: &str, ctx: Option<&str>) -> bool {
    if !matches!(ctx, None | Some("Editor")) {
        return false;
    }
    let Some(first) = keys.split_whitespace().next().and_then(|k| Keystroke::parse(k).ok()) else { return false };
    let m = &first.modifiers;
    let special = first.key.len() > 1 && first.key != "space"; // f1, pageup, escape, enter…
    !(m.control || m.alt || m.platform || m.function || special)
}

fn contexts_overlap(a: Option<&str>, b: Option<&str>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// The entry already using `keys` where it would clash with `ctx` (ignoring `except_id`).
pub fn conflict<'a>(cfg: &Config, entries: &'a [Entry], keys: &str, ctx: Option<&str>, except_id: &str) -> Option<&'a Entry> {
    let wanted = normalize(keys)?;
    entries.iter().find(|e| {
        e.id != except_id
            && contexts_overlap(e.ctx.as_deref(), ctx)
            && cfg.effective(e).iter().any(|k| normalize(k).as_deref() == Some(wanted.as_str()))
    })
}

// ---------------------------------------------------------------- labels

/// "cmd-alt-shift-j" → "⌘⌥⇧J"; Page Up/Down also show the fn+arrow they come from on Macs.
pub fn label(keys: &str) -> String {
    keys.split_whitespace()
        .map(|k| {
            let Ok(ks) = Keystroke::parse(k) else { return k.to_string() };
            let m = &ks.modifiers;
            let mut s = String::new();
            if m.function {
                s.push_str("fn ");
            }
            if m.control {
                s.push('⌃');
            }
            if m.alt {
                s.push('⌥');
            }
            if m.shift {
                s.push('⇧');
            }
            if m.platform {
                s.push('⌘');
            }
            s.push_str(&match ks.key.as_str() {
                "up" => "↑".into(),
                "down" => "↓".into(),
                "left" => "←".into(),
                "right" => "→".into(),
                "pageup" => "Page Up (fn ↑)".into(),
                "pagedown" => "Page Down (fn ↓)".into(),
                "home" => "Home (fn ←)".into(),
                "end" => "End (fn →)".into(),
                "enter" => "↩".into(),
                "escape" => "Esc".into(),
                "backspace" => "⌫".into(),
                "delete" => "⌦ (fn ⌫)".into(),
                "tab" => "⇥".into(),
                "space" => "Space".into(),
                k if k.len() > 1 && k.starts_with('f') => k.to_uppercase(),
                k => k.to_uppercase(),
            });
            s
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{Config, blocks_typing, label, normalize, reserved};

    #[test]
    fn labels_and_normalizing() {
        assert_eq!(label("cmd-alt-shift-j"), "⌥⇧⌘J");
        assert_eq!(label("pageup"), "Page Up (fn ↑)");
        assert_eq!(label("cmd-k cmd-s"), "⌘K ⌘S");
        assert_eq!(normalize("shift-cmd-p"), normalize("cmd-shift-p"));
    }

    #[test]
    fn rules() {
        assert!(reserved("cmd-space").is_some());
        assert!(reserved("ctrl-left").is_some());
        assert!(reserved("cmd-d").is_none());
        assert!(blocks_typing("j", None));
        assert!(blocks_typing("shift-a", Some("Editor")));
        assert!(!blocks_typing("j", Some("FileTree")));
        assert!(!blocks_typing("pageup", None));
        assert!(!blocks_typing("cmd-j", None));
    }

    #[test]
    fn directions_resolve() {
        let mut cfg = Config::default();
        assert_eq!(cfg.resolve("cmd-alt-{left}"), "cmd-alt-j");
        assert_eq!(cfg.resolve("cmd-alt-{up}"), "cmd-alt-i");
        cfg.dirs = ["h".into(), "j".into(), "k".into(), "l".into()];
        assert_eq!(cfg.resolve("cmd-alt-shift-{right}"), "cmd-alt-shift-l");
    }
}

#[cfg(test)]
mod defaults {
    use super::{Config, conflict, entries, normalize, reserved};

    fn load_all() {
        crate::editor::bind_keys();
        crate::workspace::bind_keys();
        crate::file_tree::bind_keys();
        crate::git_panel::bind_keys();
        crate::diff_view::bind_keys();
        crate::commit_view::bind_keys();
        crate::search::bind_keys();
        crate::diff_edit::bind_keys();
    }

    /// Default shortcuts must all parse, never collide, and avoid macOS-owned keys.
    #[test]
    fn defaults_are_clean() {
        load_all();
        let cfg = Config::default();
        let all = entries();
        assert!(all.len() > 150, "only {} entries", all.len());
        let mut problems = Vec::new();
        for e in &all {
            for k in cfg.effective(e) {
                if normalize(&k).is_none() {
                    problems.push(format!("{} doesn't parse: {k}", e.id));
                }
                if let Some(other) = conflict(&cfg, &all, &k, e.ctx.as_deref(), &e.id) {
                    problems.push(format!("{k}: {} vs {}", e.id, other.id));
                }
                if let Some(why) = reserved(&k)
                    && !e.id.ends_with("Quit")
                {
                    problems.push(format!("{k} on {} is reserved ({why})", e.id));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
