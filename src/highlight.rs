//! Syntax highlighting with tree-sitter. Produces per-line spans in char columns.

use gpui::Hsla;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};


/// Capture names we color, in priority order of specificity.
const NAMES: &[&str] = &[
    "attribute", "boolean", "comment", "constant", "constant.builtin", "constructor", "embedded", "escape",
    "function", "function.builtin", "function.method", "keyword", "label", "module", "number", "operator",
    "property", "punctuation", "punctuation.bracket", "punctuation.delimiter", "punctuation.special", "string",
    "string.special", "tag", "type", "type.builtin", "variable", "variable.builtin", "variable.parameter",
    "character", "namespace", "field", "method", "text.title", "text.uri", "text.literal",
];

/// The capture name of a span's index (`comment`, `string`, `function.method`…).
pub fn name(ix: u8) -> &'static str {
    NAMES.get(ix as usize).copied().unwrap_or("")
}

/// Span indexes from here on are CSV columns (their colors cycle).
pub const COLUMN_BASE: u8 = 200;
const COLUMN_COLORS: usize = 10;

/// A CSV column's color: the theme's own syntax colors, in turn.
pub fn column_color(col: usize) -> Hsla {
    use crate::theme::Syntax::*;
    const ORDER: [crate::theme::Syntax; COLUMN_COLORS] = [Keyword, String, Number, Function, Type, Property, Builtin, Attribute, StringSpecial, Param];
    crate::theme::syntax(ORDER[col % COLUMN_COLORS])
}

pub fn color(ix: u8) -> Hsla {
    if ix >= COLUMN_BASE {
        return column_color((ix - COLUMN_BASE) as usize);
    }
    let name = NAMES.get(ix as usize).copied().unwrap_or("");
    let base = name.split('.').next().unwrap_or("");
    use crate::theme::Syntax::*;
    crate::theme::syntax(match (name, base) {
        ("comment", _) => Comment,
        ("variable.builtin", _) | ("constant.builtin", _) => Builtin,
        ("variable.parameter", _) => Param,
        ("function.builtin", _) => FnBuiltin,
        ("punctuation.special", _) => PunctSpecial,
        ("string.special", _) | ("escape", _) => StringSpecial,
        ("type.builtin", _) => Builtin,
        (_, "keyword") => Keyword,
        (_, "string") | (_, "character") | (_, "text") => String,
        (_, "number") | (_, "boolean") | (_, "constant") => Number,
        (_, "function") | (_, "method") => Function,
        (_, "type") | (_, "constructor") | (_, "module") | (_, "namespace") => Type,
        (_, "property") | (_, "field") | (_, "tag") | (_, "label") => Property,
        (_, "attribute") => Attribute,
        (_, "operator") => Operator,
        (_, "punctuation") => Punct,
        _ => Plain,
    })
}

pub fn is_comment(ix: u8) -> bool {
    NAMES.get(ix as usize) == Some(&"comment")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    TypeScript,
    Tsx,
    JavaScript,
    Json,
    Css,
    Html,
    Rust,
    Python,
    Go,
    Bash,
    Yaml,
    Toml,
    Terraform,
    /// Comma (or semicolon…) and tab separated values: each column in its own color.
    Csv,
    Tsv,
    /// SQL (keywords, strings, numbers, comments, functions): colored by a small lexer.
    Sql,
    /// SQL Server's SQL: [bracketed names] too.
    Tsql,
    Java,
}

pub fn detect(path: &Path) -> Option<Lang> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    Some(match ext.as_str() {
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "json" | "jsonc" | "json5" => Lang::Json,
        "css" | "scss" | "less" => Lang::Css,
        "html" | "htm" | "vue" | "svelte" => Lang::Html,
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "go" => Lang::Go,
        "sh" | "bash" | "zsh" => Lang::Bash,
        "yml" | "yaml" => Lang::Yaml,
        "toml" => Lang::Toml,
        "csv" => Lang::Csv,
        "tsv" | "tab" => Lang::Tsv,
        "sql" | "psql" | "pgsql" => Lang::Sql,
        "tsql" => Lang::Tsql,
        "tf" | "tfvars" | "hcl" | "tftest" => Lang::Terraform,
        "java" => Lang::Java,
        _ if name == ".zshrc" || name == ".bashrc" || name.ends_with(".env") || name == ".envrc" => Lang::Bash,
        _ if name == "cargo.lock" => Lang::Toml,
        _ => return None,
    })
}

