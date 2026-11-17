//! What a file declares (functions, types, methods, headings), read with tree-sitter: for
//! "Go to symbol in file" (⌘⇧O, or @ in the file finder) and the breadcrumb's current symbol.
//! Languages without a grammar here (Markdown, CSS, HTML, YAML, TOML, JSON) are read line by line.

use crate::editor::Editor;
use crate::highlight::{self, Lang};
use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use tree_sitter::Node;

#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub name: String,
    /// "function", "method", "class", "struct", "enum", "interface", "trait", "type", "module"…
    pub kind: &'static str,
    /// Where the name is (0-based row, char column) and the last row of the whole declaration.
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    /// How many declarations it sits inside.
    pub depth: usize,
}

// shortcut: past this size a file lists no symbols (parsing runs on the UI thread), upgrade by parsing in the background.
const MAX_BYTES: usize = 1_500_000;
/// Deeper than this nothing is listed (and the walk stops), so a pathological file can't overflow the stack.
const MAX_DEPTH: usize = 120;
// shortcut: a YAML, TOML or JSON file lists its first 5000 keys, upgrade with a filter that lists only the top levels of a huge file.
const MAX_KEYS: usize = 5000;

pub fn of(path: &Path, text: &str) -> Vec<Symbol> {
    if text.len() > MAX_BYTES {
        return Vec::new();
    }
    let data = |mut v: Vec<Symbol>| {
        v.truncate(MAX_KEYS);
        v
    };
    match path.extension().and_then(|e| e.to_str()) {
        Some("md" | "markdown") => return headings(text),
        Some("css") => return css(text, false),
        Some("scss" | "less") => return css(text, true),
        Some("html" | "htm") => return html(text),
        Some("yaml" | "yml") => return data(yaml(text)),
        Some("toml") => return data(toml(text)),
        Some("json" | "jsonc") => return data(json(text)),
        _ => {}
    }
    let Some(lang) = highlight::detect(path) else { return Vec::new() };
    let language: tree_sitter::Language = match lang {
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
        _ => return Vec::new(),
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else { return Vec::new() };
    let mut out = Vec::new();
    walk(tree.root_node(), text, lang, None, 0, &mut out);
    out
}

fn walk(node: Node, src: &str, lang: Lang, container: Option<&'static str>, depth: usize, out: &mut Vec<Symbol>) {
    if depth > MAX_DEPTH {
        return;
    }
    let (mut container, mut depth, mut descend) = (container, depth, true);
    if let Some((kind, name)) = declared(lang, node, container) {
        let (start, col) = (name.start_byte(), name.start_position().column);
        let shown = shown(src.get(name.byte_range()).unwrap_or(""));
        if !shown.is_empty() {
            let line_start = start.saturating_sub(col);
            out.push(Symbol {
                name: shown,
                kind,
                row: name.start_position().row,
                col: src.get(line_start..start).map_or(col, |s| s.chars().count()),
                end_row: node.end_position().row,
                depth,
            });
            depth += 1;
            container = Some(kind);
            // A function's insides (callbacks, local helpers) are not part of the file's outline.
            descend = !matches!(kind, "function" | "method" | "constant" | "type");
        }
    }
    if descend {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, src, lang, container, depth, out);
        }
    }
}

/// A name on one line, not too long.
fn shown(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect()
}

fn symbol(name: &str, kind: &'static str, row: usize, col: usize, depth: usize) -> Symbol {
    Symbol { name: shown(name), kind, row, col, end_row: row, depth }
}

