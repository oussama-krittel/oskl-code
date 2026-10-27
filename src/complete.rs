//! Editing help for markup and scripts without a language server: closing tags as you type,
//! and a suggestion list for tags, attributes, types/words from the file and import paths.
//!
//! Everything is computed on demand from a window of text around the cursor, so nothing stays
//! in memory between keystrokes except the list currently shown.

use crate::buffer::Buffer;
use crate::highlight::Lang;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavor {
    Html,
    /// JS/TSX: JSX tags plus script.
    Jsx,
    /// Plain TS: script only.
    Script,
}

pub fn flavor(lang: Option<Lang>) -> Option<Flavor> {
    match lang? {
        Lang::Html => Some(Flavor::Html),
        Lang::Tsx | Lang::JavaScript => Some(Flavor::Jsx),
        Lang::TypeScript => Some(Flavor::Script),
        _ => None,
    }
}

const HTML_VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

/// How far back the cursor's tag is looked for, and how far back unclosed tags are tracked.
const TAG_WINDOW: usize = 4_000;
const NEST_WINDOW: usize = 40_000;

fn window(buf: &Buffer, pos: usize, max: usize) -> Vec<char> {
    let start = pos.saturating_sub(max);
    buf.rope.slice(start..pos).chars().collect()
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '$')
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// A `<` at `i` that opens a tag: `<name`, `</`, `<>` — not `a < b`, `a<b` or `Array<T>`.
fn starts_tag(t: &[char], i: usize) -> bool {
    let next_ok = match t.get(i + 1) {
        None => true,
        Some(c) => c.is_alphabetic() || matches!(c, '/' | '>' | '_'),
    };
    // `text</b>`: a closing tag can follow anything.
    let prev_ok = t.get(i + 1) == Some(&'/') || !i.checked_sub(1).map(|p| t[p]).is_some_and(|c| is_ident_char(c) || matches!(c, ')' | ']' | '.'));
    next_ok && prev_ok
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Name,
    Between,
    Attr,
    AfterEq,
    Unquoted,
    Quote(char),
    Brace(u32),
}

#[derive(Debug)]
struct Tag {
    closing: bool,
    name: String,
    /// Index of the closing `>` when the tag ends before the window does.
    end: Option<usize>,
    self_closing: bool,
    /// Where the scan stopped when the tag is still open at the end of the window.
    state: St,
    /// The attribute name being typed (state `Attr`).
    attr: String,
}

/// Parses the tag whose `<` is at `lt`. None when it isn't a tag after all (`<T,>`, `a <b; c`).
fn parse_tag(t: &[char], lt: usize) -> Option<Tag> {
    let n = t.len();
    let mut j = lt + 1;
    let closing = t.get(j) == Some(&'/');
    if closing {
        j += 1;
    }
    let mut name = String::new();
    while j < n && is_name_char(t[j]) {
        name.push(t[j]);
        j += 1;
    }
    let mut tag = Tag { closing, name, end: None, self_closing: false, state: St::Name, attr: String::new() };
    if j == n {
        return Some(tag);
    }
    let mut st = St::Between;
    let mut prev = ' ';
    while j < n {
        let c = t[j];
        st = match st {
            St::Name | St::Between | St::Attr | St::Unquoted if c == '>' => {
                tag.end = Some(j);
                tag.self_closing = prev == '/';
                return Some(tag);
            }
            St::Unquoted if c.is_whitespace() => St::Between,
            St::Unquoted => St::Unquoted,
            St::Name | St::Between | St::Attr => {
                if c == '"' || c == '\'' {
                    St::Quote(c)
                } else if c == '{' {
                    St::Brace(1)
                } else if c == '=' && st == St::Attr {
                    St::AfterEq
                } else if c.is_whitespace() || c == '/' {
                    St::Between
                } else if is_name_char(c) {
                    if st != St::Attr {
                        tag.attr.clear();
                    }
                    tag.attr.push(c);
                    St::Attr
                } else {
                    return None;
                }
            }
            St::AfterEq => match c {
                '"' | '\'' => St::Quote(c),
                '{' => St::Brace(1),
                c if c.is_whitespace() => St::AfterEq,
                '>' => return None,
                _ => St::Unquoted,
            },
            St::Quote(q) => {
                if c == q {
                    St::Between
                } else {
                    st
                }
            }
            St::Brace(d) => match c {
                '{' => St::Brace(d + 1),
                '}' if d == 1 => St::Between,
                '}' => St::Brace(d - 1),
                _ => st,
            },
        };
        if st != St::Attr && st != St::Between {
            tag.attr.clear();
        }
        if !c.is_whitespace() {
            prev = c;
        }
        j += 1;
    }
    tag.state = st;
    Some(tag)
}