fn build(lang: Lang) -> Option<HighlightConfiguration> {
    use tree_sitter_javascript as js;
    let (language, highlights, injections): (tree_sitter::Language, String, &str) = match lang {
        Lang::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!("{}\n{}", tree_sitter_typescript::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY),
            "",
        ),
        Lang::Tsx => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            format!("{}\n{}\n{}", tree_sitter_typescript::HIGHLIGHTS_QUERY, js::JSX_HIGHLIGHT_QUERY, js::HIGHLIGHT_QUERY),
            "",
        ),
        Lang::JavaScript => (js::LANGUAGE.into(), format!("{}\n{}", js::JSX_HIGHLIGHT_QUERY, js::HIGHLIGHT_QUERY), ""),
        Lang::Json => (tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Css => (tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Html => (tree_sitter_html::LANGUAGE.into(), tree_sitter_html::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Rust => (tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Python => (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Go => (tree_sitter_go::LANGUAGE.into(), tree_sitter_go::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Bash => (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY.into(), ""),
        Lang::Yaml => (tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Toml => (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(), ""),
        Lang::Terraform => (tree_sitter_hcl::LANGUAGE.into(), include_str!("hcl_highlights.scm").into(), ""),
        // Colored by `columns`, not tree-sitter.
        Lang::Csv | Lang::Tsv | Lang::Sql | Lang::Tsql => return None,
        Lang::Java => (tree_sitter_java::LANGUAGE.into(), tree_sitter_java::HIGHLIGHTS_QUERY.into(), ""),
    };
    let mut config = HighlightConfiguration::new(language, format!("{lang:?}"), &highlights, injections, "").ok()?;
    config.configure(NAMES);
    Some(config)
}

fn config(lang: Lang) -> Option<&'static HighlightConfiguration> {
    // One per language (room for a few more).
    static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; 24] = [const { OnceLock::new() }; 24];
    CONFIGS[lang as usize].get_or_init(|| build(lang)).as_ref()
}

/// Spans per line: (char column range, highlight index).
pub type LineSpans = Vec<Vec<(Range<usize>, u8)>>;

pub fn highlight(lang: Lang, text: &str) -> Option<LineSpans> {
    match lang {
        Lang::Csv => return Some(columns(text, crate::table::separator(std::path::Path::new("x.csv"), text.lines().next().unwrap_or("")))),
        Lang::Tsv => return Some(columns(text, b'\t')),
        Lang::Sql => return Some(sql(text, false)),
        Lang::Tsql => return Some(sql(text, true)),
        _ => {}
    }
    let config = config(lang)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter.highlight(config, text.as_bytes(), None, None, |_| None).ok()?;

    // Byte offset of each line start, to split spans per line.
    let mut line_starts = vec![0];
    line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let mut out: LineSpans = vec![Vec::new(); line_starts.len()];
    let mut stack: Vec<u8> = Vec::new();
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(h) => stack.push(h.0 as u8),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let Some(&h) = stack.last() else { continue };
                let mut line = line_starts.partition_point(|&s| s <= start) - 1;
                let mut pos = start;
                while pos < end && line < line_starts.len() {
                    let line_start = line_starts[line];
                    let line_end = line_starts.get(line + 1).map(|e| e - 1).unwrap_or(text.len());
                    let seg_end = end.min(line_end);
                    if seg_end > pos {
                        let lt = &text[line_start..line_end];
                        let c0 = lt[..pos - line_start].chars().count();
                        let c1 = c0 + lt[pos - line_start..seg_end - line_start].chars().count();
                        out[line].push((c0..c1, h));
                    }
                    line += 1;
                    pos = line_starts.get(line).copied().unwrap_or(end);
                }
            }
        }
    }
    Some(out)
}