/// The kind of declaration `n` is and the node holding its name.
fn declared<'a>(lang: Lang, n: Node<'a>, container: Option<&str>) -> Option<(&'static str, Node<'a>)> {
    let in_type = matches!(container, Some("class" | "impl" | "trait" | "interface" | "struct" | "enum"));
    let method_or = |k: &'static str| if in_type { "method" } else { k };
    let field = |f: &str| n.child_by_field_name(f);
    let name = || field("name");
    let is_fn = |v: Option<Node>| v.is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function" | "generator_function"));
    Some(match (lang, n.kind()) {
        (Lang::Rust, "function_item" | "function_signature_item") => (method_or("function"), name()?),
        (Lang::Rust, "struct_item") => ("struct", name()?),
        (Lang::Rust, "enum_item") => ("enum", name()?),
        (Lang::Rust, "trait_item") => ("trait", name()?),
        (Lang::Rust, "impl_item") => ("impl", field("type")?),
        (Lang::Rust, "mod_item") => ("module", name()?),
        (Lang::Rust, "const_item" | "static_item") => ("constant", name()?),
        (Lang::Rust, "type_item") => ("type", name()?),
        (Lang::Rust, "macro_definition") => ("macro", name()?),

        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "function_declaration" | "generator_function_declaration" | "function_signature") => ("function", name()?),
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "class_declaration" | "abstract_class_declaration") => ("class", name()?),
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "method_definition" | "method_signature" | "abstract_method_signature") => ("method", name()?),
        (Lang::TypeScript | Lang::Tsx, "interface_declaration") => ("interface", name()?),
        (Lang::TypeScript | Lang::Tsx, "type_alias_declaration") => ("type", name()?),
        (Lang::TypeScript | Lang::Tsx, "enum_declaration") => ("enum", name()?),
        (Lang::TypeScript | Lang::Tsx, "internal_module") => ("namespace", name()?),
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "variable_declarator") if is_fn(field("value")) => (method_or("function"), name()?),
        (Lang::TypeScript | Lang::Tsx, "public_field_definition") if is_fn(field("value")) => ("method", name()?),
        (Lang::JavaScript, "field_definition") if is_fn(field("value")) => ("method", field("property")?),

        (Lang::Python, "function_definition") => (method_or("function"), name()?),
        (Lang::Python, "class_definition") => ("class", name()?),

        (Lang::Go, "function_declaration") => ("function", name()?),
        (Lang::Go, "method_declaration") => ("method", name()?),
        (Lang::Go, "type_spec") => ("type", name()?),

        (Lang::Java, "class_declaration" | "record_declaration") => ("class", name()?),
        (Lang::Java, "interface_declaration" | "annotation_type_declaration") => ("interface", name()?),
        (Lang::Java, "enum_declaration") => ("enum", name()?),
        (Lang::Java, "method_declaration" | "constructor_declaration") => ("method", name()?),

        (Lang::Bash, "function_definition") => ("function", name()?),
        _ => return None,
    })
}

/// Markdown has no grammar here: its headings (outside code fences) are its outline.
fn headings(text: &str) -> Vec<Symbol> {
    let mut fence = false;
    let mut out = Vec::new();
    for (row, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fence = !fence;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let title = line[hashes..].trim();
        if !fence && (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') && !title.is_empty() {
            out.push(Symbol { name: title.trim_end_matches('#').trim_end().to_string(), kind: "heading", row, col: hashes + 1, end_row: row, depth: hashes - 1 });
        }
    }
    // A section runs until the next heading of the same or a higher level.
    for i in 0..out.len() {
        let end = out[i + 1..].iter().find(|s| s.depth <= out[i].depth).map_or(text.lines().count().saturating_sub(1), |s| s.row.saturating_sub(1));
        out[i].end_row = end.max(out[i].row);
    }
    out
}

