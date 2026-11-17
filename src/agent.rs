//! Claude Code's own files and commands, for the Claude panel: its saved sessions
//! (~/.claude/projects/<folder>/*.jsonl) and `/usage`. The chat itself talks ACP (see `acp`).

use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::UNIX_EPOCH;

pub fn claude_bin() -> Result<PathBuf, String> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    crate::tools::search_path()
        .into_iter()
        .map(|d| d.join("claude"))
        .chain([home.join(".local/bin/claude"), home.join(".claude/local/claude"), "/opt/homebrew/bin/claude".into(), "/usr/local/bin/claude".into()])
        .find(|p| p.is_file())
        .ok_or_else(|| "Claude Code isn't installed: no `claude` on your shell's PATH. Install it (claude.com/claude-code), run `claude` once in a terminal to sign in, then try again.".into())
}

/// A session id as Claude Code writes them (safe to put in a command line).
pub(crate) fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() })
}

/// Claude Code's `/usage` text (plan limits). A local command: no tokens, nothing saved.
pub fn usage(cwd: &Path) -> Result<String, String> {
    let out = crate::tools::command(&claude_bin()?)
        .args(["-p", "--no-session-persistence", "--output-format", "json", "/usage"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run Claude Code: {e}"))?;
    let records: Value = serde_json::from_slice(&out.stdout).map_err(|_| format!("Claude Code /usage failed: {}", String::from_utf8_lossy(&out.stderr).trim()))?;
    let records = records.as_array().cloned().unwrap_or_else(|| vec![records]);
    records.iter().find(|r| r["type"] == "result").and_then(|r| r["result"].as_str()).map(String::from).ok_or_else(|| "Claude Code /usage returned nothing".into())
}

/// The last `/usage` and when it was read (seconds): chat tabs and the Claude panel share it.
static USAGE: std::sync::Mutex<Option<(i64, Result<String, String>)>> = std::sync::Mutex::new(None);

/// `/usage`, read again only when the last read is older than `max_age` seconds.
pub fn usage_cached(cwd: &Path, max_age: i64) -> Result<String, String> {
    if let Some((at, r)) = USAGE.lock().unwrap().clone()
        && crate::util::now() - at < max_age
    {
        return r;
    }
    let r = usage(cwd);
    *USAGE.lock().unwrap() = Some((crate::util::now(), r.clone()));
    r
}

/// A plan limit in `/usage` text: "Current week (all models): 7% used · resets Nov 15 at 1am (…)"
/// → ("Current week (all models)", 7.0, "resets Nov 15 at 1am (…)").
pub fn limit(line: &str) -> Option<(&str, f32, &str)> {
    let (k, v) = line.trim().split_once(": ")?;
    let (pct, rest) = v.split_once("% used")?;
    Some((k, pct.trim().parse().ok()?, rest.trim_start_matches([' ', '·']).trim()))
}

fn flag(r: &Value, k: &str) -> bool {
    r[k].as_bool().unwrap_or(false)
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    Some(&s[i..i + s[i..].find(b)?])
}

/// What the user typed, from a user record's content; None for tool results and injected messages.
fn prompt_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) if !blocks.iter().any(|b| b["type"] == "tool_result") => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .filter(|t| !crate::acp::is_ide_block(t))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let t = text.trim();
    if let Some(name) = between(t, "<command-name>", "</command-name>") {
        return Some(format!("{name} {}", between(t, "<command-args>", "</command-args>").unwrap_or("")).trim().to_string());
    }
    if t.starts_with('<') || t.starts_with("[Request interrupted") || t.starts_with("Caveat:") {
        return None;
    }
    (!t.is_empty() || content.as_array().is_some_and(|b| b.iter().any(|b| b["type"] == "image"))).then(|| t.to_string())
}

fn is_prompt(r: &Value) -> bool {
    r["type"] == "user" && !flag(r, "isMeta") && !flag(r, "isSidechain") && !flag(r, "isCompactSummary") && r["parent_tool_use_id"].is_null()
}

// ---------------------------------------------------------------- saved sessions

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub title: String,
    /// seconds since the epoch
    pub updated: i64,
}

/// Claude Code's folder for a project: the real path with every non-alphanumeric UTF-16 unit replaced by '-'.
// shortcut: Claude shortens names over 200 chars with a hash suffix; such deep project paths aren't listed.
fn project_dir_name(real: &Path) -> String {
    real.to_string_lossy().chars().flat_map(|c| if c.is_ascii_alphanumeric() { vec![c] } else { vec!['-'; c.len_utf16()] }).collect()
}

pub fn sessions_dir(cwd: &Path) -> PathBuf {
    let real = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/projects").join(project_dir_name(&real))
}

