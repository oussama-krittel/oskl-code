//! Keeping secrets off the screen and out of commits: `.env` values are drawn as dots until the
//! cursor is on their line, and a commit that adds an env file or something shaped like an API
//! key asks first.

use regex::Regex;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

/// `.env`, `.env.local`, `.env.production`, `prod.env`, `.envrc` — but not the committed
/// templates (`.env.example`, `.env.sample`, `.env.template`, `.env.dist`).
pub fn is_env_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if [".example", ".sample", ".template", ".dist", ".schema"].iter().any(|s| n.ends_with(s)) {
        return false;
    }
    n == ".env" || n == ".envrc" || n.starts_with(".env.") || n.ends_with(".env")
}

pub fn is_env_path(path: &Path) -> bool {
    path.file_name().is_some_and(|n| is_env_file(&n.to_string_lossy()))
}

/// For each line of an env file, the char columns of the value to hide: what follows
/// `KEY=` (or `export KEY=`), up to a ` # comment` on unquoted values. A quoted value that
/// goes on over several lines is hidden on all of them.
pub fn env_masks(text: &str) -> Vec<Option<Range<usize>>> {
    let mut out = Vec::new();
    // Inside a multi-line quoted value: the quote that closes it.
    let mut open: Option<char> = None;
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if let Some(q) = open {
            let end = chars.iter().position(|c| *c == q).map(|p| p + 1).unwrap_or(chars.len());
            if end < chars.len() || chars.contains(&q) {
                open = None;
            }
            out.push((end > 0).then_some(0..end));
            continue;
        }
        out.push(value_range(&chars).map(|(r, unclosed)| {
            open = unclosed;
            r
        }));
    }
    out
}

/// One line of an env file with its value starred out, byte for byte (so offsets into it stay
/// the same as into the original).
pub fn mask_line(line: &str) -> String {
    let Some(r) = value_range(&line.chars().collect::<Vec<_>>()).map(|(r, _)| r) else { return line.to_string() };
    line.chars().enumerate().map(|(i, c)| if r.contains(&i) { "*".repeat(c.len_utf8()) } else { c.to_string() }).collect()
}

/// The value of a `KEY=value` line, and the quote left open at the end of the line, if any.
fn value_range(chars: &[char]) -> Option<(Range<usize>, Option<char>)> {
    let mut i = chars.iter().position(|c| !c.is_whitespace())?;
    if chars[i] == '#' {
        return None;
    }
    let rest: String = chars[i..].iter().collect();
    if rest.starts_with("export ") {
        i += 7;
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
    }
    let key_start = i;
    while chars.get(i).is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        i += 1;
    }
    if i == key_start {
        return None;
    }
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    if chars.get(i) != Some(&'=') && chars.get(i) != Some(&':') {
        return None;
    }
    i += 1;
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    let start = i;
    let end = match chars[i] {
        q @ ('"' | '\'' | '`') => match chars[i + 1..].iter().position(|c| *c == q) {
            Some(p) => i + 2 + p,
            None => return Some((start..chars.len(), Some(q))),
        },
        _ => {
            // Unquoted: up to ` #` (a comment), trailing spaces trimmed.
            let mut e = chars.len();
            for k in i..chars.len() {
                if chars[k] == '#' && k > 0 && chars[k - 1].is_whitespace() {
                    e = k;
                    break;
                }
            }
            while e > start && chars[e - 1].is_whitespace() {
                e -= 1;
            }
            e
        }
    };
    (end > start).then_some((start..end, None))
}

/// Something in a staged change that probably shouldn't be committed.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub path: String,
    /// 1-based line in the new file; None for a whole file (an env file).
    pub line: Option<usize>,
    pub what: String,
}

impl Finding {
    pub fn describe(&self) -> String {
        match self.line {
            Some(l) => format!("{}:{l} — {}", self.path, self.what),
            None => format!("{} — {}", self.path, self.what),
        }
    }
}