/// Selectors and at-rules with a block (@media, @keyframes…); `//` comments only in SCSS and Less.
fn css(text: &str, line_comments: bool) -> Vec<Symbol> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Symbol> = Vec::new();
    // The symbol (if any) of each block we are inside.
    let mut open: Vec<Option<usize>> = Vec::new();
    let (mut prelude, mut start) = (String::new(), None);
    let (mut row, mut col) = (0, 0);
    let (mut block, mut line, mut quote, mut interp, mut skip) = (false, false, None, 0usize, 0usize);
    for (i, &c) in chars.iter().enumerate() {
        let at = (row, col);
        if c == '\n' {
            (row, col) = (row + 1, 0);
        } else {
            col += 1;
        }
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        if block {
            if c == '*' && next == Some('/') {
                (block, skip) = (false, 1);
            }
        } else if line {
            line = c != '\n';
        } else if let Some(q) = quote {
            prelude.push(c);
            if c == '\\' {
                prelude.extend(next);
                skip = 1;
            } else if c == q {
                quote = None;
            }
        } else if interp > 0 {
            // `#{$name}` in SCSS: its braces are not blocks.
            prelude.push(c);
            interp = if c == '{' { interp + 1 } else if c == '}' { interp - 1 } else { interp };
        } else {
            match c {
                '/' if next == Some('*') => (block, skip) = (true, 1),
                '/' if line_comments && next == Some('/') && i.checked_sub(1).map(|p| chars[p]) != Some(':') => line = true,
                '"' | '\'' => {
                    quote = Some(c);
                    prelude.push(c);
                }
                '#' if next == Some('{') => {
                    prelude.push_str("#{");
                    (interp, skip) = (1, 1);
                }
                '{' => {
                    let name = shown(&prelude);
                    let inside_keyframes = open.iter().flatten().any(|&s| out[s].kind == "keyframes");
                    let control = ["@if", "@else", "@each", "@for", "@while"].iter().any(|k| name.starts_with(k));
                    let symbol_at = (!name.is_empty() && !inside_keyframes && !control).then(|| {
                        let kind = match name.strip_prefix('@').map(|r| r.split(|c: char| !c.is_alphanumeric() && c != '-').next().unwrap_or("")) {
                            Some("media" | "supports" | "container") => "media",
                            Some(n) if n.ends_with("keyframes") => "keyframes",
                            Some(_) => "at-rule",
                            None => "selector",
                        };
                        let (r, c) = start.unwrap_or(at);
                        out.push(symbol(&name, kind, r, c, open.iter().flatten().count()));
                        out.len() - 1
                    });
                    open.push(symbol_at);
                    prelude.clear();
                    start = None;
                }
                '}' => {
                    if let Some(Some(s)) = open.pop() {
                        out[s].end_row = at.0;
                    }
                    prelude.clear();
                    start = None;
                }
                ';' => {
                    prelude.clear();
                    start = None;
                }
                _ => {
                    if !c.is_whitespace() && start.is_none() {
                        start = Some(at);
                    }
                    prelude.push(c);
                }
            }
        }
    }
    let last = text.lines().count().saturating_sub(1);
    for s in open.into_iter().flatten() {
        out[s].end_row = last;
    }
    out
}

/// Elements with an id, and the text of h1–h6 headings.
fn html(text: &str) -> Vec<Symbol> {
    const VOID: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
    let lower = text.to_ascii_lowercase();
    let line_starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let place = |at: usize| {
        let row = line_starts.partition_point(|&s| s <= at) - 1;
        (row, text[line_starts[row]..at].chars().count())
    };
    let mut out: Vec<Symbol> = Vec::new();
    // Open elements: (tag, its symbol when it has one).
    let mut open: Vec<(String, Option<usize>)> = Vec::new();
    let mut i = 0;
    while let Some(p) = lower[i..].find('<') {
        let at = i + p;
        let rest = &lower[at..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(text.len(), |e| at + e + 3);
            continue;
        }
        let closing = rest.starts_with("</");
        let name: String = rest[if closing { 2 } else { 1 }..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        if name.is_empty() {
            i = at + 1;
            continue;
        }
        if closing {
            if let Some(k) = open.iter().rposition(|(n, _)| *n == name) {
                for (_, s) in open.drain(k..) {
                    if let Some(s) = s {
                        out[s].end_row = place(at).0;
                    }
                }
            }
            i = at + 2;
            continue;
        }
        // The end of the tag: a `>` outside quotes.
        let mut q = None;
        let end = text[at..].char_indices().find(|&(_, c)| match (q, c) {
            (None, '"' | '\'') => {
                q = Some(c);
                false
            }
            (Some(x), c) if c == x => {
                q = None;
                false
            }
            (None, '>') => true,
            _ => false,
        });
        let Some((e, _)) = end else { break };
        let (tag, after) = (&text[at..at + e], at + e + 1);
        let (row, col) = place(at);
        let heading = name.strip_prefix('h').and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| (1..=6).contains(&n));
        let depth = open.iter().filter(|o| o.1.is_some()).count();
        let mut sym = None;
        if heading {
            let stop = lower[after..].find(&format!("</{name}")).map_or(text.len(), |c| after + c);
            let (mut label, mut in_tag) = (String::new(), false);
            for c in text[after..stop].chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    _ if !in_tag => label.push(c),
                    _ => {}
                }
            }
            if !shown(&label).is_empty() {
                let mut s = symbol(&label, "heading", row, col, depth);
                s.end_row = place(stop).0;
                out.push(s);
            }
        } else if let Some(id) = attr(tag, "id").filter(|id| !id.is_empty()) {
            out.push(symbol(&format!("{name}#{id}"), "element", row, col, depth));
            sym = Some(out.len() - 1);
        }
        i = after;
        if matches!(name.as_str(), "script" | "style") && !tag.ends_with('/') {
            i = lower[after..].find(&format!("</{name}")).map_or(text.len(), |c| after + c);
        } else if !VOID.contains(&name.as_str()) && !tag.ends_with('/') {
            open.push((name, sym));
        }
    }
    let last = text.lines().count().saturating_sub(1);
    for s in open.into_iter().filter_map(|o| o.1) {
        out[s].end_row = last;
    }
    out
}

