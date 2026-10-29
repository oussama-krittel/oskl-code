//! Go to definition in Rust without rust-analyzer: the segments of `use` paths and `a::b`
//! paths, names brought in by `use` (and `use x::*` globs), through the crate's own modules,
//! its dependencies' sources (~/.cargo) and std (when rust-src is installed).

use crate::definition::Target;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::ops::Range;
use std::path::{Path, PathBuf};

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

/// One name in a `use` statement: where it is, the path up to it, and the name it binds (leaves).
#[derive(Debug, PartialEq)]
struct UseItem {
    range: Range<usize>,
    path: Vec<String>,
    local: Option<String>,
    glob: bool,
    public: bool,
}

/// Every `use` statement in `text`, as items with char ranges into `text`.
fn uses(text: &str) -> Vec<UseItem> {
    let mut out = Vec::new();
    let stmt = re(r"(?m)^[ \t]*(pub(?:\([^)]*\))?\s+)?use\s+([^;]+);");
    for c in stmt.captures_iter(text) {
        let m = c.get(2).unwrap();
        let start = text[..m.start()].chars().count();
        let chars: Vec<char> = m.as_str().chars().collect();
        let mut p = UseParser { s: &chars, i: 0, base: start, public: c.get(1).is_some(), out: &mut out };
        p.tree(Vec::new());
    }
    out
}

struct UseParser<'a> {
    s: &'a [char],
    i: usize,
    base: usize,
    public: bool,
    out: &'a mut Vec<UseItem>,
}

impl UseParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_whitespace() {
            self.i += 1;
        }
        // comments inside a use list
        if self.s.get(self.i) == Some(&'/') && self.s.get(self.i + 1) == Some(&'/') {
            while self.i < self.s.len() && self.s[self.i] != '\n' {
                self.i += 1;
            }
            self.ws();
        }
    }

    fn ident(&mut self) -> Option<(String, Range<usize>)> {
        let a = self.i;
        while self.i < self.s.len() && is_ident(self.s[self.i]) {
            self.i += 1;
        }
        (self.i > a).then(|| (self.s[a..self.i].iter().collect(), self.base + a..self.base + self.i))
    }

    fn item(&mut self, range: Range<usize>, path: Vec<String>, local: Option<String>, glob: bool) {
        self.out.push(UseItem { range, path, local, glob, public: self.public });
    }

    fn tree(&mut self, prefix: Vec<String>) {
        let mut segs = prefix;
        loop {
            self.ws();
            if self.s.get(self.i..self.i + 2) == Some(&[':', ':']) {
                self.i += 2;
                continue;
            }
            match self.s.get(self.i) {
                Some('{') => {
                    self.i += 1;
                    loop {
                        self.ws();
                        if matches!(self.s.get(self.i), Some('}') | None) {
                            self.i += 1;
                            return;
                        }
                        self.tree(segs.clone());
                        self.ws();
                        match self.s.get(self.i) {
                            Some(',') => self.i += 1,
                            Some('}') => {
                                self.i += 1;
                                return;
                            }
                            _ => return,
                        }
                    }
                }
                Some('*') => {
                    self.i += 1;
                    let at = self.base + self.i - 1;
                    self.item(at..at + 1, segs, None, true);
                    return;
                }
                _ => {}
            }
            let Some((name, range)) = self.ident() else { return };
            // `a::{self}` is `a` itself.
            if !(name == "self" && !segs.is_empty()) {
                segs.push(name);
            }
            self.item(range, segs.clone(), None, false);
            let last = self.out.len() - 1;
            self.ws();
            if self.s.get(self.i..self.i + 2) == Some(&[':', ':']) {
                self.i += 2;
                continue;
            }
            // The path ends: its last name is what it binds, or `as y` binds y.
            let mut local = segs.last().cloned();
            if self.s.get(self.i..self.i + 2) == Some(&['a', 's']) && self.s.get(self.i + 2).is_some_and(|c| c.is_whitespace()) {
                self.i += 2;
                self.ws();
                if let Some((alias, r)) = self.ident() {
                    local = Some(alias.clone());
                    self.item(r, segs.clone(), Some(alias), false);
                }
            }
            self.out[last].local = local;
            return;
        }
    }
}

