//! TypeScript errors made readable: long types are shortened in the one-line label at the end of
//! the code line, and laid out as indented steps with formatted types in the hover card.
//!
//! tsc explains an error as a chain: the first line says what's wrong, each indented line below
//! says why (two spaces per level). The innermost line is usually the one to act on.

/// A piece of a message: prose, or a type / name that was in quotes.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Code(String),
}

/// One line of the chain: how deep it is and its pieces.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub depth: usize,
    pub parts: Vec<Part>,
}

/// `[TS2322]` at the end of the first line (added when tsc output is read), if any.
pub fn code(message: &str) -> Option<String> {
    let first = message.lines().next()?;
    let open = first.rfind(" [")?;
    let tag = first[open + 2..].strip_suffix(']')?;
    (tag.starts_with("TS") || tag.contains('/') || tag.contains('-')).then(|| tag.to_string())
}

fn without_code(line: &str) -> &str {
    match line.rfind(" [") {
        Some(i) if line.ends_with(']') => &line[..i],
        _ => line,
    }
}

/// The message without its `[code]` tag (lint messages are shown as they are).
pub fn body(message: &str) -> String {
    let mut lines = message.lines();
    let first = lines.next().map(without_code).unwrap_or("");
    std::iter::once(first).chain(lines).collect::<Vec<_>>().join("\n")
}

/// Noise tsc puts in types: absolute `import("/…/node_modules/x").` prefixes and React's
/// `IntrinsicAttributes & `.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("import(\"") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 8..];
        match after.find("\").") {
            Some(j) => rest = &after[j + 3..],
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.replace("IntrinsicAttributes & ", "")
}

/// Splits a line into prose and quoted names / types (tsc quotes them with `'`).
fn parts(line: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut text = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // An opening quote: not inside a word (`don't`), with a closing quote ahead.
        let opens = c == '\'' && (i == 0 || !chars[i - 1].is_alphanumeric());
        if opens && let Some(len) = closing_quote(&chars[i + 1..]) {
            if !text.is_empty() {
                out.push(Part::Text(std::mem::take(&mut text)));
            }
            out.push(Part::Code(chars[i + 1..i + 1 + len].iter().collect()));
            i += len + 2;
            continue;
        }
        text.push(c);
        i += 1;
    }
    if !text.is_empty() {
        out.push(Part::Text(text));
    }
    out
}

/// Length up to the quote that closes a quoted type: the next `'` followed by a non-word char.
fn closing_quote(rest: &[char]) -> Option<usize> {
    let mut depth = 0i32;
    for (k, c) in rest.iter().enumerate() {
        match c {
            '{' | '(' | '[' | '<' => depth += 1,
            '}' | ')' | ']' | '>' => depth -= 1,
            '\'' if depth <= 0 && rest.get(k + 1).is_none_or(|n| !n.is_alphanumeric()) => return Some(k),
            _ => {}
        }
    }
    None
}

/// The chain as steps (code tag removed, noise cleaned).
pub fn steps(message: &str) -> Vec<Step> {
    message
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| {
            let depth = if n == 0 { 0 } else { (l.len() - l.trim_start().len()).div_ceil(2) };
            let line = if n == 0 { without_code(l) } else { l.trim() };
            Step { depth, parts: parts(&clean(line)) }
        })
        .collect()
}

/// A short form of a long type: `{ title, onPress, … }`, `Promise<…>`, `A | B | …`.
pub fn short_type(t: &str) -> String {
    let t = t.trim();
    if t.chars().count() <= 32 {
        return t.to_string();
    }
    if t.starts_with('{') && t.ends_with('}') {
        let names = member_names(&t[1..t.len() - 1]);
        let more = names.len() > 3;
        let mut s: Vec<String> = names.into_iter().take(3).collect();
        if more {
            s.push("…".into());
        }
        return if s.is_empty() { "{…}".into() } else { format!("{{ {} }}", s.join(", ")) };
    }
    let top = split_top(t, " | ");
    if top.len() > 2 {
        return format!("{} | {} | …", short_type(top[0]), short_type(top[1]));
    }
    if let Some(i) = t.find('<')
        && t.ends_with('>')
        && i > 0
    {
        return format!("{}<…>", &t[..i]);
    }
    let cut: String = t.chars().take(29).collect();
    format!("{cut}…")
}