/// The value of attribute `name` in the text of one tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        if !tag[..at].ends_with(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
            continue;
        }
        let Some(rest) = tag[from..].trim_start().strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        return match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next(),
            _ => rest.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next(),
        };
    }
    None
}

/// Keys, nested by how far they are indented (a `- key:` list item counts at the key's column).
fn yaml(text: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    // (indent, symbol) of the keys we are inside.
    let mut open: Vec<(usize, usize)> = Vec::new();
    // The indent of a key whose `|` or `>` text we are in: its lines are not keys.
    let mut block: Option<usize> = None;
    let mut last = 0;
    for (row, line) in text.lines().enumerate() {
        let mut body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        let mut at = line.chars().count() - body.chars().count();
        if block.is_some_and(|b| at > b) {
            last = row;
            continue;
        }
        block = None;
        if body.starts_with('#') {
            continue;
        }
        if body == "---" || body == "..." || body.starts_with("--- ") {
            for (_, s) in open.drain(..) {
                out[s].end_row = last;
            }
            continue;
        }
        while let Some(r) = body.strip_prefix("- ") {
            let t = r.trim_start();
            at += body.chars().count() - t.chars().count();
            body = t;
        }
        let Some((key, after, quoted)) = yaml_key(body) else {
            last = row;
            continue;
        };
        while open.last().is_some_and(|(i, _)| *i >= at) {
            let (_, s) = open.pop().unwrap();
            out[s].end_row = last;
        }
        out.push(symbol(key, "key", row, at + quoted as usize, open.len()));
        open.push((at, out.len() - 1));
        if after.starts_with(['|', '>']) {
            block = Some(at);
        }
        last = row;
    }
    for (_, s) in open {
        out[s].end_row = last;
    }
    out
}

/// (key, what follows the colon, whether the key was quoted) of a `key: value` line.
fn yaml_key(s: &str) -> Option<(&str, &str, bool)> {
    if let Some(q @ ('"' | '\'')) = s.chars().next() {
        let end = s[1..].find(q)?;
        let after = s[end + 2..].trim_start().strip_prefix(':')?;
        return (after.is_empty() || after.starts_with(char::is_whitespace)).then(|| (&s[1..1 + end], after.trim_start(), true));
    }
    if s.starts_with(['{', '[', '#', '&', '*', '!', '|', '>', '%', '@', '`', '?']) {
        return None;
    }
    let mut prev = ' ';
    for (i, c) in s.char_indices() {
        let after = &s[i + 1..];
        if c == '#' && prev.is_whitespace() {
            return None;
        }
        if c == ':' && (after.is_empty() || after.starts_with(char::is_whitespace)) {
            let key = s[..i].trim_end();
            return (!key.is_empty()).then(|| (key, after.trim_start(), false));
        }
        prev = c;
    }
    None
}

/// [tables], [[arrays]] and the keys in them.
fn toml(text: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    let (mut table, mut last) = (None::<usize>, 0);
    // A key whose value goes over several lines: the quotes it waits for, or how many brackets are still open.
    let (mut multi, mut nest, mut value) = (None::<&str>, 0i32, None::<usize>);
    for (row, line) in text.lines().enumerate() {
        let body = line.trim();
        let indent = line.chars().count() - line.trim_start().chars().count();
        if let Some(d) = multi {
            if line.contains(d) {
                multi = None;
            }
        } else if nest > 0 {
            nest += net_brackets(body);
        } else if body.is_empty() || body.starts_with('#') {
            continue;
        } else if body.starts_with('[') {
            let array = body.starts_with("[[");
            let name = body.trim_start_matches('[').split(']').next().unwrap_or("").trim();
            if let Some(t) = table {
                out[t].end_row = last;
            }
            out.push(symbol(name, if array { "array" } else { "table" }, row, indent + if array { 2 } else { 1 }, 0));
            table = Some(out.len() - 1);
            value = None;
            last = row;
            continue;
        } else if let Some((key, rest)) = body.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                last = row;
                continue;
            }
            let quoted = key.starts_with(['"', '\'']);
            out.push(symbol(key.trim_matches(['"', '\'']), "key", row, indent + quoted as usize, table.is_some() as usize));
            value = Some(out.len() - 1);
            multi = ["\"\"\"", "'''"].into_iter().find(|d| rest.matches(d).count() % 2 == 1);
            nest = net_brackets(rest);
        }
        if let Some(v) = value {
            out[v].end_row = row;
        }
        last = row;
    }
    if let Some(t) = table {
        out[t].end_row = last;
    }
    out
}