/// SQL's reserved words (and common types), upper case.
pub const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "ILIKE", "BETWEEN", "EXISTS", "AS", "ON", "JOIN", "INNER", "LEFT", "RIGHT", "FULL",
    "OUTER", "CROSS", "NATURAL", "USING", "GROUP", "BY", "ORDER", "ASC", "DESC", "NULLS", "FIRST", "LAST", "HAVING", "LIMIT", "OFFSET", "FETCH", "NEXT", "ROWS",
    "ONLY", "TOP", "DISTINCT", "ALL", "ANY", "SOME", "UNION", "INTERSECT", "EXCEPT", "INSERT", "INTO", "VALUES", "DEFAULT", "UPDATE", "SET", "DELETE", "RETURNING",
    "OUTPUT", "MERGE", "MATCHED", "THEN", "WHEN", "CASE", "ELSE", "END", "WITH", "RECURSIVE", "CREATE", "ALTER", "DROP", "TRUNCATE", "TABLE", "VIEW", "INDEX",
    "UNIQUE", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "CONSTRAINT", "CHECK", "COLUMN", "ADD", "RENAME", "TO", "SCHEMA", "DATABASE", "SEQUENCE", "IF",
    "CASCADE", "RESTRICT", "BEGIN", "COMMIT", "ROLLBACK", "TRANSACTION", "GRANT", "REVOKE", "EXPLAIN", "ANALYZE", "VACUUM", "OVER", "PARTITION", "WINDOW",
    "TRUE", "FALSE", "INTEGER", "INT", "BIGINT", "SMALLINT", "SERIAL", "BIGSERIAL", "TEXT", "VARCHAR", "NVARCHAR", "CHAR", "BOOLEAN", "BOOL", "DATE", "TIME",
    "TIMESTAMP", "TIMESTAMPTZ", "INTERVAL", "NUMERIC", "DECIMAL", "REAL", "FLOAT", "DOUBLE", "PRECISION", "JSON", "JSONB", "UUID", "BYTEA", "ARRAY", "GO", "DECLARE",
    "EXEC", "PROCEDURE", "FUNCTION", "RETURNS", "LANGUAGE", "TRIGGER", "FOR", "EACH", "ROW", "LATERAL", "ILIKE", "SIMILAR", "COLLATE", "CAST",
];

