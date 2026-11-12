//! ⌘-click on a member in JavaScript / TypeScript (`this.service.method`, `dto.field`,
//! `this.repo.find`): where it's declared, by following the declared types, without a language
//! server. Reads only the few files the chain goes through, at the click: nothing to load first,
//! nothing kept in memory.
//!
//! `this` is the enclosing class (`super` its parent); a name is typed by its annotation
//! (`dto: CreateDto`, a constructor's `private readonly service: Service`), `new Type(…)`, or is
//! a class itself (static members). Each member's type comes from its own annotation (a
//! property's, a method's return type, `Promise<T>` and `T[]` seen through). Members are looked
//! for in the class body (at its top level), then up its `extends`.

use crate::definition::{Target, imports, resolve_module};
use regex::Regex;
use std::ops::Range;
use std::path::{Path, PathBuf};

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The names before the clicked one, read back from the text before it: `this.a.` → [this, a].
/// Optional chaining and non-null (`a?.b`, `a!.b`) count; calls and indexing stop it.
pub fn receiver_chain(before: &str) -> Option<Vec<String>> {
    let mut s = before.trim_end();
    s = s.strip_suffix('.')?;
    let mut out = Vec::new();
    loop {
        s = s.strip_suffix('?').or_else(|| s.strip_suffix('!')).unwrap_or(s).trim_end();
        let name: String = s.chars().rev().take_while(|c| is_ident(*c)).collect::<Vec<_>>().into_iter().rev().collect();
        if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }
        s = s[..s.len() - name.len()].trim_end();
        out.push(name);
        match s.strip_suffix('.') {
            // `a .b` across lines, but not `…).b` (a call's result) or a number.
            Some(rest) if !rest.ends_with('.') => s = rest,
            _ => break,
        }
        if out.len() > 8 {
            return None;
        }
    }
    out.reverse();
    Some(out)
}

/// A class, interface or object type: the file it's in and where its body is.
struct Ty {
    file: PathBuf,
    text: String,
    /// Where its declaration starts (the `class` keyword), and its `{ … }` body.
    decl: usize,
    body: Range<usize>,
}

/// Where `name` (a member of what `chain` names) is declared. `at`: the byte of the click in `text`.
pub fn member_target(text: &str, file: &Path, root: Option<&Path>, chain: &[String], name: &str, at: usize) -> Option<Target> {
    let mut ty = first_type(text, file, root, &chain[0], at)?;
    for m in &chain[1..] {
        ty = member_type(&ty, m, root, 0)?;
    }
    member_decl(&ty, name, root, 0)
}

/// The type of the chain's first name, as seen from the click.
fn first_type(text: &str, file: &Path, root: Option<&Path>, first: &str, at: usize) -> Option<Ty> {
    match first {
        "this" => enclosing_class(text, file, at),
        "super" => {
            let me = enclosing_class(text, file, at)?;
            parent(&me, root)
        }
        name => {
            let n = regex::escape(name);
            // An annotation (a parameter, a local, a property) nearest before the click, else anywhere.
            let ann = re(&format!(r"(?:^|[^\w$.]){n}\s*[?!]?\s*:\s*([^=;,)\n{{]+)"));
            let made = re(&format!(r"\b{n}\s*=\s*(?:await\s+)?new\s+([A-Za-z_$][\w$]*)"));
            let pick = |r: &Regex| {
                let all: Vec<_> = r.captures_iter(text).filter_map(|c| c.get(1)).collect();
                all.iter().rev().find(|m| m.start() < at).or(all.first()).map(|m| m.as_str().to_string())
            };
            if let Some(t) = pick(&ann).or_else(|| pick(&made)) {
                return named_type(text, file, root, &t, 0);
            }
            // A class itself (its static members).
            named_type(text, file, root, name, 0)
        }
    }
}

/// The class whose body holds `at`.
fn enclosing_class(text: &str, file: &Path, at: usize) -> Option<Ty> {
    let decl = re(r"\bclass\s+[A-Za-z_$][\w$]*");
    decl.find_iter(text).filter(|m| m.start() < at).filter_map(|m| body_after(text, m.start()).map(|b| (m.start(), b))).filter(|(_, b)| b.start <= at && at <= b.end).last().map(|(d, body)| Ty {
        file: file.to_path_buf(),
        text: text.to_string(),
        decl: d,
        body,
    })
}

