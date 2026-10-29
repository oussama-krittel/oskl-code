//! What the hover card says about a name, without a language server: its declaration as
//! written (up to where the body starts) and the doc comment above it.

use crate::highlight::{self, Lang, LineSpans};
use std::path::{Path, PathBuf};

pub struct Info {
    /// The declaration, dedented, possibly over several lines.
    pub sig: String,
    pub syntax: Option<LineSpans>,
    pub doc: Doc,
    pub path: PathBuf,
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Default, PartialEq)]
pub struct Doc {
    /// The description, paragraphs separated by blank lines.
    pub text: String,
    pub params: Vec<(String, String)>,
    pub returns: Option<String>,
    /// Other tags (`@deprecated`, `@example`…) as (tag, text).
    pub tags: Vec<(String, String)>,
}

impl Doc {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.params.is_empty() && self.returns.is_none() && self.tags.is_empty()
    }
}

const MAX_LINES: usize = 20;
const MAX_CHARS: usize = 2000;

/// The card for the declaration at (`row`, `col`) of `text`, the contents of `path`.
pub fn info(text: &str, path: &Path, row: usize, col: usize) -> Option<Info> {
    // Packages' compiled code says nothing useful; their type declarations do.
    if path.components().any(|c| c.as_os_str() == "node_modules") && !path.to_string_lossy().ends_with(".d.ts") {
        return None;
    }
    let lang = highlight::detect(path);
    let lines: Vec<&str> = text.lines().collect();
    if row >= lines.len() {
        return None;
    }
    let (sig, end_row, body) = signature(&lines, row, lang)?;
    let mut doc = doc_above(&lines, row);
    if lang == Some(Lang::Python) && doc.is_empty() {
        doc = docstring(&lines, end_row + 1);
    }
    let syntax = lang.and_then(|l| {
        // Give the parser a whole declaration so the colours come out right.
        let whole = if body { format!("{sig} {{}}") } else { sig.clone() };
        highlight::highlight(l, &whole)
    });
    Some(Info { sig, syntax, doc: parse_doc(&doc), path: path.to_path_buf(), row, col })
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The declaration starting on line `row`: (its text, the line it ends on, whether a body was cut off).
fn signature(lines: &[&str], row: usize, lang: Option<Lang>) -> Option<(String, usize, bool)> {
    let python = lang == Some(Lang::Python);
    let first = lines[row];
    let indent = first.len() - first.trim_start().len();
    let mut head = first.trim_start();
    for p in ["export ", "default ", "declare "] {
        head = head.strip_prefix(p).unwrap_or(head).trim_start();
    }
    let word = |s: &str| s.split(|c: char| !is_ident(c)).next().unwrap_or("").to_string();
    let mut kw = word(head);
    let mut rest = head;
    while matches!(kw.as_str(), "pub" | "async" | "abstract" | "const" | "static" | "unsafe") && rest.len() > kw.len() {
        rest = rest[kw.len()..].trim_start();
        if rest.starts_with('(') {
            // `pub(crate)`
            rest = rest.find(')').map_or(rest, |i| rest[i + 1..].trim_start());
        }
        let next = word(rest);
        if next.is_empty() || kw == "const" && !matches!(next.as_str(), "enum" | "fn") {
            break;
        }
        kw = next;
    }
    // Type-like declarations show their body; Go's `type X struct {` too.
    let shows_body = matches!(kw.as_str(), "type" | "interface" | "enum" | "struct" | "union");
    let colon_ends = python && matches!(kw.as_str(), "def" | "class" | "async");

    let mut src: Vec<char> = Vec::new();
    src.extend(head.chars());
    for l in &lines[row + 1..lines.len().min(row + MAX_LINES * 2)] {
        src.push('\n');
        src.extend(l.chars());
    }

    let mut out = String::new();
    let (mut paren, mut angle, mut brace) = (0i32, 0i32, 0i32);
    let mut body = false;
    let mut line = row;
    let mut i = 0;
    let last_nonspace = |s: &str| s.trim_end().chars().last();
    while i < src.len() {
        let c = src[i];
        let next = src.get(i + 1).copied();
        let top = paren == 0 && angle == 0 && brace == 0;
        if out.len() > MAX_CHARS || line - row >= MAX_LINES {
            out.push_str(" …");
            break;
        }
        match c {
            // Rust lifetimes (`'a`) aren't strings; its char literals are.
            '\'' if lang == Some(Lang::Rust) && src.get(i + 2) != Some(&'\'') => {}
            '"' | '\'' | '`' => {
                let q = c;
                out.push(c);
                i += 1;
                while i < src.len() && src[i] != q && src[i] != '\n' {
                    if src[i] == '\\' && i + 1 < src.len() {
                        out.push(src[i]);
                        i += 1;
                    }
                    out.push(src[i]);
                    i += 1;
                }
                if i < src.len() && src[i] == q {
                    out.push(q);
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('/') => {
                while i < src.len() && src[i] != '\n' {
                    i += 1;
                }
                out.truncate(out.trim_end().len());
                continue;
            }
            '#' if python => {
                while i < src.len() && src[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('*') => {
                i += 2;
                while i + 1 < src.len() && !(src[i] == '*' && src[i + 1] == '/') {
                    if src[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
                continue;
            }
            '(' | '[' => {
                if c == '[' && top && !shows_body && last_nonspace(&out) == Some('=') {
                    out.push_str("[ … ]");
                    break;
                }
                paren += 1;
            }
            ')' | ']' => paren = (paren - 1).max(0),
            '<' if out.chars().last().is_some_and(is_ident) || matches!(last_nonspace(&out), Some('=' | '(' | ',' | ':' | '|' | '&')) && !out.ends_with("= ") => angle += 1,
            '>' if angle > 0 && !matches!(out.chars().last(), Some('=' | '-')) => angle -= 1,
            '{' => {
                let after_arrow = out.trim_end().ends_with("=>");
                if after_arrow {
                    out.truncate(out.trim_end().len());
                    out.push_str(" …");
                    body = false;
                    break;
                }
                if top && !shows_body {
                    match last_nonspace(&out) {
                        Some('=') => {
                            out.push_str("{ … }");
                            break;
                        }
                        Some(':' | '|' | '&' | ',' | '<' | '(') => brace += 1,
                        _ => {
                            body = true;
                            break;
                        }
                    }
                } else {
                    brace += 1;
                }
            }
            '}' => brace = (brace - 1).max(0),
            ';' if top => break,
            ':' if top && colon_ends => {
                body = true;
                break;
            }
            '=' if top && next == Some('>') && !shows_body => {
                // An arrow function: the parameters and return type are the signature.
                let after: String = src[i + 2..].iter().take_while(|c| **c != '\n').collect();
                let after = after.trim().trim_end_matches(';');
                if after.is_empty() || after.starts_with('{') {
                    body = true;
                    break;
                }
                out.push_str("=> ");
                out.push_str(&after.chars().take(120).collect::<String>());
                break;
            }
            '\n' => {
                if top {
                    let following = src[i + 1..].iter().collect::<String>();
                    let following = following.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim_start().to_string();
                    let continues = ["->", "where", ":", "|", "&", "extends", "implements", ")", "?", "=>", "{"].iter().any(|p| following.starts_with(p))
                        && !(following.starts_with('{') && !shows_body && !out.trim_end().ends_with(')'))
                        || out.lines().last().is_some_and(|l| l.trim() == "where");
                    if !continues {
                        break;
                    }
                }
                line += 1;
            }
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    let out = dedent(out.trim_end().trim_end_matches(['=', ':']).trim_end(), indent);
    (!out.is_empty()).then_some((out, line, body))
}

/// Continuation lines lose the declaration's own indentation.
fn dedent(s: &str, indent: usize) -> String {
    s.lines()
        // lines that only held a comment
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                return l.to_string();
            }
            let n = l.len() - l.trim_start().len();
            l[n.min(indent)..].to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The comment lines right above `row` (past decorators and attributes), markers removed.
fn doc_above(lines: &[&str], row: usize) -> Vec<String> {
    let mut r = row;
    while r > 0 && {
        let t = lines[r - 1].trim_start();
        t.starts_with('@') || t.starts_with("#[")
    } {
        r -= 1;
    }
    if r == 0 {
        return Vec::new();
    }
    let prev = lines[r - 1].trim();
    if prev.ends_with("*/") {
        let mut start = r - 1;
        while !lines[start].contains("/*") {
            if start == 0 || r - start > 200 {
                return Vec::new();
            }
            start -= 1;
        }
        return lines[start..r]
            .iter()
            .map(|l| {
                let t = l.trim();
                let t = t.strip_prefix("/**").or_else(|| t.strip_prefix("/*")).unwrap_or(t);
                let t = t.strip_suffix("*/").unwrap_or(t).trim_end();
                let t = t.trim_start();
                let t = t.strip_prefix('*').map(|s| s.strip_prefix(' ').unwrap_or(s)).unwrap_or(t);
                t.to_string()
            })
            .collect();
    }
    let mut out = Vec::new();
    let mut k = r;
    while k > 0 {
        let t = lines[k - 1].trim_start();
        let body = t.strip_prefix("///").or_else(|| t.strip_prefix("//")).or_else(|| t.strip_prefix('#').filter(|_| !t.starts_with("#!")));
        match body {
            Some(b) if !t.starts_with("//!") => out.push(b.strip_prefix(' ').unwrap_or(b).to_string()),
            _ => break,
        }
        k -= 1;
    }
    out.reverse();
    // A commented-out line of code isn't documentation.
    if out.iter().all(|l| l.trim().is_empty() || l.trim_end().ends_with(';')) {
        return Vec::new();
    }
    out
}

/// Python: the string right under the `def`.
fn docstring(lines: &[&str], row: usize) -> Vec<String> {
    let Some(first) = lines.get(row).map(|l| l.trim()) else { return Vec::new() };
    let Some(q) = ["\"\"\"", "'''"].into_iter().find(|q| first.starts_with(q)) else { return Vec::new() };
    let rest = &first[3..];
    if let Some(end) = rest.find(q) {
        return vec![rest[..end].to_string()];
    }
    let mut out = vec![rest.to_string()];
    for l in lines.iter().skip(row + 1).take(100) {
        match l.find(q) {
            Some(end) => {
                out.push(l[..end].trim().to_string());
                break;
            }
            None => out.push(l.trim().to_string()),
        }
    }
    out
}

/// JSDoc-style `@param` / `@returns` tags out of the comment; the rest is the description.
fn parse_doc(lines: &[String]) -> Doc {
    enum At {
        Text,
        Param,
        Returns,
        Tag,
    }
    let mut doc = Doc::default();
    let mut text: Vec<String> = Vec::new();
    let mut at = At::Text;
    for l in lines {
        let t = l.trim();
        if let Some(tag) = t.strip_prefix('@') {
            let (name, body) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
            let mut body = body.trim();
            // `{type}` in front of the name
            if body.starts_with('{') {
                body = body.find('}').map_or(body, |i| body[i + 1..].trim_start());
            }
            match name {
                "param" | "arg" | "argument" | "prop" | "property" => {
                    let (pname, desc) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
                    let pname = pname.trim_matches(['[', ']']).split('=').next().unwrap_or("").to_string();
                    let desc = desc.trim().trim_start_matches(['-', ':']).trim().to_string();
                    doc.params.push((pname, desc));
                    at = At::Param;
                }
                "returns" | "return" => {
                    doc.returns = Some(body.to_string());
                    at = At::Returns;
                }
                _ => {
                    doc.tags.push((name.to_string(), body.to_string()));
                    at = At::Tag;
                }
            }
            continue;
        }
        let join = |s: &mut String| {
            if !t.is_empty() {
                if !s.is_empty() {
                    s.push(if matches!(at, At::Tag) { '\n' } else { ' ' });
                }
                s.push_str(if matches!(at, At::Tag) { l.trim_end() } else { t });
            }
        };
        match at {
            At::Text => text.push(t.to_string()),
            At::Param => join(&mut doc.params.last_mut().unwrap().1),
            At::Returns => join(doc.returns.as_mut().unwrap()),
            At::Tag => join(&mut doc.tags.last_mut().unwrap().1),
        }
    }
    // Lines joined into paragraphs; blank lines and list items start new ones.
    let mut paras: Vec<String> = Vec::new();
    let mut cur = String::new();
    for t in text.iter().map(|t| t.as_str()).chain(std::iter::once("")) {
        let item = t.starts_with("- ") || t.starts_with("* ") || t.starts_with('#');
        if (t.is_empty() || item) && !cur.is_empty() {
            paras.push(std::mem::take(&mut cur));
        }
        if !t.is_empty() {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(t);
        }
    }
    doc.text = paras.join("\n");
    doc
}

/// Inline doc markup: the text without backticks / `{@link}`, and the ranges that were code.
pub fn inline_code(s: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let s = s.replace("{@link ", "`").replace("{@linkcode ", "`");
    let mut out = String::new();
    let mut code = Vec::new();
    let mut start: Option<usize> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => match start.take() {
                Some(a) => code.push(a..out.len()),
                None => start = Some(out.len()),
            },
            // the end of a `{@link X}` turned into a backtick above
            '}' if start.is_some() && s.contains('`') => {
                if let Some(a) = start.take() {
                    code.push(a..out.len());
                }
            }
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
            }
            _ => out.push(c),
        }
    }
    (out, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(src: &str, row: usize, ext: &str) -> (String, Doc) {
        let i = info(src, Path::new(&format!("a.{ext}")), row, 0).expect("an info");
        (i.sig, i.doc)
    }

    #[test]
    fn typescript_functions_with_their_docs() {
        let src = "/**\n * Formats an amount as money.\n * Uses the `locale` given.\n *\n * @param amount - in cents\n * @param {string} [locale=en] the locale\n * @returns the label\n * @deprecated use format()\n */\nexport async function formatMoney(amount: number, locale = 'en'): Promise<string> {\n  return '';\n}\n";
        let (s, d) = sig(src, 9, "ts");
        assert_eq!(s, "async function formatMoney(amount: number, locale = 'en'): Promise<string>");
        assert_eq!(d.text, "Formats an amount as money. Uses the `locale` given.");
        assert_eq!(d.params, vec![("amount".into(), "in cents".into()), ("locale".into(), "the locale".into())]);
        assert_eq!(d.returns.as_deref(), Some("the label"));
        assert_eq!(d.tags, vec![("deprecated".into(), "use format()".into())]);
    }

    #[test]
    fn arrows_multiline_params_and_values() {
        let src = "// Adds two.\nexport const add = (a: number, b: number): number => a + b;\nconst Card = ({\n  title,\n  onPress,\n}: Props) => {\n  return null;\n};\nconst cb = useCallback((x: string) => {\n}, []);\nconst config = {\n  a: 1,\n};\nlet n = 5\nconst [count, setCount] = useState(0);\n";
        assert_eq!(sig(src, 1, "ts"), ("const add = (a: number, b: number): number => a + b".into(), Doc { text: "Adds two.".into(), ..Doc::default() }));
        assert_eq!(sig(src, 2, "tsx").0, "const Card = ({\n  title,\n  onPress,\n}: Props)");
        assert_eq!(sig(src, 8, "ts").0, "const cb = useCallback((x: string) => …");
        assert_eq!(sig(src, 10, "ts").0, "const config = { … }");
        assert_eq!(sig(src, 13, "ts").0, "let n = 5");
        assert_eq!(sig(src, 14, "ts").0, "const [count, setCount] = useState(0)");
    }

    #[test]
    fn types_show_their_body_classes_dont() {
        let src = "export interface Props extends Base {\n  // the heading\n  title: string; // shown\n  onPress?: () => void;\n}\nexport type Handler<T> = (value: T) => void;\nexport class Store<T> extends Base {\n  x = 1;\n}\nfunction f(): { a: number } {\n}\n";
        assert_eq!(sig(src, 0, "ts").0, "interface Props extends Base {\n  title: string;\n  onPress?: () => void;\n}");
        assert_eq!(sig(src, 5, "ts").0, "type Handler<T> = (value: T) => void");
        assert_eq!(sig(src, 6, "ts").0, "class Store<T> extends Base");
        assert_eq!(sig(src, 9, "ts").0, "function f(): { a: number }");
    }

    #[test]
    fn rust_python_and_go() {
        let rs = "/// Reads the file.\n///\n/// Fails when missing.\n#[inline]\npub(crate) fn read<P: AsRef<Path>>(path: P)\n    -> io::Result<String>\nwhere\n    P: Clone,\n{\n}\n";
        let (s, d) = sig(rs, 4, "rs");
        assert_eq!(s, "pub(crate) fn read<P: AsRef<Path>>(path: P)\n    -> io::Result<String>\nwhere\n    P: Clone,");
        assert_eq!(d.text, "Reads the file.\nFails when missing.");
        let py = "@cache\ndef total(items: list[int], tax: float = 0.2) -> float:\n    \"\"\"Sum with tax.\n\n    Rounds down.\n    \"\"\"\n    return 0\n";
        let (s, d) = sig(py, 1, "py");
        assert_eq!(s, "def total(items: list[int], tax: float = 0.2) -> float");
        assert_eq!(d.text, "Sum with tax.\nRounds down.");
        let go = "// Open opens it.\nfunc (r *Repo) Open(name string) (*File, error) {\n}\n";
        assert_eq!(sig(go, 1, "go").0, "func (r *Repo) Open(name string) (*File, error)");
    }

    #[test]
    fn packages_only_through_their_types() {
        assert!(info("export function x() {}", Path::new("/p/node_modules/x/index.js"), 0, 0).is_none());
        assert!(info("export declare function x(): void;", Path::new("/p/node_modules/x/index.d.ts"), 0, 0).is_some());
        assert_eq!(sig("// let a = 1;\nconst b = 2;\n", 1, "ts").1, Doc::default(), "commented-out code isn't a doc");
    }

    #[test]
    fn inline_code_spans() {
        assert_eq!(inline_code("Use `foo` or {@link Bar}."), ("Use foo or Bar.".into(), vec![4..7, 11..14]));
    }
}
