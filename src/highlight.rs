//! Syntax highlighting with tree-sitter. Produces per-line spans in char columns.

use gpui::Hsla;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use crate::theme::c;

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

pub fn color(ix: u8) -> Hsla {
    let name = NAMES.get(ix as usize).copied().unwrap_or("");
    let base = name.split('.').next().unwrap_or("");
    c(match (name, base) {
        ("comment", _) => 0x5c6370,
        ("variable.builtin", _) | ("constant.builtin", _) => 0xe5c07b,
        ("variable.parameter", _) => 0xd19a66,
        ("function.builtin", _) => 0x56b6c2,
        ("punctuation.special", _) => 0xc678dd,
        ("string.special", _) | ("escape", _) => 0x56b6c2,
        ("type.builtin", _) => 0xe5c07b,
        (_, "keyword") => 0xc678dd,
        (_, "string") | (_, "character") | (_, "text") => 0x98c379,
        (_, "number") | (_, "boolean") | (_, "constant") => 0xd19a66,
        (_, "function") | (_, "method") => 0x61afef,
        (_, "type") | (_, "constructor") | (_, "module") | (_, "namespace") => 0xe5c07b,
        (_, "property") | (_, "field") | (_, "tag") | (_, "label") => 0xe06c75,
        (_, "attribute") => 0xd19a66,
        (_, "operator") => 0x56b6c2,
        (_, "punctuation") => 0x8b919c,
        _ => 0xc8ccd4,
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
    };
    let mut config = HighlightConfiguration::new(language, format!("{lang:?}"), &highlights, injections, "").ok()?;
    config.configure(NAMES);
    Some(config)
}

fn config(lang: Lang) -> Option<&'static HighlightConfiguration> {
    static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; 12] = [const { OnceLock::new() }; 12];
    CONFIGS[lang as usize].get_or_init(|| build(lang)).as_ref()
}

/// Spans per line: (char column range, highlight index).
pub type LineSpans = Vec<Vec<(Range<usize>, u8)>>;

pub fn highlight(lang: Lang, text: &str) -> Option<LineSpans> {
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

#[cfg(test)]
mod tests {
    use super::{Lang, highlight, is_comment};

    #[test]
    fn highlights_typescript() {
        let spans = highlight(Lang::TypeScript, "// hi\nconst x: number = 1;\n").expect("ts query compiles");
        assert!(spans[0].iter().any(|(_, h)| is_comment(*h)));
        assert!(!spans[1].is_empty());
    }

    #[test]
    fn all_languages_compile() {
        for lang in [Lang::Tsx, Lang::JavaScript, Lang::Json, Lang::Css, Lang::Html, Lang::Rust, Lang::Python, Lang::Go, Lang::Bash, Lang::Yaml, Lang::Toml] {
            assert!(highlight(lang, "x").is_some(), "{lang:?} failed to build");
        }
    }
}
