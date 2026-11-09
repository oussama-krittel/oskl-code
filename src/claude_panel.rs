//! The Claude sidebar: the project's conversations (newest first), plan limits and token usage,
//! and what Claude Code is set up with here — memory, skills, commands, agents, MCP servers,
//! hooks and settings files — each opening its file.

use crate::agent;
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{App, Context, EventEmitter, FocusHandle, SharedString, Task, Window, div};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::UNIX_EPOCH;

// ---------------------------------------------------------------- token usage

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
    /// Share of the prompt read from the cache.
    pub fn cache_hit(&self) -> f64 {
        let prompt = self.input + self.cache_read + self.cache_write;
        if prompt == 0 { 0. } else { self.cache_read as f64 / prompt as f64 }
    }
}

/// One reply's usage: its UTC day number, model and tokens.
type Reply = (i64, String, Tokens);

/// Days since 1970-01-01 of an ISO timestamp ("2026-11-09T…").
fn day_of(ts: &str) -> Option<i64> {
    let (y, m, d): (i64, i64, i64) = (ts.get(0..4)?.parse().ok()?, ts.get(5..7)?.parse().ok()?, ts.get(8..10)?.parse().ok()?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    Some(era * 146097 + doe - 719468)
}

/// A session file's replies. Claude Code writes a reply once per content block, so the last line per message id counts.
fn replies(jsonl: &str) -> Vec<Reply> {
    let mut by_id: HashMap<String, Reply> = HashMap::new();
    for line in jsonl.lines().filter(|l| l.contains("\"usage\"") && l.contains("\"assistant\"")) {
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        let (m, u) = (&r["message"], &r["message"]["usage"]);
        let (Some(id), Some(day)) = (m["id"].as_str(), r["timestamp"].as_str().and_then(day_of)) else { continue };
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        let t = Tokens { input: n("input_tokens"), output: n("output_tokens"), cache_read: n("cache_read_input_tokens"), cache_write: n("cache_creation_input_tokens") };
        by_id.insert(id.to_string(), (day, m["model"].as_str().unwrap_or("?").to_string(), t));
    }
    by_id.into_values().filter(|r| r.1 != "<synthetic>").collect()
}

/// Replies by file, with the file's (mtime, size) they were read at: counting again reads only changed files.
static REPLIES: LazyLock<Mutex<HashMap<PathBuf, ((i64, u64), Arc<Vec<Reply>>)>>> = LazyLock::new(Default::default);

pub struct Usage {
    /// Last 7 days, last 30 days, all time.
    pub windows: [(&'static str, Tokens); 3],
    /// The last 30 days by model, most used first.
    pub models: Vec<(String, Tokens)>,
}

/// Tokens of every session on this machine (~/.claude/projects, subagents included).
pub fn token_usage(today: i64) -> Usage {
    let mut stack = vec![home().join(".claude/projects")];
    let mut all = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let (path, Ok(meta)) = (e.path(), e.metadata()) else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let stamp = (meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64), meta.len());
            let cached = REPLIES.lock().unwrap().get(&path).filter(|c| c.0 == stamp).map(|c| c.1.clone());
            let r = match cached {
                Some(r) => r,
                None => {
                    let r = Arc::new(std::fs::read_to_string(&path).map(|t| replies(&t)).unwrap_or_default());
                    REPLIES.lock().unwrap().insert(path, (stamp, r.clone()));
                    r
                }
            };
            all.push(r);
        }
    }
    let mut windows = [("Last 7 days", Tokens::default()), ("Last 30 days", Tokens::default()), ("All time", Tokens::default())];
    let mut models: HashMap<String, Tokens> = HashMap::new();
    for (day, model, t) in all.iter().flat_map(|r| r.iter()) {
        let age = today - day;
        for (w, (_, sum)) in windows.iter_mut().enumerate() {
            if [age < 7, age < 30, true][w] {
                sum.add(t);
            }
        }
        if age < 30 {
            models.entry(model.clone()).or_default().add(t);
        }
    }
    let mut models: Vec<(String, Tokens)> = models.into_iter().collect();
    models.sort_by_key(|(_, t)| std::cmp::Reverse(t.total()));
    Usage { windows, models }
}

fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

// ---------------------------------------------------------------- what Claude Code is set up with

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// Where it comes from (user, project, local), and for hooks the command.
    pub detail: String,
    pub path: PathBuf,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn json_file(path: &Path) -> Value {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

/// `dir`'s entries matching `pick` (which turns one into a name), sorted.
fn listed(dir: &Path, scope: &str, pick: impl Fn(&Path) -> Option<(String, PathBuf)>) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| pick(&e.path())).map(|(name, path)| Entry { name, detail: scope.into(), path }).collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

fn md(p: &Path) -> Option<(String, PathBuf)> {
    (p.extension()? == "md").then(|| (p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), p.to_path_buf()))
}

/// The sections shown under usage, by title. Read from `home` (normally $HOME) and the project `root`.
pub fn config(home: &Path, root: &Path) -> Vec<(&'static str, Vec<Entry>)> {
    let scopes = [("user", home.join(".claude")), ("project", root.join(".claude"))];
    let exists = |name: &str, scope: &str, path: PathBuf| path.is_file().then(|| Entry { name: name.into(), detail: scope.into(), path });

    let mut memory: Vec<Entry> = [
        exists("CLAUDE.md", "user", home.join(".claude/CLAUDE.md")),
        exists("CLAUDE.md", "project", root.join("CLAUDE.md")),
        exists(".claude/CLAUDE.md", "project", root.join(".claude/CLAUDE.md")),
        exists("CLAUDE.local.md", "local", root.join("CLAUDE.local.md")),
    ]
    .into_iter()
    .flatten()
    .collect();
    memory.extend(listed(&agent::sessions_dir(root).join("memory"), "auto memory", md));

    let mut skills = Vec::new();
    let (mut commands, mut agents) = (Vec::new(), Vec::new());
    for (scope, dir) in &scopes {
        skills.extend(listed(&dir.join("skills"), scope, |p| Some((p.file_name()?.to_string_lossy().into_owned(), Some(p.join("SKILL.md")).filter(|s| s.is_file())?))));
        commands.extend(listed(&dir.join("commands"), scope, |p| md(p).map(|(n, p)| (format!("/{n}"), p))));
        agents.extend(listed(&dir.join("agents"), scope, md));
    }

    // MCP servers: the user's and this project's private ones in ~/.claude.json, shared ones in .mcp.json.
    let claude_json = home.join(".claude.json");
    let cj = json_file(&claude_json);
    let root_key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf()).to_string_lossy().into_owned();
    let mut mcp = Vec::new();
    for (servers, scope, path) in [(&cj["mcpServers"], "user", &claude_json), (&cj["projects"][&root_key]["mcpServers"], "local", &claude_json), (&json_file(&root.join(".mcp.json"))["mcpServers"], "project", &root.join(".mcp.json"))] {
        for (name, s) in servers.as_object().into_iter().flatten() {
            let how = s["type"].as_str().or(s["url"].as_str().map(|_| "http")).unwrap_or("stdio");
            mcp.push(Entry { name: name.clone(), detail: format!("{scope} · {how}"), path: path.clone() });
        }
    }

    let settings: Vec<Entry> = [
        exists("settings.json", "user", home.join(".claude/settings.json")),
        exists("settings.local.json", "user", home.join(".claude/settings.local.json")),
        exists("settings.json", "project", root.join(".claude/settings.json")),
        exists("settings.local.json", "local", root.join(".claude/settings.local.json")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut hooks = Vec::new();
    for s in &settings {
        for (event, groups) in json_file(&s.path)["hooks"].as_object().into_iter().flatten() {
            for g in groups.as_array().into_iter().flatten() {
                let name = match g["matcher"].as_str().filter(|m| !m.is_empty()) {
                    Some(m) => format!("{event} · {m}"),
                    None => event.clone(),
                };
                for h in g["hooks"].as_array().into_iter().flatten() {
                    let what = h["command"].as_str().or(h["prompt"].as_str()).unwrap_or("");
                    hooks.push(Entry { name: name.clone(), detail: format!("{} · {what}", s.detail), path: s.path.clone() });
                }
            }
        }
    }

    vec![("Memory", memory), ("Skills", skills), ("Commands", commands), ("Agents", agents), ("MCP servers", mcp), ("Hooks", hooks), ("Settings", settings)]
}

// ---------------------------------------------------------------- panel

pub struct ClaudePanel {
    focus: FocusHandle,
    root: PathBuf,
    search: gpui::Entity<Editor>,
    sessions: Vec<agent::Session>,
    /// Claude Code's `/usage` text: plan limits and what used them.
    limits: Option<Result<String, String>>,
    tokens: Option<Usage>,
    config: Vec<(&'static str, Vec<Entry>)>,
    /// Folded sections, by title.
    closed: HashSet<&'static str>,
    _tasks: [Option<Task<()>>; 3],
    _sub: gpui::Subscription,
}

impl EventEmitter<Request> for ClaudePanel {}

impl ClaudePanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.placeholder = "Search conversations".into();
            ed
        });
        let sub = cx.subscribe_in(&search, window, |p, _, ev: &EditorEvent, _, cx| match ev {
            EditorEvent::Changed => p.load_sessions(cx),
            EditorEvent::Submit => {
                if let Some(s) = p.sessions.first() {
                    cx.emit(Request::OpenChat(Some(s.id.clone())));
                }
            }
            _ => {}
        });
        Self {
            focus: cx.focus_handle(),
            root,
            search,
            sessions: Vec::new(),
            limits: None,
            tokens: None,
            config: Vec::new(),
            closed: ["Memory", "Skills", "Commands", "Agents", "MCP servers", "Hooks", "Settings"].into(),
            _tasks: [None, None, None],
            _sub: sub,
        }
    }

    pub fn focus_target(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }

    pub fn loaded(&self) -> bool {
        !self.config.is_empty()
    }

    /// Reads everything again (the panel was shown, or refresh was clicked); `root` is the project folder.
    pub fn refresh(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.root = root;
        self.load_sessions(cx);
        let (home, root) = (home(), self.root.clone());
        self.config = config(&home, &root);
        self._tasks[1] = Some(cx.spawn(async move |this, cx| {
            let u = cx.background_spawn(async move { token_usage(crate::util::now().div_euclid(86400)) }).await;
            this.update(cx, |p, cx| {
                p.tokens = Some(u);
                cx.notify();
            })
            .ok();
        }));
        self.load_limits(cx);
    }

    fn load_sessions(&mut self, cx: &mut Context<Self>) {
        let (dir, q) = (agent::sessions_dir(&self.root), self.search.read(cx).text());
        self._tasks[0] = Some(cx.spawn(async move |this, cx| {
            let list = cx.background_spawn(async move { agent::list_sessions(&dir, &q) }).await;
            this.update(cx, |p, cx| {
                p.sessions = list;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Runs Claude Code's local `/usage` (no tokens): kept until refreshed, it takes a few seconds.
    fn load_limits(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self._tasks[2] = Some(cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { agent::usage_cached(&root, 30) }).await;
            this.update(cx, |p, cx| {
                p.limits = Some(r);
                cx.notify();
            })
            .ok();
        }));
    }

    fn section(&self, title: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        ui::section_header(SharedString::from(format!("claude-{title}")), title, !self.closed.contains(title), cx.listener(move |p, _, _, cx| {
            if !p.closed.remove(title) {
                p.closed.insert(title);
            }
            cx.notify();
        }))
    }

    fn session_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        self.sessions.iter().take(50).enumerate().map(|(i, s)| {
            let (id, title) = (s.id.clone(), s.title.clone());
            let (id2, id3) = (id.clone(), id.clone());
            let root = self.root.clone();
            row(("claude-session", i))
                .group("claude-session")
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::OpenChat(Some(id.clone())))))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(s.title.clone()))
                .child(div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).group_hover("claude-session", |s| s.invisible()).child(crate::util::relative_time(s.updated)))
                .child(
                    div()
                        .absolute()
                        .right(px(6.))
                        .flex()
                        .invisible()
                        .group_hover("claude-session", |s| s.visible())
                        .child(ui::icon_button(("claude-session-term", i), "terminal").tooltip(ui::tooltip("Resume in a terminal")).on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(Request::RunInTerminal { dir: root.clone(), command: format!("claude --resume {id2}") });
                        })))
                        .child(ui::icon_button(("claude-session-del", i), "trash").tooltip(ui::tooltip("Move to Trash")).on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            let (weak, id) = (cx.entity().downgrade(), id3.clone());
                            cx.emit(Request::Ask(crate::dialog::Ask {
                                title: format!("Move \"{title}\" to the Trash?"),
                                detail: "The conversation's file goes to the Trash (you can put it back from there).".into(),
                                buttons: vec!["Move to Trash".into()],
                                danger: true,
                                cancel_default: false,
                                cancel_label: None,
                                on_answer: Rc::new(move |_, _, cx| {
                                    weak.update(cx, |p, cx| match agent::trash_session(&agent::sessions_dir(&p.root), &id) {
                                        Ok(()) => p.load_sessions(cx),
                                        Err(e) => cx.emit(Request::Error(e)),
                                    })
                                    .ok();
                                }),
                            }));
                        }))),
                )
                .into_any_element()
        }).collect()
    }

    fn usage_rows(&self) -> Vec<gpui::AnyElement> {
        let mut out = Vec::new();
        match &self.limits {
            None => out.push(note("Asking Claude Code for plan limits…")),
            Some(Err(e)) => out.push(note(e)),
            // "Current week (all models): 7% used · resets Nov 15 at 1am (…)" gets a meter; the rest is what used them.
            Some(Ok(text)) => {
                for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    out.push(match agent::limit(l) {
                        Some((k, pct, resets)) => div()
                            .px(px(20.))
                            .py(px(3.))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .text_size(px(12.))
                            .child(div().flex().gap(px(6.)).child(div().flex_1().child(k.to_string())).child(div().text_color(theme::fg_bright()).child(format!("{pct}%"))))
                            .child(div().h(px(4.)).rounded(px(2.)).bg(theme::border()).child(div().h_full().rounded(px(2.)).w(gpui::relative(pct.clamp(0., 100.) / 100.)).bg(if pct >= 80. { theme::git_deleted() } else { theme::accent() })))
                            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(resets.to_string()))
                            .into_any_element(),
                        None => note(l),
                    });
                }
            }
        }
        match &self.tokens {
            None => out.push(note("Counting tokens…")),
            Some(u) => {
                out.push(subhead("Tokens"));
                for (label, t) in &u.windows {
                    out.push(pair(label, format!("{} · {:.0}% cached", compact(t.total()), t.cache_hit() * 100.)));
                }
                if !u.models.is_empty() {
                    out.push(subhead("Last 30 days by model"));
                    for (m, t) in &u.models {
                        out.push(pair(m, format!("{} in · {} out", compact(t.input + t.cache_read + t.cache_write), compact(t.output))));
                    }
                }
            }
        }
        out
    }
}

