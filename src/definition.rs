//! Go to definition without a language server: ⌘-click (or F12) on a name or an import path.
//!
//! In order: an import path opens the module; a name imported from a module jumps to its export
//! there (following re-exports); a name defined in this file jumps to that line; anything else is
//! looked up across the project in the background. Rust paths have their own resolver
//! (`rust_def`).

use crate::buffer::Buffer;
use regex::Regex;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub path: PathBuf,
    pub row: usize,
    pub col: usize,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

const CODE_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "rs", "py", "go", "css", "scss"];

/// What ⌘ over `pos` points at: the import string, or the name.
pub enum Hit {
    /// The module spec inside an import's quotes, and its range.
    Module(String, Range<usize>),
    Name(String, Range<usize>),
}

pub fn hit_at(buf: &Buffer, pos: usize) -> Option<Hit> {
    let (row, col) = buf.pos_to_rc(pos);
    let line: Vec<char> = buf.line(row).chars().collect();
    let start = buf.line_start(row);
    if let Some((spec, r)) = import_string(&line, col) {
        return Some(Hit::Module(spec, start + r.start..start + r.end));
    }
    if col > line.len() {
        return None;
    }
    let mut a = col;
    while a > 0 && is_ident(line[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b < line.len() && is_ident(line[b]) {
        b += 1;
    }
    if a == b || line[a].is_ascii_digit() {
        return None;
    }
    Some(Hit::Name(line[a..b].iter().collect(), start + a..start + b))
}

/// The quoted module of an import/export/require on this line, when `col` is inside it.
fn import_string(line: &[char], col: usize) -> Option<(String, Range<usize>)> {
    let text: String = line.iter().collect();
    let re = re(r#"(?:\bfrom|\bimport|\brequire\(|\bimport\(|\bjest\.mock\()\s*['"]([^'"]+)['"]"#);
    for c in re.captures_iter(&text) {
        let m = c.get(1)?;
        let a = text[..m.start()].chars().count();
        let b = a + m.as_str().chars().count();
        if col + 1 >= a && col <= b + 1 {
            return Some((m.as_str().to_string(), a..b));
        }
    }
    None
}

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

/// The file an import spec points at.
pub fn resolve_module(spec: &str, from: &Path, root: Option<&Path>) -> Option<PathBuf> {
    let dir = from.parent()?;
    let base = if spec.starts_with('.') {
        dir.join(spec)
    } else if let Some((p, target)) = crate::complete::aliases(from, root).into_iter().find(|(p, _)| spec.starts_with(p.as_str())) {
        target.join(&spec[p.len()..])
    } else if let Some(rest) = spec.strip_prefix("@/").or_else(|| spec.strip_prefix("~/")) {
        let r = root?;
        if r.join("src").is_dir() { r.join("src").join(rest) } else { r.join(rest) }
    } else {
        // A package: the nearest node_modules up from the file.
        let mut d = Some(dir);
        loop {
            let cand = d?.join("node_modules").join(spec);
            if cand.exists() || cand.with_extension("js").exists() || cand.with_extension("d.ts").exists() {
                break cand;
            }
            if d == root {
                return None;
            }
            d = d?.parent();
        }
    };
    resolve_file(&base)
}

fn resolve_file(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    let s = base.to_string_lossy();
    for ext in ["ts", "tsx", "d.ts", "js", "jsx", "mjs", "cjs", "json"] {
        let p = PathBuf::from(format!("{s}.{ext}"));
        if p.is_file() {
            return Some(p);
        }
    }
    if base.is_dir() {
        if let Some(pkg) = std::fs::read_to_string(base.join("package.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
            for key in ["types", "typings", "module", "main", "react-native"] {
                if let Some(entry) = pkg[key].as_str()
                    && let Some(p) = resolve_file(&base.join(entry))
                {
                    return Some(p);
                }
            }
        }
        for name in ["index.ts", "index.tsx", "index.d.ts", "index.js", "index.jsx"] {
            if base.join(name).is_file() {
                return Some(base.join(name));
            }
        }
    }
    None
}

/// One name brought in by an import: `local` here, `imported` in the module (`default`, `*`).
#[derive(Debug, PartialEq)]
struct Binding {
    local: String,
    imported: String,
    spec: String,
}

fn imports(text: &str) -> Vec<Binding> {
    let mut out = Vec::new();
    let imp = re(r#"(?s)\bimport\s+(?:type\s+)?([^;'"]*?)\s+from\s*['"]([^'"]+)['"]"#);
    for c in imp.captures_iter(text) {
        let (clause, spec) = (&c[1], c[2].to_string());
        let (head, braces) = match clause.find('{') {
            Some(i) => (&clause[..i], Some(&clause[i + 1..clause.rfind('}').unwrap_or(clause.len())])),
            None => (clause, None),
        };
        for part in head.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            if let Some(ns) = part.strip_prefix("* as ") {
                out.push(Binding { local: ns.trim().into(), imported: "*".into(), spec: spec.clone() });
            } else if part.chars().all(is_ident) {
                out.push(Binding { local: part.into(), imported: "default".into(), spec: spec.clone() });
            }
        }
        for item in braces.unwrap_or("").split(',').map(|s| s.trim().trim_start_matches("type ").trim()).filter(|s| !s.is_empty()) {
            let (imported, local) = item.split_once(" as ").map(|(a, b)| (a.trim(), b.trim())).unwrap_or((item, item));
            out.push(Binding { local: local.into(), imported: imported.into(), spec: spec.clone() });
        }
    }
    let req = re(r#"\b(?:const|let|var)\s+(\{[^}]*\}|[\w$]+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#);
    for c in req.captures_iter(text) {
        let spec = c[2].to_string();
        let lhs = &c[1];
        if let Some(inner) = lhs.strip_prefix('{') {
            for item in inner.trim_end_matches('}').split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let (imported, local) = item.split_once(':').map(|(a, b)| (a.trim(), b.trim())).unwrap_or((item, item));
                out.push(Binding { local: local.into(), imported: imported.into(), spec: spec.clone() });
            }
        } else {
            out.push(Binding { local: lhs.into(), imported: "default".into(), spec });
        }
    }
    out
}

/// Where `name` is declared in `text` (not counting the occurrence at `skip`), as (row, col).
fn declaration(text: &str, name: &str, skip: Option<usize>) -> Option<(usize, usize)> {
    let n = regex::escape(name);
    let patterns = [
        // function/class/const… (with export/default/async/declare in front), and other languages.
        format!(r"(?m)\b(?:function\*?|class|interface|type|enum|namespace|module|const|let|var|fn|struct|trait|mod|def|func|macro_rules!)\s+({n})\b"),
        // Methods: `  name(args) {` and `async name() {`, object/class members `name = (` / `name: (`.
        format!(r"(?m)^[ \t]*(?:(?:public|private|protected|static|readonly|async|get|set|override)\s+)*({n})\s*(?:<[^>]*>)?\([^)]*\)\s*(?::\s*[^{{=]+)?\{{\s*$"),
        format!(r"(?m)^[ \t]*(?:(?:public|private|protected|static|readonly)\s+)*({n})\s*[:=]\s*(?:async\s*)?(?:\([^)]*\)|[\w$]+)\s*=>"),
        // Destructured or plain parameters/bindings: `const { name }`, `[name, setName]`.
        format!(r"(?m)\b(?:const|let|var)\s*[\{{\[][^=]*?\b({n})\b[^=]*?[\}}\]]\s*="),
    ];
    for p in &patterns {
        for c in re(p).captures_iter(text) {
            let m = c.get(1)?;
            let at = text[..m.start()].chars().count();
            if Some(at) == skip {
                continue;
            }
            let row = text[..m.start()].matches('\n').count();
            let col = text[..m.start()].rsplit('\n').next().unwrap_or("").chars().count();
            return Some((row, col));
        }
    }
    None
}

fn row_col(text: &str, byte: usize) -> (usize, usize) {
    (text[..byte].matches('\n').count(), text[..byte].rsplit('\n').next().unwrap_or("").chars().count())
}

/// Where module `file` exports `name` (`default` for the default export), following re-exports.
fn export_in(file: &Path, name: &str, root: Option<&Path>, depth: u32) -> Option<Target> {
    let text = std::fs::read_to_string(file).ok()?;
    let at = |byte: usize| {
        let (row, col) = row_col(&text, byte);
        Some(Target { path: file.to_path_buf(), row, col })
    };
    let n = regex::escape(name);
    if name == "default" {
        let d = re(r"(?m)\bexport\s+default\s+(?:async\s+)?(?:function\*?\s+|class\s+|abstract\s+class\s+)?([\w$]+)?");
        if let Some(c) = d.captures(&text) {
            // `export default Foo;` points at Foo's declaration.
            if let Some(id) = c.get(1)
                && !matches!(id.as_str(), "function" | "class")
                && let Some((row, col)) = declaration(&text, id.as_str(), Some(text[..id.start()].chars().count()))
            {
                return Some(Target { path: file.to_path_buf(), row, col });
            }
            return at(c.get(1).map_or(c.get(0)?.start(), |m| m.start()));
        }
        let reexport = re(r#"(?m)\bexport\s*\{[^}]*\bdefault\b[^}]*\}\s*from\s*['"]([^'"]+)['"]"#);
        if let Some(c) = reexport.captures(&text)
            && depth < 6
            && let Some(next) = resolve_module(&c[1], file, root)
        {
            return export_in(&next, "default", root, depth + 1);
        }
        return Some(Target { path: file.to_path_buf(), row: 0, col: 0 });
    }
    let direct = re(&format!(
        r"(?m)\bexport\s+(?:declare\s+)?(?:default\s+)?(?:async\s+)?(?:function\*?|class|abstract\s+class|interface|type|enum|const\s+enum|namespace|const|let|var)\s+({n})\b"
    ));
    if let Some(c) = direct.captures(&text) {
        return at(c.get(1)?.start());
    }
    // `export { a, b as name }` — local declaration of the original name, or a re-export.
    let list = re(r#"(?s)\bexport\s+(?:type\s+)?\{([^}]*)\}\s*(?:from\s*['"]([^'"]+)['"])?"#);
    for c in list.captures_iter(&text) {
        for item in c[1].split(',').map(|s| s.trim().trim_start_matches("type ").trim()) {
            let (orig, exported) = item.split_once(" as ").map(|(a, b)| (a.trim(), b.trim())).unwrap_or((item, item));
            if exported != name {
                continue;
            }
            if let Some(spec) = c.get(2) {
                if depth < 6
                    && let Some(next) = resolve_module(spec.as_str(), file, root)
                {
                    return export_in(&next, orig, root, depth + 1).or_else(|| Some(Target { path: next, row: 0, col: 0 }));
                }
            } else if let Some((row, col)) = declaration(&text, orig, None) {
                return Some(Target { path: file.to_path_buf(), row, col });
            }
        }
    }
    // `export * from './x'`: look in each.
    if depth < 6 {
        let star = re(r#"(?m)\bexport\s+\*\s+from\s*['"]([^'"]+)['"]"#);
        for c in star.captures_iter(&text) {
            if let Some(next) = resolve_module(&c[1], file, root)
                && let Some(t) = export_in(&next, name, root, depth + 1)
                && (t.row, t.col) != (0, 0)
            {
                return Some(t);
            }
        }
    }
    // CommonJS: `module.exports.name =` / `exports.name =`.
    if let Some(m) = re(&format!(r"(?m)\b(?:module\.)?exports\.({n})\s*=")).captures(&text).and_then(|c| c.get(1)) {
        return at(m.start());
    }
    declaration(&text, name, None).map(|(row, col)| Target { path: file.to_path_buf(), row, col })
}

pub enum Found {
    At(Target),
    /// Not in this file or its imports: worth a project-wide search for this name.
    Search(String),
    /// The name is its own declaration (or nothing to go to).
    Nothing,
}

/// Resolves what's under `pos` in `buf` (the file at `file`).
pub fn find(buf: &Buffer, pos: usize, file: Option<&Path>, root: Option<&Path>) -> Found {
    let Some(hit) = hit_at(buf, pos) else { return Found::Nothing };
    let text = buf.text();
    match hit {
        Hit::Module(spec, _) => match file.and_then(|f| resolve_module(&spec, f, root)) {
            Some(path) => Found::At(Target { path, row: 0, col: 0 }),
            None => Found::Nothing,
        },
        Hit::Name(name, range) => {
            let here = file.map(Path::to_path_buf).unwrap_or_default();
            let rust = file.filter(|f| f.extension().is_some_and(|e| e == "rs"));
            // Rust paths (`use a::b`, `a::b::c`) and names their `use`s bring in.
            if let Some(f) = rust
                && let Some(t) = crate::rust_def::find(&text, range.clone(), &name, f)
            {
                return Found::At(t);
            }
            // `ns.member` where ns is `import * as ns` (or a default import object).
            let before: String = buf.rope.slice(range.start.saturating_sub(200)..range.start).chars().collect();
            let qualifier = before.strip_suffix('.').map(|b| b.chars().rev().take_while(|c| is_ident(*c)).collect::<String>().chars().rev().collect::<String>());
            let bindings = imports(&text);
            if let (Some(q), Some(f)) = (qualifier.as_deref(), file)
                && let Some(b) = bindings.iter().find(|b| b.local == q)
                && let Some(module) = resolve_module(&b.spec, f, root)
            {
                return export_in(&module, &name, root, 0).map_or(Found::Search(name), Found::At);
            }
            if qualifier.is_none()
                && let Some(b) = bindings.iter().find(|b| b.local == name)
            {
                let Some(module) = file.and_then(|f| resolve_module(&b.spec, f, root)) else {
                    return Found::Nothing;
                };
                if b.imported == "*" {
                    return Found::At(Target { path: module, row: 0, col: 0 });
                }
                return Found::At(export_in(&module, &b.imported, root, 0).unwrap_or(Target { path: module, row: 0, col: 0 }));
            }
            if qualifier.is_none()
                && let Some((row, col)) = declaration(&text, &name, Some(range.start))
            {
                return Found::At(Target { path: here, row, col });
            }
            // The declaration itself: it "goes" to itself, like VS Code.
            if qualifier.is_none()
                && let Some((row, col)) = declaration(&text, &name, None)
            {
                return Found::At(Target { path: here, row, col });
            }
            // Rust: maybe from one of the file's `use x::*`.
            if let Some(f) = rust
                && qualifier.is_none()
                && let Some(t) = crate::rust_def::from_globs(&text, range.clone(), &name, f)
            {
                return Found::At(t);
            }
            // A member or a global: look across the project.
            Found::Search(name)
        }
    }
}

/// Project-wide: the first declaration of `name` in the project's code files (exports first;
/// only exports when `exported_only`). Runs off the UI thread; skips node_modules, build output
/// and hidden folders.
pub fn search_project(root: &Path, name: &str, skip: Option<&Path>, exported_only: bool) -> Option<Target> {
    const MAX_FILES: usize = 20_000;
    const MAX_SIZE: u64 = 1_000_000;
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    let mut fallback: Option<Target> = None;
    let n = regex::escape(name);
    let exported = re(&format!(
        r"(?m)\bexport\s+(?:declare\s+)?(?:default\s+)?(?:async\s+)?(?:function\*?|class|abstract\s+class|interface|type|enum|namespace|const|let|var)\s+({n})\b|\bpub(?:\([^)]*\))?\s+(?:async\s+)?(?:fn|struct|enum|trait|type|const|mod)\s+({n})\b"
    ));
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !fname.starts_with('.') && !matches!(fname.as_str(), "node_modules" | "target" | "dist" | "build" | "out" | "coverage" | "Pods" | "vendor" | "__pycache__") {
                    stack.push(p);
                }
                continue;
            }
            if Some(p.as_path()) == skip || !p.extension().is_some_and(|x| CODE_EXTS.contains(&x.to_string_lossy().as_ref())) {
                continue;
            }
            if e.metadata().map(|m| m.len() > MAX_SIZE).unwrap_or(true) {
                continue;
            }
            seen += 1;
            if seen > MAX_FILES {
                return fallback;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            if !text.contains(name) {
                continue;
            }
            if let Some(m) = exported.captures(&text).and_then(|c| c.get(1).or(c.get(2))) {
                let (row, col) = row_col(&text, m.start());
                return Some(Target { path: p, row, col });
            }
            if fallback.is_none()
                && !exported_only
                && let Some((row, col)) = declaration(&text, name, None)
            {
                fallback = Some(Target { path: p.clone(), row, col });
            }
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kite-def-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_import_bindings() {
        let b = imports("import React, { useState as useS, type FC } from 'react';\nimport * as api from \"./api\";\nconst { join } = require('path');");
        assert!(b.contains(&Binding { local: "React".into(), imported: "default".into(), spec: "react".into() }));
        assert!(b.contains(&Binding { local: "useS".into(), imported: "useState".into(), spec: "react".into() }));
        assert!(b.contains(&Binding { local: "FC".into(), imported: "FC".into(), spec: "react".into() }));
        assert!(b.contains(&Binding { local: "api".into(), imported: "*".into(), spec: "./api".into() }));
        assert!(b.contains(&Binding { local: "join".into(), imported: "join".into(), spec: "path".into() }));
    }

    #[test]
    fn finds_local_declarations() {
        let text = "const a = 1;\nfunction greet(n: string) {\n  return n;\n}\nclass Box {\n  open(x) {\n  }\n}\ngreet('x');\nconst [count, setCount] = useState(0);\n";
        assert_eq!(declaration(text, "greet", None), Some((1, 9)));
        assert_eq!(declaration(text, "open", None), Some((5, 2)));
        assert_eq!(declaration(text, "setCount", None), Some((9, 14)));
        assert_eq!(declaration(text, "Box", None), Some((4, 6)));
    }

    #[test]
    fn follows_imports_into_other_files() {
        let d = tmp("imports");
        std::fs::create_dir_all(d.join("src/ui")).unwrap();
        std::fs::write(d.join("src/ui/Button.tsx"), "import x from 'y';\n\nexport function Button() {\n  return null;\n}\nexport default function Card() {}\n").unwrap();
        std::fs::write(d.join("src/ui/index.ts"), "export * from './Button';\nexport { default as Card } from './Button';\n").unwrap();
        std::fs::write(d.join("src/app.tsx"), "").unwrap();
        let app = d.join("src/app.tsx");
        let buf = Buffer::new("import { Button, Card } from './ui';\nimport Btn from './ui/Button';\n<Button />\n<Card />\n<Btn />\n");
        let go = |needle: &str, nth: usize| {
            let text = buf.text();
            let byte = text.match_indices(needle).nth(nth).unwrap().0;
            match find(&buf, text[..byte].chars().count() + 1, Some(&app), Some(&d)) {
                Found::At(t) => Some((t.path.file_name().unwrap().to_string_lossy().to_string(), t.row, t.col)),
                _ => None,
            }
        };
        assert_eq!(go("Button", 2), Some(("Button.tsx".into(), 2, 16)), "through export * in index.ts");
        assert_eq!(go("Card", 1), Some(("Button.tsx".into(), 5, 24)), "a re-exported default");
        assert_eq!(go("Btn", 1), Some(("Button.tsx".into(), 5, 24)), "a default import");
        assert_eq!(go("./ui'", 0), Some(("index.ts".into(), 0, 0)), "the import path opens the module");
        assert_eq!(go("./ui/Button", 0), Some(("Button.tsx".into(), 0, 0)));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rust_paths_and_globs() {
        let d = tmp("rust");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(d.join("src/main.rs"), "mod ui;\nmod theme;\n").unwrap();
        std::fs::write(d.join("src/theme.rs"), "pub fn fg() {}\npub mod prelude {\n    pub fn accent() {}\n}\n").unwrap();
        let file = d.join("src/ui.rs");
        let buf = Buffer::new("use crate::theme::{self, fg};\nuse crate::theme::*;\nfn f() {\n    theme::fg();\n    prelude::accent();\n}\n");
        let go = |needle: &str, nth: usize| {
            let text = buf.text();
            let byte = text.match_indices(needle).nth(nth).unwrap().0;
            match find(&buf, text[..byte].chars().count() + 1, Some(&file), Some(&d)) {
                Found::At(t) => Some((t.path.file_name().unwrap().to_string_lossy().to_string(), t.row, t.col)),
                _ => None,
            }
        };
        assert_eq!(go("theme", 0), Some(("theme.rs".into(), 0, 0)), "a module in a use path");
        assert_eq!(go("fg", 1), Some(("theme.rs".into(), 0, 7)), "a path in code");
        assert_eq!(go("prelude", 0), Some(("theme.rs".into(), 1, 8)), "an inline module, through a glob");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn searches_the_project_for_the_rest() {
        let d = tmp("search");
        std::fs::create_dir_all(d.join("src/lib")).unwrap();
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::write(d.join("node_modules/x/index.ts"), "export function formatMoney() {}").unwrap();
        std::fs::write(d.join("src/lib/money.ts"), "// money\nexport const formatMoney = (n: number) => n;\n").unwrap();
        let t = search_project(&d, "formatMoney", None, true).expect("found");
        assert_eq!((t.path.ends_with("src/lib/money.ts"), t.row, t.col), (true, 1, 13));
        std::fs::write(d.join("src/lib/util.ts"), "const helper = 1;\n").unwrap();
        assert_eq!(search_project(&d, "helper", None, true), None, "a local in another file isn't a target");
        assert!(search_project(&d, "helper", None, false).is_some());
        std::fs::remove_dir_all(&d).ok();
    }
}