fn patterns() -> &'static [(&'static str, Regex)] {
    static P: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    P.get_or_init(|| {
        [
            ("a private key", r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY"),
            ("an AWS access key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            ("a GitHub token", r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{40,})\b"),
            ("an Anthropic API key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
            ("an OpenAI API key", r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{32,}"),
            ("a Stripe live key", r"\b(?:sk|rk)_live_[0-9A-Za-z]{16,}"),
            ("a Google API key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
            ("a Slack token", r"\bxox[abprs]-[0-9A-Za-z\-]{10,}"),
            ("a SendGrid key", r"\bSG\.[0-9A-Za-z_\-]{16,}\.[0-9A-Za-z_\-]{16,}"),
            (
                "a hard-coded secret",
                r#"(?i)\b(?:api[_-]?key|api[_-]?secret|secret[_-]?key|client[_-]?secret|access[_-]?token|auth[_-]?token|private[_-]?key|password|passwd)\b["']?\s*[:=]\s*["'][^"'\s]{12,}["']"#,
            ),
        ]
        .into_iter()
        .map(|(what, re)| (what, Regex::new(re).expect("valid secret pattern")))
        .collect()
    })
}

/// What a line looks like it contains, with the match shortened so it isn't shown in full.
pub fn scan_line(line: &str) -> Option<String> {
    for (what, re) in patterns() {
        if let Some(m) = re.find(line) {
            let shown: String = m.as_str().chars().take(6).collect();
            return Some(format!("looks like {what} ({shown}…)"));
        }
    }
    None
}

/// Env files and key-shaped strings in what's staged. Lock files and minified lines are
/// skipped (hashes and bundles look random); at most 20 findings.
pub fn scan_staged(root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Ok(names) = crate::git::run(root, &["diff", "--cached", "--name-only", "--diff-filter=ACMR"]) {
        for name in names.lines().filter(|n| !n.is_empty()) {
            let file = name.rsplit('/').next().unwrap_or(name);
            if is_env_file(file) {
                out.push(Finding { path: name.to_string(), line: None, what: "an environment file (keys and passwords usually live here)".into() });
            }
        }
    }
    let Ok(diff) = crate::git::run(root, &["diff", "--cached", "-U0", "--no-color", "--no-ext-diff", "--diff-filter=ACMR"]) else { return out };
    out.extend(scan_diff(&diff));
    out.truncate(20);
    out
}

/// Added lines of a `git diff -U0` that look like secrets.
pub fn scan_diff(diff: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut file: Option<String> = None;
    let mut line_no = 0usize;
    for l in diff.lines() {
        if let Some(p) = l.strip_prefix("+++ ") {
            let p = p.strip_prefix("b/").unwrap_or(p).to_string();
            let name = p.rsplit('/').next().unwrap_or(&p).to_ascii_lowercase();
            let skip = is_env_file(&name) || name.ends_with(".lock") || name.ends_with("-lock.json") || name.ends_with("-lock.yaml") || name.ends_with(".min.js") || name.ends_with(".map");
            file = (!skip && p != "/dev/null").then_some(p);
            continue;
        }
        if let Some(h) = l.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@ — the next added line is line c.
            line_no = h.split_whitespace().find_map(|t| t.strip_prefix('+')).and_then(|t| t.split(',').next()).and_then(|n| n.parse().ok()).unwrap_or(1);
            continue;
        }
        let Some(added) = l.strip_prefix('+') else { continue };
        if let Some(path) = &file
            && added.len() < 2_000
            && let Some(what) = scan_line(added)
        {
            out.push(Finding { path: path.clone(), line: Some(line_no), what });
            if out.len() >= 20 {
                break;
            }
        }
        line_no += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{env_masks, is_env_file, scan_diff, scan_line};

    #[test]
    fn env_files_but_not_templates() {
        for n in [".env", ".env.local", ".env.production", "prod.env", ".envrc"] {
            assert!(is_env_file(n), "{n}");
        }
        for n in [".env.example", ".env.sample", ".env.template", "environment.ts", "env.d.ts"] {
            assert!(!is_env_file(n), "{n}");
        }
    }

    #[test]
    fn masks_values_only() {
        let text = "# comment\nAPI_KEY=abc123\nexport TOKEN = \"x y\"\nURL=http://a # note\nEMPTY=\nPEM=\"-----BEGIN\nsecret\n-----END\"\nNEXT=1";
        let m = env_masks(text);
        assert_eq!(m[0], None);
        assert_eq!(m[1], Some(8..14));
        assert_eq!(m[2], Some(15..20), "quotes included");
        assert_eq!(m[3], Some(4..12), "comment stays visible");
        assert_eq!(m[4], None);
        assert_eq!(m[5], Some(4..15), "multi-line value starts");
        assert_eq!(m[6], Some(0..6), "and continues");
        assert_eq!(m[7], Some(0..9), "up to the closing quote");
        assert_eq!(m[8], Some(5..6));
    }

    #[test]
    fn stars_values_byte_for_byte() {
        assert_eq!(super::mask_line("KEY=abc # note"), "KEY=*** # note");
        assert_eq!(super::mask_line("KEY=é1").len(), "KEY=é1".len());
        assert_eq!(super::mask_line("# just a comment"), "# just a comment");
    }

    #[test]
    fn spots_keys_in_added_lines() {
        assert!(scan_line("const key = 'sk_live_51HxQabcdefghijklmnop';").unwrap().contains("Stripe"));
        assert!(scan_line(r#"  "apiKey": "AIzaSyA1234567890abcdefghijklmnopqrstuv","#).unwrap().contains("Google"));
        assert!(scan_line("password: 'correct-horse-battery'").unwrap().contains("hard-coded"));
        assert!(scan_line("const password = input.value;").is_none());
        assert!(scan_line("integrity sha512-abcdefghijklmnopqrstuvwxyz0123456789").is_none());
        let diff = "diff --git a/src/a.ts b/src/a.ts\n+++ b/src/a.ts\n@@ -0,0 +10,2 @@\n+const ok = 1;\n+const t = 'ghp_abcdefghijklmnopqrstuvwxyz0123456789AB';\n+++ b/yarn.lock\n@@ -1 +1 @@\n+  token ghp_abcdefghijklmnopqrstuvwxyz0123456789AB\n";
        let f = scan_diff(diff);
        assert_eq!(f.len(), 1, "lock files skipped: {f:?}");
        assert_eq!((f[0].path.as_str(), f[0].line), ("src/a.ts", Some(11)));
        assert!(f[0].what.contains("GitHub") && !f[0].what.contains("0123456789AB"), "shortened: {}", f[0].what);
    }
}