/// What a path leads to.
#[derive(Clone)]
enum Res {
    Module(PathBuf),
    Item(Target),
}

impl Res {
    fn target(self) -> Target {
        match self {
            Res::Module(path) => Target { path, row: 0, col: 0 },
            Res::Item(t) => t,
        }
    }
}

struct Resolver<'a> {
    /// The open file and its (maybe unsaved) text.
    here: (&'a Path, &'a str),
    /// Lookups under way (module, name, from inside), so `pub use` cycles end.
    busy: HashSet<(PathBuf, String, bool)>,
    /// Answers so far: a crate's globs are walked once, however many paths cross them.
    memo: HashMap<(PathBuf, String, bool), Option<Res>>,
    files: HashMap<PathBuf, Option<Rc<(String, Vec<UseItem>)>>>,
    crates: HashMap<String, Option<PathBuf>>,
}

impl<'a> Resolver<'a> {
    fn new(file: &'a Path, text: &'a str) -> Self {
        Resolver { here: (file, text), busy: HashSet::new(), memo: HashMap::new(), files: HashMap::new(), crates: HashMap::new() }
    }

    /// A module file's text and its `use` items.
    fn read(&mut self, path: &Path) -> Option<Rc<(String, Vec<UseItem>)>> {
        if let Some(f) = self.files.get(path) {
            return f.clone();
        }
        let text = if path == self.here.0 { Some(self.here.1.to_string()) } else { std::fs::read_to_string(path).ok() };
        let f = text.map(|t| {
            let u = uses(&t);
            Rc::new((t, u))
        });
        self.files.insert(path.to_path_buf(), f.clone());
        f
    }

    fn extern_crate(&mut self, name: &str, from: &Path) -> Option<PathBuf> {
        if let Some(c) = self.crates.get(name) {
            return c.clone();
        }
        let c = extern_crate(name, from);
        self.crates.insert(name.to_string(), c.clone());
        c
    }

    /// The module path `segs` names, as seen from module file `from`.
    fn resolve(&mut self, segs: &[String], from: &Path, depth: u32) -> Option<Res> {
        if depth > 10 || segs.is_empty() {
            return None;
        }
        let first = segs[0].as_str();
        let mut cur = match first {
            "crate" => Res::Module(crate_root(from)?),
            "self" => Res::Module(from.to_path_buf()),
            "super" => Res::Module(parent_module(from)?),
            // In scope by name first, then the crates, then what globs bring in.
            _ => match self.lookup_named(from, first, depth, true) {
                Some(r) => r,
                None => match self.extern_crate(first, from) {
                    Some(c) => Res::Module(c),
                    None if matches!(first, "std" | "core" | "alloc") => return None,
                    None => self.lookup(from, first, depth, true)?,
                },
            },
        };
        for (k, seg) in segs.iter().enumerate().skip(1) {
            cur = match cur {
                Res::Module(m) => match seg.as_str() {
                    "super" => Res::Module(parent_module(&m)?),
                    "self" => Res::Module(m),
                    _ => self.lookup(&m, seg, depth, false)?,
                },
                // `Type::method`: a function of that name in the type's file, else the type.
                Res::Item(t) => {
                    let last = segs.last()?;
                    let f = self.read(&t.path)?;
                    return Some(match item_in(&f.0, last) {
                        Some((row, col)) if k + 1 == segs.len() => Res::Item(Target { path: t.path, row, col }),
                        _ => Res::Item(t),
                    });
                }
            };
        }
        Some(cur)
    }

    /// `name` inside module file `module`: a child module, an item, or something it `use`s.
    /// `inside` is a lookup from that module itself, which sees its private modules.
    fn lookup(&mut self, module: &Path, name: &str, depth: u32, inside: bool) -> Option<Res> {
        self.lookup_with(module, name, depth, inside, true)
    }

    /// Like `lookup`, without what globs bring in.
    fn lookup_named(&mut self, module: &Path, name: &str, depth: u32, inside: bool) -> Option<Res> {
        self.lookup_with(module, name, depth, inside, false)
    }