/// The core name of a type annotation: `Promise<Foo[]>` → Foo, `Readonly<Bar> | null` → Bar.
fn core_type(t: &str) -> Option<String> {
    const WRAPPERS: &[&str] = &["Promise", "Array", "ReadonlyArray", "Readonly", "Partial", "Required", "Observable", "Awaited", "NonNullable", "Set"];
    let mut s = t.trim();
    loop {
        s = s.trim_start_matches(|c: char| c == '(' || c.is_whitespace());
        let name: String = s.chars().take_while(|c| is_ident(*c) || *c == '.').collect();
        if name.is_empty() {
            return None;
        }
        let rest = s[name.len()..].trim_start();
        let last = name.rsplit('.').next().unwrap_or(&name).to_string();
        if WRAPPERS.contains(&last.as_str()) && rest.starts_with('<') {
            s = &rest[1..];
            continue;
        }
        if matches!(last.as_str(), "string" | "number" | "boolean" | "any" | "unknown" | "void" | "never" | "object" | "null" | "undefined") {
            return None;
        }
        return Some(name);
    }
}

/// The class / interface / type a name refers to in `text` (declared there, or imported).
fn named_type(text: &str, file: &Path, root: Option<&Path>, annotation: &str, depth: u32) -> Option<Ty> {
    if depth > 6 {
        return None;
    }
    let name = core_type(annotation)?;
    // `ns.Type`: the namespace's module.
    if let Some((ns, inner)) = name.split_once('.') {
        let b = imports(text).into_iter().find(|b| b.local == ns && b.imported == "*")?;
        let module = resolve_module(&b.spec, file, root)?;
        let t = std::fs::read_to_string(&module).ok()?;
        return named_type(&t, &module, root, inner, depth + 1);
    }
    if let Some(ty) = declared_type(text, file, root, &name, depth) {
        return Some(ty);
    }
    let b = imports(text).into_iter().find(|b| b.local == name)?;
    let module = resolve_module(&b.spec, file, root)?;
    let target = crate::definition::export_in(&module, &b.imported, root, 0)?;
    let t = std::fs::read_to_string(&target.path).ok()?;
    let imported = if b.imported == "default" { name.clone() } else { b.imported.clone() };
    declared_type(&t, &target.path, root, &imported, depth).or_else(|| {
        // `export default class …` without its name here.
        let at: usize = t.split('\n').take(target.row).map(|l| l.len() + 1).sum();
        body_after(&t, at).map(|body| Ty { file: target.path.clone(), text: t.clone(), decl: at, body })
    })
}

/// A class, interface or object type declared in `text` as `name`.
fn declared_type(text: &str, file: &Path, root: Option<&Path>, name: &str, depth: u32) -> Option<Ty> {
    let n = regex::escape(name);
    let decl = re(&format!(r"\b(?:class|interface)\s+{n}\b|\btype\s+{n}\s*(?:<[^=]*>)?\s*=\s*"));
    for m in decl.find_iter(text) {
        if m.as_str().starts_with("type") {
            let rest = text[m.end()..].trim_start();
            if rest.starts_with('{') {
                let open = m.end() + (text[m.end()..].len() - rest.len());
                return Some(Ty { file: file.to_path_buf(), text: text.to_string(), decl: m.start(), body: open..close_of(text, open)? });
            }
            // `type A = B` (or `B & {…}`): A is B.
            let other: String = rest.chars().take_while(|c| *c != ';' && *c != '\n').collect();
            return named_type(text, file, root, &other, depth + 1);
        }
        if let Some(body) = body_after(text, m.start()) {
            return Some(Ty { file: file.to_path_buf(), text: text.to_string(), decl: m.start(), body });
        }
    }
    None
}

/// The `{ … }` body of the declaration starting at `from` (past its generics and heritage).
fn body_after(text: &str, from: usize) -> Option<Range<usize>> {
    let open = from + text[from..].find('{')?;
    Some(open..close_of(text, open)?)
}

