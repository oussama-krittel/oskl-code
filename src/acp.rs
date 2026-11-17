//! Agent chat over the Agent Client Protocol (agentclientprotocol.com): runs an ACP agent
//! (Claude Code, Codex, Gemini…) in the project, talks JSON-RPC with it on a background thread,
//! and turns what it sends into chat items.

use agent_client_protocol::schema::{MaybeUndefined, ProtocolVersion};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::*;
use agent_client_protocol::{Agent, Client, Lines, Responder, V1};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{SinkExt, StreamExt};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Stdio};

// ---------------------------------------------------------------- agents

/// An agent the chat can run: the command that starts it speaking ACP on stdio.
pub struct AgentSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub cmd: &'static [&'static str],
}

/// From the ACP registry (cdn.agentclientprotocol.com/registry/v1/latest/registry.json).
/// shortcut: a fixed list, no custom commands; read the registry or a setting when someone needs another agent.
pub const AGENTS: &[AgentSpec] = &[
    AgentSpec { id: "claude", name: "Claude Code", cmd: &["npx", "-y", "@agentclientprotocol/claude-agent-acp"] },
    AgentSpec { id: "codex", name: "Codex", cmd: &["npx", "-y", "@agentclientprotocol/codex-acp"] },
    AgentSpec { id: "gemini", name: "Gemini CLI", cmd: &["npx", "-y", "@google/gemini-cli", "--acp"] },
    AgentSpec { id: "copilot", name: "GitHub Copilot", cmd: &["npx", "-y", "@github/copilot", "--acp"] },
    AgentSpec { id: "opencode", name: "OpenCode", cmd: &["opencode", "acp"] },
    AgentSpec { id: "goose", name: "Goose", cmd: &["goose", "acp"] },
    AgentSpec { id: "cursor", name: "Cursor", cmd: &["cursor", "acp"] },
];

pub fn agent(id: &str) -> &'static AgentSpec {
    AGENTS.iter().find(|a| a.id == id).unwrap_or(&AGENTS[0])
}

// ---------------------------------------------------------------- process

/// A process and everything it started (its process group); dropping it stops them all: asked to
/// stop first, so it cleans up; killed if still there after 2 s. Off the main thread.
pub struct Group(pub Option<Child>);

impl Drop for Group {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
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

/// What the agent sends that the chat must handle. Requests carry their responder: answer them on
/// the main thread, whenever ready.
pub enum Event {
    Update(SessionNotification),
    Permission(RequestPermissionRequest, Responder<RequestPermissionResponse, V1>),
    Read(ReadTextFileRequest, Responder<ReadTextFileResponse, V1>),
    Write(WriteTextFileRequest, Responder<WriteTextFileResponse, V1>),
    /// The agent stopped: why, and the end of what it printed on stderr.
    Exit(String),
}

/// A running agent, initialized. Dropping it stops the agent.
pub struct Conn {
    pub to: ConnectionTo<Agent>,
    pub init: InitializeResponse,
    _group: Group,
}

impl Conn {
    /// Sends a request and waits for its answer (not from inside a handler: there it deadlocks).
    pub async fn ask<R: agent_client_protocol::JsonRpcRequest>(&self, req: R) -> Result<R::Response, String> {
        self.to.send_request(req).block_task().await.map_err(|e| e.to_string())
    }