    fn lookup_with(&mut self, module: &Path, name: &str, depth: u32, inside: bool, globs: bool) -> Option<Res> {
        let key = (module.to_path_buf(), format!("{name}{}", if globs { "" } else { " (named)" }), inside);
        if let Some(r) = self.memo.get(&key) {
            return r.clone();
        }
        if depth > 10 || !self.busy.insert(key.clone()) {
            return None;
        }
        let r = self.lookup_in(module, name, depth, inside, globs);
        self.busy.remove(&key);
        self.memo.insert(key, r.clone());
        r
    }

    fn lookup_in(&mut self, module: &Path, name: &str, depth: u32, inside: bool, globs: bool) -> Option<Res> {
        let f = self.read(module)?;
        let (text, items) = (&f.0, &f.1);
        let n = regex::escape(name);
        let m = re(&format!(r"(?m)^[ \t]*(?:#\[[^\]]*\]\s*)*(pub(?:\([^)]*\))?\s+)?mod\s+({n})\s*([;{{])"));
        if let Some(c) = m.captures(text) {
            // A private module is only seen from its own crate (another crate's `mod div;` next
            // to `pub use div::*` isn't the `div` it exports).
            let visible = c.get(1).is_some() || inside || crate_root(module) == crate_root(self.here.0);
            if visible {
                let at = c.get(2).unwrap().start();
                if &c[3] == ";" {
                    return child_module(module, name).map(Res::Module);
                }
                let (row, col) = row_col(text, at);
                return Some(Res::Item(Target { path: module.to_path_buf(), row, col }));
            }
        }
        if let Some((row, col)) = item_in(text, name) {
            return Some(Res::Item(Target { path: module.to_path_buf(), row, col }));
        }
        if let Some(it) = items.iter().find(|it| it.local.as_deref() == Some(name) || !it.glob && it.local.as_deref() == Some("_") && it.path.last().map(String::as_str) == Some(name)) {
            let path = it.path.clone();
            if let Some(r) = self.resolve(&path, module, depth + 1) {
                return Some(r);
            }
        }
        for it in items.iter().filter(|it| it.glob && globs) {
            let path = it.path.clone();
            if let Some(Res::Module(m)) = self.resolve(&path, module, depth + 1)
                && let Some(r) = self.lookup(&m, name, depth + 1, false)
            {
                return Some(r);
            }
        }
        None
    }
}

fn row_col(text: &str, byte: usize) -> (usize, usize) {
    (text[..byte].matches('\n').count(), text[..byte].rsplit('\n').next().unwrap_or("").chars().count())
}

/// Where item `name` (fn, struct, trait, macro…) is declared in `text`.
fn item_in(text: &str, name: &str) -> Option<(usize, usize)> {
    let n = regex::escape(name);
    let item = re(&format!(r"(?m)\b(?:fn|struct|enum|trait|type|const|static|union|macro_rules!)\s+({n})\b"));
    item.captures(text).and_then(|c| c.get(1)).map(|m| row_col(text, m.start()))
}

/// A module file's own name decides where its children live: `a/mod.rs`, `lib.rs` and
/// `main.rs` hold them next to themselves, `a.rs` in `a/`.
fn children_dir(module: &Path) -> Option<PathBuf> {
    let dir = module.parent()?;
    let stem = module.file_stem()?.to_string_lossy();
    if stem == "mod" || is_crate_root(module) {
        Some(dir.to_path_buf())
    } else {
        Some(dir.join(stem.as_ref()))
    }
}

fn child_module(module: &Path, name: &str) -> Option<PathBuf> {
    let dir = children_dir(module)?;
    [dir.join(format!("{name}.rs")), dir.join(name).join("mod.rs")].into_iter().find(|p| p.is_file())
}

fn parent_module(module: &Path) -> Option<PathBuf> {
    if is_crate_root(module) {
        return None;
    }
    let dir = if module.file_stem()? == "mod" { module.parent()?.parent()? } else { module.parent()? };
    // The file whose children are in `dir`.
    let root = crate_root(module)?;
    if root.parent() == Some(dir) {
        return Some(root);
    }
    [dir.with_extension("rs"), dir.join("mod.rs")].into_iter().find(|p| p.is_file())
}