/// The tag the end of `t` is inside, if any.
fn open_tag(t: &[char]) -> Option<(usize, Tag)> {
    let lim = t.len().saturating_sub(TAG_WINDOW);
    let mut i = t.len();
    while i > lim {
        i -= 1;
        match t[i] {
            '<' if starts_tag(t, i) => {
                let tag = parse_tag(t, i)?;
                return tag.end.is_none().then_some((i, tag));
            }
            _ => {}
        }
    }
    None
}

/// A generic parameter (`<T>`, `<K extends …>`), not a component.
fn looks_generic(name: &str) -> bool {
    name.len() == 1 && name.chars().all(|c| c.is_ascii_uppercase())
}

/// Called right after `>` was typed at `pos - 1`: the closing tag to insert after the cursor.
pub fn close_tag_after_gt(buf: &Buffer, pos: usize, fl: Flavor) -> Option<String> {
    if fl == Flavor::Script || pos == 0 {
        return None;
    }
    let t = window(buf, pos - 1, TAG_WINDOW);
    let (_, tag) = open_tag(&t)?;
    if tag.closing || !matches!(tag.state, St::Name | St::Between | St::Attr | St::Unquoted) || t.last() == Some(&'/') {
        return None;
    }
    if tag.name.is_empty() {
        return (fl == Flavor::Jsx).then(|| "</>".to_string());
    }
    if looks_generic(&tag.name) || (fl == Flavor::Html && HTML_VOID.contains(&tag.name.to_ascii_lowercase().as_str())) {
        return None;
    }
    let close = format!("</{}>", tag.name);
    let after: String = buf.rope.slice(pos..(pos + close.chars().count()).min(buf.len())).chars().collect();
    (after != close).then_some(close)
}