/// Colors SQL: keywords, 'strings', numbers, -- and /* */ comments, "quoted names", and names
/// called like functions.
/// `tsql`: SQL Server's dialect, where [brackets] quote names (elsewhere, PostgreSQL's
/// $$ … $$ / $tag$ … $tag$ strings).
fn sql(text: &str, tsql: bool) -> LineSpans {
    let idx = |n: &str| NAMES.iter().position(|x| *x == n).unwrap_or(0) as u8;
    let (kw, string, number, comment, func, prop, op) = (idx("keyword"), idx("string"), idx("number"), idx("comment"), idx("function"), idx("property"), idx("operator"));
    let mut out: LineSpans = Vec::new();
    let mut in_block = false;
    // Inside a dollar-quoted string: its tag ("$$", "$body$").
    let mut in_dollar: Option<Vec<char>> = None;
    let ends_at = |c: &[char], from: usize, tag: &[char]| (from..c.len().saturating_sub(tag.len() - 1)).find(|&j| c[j..].starts_with(tag));
    for line in text.split('\n') {
        let c: Vec<char> = line.chars().collect();
        let mut row = Vec::new();
        let mut i = 0;
        if let Some(tag) = in_dollar.clone() {
            match ends_at(&c, 0, &tag) {
                Some(j) => {
                    row.push((0..j + tag.len(), string));
                    i = j + tag.len();
                    in_dollar = None;
                }
                None => {
                    row.push((0..c.len(), string));
                    out.push(row);
                    continue;
                }
            }
        }
        if in_block {
            match line.find("*/") {
                Some(b) => {
                    let end = line[..b + 2].chars().count();
                    row.push((0..end, comment));
                    i = end;
                    in_block = false;
                }
                None => {
                    row.push((0..c.len(), comment));
                    out.push(row);
                    continue;
                }
            }
        }
        while i < c.len() {
            let ch = c[i];
            let start = i;
            if ch == '-' && c.get(i + 1) == Some(&'-') {
                row.push((i..c.len(), comment));
                break;
            } else if ch == '/' && c.get(i + 1) == Some(&'*') {
                let mut j = i + 2;
                while j + 1 < c.len() && !(c[j] == '*' && c[j + 1] == '/') {
                    j += 1;
                }
                if j + 1 < c.len() {
                    row.push((i..j + 2, comment));
                    i = j + 2;
                } else {
                    row.push((i..c.len(), comment));
                    in_block = true;
                    break;
                }
            } else if ch == '$' && !tsql && let Some(len) = c[i + 1..].iter().position(|x| *x == '$').filter(|n| c[i + 1..i + 1 + n].iter().all(|x| x.is_alphanumeric() || *x == '_') && !c.get(i + 1).is_some_and(|x| x.is_ascii_digit())) {
                // $$ … $$ or $tag$ … $tag$ (perhaps over several lines).
                let tag: Vec<char> = c[i..i + len + 2].to_vec();
                match ends_at(&c, i + tag.len(), &tag) {
                    Some(j) => {
                        row.push((i..j + tag.len(), string));
                        i = j + tag.len();
                    }
                    None => {
                        row.push((i..c.len(), string));
                        in_dollar = Some(tag);
                        break;
                    }
                }
            } else if ch == '\'' || ch == '"' || ch == '`' || (ch == '[' && tsql) {
                let close = if ch == '[' { ']' } else { ch };
                let mut j = i + 1;
                while j < c.len() && c[j] != close {
                    j += 1;
                }
                i = (j + 1).min(c.len());
                row.push((start..i, if ch == '\'' { string } else { prop }));
            } else if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
                while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.') {
                    i += 1;
                }
                row.push((start..i, number));
            } else if ch.is_alphabetic() || ch == '_' || ch == '@' || ch == '$' {
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$' || c[i] == '@') {
                    i += 1;
                }
                let word: String = c[start..i].iter().collect::<String>().to_uppercase();
                let mut k = i;
                while k < c.len() && c[k] == ' ' {
                    k += 1;
                }
                if SQL_KEYWORDS.contains(&word.as_str()) {
                    row.push((start..i, kw));
                } else if c.get(k) == Some(&'(') {
                    row.push((start..i, func));
                }
            } else {
                if "=<>!+-*/%|&".contains(ch) {
                    row.push((i..i + 1, op));
                }
                i += 1;
            }
        }
        out.push(row);
    }
    out
}