/// The byte just past the brace matching the one at `open` (strings and comments skipped).
fn close_of(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut it = text[open..].char_indices().peekable();
    let mut quote: Option<char> = None;
    while let Some((i, c)) = it.next() {
        if let Some(q) = quote {
            if c == '\\' {
                it.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '/' if it.peek().is_some_and(|(_, n)| *n == '/') => {
                while it.peek().is_some_and(|(_, n)| *n != '\n') {
                    it.next();
                }
            }
            '/' if it.peek().is_some_and(|(_, n)| *n == '*') => {
                it.next();
                let mut star = false;
                for (_, n) in it.by_ref() {
                    if star && n == '/' {
                        break;
                    }
                    star = n == '*';
                }
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// For each byte of the body, whether it's at the body's own level (not inside a method).
fn top_level(text: &str, body: &Range<usize>) -> Vec<bool> {
    let s = &text[body.clone()];
    let mut out = vec![false; s.len()];
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if let Some(q) = quote {
            if c == '\\' {
                it.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '/' if it.peek().is_some_and(|(_, n)| *n == '/') => {
                while it.peek().is_some_and(|(_, n)| *n != '\n') {
                    it.next();
                }
            }
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        if depth == 1 {
            out[i] = true;
        }
    }
    out
}

/// Where member `name` is declared at the type's top level: (byte of the name, the text after it).
fn find_member(ty: &Ty, name: &str) -> Option<usize> {
    let n = regex::escape(name);
    let body = &ty.text[ty.body.clone()];
    let top = top_level(&ty.text, &ty.body);
    let mods = r"(?:(?:public|private|protected|static|readonly|async|abstract|override|declare|get|set|accessor)\s+)*";
    let patterns = [
        // A property, method, accessor or arrow member at the start of its line.
        format!(r"(?m)^[ \t]*(?:@[\w$.]+(?:\([^)]*\))?\s+)*{mods}#?({n})\s*[?!]?\s*[:(=<;]"),
        // A constructor's parameter property: `private readonly name: T`.
        format!(r"(?:public|private|protected|readonly)\s+(?:readonly\s+)?({n})\s*[?!]?\s*:"),
        // A one-line object type: `{ a: string; name: T }`.
        format!(r"[{{;,]\s*({n})\s*[?!]?\s*[:(]"),
    ];
    patterns
        .iter()
        .filter_map(|p| re(p).captures_iter(body).filter_map(|c| c.get(1)).find(|m| top.get(m.start()).copied().unwrap_or(false)).map(|m| m.start()))
        .min()
        .map(|i| ty.body.start + i)
}

/// The type it `extends` (a class's parent, an interface's first base).
fn parent(ty: &Ty, root: Option<&Path>) -> Option<Ty> {
    let head = &ty.text[ty.decl..ty.body.start];
    let c = re(r"\bextends\s+([A-Za-z_$][\w$.]*)").captures(head)?;
    named_type(&ty.text, &ty.file, root, &c[1], 1)
}

/// Where `name` is declared in the type (or the types it extends).
fn member_decl(ty: &Ty, name: &str, root: Option<&Path>, depth: u32) -> Option<Target> {
    if let Some(at) = find_member(ty, name) {
        let row = ty.text[..at].matches('\n').count();
        let col = ty.text[..at].rsplit('\n').next().unwrap_or("").chars().count();
        return Some(Target { path: ty.file.clone(), row, col });
    }
    if depth > 6 {
        return None;
    }
    member_decl(&parent(ty, root)?, name, root, depth + 1)
}

/// The type of member `name`: its annotation, or what the method returns.
fn member_type(ty: &Ty, name: &str, root: Option<&Path>, depth: u32) -> Option<Ty> {
    let Some(at) = find_member(ty, name) else {
        return if depth > 6 { None } else { member_type(&parent(ty, root)?, name, root, depth + 1) };
    };
    let rest = &ty.text[at + name.len()..ty.body.end];
    let rest = rest.trim_start_matches(['?', '!']).trim_start();
    let annotation = if let Some(r) = rest.strip_prefix(':') {
        r.to_string()
    } else {
        // A method: past its generics and parameters, the return type.
        let r = rest.strip_prefix('<').map(|r| r.find('>').map_or(r, |i| &r[i + 1..])).unwrap_or(rest).trim_start();
        let open = r.strip_prefix('(')?;
        let mut depth = 1;
        let mut end = None;
        for (i, c) in open.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        open[end? + 1..].trim_start().strip_prefix(':')?.to_string()
    };
    let annotation: String = annotation.chars().take_while(|c| !matches!(c, '=' | ';' | ',' | '{' | '\n')).collect();
    named_type(&ty.text, &ty.file, root, &annotation, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_are_read_back() {
        assert_eq!(receiver_chain("return this.service."), Some(vec!["this".into(), "service".into()]));
        assert_eq!(receiver_chain("  dto?."), Some(vec!["dto".into()]));
        assert_eq!(receiver_chain("x = a.b!.c."), Some(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(receiver_chain("this.repo\n      ."), Some(vec!["this".into(), "repo".into()]));
        assert_eq!(receiver_chain("f()."), None, "a call's result isn't followed");
        assert_eq!(receiver_chain("x "), None);
        assert_eq!(core_type(" Promise<Foo[]> "), Some("Foo".into()));
        assert_eq!(core_type("Readonly<Bar> | null"), Some("Bar".into()));
        assert_eq!(core_type("string"), None);
    }
}
