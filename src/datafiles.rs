//! Built-in checks and formatting for JSON and YAML (no external tools needed).

use crate::editor::{Diag, Severity};
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Json,
    /// JSON with comments and trailing commas (tsconfig, VS Code settings, .jsonc).
    Jsonc,
    Yaml,
}

pub fn kind(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let in_vscode = path.components().any(|c| c.as_os_str() == ".vscode" || c.as_os_str() == ".devcontainer");
    match ext.as_str() {
        "jsonc" | "json5" => Some(Kind::Jsonc),
        "json" if in_vscode || name.starts_with("tsconfig") || name.starts_with("jsconfig") || name == ".eslintrc.json" || name == "devcontainer.json" || name == "biome.json" => Some(Kind::Jsonc),
        "json" => Some(Kind::Json),
        "yml" | "yaml" => Some(Kind::Yaml),
        _ => None,
    }
}

fn diag(row: usize, col: usize, severity: Severity, message: String, source: &str) -> Diag {
    Diag { row, col, end_row: row, end_col: col, severity, message, source: source.into() }
}

/// Syntax errors and duplicate keys.
pub fn check(kind: Kind, text: &str) -> Vec<Diag> {
    match kind {
        Kind::Yaml => match yaml_rust2::YamlLoader::load_from_str(text) {
            Ok(_) => Vec::new(),
            Err(e) => {
                let m = e.marker();
                vec![diag(m.line().saturating_sub(1), m.col(), Severity::Error, format!("YAML: {}", e.info()), "yaml")]
            }
        },
        Kind::Json | Kind::Jsonc => {
            let scan = scan_json(text, kind == Kind::Jsonc);
            let mut out = scan.problems;
            if text.trim().is_empty() {
                return out;
            }
            if let Err(e) = serde_json::from_str::<serde_json::Value>(&scan.cleaned) {
                let msg = e.to_string();
                let msg = msg.split(" at line").next().unwrap_or(&msg).to_string();
                out.push(diag(e.line().saturating_sub(1), e.column().saturating_sub(1), Severity::Error, format!("JSON: {msg}"), "json"));
            }
            out
        }
    }
}

struct Scan {
    /// Same length/positions as the input, with comments and trailing commas blanked (for jsonc).
    cleaned: String,
    problems: Vec<Diag>,
}

/// Walks JSON tokens: blanks comments/trailing commas (jsonc) and finds duplicate keys.
fn scan_json(text: &str, jsonc: bool) -> Scan {
    let bytes = text.as_bytes();
    let mut cleaned = bytes.to_vec();
    let mut problems = Vec::new();
    // Stack of containers: Some(keys) for objects, None for arrays.
    let mut stack: Vec<Option<HashSet<String>>> = Vec::new();
    let (mut row, mut col) = (0usize, 0usize);
    let mut i = 0;
    let mut last_comma: Option<usize> = None;
    let mut expect_key = false;
    let blank = |c: &mut Vec<u8>, a: usize, b: usize| {
        for x in c.iter_mut().take(b).skip(a) {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    };
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' => {
                row += 1;
                col = 0;
                i += 1;
                continue;
            }
            b'/' if jsonc && bytes.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map(|n| i + n).unwrap_or(bytes.len());
                blank(&mut cleaned, i, end);
                col += end - i;
                i = end;
                continue;
            }
            b'/' if jsonc && bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map(|n| i + 2 + n + 2).unwrap_or(bytes.len());
                for &c in &bytes[i..end] {
                    if c == b'\n' {
                        row += 1;
                        col = 0;
                    } else {
                        col += 1;
                    }
                }
                blank(&mut cleaned, i, end);
                i = end;
                continue;
            }
            b'"' => {
                let (start_row, start_col) = (row, col);
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'\n') {
                        break;
                    }
                    j += 1;
                }
                let s = &text[i + 1..j.min(bytes.len())];
                // Is this string a key? (followed by ':' inside an object, where a key is expected)
                let mut k = j + 1;
                while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t' || bytes[k] == b'\r' || bytes[k] == b'\n') {
                    k += 1;
                }
                if expect_key
                    && bytes.get(k) == Some(&b':')
                    && let Some(Some(keys)) = stack.last_mut()
                    && !keys.insert(s.to_string())
                {
                    problems.push(Diag {
                        row: start_row,
                        col: start_col,
                        end_row: start_row,
                        end_col: start_col + s.chars().count() + 2,
                        severity: Severity::Warning,
                        message: format!("Duplicate key \"{s}\" (the last one wins)"),
                        source: "json".into(),
                    });
                }
                expect_key = false;
                last_comma = None;
                col += text[i..(j + 1).min(bytes.len())].chars().count();
                i = (j + 1).min(bytes.len());
                continue;
            }
            b'{' => {
                stack.push(Some(HashSet::new()));
                expect_key = true;
                last_comma = None;
            }
            b'[' => {
                stack.push(None);
                last_comma = None;
            }
            b'}' | b']' => {
                if jsonc && let Some(c) = last_comma.take() {
                    cleaned[c] = b' ';
                }
                stack.pop();
                expect_key = false;
            }
            b',' => {
                last_comma = Some(i);
                expect_key = matches!(stack.last(), Some(Some(_)));
            }
            b' ' | b'\t' | b'\r' => {}
            _ => {
                last_comma = None;
            }
        }
        if b < 0x80 || b >= 0xC0 {
            col += 1;
        }
        i += 1;
    }
    Scan { cleaned: String::from_utf8(cleaned).unwrap_or_default(), problems }
}