/// Colors a delimited file's columns (quoted fields, even over several lines, stay one
/// column). Huge files are colored up to a point, so the colors cost little memory.
fn columns(text: &str, sep: u8) -> LineSpans {
    const MAX_SPANS: usize = 400_000;
    let delim = NAMES.iter().position(|n| *n == "punctuation.delimiter").unwrap_or(0) as u8;
    let mut out: LineSpans = Vec::new();
    let (mut col, mut quoted, mut spans) = (0usize, false, 0usize);
    for line in text.split('\n') {
        let mut row = Vec::new();
        if spans < MAX_SPANS {
            let (mut start, mut c) = (0usize, 0usize);
            for ch in line.chars() {
                if ch == '"' {
                    quoted = !quoted;
                } else if ch as u32 == sep as u32 && !quoted {
                    if c > start {
                        row.push((start..c, COLUMN_BASE + (col % COLUMN_COLORS) as u8));
                    }
                    row.push((c..c + 1, delim));
                    col += 1;
                    start = c + 1;
                }
                c += 1;
            }
            if c > start {
                row.push((start..c, COLUMN_BASE + (col % COLUMN_COLORS) as u8));
            }
            spans += row.len();
        }
        if !quoted {
            col = 0;
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn sql_is_colored() {
        let names = |l: &Vec<(std::ops::Range<usize>, u8)>, text: &str| -> Vec<(String, &'static str)> {
            let c: Vec<char> = text.chars().collect();
            l.iter().map(|(r, h)| (c[r.clone()].iter().collect(), super::NAMES[*h as usize])).collect()
        };
        let text = "SELECT count(*) FROM \"users\" WHERE name = 'a''b' AND n > 2 -- note\n/* one\ntwo */ select 1";
        let s = super::sql(text, false);
        let lines: Vec<&str> = text.split('\n').collect();
        let first = names(&s[0], lines[0]);
        assert_eq!(first[0], ("SELECT".to_string(), "keyword"));
        assert!(first.contains(&("count".to_string(), "function")));
        assert!(first.contains(&("\"users\"".to_string(), "property")));
        assert!(first.contains(&("2".to_string(), "number")));
        assert!(first.contains(&("-- note".to_string(), "comment")));
        assert_eq!(names(&s[1], lines[1]), [("/* one".to_string(), "comment")]);
        let third = names(&s[2], lines[2]);
        assert_eq!(third[0], ("two */".to_string(), "comment"));
        assert_eq!(third[1], ("select".to_string(), "keyword"), "lower case too");
    }

    #[test]
    fn sql_dialects_quote_their_way() {
        let names = |l: &Vec<(std::ops::Range<usize>, u8)>, text: &str| -> Vec<(String, &'static str)> {
            let c: Vec<char> = text.chars().collect();
            l.iter().map(|(r, h)| (c[r.clone()].iter().collect(), super::NAMES[*h as usize])).collect()
        };
        // PostgreSQL: an array index isn't a name; $$ strings, over lines.
        let pg = "select tags[1] from t; do $$ begin\nraise 'x'; end $$;";
        let s = super::sql(pg, false);
        let first = names(&s[0], pg.lines().next().unwrap());
        assert!(first.contains(&("1".to_string(), "number")), "{first:?}");
        assert!(first.iter().any(|(t, n)| t.starts_with("$$ begin") && *n == "string"), "{first:?}");
        assert_eq!(names(&s[1], "raise 'x'; end $$;")[0], ("raise 'x'; end $$".to_string(), "string"));
        // SQL Server: [names].
        let ms = "select [order] from t";
        assert!(names(&super::sql(ms, true)[0], ms).contains(&("[order]".to_string(), "property")));
    }

    #[test]
    fn csv_columns_take_turns_in_colors() {
        let s = super::columns("a,b,\"c,d\"\nx;y", b',');
        let cols: Vec<u8> = s[0].iter().map(|(_, h)| *h).filter(|h| *h >= super::COLUMN_BASE).collect();
        assert_eq!(cols, [200, 201, 202], "the quoted comma stays in its column");
        assert_eq!(s[1].len(), 1, "a new line starts at the first column again");
    }

    use super::{Lang, highlight, is_comment};

    #[test]
    fn highlights_typescript() {
        let spans = highlight(Lang::TypeScript, "// hi\nconst x: number = 1;\n").expect("ts query compiles");
        assert!(spans[0].iter().any(|(_, h)| is_comment(*h)));
        assert!(!spans[1].is_empty());
    }

    #[test]
    fn highlights_terraform() {
        let src = "# note\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"logs-${var.env}\"\n  count  = 2\n}\n";
        let spans = highlight(Lang::Terraform, src).expect("hcl query compiles");
        let name_at = |row: usize, col: usize| spans[row].iter().find(|(r, _)| r.contains(&col)).map(|(_, h)| super::name(*h));
        assert_eq!(name_at(0, 0), Some("comment"));
        assert_eq!(name_at(1, 0), Some("keyword"), "block type");
        assert_eq!(name_at(1, 10), Some("type"), "block label");
        assert_eq!(name_at(2, 2), Some("property"), "attribute name");
        assert_eq!(name_at(2, 12), Some("string"));
        assert_eq!(name_at(2, 19), Some("variable.builtin"), "var in an interpolation");
        assert_eq!(name_at(3, 11), Some("number"));
        let map = highlight(Lang::Terraform, "tags = { Name = var.x }\n").unwrap();
        assert_eq!(map[0].iter().find(|(r, _)| r.contains(&9)).map(|(_, h)| super::name(*h)), Some("property"), "map keys");
    }

    #[test]
    fn all_languages_compile() {
        for lang in [Lang::Tsx, Lang::JavaScript, Lang::Json, Lang::Css, Lang::Html, Lang::Rust, Lang::Python, Lang::Go, Lang::Bash, Lang::Yaml, Lang::Toml, Lang::Terraform, Lang::Java] {
            assert!(highlight(lang, "x").is_some(), "{lang:?} failed to build");
        }
    }
}
