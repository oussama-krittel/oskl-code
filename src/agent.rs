//! Claude Code chat: runs the user's own `claude` CLI in the project (stream-json over stdin/stdout,
//! permission prompts answered by the app), turns what it streams into chat items, and reads
//! Claude's own session files (~/.claude/projects/<folder>/*.jsonl) for the history list.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::time::UNIX_EPOCH;

const APPEND_PROMPT: &str = "You are running inside the OSKL Code editor's Claude tab. <ide_selection> and <ide_opened_file> blocks in a user message say what the user has open or selected in the editor.";

// ---------------------------------------------------------------- process

/// A running `claude`; dropping it stops it.
pub struct Proc {
    child: Option<Child>,
    stdin: ChildStdin,
}

impl Proc {
    /// One JSON message on its stdin (a user message, a control request or response).
    pub fn send(&mut self, msg: &Value) -> Result<(), String> {
        writeln!(self.stdin, "{msg}").and_then(|_| self.stdin.flush()).map_err(|e| format!("Claude Code has stopped: {e}"))
    }
}

impl Drop for Proc {
    /// Ends Claude Code and everything it started (its process group: dev servers, background
    /// commands): asked to stop first, so it cleans up; killed if still there after 2 s. Off the
    /// main thread.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let group = format!("-{}", child.id());
        std::thread::spawn(move || {
            let signal = |sig: &str| std::process::Command::new("/bin/kill").args([sig, "--", &group]).stderr(Stdio::null()).status();
            let _ = signal("-TERM");
            for _ in 0..20 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = signal("-KILL");
            let _ = child.wait();
        });
    }
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Opts {
    pub resume: Option<String>,
    /// With `resume`: continue from this record, as a new session (an edited or retried message).
    pub resume_at: Option<String>,
    pub model: String,
    pub effort: String,
    pub mode: String,
    /// Output style (Claude Code's writing style).
    pub style: String,
}

pub fn claude_bin() -> Result<PathBuf, String> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    crate::tools::search_path()
        .into_iter()
        .map(|d| d.join("claude"))
        .chain([home.join(".local/bin/claude"), home.join(".claude/local/claude"), "/opt/homebrew/bin/claude".into(), "/usr/local/bin/claude".into()])
        .find(|p| p.is_file())
        .ok_or_else(|| "Claude Code isn't installed: no `claude` on your shell's PATH. Install it (claude.com/claude-code), run `claude` once in a terminal to sign in, then try again.".into())
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() })
}

/// Starts `claude` in `cwd`; every line it prints arrives on the channel as JSON, then
/// `{"type":"exit","stderr":…}` when it ends.
pub fn start(cwd: &Path, o: &Opts) -> Result<(Proc, futures::channel::mpsc::UnboundedReceiver<Value>), String> {
    let mut cmd = crate::tools::command(&claude_bin()?);
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--replay-user-messages"])
        .args(["--permission-prompt-tool", "stdio", "--append-system-prompt", APPEND_PROMPT]);
    // Values go as separate arguments (no shell), but a leading '-' would still read as a flag.
    let ok = |v: &str| !v.is_empty() && !v.starts_with('-') && v.len() <= 80;
    if let Some(r) = o.resume.as_deref().filter(|r| is_uuid(r)) {
        cmd.args(["--resume", r]);
        if let Some(at) = o.resume_at.as_deref().filter(|r| is_uuid(r)) {
            cmd.args(["--resume-session-at", at, "--fork-session"]);
        }
    }
    for (flag, v) in [("--model", &o.model), ("--effort", &o.effort), ("--permission-mode", &o.mode)] {
        if ok(v) {
            cmd.args([flag, v]);
        }
    }
    if !o.style.is_empty() && o.style.len() <= 64 && !o.style.chars().any(char::is_control) {
        cmd.args(["--settings", &json!({ "outputStyle": o.style }).to_string()]);
    }
    let mut child = std::os::unix::process::CommandExt::process_group(&mut cmd, 0)
        .current_dir(cwd)
        // Print mode keeps no task list unless asked; the chat pins it above the message box.
        .env("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1")
        // Checkpoints, so "Rewind code" can put files back as they were before a message.
        .env("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start Claude Code: {e}"))?;
    let (stdout, mut stderr, stdin) = (child.stdout.take().unwrap(), child.stderr.take().unwrap(), child.stdin.take().unwrap());
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        let start = s.char_indices().rev().nth(3000).map_or(0, |(i, _)| i);
        s[start..].trim().to_string()
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && tx.unbounded_send(v).is_err()
            {
                return; // the chat is gone
            }
        }
        let _ = tx.unbounded_send(json!({ "type": "exit", "stderr": errors.join().unwrap_or_default() }));
    });
    Ok((Proc { child: Some(child), stdin }, rx))
}

