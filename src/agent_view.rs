//! The Claude tab: a chat with the user's own Claude Code (see `agent`). Streams replies, shows
//! tool calls and edits as cards, asks before tools run, and offers what Claude Code's VS Code
//! panel does: @-mentions, /commands, the open file and selection as context, pictures,
//! permission modes, model and effort, plan review, questions, history, forking a conversation
//! from an earlier message, rewinding files to a checkpoint, MCP and context usage.

use crate::agent::{self, Answer, Item, Log, Opts, Proc};
use crate::complete::{Item as Choice, Kind};
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::preview::Markdown;
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use futures::StreamExt;
use gpui::prelude::*;
use gpui::*;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

actions!(agent_view, [Interrupt, CycleMode]);

pub fn bind_keys() {
    crate::keybindings::collect([
        crate::keybindings::kb("escape", Interrupt, Some("AgentView")),
        crate::keybindings::kb("shift-tab", CycleMode, Some("AgentView")),
        crate::keybindings::kb("shift-tab", CycleMode, Some("AgentView > Editor")),
    ]);
}

/// Permission modes: Claude Code's name, what the picker says.
const MODES: [(&str, &str); 5] = [
    ("default", "Ask before edits"),
    ("acceptEdits", "Edit automatically"),
    ("plan", "Plan mode"),
    ("auto", "Auto mode"),
    ("bypassPermissions", "Bypass permissions"),
];

/// Commands the tab handles itself (the rest go to Claude Code).
const LOCAL: [(&str, &str); 7] = [
    ("clear", "Start a new conversation"),
    ("resume", "Open a past conversation"),
    ("model", "Choose the model"),
    ("mcp", "MCP servers and their status"),
    ("rename", "Rename this conversation"),
    ("export", "Save the conversation as Markdown"),
    ("terminal", "Continue this conversation in a terminal"),
];

#[derive(Clone, Copy, PartialEq)]
enum Menu {
    Model,
    Effort,
    Mode,
    History,
    More,
    Style,
    /// Every subagent in the conversation and what it's doing.
    Agents,
}

struct Outgoing {
    text: String,
    images: Vec<(String, String)>,
    context: Vec<String>,
}

type Reply = Box<dyn FnOnce(&mut AgentView, Result<Value, String>, &mut Context<AgentView>)>;

/// An idle Claude Code (~250 MB) is stopped after this long; the next message resumes the session.
const IDLE_STOP: Duration = Duration::from_secs(120);

/// The biggest picture sent (the API's limit).
const MAX_IMAGE: usize = 5 * 1024 * 1024;

thread_local! {
    /// What Claude Code offers (its `initialize` answer), asked once per app run for every tab.
    static OFFERS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FETCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Each project's files for @-mentions (shared by its tabs) and when they were listed.
    static FILES: RefCell<HashMap<PathBuf, (Rc<RefCell<Vec<String>>>, Option<std::time::Instant>)>> = RefCell::new(HashMap::new());
}