/// Property names of an object type body (`a: X; b?: Y; c(): Z`), at the top level.
fn member_names(body: &str) -> Vec<String> {
    split_top(body, ";")
        .into_iter()
        .filter_map(|m| {
            let m = m.trim().trim_start_matches("readonly ");
            let end = m.find([':', '(', '<']).unwrap_or(m.len());
            let name = m[..end].trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Splits on `sep` outside brackets and quotes.
fn split_top<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0usize, None::<char>);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        let c = bytes[i] as char;
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '`' => quote = Some(c),
                '{' | '(' | '[' | '<' => depth += 1,
                '}' | ')' | ']' => depth -= 1,
                '>' if i == 0 || bytes[i - 1] != b'=' => depth -= 1,
                _ if depth == 0 && s[i..].starts_with(sep) => {
                    out.push(&s[start..i]);
                    i += sep.len();
                    start = i;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    out.push(&s[start..]);
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

fn line_text(parts: &[Part], short: bool) -> String {
    parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => t.clone(),
            Part::Code(c) if short => format!("'{}'", short_type(c)),
            Part::Code(c) => format!("'{c}'"),
        })
        .collect()
}

/// The one-line label: the first line, and for a chain the innermost reason after `→`, with
/// long types shortened. `ts` is false for lint messages (only the first line is used).
pub fn label(message: &str, ts: bool) -> String {
    if !ts {
        return message.lines().next().unwrap_or("").to_string();
    }
    let steps = steps(message);
    let Some(first) = steps.first() else { return String::new() };
    let mut s = line_text(&first.parts, true);
    if steps.len() > 1
        && let Some(last) = steps.last()
    {
        s = format!("{} → {}", s.trim_end_matches('.'), line_text(&last.parts, true));
    }
    if let Some(code) = code(message) {
        s = format!("{s} [{code}]");
    }
    s
}

/// A long type laid out on several lines (object members one per line, nested objects
/// indented), for the hover card. Short types stay as they are.
pub fn format_type(t: &str) -> String {
    if t.chars().count() <= 60 {
        return t.to_string();
    }
    let mut out = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let indent = |d: usize| "  ".repeat(d);
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '`' => {
                quote = Some(c);
                out.push(c);
            }
            '{' if chars.get(i + 1) == Some(&' ') => {
                depth += 1;
                out.push('{');
                out.push('\n');
                out.push_str(&indent(depth));
                i += 2;
                continue;
            }
            ';' => {
                out.push(';');
                let mut j = i + 1;
                while chars.get(j) == Some(&' ') {
                    j += 1;
                }
                if chars.get(j) == Some(&'}') {
                    // `; }`: the closing brace goes on its own line.
                    depth = depth.saturating_sub(1);
                    out.push('\n');
                    out.push_str(&indent(depth));
                    out.push('}');
                    i = j + 1;
                    continue;
                }
                out.push('\n');
                out.push_str(&indent(depth));
                i = j;
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Part, code, format_type, label, short_type, steps};

    const MISSING: &str = "Type '{ title: string; onPress: () => void; }' is not assignable to type 'IntrinsicAttributes & ButtonProps'. [TS2322]\n  Property 'color' is missing in type '{ title: string; onPress: () => void; }' but required in type 'ButtonProps'.";

    #[test]
    fn labels_lead_with_the_reason() {
        assert_eq!(
            label(MISSING, true),
            "Type '{ title, onPress }' is not assignable to type 'ButtonProps' → Property 'color' is missing in type '{ title, onPress }' but required in type 'ButtonProps'. [TS2322]"
        );
        assert_eq!(label("Cannot find name 'foo'. [TS2304]", true), "Cannot find name 'foo'. [TS2304]");
        assert_eq!(label("'x' is unused [no-unused-vars]\nmore", false), "'x' is unused [no-unused-vars]");
        assert_eq!(code(MISSING).as_deref(), Some("TS2322"));
    }

    #[test]
    fn steps_keep_depth_and_split_types() {
        let s = steps(MISSING);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].depth, 1);
        assert_eq!(s[0].parts[0], Part::Text("Type ".into()));
        assert_eq!(s[0].parts[1], Part::Code("{ title: string; onPress: () => void; }".into()));
        assert_eq!(s[0].parts[3], Part::Code("ButtonProps".into()), "IntrinsicAttributes noise dropped");
        let imp = steps("Type 'import(\"/Users/a/node_modules/x/index\").Foo' is not assignable to type 'Bar'.");
        assert_eq!(imp[0].parts[1], Part::Code("Foo".into()));
        // An apostrophe inside a word isn't a quote.
        assert_eq!(steps("Object doesn't have 'x'.")[0].parts.len(), 3);
    }

    #[test]
    fn shortens_and_formats_types() {
        assert_eq!(short_type("string"), "string");
        assert_eq!(short_type("{ a: string; b: number; c: boolean; d: string[]; }"), "{ a, b, c, … }");
        assert_eq!(short_type("\"primary\" | \"secondary\" | \"danger\" | \"link\" | undefined"), "\"primary\" | \"secondary\" | …");
        assert_eq!(short_type("Promise<{ id: string; name: string; email: string; }>"), "Promise<…>");
        assert_eq!(
            format_type("{ title: string; onPress: () => void; style: { margin: number; padding: number; }; }"),
            "{\n  title: string;\n  onPress: () => void;\n  style: {\n    margin: number;\n    padding: number;\n  };\n}"
        );
    }
}