/// Pretty-prints JSON keeping key order (used when neither Prettier nor Biome is installed).
/// Files with comments are left to real formatters.
pub fn format_json(text: &str, indent: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text).ok()?;
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut depth = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(indent);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                out.push('"');
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == '\\' {
                        i += 1;
                        if i < chars.len() {
                            out.push(chars[i]);
                        }
                    } else if chars[i] == '"' {
                        break;
                    }
                    i += 1;
                }
            }
            '{' | '[' => {
                // Keep empty containers compact: {} and [].
                let close = if c == '{' { '}' } else { ']' };
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&close) {
                    out.push(c);
                    out.push(close);
                    i = j;
                } else {
                    out.push(c);
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
        i += 1;
    }
    out.push('\n');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{Kind, check, format_json};

    #[test]
    fn json_errors_and_duplicates() {
        let d = check(Kind::Json, "{\n  \"a\": 1,\n  \"a\": 2\n}");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].row, 2);
        assert!(d[0].message.contains("Duplicate"));
        let d = check(Kind::Json, "{\n  \"a\": 1,\n}");
        assert!(d.iter().any(|d| d.message.starts_with("JSON") && d.row == 2));
        // Same object nesting level only: nested "a" is fine.
        assert!(check(Kind::Json, r#"{"a": {"a": 1}}"#).is_empty());
    }

    #[test]
    fn jsonc_allows_comments_and_trailing_commas() {
        let text = "{\n  // comment\n  \"compilerOptions\": { \"strict\": true, },\n  /* block */\n}";
        assert!(check(Kind::Jsonc, text).is_empty());
        assert!(!check(Kind::Json, text).is_empty());
    }

    #[test]
    fn yaml_errors() {
        assert!(check(Kind::Yaml, "a: 1\nb:\n  - x\n").is_empty());
        let d = check(Kind::Yaml, "a: 1\nb: [1, 2\nc: 3\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].row >= 1, "{:?}", d[0]);
    }

    #[test]
    fn formats_json_keeping_order() {
        let out = format_json(r#"{"z":1,"a":[1,2],"e":{},"s":"x,y:{}"}"#, "  ").unwrap();
        assert_eq!(out, "{\n  \"z\": 1,\n  \"a\": [\n    1,\n    2\n  ],\n  \"e\": {},\n  \"s\": \"x,y:{}\"\n}\n");
        assert!(format_json("{bad", "  ").is_none());
    }
}