pub struct AgentView {
    focus: FocusHandle,
    pub root: PathBuf,
    input: Entity<Editor>,
    search: Entity<Editor>,
    log: Log,
    list: ListState,
    proc: Option<Proc>,
    _reader: Option<Task<()>>,
    /// After an edit or retry: the next process continues the session from this record, as a new session.
    branch_at: Option<String>,
    /// Effort changed: the next message starts Claude Code again (resuming).
    restart: bool,
    waiting: HashMap<String, Reply>,
    n: u64,
    /// What Claude Code offers: its models (initialize's), commands (name, description).
    models: Vec<Value>,
    /// Output styles Claude Code offers, and the one chosen ("" = the user's setting).
    styles: Vec<String>,
    style: String,
    commands: Rc<RefCell<Vec<(String, String)>>>,
    model: String,
    effort: String,
    mode: String,
    queue: Vec<Outgoing>,
    images: Vec<(String, String)>,
    /// The editor the user was in last (the workspace keeps it): its file and selection go along.
    editor: Rc<RefCell<Option<WeakEntity<Editor>>>>,
    share_editor: bool,
    title: Option<String>,
    /// Cards opened (or closed, for those open by default).
    toggled: HashSet<usize>,
    menu: Option<(Menu, Point<Pixels>)>,
    /// The menu a click outside just closed (so a click on its own button doesn't reopen it).
    just_closed: Option<Menu>,
    /// A forked tab: the message (its uuid) to go back to once the conversation has loaded.
    fork_at: Option<String>,
    /// Edit diffs by (item, edit), worked out once.
    diffs: RefCell<HashMap<(usize, usize), Rc<Vec<(similar::ChangeTag, String)>>>>,
    sessions: Vec<agent::Session>,
    /// A popup of facts: title and (name, value) rows (MCP servers, context usage).
    info: Option<(String, Vec<(String, String)>)>,
    md: HashMap<usize, (usize, Rc<Markdown>)>,
    md_images: Entity<RetainAllImageCache>,
    /// AskUserQuestion answers being picked: request id -> per question, the labels chosen.
    picks: HashMap<String, Vec<Vec<String>>>,
    tasks_open: bool,
    _stop: Option<Task<()>>,
    _idle: Option<Task<()>>,
    _history: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for AgentView {}

impl Focusable for AgentView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AgentView {
    pub fn new(root: PathBuf, session: Option<String>, editor: Rc<RefCell<Option<WeakEntity<Editor>>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let files = FILES.with(|f| f.borrow_mut().entry(root.clone()).or_insert_with(|| (Rc::default(), None)).0.clone());
        let commands = Rc::new(RefCell::new(LOCAL.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect::<Vec<_>>()));
        let input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::MultiLine, cx);
            ed.chat = true;
            ed.font_size = 13.;
            ed.placeholder = "Ask Claude to edit, explain or build… (@ files, / commands, ⇧⇥ modes)".into();
            ed.suggest = Some(suggestions(files.clone(), commands.clone()));
            ed
        });
        let search = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.placeholder = "Search conversations".into();
            ed
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |v, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Submit => v.submit(window, cx),
                EditorEvent::Cancel => v.escape(cx),
                EditorEvent::Image(img) => v.add_image(img, cx),
                _ => {}
            }),
            cx.subscribe_in(&search, window, |v, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed => v.refresh_history(cx),
                EditorEvent::Submit => {
                    if let Some(s) = v.sessions.first().cloned() {
                        v.load(&s.id, Some(s.title), cx);
                    }
                }
                EditorEvent::Cancel => {
                    v.menu = None;
                    window.focus(&v.input.read(cx).focus_target(cx), cx);
                    cx.notify();
                }
                _ => {}
            }),
            // Using the box: the project's files for @, and (once per app run) what Claude Code offers.
            cx.on_focus(&input.focus_handle(cx), window, |v, _, cx| {
                v.list_files(cx);
                v.fetch_offers(cx);
            }),
        ];
        let mut v = Self {
            focus: cx.focus_handle(),
            root,
            input,
            search,
            log: Log::default(),
            list: ListState::new(0, ListAlignment::Bottom, gpui::px(800.)),
            proc: None,
            _reader: None,
            branch_at: None,
            restart: false,
            waiting: HashMap::new(),
            n: 0,
            models: Vec::new(),
            commands,
            model: String::new(),
            effort: String::new(),
            styles: Vec::new(),
            style: String::new(),
            mode: "default".into(),
            queue: Vec::new(),
            images: Vec::new(),
            editor,
            share_editor: true,
            title: None,
            toggled: HashSet::new(),
            menu: None,
            just_closed: None,
            fork_at: None,
            diffs: RefCell::default(),
            sessions: Vec::new(),
            info: None,
            md: HashMap::new(),
            md_images: RetainAllImageCache::new(cx),
            picks: HashMap::new(),
            tasks_open: true,
            _stop: None,
            _idle: None,
            _history: None,
            _subs: subs,
        };
        if let Some(offers) = OFFERS.with(|o| o.borrow().clone()) {
            v.take_offers(&offers);
        }
        if let Some(id) = session {
            v.load(&id, None, cx);
        }
        v
    }

    /// Lists the project's files (for @) when the box is used, at most once a minute for all its tabs.
    fn list_files(&self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let Some(files) = FILES.with(|f| {
            let mut f = f.borrow_mut();
            let e = f.entry(root.clone()).or_insert_with(|| (Rc::default(), None));
            let stale = e.1.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
            stale.then(|| {
                e.1 = Some(std::time::Instant::now());
                e.0.clone()
            })
        }) else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let list = cx.background_spawn(async move { crate::git::list_files(&root) }).await;
            *files.borrow_mut() = list;
        })
        .detach();
    }

    /// Asks Claude Code (once per app run, outside the project) what it offers, for the pickers and /.
    fn fetch_offers(&mut self, cx: &mut Context<Self>) {
        if !self.models.is_empty() || FETCHING.with(|f| f.replace(true)) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { agent::offers() }).await;
            FETCHING.with(|f| f.set(false));
            this.update(cx, |v, cx| match r {
                Ok(r) => {
                    v.take_offers(&r);
                    OFFERS.with(|o| *o.borrow_mut() = Some(r));
                    cx.notify();
                }
                Err(e) => {
                    v.error(e);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Models, commands, styles and the permission mode from Claude Code's `initialize` answer.
    fn take_offers(&mut self, r: &Value) {
        self.models = r["models"].as_array().cloned().unwrap_or_default();
        let mut cmds: Vec<(String, String)> = LOCAL.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect();
        for c in r["commands"].as_array().into_iter().flatten() {
            let name = c["name"].as_str().unwrap_or("").to_string();
            if !cmds.iter().any(|(n, _)| *n == name) {
                cmds.push((name, c["description"].as_str().unwrap_or("").to_string()));
            }
        }
        *self.commands.borrow_mut() = cmds;
        if self.log.items.is_empty()
            && let Some(m) = r["current_permission_mode"].as_str()
        {
            self.mode = m.into();
        }
        self.styles = r["available_output_styles"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect();
        if self.style.is_empty() {
            self.style = r["output_style"].as_str().unwrap_or("").to_string();
        }
    }

    /// Stops this tab's Claude Code (on purpose: what it says while ending isn't shown).
    fn stop_proc(&mut self) {
        self.proc = None;
        self._reader = None;
        self._stop = None;
        self.waiting.clear();
        self.log.busy = false;
        for j in self.log.jobs.iter_mut().filter(|j| j.running()) {
            j.status = "stopped".into(); // they ended with it
        }
        let first = self.log.close_asks();
        let n = self.log.items.len();
        if first < n {
            self.list.splice(first..n, n - first);
        }
    }

    /// Nothing going on: no turn, question, request or background job.
    fn idle(&self) -> bool {
        !self.log.busy && self.waiting.is_empty() && self.pending_ask().is_none() && self.queue.is_empty() && !self.log.jobs.iter().any(|j| j.running())
    }

    /// Stops Claude Code once it has been idle for a while (it holds ~250 MB).
    fn stop_when_idle(&mut self, cx: &mut Context<Self>) {
        self._idle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(IDLE_STOP).await;
            this.update(cx, |v, cx| {
                if v.proc.is_some() && v.idle() {
                    v.stop_proc();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub fn focus_target(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_target(cx)
    }

    pub fn session(&self) -> Option<String> {
        self.log.session.clone()
    }

    pub fn title(&self) -> String {
        let first = self.log.items.iter().find_map(|it| if let Item::User { text, .. } = it { text.lines().next() } else { None });
        let t = self.title.as_deref().or(first).unwrap_or("Claude").to_string();
        if t.chars().count() > 32 { t.chars().take(31).collect::<String>() + "…" } else { t }
    }

    /// ⌘K ⌘A: `@file#L1-5` for the editor's selection, in the box.
    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |ed, cx| {
            let head = ed.buf.sel.head;
            ed.replace_range(head..head, text, cx);
        });
        window.focus(&self.input.read(cx).focus_target(cx), cx);
    }

    /// Sends what's in the box (`claude <message>` on the command line).
    pub fn send_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(window, cx);
    }

    // ------------------------------------------------------------ process

    fn ensure_proc(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if self.proc.is_some() {
            return Ok(());
        }
        let opts = Opts {
            resume: self.log.session.clone(),
            resume_at: self.branch_at.take(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            style: self.style.clone(),
            mode: if self.mode == "default" { String::new() } else { self.mode.clone() },
        };
        let (proc, mut rx) = agent::start(&self.root, &opts)?;
        self.proc = Some(proc);
        self._reader = Some(cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                // Everything that has arrived, applied at once; then a pause, so a fast stream
                // redraws ~30 times a second instead of once per token.
                let mut batch = vec![ev];
                while let Ok(ev) = rx.try_recv() {
                    batch.push(ev);
                }
                if this.update(cx, |v, cx| batch.into_iter().for_each(|ev| v.on_event(ev, cx))).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(33)).await;
            }
        }));
        self.request(json!({ "subtype": "initialize" }), |v, r, cx| {
            let Ok(r) = r else { return };
            v.take_offers(&r);
            OFFERS.with(|o| *o.borrow_mut() = Some(r));
            if v.idle() {
                v.stop_when_idle(cx);
            }
            cx.notify();
        }, cx);
        Ok(())
    }

    /// A control request; `then` gets Claude Code's answer.
    fn request(&mut self, req: Value, then: impl FnOnce(&mut Self, Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        if let Err(e) = self.ensure_proc(cx) {
            return then(self, Err(e), cx);
        }
        self.n += 1;
        let id = format!("kite-{}", self.n);
        match self.proc.as_mut().map(|p| p.send(&agent::control(&id, req))) {
            Some(Ok(())) => {
                self.waiting.insert(id, Box::new(then));
            }
            Some(Err(e)) => then(self, Err(e), cx),
            None => {}
        }
    }

    fn send(&mut self, msg: &Value) {
        if let Some(Err(e)) = self.proc.as_mut().map(|p| p.send(msg)) {
            self.error(e);
        }
    }

    fn on_event(&mut self, ev: Value, cx: &mut Context<Self>) {
        match ev["type"].as_str() {
            Some("control_response") => {
                let r = &ev["response"];
                if let Some(then) = r["request_id"].as_str().and_then(|id| self.waiting.remove(id)) {
                    let res = if r["subtype"] == "success" { Ok(r["response"].clone()) } else { Err(r["error"].as_str().unwrap_or("Claude Code refused").to_string()) };
                    then(self, res, cx);
                }
                return cx.notify();
            }
            Some("control_request") if ev["request"]["subtype"] != "can_use_tool" => {
                let id = ev["request_id"].clone();
                return self.send(&json!({ "type": "control_response", "response": { "subtype": "error", "request_id": id, "error": "Not supported in OSKL Code" } }));
            }
            Some("exit") => {
                self.proc = None;
                self._idle = None;
                for (_, then) in std::mem::take(&mut self.waiting) {
                    then(self, Err("Claude Code stopped".into()), cx);
                }
            }
            _ => {}
        }
        let busy = self.log.busy;
        let changed = self.apply(&ev) || busy != self.log.busy || matches!(ev["type"].as_str(), Some("system" | "result" | "exit" | "control_request"));
        if let Some(m) = self.log.mode.take() {
            self.mode = m;
        }
        if matches!(ev["type"].as_str(), Some("result" | "exit")) {
            self._stop = None;
            self.drain(cx);
        }
        if self.proc.is_some() && self.idle() {
            self.stop_when_idle(cx);
        }
        // Most stream events (signatures, block ends, rate limits) change nothing on screen.
        if changed {
            cx.notify();
        }
    }

    /// Applies an event and redraws the items it changed; whether any did.
    fn apply(&mut self, ev: &Value) -> bool {
        let old = self.log.items.len();
        let first = self.log.apply(ev).min(old);
        self.list.splice(first..old, self.log.items.len() - first);
        first < self.log.items.len()
    }

    fn error(&mut self, e: String) {
        let old = self.log.items.len();
        self.log.items.push(Item::Error(e));
        self.list.splice(old..old, 1);
    }

    // ------------------------------------------------------------ sending

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() && self.images.is_empty() {
            return;
        }
        // A question or permission waiting: what's typed answers it.
        if let Some(ix) = self.pending_ask().filter(|_| self.proc.is_some()) {
            if let Item::Ask(q) = &self.log.items[ix]
                && q.tool == "AskUserQuestion"
            {
                let qs = q.input["questions"].as_array().cloned().unwrap_or_default();
                let picks = self.picks.get(&q.id).cloned().unwrap_or_default();
                let answers: serde_json::Map<String, Value> = qs
                    .iter()
                    .enumerate()
                    .map(|(i, qq)| {
                        let chosen = picks.get(i).filter(|p| !p.is_empty()).map(|p| p.join(", ")).unwrap_or_else(|| text.clone());
                        (qq["question"].as_str().unwrap_or("").to_string(), Value::String(chosen))
                    })
                    .collect();
                let mut input = q.input.clone();
                input["answers"] = Value::Object(answers);
                self.input.update(cx, |e, cx| e.set_text("", cx));
                return self.answer(ix, Answer::Allow, Some(input), None, cx);
            }
            return self.answer(ix, Answer::Deny, None, None, cx);
        }
        if let Some(cmd) = text.strip_prefix('/')
            && self.local_command(cmd, window, cx)
        {
            self.input.update(cx, |e, cx| e.set_text("", cx));
            return;
        }
        let m = Outgoing { text, images: std::mem::take(&mut self.images), context: self.context_blocks(cx) };
        self.input.update(cx, |e, cx| e.set_text("", cx));
        if self.log.busy {
            self.queue.push(m);
        } else {
            self.deliver(m, cx);
        }
        cx.notify();
    }

    fn deliver(&mut self, m: Outgoing, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.restart) {
            self.stop_proc();
        }
        self._idle = None;
        if let Err(e) = self.ensure_proc(cx) {
            return self.error(e);
        }
        let old = self.log.items.len();
        let shown = m.context.iter().filter(|c| !c.starts_with("<ide_opened_file>")).cloned().collect();
        self.log.items.push(Item::User { text: m.text.clone(), images: m.images.len(), context: shown, uuid: None, before: self.log.last_uuid.clone() });
        self.list.splice(old..old, 1);
        self.list.reset(self.log.items.len()); // follow the bottom again
        self.log.busy = true;
        self.send(&agent::user_message(&m.text, &m.context, &m.images));
    }

    /// Sends what was queued once a turn has ended.
    fn drain(&mut self, cx: &mut Context<Self>) {
        if !self.log.busy && !self.queue.is_empty() {
            let m = self.queue.remove(0);
            self.deliver(m, cx);
        }
    }

    /// What the user has open or selected in the editor, the way Claude Code's IDE extension says it.
    fn context_blocks(&self, cx: &App) -> Vec<String> {
        let Some(ed) = self.shared_editor(cx) else { return Vec::new() };
        let ed = ed.read(cx);
        let Some(path) = ed.path.clone() else { return Vec::new() };
        let r = ed.buf.sel.range();
        if r.is_empty() {
            return vec![format!("<ide_opened_file>The user opened the file {} in the IDE. This may or may not be related to the current task.</ide_opened_file>", path.display())];
        }
        let (a, b) = (ed.buf.pos_to_rc(r.start).0 + 1, ed.buf.pos_to_rc(r.end).0 + 1);
        vec![format!(
            "<ide_selection>The user selected the lines {a} to {b} from {}:\n{}\n\nThis may or may not be related to the current task.</ide_selection>",
            path.display(),
            ed.buf.slice(r)
        )]
    }

    fn shared_editor(&self, _: &App) -> Option<Entity<Editor>> {
        self.share_editor.then(|| self.editor.borrow().as_ref().and_then(|w| w.upgrade())).flatten()
    }

    fn add_image(&mut self, img: &Image, cx: &mut Context<Self>) {
        let mt = match img.format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Webp => "image/webp",
            _ => return cx.emit(Request::Error("Claude reads PNG, JPEG, GIF and WebP pictures".into())),
        };
        if img.bytes.len() > MAX_IMAGE {
            return cx.emit(Request::Error("Pictures can be at most 5 MB".into()));
        }
        self.images.push((mt.into(), agent::base64(&img.bytes)));
        cx.notify();
    }

    /// Files added (attach button, drop): pictures are attached, the rest @-mentioned.
    fn add_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let mut mentions = String::new();
        for p in paths {
            let mt = p.extension().and_then(|e| agent::image_type(&e.to_string_lossy()));
            let small = std::fs::metadata(&p).is_ok_and(|m| m.len() as usize <= MAX_IMAGE);
            match (mt.filter(|_| small), std::fs::read(&p)) {
                (Some(mt), Ok(bytes)) => self.images.push((mt.into(), agent::base64(&bytes))),
                _ => mentions.push_str(&format!("@{} ", p.strip_prefix(&self.root).unwrap_or(&p).display())),
            }
        }
        if !mentions.is_empty() {
            self.insert(&mentions, window, cx);
        }
        cx.notify();
    }

    fn pending_ask(&self) -> Option<usize> {
        self.log.items.iter().rposition(|it| matches!(it, Item::Ask(q) if q.answer.is_none()))
    }

    /// Answers a permission card. `input`: the tool's input as approved; `mode`: the permission mode to go on in.
    fn answer(&mut self, ix: usize, how: Answer, input: Option<Value>, mode: Option<&str>, cx: &mut Context<Self>) {
        let Some(Item::Ask(q)) = self.log.items.get(ix).cloned() else { return };
        let response = if how == Answer::Deny {
            // What's in the box tells Claude what to do instead.
            let text = self.input.read(cx).text().trim().to_string();
            self.input.update(cx, |e, cx| e.set_text("", cx));
            let message = if text.is_empty() { "The user doesn't want to proceed with this tool use. The tool use was rejected. STOP what you are doing and wait for the user to tell you how to proceed.".to_string() } else { text };
            json!({ "behavior": "deny", "message": message })
        } else {
            let mut r = json!({ "behavior": "allow", "updatedInput": input.unwrap_or(q.input.clone()) });
            if how == Answer::Always && q.suggestions.is_array() {
                r["updatedPermissions"] = q.suggestions.clone();
            } else if let Some(m) = mode {
                r["updatedPermissions"] = json!([{ "type": "setMode", "mode": m, "destination": "session" }]);
            }
            r
        };
        self.send(&json!({ "type": "control_response", "response": { "subtype": "success", "request_id": q.id, "response": response } }));
        self.log.answer(&q.id, how);
        self.list.splice(ix..ix + 1, 1);
        if let Some(m) = mode {
            self.mode = m.into();
        }
        cx.notify();
    }

    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() | self.info.take().is_some() {
            return cx.notify();
        }
        self.interrupt(cx);
    }

    /// Esc: stops the turn; a process that doesn't stop within 3 s is ended (the next message resumes).
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if !self.log.busy {
            return;
        }
        self.request(json!({ "subtype": "interrupt" }), |_, _, _| {}, cx);
        self._stop = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            this.update(cx, |v, cx| {
                if v.log.busy {
                    v.stop_proc();
                    v.apply(&json!({ "type": "result", "subtype": "error_during_execution" }));
                    v.drain(cx);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn cycle_mode(&mut self, _: &CycleMode, _: &mut Window, cx: &mut Context<Self>) {
        let i = MODES.iter().take(3).position(|(m, _)| *m == self.mode).map_or(0, |i| (i + 1) % 3);
        self.set_mode(MODES[i].0, cx);
    }

    fn set_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        let was = std::mem::replace(&mut self.mode, mode.into());
        if self.proc.is_some() {
            self.request(json!({ "subtype": "set_permission_mode", "mode": mode }), move |v, r, cx| {
                if let Err(e) = r {
                    v.mode = was; // refused: the picker shows what's really on
                    v.error(e);
                    cx.notify();
                }
            }, cx);
        }
        cx.notify();
    }

    /// Bypass runs everything without asking: only after the user confirms.
    fn pick_mode(&mut self, mode: &'static str, cx: &mut Context<Self>) {
        if mode != "bypassPermissions" {
            return self.set_mode(mode, cx);
        }
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: "Bypass permissions?".into(),
            detail: "Claude will edit files and run any command without asking, until you switch modes. Use it only in a sandbox you trust.".into(),
            buttons: vec!["Bypass permissions".into()],
            danger: true,
            cancel_default: true,
            cancel_label: None,
            on_answer: Rc::new(move |_, _, cx| {
                weak.update(cx, |v, cx| v.set_mode(mode, cx)).ok();
            }),
        }));
    }

    fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        self.model = model.clone();
        if self.proc.is_some() {
            self.request(json!({ "subtype": "set_model", "model": model }), |v, r, cx| {
                if let Err(e) = r {
                    v.error(e);
                    cx.notify();
                }
            }, cx);
        }
        cx.notify();
    }

    fn model_info(&self) -> Option<&Value> {
        let want = if self.model.is_empty() { "default" } else { &self.model };
        self.models.iter().find(|m| m["value"] == want)
    }

    // ------------------------------------------------------------ conversations

    /// Starts over in this tab.
    fn clear(&mut self, cx: &mut Context<Self>) {
        self.stop_proc();
        self.log = Log::default();
        self.title = None;
        self.queue.clear();
        self.branch_at = None;
        self.reset_view();
        cx.notify();
    }

    fn reset_view(&mut self) {
        self.md.clear();
        self.diffs.borrow_mut().clear();
        self.toggled.clear();
        self.picks.clear();
        self.list.reset(self.log.items.len());
    }

    /// Opens a saved conversation in this tab (read off the main thread: sessions can be large).
    fn load(&mut self, id: &str, title: Option<String>, cx: &mut Context<Self>) {
        let (dir, id) = (agent::sessions_dir(&self.root), id.to_string());
        self.menu = None;
        self._history = Some(cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { agent::load_session(&dir, &id).map(|log| (log, title.or_else(|| agent::session_title(&dir, &id)))) }).await;
            this.update(cx, |v, cx| {
                match r {
                    Ok((log, title)) => {
                        v.stop_proc();
                        v.log = log;
                        v.title = title;
                        v.queue.clear();
                        v.branch_at = None;
                        v.reset_view();
                        if let Some(uuid) = v.fork_at.take()
                            && let Some(ix) = v.log.items.iter().position(|it| matches!(it, Item::User { uuid: Some(u), .. } if *u == uuid))
                            && let Some(t) = v.rewind_chat(ix)
                        {
                            v.input.update(cx, |e, cx| e.set_text(&t, cx));
                        }
                    }
                    Err(e) => v.error(e),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let (dir, q) = (agent::sessions_dir(&self.root), self.search.read(cx).text());
        self._history = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await; // typing: search once it pauses
            let list = cx.background_spawn(async move { agent::list_sessions(&dir, &q) }).await;
            this.update(cx, |v, cx| {
                v.sessions = list;
                cx.notify();
            })
            .ok();
        }));
    }

    fn open_menu(&mut self, menu: Menu, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // The click that closed this menu (outside it) was on its own button: it stays closed.
        if self.just_closed.take() == Some(menu) || self.menu.is_some_and(|(m, _)| m == menu) {
            self.menu = None;
            return cx.notify();
        }
        self.menu = Some((menu, at));
        if matches!(menu, Menu::Model | Menu::Effort | Menu::Style) {
            self.fetch_offers(cx);
        }
        if menu == Menu::History {
            self.search.update(cx, |e, cx| e.set_text("", cx));
            window.focus(&self.search.read(cx).focus_target(cx), cx);
            self.refresh_history(cx);
        }
        cx.notify();
    }

    /// Takes the conversation back to just before message `ix` and returns that message's
    /// text: sent again or edited, it continues as a new session branched off there.
    fn rewind_chat(&mut self, ix: usize) -> Option<String> {
        let Some(Item::User { text, before, .. }) = self.log.items.get(ix).cloned() else { return None };
        self.stop_proc();
        self.queue.clear();
        self.branch_at = before.clone().filter(|_| self.log.session.is_some());
        if before.is_none() {
            self.log.session = None; // the first message: nothing before it to keep
        }
        self.log.truncate(ix);
        self.log.last_uuid = before;
        self.md.retain(|i, _| *i < ix);
        self.toggled.retain(|i| *i < ix);
        self.diffs.borrow_mut().retain(|k, _| k.0 < ix);
        self.list.reset(self.log.items.len());
        Some(text)
    }

    /// Puts the project's files back as they were before message `ix` (Claude Code's checkpoints).
    fn rewind_code(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Item::User { uuid: Some(uuid), .. }) = self.log.items.get(ix).cloned() else {
            return cx.emit(Request::Message("No checkpoint for this message yet".into()));
        };
        let req = json!({ "subtype": "rewind_files", "user_message_id": uuid, "dry_run": true });
        self.request(req, move |v, r, cx| {
            let r = match r {
                Ok(r) if r["canRewind"] == true => r,
                Ok(r) => return cx.emit(Request::Error(r["error"].as_str().unwrap_or("Can't rewind to this message").into())),
                Err(e) => return cx.emit(Request::Error(e)),
            };
            let files: Vec<String> = r["filesChanged"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).map(|f| f.strip_prefix(&format!("{}/", v.root.display())).unwrap_or(f).to_string()).collect();
            if files.is_empty() {
                return cx.emit(Request::Message("No files changed since this message".into()));
            }
            let weak = cx.entity().downgrade();
            cx.emit(Request::Ask(crate::dialog::Ask {
                title: "Rewind code to before this message?".into(),
                detail: format!("These files go back to how they were:\n{}", files.iter().map(|f| format!("• {f}")).collect::<Vec<_>>().join("\n")),
                buttons: vec!["Rewind".into()],
                danger: true,
                cancel_default: false,
                cancel_label: None,
                on_answer: Rc::new(move |_, _, cx| {
                    let uuid = uuid.clone();
                    weak.update(cx, |v, cx| {
                        v.request(json!({ "subtype": "rewind_files", "user_message_id": uuid }), |_, r, cx| match r {
                            Ok(_) => cx.emit(Request::Message("Files rewound".into())),
                            Err(e) => cx.emit(Request::Error(e)),
                        }, cx)
                    })
                    .ok();
                }),
            }));
        }, cx);
    }

    /// /commands the tab runs itself; false: send it to Claude Code.
    fn local_command(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (name, arg) = cmd.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
        let at = point(window.bounds().size.width / 2., window.bounds().size.height / 2.);
        match name {
            "clear" | "new" => self.clear(cx),
            "resume" => self.open_menu(Menu::History, at, window, cx),
            "model" if arg.is_empty() => self.open_menu(Menu::Model, at, window, cx),
            "model" => self.set_model(arg.into(), cx),
            "mcp" => self.show_mcp(cx),
            "rename" => self.rename(arg, cx),
            "export" => self.export(cx),
            "terminal" => self.open_in_terminal(cx),
            _ => return false,
        }
        true
    }

    fn rename(&mut self, title: &str, cx: &mut Context<Self>) {
        let Some(id) = self.log.session.clone() else { return cx.emit(Request::Error("Send a message first: the conversation has no session yet".into())) };
        match agent::rename_session(&agent::sessions_dir(&self.root), &id, title) {
            Ok(()) => self.title = Some(title.split_whitespace().collect::<Vec<_>>().join(" ")),
            Err(e) => cx.emit(Request::Error(e)),
        }
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let title = self.title();
        let text = self.log.markdown(&title, &self.root);
        let name: String = title.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let pick = cx.prompt_for_new_path(&self.root, Some(&format!("{}.md", name.trim_matches('-'))));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = pick.await else { return };
            let msg = match std::fs::write(&path, text) {
                Ok(()) => Request::Message(format!("Saved {}", path.display())),
                Err(e) => Request::Error(format!("{}: {e}", path.display())),
            };
            this.update(cx, |_, cx| cx.emit(msg)).ok();
        })
        .detach();
    }

    fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        let cmd = match &self.log.session {
            Some(id) => format!("claude --resume {id}"),
            None => "claude".into(),
        };
        self.stop_proc(); // one Claude Code per conversation
        cx.emit(Request::RunInTerminal { dir: self.root.clone(), command: cmd });
    }

    fn show_mcp(&mut self, cx: &mut Context<Self>) {
        self.request(json!({ "subtype": "mcp_status" }), |v, r, cx| {
            let rows = match r {
                Ok(r) => r["mcpServers"].as_array().into_iter().flatten().map(|s| (s["name"].as_str().unwrap_or("").to_string(), s["status"].as_str().unwrap_or("").to_string())).collect(),
                Err(e) => vec![("Error".into(), e)],
            };
            v.info = Some(("MCP servers".into(), if rows.is_empty() { vec![("None configured".into(), String::new())] } else { rows }));
            cx.notify();
        }, cx);
    }

    /// Sends a message now, or after the turn when Claude is working.
    fn say(&mut self, text: &str, cx: &mut Context<Self>) {
        let m = Outgoing { text: text.into(), images: Vec::new(), context: Vec::new() };
        if self.log.busy { self.queue.push(m) } else { self.deliver(m, cx) }
        cx.notify();
    }

    /// Subagents and background commands, parents before their children: (depth, job). Subagents
    /// of an older session (no job events) come from their tool calls.
    fn jobs(&self) -> Vec<(usize, agent::Job)> {
        let mut all = self.log.jobs.clone();
        for it in &self.log.items {
            if let Item::Tool(t) = it
                && matches!(t.name.as_str(), "Task" | "Agent")
                && self.log.job_of(&t.id).is_none()
            {
                let (status, id) = match &t.result {
                    Some((err, text)) => (if *err { "failed" } else { "completed" }, agent::agent_id(text)),
                    None => ("stopped", None),
                };
                all.push(agent::Job {
                    id: id.unwrap_or_else(|| t.id.clone()),
                    kind: "local_agent".into(),
                    what: t.input["description"].as_str().unwrap_or("").into(),
                    agent: t.input["subagent_type"].as_str().unwrap_or("").into(),
                    parent: None,
                    tool_use: Some(t.id.clone()),
                    status: status.into(),
                    last: String::new(),
                });
            }
        }
        fn walk(all: &[agent::Job], parent: Option<&str>, depth: usize, out: &mut Vec<(usize, agent::Job)>) {
            for j in all.iter().filter(|j| j.parent.as_deref() == parent || (parent.is_none() && j.parent.as_ref().is_some_and(|p| !all.iter().any(|x| x.id == *p)))) {
                if out.iter().any(|(_, x)| x.id == j.id) || depth > 8 {
                    continue;
                }
                out.push((depth, j.clone()));
                walk(all, Some(&j.id), depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(&all, None, 0, &mut out);
        out
    }

    /// Stops a subagent or background command.
    fn stop_job(&mut self, id: String, cx: &mut Context<Self>) {
        self.request(json!({ "subtype": "stop_task", "task_id": id }), |v, r, cx| {
            if let Err(e) = r {
                v.error(e);
            }
            cx.notify();
        }, cx);
    }

    /// Asks Claude to continue a stopped subagent (Claude Code continues agents with SendMessage).
    fn resume_job(&mut self, job: &agent::Job, cx: &mut Context<Self>) {
        let what = if job.what.is_empty() { String::new() } else { format!(" (\"{}\")", job.what) };
        self.say(&format!("Resume the stopped subagent{what} where it left off: use SendMessage with to: '{}'.", job.id), cx);
    }

    /// The Stop or Resume button for a job, if it has one.
    fn job_button(&self, id: impl Into<ElementId>, job: &agent::Job, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let j = job.clone();
        if job.running() {
            return Some(btn(id, "Stop", false).on_click(cx.listener(move |v, _, _, cx| {
                cx.stop_propagation();
                v.stop_job(j.id.clone(), cx)
            })));
        }
        (job.is_agent() && matches!(job.status.as_str(), "stopped" | "killed" | "failed") && !job.id.starts_with("toolu_")).then(|| {
            btn(id, "Resume", false).on_click(cx.listener(move |v, _, _, cx| {
                cx.stop_propagation();
                v.resume_job(&j, cx)
            }))
        })
    }

    fn show_usage(&mut self, cx: &mut Context<Self>) {
        self.info = Some(("Usage".into(), vec![("Asking Claude Code…".into(), String::new())]));
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { agent::usage(&root) }).await;
            this.update(cx, |v, cx| {
                let rows = match r {
                    Ok(text) => text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| match l.split_once(": ") {
                            Some((k, val)) => (k.trim().to_string(), val.trim().to_string()),
                            None => (l.trim().to_string(), String::new()),
                        })
                        .collect(),
                    Err(e) => vec![("Error".into(), e)],
                };
                v.info = Some(("Usage".into(), rows));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A forked tab: once its conversation has loaded, back to just before the message `uuid`.
    pub fn fork_from(&mut self, uuid: String) {
        self.fork_at = Some(uuid);
    }

    fn show_context(&mut self, cx: &mut Context<Self>) {
        self.request(json!({ "subtype": "get_context_usage" }), |v, r, cx| {
            let rows = match r {
                Ok(r) => r["categories"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["tokens"].as_u64().unwrap_or(0) > 0)
                    .map(|c| (c["name"].as_str().unwrap_or("").to_string(), format!("{} tokens{}", c["tokens"], if c["isDeferred"] == true { " (deferred)" } else { "" })))
                    .collect(),
                Err(e) => vec![("Error".into(), e)],
            };
            v.info = Some(("Context usage".into(), rows));
            cx.notify();
        }, cx);
    }
}

/// The box's suggestions: /commands at the start, @files after a space.
fn suggestions(files: Rc<RefCell<Vec<String>>>, commands: Rc<RefCell<Vec<(String, String)>>>) -> Rc<dyn Fn(&str) -> Option<(String, Vec<Choice>)>> {
    Rc::new(move |before: &str| {
        let choice = |label: String, kind| Choice { label, kind, suffix: " ", back: 0 };
        if let Some(q) = before.strip_prefix('/').filter(|q| !q.contains(char::is_whitespace)) {
            let q = q.to_lowercase();
            let cmds = commands.borrow();
            let mut hits: Vec<&(String, String)> = cmds.iter().filter(|(n, _)| n.to_lowercase().contains(&q)).collect();
            hits.sort_by_key(|(n, _)| !n.to_lowercase().starts_with(&q));
            return Some((q, hits.into_iter().take(40).map(|(n, _)| choice(n.clone(), Kind::Keyword)).collect()));
        }
        let at = before.rfind('@')?;
        let q = &before[at + 1..];
        if q.contains(char::is_whitespace) || !before[..at].chars().last().is_none_or(char::is_whitespace) {
            return None;
        }
        let files = files.borrow();
        let mut hits: Vec<(i64, &String)> = files.iter().filter_map(|f| crate::util::fuzzy(q, f).map(|(s, _)| (s, f))).collect();
        // Only the best 40 are shown: find them, then sort just those.
        let key = |h: &(i64, &String)| (-h.0, h.1.len());
        if hits.len() > 40 {
            hits.select_nth_unstable_by_key(40, key);
            hits.truncate(40);
        }
        hits.sort_by_key(key);
        Some((q.to_string(), hits.into_iter().take(40).map(|(_, f)| choice(f.clone(), Kind::File)).collect()))
    })
}

// ---------------------------------------------------------------- drawing

fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .px(px(10.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(px(12.))
        .when(primary, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).hover(|s| s.bg(theme::accent_hover())))
        .when(!primary, |d| d.border_1().border_color(theme::border_input()).text_color(theme::fg()).hover(|s| s.bg(theme::hover())))
        .child(label.into())
}

/// A small text button in the footer (mode, model, effort).
fn pill(id: &'static str, label: String) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(3.))
        .h(px(22.))
        .px(px(6.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(theme::fg_dim())
        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
        .child(label)
        .child(ui::small_icon("chevron-down", theme::fg_muted()).size(px(10.)))
}

fn menu_row(id: impl Into<ElementId>, label: impl Into<SharedString>, detail: impl Into<SharedString>, checked: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .px(px(10.))
        .py(px(4.))
        .mx(px(4.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::selected_focused()))
        .child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).child(div().w(px(12.)).child(if checked { "✓" } else { "" })).child(label.into()))
        .child(div().pl(px(18.)).text_size(px(11.)).text_color(theme::fg_muted()).child(detail.into()))
}

fn mono_box(text: String) -> Div {
    let lines: Vec<&str> = text.lines().collect();
    let shown = if lines.len() > 40 { format!("{}\n… {} more lines", lines[..40].join("\n"), lines.len() - 40) } else { text.clone() };
    div()
        .rounded(px(4.))
        .bg(theme::fg().opacity(0.05))
        .px(px(8.))
        .py(px(6.))
        .font_family(theme::MONO_FONT)
        .text_size(px(11.5))
        .line_height(relative(1.4))
        .text_color(theme::fg_dim())
        .whitespace_normal()
        .child(shown)
}

/// An edit's changed lines (at most 400), worked out once per edit.
fn diff_lines(old: &str, new: &str) -> Vec<(similar::ChangeTag, String)> {
    similar::TextDiff::from_lines(old, new).iter_all_changes().take(400).map(|ch| (ch.tag(), ch.value().trim_end_matches('\n').to_string())).collect()
}

/// The lines an edit changes, removed red and added green.
fn diff_box(lines: &[(similar::ChangeTag, String)]) -> Div {
    let mut rows = Vec::new();
    for (tag, text) in lines {
        let (sign, bg) = match tag {
            similar::ChangeTag::Delete => ("-", theme::diff_del_line()),
            similar::ChangeTag::Insert => ("+", theme::diff_ins_line()),
            similar::ChangeTag::Equal => (" ", gpui::transparent_black()),
        };
        rows.push(div().bg(bg).px(px(6.)).whitespace_nowrap().child(format!("{sign} {text}")));
    }
    div()
        .rounded(px(4.))
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .font_family(theme::MONO_FONT)
        .text_size(px(11.5))
        .line_height(relative(1.4))
        .flex()
        .flex_col()
        .children(rows)
}

/// What a tool card shows when open: an edit's diff, a command, the input, then the result.
/// `diff(k, old, new)` gives edit k's lines.
fn tool_body(t: &agent::Tool, diff: &dyn Fn(usize, &str, &str) -> Rc<Vec<(similar::ChangeTag, String)>>) -> Vec<AnyElement> {
    let i = &t.input;
    let s = |k: &str| i[k].as_str().unwrap_or("").to_string();
    let mut out: Vec<AnyElement> = Vec::new();
    match t.name.as_str() {
        "Edit" => out.push(diff_box(&diff(0, i["old_string"].as_str().unwrap_or(""), i["new_string"].as_str().unwrap_or(""))).into_any_element()),
        "MultiEdit" => {
            for (k, e) in i["edits"].as_array().into_iter().flatten().enumerate() {
                out.push(diff_box(&diff(k, e["old_string"].as_str().unwrap_or(""), e["new_string"].as_str().unwrap_or(""))).into_any_element());
            }
        }
        "Write" => out.push(diff_box(&diff(0, "", i["content"].as_str().unwrap_or(""))).into_any_element()),
        "Bash" => out.push(mono_box(format!("$ {}", s("command"))).into_any_element()),
        "Read" | "Glob" | "Grep" | "LS" | "WebFetch" | "WebSearch" | "TodoWrite" | "Task" | "Agent" => {}
        _ => out.push(mono_box(serde_json::to_string_pretty(i).unwrap_or_default()).into_any_element()),
    }
    if let Some((err, text)) = &t.result
        && !text.trim().is_empty()
        && !matches!(t.name.as_str(), "Edit" | "MultiEdit" | "Write")
    {
        out.push(mono_box(text.clone()).when(*err, |d| d.text_color(theme::git_deleted())).into_any_element());
    }
    out
}

impl AgentView {
    fn render_item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Markdown is parsed when a message's text changes, not on every frame.
        let text = match self.log.items.get(ix) {
            Some(Item::Text(t)) => Some(t.as_str()),
            Some(Item::Ask(q)) if q.tool == "ExitPlanMode" => q.input["plan"].as_str(),
            _ => None,
        };
        if let Some(text) = text
            && self.md.get(&ix).is_none_or(|(len, _)| *len != text.len())
        {
            self.md.insert(ix, (text.len(), Rc::new(Markdown::parse(text, &self.root))));
        }
        self.draw_item(ix, cx)
    }

    /// One message, drawn from the log as it is (nothing copied but the text shown).
    fn draw_item(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.log.items.get(ix) else { return div().into_any_element() };
        let root = &self.root;
        let body: AnyElement = match item {
            Item::User { text, images, context, uuid, .. } => {
                let images = *images;
                let chips = context
                    .iter()
                    .filter_map(|c| between_tags(c))
                    .chain((images > 0).then(|| format!("{images} picture{}", if images == 1 { "" } else { "s" })))
                    .map(|c| div().px(px(6.)).rounded(px(3.)).bg(theme::badge()).text_size(px(11.)).text_color(theme::fg_dim()).child(c));
                let text2 = text.clone();
                let actions = div()
                    .flex()
                    .gap(px(2.))
                    .invisible()
                    .group_hover("user-msg", |s| s.visible())
                    .child(ui::icon_button(("edit", ix), "new-file").tooltip(ui::tooltip("Edit: fork the conversation from here")).on_click(cx.listener(move |v, _, w, cx| {
                        if let Some(t) = v.rewind_chat(ix) {
                            v.input.update(cx, |e, cx| e.set_text(&t, cx));
                            w.focus(&v.input.read(cx).focus_target(cx), cx);
                        }
                        cx.notify();
                    })))
                    .when_some(uuid.clone().zip(self.log.session.clone()), |d, (uuid, session)| {
                        d.child(ui::icon_button(("fork", ix), "split").tooltip(ui::tooltip("Fork: continue from before this message in a new tab")).on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(Request::ForkChat { session: session.clone(), uuid: uuid.clone() });
                        })))
                    })
                    .child(ui::icon_button(("retry", ix), "refresh").tooltip(ui::tooltip("Retry from here")).on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(t) = v.rewind_chat(ix) {
                            let context = v.context_blocks(cx);
                            v.deliver(Outgoing { text: t, images: Vec::new(), context }, cx);
                        }
                        cx.notify();
                    })))
                    .when(uuid.is_some(), |d| {
                        d.child(ui::icon_button(("rewind", ix), "discard").tooltip(ui::tooltip("Rewind code to before this message")).on_click(cx.listener(move |v, _, _, cx| v.rewind_code(ix, cx))))
                    })
                    .child(ui::icon_button(("copy-u", ix), "copy").tooltip(ui::tooltip("Copy")).on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text2.clone()))));
                div()
                    .group("user-msg")
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(6.))
                            .bg(theme::bg_input())
                            .border_1()
                            .border_color(theme::border_input())
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().font_family(".SystemUIFont").text_size(px(13.)).line_height(relative(1.5)).text_color(theme::fg_bright()).whitespace_normal().child(text.clone()))
                            .child(div().flex().flex_wrap().gap(px(4.)).children(chips)),
                    )
                    .child(div().flex().justify_end().child(actions))
                    .into_any_element()
            }
            Item::Text(text) => {
                let md = self.md.get(&ix).map(|(_, md)| md.clone()).unwrap_or_else(|| Rc::new(Markdown::parse(text, root)));
                let open = self.opener(cx);
                let text = text.clone();
                div()
                    .group("text-msg")
                    .relative()
                    .child(md.render(root, &self.md_images, open))
                    .child(
                        div()
                            .absolute()
                            .top(px(-4.))
                            .right(px(0.))
                            .invisible()
                            .group_hover("text-msg", |s| s.visible())
                            .child(ui::icon_button(("copy-t", ix), "copy").tooltip(ui::tooltip("Copy")).on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())))),
                    )
                    .into_any_element()
            }
            Item::Thinking(text) => {
                let open = self.toggled.contains(&ix);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .id(("think", ix))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .italic()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.text_color(theme::fg_dim()))
                            .on_click(cx.listener(move |v, _, _, cx| v.toggle(ix, cx)))
                            .child(if open { "∴ Thinking ▾" } else { "∴ Thinking ▸" }),
                    )
                    .when(open, |d| d.child(div().pl(px(12.)).border_l_2().border_color(theme::border()).text_size(px(12.)).text_color(theme::fg_dim()).whitespace_normal().child(text.clone())))
                    .into_any_element()
            }
            Item::Tool(t) => self.render_tool(ix, t, cx),
            Item::Ask(q) => self.render_ask(ix, q, cx),
            Item::Error(e) => div()
                .px(px(10.))
                .py(px(6.))
                .rounded(px(4.))
                .bg(theme::diff_del_line())
                .text_size(px(12.))
                .text_color(theme::fg_bright())
                .whitespace_normal()
                .child(e.clone())
                .into_any_element(),
            Item::Note(n) => div().flex().justify_center().text_size(px(11.)).text_color(theme::fg_muted()).child(n.clone()).into_any_element(),
        };
        div().id(("item", ix)).w_full().px(px(16.)).py(px(6.)).child(div().max_w(px(860.)).mx_auto().child(body)).into_any_element()
    }

    /// Edit diffs for item `ix`, worked out once.
    fn differ(&self, ix: usize) -> impl Fn(usize, &str, &str) -> Rc<Vec<(similar::ChangeTag, String)>> + '_ {
        move |k, old, new| self.diffs.borrow_mut().entry((ix, k)).or_insert_with(|| Rc::new(diff_lines(old, new))).clone()
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.toggled.remove(&ix) {
            self.toggled.insert(ix);
        }
        self.list.splice(ix..ix + 1, 1);
        cx.notify();
    }

    /// Opens a file Claude mentioned: in the editor, or ⌘-click in its default app.
    fn opener(&self, cx: &mut Context<Self>) -> crate::preview::Opener {
        let weak = cx.entity().downgrade();
        Rc::new(move |path, line, window, cx| open_file(&weak, path, line, window, cx))
    }

    fn render_tool(&self, ix: usize, t: &agent::Tool, cx: &mut Context<Self>) -> AnyElement {
        let edit = matches!(t.name.as_str(), "Edit" | "MultiEdit" | "Write");
        let open = self.toggled.contains(&ix) != edit; // edits show their diff unless closed
        let (dot, color) = match &t.result {
            None if self.log.busy => ("◌", theme::accent()),
            None => ("○", theme::fg_muted()),
            Some((true, _)) => ("✕", theme::git_deleted()),
            Some((false, _)) => ("●", theme::git_added()),
        };
        let target = agent::tool_target(&t.input, &self.root);
        let file = t.input["file_path"].as_str().or(t.input["notebook_path"].as_str()).map(PathBuf::from);
        let line = t.input["offset"].as_u64().map(|l| l as usize);
        let job = self.log.job_of(&t.id).cloned().or_else(|| {
            matches!(t.name.as_str(), "Task" | "Agent").then(|| self.jobs().into_iter().map(|(_, j)| j).find(|j| j.tool_use.as_deref() == Some(&t.id))).flatten()
        });
        let job_btn = job.as_ref().and_then(|j| self.job_button(("tool-job", ix), j, cx));
        let header = div()
            .id(("tool", ix))
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .text_size(px(12.))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(ix, cx)))
            .child(div().text_color(color).child(dot))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child(t.name.clone()))
            .child(
                div()
                    .id(("tool-target", ix))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(11.5))
                    .text_color(theme::fg_dim())
                    .when_some(file, |d, path| {
                        let weak = cx.entity().downgrade();
                        d.text_color(theme::accent()).hover(|s| s.underline()).tooltip(ui::tooltip("Open (⌘-click: in its default app)")).on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            open_file(&weak, path.clone(), line, window, cx)
                        })
                    })
                    .child(target),
            )
            .children(job.as_ref().filter(|j| !j.running() && j.status != "completed").map(|j| div().text_size(px(11.)).text_color(theme::fg_muted()).child(j.status.clone())))
            .children(job_btn);
        // A subagent: the latest step shown, all of them when open.
        let steps = (!t.steps.is_empty()).then(|| {
            let n = t.steps.len();
            let shown: Vec<&(String, String)> = if open { t.steps.iter().collect() } else { t.steps.iter().skip(n - 1).collect() };
            div()
                .pl(px(18.))
                .flex()
                .flex_col()
                .text_size(px(11.))
                .text_color(theme::fg_muted())
                .child(format!("{n} step{}", if n == 1 { "" } else { "s" }))
                .children(shown.into_iter().map(|(tool, target)| div().pl(px(8.)).overflow_hidden().whitespace_nowrap().child(format!("↳ {tool} {target}"))))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(header)
            .children(steps)
            .when(open, |d| d.child(div().pl(px(18.)).flex().flex_col().gap(px(4.)).children(tool_body(t, &self.differ(ix)))))
            .into_any_element()
    }

    fn render_ask(&self, ix: usize, q: &agent::Ask, cx: &mut Context<Self>) -> AnyElement {
        let card = div().flex().flex_col().gap(px(8.)).p(px(10.)).rounded(px(6.)).border_1().border_color(if q.answer.is_none() { theme::accent() } else { theme::border() });
        if let Some(a) = q.answer {
            let what = match (q.tool.as_str(), a) {
                ("ExitPlanMode", Answer::Deny) => "Kept planning".to_string(),
                ("ExitPlanMode", _) => "Plan approved".to_string(),
                ("AskUserQuestion", Answer::Deny) => "Question dismissed".to_string(),
                ("AskUserQuestion", _) => "Answered".to_string(),
                (_, Answer::Deny) => format!("Denied {}", q.tool),
                (_, Answer::Always) => format!("Allowed {} (and from now on)", q.tool),
                (_, Answer::Allow) => format!("Allowed {}", q.tool),
            };
            let mut card = card.child(div().text_size(px(12.)).text_color(theme::fg_dim()).child(what));
            if q.tool == "ExitPlanMode" {
                card = card.children(self.md.get(&ix).map(|(_, md)| md.render(&self.root, &self.md_images, self.opener(cx))));
            }
            return card.into_any_element();
        }
        match q.tool.as_str() {
            "AskUserQuestion" => {
                let qs = q.input["questions"].as_array().cloned().unwrap_or_default();
                let picks = self.picks.get(&q.id).cloned().unwrap_or_default();
                let quick = qs.len() == 1 && qs[0]["multiSelect"] != true;
                let mut card = card;
                for (qi, qq) in qs.iter().enumerate() {
                    let multi = qq["multiSelect"] == true;
                    let options = qq["options"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(oi, o)| {
                        let label = o["label"].as_str().unwrap_or("").to_string();
                        let on = picks.get(qi).is_some_and(|p| p.contains(&label));
                        let id = q.id.clone();
                        div()
                            .id(SharedString::from(format!("opt-{ix}-{qi}-{oi}")))
                            .flex()
                            .flex_col()
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if on { theme::accent() } else { theme::border_input() })
                            .when(on, |d| d.bg(theme::selected_focused()))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .child(div().text_size(px(12.)).child(label.clone()))
                            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(o["description"].as_str().unwrap_or("").to_string()))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                let p = v.picks.entry(id.clone()).or_default();
                                p.resize(p.len().max(qi + 1), Vec::new());
                                let chosen = &mut p[qi];
                                if multi {
                                    if let Some(k) = chosen.iter().position(|l| *l == label) {
                                        chosen.remove(k);
                                    } else {
                                        chosen.push(label.clone());
                                    }
                                } else {
                                    *chosen = vec![label.clone()];
                                }
                                if quick {
                                    v.submit_answers(ix, cx);
                                } else {
                                    v.list.splice(ix..ix + 1, 1);
                                }
                                cx.notify();
                            }))
                    });
                    card = card
                        .child(div().flex().items_center().gap(px(6.)).child(div().px(px(5.)).rounded(px(3.)).bg(theme::badge()).text_size(px(10.5)).child(qq["header"].as_str().unwrap_or("").to_string())).child(div().text_size(px(12.5)).text_color(theme::fg_bright()).child(qq["question"].as_str().unwrap_or("").to_string())))
                        .child(div().flex().flex_col().gap(px(4.)).children(options));
                }
                card.child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .items_center()
                        .when(!quick, |d| d.child(btn(("submit-q", ix), "Submit", true).on_click(cx.listener(move |v, _, _, cx| v.submit_answers(ix, cx)))))
                        .child(btn(("skip-q", ix), "Skip", false).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Deny, None, None, cx))))
                        .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("or type your own answer below")),
                )
                .into_any_element()
            }
            "ExitPlanMode" => card
                .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child("Claude's plan is ready. Proceed?"))
                .children(self.md.get(&ix).map(|(_, md)| md.render(&self.root, &self.md_images, self.opener(cx))))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(btn(("plan-auto", ix), "Yes, and auto-accept edits", true).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Allow, None, Some("acceptEdits"), cx))))
                        .child(btn(("plan-manual", ix), "Yes, and manually approve edits", false).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Allow, None, Some("default"), cx))))
                        .child(btn(("plan-no", ix), "No, keep planning", false).tooltip(ui::tooltip("Type below what to change, then click")).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Deny, None, None, cx)))),
                )
                .into_any_element(),
            tool => {
                let preview = tool_body(&agent::Tool { id: String::new(), name: tool.into(), input: q.input.clone(), result: None, steps: Vec::new() }, &self.differ(ix));
                let target = agent::tool_target(&q.input, &self.root);
                let always = q.suggestions.as_array().is_some_and(|s| !s.is_empty());
                card.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme::fg_bright())
                        .child(format!("Allow Claude to use {tool}{}?", if target.is_empty() || tool == "Bash" { String::new() } else { format!(" on {target}") })),
                )
                .children(preview)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(btn(("allow", ix), "Yes", true).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Allow, None, None, cx))))
                        .when(always, |d| d.child(btn(("always", ix), "Yes, and don't ask again", false).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Always, None, None, cx)))))
                        .child(btn(("deny", ix), "No", false).tooltip(ui::tooltip("Type below what Claude should do instead, then click")).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, Answer::Deny, None, None, cx)))),
                )
                .into_any_element()
            }
        }
    }

    fn submit_answers(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Item::Ask(q)) = self.log.items.get(ix).cloned() else { return };
        let picks = self.picks.get(&q.id).cloned().unwrap_or_default();
        let answers: serde_json::Map<String, Value> = q.input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, qq)| (qq["question"].as_str().unwrap_or("").to_string(), Value::String(picks.get(i).map(|p| p.join(", ")).unwrap_or_default())))
            .collect();
        let mut input = q.input.clone();
        input["answers"] = Value::Object(answers);
        self.answer(ix, Answer::Allow, Some(input), None, cx);
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cost = (self.log.cost > 0.).then(|| format!("${:.2}", self.log.cost));
        let jobs = self.jobs();
        let running = jobs.iter().filter(|(_, j)| j.running()).count();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(32.))
            .px(px(10.))
            .border_b_1()
            .border_color(theme::border())
            .child(ui::small_icon("sparkle", theme::accent()))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(12.)).text_color(theme::fg_bright()).child(self.title()))
            .children(cost.map(|c| div().text_size(px(11.)).text_color(theme::fg_muted()).child(c)))
            .when(!jobs.is_empty(), |d| {
                d.child(
                    div()
                        .id("agent-jobs")
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .px(px(5.))
                        .h(px(22.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("Subagents and background jobs"))
                        .on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Agents, e.position, w, cx)))
                        .child(ui::small_icon("list-tree", theme::fg()))
                        .when(running > 0, |d| d.child(div().text_size(px(11.)).text_color(theme::accent()).child(running.to_string()))),
                )
            })
            .child(ui::icon_button("agent-history", "history").tooltip(ui::tooltip("Past conversations")).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::History, e.position, w, cx))))
            .child(ui::icon_button("agent-new", "plus").tooltip(ui::tooltip("New conversation")).on_click(cx.listener(|v, _, _, cx| v.clear(cx))))
            .child(ui::icon_button("agent-more", "ellipsis").tooltip(ui::tooltip("More")).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::More, e.position, w, cx))))
    }

    fn render_bottom(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tasks = self.log.tasks();
        let done = tasks.iter().filter(|t| t.1 == "completed").count();
        let tasks_box = (!tasks.is_empty()).then(|| {
            let rows = tasks.iter().map(|(text, status, doing)| {
                let (mark, color) = match status.as_str() {
                    "completed" => ("☑", theme::fg_muted()),
                    "in_progress" => ("◐", theme::accent()),
                    _ => ("☐", theme::fg_dim()),
                };
                div()
                    .flex()
                    .gap(px(6.))
                    .text_size(px(11.5))
                    .text_color(color)
                    .child(mark)
                    .child(div().when(status == "completed", |d| d.line_through()).child(if status == "in_progress" && !doing.is_empty() { doing.clone() } else { text.clone() }))
            });
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(10.))
                .py(px(6.))
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .id("agent-tasks")
                        .cursor_pointer()
                        .text_size(px(11.))
                        .text_color(theme::fg_dim())
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.tasks_open = !v.tasks_open;
                            cx.notify();
                        }))
                        .child(format!("Tasks {done}/{} {}", tasks.len(), if self.tasks_open { "▾" } else { "▸" })),
                )
                .when(self.tasks_open, |d| d.children(rows))
        });
        let queued = self.queue.iter().enumerate().map(|(i, m)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .text_size(px(11.5))
                .text_color(theme::fg_dim())
                .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(format!("Queued: {}", m.text)))
                .child(ui::icon_button(("unqueue", i), "close").on_click(cx.listener(move |v, _, _, cx| {
                    if i < v.queue.len() {
                        v.queue.remove(i);
                    }
                    cx.notify();
                })))
        });
        let editor_chip = self.editor.borrow().as_ref().and_then(|w| w.upgrade()).and_then(|ed| {
            let ed = ed.read(cx);
            let name = ed.path.as_ref()?.file_name()?.to_string_lossy().to_string();
            let r = ed.buf.sel.range();
            let (a, b) = (ed.buf.pos_to_rc(r.start).0 + 1, ed.buf.pos_to_rc(r.end).0 + 1);
            Some(if r.is_empty() { name } else if a == b { format!("{name}:{a}") } else { format!("{name}:{a}-{b}") })
        });
        let share = self.share_editor;
        let chips = div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .children(editor_chip.map(|label| {
                div()
                    .id("agent-ctx")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .h(px(20.))
                    .rounded(px(3.))
                    .bg(theme::badge())
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(if share { theme::fg() } else { theme::fg_muted() })
                    .when(!share, |d| d.line_through())
                    .tooltip(ui::tooltip("The open file (and selection) goes with your message. Click to switch off."))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.share_editor = !v.share_editor;
                        cx.notify();
                    }))
                    .child(ui::small_icon("eye", theme::fg_dim()).size(px(11.)))
                    .child(label)
            }))
            .children(self.images.iter().enumerate().map(|(i, (mt, _))| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(6.))
                    .h(px(20.))
                    .rounded(px(3.))
                    .bg(theme::badge())
                    .text_size(px(11.))
                    .child(format!("Picture {} ({})", i + 1, mt.trim_start_matches("image/")))
                    .child(ui::icon_button(("unimage", i), "close").size(px(18.)).on_click(cx.listener(move |v, _, _, cx| {
                        if i < v.images.len() {
                            v.images.remove(i);
                        }
                        cx.notify();
                    })))
            }));
        let mode_label = MODES.iter().find(|(m, _)| *m == self.mode).map_or(self.mode.clone(), |(_, l)| l.to_string());
        let model_label = self.model_info().and_then(|m| m["displayName"].as_str()).map(String::from).unwrap_or_else(|| if self.model.is_empty() { "Default model".into() } else { self.model.clone() });
        let effort_label = if self.effort.is_empty() { "Effort".to_string() } else { format!("Effort: {}", self.effort) };
        let ctx = self.log.context.map(|(used, window)| (used as f64 / window.max(1) as f64 * 100.).round() as u32);
        let busy = self.log.busy;
        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .child(pill("agent-mode", mode_label).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Mode, e.position, w, cx))))
            .child(pill("agent-model", model_label).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Model, e.position, w, cx))))
            .when(!self.styles.is_empty(), |d| {
                let label = if self.style.is_empty() || self.style == "default" { "Style".to_string() } else { format!("Style: {}", self.style) };
                d.child(pill("agent-style", label).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Style, e.position, w, cx))))
            })
            .child(pill("agent-effort", effort_label).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Effort, e.position, w, cx))))
            .child(div().flex_1())
            .children(ctx.map(|p| {
                div()
                    .id("agent-context")
                    .px(px(4.))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(if p >= 80 { theme::git_modified() } else { theme::fg_muted() })
                    .tooltip(ui::tooltip("Context used (click for details; /compact frees it)"))
                    .on_click(cx.listener(|v, _, _, cx| v.show_context(cx)))
                    .child(format!("{p}% context"))
            }))
            .child(ui::icon_button("agent-attach", "paperclip").tooltip(ui::tooltip("Attach files or pictures")).on_click(cx.listener(|_, _, window, cx| {
                let pick = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(paths))) = pick.await else { return };
                    this.update_in(cx, |v, w, cx| v.add_paths(paths, w, cx)).ok();
                })
                .detach();
            })))
            .child(if busy {
                ui::icon_button("agent-stop", "stop-filled").tooltip(ui::tooltip("Stop (Esc)")).on_click(cx.listener(|v, _, _, cx| v.interrupt(cx)))
            } else {
                ui::icon_button("agent-send", "send").tooltip(ui::tooltip("Send (Enter)")).on_click(cx.listener(|v, _, w, cx| v.submit(w, cx)))
            });
        div()
            .flex()
            .flex_none()
            .flex_col()
            .border_t_1()
            .border_color(theme::border())
            .children(tasks_box)
            .children(queued)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(10.))
                    .child(chips)
                    .child(
                        div()
                            .id("agent-input")
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(if self.mode == "plan" { theme::git_modified() } else { theme::border_input() })
                            .bg(theme::bg_input())
                            .child(self.input.clone()),
                    )
                    .child(footer)
                    .children(self.log.status.clone().map(|s| div().text_size(px(10.5)).text_color(theme::fg_muted()).child(s))),
            )
    }

    fn render_menu(&self, menu: Menu, cx: &mut Context<Self>) -> AnyElement {
        let base = div()
            .id("agent-menu")
            .occlude()
            .min_w(px(240.))
            .max_w(px(420.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|v, _, window, cx| {
                v.just_closed = v.menu.take().map(|(m, _)| m);
                window.focus(&v.input.read(cx).focus_target(cx), cx);
                cx.defer_in(window, |v, _, _| v.just_closed = None);
                cx.notify();
            }));
        match menu {
            Menu::Mode => base
                .children(MODES.iter().enumerate().map(|(i, (m, label))| {
                    let detail = match *m {
                        "default" => "Claude asks before each edit or command",
                        "acceptEdits" => "Edits are made without asking; commands still ask",
                        "plan" => "Claude explores and plans, changes nothing until you approve",
                        "auto" => "Claude decides what's safe to run without asking",
                        _ => "Nothing asks first (only in a sandbox you trust)",
                    };
                    menu_row(("mode", i), *label, detail, self.mode == *m).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        v.pick_mode(m, cx);
                    }))
                }))
                .into_any_element(),
            Menu::Model => {
                let rows = self.models.iter().enumerate().map(|(i, m)| {
                    let value = m["value"].as_str().unwrap_or("").to_string();
                    let on = if self.model.is_empty() { value == "default" } else { value == self.model };
                    menu_row(("model", i), m["displayName"].as_str().unwrap_or("").to_string(), m["description"].as_str().unwrap_or("").to_string(), on).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        v.set_model(if value == "default" { String::new() } else { value.clone() }, cx);
                    }))
                });
                base.when(self.models.is_empty(), |d| d.child(div().px(px(10.)).text_size(px(12.)).text_color(theme::fg_muted()).child("Asking Claude Code…"))).children(rows).into_any_element()
            }
            Menu::Style => base
                .children(self.styles.iter().enumerate().map(|(i, st)| {
                    let st = st.clone();
                    let on = self.style == st || (self.style.is_empty() && st == "default");
                    menu_row(("style", i), st.clone(), "How Claude writes its answers (from the next message)", on).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        if v.style != st {
                            v.style = st.clone();
                            v.restart = v.proc.is_some();
                        }
                        cx.notify();
                    }))
                }))
                .into_any_element(),
            Menu::Agents => {
                let rows: Vec<_> = self
                    .jobs()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (depth, j))| {
                        let color = match j.status.as_str() {
                            "running" => theme::accent(),
                            "completed" => theme::git_added(),
                            "failed" => theme::git_deleted(),
                            _ => theme::fg_muted(),
                        };
                        let kind = if j.is_agent() { format!("Agent · {}", if j.agent.is_empty() { "subagent" } else { &j.agent }) } else { "Command".to_string() };
                        let item = j.tool_use.as_ref().and_then(|t| self.log.items.iter().position(|it| matches!(it, Item::Tool(x) if x.id == *t)));
                        div()
                            .id(("job", i))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pl(px(10. + 14. * depth as f32))
                            .pr(px(6.))
                            .py(px(4.))
                            .mx(px(4.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::selected_focused()))
                            .when_some(item, |d, ix| {
                                d.on_click(cx.listener(move |v, _, _, cx| {
                                    v.menu = None;
                                    v.toggled.insert(ix);
                                    v.list.splice(ix..ix + 1, 1);
                                    v.list.scroll_to_reveal_item(ix);
                                    cx.notify();
                                }))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(6.))
                                            .text_size(px(12.))
                                            .child(div().text_color(color).child(j.status.clone()))
                                            .child(div().font_weight(FontWeight::SEMIBOLD).child(kind))
                                            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(j.what.clone())),
                                    )
                                    .when(!j.last.is_empty(), |d| d.child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_muted()).child(j.last.clone()))),
                            )
                            .children(self.job_button(("job-btn", i), &j, cx))
                    })
                    .collect();
                base.w(px(460.))
                    .child(div().px(px(10.)).pb(px(4.)).text_size(px(11.)).text_color(theme::fg_muted()).child("Subagents and background jobs (Stop ends one; a stopped subagent can resume)"))
                    .children(rows)
                    .into_any_element()
            }
            Menu::Effort => {
                let levels: Vec<String> = self.model_info().and_then(|m| m["supportedEffortLevels"].as_array()).map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let rows = std::iter::once(String::new()).chain(levels).enumerate().map(|(i, level)| {
                    let label = if level.is_empty() { "Default".to_string() } else { level.clone() };
                    menu_row(("effort", i), label, if level.is_empty() { "Your Claude Code setting" } else { "How hard Claude thinks (applies from the next message)" }, self.effort == level).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        if v.effort != level {
                            v.effort = level.clone();
                            v.restart = v.proc.is_some();
                        }
                        cx.notify();
                    }))
                });
                base.children(rows).into_any_element()
            }
            Menu::More => {
                let items: [(&str, &str); 6] = [
                    ("MCP servers", "Their status (/mcp)"),
                    ("Context usage", "What fills the context window"),
                    ("Compact conversation", "Summarize to free context (/compact)"),
                    ("Usage limits", "Your plan's limits and what's left"),
                    ("Export as Markdown…", "Save this conversation"),
                    ("Continue in terminal", "claude --resume in a terminal tab"),
                ];
                base.children(items.into_iter().enumerate().map(|(i, (label, detail))| {
                    menu_row(("more", i), label, detail, false).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        match i {
                            0 => v.show_mcp(cx),
                            1 => v.show_context(cx),
                            2 => v.say("/compact", cx),
                            3 => v.show_usage(cx),
                            4 => v.export(cx),
                            _ => v.open_in_terminal(cx),
                        }
                        cx.notify();
                    }))
                }))
                .into_any_element()
            }
            Menu::History => {
                let rows = self.sessions.iter().take(200).enumerate().map(|(i, s)| {
                    let (id, title) = (s.id.clone(), s.title.clone());
                    let id2 = id.clone();
                    div()
                        .id(("session", i))
                        .group("session-row")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(4.))
                        .mx(px(4.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::selected_focused()))
                        .when(self.log.session.as_deref() == Some(&s.id), |d| d.bg(theme::selected()))
                        .on_click(cx.listener(move |v, _, w, cx| {
                            v.load(&id, Some(title.clone()), cx);
                            w.focus(&v.input.read(cx).focus_target(cx), cx);
                        }))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(12.)).child(s.title.clone()))
                        .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(crate::util::relative_time(s.updated)))
                        .when(self.log.session.as_deref() != Some(&s.id), |d| {
                            let title = s.title.clone();
                            d.child(div().invisible().group_hover("session-row", |s| s.visible()).child(ui::icon_button(("del-session", i), "trash").tooltip(ui::tooltip("Move to Trash")).on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                let (weak, id) = (cx.entity().downgrade(), id2.clone());
                                cx.emit(Request::Ask(crate::dialog::Ask {
                                    title: format!("Move \"{title}\" to the Trash?"),
                                    detail: "The conversation's file goes to the Trash (you can put it back from there).".into(),
                                    buttons: vec!["Move to Trash".into()],
                                    danger: true,
                                    cancel_default: false,
                                    cancel_label: None,
                                    on_answer: Rc::new(move |_, _, cx| {
                                        weak.update(cx, |v, cx| match agent::trash_session(&agent::sessions_dir(&v.root), &id) {
                                            Ok(()) => v.refresh_history(cx),
                                            Err(e) => cx.emit(Request::Error(e)),
                                        })
                                        .ok();
                                    }),
                                }));
                            }))))
                        })
                });
                base.w(px(420.))
                    .child(div().px(px(8.)).pb(px(4.)).child(div().px(px(6.)).rounded(px(4.)).border_1().border_color(theme::border_input()).bg(theme::bg_input()).child(self.search.clone())))
                    .when(self.sessions.is_empty(), |d| d.child(div().px(px(10.)).py(px(4.)).text_size(px(12.)).text_color(theme::fg_muted()).child("No conversations")))
                    .children(rows)
                    .into_any_element()
            }
        }
    }

    fn render_info(&self, title: &str, rows: &[(String, String)], cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("agent-info")
            .occlude()
            .absolute()
            .top(px(40.))
            .right(px(12.))
            .w(px(360.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .p(px(10.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.info = None;
                cx.notify();
            }))
            .child(div().pb(px(6.)).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
            .children(rows.iter().map(|(k, v)| div().flex().gap(px(8.)).text_size(px(12.)).child(div().flex_1().child(k.clone())).child(div().text_color(theme::fg_dim()).child(v.clone()))))
            .into_any_element()
    }
}

/// Opens a file in the editor at `line`, or with ⌘ held in the app macOS opens it with.
fn open_file(weak: &WeakEntity<AgentView>, path: PathBuf, line: Option<usize>, window: &mut Window, cx: &mut App) {
    if window.modifiers().platform {
        // A path from Claude's reply could point anywhere: only the project's files open in their
        // app, and never something that runs (those are shown in Finder).
        const RUNS: [&str; 12] = ["app", "command", "sh", "zsh", "bash", "terminal", "tool", "pkg", "dmg", "scpt", "workflow", "jar"];
        let root = weak.upgrade().map(|v| v.read(cx).root.clone()).and_then(|r| r.canonicalize().ok());
        let inside = path.canonicalize().ok().zip(root).is_some_and(|(p, r)| p.starts_with(r));
        let runs = path.extension().is_some_and(|e| RUNS.contains(&e.to_string_lossy().to_lowercase().as_str()));
        return match (inside, runs) {
            (true, false) => cx.open_with_system(&path),
            _ => cx.reveal_path(&path),
        };
    }
    weak.update(cx, |_, cx| cx.emit(Request::OpenFile { path, line, preview: false })).ok();
}

/// "a.rs:3-9" for an <ide_selection> block, "a.rs" for <ide_opened_file>.
fn between_tags(block: &str) -> Option<String> {
    let path = block.split_once(" from ").map(|(_, r)| r).or_else(|| block.split_once(" the file ").map(|(_, r)| r))?;
    let path = path.split([':', ' ']).next()?;
    let name = Path::new(path).file_name()?.to_string_lossy().to_string();
    let lines = block.split_once("the lines ").and_then(|(_, r)| r.split_once(" from")).map(|(l, _)| l.replace(" to ", "-"));
    Some(match lines {
        Some(l) => format!("{name}:{l}"),
        None => name,
    })
}

impl Render for AgentView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.log.items.is_empty();
        let log: AnyElement = if empty {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(24.))
                .text_center()
                .font_family(".SystemUIFont")
                .text_color(theme::fg_muted())
                .child(ui::icon("sparkle", theme::accent()).size(px(28.)))
                .child(div().text_size(px(14.)).text_color(theme::fg()).child("Claude Code"))
                .child(div().text_size(px(12.)).child("Uses the Claude Code installed on this Mac, signed in as you."))
                .child(div().text_size(px(11.5)).child("@ mention files · / commands · ⇧⇥ switch mode · Esc stop · ⌘K ⌘A mention the selection"))
                .into_any_element()
        } else {
            list(self.list.clone(), cx.processor(|v: &mut AgentView, ix: usize, w, cx| v.render_item(ix, w, cx))).flex_1().into_any_element()
        };
        let menu = self.menu.map(|(m, at)| {
            let corner = if matches!(m, Menu::Mode | Menu::Model | Menu::Effort | Menu::Style) { Anchor::BottomLeft } else { Anchor::TopRight };
            deferred(anchored().anchor(corner).position(at).snap_to_window().child(self.render_menu(m, cx))).with_priority(2)
        });
        let info = self.info.clone().map(|(t, rows)| self.render_info(&t, &rows, cx));
        div()
            .id("agent-view")
            .key_context("AgentView")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .text_color(theme::fg())
            .text_size(theme::ui_text())
            .on_action(cx.listener(|v, _: &Interrupt, _, cx| v.interrupt(cx)))
            .on_action(cx.listener(Self::cycle_mode))
            .on_drop(cx.listener(|v, f: &ui::DraggedFile, w, cx| v.add_paths(vec![f.path.clone()], w, cx)))
            .on_drop(cx.listener(|v, p: &ExternalPaths, w, cx| v.add_paths(p.paths().to_vec(), w, cx)))
            .child(self.render_header(cx))
            .child(log)
            .child(self.render_bottom(cx))
            .children(menu)
            .children(info)
    }
}