fn row(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div().id(id).relative().flex().items_center().gap(px(6.)).h(px(22.)).pl(px(20.)).pr(px(8.)).text_size(px(12.)).cursor_pointer().hover(|s| s.bg(theme::hover()))
}

fn note(text: &str) -> gpui::AnyElement {
    div().px(px(20.)).py(px(2.)).text_size(px(11.5)).text_color(theme::fg_dim()).child(text.to_string()).into_any_element()
}

fn subhead(text: &str) -> gpui::AnyElement {
    div().px(px(20.)).pt(px(8.)).pb(px(2.)).text_size(px(10.5)).text_color(theme::fg_muted()).child(text.to_uppercase()).into_any_element()
}

fn pair(k: &str, v: String) -> gpui::AnyElement {
    div().flex().gap(px(8.)).px(px(20.)).py(px(1.)).text_size(px(12.)).child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(k.to_string())).child(div().flex_none().text_color(theme::fg_dim()).child(v)).into_any_element()
}

impl Render for ClaudePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let head_button = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).tooltip(ui::tooltip(tip));
        let mut body = div().id("claude-panel-body").flex_1().min_h_0().overflow_y_scroll().pb(px(12.));

        body = body.child(self.section("Sessions", cx));
        if !self.closed.contains("Sessions") {
            body = body
                .child(div().px(px(12.)).pb(px(4.)).child(div().px(px(6.)).rounded(px(4.)).border_1().border_color(theme::border_input()).bg(theme::bg_input()).child(self.search.clone())))
                .when(self.sessions.is_empty(), |d| d.child(note("No conversations in this project")))
                .children(self.session_rows(cx));
        }
        body = body.child(self.section("Usage", cx));
        if !self.closed.contains("Usage") {
            body = body.children(self.usage_rows());
        }
        for (i, (title, entries)) in self.config.iter().enumerate() {
            let title = *title;
            body = body.child(self.section(title, cx).child(div().text_color(theme::fg_muted()).font_weight(gpui::FontWeight::NORMAL).child(entries.len().to_string())));
            if self.closed.contains(title) {
                continue;
            }
            if entries.is_empty() {
                body = body.child(note("None"));
            }
            body = body.children(entries.iter().enumerate().map(|(j, e)| {
                let path = e.path.clone();
                row(("claude-entry", i * 10_000 + j))
                    .tooltip(ui::tooltip_text(e.path.display().to_string()))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::OpenFile { path: path.clone(), line: None, preview: false })))
                    .child(div().flex_none().max_w(gpui::relative(0.6)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(11.)).text_color(theme::fg_muted()).child(e.detail.clone()))
            }));
        }

        div()
            .key_context("ClaudePanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .pl(px(20.))
                    .pr(px(6.))
                    .gap(px(1.))
                    .text_size(px(10.5))
                    .child(div().flex_1().child("CLAUDE"))
                    .child(head_button("claude-new", "plus", "New chat (⌘⇧Esc)").on_click(cx.listener(|_, _, _, cx| cx.emit(Request::OpenChat(None)))))
                    .child(head_button("claude-refresh", "refresh", "Refresh").on_click(cx.listener(|p, _, _, cx| p.refresh(p.root.clone(), cx)))),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_replies() {
        assert_eq!(day_of("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(day_of("2026-11-09T10:00:00.000Z"), Some(20766));
        assert_eq!(day_of("2024-03-01"), Some(19783));
        // The same reply written per content block counts once, with its last usage.
        let line = |out: u64| format!(r#"{{"type":"assistant","timestamp":"2026-11-09T10:00:00Z","message":{{"id":"m1","model":"claude-opus-5-5","usage":{{"input_tokens":2,"output_tokens":{out},"cache_read_input_tokens":90,"cache_creation_input_tokens":8}}}}}}"#);
        let r = replies(&format!("{}\n{}\n", line(1), line(40)));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].2, Tokens { input: 2, output: 40, cache_read: 90, cache_write: 8 });
        assert!((r[0].2.cache_hit() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn config_lists_what_is_set_up() {
        let tmp = std::env::temp_dir().join(format!("claude-panel-{}", std::process::id()));
        let (home, root) = (tmp.join("home"), tmp.join("proj"));
        for d in [home.join(".claude/skills/deploy"), home.join(".claude/commands"), root.join(".claude/agents")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join(".claude/skills/deploy/SKILL.md"), "x").unwrap();
        std::fs::write(home.join(".claude/commands/fix.md"), "x").unwrap();
        std::fs::write(root.join(".claude/agents/reviewer.md"), "x").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "x").unwrap();
        std::fs::write(root.join(".mcp.json"), r#"{"mcpServers":{"docs":{"url":"https://x"}}}"#).unwrap();
        std::fs::write(home.join(".claude/settings.json"), r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"lint"}]}]}}"#).unwrap();
        let c: HashMap<_, _> = config(&home, &root).into_iter().map(|(t, e)| (t, e.into_iter().map(|e| format!("{} ({})", e.name, e.detail)).collect::<Vec<_>>())).collect();
        assert_eq!(c["Skills"], ["deploy (user)"]);
        assert_eq!(c["Commands"], ["/fix (user)"]);
        assert_eq!(c["Agents"], ["reviewer (project)"]);
        assert_eq!(c["Memory"], ["CLAUDE.md (project)"]);
        assert_eq!(c["MCP servers"], ["docs (project · http)"]);
        assert_eq!(c["Hooks"], ["PreToolUse · Bash (user · lint)"]);
        assert_eq!(c["Settings"], ["settings.json (user)"]);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