    pub fn tell<N: agent_client_protocol::JsonRpcNotification>(&self, n: N) -> Result<(), String> {
        self.to.send_notification(n).map_err(|e| e.to_string())
    }
}

/// Starts `agent` in `cwd` and initializes it. Blocks until the agent answers (call it off the
/// main thread); what it sends from then on arrives on the channel, ending with `Event::Exit`.
pub fn start(agent: &AgentSpec, cwd: &Path) -> Result<(Conn, UnboundedReceiver<Event>), String> {
    let program = crate::tools::search_path()
        .into_iter()
        .map(|d| d.join(agent.cmd[0]))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("{} needs `{}`, and it isn't on your shell's PATH.", agent.name, agent.cmd[0]))?;
    let mut child = std::os::unix::process::CommandExt::process_group(crate::tools::command(&program).args(&agent.cmd[1..]), 0)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {e}", agent.name))?;
    let (mut stdin, stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let group = Group(Some(child));

    // Plain threads move the lines; the SDK only sees a Sink and a Stream.
    // Only the end of stderr is kept (agents log a lot over a long session), read when the agent
    // stops: without waiting for stderr to close, which a process it started may hold open.
    let errors = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<String>::new()));
    let tail = errors.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let mut t = tail.lock().unwrap();
            if t.len() == 40 {
                t.pop_front();
            }
            t.push_back(line);
        }
    });
    let (in_tx, in_rx) = unbounded::<std::io::Result<String>>();
    let (closed_tx, closed_rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if in_tx.unbounded_send(line).is_err() {
                break;
            }
        }
        drop(closed_tx);
    });
    let (out_tx, mut out_rx) = unbounded::<String>();
    std::thread::spawn(move || {
        while let Some(line) = futures::executor::block_on(out_rx.next()) {
            if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
                return;
            }
        }
    });
    let transport = Lines::new(out_tx.sink_map_err(std::io::Error::other), in_rx);

    let (tx, rx) = unbounded::<Event>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<(ConnectionTo<Agent>, InitializeResponse)>();
    std::thread::spawn(move || {
        let send = |tx: &UnboundedSender<Event>, ev| {
            let _ = tx.unbounded_send(ev);
        };
        let (t1, t2, t3, t4) = (tx.clone(), tx.clone(), tx.clone(), tx.clone());
        let run = Client
            .v1()
            .on_receive_notification(
                async move |n: SessionNotification, _| {
                    send(&t1, Event::Update(n));
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |r: RequestPermissionRequest, answer, _| {
                    send(&t2, Event::Permission(r, answer));
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |r: ReadTextFileRequest, answer, _| {
                    send(&t3, Event::Read(r, answer));
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |r: WriteTextFileRequest, answer, _| {
                    send(&t4, Event::Write(r, answer));
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(transport, async move |to: ConnectionTo<Agent>| {
                let caps = ClientCapabilities::new().fs(FileSystemCapabilities::new().read_text_file(true).write_text_file(true));
                let info = Implementation::new("oskl", env!("CARGO_PKG_VERSION")).title("OSKL Code".to_string());
                let init = to.send_request(InitializeRequest::new(ProtocolVersion::V1).client_capabilities(caps).client_info(info)).block_task().await?;
                let _ = ready_tx.send((to, init));
                let _ = closed_rx.await; // until the agent's stdout closes
                Ok(())
            });
        let why = match futures::executor::block_on(run) {
            Ok(()) => "The agent stopped.".to_string(),
            Err(e) => e.to_string(),
        };
        std::thread::sleep(std::time::Duration::from_millis(100)); // its last words reach the tail
        let stderr = Vec::from(errors.lock().unwrap().clone()).join("\n").trim().to_string();
        send(&tx, Event::Exit(if stderr.is_empty() { why } else { format!("{why}\n{stderr}") }));
    });
    // An agent that never answers (npx stuck on the network) is given up on, and stopped.
    match ready_rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok((to, init)) => Ok((Conn { to, init, _group: group }, rx)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!("{} didn't answer within 2 minutes.", agent.name)),
        // It ended before answering: stopped for sure, then why (what it printed).
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            drop(group);
            let why = futures::executor::block_on_stream(rx).find_map(|ev| if let Event::Exit(why) = ev { Some(why) } else { None });
            Err(format!("{} couldn't start. {}", agent.name, why.unwrap_or_default()))
        }
    }
}

/// A picture in the chat: sent to the agent, and drawn in the message.
pub type Picture = std::sync::Arc<gpui::Image>;

/// The media type a picture is sent as (agents read PNG, JPEG, GIF and WebP).
pub fn media_type(f: gpui::ImageFormat) -> Option<&'static str> {
    use gpui::ImageFormat::*;
    match f {
        Png => Some("image/png"),
        Jpeg => Some("image/jpeg"),
        Gif => Some("image/gif"),
        Webp => Some("image/webp"),
        _ => None,
    }
}

/// A picture of a file extension or media type ("png", "image/png") agents read.
pub fn picture(kind: &str, bytes: Vec<u8>) -> Option<Picture> {
    use gpui::ImageFormat::*;
    let format = match kind.to_ascii_lowercase().trim_start_matches("image/") {
        "png" => Png,
        "jpg" | "jpeg" => Jpeg,
        "gif" => Gif,
        "webp" => Webp,
        _ => return None,
    };
    Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
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

/// Text blocks the editor adds around a message (what's open, what's selected).
pub fn is_ide_block(t: &str) -> bool {
    t.starts_with("<ide_selection>") || t.starts_with("<ide_opened_file>")
}

/// Seconds since the epoch of an ISO 8601 UTC time ("2026-11-12T21:00:09.879Z"), as agents date sessions.
pub fn unix_time(iso: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// A prompt: pictures first, then the context blocks (what's open or selected), then the text.
pub fn prompt(text: &str, context: &[String], images: &[Picture]) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = images.iter().filter_map(|i| Some(ContentBlock::Image(ImageContent::new(base64(&i.bytes), media_type(i.format)?)))).collect();
    out.extend(context.iter().map(|c| ContentBlock::Text(TextContent::new(c.clone()))));
    if !text.is_empty() {
        out.push(ContentBlock::Text(TextContent::new(text.to_string())));
    }
    out
}

// ---------------------------------------------------------------- chat log

/// The agent asking before it runs a tool: the options it offers, and what the user chose.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub tool_call_id: ToolCallId,
    pub title: String,
    pub options: Vec<PermissionOption>,
    /// The chosen option's name, or "Cancelled".
    pub answer: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    User { text: String, images: Vec<Picture>, context: Vec<String> },
    Text(String),
    Thinking(String),
    Tool(ToolCall),
    Ask(Ask),
    Error(String),
    /// A quiet line: how a turn ended, a compaction, a notice.
    Note(String),
}

/// A plan limit the agent reports (Claude's 5-hour and weekly windows): name, used 0–1, resets at (unix s).
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub name: String,
    pub used: f64,
    pub resets_at: Option<i64>,
}