/// What Claude Code offers (models, commands, styles: its `initialize` answer), asked in the home
/// folder so no project's hooks or servers run just to fill the pickers.
pub fn offers() -> Result<Value, String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let (mut p, rx) = start(&home, &Opts::default())?;
    p.send(&control("offers", json!({ "subtype": "initialize" })))?;
    futures::executor::block_on_stream(rx)
        .find_map(|ev| match ev["type"].as_str() {
            Some("control_response") if ev["response"]["request_id"] == "offers" => Some(Ok(ev["response"]["response"].clone())),
            Some("exit") => Some(Err(format!("Claude Code stopped: {}", ev["stderr"].as_str().unwrap_or("")))),
            _ => None,
        })
        .unwrap_or_else(|| Err("Claude Code stopped".into()))
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

pub fn control(id: &str, request: Value) -> Value {
    json!({ "type": "control_request", "request_id": id, "request": request })
}

/// A user message: pictures (media type, base64) first, then the context blocks, then the text.
pub fn user_message(text: &str, context: &[String], images: &[(String, String)]) -> Value {
    let mut content: Vec<Value> = images.iter().map(|(mt, data)| json!({ "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } })).collect();
    content.extend(context.iter().map(|c| json!({ "type": "text", "text": c })));
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    json!({ "type": "user", "message": { "role": "user", "content": content } })
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The picture types Claude reads, by file extension.
pub fn image_type(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

// ---------------------------------------------------------------- chat log

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub input: Value,
    /// (failed, text)
    pub result: Option<(bool, String)>,
    /// A subagent's steps (tool, target), when this tool runs one (Task / Agent).
    pub steps: Vec<(String, String)>,
}

/// A subagent or a background command Claude Code runs (its "tasks").
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: String,
    /// `local_agent` (a subagent), `local_bash` (a command), …
    pub kind: String,
    pub what: String,
    /// The subagent's type (general-purpose, Explore…).
    pub agent: String,
    /// The job that started this one.
    pub parent: Option<String>,
    /// The tool call that started it (its card in the chat).
    pub tool_use: Option<String>,
    /// running, completed, stopped, killed, failed
    pub status: String,
    /// What it's doing now, or its summary once done.
    pub last: String,
}

impl Job {
    pub fn running(&self) -> bool {
        self.status == "running"
    }

    pub fn is_agent(&self) -> bool {
        self.kind == "local_agent"
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Answer {
    Allow,
    Always,
    Deny,
}

/// Claude asking to use a tool (or a question, or its plan for review).
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub id: String,
    pub tool: String,
    pub input: Value,
    pub suggestions: Value,
    pub answer: Option<Answer>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// uuid: this message's record (file checkpoints are by it); before: the record just before
    /// it, where an edit or retry branches off.
    User { text: String, images: usize, context: Vec<String>, uuid: Option<String>, before: Option<String> },
    Text(String),
    Thinking(String),
    Tool(Tool),
    Ask(Ask),
    Error(String),
    /// A quiet line: how long a turn took, a compaction.
    Note(String),
}

#[derive(Default)]
pub struct Log {
    pub items: Vec<Item>,
    /// stream block / full message block -> index in items, so the final message replaces what was streamed
    keys: HashMap<String, usize>,
    counts: HashMap<String, usize>,
    blocks: HashMap<u64, String>,
    msg: String,
    pub session: Option<String>,
    /// The newest session record: a message sent now comes after it.
    pub last_uuid: Option<String>,
    pub busy: bool,
    /// Tokens in the context now, and the model's window.
    pub context: Option<(u64, u64)>,
    /// API-equivalent USD since the chat opened; `base`: what earlier processes spent.
    pub cost: f64,
    base: f64,
    /// The permission mode Claude Code reports (it leaves plan mode itself).
    pub mode: Option<String>,
    /// The usage-limits line Claude Code shows in its status bar.
    pub status: Option<String>,
    pub model: Option<String>,
    /// Subagents and background commands, in the order they started.
    pub jobs: Vec<Job>,
}

fn flag(r: &Value, k: &str) -> bool {
    r[k].as_bool().unwrap_or(false)
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    Some(&s[i..i + s[i..].find(b)?])
}

/// Text blocks the editor adds around a message (what's open, what's selected).
pub fn is_ide_block(t: &str) -> bool {
    t.starts_with("<ide_selection>") || t.starts_with("<ide_opened_file>")
}

/// What the user typed, from a user record's content; None for tool results and injected messages.
fn prompt_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) if !blocks.iter().any(|b| b["type"] == "tool_result") => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .filter(|t| !is_ide_block(t))
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

fn result_text(c: &Value) -> String {
    let t = match c {
        Value::String(s) => s.clone(),
        Value::Array(b) => b.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    if t.chars().count() > 8000 { t.chars().take(8000).collect::<String>() + "\n…" } else { t }
}

/// Where a tool call points: a file, a command, a pattern. Paths in the project are shown relative.
pub fn tool_target(i: &Value, root: &Path) -> String {
    let s = ["file_path", "notebook_path", "command", "pattern", "url", "query", "skill", "description", "path", "subject"]
        .iter()
        .find_map(|k| i[k].as_str().map(String::from))
        .or_else(|| i["questions"][0]["question"].as_str().map(String::from))
        .or_else(|| i["todos"].as_array().map(|t| format!("{} tasks", t.len())))
        .or_else(|| i["plan"].as_str().and_then(|p| p.lines().find(|l| !l.trim().is_empty())).map(String::from))
        .unwrap_or_default();
    let root = format!("{}/", root.display());
    s.strip_prefix(&root).map(String::from).unwrap_or(s).lines().next().unwrap_or("").to_string()
}

impl Log {
    /// Applies one stream-json event (or saved session record). Returns the first item index it
    /// changed (items from there on need drawing again).
    pub fn apply(&mut self, ev: &Value) -> usize {
        let len = self.items.len();
        let mut first = len;
        if !ev["parent_tool_use_id"].is_null() || flag(ev, "isSidechain") {
            // a subagent at work: its card shows how far it got
            let parent = ev["parent_tool_use_id"].as_str().unwrap_or("");
            if ev["type"] == "assistant"
                && let Some(i) = self.items.iter().position(|it| matches!(it, Item::Tool(t) if t.id == parent))
                && let Item::Tool(t) = &mut self.items[i]
            {
                for b in ev["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                    t.steps.push((b["name"].as_str().unwrap_or("").to_string(), tool_target(&b["input"], Path::new(""))));
                    first = first.min(i);
                }
            }
            return first;
        }
        let uuid = matches!(ev["type"].as_str(), Some("user" | "assistant")).then(|| ev["uuid"].as_str()).flatten().map(String::from);
        let before = self.last_uuid.clone();
        if uuid.is_some() {
            self.last_uuid = uuid.clone();
        }
        match ev["type"].as_str().unwrap_or("") {
            "system" => {
                if let Some(m) = ev["permissionMode"].as_str() {
                    self.mode = Some(m.to_string());
                }
                match ev["subtype"].as_str() {
                    Some("init") => {
                        self.session = ev["session_id"].as_str().map(String::from);
                        self.model = ev["model"].as_str().map(String::from);
                        self.base = self.cost; // a new process counts total_cost_usd from 0
                    }
                    Some("compact_boundary") => self.items.push(Item::Note("Conversation compacted".into())),
                    Some("ui_status") => self.status = ev["text"].as_str().map(String::from),
                    Some("task_started") => {
                        let job = self.job(ev);
                        job.status = "running".into();
                        job.tool_use = ev["tool_use_id"].as_str().map(String::from).or(job.tool_use.take());
                        job.agent = ev["subagent_type"].as_str().unwrap_or(&job.agent).to_string();
                    }
                    Some("task_progress") => {
                        let job = self.job(ev);
                        job.last = ev["description"].as_str().unwrap_or("").to_string();
                    }
                    Some("task_updated") => {
                        if let Some(st) = ev["patch"]["status"].as_str() {
                            self.job(ev).status = st.into();
                        }
                    }
                    Some("task_notification") => {
                        let job = self.job(ev);
                        job.status = ev["status"].as_str().unwrap_or("completed").into();
                        job.last = ev["summary"].as_str().unwrap_or("").to_string();
                    }
                    Some("background_tasks_changed") => {
                        for t in ev["tasks"].as_array().into_iter().flatten() {
                            if !self.jobs.iter().any(|j| t["task_id"] == j.id.as_str()) {
                                self.job(t).status = "running".into();
                            }
                        }
                    }
                    _ => {}
                }
            }
            "stream_event" => {
                let e = &ev["event"];
                let index = e["index"].as_u64().unwrap_or(0);
                match e["type"].as_str() {
                    Some("message_start") => {
                        self.msg = e["message"]["id"].as_str().unwrap_or("").to_string();
                        // Claude can start a turn on its own (a background job finished).
                        self.busy = true;
                    }
                    Some("content_block_start") => {
                        let kind = e["content_block"]["type"].as_str().unwrap_or("");
                        if kind == "text" || kind == "thinking" {
                            let key = self.next(&format!("{}:{kind}", self.msg));
                            self.blocks.insert(index, key.clone());
                            self.keys.insert(key, self.items.len());
                            self.items.push(if kind == "text" { Item::Text(String::new()) } else { Item::Thinking(String::new()) });
                        }
                    }
                    Some("content_block_delta") => {
                        let add = e["delta"]["text"].as_str().or(e["delta"]["thinking"].as_str());
                        if let (Some(add), Some(&i)) = (add, self.blocks.get(&index).and_then(|k| self.keys.get(k)))
                            && let Some(Item::Text(t) | Item::Thinking(t)) = self.items.get_mut(i)
                        {
                            t.push_str(add);
                            first = first.min(i);
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let m = &ev["message"];
                let id = m["id"].as_str().unwrap_or("").to_string();
                for b in m["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some(kind @ ("text" | "thinking")) => {
                            let text = b["text"].as_str().or(b["thinking"].as_str()).unwrap_or("").to_string();
                            let key = self.next(&format!("f{id}:{kind}"));
                            let key = key[1..].to_string(); // same key as the streamed block
                            if let Some(&i) = self.keys.get(&key) {
                                if !text.is_empty()
                                    && let Some(Item::Text(t) | Item::Thinking(t)) = self.items.get_mut(i)
                                {
                                    *t = text;
                                    first = first.min(i);
                                }
                            } else if !text.trim().is_empty() {
                                self.keys.insert(key, self.items.len());
                                self.items.push(if kind == "text" { Item::Text(text) } else { Item::Thinking(text) });
                            }
                        }
                        Some("tool_use") => {
                            let tid = b["id"].as_str().unwrap_or("").to_string();
                            if !self.items.iter().any(|it| matches!(it, Item::Tool(t) if t.id == tid)) {
                                self.items.push(Item::Tool(Tool { id: tid, name: b["name"].as_str().unwrap_or("").into(), input: b["input"].clone(), result: None, steps: Vec::new() }));
                            }
                        }
                        _ => {}
                    }
                }
                let u = &m["usage"];
                let used: u64 = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"].iter().filter_map(|k| u[k].as_u64()).sum();
                if used > 0 {
                    self.context = Some((used, self.context.map_or(200_000, |c| c.1)));
                }
            }
            "user" => {
                let content = &ev["message"]["content"];
                if content.as_array().is_some_and(|b| b.iter().any(|b| b["type"] == "tool_result")) {
                    for b in content.as_array().into_iter().flatten() {
                        let tid = b["tool_use_id"].as_str().unwrap_or("");
                        if let Some(i) = self.items.iter().position(|it| matches!(it, Item::Tool(t) if t.id == tid))
                            && let Item::Tool(t) = &mut self.items[i]
                        {
                            t.result = Some((flag(b, "is_error"), result_text(&b["content"])));
                            first = first.min(i);
                        }
                    }
                } else if flag(ev, "isReplay") {
                    // Our own message coming back: it's shown already, now it has its id.
                    if let Some(i) = self.items.iter().rposition(|it| matches!(it, Item::User { uuid: None, .. }))
                        && let Item::User { uuid: u, .. } = &mut self.items[i]
                    {
                        *u = uuid;
                    }
                } else if is_prompt(ev)
                    && let Some(text) = prompt_text(content)
                {
                    let blocks = content.as_array();
                    let images = blocks.map_or(0, |b| b.iter().filter(|b| b["type"] == "image").count());
                    let context = blocks.into_iter().flatten().filter_map(|b| b["text"].as_str()).filter(|t| is_ide_block(t)).map(String::from).collect();
                    self.items.push(Item::User { text, images, context, uuid, before });
                }
            }
            // Claude Code withdrew a question (the turn was interrupted).
            "control_cancel_request" => {
                if let Some(i) = ev["request_id"].as_str().and_then(|id| self.answer(id, Answer::Deny)) {
                    first = first.min(i);
                }
            }
            "control_request" if ev["request"]["subtype"] == "can_use_tool" => {
                let r = &ev["request"];
                self.items.push(Item::Ask(Ask {
                    id: ev["request_id"].as_str().unwrap_or("").into(),
                    tool: r["tool_name"].as_str().unwrap_or("").into(),
                    input: r["input"].clone(),
                    suggestions: r["permission_suggestions"].clone(),
                    answer: None,
                }));
            }
            "result" => {
                self.busy = false;
                first = first.min(self.close_asks());
                let windows = ev["modelUsage"].as_object().into_iter().flat_map(|m| m.values()).filter_map(|u| u["contextWindow"].as_u64()).max();
                if let (Some(w), Some(c)) = (windows, self.context.as_mut()) {
                    c.1 = w;
                }
                let stopped = ev["subtype"] == "error_during_execution";
                if flag(ev, "is_error") && !stopped {
                    let errors = ev["errors"].as_array().map(|e| e.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"));
                    self.items.push(Item::Error(ev["result"].as_str().map(String::from).or(errors).unwrap_or_else(|| ev["subtype"].to_string())));
                }
                let cost = ev["total_cost_usd"].as_f64().unwrap_or(0.);
                let parts: Vec<String> = [
                    stopped.then(|| "Stopped".to_string()),
                    ev["duration_ms"].as_f64().filter(|d| *d > 0.).map(|d| format!("{:.1} s", d / 1000.)),
                    (cost > 0.).then(|| format!("${cost:.3}")),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !parts.is_empty() {
                    self.items.push(Item::Note(parts.join(" · ")));
                }
                self.cost = self.base + cost;
            }
            "exit" => {
                let stderr = ev["stderr"].as_str().unwrap_or("");
                if self.busy || !stderr.is_empty() {
                    self.items.push(Item::Error(format!("Claude Code stopped.{}", if stderr.is_empty() { String::new() } else { format!("\n{stderr}") })));
                }
                self.busy = false;
                first = first.min(self.close_asks());
                // Its jobs ended with it.
                for j in self.jobs.iter_mut().filter(|j| j.running()) {
                    j.status = "stopped".into();
                }
            }
            _ => {}
        }
        first
    }

    /// The job an event is about, added if new.
    fn job(&mut self, ev: &Value) -> &mut Job {
        let id = ev["task_id"].as_str().unwrap_or("").to_string();
        let i = match self.jobs.iter().position(|j| j.id == id) {
            Some(i) => i,
            None => {
                self.jobs.push(Job {
                    id,
                    kind: ev["task_type"].as_str().unwrap_or("").into(),
                    what: ev["description"].as_str().unwrap_or("").into(),
                    agent: ev["subagent_type"].as_str().unwrap_or("").into(),
                    parent: ev["parent_task_id"].as_str().map(String::from),
                    tool_use: ev["tool_use_id"].as_str().map(String::from),
                    status: "running".into(),
                    last: String::new(),
                });
                self.jobs.len() - 1
            }
        };
        &mut self.jobs[i]
    }

    /// The job a tool call started.
    pub fn job_of(&self, tool_use: &str) -> Option<&Job> {
        self.jobs.iter().find(|j| j.tool_use.as_deref() == Some(tool_use))
    }

    fn next(&mut self, prefix: &str) -> String {
        let n = self.counts.entry(prefix.to_string()).or_default();
        *n += 1;
        format!("{prefix}:{}", *n - 1)
    }

    /// Questions still open are dropped (their turn or process ended); the first one's index.
    pub fn close_asks(&mut self) -> usize {
        let mut first = self.items.len();
        for (i, it) in self.items.iter_mut().enumerate() {
            if let Item::Ask(q) = it
                && q.answer.is_none()
            {
                q.answer = Some(Answer::Deny);
                first = first.min(i);
            }
        }
        first
    }

    /// Marks a permission card answered.
    pub fn answer(&mut self, id: &str, a: Answer) -> Option<usize> {
        let i = self.items.iter().position(|it| matches!(it, Item::Ask(q) if q.id == id))?;
        if let Item::Ask(q) = &mut self.items[i] {
            q.answer = Some(a);
        }
        Some(i)
    }

    /// Cuts the conversation back to just before item `ix` (a user message).
    pub fn truncate(&mut self, ix: usize) {
        self.items.truncate(ix);
        self.keys.retain(|_, i| *i < ix);
        self.busy = false;
    }

    /// Claude's task list as it stands: the last TodoWrite, or TaskCreate / TaskUpdate calls.
    /// (text, status, doing)
    pub fn tasks(&self) -> Vec<(String, String, String)> {
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let tools = self.items.iter().filter_map(|it| if let Item::Tool(t) = it { Some(t) } else { None });
        if let Some(t) = tools.clone().filter(|t| t.name == "TodoWrite").last() {
            return t.input["todos"].as_array().into_iter().flatten().map(|t| (s(&t["content"]), s(&t["status"]), s(&t["activeForm"]))).collect();
        }
        let mut out: Vec<(String, (String, String, String))> = Vec::new();
        for t in tools {
            if t.name == "TaskCreate" {
                let id = t.result.as_ref().and_then(|r| between(&r.1, "#", " ")).map(String::from).unwrap_or_else(|| t.id.clone());
                let text = t.input["subject"].as_str().or(t.input["description"].as_str()).unwrap_or("").to_string();
                out.push((id, (text, "pending".into(), s(&t.input["activeForm"]))));
            } else if t.name == "TaskUpdate" {
                let id = t.input["taskId"].as_str().map(String::from).unwrap_or_else(|| t.input["taskId"].to_string());
                if t.input["status"] == "deleted" {
                    out.retain(|(k, _)| *k != id);
                } else if let Some((_, task)) = out.iter_mut().find(|(k, _)| *k == id) {
                    if let Some(st) = t.input["status"].as_str() {
                        task.1 = st.into();
                    }
                    if let Some(sub) = t.input["subject"].as_str() {
                        task.0 = sub.into();
                    }
                }
            }
        }
        out.into_iter().map(|(_, t)| t).collect()
    }

    /// The conversation as Markdown: what was said, and tool calls as one line each.
    pub fn markdown(&self, title: &str, root: &Path) -> String {
        let mut out = vec![format!("# {title}"), String::new(), "_Claude Code conversation, exported from OSKL Code._".into()];
        let mut who = "";
        for it in &self.items {
            let (speaker, line) = match it {
                Item::User { text, .. } => ("You", text.trim().to_string()),
                Item::Text(t) => ("Claude", t.trim().to_string()),
                Item::Tool(t) => ("Claude", format!("- {} `{}`{}", t.name, tool_target(&t.input, root), if t.result.as_ref().is_some_and(|r| r.0) { " (failed)" } else { "" })),
                _ => continue,
            };
            if speaker != who {
                out.extend([String::new(), format!("## {speaker}")]);
            }
            if !line.starts_with("- ") || !out.last().is_some_and(|l| l.starts_with("- ")) {
                out.push(String::new());
            }
            who = speaker;
            out.push(line);
        }
        out.join("\n") + "\n"
    }
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

/// One session's title (without listing the others).
pub fn session_title(dir: &Path, id: &str) -> Option<String> {
    is_uuid(id).then(|| std::fs::read_to_string(dir.join(format!("{id}.jsonl"))).ok()).flatten().map(|t| summarize(&t).0)
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

/// A subagent's id, from its tool result ("agentId: …"), to continue it when its job isn't known (an older session).
pub fn agent_id(result: &str) -> Option<String> {
    let rest = result.split_once("agentId: ")?.1;
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (!id.is_empty()).then_some(id)
}

/// A saved session replayed into a chat log.
pub fn load_session(dir: &Path, id: &str) -> Result<Log, String> {
    if !is_uuid(id) {
        return Err(format!("Bad session id: {id}"));
    }
    let text = std::fs::read_to_string(dir.join(format!("{id}.jsonl"))).map_err(|e| format!("Session {id}: {e}"))?;
    let mut log = Log { session: Some(id.into()), ..Default::default() };
    for r in text.lines().filter(|l| l.contains("\"user\"") || l.contains("\"assistant\"") || l.contains("compact_boundary")).filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if !flag(&r, "isMeta") && !flag(&r, "isCompactSummary") {
            log.apply(&r);
        }
    }
    log.busy = false;
    Ok(log)
}

/// Names a session the way Claude Code's /rename does: a custom-title record at the end of its file.
pub fn rename_session(dir: &Path, id: &str, title: &str) -> Result<(), String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if !is_uuid(id) || title.is_empty() || title.chars().count() > 200 {
        return Err("Bad session id or title".into());
    }
    let mut f = std::fs::OpenOptions::new().append(true).open(dir.join(format!("{id}.jsonl"))).map_err(|e| format!("Session {id}: {e}"))?;
    writeln!(f, "{}", json!({ "type": "custom-title", "customTitle": title, "sessionId": id })).map_err(|e| format!("Session {id}: {e}"))
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

    fn ev(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn base64_and_ids() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert!(is_uuid(ID) && !is_uuid("../../etc/passwd") && !is_uuid(&format!("{ID}.jsonl")));
        assert_eq!(project_dir_name(Path::new("/Users/me/.cloudflared/a_b")), "-Users-me--cloudflared-a-b");
        assert_eq!(project_dir_name(Path::new("/x/é😀")), "-x----");
    }

    #[test]
    fn streamed_text_is_replaced_by_the_final_message_and_tools_get_results() {
        let mut log = Log::default();
        log.apply(&ev(r#"{"type":"system","subtype":"init","session_id":"s1","model":"m","permissionMode":"default"}"#));
        log.items.push(Item::User { text: "hi".into(), images: 0, context: vec![], uuid: None, before: None });
        log.apply(&ev(r#"{"type":"user","isReplay":true,"uuid":"u1","message":{"content":[{"type":"text","text":"hi"}]}}"#));
        assert!(matches!(&log.items[0], Item::User { uuid: Some(u), .. } if u == "u1"));
        log.apply(&ev(r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#));
        log.apply(&ev(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}}}"#));
        assert_eq!(log.apply(&ev(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"text":"Hel"}}}"#)), 1);
        assert_eq!(log.items[1], Item::Text("Hel".into()));
        log.apply(&ev(r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/p/src/a.rs"}}],"usage":{"input_tokens":10,"output_tokens":5}}}"#));
        assert_eq!(log.items.len(), 3);
        assert_eq!(log.items[1], Item::Text("Hello".into()));
        assert_eq!(log.context, Some((15, 200_000)));
        log.apply(&ev(r#"{"type":"user","uuid":"r1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main","is_error":false}]}}"#));
        assert!(matches!(&log.items[2], Item::Tool(t) if t.result == Some((false, "fn main".into()))));
        assert_eq!(tool_target(&json!({"file_path":"/p/src/a.rs"}), Path::new("/p")), "src/a.rs");
        log.apply(&ev(r#"{"type":"control_request","request_id":"q1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}"#));
        assert_eq!(log.answer("q1", Answer::Allow), Some(3));
        log.busy = true;
        log.apply(&ev(r#"{"type":"result","subtype":"success","duration_ms":1500,"total_cost_usd":0.01,"modelUsage":{"m":{"contextWindow":1000000}}}"#));
        assert!(!log.busy);
        assert_eq!(log.items.last(), Some(&Item::Note("1.5 s · $0.010".into())));
        assert_eq!(log.context, Some((15, 1_000_000)));
        assert_eq!(log.session.as_deref(), Some("s1"));
        assert!(log.markdown("T", Path::new("/p")).contains("- Read `src/a.rs`"));
    }

    /// Runs the real `claude` (spends a few tokens): `cargo test live_turn -- --ignored`.
    #[test]
    #[ignore]
    fn live_turn() {
        let dir = std::env::temp_dir();
        let opts = Opts { model: "haiku".into(), effort: "low".into(), style: "Concise".into(), ..Default::default() };
        let (mut p, rx) = start(&dir, &opts).unwrap();
        p.send(&control("i", json!({ "subtype": "initialize" }))).unwrap();
        p.send(&user_message("Reply with just: pong", &[], &[])).unwrap();
        let mut log = Log::default();
        log.items.push(Item::User { text: "Reply with just: pong".into(), images: 0, context: vec![], uuid: None, before: None });
        log.busy = true;
        let mut rx = futures::executor::block_on_stream(rx);
        while log.busy {
            let ev = rx.next().expect("claude ended early");
            log.apply(&ev);
        }
        assert!(log.session.is_some());
        assert!(matches!(&log.items[0], Item::User { uuid: Some(_), .. }), "replayed message got its id");
        assert!(log.items.iter().any(|i| matches!(i, Item::Text(t) if t.to_lowercase().contains("pong"))), "{:?}", log.items);
    }

    /// The pickers' probe (runs the real `claude`): `cargo test live_offers -- --ignored`.
    #[test]
    #[ignore]
    fn live_offers() {
        let offers = offers().unwrap();
        assert!(offers["models"].as_array().is_some_and(|m| !m.is_empty()), "{offers}");
    }

    /// Stopping Claude Code ends what it started too (its process group), without waiting.
    #[test]
    fn stopping_ends_the_whole_process_group() {
        let mut cmd = std::process::Command::new("/bin/sh");
        let mut child = std::os::unix::process::CommandExt::process_group(&mut cmd, 0).args(["-c", "sleep 30 & wait"]).stdin(Stdio::piped()).spawn().unwrap();
        let group = child.id().to_string();
        let alive = || std::process::Command::new("/usr/bin/pgrep").args(["-g", &group]).output().is_ok_and(|o| !o.stdout.is_empty());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(alive());
        let stdin = child.stdin.take().unwrap();
        let t = std::time::Instant::now();
        drop(Proc { child: Some(child), stdin });
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "dropping doesn't wait on the caller's thread");
        let t = std::time::Instant::now();
        while alive() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!alive(), "the shell and its sleep are gone");
    }

    #[test]
    fn subagent_steps_land_on_their_tool() {
        let mut log = Log::default();
        log.apply(&ev(r#"{"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"Task","input":{"description":"Find it"}}]}}"#));
        log.apply(&ev(r#"{"type":"assistant","parent_tool_use_id":"t","message":{"id":"s","content":[{"type":"tool_use","id":"x","name":"Grep","input":{"pattern":"foo"}}]}}"#));
        assert!(matches!(&log.items[0], Item::Tool(t) if t.steps == vec![("Grep".to_string(), "foo".to_string())]));
        assert_eq!(log.items.len(), 1);
    }

    #[test]
    fn jobs_follow_task_events() {
        let mut log = Log::default();
        log.apply(&ev(r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"Sleep"}]}"#));
        log.apply(&ev(r#"{"type":"system","subtype":"task_started","task_id":"a1","tool_use_id":"t9","task_type":"local_agent","subagent_type":"Explore","description":"Look"}"#));
        log.apply(&ev(r#"{"type":"system","subtype":"task_started","task_id":"b2","parent_task_id":"a1","task_type":"local_bash","description":"grep"}"#));
        log.apply(&ev(r#"{"type":"system","subtype":"task_progress","task_id":"a1","description":"Running grep"}"#));
        log.apply(&ev(r#"{"type":"system","subtype":"task_updated","task_id":"b1","patch":{"status":"killed"}}"#));
        assert_eq!(log.jobs.len(), 3);
        assert_eq!(log.jobs[0].status, "killed");
        let a = log.job_of("t9").unwrap();
        assert!(a.is_agent() && a.running() && a.agent == "Explore" && a.last == "Running grep");
        assert_eq!(log.jobs[2].parent.as_deref(), Some("a1"));
        log.apply(&ev(r#"{"type":"system","subtype":"task_notification","task_id":"a1","status":"completed","summary":"Found it"}"#));
        assert_eq!((log.jobs[1].status.as_str(), log.jobs[1].last.as_str()), ("completed", "Found it"));
        log.apply(&ev(r#"{"type":"exit","stderr":""}"#));
        assert_eq!(log.jobs[2].status, "stopped");
        assert_eq!(agent_id("Async agent launched.\nagentId: ac27a20c0ed50399d (internal ID"), Some("ac27a20c0ed50399d".into()));
    }

    #[test]
    fn tasks_follow_todo_write() {
        let mut log = Log::default();
        log.apply(&ev(r#"{"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"TodoWrite","input":{"todos":[{"content":"A","status":"completed","activeForm":"Doing A"},{"content":"B","status":"in_progress","activeForm":"Doing B"}]}}]}}"#));
        assert_eq!(log.tasks(), vec![("A".into(), "completed".into(), "Doing A".into()), ("B".into(), "in_progress".into(), "Doing B".into())]);
    }

    #[test]
    fn sessions_list_load_rename_and_trash() {
        let dir = std::env::temp_dir().join(format!("kite-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = [
            r#"{"type":"user","uuid":"u1","message":{"role":"user","content":[{"type":"text","text":"<ide_opened_file>x</ide_opened_file>"},{"type":"text","text":"Fix the   login\nbug"}]}}"#,
            r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"text","text":"Done"}]}}"#,
            r#"{"type":"user","isMeta":true,"uuid":"u2","message":{"role":"user","content":"meta"}}"#,
        ]
        .join("\n")
            + "\n";
        std::fs::write(dir.join(format!("{ID}.jsonl")), &text).unwrap();
        std::fs::write(dir.join("notes.jsonl"), &text).unwrap();
        let list = list_sessions(&dir, "");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Fix the login bug");
        assert_eq!(list_sessions(&dir, "DONE").len(), 1);
        assert!(list_sessions(&dir, "nowhere").is_empty());
        let log = load_session(&dir, ID).unwrap();
        assert_eq!(log.items.len(), 2);
        assert!(matches!(&log.items[0], Item::User { text, context, uuid: Some(_), .. } if text == "Fix the   login\nbug" && context.len() == 1));
        assert!(load_session(&dir, "../x").is_err() && trash_session(&dir, "../x").is_err());
        rename_session(&dir, ID, " Login  ").unwrap();
        assert_eq!(list_sessions(&dir, "")[0].title, "Login");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