/// Open minus closed `[` and `{` in a value (not counting quoted text or a comment).
fn net_brackets(s: &str) -> i32 {
    let (mut n, mut quote) = (0, None);
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => break,
            (None, '[' | '{') => n += 1,
            (None, ']' | '}') => n -= 1,
            _ => {}
        }
    }
    n
}

/// Object keys, nested by the objects they sit in (`//` and `/* */` comments are skipped).
fn json(text: &str) -> Vec<Symbol> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Symbol> = Vec::new();
    // For each open { or [: the key it is the value of.
    let mut open: Vec<Option<usize>> = Vec::new();
    // The key whose value is next, and a string just read that a `:` would make a key.
    let (mut pending, mut string) = (None::<usize>, None::<(String, usize, usize)>);
    let (mut row, mut col) = (0, 0);
    let (mut block, mut line, mut inside, mut skip) = (false, false, None::<(String, usize, usize)>, 0usize);
    for (i, &c) in chars.iter().enumerate() {
        let at = (row, col);
        if c == '\n' {
            (row, col) = (row + 1, 0);
        } else {
            col += 1;
        }
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        if block {
            if c == '*' && next == Some('/') {
                (block, skip) = (false, 1);
            }
        } else if line {
            line = c != '\n';
        } else if let Some(s) = &mut inside {
            match c {
                '\\' => {
                    s.0.push(c);
                    s.0.extend(next);
                    skip = 1;
                }
                '"' => string = inside.take(),
                _ => s.0.push(c),
            }
        } else if c.is_whitespace() {
        } else if c == '/' && next == Some('*') {
            (block, skip) = (true, 1);
        } else if c == '/' && next == Some('/') {
            line = true;
        } else {
            match c {
                '"' => inside = Some((String::new(), at.0, at.1 + 1)),
                ':' => {
                    if let Some((name, r, cl)) = string.take() {
                        out.push(symbol(&name, "key", r, cl, open.iter().flatten().count()));
                        pending = Some(out.len() - 1);
                    }
                }
                '{' | '[' => open.push(pending.take()),
                '}' | ']' => {
                    pending = None;
                    if let Some(Some(k)) = open.pop() {
                        out[k].end_row = at.0;
                    }
                }
                ',' => pending = None,
                _ => {}
            }
            if c != ':' {
                string = None;
            }
        }
    }
    out
}

/// The declarations the row is inside, outermost first.
pub fn around(symbols: &[Symbol], row: usize) -> Vec<&Symbol> {
    symbols.iter().filter(|s| s.row <= row && row <= s.end_row).collect()
}

/// A buffer's symbols, read again only when its text changed.
#[derive(Default)]
pub struct Cache(RefCell<Option<(u64, usize, Arc<Vec<Symbol>>)>>);