fn cargo_toml(start: &Path) -> Option<PathBuf> {
    start.ancestors().skip(1).map(|d| d.join("Cargo.toml")).find(|p| p.is_file())
}

/// A package's root source file: `[lib] path`, else src/lib.rs, else src/main.rs.
fn package_root(pkg: &Path) -> Option<PathBuf> {
    let toml = std::fs::read_to_string(pkg.join("Cargo.toml")).unwrap_or_default();
    if let Some(c) = re(r#"(?s)\[lib\][^\[]*?\bpath\s*=\s*"([^"]+)""#).captures(&toml) {
        return Some(pkg.join(&c[1]));
    }
    [pkg.join("src/lib.rs"), pkg.join("src/main.rs")].into_iter().find(|p| p.is_file())
}

fn is_crate_root(file: &Path) -> bool {
    let Some(toml) = cargo_toml(file) else { return false };
    let pkg = toml.parent().unwrap();
    package_root(pkg).as_deref() == Some(file) || file == pkg.join("src/main.rs") || file.parent() == Some(&pkg.join("src/bin"))
}

/// The root file of the crate `file` is in (`main.rs` for itself, else the library first).
fn crate_root(file: &Path) -> Option<PathBuf> {
    let toml = cargo_toml(file)?;
    let pkg = toml.parent()?;
    if is_crate_root(file) {
        return Some(file.to_path_buf());
    }
    package_root(pkg)
}