/// Title of a session file, and whether anything was said in it.
fn summarize(jsonl: &str) -> (String, bool) {
    let (mut custom, mut ai, mut summary, mut first, mut said) = (None, None, None, None, false);
    // Only lines that can matter are parsed: session files also hold large blobs.
    for line in jsonl.lines().filter(|l| ["\"user\"", "\"assistant\"", "-title\"", "\"summary\""].iter().any(|k| l.contains(k))) {
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        let s = |k: &str| r[k].as_str().map(String::from);
        match r["type"].as_str() {
            Some("custom-title") => custom = s("customTitle"),
            Some("ai-title") => ai = s("aiTitle"),
            Some("summary") => summary = s("summary"),
            Some("user") if is_prompt(&r) => {
                if let Some(t) = prompt_text(&r["message"]["content"]) {
                    first.get_or_insert(t);
                    said = true;
                }
            }
            _ => {}
        }
    }
    let title = [custom, ai, summary, first].into_iter().flatten().find(|t| !t.trim().is_empty()).unwrap_or_else(|| "Untitled".into());
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if title.chars().count() > 80 { title.chars().take(79).collect::<String>() + "…" } else { title };
    (title, said)
}

/// Session titles by file, with the file's (mtime, size) they were read at: listing again reads only changed files.
static TITLES: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, ((i64, u64), String, bool)>>> = std::sync::LazyLock::new(Default::default);

/// The project's saved conversations, newest first; `query` keeps those where something said contains it.
pub fn list_sessions(dir: &Path, query: &str) -> Vec<Session> {
    let q = query.trim().to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Session> = entries
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().strip_suffix(".jsonl").filter(|id| is_uuid(id))?.to_string();
            let meta = e.metadata().ok()?;
            let updated = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
            let path = e.path();
            let cached = TITLES.lock().unwrap().get(&path).filter(|c| c.0 == (updated, meta.len())).cloned();
            let (title, said) = match cached {
                Some((_, t, s)) => (t, s),
                None => {
                    let (t, s) = summarize(&std::fs::read_to_string(&path).ok()?);
                    TITLES.lock().unwrap().insert(path.clone(), ((updated, meta.len()), t.clone(), s));
                    (t, s)
                }
            };
            (said && (q.is_empty() || title.to_lowercase().contains(&q) || said_contains(&path, &q))).then_some(Session { id, title, updated })
        })
        .collect();
    out.sort_by_key(|s| -s.updated);
    out
}

/// Whether the user or Claude said `q` (lowercase) in a session, line by line.
fn said_contains(path: &Path, q: &str) -> bool {
    let Ok(f) = std::fs::File::open(path) else { return false };
    BufReader::new(f).lines().map_while(Result::ok).filter(|l| l.contains("\"user\"") || l.contains("\"assistant\"")).any(|l| {
        let Ok(r) = serde_json::from_str::<Value>(&l) else { return false };
        let said = match r["type"].as_str() {
            Some("user") if is_prompt(&r) => prompt_text(&r["message"]["content"]),
            Some("assistant") if !flag(&r, "isSidechain") => Some(r["message"]["content"].as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")),
            _ => None,
        };
        said.is_some_and(|t| t.to_lowercase().contains(q))
    })
}

/// Moves a session to the Trash: its .jsonl and the folder Claude keeps beside it.
pub fn trash_session(dir: &Path, id: &str) -> Result<(), String> {
    if !is_uuid(id) {
        return Err(format!("Bad session id: {id}"));
    }
    let paths: Vec<PathBuf> = [dir.join(format!("{id}.jsonl")), dir.join(id)].into_iter().filter(|p| p.exists()).collect();
    if paths.is_empty() {
        return Err(format!("Session {id} not found"));
    }
    trash::delete_all(&paths).map_err(|e| format!("Session {id}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0eb19df7-3ca5-48ef-aba8-04f15c8de66c";

    #[test]
    fn usage_limits() {
        assert_eq!(limit("Current week (all models): 7% used · resets Nov 15 at 1am (Africa/Casablanca)"), Some(("Current week (all models)", 7., "resets Nov 15 at 1am (Africa/Casablanca)")));
        assert_eq!(limit("Last 7d · 14582 requests · 42 sessions"), None);
    }

    #[test]
    fn ids_and_folders() {
        assert!(is_uuid(ID) && !is_uuid("../../etc/passwd") && !is_uuid(&format!("{ID}.jsonl")));
        assert_eq!(project_dir_name(Path::new("/Users/me/.cloudflared/a_b")), "-Users-me--cloudflared-a-b");
        assert_eq!(project_dir_name(Path::new("/x/é😀")), "-x----");
    }

    #[test]
    fn sessions_list_and_trash() {
        let dir = std::env::temp_dir().join(format!("oskl-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = [
            r#"{"type":"user","uuid":"u1","message":{"role":"user","content":[{"type":"text","text":"<ide_opened_file>x</ide_opened_file>"},{"type":"text","text":"Fix the   login\nbug"}]}}"#,
            r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"text","text":"Done"}]}}"#,
            r#"{"type":"custom-title","customTitle":"Login","sessionId":"x"}"#,
        ]
        .join("\n")
            + "\n";
        std::fs::write(dir.join(format!("{ID}.jsonl")), &text).unwrap();
        std::fs::write(dir.join("notes.jsonl"), &text).unwrap();
        let list = list_sessions(&dir, "");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Login");
        assert_eq!(list_sessions(&dir, "DONE").len(), 1);
        assert_eq!(list_sessions(&dir, "login bug").len(), 0, "said text is matched as written");
        assert!(list_sessions(&dir, "nowhere").is_empty());
        assert!(trash_session(&dir, "../x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