/// The innermost tag still open at the end of `t`.
fn innermost_open(t: &[char], fl: Flavor) -> Option<String> {
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if t[i] == '<' && t.get(i + 1..i + 4) == Some(&['!', '-', '-']) {
            let mut j = i + 4;
            while j + 2 < t.len() && !(t[j] == '-' && t[j + 1] == '-' && t[j + 2] == '>') {
                j += 1;
            }
            i = j + 3;
            continue;
        }
        if t[i] == '<' && starts_tag(t, i) {
            let Some(tag) = parse_tag(t, i) else {
                i += 1;
                continue;
            };
            let Some(end) = tag.end else { break };
            if tag.closing {
                if let Some(k) = stack.iter().rposition(|n| *n == tag.name) {
                    stack.truncate(k);
                }
            } else if !tag.self_closing && !looks_generic(&tag.name) && !(fl == Flavor::Html && HTML_VOID.contains(&tag.name.to_ascii_lowercase().as_str())) {
                stack.push(tag.name);
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    stack.pop()
}

/// Called right after `/` was typed at `pos - 1` following `<`: the rest of the closing tag.
pub fn close_tag_after_slash(buf: &Buffer, pos: usize, fl: Flavor) -> Option<String> {
    if fl == Flavor::Script || pos < 2 || buf.char_at(pos - 2) != Some('<') {
        return None;
    }
    let t = window(buf, pos - 2, NEST_WINDOW);
    let name = innermost_open(&t, fl)?;
    if buf.char_at(pos) == Some('>') {
        return Some(name);
    }
    Some(format!("{name}>"))
}

// ------------------------------------------------------------------ suggestions

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Tag,
    Component,
    Attr,
    Type,
    Keyword,
    Word,
    Folder,
    File,
}

impl Kind {
    pub fn glyph(self) -> &'static str {
        match self {
            Kind::Tag => "<>",
            Kind::Component => "◇",
            Kind::Attr => "=",
            Kind::Type => "T",
            Kind::Keyword => "k",
            Kind::Word => "ab",
            Kind::Folder => "▸",
            Kind::File => "·",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tag => "tag",
            Kind::Component => "component",
            Kind::Attr => "attribute",
            Kind::Type => "type",
            Kind::Keyword => "keyword",
            Kind::Word => "",
            Kind::Folder => "folder",
            Kind::File => "file",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    pub kind: Kind,
    /// Text inserted after the label (`=""`, `={}`, `/`).
    pub suffix: &'static str,
    /// Where the cursor lands, counted back from the end of the inserted text.
    pub back: usize,
}

impl Item {
    fn new(label: impl Into<String>, kind: Kind) -> Self {
        Self { label: label.into(), kind, suffix: "", back: 0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ctx {
    /// `<di|`
    TagName { prefix: String },
    /// `<div cla|`
    Attr { tag: String, prefix: String },
    /// `from "./comp|`
    Path { typed: String },
    /// `useSt|`, `: Us|`
    Word { prefix: String, type_pos: bool },
}

impl Ctx {
    /// The text the chosen item replaces (just before the cursor).
    pub fn prefix(&self) -> &str {
        match self {
            Ctx::TagName { prefix } | Ctx::Attr { prefix, .. } | Ctx::Word { prefix, .. } => prefix,
            Ctx::Path { typed } => typed.rsplit('/').next().unwrap_or(""),
        }
    }

    /// Same list of candidates (only the filter differs).
    pub fn same_source(&self, other: &Ctx) -> bool {
        match (self, other) {
            (Ctx::TagName { .. }, Ctx::TagName { .. }) => true,
            (Ctx::Attr { tag: a, .. }, Ctx::Attr { tag: b, .. }) => a == b,
            (Ctx::Path { typed: a }, Ctx::Path { typed: b }) => a.rsplit_once('/').map(|p| p.0) == b.rsplit_once('/').map(|p| p.0),
            (Ctx::Word { type_pos: a, .. }, Ctx::Word { type_pos: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// What the cursor at `pos` could be completed as.
pub fn context(buf: &Buffer, pos: usize, fl: Flavor) -> Option<Ctx> {
    let (row, col) = buf.pos_to_rc(pos);
    let line: Vec<char> = buf.line(row).chars().take(col).collect();
    if fl != Flavor::Html {
        if let Some(typed) = import_path(&line) {
            return Some(Ctx::Path { typed });
        }
    }
    if fl != Flavor::Script {
        let t = window(buf, pos, TAG_WINDOW);
        if let Some((_, tag)) = open_tag(&t) {
            if tag.closing {
                return None;
            }
            return match tag.state {
                St::Name => {
                    // In JSX a bare `<` is too often a comparison to pop a list for it.
                    if fl == Flavor::Jsx && tag.name.is_empty() {
                        return None;
                    }
                    Some(Ctx::TagName { prefix: tag.name })
                }
                St::Attr => Some(Ctx::Attr { tag: tag.name, prefix: tag.attr }),
                _ => None,
            };
        }
    }
    if fl == Flavor::Html {
        return None;
    }
    // A word in script: not inside a string or a line comment.
    let start = line.iter().rposition(|c| !is_ident_char(*c)).map_or(0, |p| p + 1);
    let prefix: String = line[start..].iter().collect();
    if prefix.chars().count() < 2 || prefix.starts_with(|c: char| c.is_ascii_digit()) || in_string_or_comment(&line[..start]) {
        return None;
    }
    let before: String = line[..start].iter().collect();
    let b = before.trim_end();
    let type_pos = b.ends_with(':') || b.ends_with('|') || b.ends_with('&') || b.ends_with('<') || b.ends_with(" as") || b.ends_with("extends") || b.ends_with("implements") || b.ends_with("keyof");
    Some(Ctx::Word { prefix, type_pos })
}

fn in_string_or_comment(line: &[char]) -> bool {
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        match quote {
            Some(_) if c == '\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if matches!(c, '"' | '\'' | '`') => quote = Some(c),
            None if c == '/' && line.get(i + 1) == Some(&'/') => return true,
            None => {}
        }
        i += 1;
    }
    quote.is_some()
}

/// The partial path when the cursor is inside the string of an import/require.
fn import_path(line: &[char]) -> Option<String> {
    let q = line.iter().rposition(|c| *c == '"' || *c == '\'')?;
    let quote = line[q];
    // The string must still be open: an even number of these quotes before it.
    if line[..q].iter().filter(|c| **c == quote).count() % 2 == 1 {
        return None;
    }
    let before: String = line[..q].iter().collect();
    let b = before.trim_end();
    let ok = b.ends_with("from") || b.ends_with("import") || b.ends_with("require(") || b.ends_with("import(") || b.ends_with("jest.mock(");
    ok.then(|| line[q + 1..].iter().collect())
}

fn has_react_native(buf: &Buffer) -> (bool, bool) {
    let mut native = false;
    let mut dom = false;
    for row in 0..buf.line_count().min(300) {
        let l = buf.line(row);
        if l.contains("react-native") {
            native = true;
        }
        if l.contains("react-dom") || l.contains("next/") {
            dom = true;
        }
    }
    (native, dom)
}

/// Identifiers in the file (capped), and which of them name types.
fn file_words(buf: &Buffer) -> (Vec<String>, HashSet<String>) {
    const MAX_CHARS: usize = 2_000_000;
    const MAX_WORDS: usize = 8_000;
    let mut words: HashSet<String> = HashSet::new();
    let mut types: HashSet<String> = HashSet::new();
    let mut cur = String::new();
    let mut last_kw = String::new();
    let flush = |cur: &mut String, last_kw: &mut String, words: &mut HashSet<String>, types: &mut HashSet<String>| {
        if cur.is_empty() {
            return;
        }
        if matches!(last_kw.as_str(), "type" | "interface" | "class" | "enum") {
            types.insert(cur.clone());
        }
        if cur.chars().count() >= 2 && !cur.starts_with(|c: char| c.is_ascii_digit()) && words.len() < MAX_WORDS {
            words.insert(cur.clone());
        }
        *last_kw = std::mem::take(cur);
    };
    for c in buf.rope.chars().take(MAX_CHARS) {
        if is_ident_char(c) {
            cur.push(c);
        } else {
            flush(&mut cur, &mut last_kw, &mut words, &mut types);
        }
    }
    flush(&mut cur, &mut last_kw, &mut words, &mut types);
    let mut v: Vec<String> = words.into_iter().collect();
    v.sort();
    (v, types)
}

/// Attribute names already used on `<tag` elsewhere in the file (a component's own props).
fn props_used(buf: &Buffer, tag: &str) -> Vec<String> {
    let needle = format!("<{tag}");
    let mut out: HashSet<String> = HashSet::new();
    for row in 0..buf.line_count().min(20_000) {
        let l = buf.line(row);
        if !l.contains(&needle) {
            continue;
        }
        let start = buf.line_start(row);
        let end = (start + 4_000).min(buf.len());
        let t: Vec<char> = buf.rope.slice(start..end).chars().collect();
        let mut i = 0;
        while i + needle.len() <= t.len() {
            if t[i] == '<' && t[i + 1..].iter().take(tag.chars().count()).copied().eq(tag.chars()) && t.get(i + 1 + tag.chars().count()).is_none_or(|c| !is_name_char(*c)) {
                let mut j = i + 1 + tag.chars().count();
                let mut depth = 0i32;
                let mut quote: Option<char> = None;
                let mut name = String::new();
                while j < t.len() {
                    let c = t[j];
                    match (quote, c) {
                        (Some(q), c) if c == q => quote = None,
                        (Some(_), _) => {}
                        (None, '"' | '\'' | '`') => quote = Some(c),
                        (None, '{') => depth += 1,
                        (None, '}') => depth -= 1,
                        (None, '>') if depth == 0 => break,
                        (None, c) if depth == 0 && is_name_char(c) => name.push(c),
                        _ => {
                            if !name.is_empty() && depth == 0 {
                                out.insert(std::mem::take(&mut name));
                            }
                            name.clear();
                        }
                    }
                    j += 1;
                }
                i = j;
            }
            i += 1;
        }
    }
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort();
    v
}

/// All candidates for a context (before filtering by what's typed).
pub fn candidates(buf: &Buffer, ctx: &Ctx, fl: Flavor, file: Option<&Path>, root: Option<&Path>) -> Vec<Item> {
    match ctx {
        Ctx::TagName { .. } => {
            let mut out = Vec::new();
            if fl == Flavor::Html {
                out.extend(HTML_TAGS.iter().map(|t| Item::new(*t, Kind::Tag)));
                return out;
            }
            let (native, dom) = has_react_native(buf);
            let (words, _) = file_words(buf);
            let mut seen: HashSet<String> = HashSet::new();
            for w in words.iter().filter(|w| w.starts_with(|c: char| c.is_ascii_uppercase()) && !looks_generic(w)) {
                if seen.insert(w.clone()) {
                    out.push(Item::new(w.clone(), Kind::Component));
                }
            }
            if native {
                for c in RN_COMPONENTS {
                    if seen.insert(c.to_string()) {
                        out.push(Item::new(*c, Kind::Component));
                    }
                }
            }
            if !native || dom {
                out.extend(HTML_TAGS.iter().map(|t| Item::new(*t, Kind::Tag)));
            }
            out
        }
        Ctx::Attr { tag, .. } => attrs(buf, tag, fl),
        Ctx::Path { typed } => paths(typed, file, root),
        Ctx::Word { type_pos, .. } => {
            let (words, types) = file_words(buf);
            let mut out: Vec<Item> = Vec::new();
            let mut seen: HashSet<&str> = HashSet::new();
            if *type_pos {
                for t in TS_TYPES {
                    seen.insert(t);
                    out.push(Item::new(*t, Kind::Type));
                }
            }
            for w in &words {
                if seen.contains(w.as_str()) {
                    continue;
                }
                let is_type = types.contains(w) || (*type_pos && w.starts_with(|c: char| c.is_ascii_uppercase()));
                out.push(Item::new(w.clone(), if is_type { Kind::Type } else { Kind::Word }));
            }
            if !*type_pos {
                for k in TS_KEYWORDS {
                    if !words.iter().any(|w| w == k) {
                        out.push(Item::new(*k, Kind::Keyword));
                    }
                }
            }
            // In a type position, types first.
            if *type_pos {
                out.sort_by_key(|i| i.kind != Kind::Type);
            }
            out
        }
    }
}

fn attrs(buf: &Buffer, tag: &str, fl: Flavor) -> Vec<Item> {
    let mut names: Vec<String> = Vec::new();
    let component = tag.starts_with(|c: char| c.is_ascii_uppercase());
    if component {
        names.extend(props_used(buf, tag));
        if let Some((_, props)) = RN_PROPS.iter().find(|(c, _)| *c == tag) {
            names.extend(props.iter().map(|s| s.to_string()));
        }
        if RN_COMPONENTS.contains(&tag) {
            names.extend(RN_COMMON.iter().map(|s| s.to_string()));
        } else {
            names.extend(["key", "ref", "style", "children", "className"].iter().map(|s| s.to_string()));
        }
    } else {
        let lower = tag.to_ascii_lowercase();
        if let Some((_, a)) = HTML_ATTRS.iter().find(|(t, _)| *t == lower) {
            names.extend(a.iter().map(|s| s.to_string()));
        }
        if fl == Flavor::Jsx {
            names.extend(JSX_GLOBAL.iter().map(|s| s.to_string()));
        } else {
            names.extend(HTML_GLOBAL.iter().map(|s| s.to_string()));
        }
    }
    let mut seen = HashSet::new();
    names
        .into_iter()
        .filter(|n| seen.insert(n.clone()))
        .map(|n| {
            let (suffix, back) = attr_suffix(&n, fl);
            Item { label: n, kind: Kind::Attr, suffix, back }
        })
        .collect()
}

fn attr_suffix(name: &str, fl: Flavor) -> (&'static str, usize) {
    if BOOLEAN_ATTRS.contains(&name) {
        return ("", 0);
    }
    if fl == Flavor::Html {
        return ("=\"\"", 1);
    }
    let string_like = STRING_PROPS.contains(&name) || name.starts_with("aria-") || name.starts_with("data-");
    if string_like { ("=\"\"", 1) } else { ("={}", 1) }
}

/// Folders and files for an import path being typed.
fn paths(typed: &str, file: Option<&Path>, root: Option<&Path>) -> Vec<Item> {
    if typed.is_empty() {
        // Just opened the quotes: where paths start (packages show once a letter is typed).
        return ["./", "../", "@/"].iter().map(|s| Item::new(*s, Kind::Folder)).collect();
    }
    let (dir_part, _) = typed.rsplit_once('/').map(|(d, f)| (format!("{d}/"), f)).unwrap_or((String::new(), typed));
    let base: Option<PathBuf> = if typed.starts_with('.') {
        file.and_then(|f| f.parent()).map(|d| d.join(&dir_part))
    } else if let Some(rest) = dir_part.strip_prefix("@/").or_else(|| dir_part.strip_prefix("~/")) {
        root.map(|r| if r.join("src").is_dir() { r.join("src") } else { r.to_path_buf() }).map(|r| r.join(rest))
    } else if typed == "@" || typed == "~" {
        return vec![Item { label: format!("{typed}/"), kind: Kind::Folder, suffix: "", back: 0 }];
    } else {
        root.map(|r| r.join("node_modules").join(&dir_part))
    };
    if dir_part.is_empty() && typed.starts_with('.') {
        // `.` or `..` alone: offer the two starting points.
        return ["./", "../"].iter().map(|s| Item::new(*s, Kind::Folder)).collect();
    }
    let Some(base) = base else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&base) else { return Vec::new() };
    let in_modules = !typed.starts_with('.') && !typed.starts_with('@') && !typed.starts_with('~') || typed.starts_with("@") && !typed.starts_with("@/");
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in rd.flatten().take(2_000) {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || (!in_modules && name == "node_modules") {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir() || t.is_symlink()).unwrap_or(false) && e.path().is_dir();
        if is_dir {
            dirs.push(Item { label: name, kind: Kind::Folder, suffix: "/", back: 0 });
        } else if !in_modules {
            files.push(Item::new(strip_code_ext(&name), Kind::File));
        }
    }
    dirs.sort_by(|a, b| a.label.cmp(&b.label));
    files.sort_by(|a, b| a.label.cmp(&b.label));
    files.dedup_by(|a, b| a.label == b.label);
    dirs.extend(files);
    dirs
}

fn strip_code_ext(name: &str) -> &str {
    for ext in [".d.ts", ".tsx", ".ts", ".jsx", ".js", ".mjs", ".cjs"] {
        if let Some(s) = name.strip_suffix(ext) {
            return s;
        }
    }
    name
}

/// Filters and ranks candidates by what's typed: prefix matches first, then fuzzy ones.
pub fn filter(items: &[Item], prefix: &str, limit: usize) -> Vec<usize> {
    let p = prefix.to_lowercase();
    let mut scored: Vec<(u8, usize, usize)> = Vec::new();
    for (ix, it) in items.iter().enumerate() {
        if it.label == prefix && it.suffix.is_empty() {
            continue;
        }
        let l = it.label.to_lowercase();
        let score = if it.label.starts_with(prefix) {
            0
        } else if l.starts_with(&p) {
            1
        } else if p.len() >= 2 && subsequence(&l, &p) {
            2
        } else {
            continue;
        };
        scored.push((score, ix, it.label.len()));
    }
    scored.sort_by_key(|(s, ix, _)| (*s, *ix));
    scored.into_iter().take(limit).map(|(_, ix, _)| ix).collect()
}

fn subsequence(hay: &str, needle: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

// ------------------------------------------------------------------ word lists

const HTML_TAGS: &[&str] = &[
    "a", "abbr", "address", "article", "aside", "audio", "b", "blockquote", "body", "br", "button", "canvas", "caption", "code", "col", "colgroup", "data", "datalist", "dd", "details", "dialog", "div", "dl", "dt", "em", "embed", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hr", "html", "i", "iframe", "img", "input", "label", "legend", "li", "link", "main", "mark", "meta", "nav", "noscript", "ol", "optgroup", "option", "output", "p", "picture", "pre", "progress", "script", "section", "select", "slot", "small", "source", "span", "strong", "style", "sub", "summary", "sup", "svg", "table", "tbody", "td", "template", "textarea", "tfoot", "th", "thead", "time", "title", "tr", "u", "ul", "video",
];

const HTML_GLOBAL: &[&str] = &["class", "id", "style", "title", "hidden", "tabindex", "lang", "dir", "role", "aria-label", "aria-hidden", "aria-describedby", "data-", "draggable", "contenteditable", "onclick", "onchange", "oninput", "onsubmit", "onkeydown"];

const JSX_GLOBAL: &[&str] = &[
    "className", "id", "style", "key", "ref", "onClick", "onChange", "onInput", "onSubmit", "onKeyDown", "onKeyUp", "onFocus", "onBlur", "onMouseEnter", "onMouseLeave", "title", "hidden", "tabIndex", "role", "aria-label", "aria-hidden", "data-testid", "dangerouslySetInnerHTML", "children",
];

const HTML_ATTRS: &[(&str, &[&str])] = &[
    ("a", &["href", "target", "rel", "download"]),
    ("img", &["src", "alt", "width", "height", "loading", "srcSet", "sizes"]),
    ("input", &["type", "name", "value", "placeholder", "checked", "disabled", "required", "readOnly", "autoComplete", "autoFocus", "min", "max", "step", "pattern", "maxLength", "defaultValue"]),
    ("button", &["type", "disabled", "name", "value", "form"]),
    ("form", &["action", "method", "encType", "noValidate"]),
    ("label", &["htmlFor", "for"]),
    ("link", &["rel", "href", "type", "as", "crossOrigin"]),
    ("script", &["src", "type", "async", "defer"]),
    ("meta", &["name", "content", "charset", "property"]),
    ("select", &["name", "value", "multiple", "disabled", "required", "defaultValue"]),
    ("option", &["value", "selected", "disabled", "label"]),
    ("textarea", &["name", "value", "rows", "cols", "placeholder", "disabled", "readOnly", "maxLength", "defaultValue"]),
    ("td", &["colSpan", "rowSpan"]),
    ("th", &["colSpan", "rowSpan", "scope"]),
    ("video", &["src", "controls", "autoPlay", "loop", "muted", "poster", "playsInline", "width", "height"]),
    ("audio", &["src", "controls", "autoPlay", "loop", "muted"]),
    ("iframe", &["src", "width", "height", "allow", "title", "loading"]),
    ("source", &["src", "type", "srcSet", "media"]),
    ("svg", &["viewBox", "width", "height", "fill", "stroke", "xmlns"]),
    ("html", &["lang"]),
    ("time", &["dateTime"]),
    ("details", &["open"]),
    ("dialog", &["open"]),
    ("ol", &["start", "reversed", "type"]),
    ("progress", &["value", "max"]),
];

const BOOLEAN_ATTRS: &[&str] = &[
    "disabled", "checked", "hidden", "required", "readonly", "readOnly", "multiple", "selected", "autofocus", "autoFocus", "async", "defer", "controls", "autoplay", "autoPlay", "loop", "muted", "novalidate", "noValidate", "open", "reversed", "playsInline", "horizontal", "multiline", "secureTextEntry", "transparent", "selectable", "pagingEnabled", "accessible", "animating", "inverted",
];

const STRING_PROPS: &[&str] = &[
    "className", "id", "href", "src", "alt", "title", "placeholder", "type", "name", "rel", "target", "role", "htmlFor", "lang", "testID", "nativeID", "accessibilityLabel", "accessibilityHint", "accessibilityRole", "keyboardType", "autoCapitalize", "returnKeyType", "resizeMode", "animationType", "behavior", "barStyle", "ellipsizeMode", "presentationStyle", "pointerEvents", "textContentType", "autoComplete", "method", "action", "loading", "viewBox", "fill", "stroke", "as", "dateTime", "content", "property",
];

const RN_COMPONENTS: &[&str] = &[
    "View", "Text", "Image", "ImageBackground", "ScrollView", "FlatList", "SectionList", "VirtualizedList", "Pressable", "TouchableOpacity", "TouchableHighlight", "TouchableWithoutFeedback", "TextInput", "SafeAreaView", "KeyboardAvoidingView", "ActivityIndicator", "Modal", "Switch", "StatusBar", "RefreshControl", "Button",
];

const RN_COMMON: &[&str] = &["style", "testID", "key", "ref", "onLayout", "accessible", "accessibilityLabel", "accessibilityHint", "accessibilityRole", "pointerEvents", "hitSlop", "nativeID", "children"];

const RN_PROPS: &[(&str, &[&str])] = &[
    ("Text", &["numberOfLines", "ellipsizeMode", "selectable", "onPress", "onLongPress", "allowFontScaling", "adjustsFontSizeToFit"]),
    ("Pressable", &["onPress", "onLongPress", "onPressIn", "onPressOut", "disabled", "android_ripple", "delayLongPress", "unstable_pressDelay"]),
    ("TouchableOpacity", &["onPress", "onLongPress", "activeOpacity", "disabled", "delayLongPress"]),
    ("TouchableHighlight", &["onPress", "onLongPress", "underlayColor", "activeOpacity", "disabled"]),
    ("TouchableWithoutFeedback", &["onPress", "onLongPress", "disabled"]),
    ("Image", &["source", "resizeMode", "onLoad", "onError", "blurRadius", "defaultSource", "fadeDuration"]),
    ("ImageBackground", &["source", "resizeMode", "imageStyle", "blurRadius"]),
    ("TextInput", &["value", "onChangeText", "placeholder", "placeholderTextColor", "secureTextEntry", "keyboardType", "autoCapitalize", "autoCorrect", "autoFocus", "multiline", "maxLength", "onSubmitEditing", "returnKeyType", "editable", "onFocus", "onBlur", "textContentType", "autoComplete", "defaultValue", "selectionColor"]),
    ("ScrollView", &["horizontal", "contentContainerStyle", "showsVerticalScrollIndicator", "showsHorizontalScrollIndicator", "keyboardShouldPersistTaps", "keyboardDismissMode", "refreshControl", "onScroll", "scrollEventThrottle", "pagingEnabled", "bounces", "contentInsetAdjustmentBehavior", "stickyHeaderIndices"]),
    ("FlatList", &["data", "renderItem", "keyExtractor", "ListHeaderComponent", "ListFooterComponent", "ListEmptyComponent", "ItemSeparatorComponent", "numColumns", "horizontal", "onEndReached", "onEndReachedThreshold", "refreshing", "onRefresh", "initialNumToRender", "contentContainerStyle", "inverted", "extraData", "getItemLayout", "showsVerticalScrollIndicator", "contentInsetAdjustmentBehavior"]),
    ("SectionList", &["sections", "renderItem", "renderSectionHeader", "keyExtractor", "stickySectionHeadersEnabled", "ListHeaderComponent", "ListFooterComponent", "ListEmptyComponent", "onEndReached", "refreshing", "onRefresh", "contentContainerStyle"]),
    ("Modal", &["visible", "animationType", "transparent", "onRequestClose", "presentationStyle", "onShow", "statusBarTranslucent"]),
    ("Switch", &["value", "onValueChange", "disabled", "trackColor", "thumbColor", "ios_backgroundColor"]),
    ("ActivityIndicator", &["size", "color", "animating", "hidesWhenStopped"]),
    ("KeyboardAvoidingView", &["behavior", "keyboardVerticalOffset", "enabled", "contentContainerStyle"]),
    ("SafeAreaView", &["edges"]),
    ("StatusBar", &["barStyle", "backgroundColor", "hidden", "translucent", "animated"]),
    ("RefreshControl", &["refreshing", "onRefresh", "tintColor", "colors", "title"]),
    ("Button", &["title", "onPress", "color", "disabled"]),
];

const TS_TYPES: &[&str] = &[
    "string", "number", "boolean", "bigint", "symbol", "object", "unknown", "never", "void", "any", "null", "undefined", "Array", "ReadonlyArray", "Record", "Partial", "Required", "Readonly", "Pick", "Omit", "Exclude", "Extract", "NonNullable", "ReturnType", "Parameters", "Awaited", "Promise", "Map", "Set", "Date", "Error", "RegExp", "Function",
];

const TS_KEYWORDS: &[&str] = &[
    "const", "let", "function", "return", "import", "export", "default", "interface", "type", "async", "await", "class", "extends", "implements", "new", "this", "if", "else", "for", "while", "switch", "case", "break", "continue", "try", "catch", "finally", "throw", "typeof", "instanceof", "keyof", "readonly", "private", "public", "protected", "static", "enum", "true", "false", "null", "undefined",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn closes(text: &str, fl: Flavor) -> Option<String> {
        let b = Buffer::new(text);
        close_tag_after_gt(&b, b.len(), fl)
    }

    #[test]
    fn closes_tags_on_gt() {
        assert_eq!(closes("<div>", Flavor::Html).as_deref(), Some("</div>"));
        assert_eq!(closes("<div class=\"a > b\">", Flavor::Html).as_deref(), Some("</div>"));
        assert_eq!(closes("<br>", Flavor::Html), None, "void element");
        assert_eq!(closes("<View style={{ flex: 1 }}>", Flavor::Jsx).as_deref(), Some("</View>"));
        assert_eq!(closes("<Foo onPress={() => go()}>", Flavor::Jsx).as_deref(), Some("</Foo>"));
        assert_eq!(closes("<>", Flavor::Jsx).as_deref(), Some("</>"));
        assert_eq!(closes("<Image />", Flavor::Jsx), None, "self-closing");
        assert_eq!(closes("</div>", Flavor::Html), None);
        assert_eq!(closes("const x: Array<string>", Flavor::Jsx), None, "generic");
        assert_eq!(closes("if (a <b) >", Flavor::Jsx), None);
        assert_eq!(closes("const f = <T,>", Flavor::Jsx), None);
        assert_eq!(closes("x => y >", Flavor::Jsx), None);
        assert_eq!(closes("<div>", Flavor::Script), None);
    }

    #[test]
    fn slash_closes_the_innermost_open_tag() {
        let b = Buffer::new("<ul>\n  <li>one</li>\n  <li><br>two</");
        assert_eq!(close_tag_after_slash(&b, b.len(), Flavor::Html).as_deref(), Some("li>"));
        let b = Buffer::new("<View>\n  <Text>hi</Text>\n  <Image source={x} />\n</");
        assert_eq!(close_tag_after_slash(&b, b.len(), Flavor::Jsx).as_deref(), Some("View>"));
        let b = Buffer::new("a </");
        assert_eq!(close_tag_after_slash(&b, b.len(), Flavor::Jsx), None);
    }

    #[test]
    fn finds_the_context() {
        let ctx = |s: &str, fl| {
            let b = Buffer::new(s);
            context(&b, b.len(), fl)
        };
        assert_eq!(ctx("<di", Flavor::Html), Some(Ctx::TagName { prefix: "di".into() }));
        assert_eq!(ctx("<View sty", Flavor::Jsx), Some(Ctx::Attr { tag: "View".into(), prefix: "sty".into() }));
        assert_eq!(ctx("<View style={{ fl", Flavor::Jsx), None, "inside an expression");
        assert_eq!(ctx("import x from \"./comp", Flavor::Jsx), Some(Ctx::Path { typed: "./comp".into() }));
        assert_eq!(ctx("const a: Us", Flavor::Script), Some(Ctx::Word { prefix: "Us".into(), type_pos: true }));
        assert_eq!(ctx("const a = \"us", Flavor::Script), None, "inside a string");
        assert_eq!(ctx("// us", Flavor::Script), None, "inside a comment");
    }

    #[test]
    fn suggests_native_components_and_file_types() {
        let b = Buffer::new("import { View, Text } from \"react-native\";\ntype Account = { id: string };\nconst Card = () => <Vi");
        let items = candidates(&b, &Ctx::TagName { prefix: "Vi".into() }, Flavor::Jsx, None, None);
        let shown: Vec<&str> = filter(&items, "Vi", 10).into_iter().map(|i| items[i].label.as_str()).collect();
        assert_eq!(shown.first(), Some(&"View"));
        assert!(!items.iter().any(|i| i.label == "div"), "no HTML tags in a React Native file");
        let items = candidates(&b, &Ctx::Word { prefix: "Ac".into(), type_pos: true }, Flavor::Jsx, None, None);
        let first = &items[filter(&items, "Ac", 10)[0]];
        assert_eq!((first.label.as_str(), first.kind), ("Account", Kind::Type));
    }

    #[test]
    fn lists_import_paths() {
        let dir = std::env::temp_dir().join(format!("kite-complete-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src/components")).unwrap();
        std::fs::write(dir.join("src/components/Card.tsx"), "").unwrap();
        std::fs::write(dir.join("src/app.tsx"), "").unwrap();
        let file = dir.join("src/app.tsx");
        let items = paths("./", Some(&file), Some(&dir));
        assert!(items.iter().any(|i| i.label == "components" && i.kind == Kind::Folder));
        let items = paths("./components/Ca", Some(&file), Some(&dir));
        assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["Card"]);
        let items = paths("@/components/", Some(&file), Some(&dir));
        assert_eq!(items.len(), 1, "@/ maps to src/");
        std::fs::remove_dir_all(&dir).ok();
    }
}
