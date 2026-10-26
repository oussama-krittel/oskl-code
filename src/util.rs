//! Small helpers: time formatting and fuzzy matching.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn relative_time(t: i64) -> String {
    let s = (now() - t).max(0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match s {
        0..60 => "just now".into(),
        60..3600 => plural(s / 60, "minute"),
        3600..86400 => plural(s / 3600, "hour"),
        86400..2_592_000 => plural(s / 86400, "day"),
        2_592_000..31_536_000 => plural(s / 2_592_000, "month"),
        _ => plural(s / 31_536_000, "year"),
    }
}

pub fn short_time(t: i64) -> String {
    let s = (now() - t).max(0);
    match s {
        0..3600 => format!("{}m", (s / 60).max(1)),
        3600..86400 => format!("{}h", s / 3600),
        86400..2_592_000 => format!("{}d", s / 86400),
        2_592_000..31_536_000 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

/// Fuzzy subsequence match. Returns a score (higher is better) and the
/// byte positions of matched chars, or None when `query` doesn't match.
pub fn fuzzy(query: &str, candidate: &str) -> Option<(i64, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let cand: Vec<(usize, char)> = candidate.char_indices().collect();
    let base_start = candidate.rfind('/').map(|i| i + 1).unwrap_or(0);
    let mut positions = Vec::with_capacity(q.len());
    let mut score: i64 = 0;
    let mut qi = 0;
    let mut prev_match: Option<usize> = None;
    for (ci, &(byte, ch)) in cand.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if ch.to_lowercase().next() == Some(q[qi]) {
            let mut s = 1;
            let boundary = ci == 0 || matches!(cand[ci - 1].1, '/' | '_' | '-' | '.' | ' ') || (ch.is_uppercase() && cand[ci - 1].1.is_lowercase());
            if boundary {
                s += 8;
            }
            if prev_match == Some(ci.wrapping_sub(1)) {
                s += 6;
            }
            if byte >= base_start {
                s += 4;
            }
            score += s;
            positions.push(byte);
            prev_match = Some(ci);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    score -= (candidate.len() as i64) / 8;
    Some((score, positions))
}

/// Parses "path:line:col" style locations in tool output.
pub fn parse_location(line: &str) -> Option<(String, usize, usize)> {
    for token in line.split_whitespace() {
        let token = token.trim_matches(|c: char| c == '(' || c == ')' || c == ',' || c == '\'' || c == '"');
        let mut parts = token.split(':');
        let path = parts.next()?;
        if path.is_empty() || !path.contains('.') {
            continue;
        }
        let Some(row) = parts.next().and_then(|p| p.parse::<usize>().ok()) else { continue };
        let col = parts.next().and_then(|p| p.parse::<usize>().ok()).unwrap_or(1);
        return Some((path.to_string(), row, col));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_basename_boundaries() {
        let (a, _) = fuzzy("as", "src/components/ui/action-sheet.tsx").unwrap();
        let (b, _) = fuzzy("as", "src/assets/readme.md").unwrap();
        assert!(fuzzy("xyz", "abc").is_none());
        assert!(a > 0 && b > 0);
    }

    #[test]
    fn parses_locations() {
        assert_eq!(parse_location("src/a.ts:12:5 lint/x"), Some(("src/a.ts".into(), 12, 5)));
        assert_eq!(parse_location("no location here"), None);
    }
}