impl Editor {
    /// This file's symbols (empty for languages without a reader).
    pub fn symbols(&self) -> Arc<Vec<Symbol>> {
        let Some(path) = &self.path else { return Arc::default() };
        let key = (self.buf.version(), self.buf.len());
        if let Some((v, l, s)) = &*self.symbol_cache.0.borrow()
            && (*v, *l) == key
        {
            return s.clone();
        }
        let symbols = Arc::new(of(path, &self.buf.text()));
        *self.symbol_cache.0.borrow_mut() = Some((key.0, key.1, symbols.clone()));
        symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(path: &str, text: &str) -> Vec<(String, &'static str, usize)> {
        of(Path::new(path), text).into_iter().map(|s| (s.name, s.kind, s.row)).collect()
    }

    #[test]
    fn rust_items_and_their_methods() {
        let text = "pub struct Cart {}\nimpl Cart {\n    pub fn add(&self) {\n        fn inner() {}\n    }\n}\nenum Kind { A }\nfn total() {}\n";
        assert_eq!(
            names("a.rs", text),
            [("Cart".into(), "struct", 0), ("Cart".into(), "impl", 1), ("add".into(), "method", 2), ("Kind".into(), "enum", 6), ("total".into(), "function", 7)],
            "a function's inner items are left out"
        );
    }

    #[test]
    fn typescript_functions_classes_and_arrow_consts() {
        let text = "export function a() {}\nclass Cart {\n  add(x: number) {}\n}\nconst total = (c: Cart) => 1;\nconst n = 3;\ninterface Item {}\ntype Id = string;\n";
        assert_eq!(
            names("a.ts", text),
            [("a".into(), "function", 0), ("Cart".into(), "class", 1), ("add".into(), "method", 2), ("total".into(), "function", 4), ("Item".into(), "interface", 6), ("Id".into(), "type", 7)]
        );
    }

    #[test]
    fn python_go_java_and_headings() {
        assert_eq!(names("a.py", "class A:\n    def f(self):\n        pass\ndef g():\n    pass\n"), [("A".into(), "class", 0), ("f".into(), "method", 1), ("g".into(), "function", 3)]);
        assert_eq!(names("a.go", "package a\nfunc F() {}\ntype T struct{}\nfunc (t T) M() {}\n"), [("F".into(), "function", 1), ("T".into(), "type", 2), ("M".into(), "method", 3)]);
        assert_eq!(names("A.java", "class A {\n  A() {}\n  void run() {}\n}\n"), [("A".into(), "class", 0), ("A".into(), "method", 1), ("run".into(), "method", 2)]);
        assert_eq!(names("a.md", "# Title\n```\n# not one\n```\n## Part\n"), [("Title".into(), "heading", 0), ("Part".into(), "heading", 4)]);
        assert!(names("a.txt", "fn x() {}").is_empty());
    }

    #[test]
    fn the_symbols_around_a_row() {
        let s = of(Path::new("a.rs"), "impl Cart {\n    fn add(&self) {\n        let x = 1;\n    }\n    fn total(&self) {}\n}\nfn after() {}\n");
        let at = |row| around(&s, row).iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        assert_eq!(at(2), ["Cart", "add"]);
        assert_eq!(at(4), ["Cart", "total"]);
        assert_eq!(at(6), ["after"]);
    }

    #[test]
    fn columns_count_characters() {
        let s = of(Path::new("a.rs"), "// é\nfn é() {}\nconst Δ: u8 = 1;\n");
        assert_eq!((s[0].name.as_str(), s[0].col), ("é", 3));
        assert_eq!((s[1].name.as_str(), s[1].col), ("Δ", 6));
    }

    fn outline(path: &str, text: &str) -> Vec<(String, &'static str, usize, usize, usize)> {
        of(Path::new(path), text).into_iter().map(|s| (s.name, s.kind, s.row, s.end_row, s.depth)).collect()
    }

    fn s(name: &str, kind: &'static str, row: usize, end_row: usize, depth: usize) -> (String, &'static str, usize, usize, usize) {
        (name.into(), kind, row, end_row, depth)
    }

    #[test]
    fn css_selectors_media_and_keyframes() {
        let text = "/* a { */\n.a, .b > c {\n  color: red; /* } */\n}\n@media (min-width: 600px) {\n  .a:hover {\n    background: url(\"x{y}\");\n  }\n}\n@keyframes spin {\n  from { opacity: 0; }\n  to { opacity: 1; }\n}\n";
        assert_eq!(
            outline("a.css", text),
            [s(".a, .b > c", "selector", 1, 3, 0), s("@media (min-width: 600px)", "media", 4, 8, 0), s(".a:hover", "selector", 5, 7, 1), s("@keyframes spin", "keyframes", 9, 12, 0)],
            "comments and strings are skipped, keyframe steps are not listed"
        );
    }

    #[test]
    fn scss_and_less_nesting_interpolation_and_line_comments() {
        let text = "// .gone {\n.card {\n  &:hover { x: y; }\n  .#{$name}-body {\n    @if $a { z: 1; }\n  }\n}\n@mixin big($n) { a: b; }\n";
        assert_eq!(
            outline("a.scss", text),
            [s(".card", "selector", 1, 6, 0), s("&:hover", "selector", 2, 2, 1), s(".#{$name}-body", "selector", 3, 5, 1), s("@mixin big($n)", "at-rule", 7, 7, 0)]
        );
        assert_eq!(outline("a.less", "// x\n.b { .c { d: e; } }\n").len(), 2);
        assert_eq!(outline("a.css", "a { background: url(http://x/y.png); }\n").len(), 1, "// is not a comment in plain CSS or after a colon");
    }

    #[test]
    fn html_ids_and_headings() {
        let text = "<!-- <div id=\"no\"> -->\n<body>\n  <div id=\"app\" class=\"a\">\n    <h1>Hello <b>big</b>\n      world</h1>\n    <img id='logo' src=\"a>b.png\">\n    <p data-id=\"x\">text</p>\n  </div>\n  <script>if (a < b) { x = '<h2 id=z>' }</script>\n  <H2 class=sub>Part</H2>\n</body>\n";
        assert_eq!(
            outline("a.html", text),
            [s("div#app", "element", 2, 7, 0), s("Hello big world", "heading", 3, 4, 1), s("img#logo", "element", 5, 5, 1), s("Part", "heading", 9, 9, 0)]
        );
        assert!(outline("a.htm", "<p>no ids</p>").is_empty());
    }

    #[test]
    fn yaml_keys_nested_by_indentation() {
        let text = "# top\nname: app\nservices:\n  web:\n    image: x\n    command: |\n      not: a key\n    ports:\n      - 80\n  - job: one\n    run: two\n\"quoted\": 1\nurl: http://a.b\n---\nnext: 1\n";
        assert_eq!(
            outline("a.yml", text),
            [
                s("name", "key", 1, 1, 0),
                s("services", "key", 2, 10, 0),
                s("web", "key", 3, 10, 1),
                s("image", "key", 4, 4, 2),
                s("command", "key", 5, 6, 2),
                s("ports", "key", 7, 8, 2),
                s("job", "key", 9, 9, 2),
                s("run", "key", 10, 10, 2),
                s("quoted", "key", 11, 11, 0),
                s("url", "key", 12, 12, 0),
                s("next", "key", 14, 14, 0),
            ]
        );
        assert_eq!(of(Path::new("a.yaml"), "- a: 1\n")[0].col, 2, "a list item's key is at the key");
    }

    #[test]
    fn toml_tables_arrays_and_keys() {
        let text = "title = \"x\"\n# c\n[server]\nhost = \"\"\"\n[not.a.table]\n\"\"\"\nports = [\n  [1, 2],\n  3,\n]\n[[bin]]\nname = 'a'\n[a.b]\n";
        assert_eq!(
            outline("Cargo.toml", text),
            [
                s("title", "key", 0, 0, 0),
                s("server", "table", 2, 9, 0),
                s("host", "key", 3, 5, 1),
                s("ports", "key", 6, 9, 1),
                s("bin", "array", 10, 11, 0),
                s("name", "key", 11, 11, 1),
                s("a.b", "table", 12, 12, 0),
            ]
        );
    }

    #[test]
    fn json_keys_nested_with_their_ends() {
        let text = "{\n  \"name\": \"a, \\\"b\\\": c\",\n  // note\n  \"deps\": {\n    \"x\": [ { \"y\": 1 } ],\n    \"z\": null\n  },\n  \"tags\": [\"k\", \"v\"]\n}\n";
        assert_eq!(
            outline("package.json", text),
            [s("name", "key", 1, 1, 0), s("deps", "key", 3, 6, 0), s("x", "key", 4, 4, 1), s("y", "key", 4, 4, 2), s("z", "key", 5, 5, 1), s("tags", "key", 7, 7, 0)],
            "a string that is only a value is not a key"
        );
        assert_eq!(of(Path::new("a.json"), "{\"é\": 1}")[0].col, 2);
        assert!(of(Path::new("a.json"), "[1, \"two\", {}]").is_empty());
    }

    #[test]
    fn huge_data_files_list_only_the_first_keys() {
        let text: String = (0..MAX_KEYS + 10).map(|i| format!("k{i}: 1\n")).collect();
        assert_eq!(of(Path::new("a.yml"), &text).len(), MAX_KEYS);
    }
}