/// The root file of dependency `name`, found through Cargo.lock: in the registry, a git
/// checkout, or the workspace; std, core and alloc in the toolchain's rust-src.
fn extern_crate(name: &str, from: &Path) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if matches!(name, "std" | "core" | "alloc" | "proc_macro" | "test") {
        let rustup = std::env::var_os("RUSTUP_HOME").map(PathBuf::from).unwrap_or(home.join(".rustup"));
        let tcs = std::fs::read_dir(rustup.join("toolchains")).ok()?;
        return tcs.flatten().map(|t| t.path().join("lib/rustlib/src/rust/library").join(name).join("src/lib.rs")).find(|p| p.is_file());
    }
    let lock = from.ancestors().skip(1).map(|d| d.join("Cargo.lock")).find(|p| p.is_file())?;
    let text = std::fs::read_to_string(&lock).ok()?;
    let cargo = std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or(home.join(".cargo"));
    for pkg in text.split("[[package]]") {
        let field = |k: &str| re(&format!(r#"(?m)^{k} = "([^"]*)""#)).captures(pkg).map(|c| c[1].to_string());
        let (Some(pname), Some(version)) = (field("name"), field("version")) else { continue };
        if pname.replace('-', "_") != name {
            continue;
        }
        let source = field("source").unwrap_or_default();
        let dir = if source.starts_with("registry+") {
            let regs = std::fs::read_dir(cargo.join("registry/src")).ok()?;
            regs.flatten().map(|r| r.path().join(format!("{pname}-{version}"))).find(|p| p.is_dir())
        } else if let Some((_, rev)) = source.split_once('#') {
            let short = &rev[..rev.len().min(7)];
            let checkouts = std::fs::read_dir(cargo.join("git/checkouts")).ok()?;
            checkouts.flatten().map(|c| c.path().join(short)).filter(|p| p.is_dir()).find_map(|p| package_dir(&p, &pname, 3))
        } else {
            package_dir(lock.parent()?, &pname, 3)
        };
        if let Some(root) = dir.and_then(|d| package_root(&d)) {
            return Some(root);
        }
    }
    None
}

/// The folder (up to `depth` down) whose Cargo.toml is package `name`.
fn package_dir(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
    if re(&format!(r#"(?m)^name\s*=\s*"{}""#, regex::escape(name))).is_match(&toml) {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).find_map(|e| {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with('.') || n == "target" { None } else { package_dir(&e.path(), name, depth - 1) }
    })
}

/// `a::b::` written right before char `at` of `line`: the path segments in front of a name.
fn qualifier(line: &[char], mut at: usize) -> Vec<String> {
    let mut segs = Vec::new();
    loop {
        if at < 2 || line[at - 1] != ':' || line[at - 2] != ':' {
            break;
        }
        let mut a = at - 2;
        // `Vec::<T>::new`
        if a > 0 && line[a - 1] == '>' {
            break;
        }
        let end = a;
        while a > 0 && is_ident(line[a - 1]) {
            a -= 1;
        }
        if a == end {
            break;
        }
        segs.push(line[a..end].iter().collect());
        at = a;
    }
    segs.reverse();
    segs
}

/// Where the name at `range` (char positions in `text`, the contents of `file`) is defined,
/// when Rust's paths say so: a segment of a `use` or `a::b` path, or a name some `use` brings in.
pub fn find(text: &str, range: Range<usize>, name: &str, file: &Path) -> Option<Target> {
    let mut r = Resolver::new(file, text);
    let items = uses(text);
    if let Some(it) = items.iter().find(|it| it.range == range) {
        return r.resolve(&it.path, file, 0).map(Res::target);
    }
    let before: String = text.chars().take(range.start).collect::<Vec<_>>().into_iter().rev().take_while(|c| *c != '\n').collect();
    let line: Vec<char> = before.chars().rev().collect();
    let mut segs = qualifier(&line, line.len());
    if segs.first().is_some_and(|s| s == "Self") {
        return None;
    }
    segs.push(name.to_string());
    if segs.len() == 1 && !items.iter().any(|it| it.local.as_deref() == Some(name)) {
        // A plain name is only ours to answer when a `use` brings it in.
        return None;
    }
    r.resolve(&segs, file, 0).map(Res::target)
}

/// A plain name that isn't declared or imported by name in `file`: look in what its
/// `use x::*` bring in. Not for path segments (a `std::` path we can't open isn't a glob's).
pub fn from_globs(text: &str, range: Range<usize>, name: &str, file: &Path) -> Option<Target> {
    let mut r = Resolver::new(file, text);
    let items = uses(text);
    let before: Vec<char> = text.chars().take(range.start).collect();
    let quoted = before.len() >= 2 && before[before.len() - 2..] == [':', ':'];
    if quoted || matches!(name, "std" | "core" | "alloc") || items.iter().any(|it| it.range == range) {
        return None;
    }
    for it in items.into_iter().filter(|it| it.glob) {
        if let Some(Res::Module(m)) = r.resolve(&it.path, file, 0)
            && let Some(res) = r.lookup(&m, name, 1, false)
        {
            return Some(res.target());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kite-rdef-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_use_trees() {
        let text = "use crate::git::{self, CommitDetails as Details, sub::Thing};\npub use gpui::prelude::*;\n";
        let items = uses(text);
        let show: Vec<(String, String, Option<String>, bool)> = items
            .iter()
            .map(|it| (text.chars().skip(it.range.start).take(it.range.len()).collect(), it.path.join("::"), it.local.clone(), it.glob))
            .collect();
        assert_eq!(
            show,
            vec![
                ("crate".into(), "crate".into(), None, false),
                ("git".into(), "crate::git".into(), None, false),
                ("self".into(), "crate::git".into(), Some("git".into()), false),
                ("CommitDetails".into(), "crate::git::CommitDetails".into(), Some("Details".into()), false),
                ("Details".into(), "crate::git::CommitDetails".into(), Some("Details".into()), false),
                ("sub".into(), "crate::git::sub".into(), None, false),
                ("Thing".into(), "crate::git::sub::Thing".into(), Some("Thing".into()), false),
                ("gpui".into(), "gpui".into(), None, false),
                ("prelude".into(), "gpui::prelude".into(), None, false),
                ("*".into(), "gpui::prelude".into(), None, true),
            ]
        );
        assert!(items[7].public);
    }

    #[test]
    fn follows_modules_dependencies_and_reexports() {
        let d = tmp("crate");
        // The app: src/main.rs with `mod git;` and src/git.rs, src/git/sub.rs.
        std::fs::create_dir_all(d.join("app/src/git")).unwrap();
        std::fs::write(d.join("app/Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(d.join("app/src/main.rs"), "mod git;\nfn main() {}\n").unwrap();
        std::fs::write(d.join("app/src/git.rs"), "pub mod sub;\npub struct CommitDetails;\nimpl CommitDetails {\n    pub fn load() {}\n}\n").unwrap();
        std::fs::write(d.join("app/src/git/sub.rs"), "pub fn thing() {}\nuse super::CommitDetails;\n").unwrap();
        // A dependency in a workspace folder, found through Cargo.lock: [lib] path, `pub use x::*`.
        std::fs::create_dir_all(d.join("app/vendor/ui/src/elements")).unwrap();
        std::fs::write(d.join("app/vendor/ui/Cargo.toml"), "[package]\nname = \"ui-kit\"\n[lib]\npath = \"src/ui.rs\"\n").unwrap();
        std::fs::write(d.join("app/vendor/ui/src/ui.rs"), "mod elements;\npub mod prelude;\npub use elements::*;\n").unwrap();
        std::fs::write(d.join("app/vendor/ui/src/elements/mod.rs"), "mod div;\npub use div::*;\n").unwrap();
        std::fs::write(d.join("app/vendor/ui/src/elements/div.rs"), "\npub fn div() {}\n").unwrap();
        std::fs::write(d.join("app/vendor/ui/src/prelude.rs"), "pub use crate::{Styled as _, div};\npub trait Styled {}\n").unwrap();
        std::fs::write(d.join("app/Cargo.lock"), "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"ui-kit\"\nversion = \"0.2.0\"\n").unwrap();

        let file = d.join("app/src/git/other.rs");
        let text = "use crate::git::{self, CommitDetails};\nuse ui_kit::prelude::*;\nuse ui_kit::div as make;\nfn f() {\n    CommitDetails::load();\n    crate::git::sub::thing();\n    make();\n    div();\n    super::CommitDetails;\n}\n";
        let go = |needle: &str, nth: usize| {
            let byte = text.match_indices(needle).nth(nth).unwrap().0;
            let a = text[..byte].chars().count();
            let t = find(text, a..a + needle.chars().count(), needle, &file).or_else(|| from_globs(text, a..a + needle.chars().count(), needle, &file))?;
            Some((t.path.strip_prefix(&d).unwrap().to_string_lossy().to_string(), t.row, t.col))
        };
        assert_eq!(go("crate", 0), Some(("app/src/main.rs".into(), 0, 0)), "crate is the crate's root file");
        assert_eq!(go("git", 0), Some(("app/src/git.rs".into(), 0, 0)));
        assert_eq!(go("self", 0), Some(("app/src/git.rs".into(), 0, 0)), "self in a use list is that module");
        assert_eq!(go("CommitDetails", 0), Some(("app/src/git.rs".into(), 1, 11)));
        assert_eq!(go("ui_kit", 0), Some(("app/vendor/ui/src/ui.rs".into(), 0, 0)), "a dependency through Cargo.lock");
        assert_eq!(go("prelude", 0), Some(("app/vendor/ui/src/prelude.rs".into(), 0, 0)));
        assert_eq!(go("div", 0), Some(("app/vendor/ui/src/elements/div.rs".into(), 1, 7)), "through pub use x::* twice");
        assert_eq!(go("make", 1), Some(("app/vendor/ui/src/elements/div.rs".into(), 1, 7)), "an alias");
        assert_eq!(go("load", 0), Some(("app/src/git.rs".into(), 3, 11)), "Type::method");
        assert_eq!(go("thing", 0), Some(("app/src/git/sub.rs".into(), 0, 7)));
        assert_eq!(go("div", 1), Some(("app/vendor/ui/src/elements/div.rs".into(), 1, 7)), "a name from a glob");
        assert_eq!(go("CommitDetails", 2), Some(("app/src/git.rs".into(), 1, 11)), "super::");
        std::fs::remove_dir_all(&d).ok();
    }
}