#[derive(Default)]
pub struct Log {
    pub items: Vec<Item>,
    pub busy: bool,
    pub title: Option<String>,
    pub plan: Vec<PlanEntry>,
    pub commands: Vec<AvailableCommand>,
    /// Mode, model, thinking… (the agent's own pickers).
    pub config: Vec<SessionConfigOption>,
    /// Older agents' modes, when they send no `config`.
    pub modes: Option<SessionModeState>,
    /// Tokens in the context, the window, and the session's cost.
    pub usage: Option<UsageUpdate>,
    pub limits: Vec<Limit>,
}

/// What a content block shows as text; None for pictures and sounds.
fn block_text(b: &ContentBlock) -> Option<String> {
    match b {
        ContentBlock::Text(t) => Some(t.text.clone()),
        ContentBlock::ResourceLink(l) => Some(format!("@{}", l.name)),
        ContentBlock::Resource(r) => match &r.resource {
            EmbeddedResourceResource::TextResourceContents(t) => Some(format!("@{}", t.uri.rsplit('/').next().unwrap_or(&t.uri))),
            _ => None,
        },
        _ => None,
    }
}

impl Log {
    /// Applies one update. Returns the first item index it changed (items from there on need
    /// drawing again).
    pub fn apply(&mut self, u: SessionUpdate) -> usize {
        let len = self.items.len();
        match u {
            SessionUpdate::UserMessageChunk(c) => {
                // Replayed history (session/load): a message comes in several chunks.
                if !matches!(self.items.last(), Some(Item::User { .. })) {
                    self.items.push(Item::User { text: String::new(), images: Vec::new(), context: Vec::new() });
                }
                let Some(Item::User { text, images, context }) = self.items.last_mut() else { unreachable!() };
                match (&c.content, block_text(&c.content)) {
                    (_, Some(t)) if is_ide_block(&t) => context.push(t),
                    (_, Some(t)) => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&t);
                    }
                    (ContentBlock::Image(i), None) => images.extend(picture(&i.mime_type, crate::api::unbase64(&i.data))),
                    _ => {}
                }
                return self.items.len() - 1;
            }
            SessionUpdate::AgentMessageChunk(c) | SessionUpdate::AgentThoughtChunk(c) if block_text(&c.content).is_none() => {}
            SessionUpdate::AgentMessageChunk(c) => return self.append(false, block_text(&c.content).unwrap_or_default()),
            SessionUpdate::AgentThoughtChunk(c) => return self.append(true, block_text(&c.content).unwrap_or_default()),
            SessionUpdate::ToolCall(t) => {
                if let Some(i) = self.tool(&t.tool_call_id) {
                    self.items[i] = Item::Tool(t);
                    return i;
                }
                self.items.push(Item::Tool(t));
            }
            SessionUpdate::ToolCallUpdate(u) => {
                if let Some(i) = self.tool(&u.tool_call_id) {
                    if let Item::Tool(t) = &mut self.items[i] {
                        t.update(u.fields);
                    }
                    return i;
                }
                // An update for a call we never saw (a replay can start mid-way): it becomes the call.
                if let Ok(t) = ToolCall::try_from(u) {
                    self.items.push(Item::Tool(t));
                }
            }
            SessionUpdate::Plan(p) => self.plan = p.entries,
            SessionUpdate::AvailableCommandsUpdate(c) => self.commands = c.available_commands,
            SessionUpdate::CurrentModeUpdate(m) => {
                if let Some(modes) = &mut self.modes {
                    modes.current_mode_id = m.current_mode_id;
                }
            }
            SessionUpdate::ConfigOptionUpdate(c) => self.config = c.config_options,
            SessionUpdate::SessionInfoUpdate(s) => {
                if let MaybeUndefined::Value(t) = s.title {
                    self.title = Some(t);
                }
            }
            SessionUpdate::UsageUpdate(mut u) => {
                if let Some(limits) = u.meta.as_ref().and_then(claude_limits) {
                    self.limits = limits;
                }
                // Cost comes only now and then: keep the last one.
                if u.cost.is_none() {
                    u.cost = self.usage.take().and_then(|p| p.cost);
                }
                self.usage = Some(u);
            }
            SessionUpdate::Notice(n) => self.items.push(Item::Note(n.description.map_or(n.title.clone(), |d| format!("{}: {d}", n.title)))),
            SessionUpdate::CompactionUpdate(c) if c.status == CompactionStatus::Completed => self.items.push(Item::Note("Conversation compacted".into())),
            _ => {}
        }
        len
    }

    /// Adds streamed text to the message (or thinking) being written, or starts one.
    fn append(&mut self, thinking: bool, add: String) -> usize {
        match self.items.last_mut() {
            Some(Item::Thinking(t)) if thinking => t.push_str(&add),
            Some(Item::Text(t)) if !thinking => t.push_str(&add),
            _ => self.items.push(if thinking { Item::Thinking(add) } else { Item::Text(add) }),
        }
        self.items.len() - 1
    }

    fn tool(&self, id: &ToolCallId) -> Option<usize> {
        self.items.iter().rposition(|it| matches!(it, Item::Tool(t) if t.tool_call_id == *id))
    }

    /// A permission card; its index is the request's id until answered.
    pub fn ask(&mut self, r: &RequestPermissionRequest) -> usize {
        let title = r.tool_call.fields.title.clone().or_else(|| {
            self.tool(&r.tool_call.tool_call_id).and_then(|i| if let Item::Tool(t) = &self.items[i] { Some(t.title.clone()) } else { None })
        });
        self.items.push(Item::Ask(Ask { tool_call_id: r.tool_call.tool_call_id.clone(), title: title.unwrap_or_default(), options: r.options.clone(), answer: None }));
        self.items.len() - 1
    }

    /// Marks a permission card answered.
    pub fn answer(&mut self, ix: usize, answer: String) {
        if let Some(Item::Ask(q)) = self.items.get_mut(ix) {
            q.answer = Some(answer);
        }
    }

    /// The turn ended (the prompt's answer, or an error).
    pub fn finish(&mut self, r: Result<StopReason, String>) {
        self.busy = false;
        match r {
            Ok(StopReason::EndTurn) => {}
            Ok(StopReason::Cancelled) => self.items.push(Item::Note("Stopped".into())),
            Ok(StopReason::MaxTokens) => self.items.push(Item::Error("The reply hit the output token limit.".into())),
            Ok(StopReason::MaxTurnRequests) => self.items.push(Item::Error("The agent hit its turn limit.".into())),
            Ok(StopReason::Refusal) => self.items.push(Item::Error("The agent refused to continue.".into())),
            Ok(other) => self.items.push(Item::Note(format!("{other:?}"))),
            Err(e) => self.items.push(Item::Error(e)),
        }
    }

    /// The current value of a config option of this category (mode, model…).
    pub fn option(&self, category: SessionConfigOptionCategory) -> Option<&SessionConfigOption> {
        self.config.iter().find(|o| o.category.as_ref() == Some(&category))
    }

    /// The conversation as Markdown: what was said, and tool calls as one line each.
    pub fn markdown(&self, title: &str, agent: &str) -> String {
        let mut out = vec![format!("# {title}"), String::new(), format!("_{agent} conversation, exported from OSKL Code._")];
        let mut who = "";
        for it in &self.items {
            let (speaker, line) = match it {
                Item::User { text, .. } => ("You", text.trim().to_string()),
                Item::Text(t) => (agent, t.trim().to_string()),
                Item::Tool(t) => (agent, format!("- {}{}", t.title, if t.status == ToolCallStatus::Failed { " (failed)" } else { "" })),
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

/// Claude's plan limits, from the extra fields its adapter puts on a usage update.
fn claude_limits(meta: &Meta) -> Option<Vec<Limit>> {
    let windows = meta.get("_claude/rateLimit")?.get("unifiedWindows")?.as_object()?;
    let name = |k: &str| match k {
        "five_hour" => "5h".to_string(),
        "seven_day" => "Week".to_string(),
        k => k.replace('_', " "),
    };
    Some(windows.iter().map(|(k, w)| Limit { name: name(k), used: w["utilization"].as_f64().unwrap_or(0.), resets_at: w["resetsAt"].as_i64() }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(log: &mut Log, lines: &[&str]) {
        for l in lines {
            log.apply(serde_json::from_str::<SessionUpdate>(l).unwrap());
        }
    }

    /// Updates as claude-agent-acp 0.106 sent them (trimmed).
    #[test]
    fn claude_turn() {
        let mut log = Log::default();
        feed(
            &mut log,
            &[
                r#"{"sessionUpdate":"session_info_update","title":"Agent.rs first line"}"#,
                r#"{"sessionUpdate":"usage_update","used":27494,"size":1000000,"_meta":{"_claude/rateLimit":{"unifiedWindows":{"five_hour":{"utilization":0.02,"resetsAt":1794533400},"seven_day":{"utilization":0.36,"resetsAt":1794704400}}}}}"#,
                r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Let me "}}"#,
                r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"read it."}}"#,
                r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Read File","kind":"read","rawInput":{}}"#,
                r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","title":"Read src/agent.rs","locations":[{"path":"/p/src/agent.rs","line":1}]}"#,
                r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"line one"}}]}"#,
                r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The package "}}"#,
                r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"is **oskl**."}}"#,
                r#"{"sessionUpdate":"usage_update","used":30603,"size":1000000,"cost":{"amount":0.15,"currency":"USD"}}"#,
                r#"{"sessionUpdate":"usage_update","used":30700,"size":1000000}"#,
            ],
        );
        assert_eq!(log.title.as_deref(), Some("Agent.rs first line"));
        assert_eq!(log.items.len(), 3);
        assert_eq!(log.items[0], Item::Thinking("Let me read it.".into()));
        let Item::Tool(t) = &log.items[1] else { panic!() };
        assert_eq!((t.title.as_str(), t.status, t.locations.len(), t.content.len()), ("Read src/agent.rs", ToolCallStatus::Completed, 1, 1));
        assert_eq!(log.items[2], Item::Text("The package is **oskl**.".into()));
        let u = log.usage.as_ref().unwrap();
        assert_eq!((u.used, u.cost.as_ref().map(|c| c.amount)), (30700, Some(0.15)));
        assert_eq!(log.limits[0], Limit { name: "5h".into(), used: 0.02, resets_at: Some(1794533400) });
    }

    #[test]
    fn replayed_user_message() {
        let mut log = Log::default();
        feed(
            &mut log,
            &[
                r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<ide_opened_file>src/a.rs</ide_opened_file>"}}"#,
                r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Fix it"}}"#,
                r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Done."}}"#,
                r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Thanks"}}"#,
            ],
        );
        assert_eq!(log.items[0], Item::User { text: "Fix it".into(), images: vec![], context: vec!["<ide_opened_file>src/a.rs</ide_opened_file>".into()] });
        assert_eq!(log.items[1], Item::Text("Done.".into()));
        assert!(matches!(&log.items[2], Item::User { text, .. } if text == "Thanks"));
    }

    #[test]
    fn permission_card_takes_the_tool_title() {
        let mut log = Log::default();
        feed(&mut log, &[r#"{"sessionUpdate":"tool_call","toolCallId":"w","title":"Write target/x.txt","kind":"edit"}"#]);
        let req: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"w"},"options":[{"optionId":"allow","name":"Yes","kind":"allow_once"},{"optionId":"reject","name":"No","kind":"reject_once"}]}"#,
        )
        .unwrap();
        let ix = log.ask(&req);
        log.answer(ix, "Yes".into());
        let Item::Ask(q) = &log.items[ix] else { panic!() };
        assert_eq!((q.title.as_str(), q.options.len(), q.answer.as_deref()), ("Write target/x.txt", 2, Some("Yes")));
    }

    #[test]
    fn pictures_base64_and_times() {
        assert_eq!([base64(b""), base64(b"f"), base64(b"fo"), base64(b"foobar")], ["", "Zg==", "Zm8=", "Zm9vYmFy"]);
        let pic = picture("png", b"\x89PNG fake".to_vec()).unwrap();
        let ContentBlock::Image(i) = &prompt("", &[], &[pic])[0] else { panic!() };
        assert_eq!((i.mime_type.as_str(), crate::api::unbase64(&i.data)), ("image/png", b"\x89PNG fake".to_vec()));
        assert!(picture("bmp", vec![]).is_none());
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_time("2026-11-12T21:00:09.879Z"), Some(1794517209));
        assert_eq!(unix_time("yesterday"), None);
    }

    /// Stopping an agent ends what it started too (its process group), without waiting.
    #[test]
    fn stopping_ends_the_whole_process_group() {
        let mut cmd = std::process::Command::new("/bin/sh");
        let child = std::os::unix::process::CommandExt::process_group(&mut cmd, 0).args(["-c", "sleep 30 & wait"]).stdin(Stdio::piped()).spawn().unwrap();
        let group = child.id().to_string();
        let alive = || std::process::Command::new("/usr/bin/pgrep").args(["-g", &group]).output().is_ok_and(|o| !o.stdout.is_empty());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(alive());
        let t = std::time::Instant::now();
        drop(Group(Some(child)));
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "dropping doesn't wait on the caller's thread");
        let t = std::time::Instant::now();
        while alive() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!alive(), "the shell and its sleep are gone");
    }

    /// An agent that dies before the handshake says why (its stderr), and nothing is left running.
    #[test]
    fn start_failure_says_why() {
        let spec = AgentSpec { id: "x", name: "Broken", cmd: &["sh", "-c", "echo 'node: version too old' >&2; exit 1"] };
        let e = start(&spec, Path::new("/")).err().unwrap();
        assert!(e.starts_with("Broken couldn't start.") && e.contains("node: version too old"), "{e}");
        let missing = AgentSpec { id: "y", name: "Nowhere", cmd: &["no-such-agent-binary"] };
        assert!(start(&missing, Path::new("/")).err().unwrap().contains("isn't on your shell's PATH"));
    }

    /// Talks to the real Claude adapter (needs npx, a Claude login, and spends a few cents):
    /// `cargo test acp::tests::live -- --ignored`.
    #[test]
    #[ignore]
    fn live() {
        let cwd = std::env::current_dir().unwrap();
        let (conn, rx) = start(agent("claude"), &cwd).unwrap();
        assert!(conn.init.agent_capabilities.load_session);
        let mut log = Log::default();
        futures::executor::block_on(async {
            let s = conn.ask(NewSessionRequest::new(cwd.clone())).await.unwrap();
            log.config = s.config_options.unwrap_or_default();
            assert!(log.option(SessionConfigOptionCategory::Model).is_some());
            let r = conn.ask(PromptRequest::new(s.session_id.clone(), prompt("Reply with just the word pong.", &[], &[]))).await;
            log.finish(r.map(|r| r.stop_reason));
            // Loaded again, the conversation replays (into a second log, below).
            conn.ask(LoadSessionRequest::new(s.session_id, cwd.clone())).await.unwrap();
        });
        drop(conn);
        let (mut replay, mut users) = (Log::default(), 0);
        for ev in futures::executor::block_on_stream(rx) {
            if let Event::Update(n) = ev {
                // The prompt's updates come first; the replay starts with the user's message again.
                users += matches!(n.update, SessionUpdate::UserMessageChunk(_)) as usize;
                if users == 0 { log.apply(n.update) } else { replay.apply(n.update) };
            }
        }
        assert!(log.items.iter().any(|it| matches!(it, Item::Text(t) if t.to_lowercase().contains("pong"))), "{:?}", log.items);
        assert!(matches!(&replay.items[0], Item::User { text, .. } if text.contains("pong")), "{:?}", replay.items);
        assert!(replay.items.iter().any(|it| matches!(it, Item::Text(t) if t.to_lowercase().contains("pong"))), "{:?}", replay.items);
    }
}
