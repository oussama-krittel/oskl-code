//! The agent tab: a chat with an ACP agent (see `acp`), Claude Code by default. Streams replies,
//! shows tool calls and edits as cards, asks before tools run (with the agent's own options), and
//! offers @-mentions, /commands, the open file and selection as context, pictures, the agent's
//! own pickers (mode, model…), its plan, and past conversations.

use crate::acp::{self, Conn, Event, Item, Log};
use crate::complete::{Item as Choice, Kind};
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::preview::Markdown;
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use agent_client_protocol::v1::schema::*;
use agent_client_protocol::{Responder, V1};
use futures::{FutureExt, StreamExt};
use gpui::prelude::*;
use gpui::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::result::Result;
use std::time::Duration;

actions!(agent_view, [Interrupt, CycleMode]);

pub fn bind_keys() {
    crate::keybindings::collect([
        crate::keybindings::kb("escape", Interrupt, Some("AgentView")),
        crate::keybindings::kb("shift-tab", CycleMode, Some("AgentView")),
        crate::keybindings::kb("shift-tab", CycleMode, Some("AgentView > Editor")),
    ]);
}

/// Commands the tab handles itself (the rest go to the agent).
const LOCAL: [(&str, &str); 5] = [
    ("clear", "Start a new conversation"),
    ("resume", "Open a past conversation"),
    ("agent", "Switch to another agent"),
    ("export", "Save the conversation as Markdown"),
    ("terminal", "Continue this conversation in a terminal (Claude Code)"),
];

#[derive(Clone, Copy, PartialEq)]
enum Menu {
    /// One of the agent's pickers (mode, model…): its index in `log.config`.
    Option(usize),
    /// An older agent's modes (it sends no pickers).
    Mode,
    Agent,
    History,
    More,
    /// The tool calls and subagents going on.
    Running,
}

struct Outgoing {
    text: String,
    images: Vec<acp::Picture>,
    context: Vec<String>,
}

/// An idle agent (Claude's ~250 MB) is stopped after this long; the next message resumes the session.
const IDLE_STOP: Duration = Duration::from_secs(120);

/// Ways to start a conversation, offered while it's empty: (what it says, what goes in the box).
const STARTERS: [(&str, &str); 3] = [
    ("Explain how this project is organized", "How is this project organized? Walk me through its main parts and how they fit together."),
    ("Find and fix a failing test", "Run the tests, find what fails and fix it."),
    ("Review my uncommitted changes", "Review my uncommitted changes: bugs, risks and what could be simpler."),
];

/// The conversation's column (wider tabs keep it centered, as easy to read as a page).
const COLUMN: f32 = 740.;

/// The biggest picture sent (the API's limit).
const MAX_IMAGE: usize = 5 * 1024 * 1024;

thread_local! {
    /// Each project's files for @-mentions (shared by its tabs) and when they were listed.
    static FILES: RefCell<HashMap<PathBuf, (Rc<RefCell<Vec<String>>>, Option<std::time::Instant>)>> = RefCell::new(HashMap::new());
    /// The agent picked last: new chats start with it.
    static LAST_AGENT: Cell<&'static str> = const { Cell::new("claude") };
}

pub struct AgentView {
    pub root: PathBuf,
    input: Entity<Editor>,
    search: Entity<Editor>,
    log: Log,
    list: ListState,
    agent: &'static acp::AgentSpec,
    conn: Option<Rc<Conn>>,
    /// What the agent sends, read by `_reader` (and drained before a turn's end, to keep the order).
    events: Option<Rc<RefCell<futures::stream::Peekable<futures::channel::mpsc::UnboundedReceiver<Event>>>>>,
    session: Option<SessionId>,
    /// The session this tab left (cleared): what it still sends is dropped.
    left: Option<SessionId>,
    /// The session is open on the running agent (new, loaded or resumed).
    ready: bool,
    /// An old session reopened to continue it: its history replays, but it's on screen already.
    replaying: bool,
    /// Permission cards waiting, by item index: their answer goes here.
    asks: HashMap<usize, Responder<RequestPermissionResponse, V1>>,
    /// Picker choices made while the agent was stopped: sent once the session is open.
    pending: Vec<(SessionConfigId, SessionConfigValueId)>,
    commands: Rc<RefCell<Vec<(String, String)>>>,
    queue: Vec<Outgoing>,
    images: Vec<acp::Picture>,
    /// The editor the user was in last (the workspace keeps it): its file and selection go along.
    editor: Rc<RefCell<Option<WeakEntity<Editor>>>>,
    share_editor: bool,
    /// Cards opened (or closed, for those open by default).
    toggled: HashSet<usize>,
    menu: Option<(Menu, Point<Pixels>)>,
    /// The menu a click outside just closed (so a click on its own button doesn't reopen it).
    just_closed: Option<Menu>,
    /// Edit diffs by (item, edit), worked out once.
    diffs: RefCell<HashMap<(usize, usize), Rc<Vec<(similar::ChangeTag, String)>>>>,
    sessions: Vec<SessionInfo>,
    /// The past conversations are being asked for; or why they couldn't be.
    history_loading: bool,
    history_error: Option<String>,
    /// Why the agent couldn't start (shown where its answers would be).
    start_error: Option<String>,
    /// The tab's width (logical px) at the last frame: a narrow tab drops the header's extras.
    width: Rc<Cell<f32>>,
    /// The list follows new messages (it does unless scrolled up): else "↓ Latest" shows.
    following: Rc<Cell<bool>>,
    md: HashMap<usize, (usize, Rc<Markdown>)>,
    md_images: Entity<RetainAllImageCache>,
    tasks_open: bool,
    /// The status row as last drawn (it's measured again when that changes).
    shown_status: Option<(String, bool)>,
    _starting: Option<Task<()>>,
    _opening: Option<Task<()>>,
    _reader: Option<Task<()>>,
    _turn: Option<Task<()>>,
    _stop: Option<Task<()>>,
    _idle: Option<Task<()>>,
    _history: Option<Task<()>>,
    /// Picker changes and deletions on their way (they end with the agent).
    _calls: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for AgentView {}

impl Focusable for AgentView {
    /// The box: the tab has no focus of its own, so a click on a button leaves you typing.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_target(cx)
    }
}

/// A picker's choices, groups flattened.
fn choices(o: &SessionConfigOption) -> Vec<&SessionConfigSelectOption> {
    match &o.kind {
        SessionConfigKind::Select(s) => match &s.options {
            SessionConfigSelectOptions::Ungrouped(v) => v.iter().collect(),
            SessionConfigSelectOptions::Grouped(g) => g.iter().flat_map(|g| &g.options).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn current(o: &SessionConfigOption) -> Option<&SessionConfigValueId> {
    match &o.kind {
        SessionConfigKind::Select(s) => Some(&s.current_value),
        _ => None,
    }
}

impl AgentView {
    /// `agent`: an id from `acp::AGENTS` (None: the one picked last); `session`: a past conversation to open.
    pub fn new(root: PathBuf, agent: Option<&str>, session: Option<String>, editor: Rc<RefCell<Option<WeakEntity<Editor>>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let agent = acp::agent(agent.unwrap_or_else(|| LAST_AGENT.with(Cell::get)));
        let files = FILES.with(|f| f.borrow_mut().entry(root.clone()).or_insert_with(|| (Rc::default(), None)).0.clone());
        let commands = Rc::new(RefCell::new(LOCAL.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect::<Vec<_>>()));
        let input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::MultiLine, cx);
            ed.chat = true;
            ed.font_size = 13.;
            ed.placeholder = "Ask anything. @ mentions a file, / lists commands.".into();
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
                    if let Some(s) = v.shown_sessions(cx).first().cloned() {
                        v.load(s.session_id, cx);
                    }
                }
                EditorEvent::Cancel => {
                    v.menu = None;
                    window.focus(&v.input.read(cx).focus_target(cx), cx);
                    cx.notify();
                }
                _ => {}
            }),
            // Using the box: the project's files for @, and the agent started so it's ready.
            cx.on_focus(&input.focus_handle(cx), window, |v, _, cx| {
                v.list_files(cx);
                v.connect(cx);
                cx.notify(); // the box's border shows it
            }),
            cx.on_blur(&input.focus_handle(cx), window, |_, _, cx| cx.notify()),
        ];
        let mut v = Self {
            root,
            input,
            search,
            log: Log::default(),
            list: {
                // The conversation's items, then the status row ("Thinking…").
                let list = ListState::new(1, ListAlignment::Bottom, gpui::px(800.));
                list.set_follow_mode(FollowMode::Tail);
                list
            },
            agent,
            conn: None,
            events: None,
            session: None,
            left: None,
            ready: false,
            replaying: false,
            asks: HashMap::new(),
            pending: Vec::new(),
            commands,
            queue: Vec::new(),
            images: Vec::new(),
            editor,
            share_editor: true,
            toggled: HashSet::new(),
            menu: None,
            just_closed: None,
            diffs: RefCell::default(),
            sessions: Vec::new(),
            history_loading: false,
            history_error: None,
            start_error: None,
            width: Rc::new(Cell::new(f32::MAX)),
            following: Rc::new(Cell::new(true)),
            md: HashMap::new(),
            md_images: RetainAllImageCache::new(cx),
            tasks_open: true,
            shown_status: None,
            _starting: None,
            _opening: None,
            _reader: None,
            _turn: None,
            _stop: None,
            _idle: None,
            _history: None,
            _calls: Vec::new(),
            _subs: subs,
        };
        // Scrolled up or back down: "↓ Latest" comes and goes (drawn again only then).
        let (following, weak) = (v.following.clone(), cx.entity().downgrade());
        v.list.set_scroll_handler(move |e, _, cx| {
            if following.replace(e.is_following_tail) != e.is_following_tail {
                weak.update(cx, |_, cx| cx.notify()).ok();
            }
        });
        if let Some(id) = session {
            v.load(SessionId::new(id), cx);
        }
        v
    }

    /// What the agent is doing, said at the end of the conversation; true: it waits for you.
    fn status(&self) -> Option<(String, bool)> {
        if self._starting.is_some() || (self._opening.is_some() && !self.ready && self.log.items.is_empty() && self.session.is_none()) {
            Some((format!("Starting {}…", self.agent.name), false))
        } else if self._opening.is_some() {
            Some(("Opening the conversation…".to_string(), false))
        } else if self._stop.is_some() {
            Some(("Stopping…".to_string(), false))
        } else if !self.asks.is_empty() {
            Some((format!("{} is waiting for your answer above", self.agent.name), true))
        } else {
            self.log.busy.then(|| ("Thinking…".to_string(), false))
        }
    }

    fn status_row(&self) -> Option<Div> {
        let (s, waiting) = self.status()?;
        let busy = self.log.busy;
        Some(
            div()
                .debug_selector(|| "agent-status".into())
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(20.))
                .text_size(px(12.))
                .text_color(if waiting { theme::git_modified() } else { theme::fg_muted() })
                .child(div().size(px(6.)).rounded_full().bg(if waiting { theme::git_modified() } else { theme::accent() }))
                .child(s)
                .when(busy && !waiting, |d| d.child(div().px(px(5.)).rounded(px(4.)).border_1().border_color(theme::border_input()).font_family(theme::MONO_FONT).text_size(px(10.5)).child("esc")).child("stops")),
        )
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

    pub fn focus_target(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_target(cx)
    }

    pub fn session(&self) -> Option<String> {
        self.session.as_ref().map(|s| s.0.to_string())
    }

    pub fn agent_id(&self) -> &'static str {
        self.agent.id
    }

    /// The tab's name: the conversation's title, short.
    pub fn title(&self) -> String {
        let t = self.full_title();
        if t.chars().count() > 32 { t.chars().take(31).collect::<String>() + "…" } else { t }
    }

    fn full_title(&self) -> String {
        let first = self.log.items.iter().find_map(|it| if let Item::User { text, .. } = it { text.lines().next() } else { None });
        self.log.title.as_deref().or(first).unwrap_or(self.agent.name).to_string()
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

    // ------------------------------------------------------------ the agent

    /// Starts the agent if it isn't running; once it is, opens the session if one is wanted.
    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.conn.is_some() || self._starting.is_some() {
            return self.want_session(cx);
        }
        let (agent, root) = (self.agent, self.root.clone());
        self.start_error = None;
        self._starting = Some(cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { acp::start(agent, &root) }).await;
            this.update(cx, |v, cx| {
                v._starting = None;
                match r {
                    Ok((conn, rx)) => {
                        v.conn = Some(Rc::new(conn));
                        v.read_events(rx, cx);
                        if v.menu.is_some_and(|(m, _)| m == Menu::History) {
                            v.refresh_history(cx);
                        }
                        v.want_session(cx);
                        v.stop_when_idle(cx); // started just in case (the box focused): not kept for nothing
                    }
                    Err(e) => {
                        v.start_error = Some(e.clone());
                        v.history_loading = false;
                        v.fail(e);
                        v.unqueue_all(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Opens the session when there's something for it: a message to send, a past conversation
    /// to show, a picker choice to make.
    fn want_session(&mut self, cx: &mut Context<Self>) {
        let wanted = !self.queue.is_empty() || !self.pending.is_empty() || (self.session.is_some() && self.log.items.is_empty());
        if wanted && self.conn.is_some() && !self.ready && self._opening.is_none() {
            self.open_session(cx);
        }
    }

    /// session/new, or the session this tab shows: loaded (its history replays) or resumed.
    fn open_session(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else { return };
        let caps = &conn.init.agent_capabilities;
        let (load, resume) = (caps.load_session, caps.session_capabilities.resume.is_some());
        let shown = !self.log.items.is_empty();
        if self.session.is_some() && !load && !resume {
            self.session = None; // this agent can't go back to it: a new one
        }
        let root = self.root.clone();
        let id = self.session.clone();
        self.replaying = shown && !resume && id.is_some();
        self._opening = Some(cx.spawn(async move |this, cx| {
            type Opened = (SessionId, Option<Vec<SessionConfigOption>>, Option<SessionModeState>);
            let r: Result<Opened, String> = match id {
                None => conn.ask(NewSessionRequest::new(root)).await.map(|r| (r.session_id, r.config_options, r.modes)),
                Some(id) if resume && (shown || !load) => conn.ask(ResumeSessionRequest::new(id.clone(), root)).await.map(|r| (id, r.config_options, r.modes)),
                Some(id) => conn.ask(LoadSessionRequest::new(id.clone(), root)).await.map(|r| (id, r.config_options, r.modes)),
            };
            this.update(cx, |v, cx| {
                v.take_events(cx); // the replay, before it's over
                v._opening = None;
                v.replaying = false;
                match r {
                    Ok((id, config, modes)) => {
                        v.session = Some(id);
                        v.ready = true;
                        v.log.config = config.unwrap_or_default();
                        v.log.modes = modes;
                        for (id, value) in std::mem::take(&mut v.pending) {
                            v.set_option(id, value, cx);
                        }
                        v.drain(cx);
                    }
                    Err(e) => {
                        v.fail(e);
                        v.unqueue_all(cx);
                    }
                }
                v.stop_when_idle(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn read_events(&mut self, rx: futures::channel::mpsc::UnboundedReceiver<Event>, cx: &mut Context<Self>) {
        let rx = Rc::new(RefCell::new(rx.peekable()));
        self.events = Some(rx.clone());
        self._reader = Some(cx.spawn(async move |this, cx| {
            loop {
                // Once something arrives, all that has is applied at once; then a pause, so a fast
                // stream redraws ~30 times a second instead of once per token.
                let more = futures::future::poll_fn(|w| std::pin::Pin::new(&mut *rx.borrow_mut()).poll_peek(w).map(|e| e.is_some())).await;
                if !more || this.update(cx, |v, cx| v.take_events(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(33)).await;
            }
        }));
    }

    /// Applies every event that has arrived. A turn's end and a session opening come by another
    /// path (their request's answer): they take what came before them first.
    fn take_events(&mut self, cx: &mut Context<Self>) {
        let Some(rx) = self.events.clone() else { return };
        let batch: Vec<Event> = std::iter::from_fn(|| rx.borrow_mut().next().now_or_never().flatten()).collect();
        for ev in batch {
            self.on_event(ev, cx);
        }
        cx.notify();
    }

    /// Whether an update or request is about this tab's conversation: its session, or the one
    /// being opened (not one it left).
    fn ours(&self, id: &SessionId) -> bool {
        match &self.session {
            Some(s) => s == id,
            None => self._opening.is_some() && self.left.as_ref() != Some(id),
        }
    }

    fn on_event(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::Update(n) => {
                if !self.ours(&n.session_id) {
                    return;
                }
                use SessionUpdate::*;
                // A replay of what's on screen: only the session's state is taken from it.
                if self.replaying && !matches!(n.update, AvailableCommandsUpdate(_) | ConfigOptionUpdate(_) | CurrentModeUpdate(_) | SessionInfoUpdate(_) | UsageUpdate(_)) {
                    return;
                }
                let commands = matches!(n.update, AvailableCommandsUpdate(_));
                self.apply(n.update);
                if commands {
                    let mut all: Vec<(String, String)> = LOCAL.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect();
                    all.extend(self.log.commands.iter().filter(|c| !LOCAL.iter().any(|(n, _)| *n == c.name)).map(|c| (c.name.clone(), c.description.clone())));
                    *self.commands.borrow_mut() = all;
                }
            }
            Event::Permission(r, answer) => {
                if !self.ours(&r.session_id) {
                    let _ = answer.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
                    return;
                }
                let ix = self.log.ask(&r);
                self.list.splice(ix..ix, 1);
                self.asks.insert(ix, answer);
                self.redraw_tool_of_ask(ix);
                self._idle = None;
            }
            Event::Read(r, answer) => {
                let _ = match self.read_file(&r, cx) {
                    Ok(text) => answer.respond(ReadTextFileResponse::new(text)),
                    Err(e) => answer.respond_with_error(agent_client_protocol::Error::internal_error().data(e)),
                };
            }
            Event::Write(r, answer) => {
                // Open editors follow the file on disk (when they have no unsaved changes).
                let written = if r.path.is_absolute() {
                    r.path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&r.path, &r.content)).map_err(|e| e.to_string())
                } else {
                    Err("The path must be absolute".to_string())
                };
                let _ = match written {
                    Ok(()) => answer.respond(WriteTextFileResponse::new()),
                    Err(e) => answer.respond_with_error(agent_client_protocol::Error::internal_error().data(format!("{}: {e}", r.path.display()))),
                };
            }
            Event::Exit(why) => {
                let busy = self.log.busy || self._opening.is_some();
                self.stop_conn();
                if busy {
                    self.fail(format!("{} stopped.\n{why}", self.agent.name));
                }
            }
        }
    }

    /// A file the agent reads: the editor's text when it's the file open there (unsaved changes
    /// included), else the disk's. `line` (1-based) and `limit` pick lines.
    fn read_file(&self, r: &ReadTextFileRequest, cx: &App) -> Result<String, String> {
        if !r.path.is_absolute() {
            return Err(format!("{}: the path must be absolute", r.path.display()));
        }
        let open = self.editor.borrow().as_ref().and_then(|w| w.upgrade()).filter(|e| e.read(cx).path.as_deref() == Some(r.path.as_path()));
        let text = match open {
            Some(e) => e.read(cx).text(),
            None => std::fs::read_to_string(&r.path).map_err(|e| format!("{}: {e}", r.path.display()))?,
        };
        if r.line.is_none() && r.limit.is_none() {
            return Ok(text);
        }
        let skip = r.line.map_or(0, |l| l.saturating_sub(1) as usize);
        Ok(text.lines().skip(skip).take(r.limit.map_or(usize::MAX, |l| l as usize)).collect::<Vec<_>>().join("\n"))
    }

    /// Applies an update and redraws the items it changed.
    fn apply(&mut self, u: SessionUpdate) {
        let old = self.log.items.len();
        let first = self.log.apply(u).min(old);
        self.list.splice(first..old, self.log.items.len() - first);
    }

    fn push(&mut self, item: Item) {
        let old = self.log.items.len();
        self.log.items.push(item);
        self.list.splice(old..old, 1);
    }

    /// Something went wrong: said in the chat (a turn going on goes on; what was queued is kept).
    fn fail(&mut self, e: String) {
        self.push(Item::Error(e));
    }

    /// Stops this tab's agent (on purpose: what it says while ending isn't shown).
    fn stop_conn(&mut self) {
        self.cancel_asks();
        self.conn = None;
        self.events = None;
        self.ready = false;
        self.replaying = false;
        self.log.busy = false;
        (self._starting, self._opening, self._reader, self._turn, self._stop, self._idle, self._history) = (None, None, None, None, None, None, None);
        self._calls.clear();
    }

    /// Permission cards still open are answered "cancelled" (their turn ended or was stopped).
    fn cancel_asks(&mut self) {
        for (ix, answer) in std::mem::take(&mut self.asks) {
            let _ = answer.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
            self.log.answer(ix, "Cancelled".into());
            self.redraw_tool_of_ask(ix);
            self.list.splice(ix..ix + 1, 1);
        }
    }

    /// Nothing going on: no turn, question, message waiting or session opening.
    fn idle(&self) -> bool {
        !self.log.busy && self.asks.is_empty() && self.queue.is_empty() && self._opening.is_none()
    }

    /// Stops the agent once it has been idle for a while (checked then; a new turn clears the timer).
    fn stop_when_idle(&mut self, cx: &mut Context<Self>) {
        self._idle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(IDLE_STOP).await;
            this.update(cx, |v, cx| {
                if v.conn.is_some() && v.idle() {
                    v.stop_conn();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    // ------------------------------------------------------------ sending

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() && self.images.is_empty() {
            return;
        }
        // A permission waiting: a message instead means "no, do this".
        if let Some(ix) = self.pending_ask()
            && let Some(Item::Ask(q)) = self.log.items.get(ix)
            && let Some(no) = q.options.iter().find(|o| o.kind == PermissionOptionKind::RejectOnce).cloned()
        {
            self.answer(ix, no, cx);
        }
        if let Some(cmd) = text.strip_prefix('/')
            && self.local_command(cmd, window, cx)
        {
            self.input.update(cx, |e, cx| e.set_text("", cx));
            return;
        }
        let m = Outgoing { text, images: std::mem::take(&mut self.images), context: self.context_blocks(cx) };
        self.input.update(cx, |e, cx| e.set_text("", cx));
        self.say(m, cx);
    }

    /// Sends a message now, or after the turn when the agent is working (or starting).
    fn say(&mut self, m: Outgoing, cx: &mut Context<Self>) {
        if self.log.busy || !self.ready {
            self.queue.push(m);
            self.connect(cx);
        } else {
            self.deliver(m, cx);
        }
        cx.notify();
    }

    fn deliver(&mut self, m: Outgoing, cx: &mut Context<Self>) {
        let (Some(conn), Some(session)) = (self.conn.clone(), self.session.clone()) else { return };
        self._idle = None;
        let shown = m.context.iter().filter(|c| !c.starts_with("<ide_opened_file>")).cloned().collect();
        self.push(Item::User { text: m.text.clone(), images: m.images.clone(), context: shown });
        self.list.set_follow_mode(FollowMode::Tail); // follow the bottom again
        self.log.busy = true;
        let prompt = acp::prompt(&m.text, &m.context, &m.images);
        self._turn = Some(cx.spawn(async move |this, cx| {
            let r = conn.ask(PromptRequest::new(session, prompt)).await;
            this.update(cx, |v, cx| v.turn_ended(r.map(|r| r.stop_reason), cx)).ok();
        }));
    }

    fn turn_ended(&mut self, r: Result<StopReason, String>, cx: &mut Context<Self>) {
        self.take_events(cx); // the turn's last words, before it's over
        self._stop = None;
        self.cancel_asks();
        let old = self.log.items.len();
        self.log.finish(r);
        self.list.splice(old..old, self.log.items.len() - old);
        self.drain(cx);
        self.stop_when_idle(cx);
        cx.notify();
    }

    /// Your message `ix` back in the box (text and pictures), to change and send.
    /// shortcut: the agent keeps what came after it (ACP's fork and rewind aren't stable yet); use session/fork once they are.
    fn edit_message(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Item::User { text, images, .. }) = self.log.items.get(ix) else { return };
        let (text, images) = (text.clone(), images.clone());
        self.input.update(cx, |e, cx| e.set_text(&text, cx));
        self.images = images;
        window.focus(&self.input.read(cx).focus_target(cx), cx);
        cx.notify();
    }

    /// Sends your message `ix` again, with its pictures and selection (queued while the agent works).
    fn retry(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Item::User { text, images, context }) = self.log.items.get(ix) else { return };
        let m = Outgoing { text: text.clone(), images: images.clone(), context: context.clone() };
        self.say(m, cx);
    }

    /// Messages that can't go (the agent didn't start) back in the box, to send again.
    fn unqueue_all(&mut self, cx: &mut Context<Self>) {
        if self.queue.is_empty() {
            return;
        }
        let typed = self.input.read(cx).text();
        let mut texts: Vec<String> = self.queue.iter().map(|m| m.text.clone()).collect();
        if !typed.trim().is_empty() {
            texts.push(typed);
        }
        for m in std::mem::take(&mut self.queue) {
            self.images.extend(m.images);
        }
        self.input.update(cx, |e, cx| e.set_text(&texts.join("\n\n"), cx));
    }

    /// Sends what was queued once a turn has ended.
    fn drain(&mut self, cx: &mut Context<Self>) {
        if !self.log.busy && self.ready && !self.queue.is_empty() {
            let m = self.queue.remove(0);
            self.deliver(m, cx);
        }
    }

    /// What the user has open or selected in the editor, the way Claude Code's IDE extension says it.
    fn context_blocks(&self, cx: &App) -> Vec<String> {
        let Some(ed) = self.shared_editor() else { return Vec::new() };
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

    fn shared_editor(&self) -> Option<Entity<Editor>> {
        self.share_editor.then(|| self.editor.borrow().as_ref().and_then(|w| w.upgrade())).flatten()
    }

    fn add_image(&mut self, img: &Image, cx: &mut Context<Self>) {
        if acp::media_type(img.format).is_none() {
            return cx.emit(Request::Error("Pictures can be PNG, JPEG, GIF or WebP".into()));
        }
        if img.bytes.len() > MAX_IMAGE {
            return cx.emit(Request::Error("Pictures can be at most 5 MB".into()));
        }
        self.images.push(std::sync::Arc::new(img.clone()));
        cx.notify();
    }

    /// Files added (attach button, drop): pictures are attached, the rest @-mentioned.
    fn add_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let mut mentions = String::new();
        for p in paths {
            let ext = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            let small = std::fs::metadata(&p).is_ok_and(|m| m.len() as usize <= MAX_IMAGE);
            match small.then(|| std::fs::read(&p).ok()).flatten().and_then(|bytes| acp::picture(&ext, bytes)) {
                Some(pic) => self.images.push(pic),
                None => mentions.push_str(&format!("@{} ", p.strip_prefix(&self.root).unwrap_or(&p).display())),
            }
        }
        if !mentions.is_empty() {
            self.insert(&mentions, window, cx);
        }
        cx.notify();
    }

    fn pending_ask(&self) -> Option<usize> {
        self.asks.keys().max().copied()
    }

    /// Answers a permission card with one of the agent's options.
    fn answer(&mut self, ix: usize, option: PermissionOption, cx: &mut Context<Self>) {
        let Some(answer) = self.asks.remove(&ix) else { return };
        let _ = answer.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option.option_id))));
        self.log.answer(ix, option.name);
        self.redraw_tool_of_ask(ix);
        self.list.splice(ix..ix + 1, 1);
        cx.notify();
    }

    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            return cx.notify();
        }
        self.interrupt(cx);
    }

    /// Esc: cancels the turn; an agent that doesn't stop within 3 s is ended (the next message resumes).
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if !self.log.busy {
            return;
        }
        if let (Some(conn), Some(session)) = (&self.conn, &self.session)
            && let Err(e) = conn.tell(CancelNotification::new(session.clone()))
        {
            self.fail(e);
        }
        self.cancel_asks();
        self._stop = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            this.update(cx, |v, cx| {
                if v.log.busy {
                    v.stop_conn();
                    v.turn_ended(Ok(StopReason::Cancelled), cx);
                    if !v.queue.is_empty() {
                        v.connect(cx);
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    // ------------------------------------------------------------ pickers

    /// The picker for the permission mode, if the agent has one.
    fn mode_option(&self) -> Option<&SessionConfigOption> {
        self.log.option(SessionConfigOptionCategory::Mode)
    }

    fn mode_id(&self) -> Option<String> {
        match self.mode_option() {
            Some(o) => current(o).map(|v| v.0.to_string()),
            None => self.log.modes.as_ref().map(|m| m.current_mode_id.0.to_string()),
        }
    }

    /// ⇧⇥: the next mode, leaving out those that skip asking (they need a confirmation).
    fn cycle_mode(&mut self, _: &CycleMode, _: &mut Window, cx: &mut Context<Self>) {
        let now = self.mode_id().unwrap_or_default();
        let ids: Vec<String> = match self.mode_option() {
            Some(o) => choices(o).into_iter().map(|c| c.value.0.to_string()).collect(),
            None => self.log.modes.iter().flat_map(|m| &m.available_modes).map(|m| m.id.0.to_string()).collect(),
        };
        let ids: Vec<String> = ids.into_iter().filter(|i| !is_bypass(i)).collect();
        if let Some(next) = ids.iter().position(|i| *i == now).map_or(ids.first(), |k| ids.get((k + 1) % ids.len())).cloned() {
            self.pick_mode(next, cx);
        }
    }

    fn pick_mode(&mut self, value: String, cx: &mut Context<Self>) {
        match self.mode_option().map(|o| o.id.clone()) {
            Some(id) => self.pick_option(id, SessionConfigValueId::new(value), cx),
            None => self.set_mode(value, cx),
        }
    }

    /// A picker choice; one that runs everything without asking only after the user confirms.
    fn pick_option(&mut self, id: SessionConfigId, value: SessionConfigValueId, cx: &mut Context<Self>) {
        if !is_bypass(&value.0) {
            return self.set_option(id, value, cx);
        }
        let weak = cx.entity().downgrade();
        let name = self.agent.name;
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: "Bypass permissions?".into(),
            detail: format!("{name} will edit files and run any command without asking, until you switch modes. Use it only in a sandbox you trust."),
            buttons: vec!["Bypass permissions".into()],
            danger: true,
            cancel_default: true,
            cancel_label: None,
            output: None,
            on_answer: Rc::new(move |_, _, cx| {
                weak.update(cx, |v, cx| v.set_option(id.clone(), value.clone(), cx)).ok();
            }),
        }));
    }

    fn set_option(&mut self, id: SessionConfigId, value: SessionConfigValueId, cx: &mut Context<Self>) {
        let (Some(conn), Some(session), true) = (self.conn.clone(), self.session.clone(), self.ready) else {
            // The agent is stopped: shown now, sent once the session is open again.
            if let Some(SessionConfigKind::Select(s)) = self.log.config.iter_mut().find(|o| o.id == id).map(|o| &mut o.kind) {
                s.current_value = value.clone();
            }
            self.pending.push((id, value));
            return cx.notify();
        };
        let call = cx.spawn(async move |this, cx| {
            let r = conn.ask(SetSessionConfigOptionRequest::new(session, id, SessionConfigOptionValue::value_id(value))).await;
            this.update(cx, |v, cx| {
                match r {
                    Ok(r) => v.log.config = r.config_options,
                    Err(e) => v.fail(e),
                }
                cx.notify();
            })
            .ok();
        });
        self._calls.push(call);
    }

    /// An older agent's mode switch (session/set_mode).
    fn set_mode(&mut self, mode: String, cx: &mut Context<Self>) {
        let (Some(conn), Some(session)) = (self.conn.clone(), self.session.clone()) else { return };
        let call = cx.spawn(async move |this, cx| {
            let r = conn.ask(SetSessionModeRequest::new(session, mode.clone())).await;
            this.update(cx, |v, cx| {
                match r {
                    Ok(_) => {
                        if let Some(m) = &mut v.log.modes {
                            m.current_mode_id = SessionModeId::new(mode);
                        }
                    }
                    Err(e) => v.fail(e),
                }
                cx.notify();
            })
            .ok();
        });
        self._calls.push(call);
    }

    // ------------------------------------------------------------ conversations

    /// Starts over in this tab (the agent keeps running).
    fn clear(&mut self, cx: &mut Context<Self>) {
        if self.log.busy
            && let (Some(conn), Some(session)) = (&self.conn, &self.session)
        {
            let _ = conn.tell(CancelNotification::new(session.clone()));
        }
        self.cancel_asks();
        self.log = Log::default();
        if let Some(s) = self.session.take() {
            self.left = Some(s);
        }
        self.ready = false;
        self.queue.clear();
        self.pending.clear();
        (self._opening, self._turn, self._stop) = (None, None, None);
        self.reset_view();
        if self.conn.is_some() {
            self.stop_when_idle(cx);
        }
        cx.notify();
    }

    fn reset_view(&mut self) {
        self.md.clear();
        self.diffs.borrow_mut().clear();
        self.toggled.clear();
        self.list.reset(self.log.items.len() + 1);
    }

    /// Opens a past conversation in this tab: the agent replays it.
    fn load(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.menu = None;
        self.clear(cx);
        self.session = Some(id);
        self.connect(cx);
    }

    /// Another agent in this tab: a new conversation with it.
    fn switch_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        self.menu = None;
        let agent = acp::agent(id);
        LAST_AGENT.with(|l| l.set(agent.id));
        if agent.id != self.agent.id {
            self.stop_conn();
            self.clear(cx);
            self.agent = agent;
            self.sessions.clear();
        }
        cx.notify();
    }

    /// The past conversations the search box matches.
    fn shown_sessions(&self, cx: &App) -> Vec<SessionInfo> {
        let q = self.search.read(cx).text().to_lowercase();
        self.sessions.iter().filter(|s| s.title.as_deref().unwrap_or("").to_lowercase().contains(&q)).cloned().collect()
    }

    /// The agent's past conversations in this project (it must be running to say).
    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else { return self.connect(cx) };
        if conn.init.agent_capabilities.session_capabilities.list.is_none() {
            return;
        }
        let root = self.root.clone();
        (self.history_loading, self.history_error) = (true, None);
        self._history = Some(cx.spawn(async move |this, cx| {
            // shortcut: the first page only (no next_cursor); page on when someone has more than the agent sends at once.
            let r = conn.ask(ListSessionsRequest::new().cwd(root)).await;
            this.update(cx, |v, cx| {
                v.history_loading = false;
                match r {
                    Ok(r) => {
                        v.sessions = r.sessions;
                        v.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                    }
                    Err(e) => v.history_error = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn delete_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else { return };
        let call = cx.spawn(async move |this, cx| {
            let r = conn.ask(DeleteSessionRequest::new(id)).await;
            this.update(cx, |v, cx| match r {
                Ok(_) => v.refresh_history(cx),
                Err(e) => cx.emit(Request::Error(e)),
            })
            .ok();
        });
        self._calls.push(call);
    }

    fn open_menu(&mut self, menu: Menu, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // The click that closed this menu (outside it) was on its own button: it stays closed.
        if self.just_closed.take() == Some(menu) || self.menu.is_some_and(|(m, _)| m == menu) {
            self.menu = None;
            return cx.notify();
        }
        self.menu = Some((menu, at));
        if menu == Menu::History {
            self.search.update(cx, |e, cx| e.set_text("", cx));
            window.focus(&self.search.read(cx).focus_target(cx), cx);
            self.refresh_history(cx);
        }
        cx.notify();
    }

    /// /commands the tab runs itself; false: send it to the agent.
    fn local_command(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (name, arg) = cmd.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
        let at = point(window.bounds().size.width / 2., window.bounds().size.height / 2.);
        match name {
            "clear" | "new" => self.clear(cx),
            "resume" => self.open_menu(Menu::History, at, window, cx),
            "agent" if arg.is_empty() => self.open_menu(Menu::Agent, at, window, cx),
            "agent" => self.switch_agent(arg, cx),
            "export" => self.export(cx),
            "terminal" => self.open_in_terminal(cx),
            _ => return false,
        }
        true
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let title = self.title();
        let text = self.log.markdown(&title, self.agent.name);
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

    /// Claude Code only: `claude --resume` in a terminal tab.
    fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        if self.agent.id != "claude" {
            return cx.emit(Request::Error(format!("{} can't continue in a terminal from here", self.agent.name)));
        }
        // Only a well-formed session id goes into the command line (it runs in a shell).
        let cmd = match self.session() {
            Some(id) if crate::agent::is_uuid(&id) => format!("claude --resume {id}"),
            _ => "claude".into(),
        };
        self.stop_conn(); // one agent per conversation
        cx.emit(Request::RunInTerminal { dir: self.root.clone(), command: cmd });
    }
}

fn is_bypass(mode: &str) -> bool {
    mode.to_ascii_lowercase().contains("bypass")
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
        .h(px(26.))
        .px(px(10.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(12.))
        .when(primary, |d| d.bg(theme::accent()).font_weight(FontWeight::SEMIBOLD).text_color(theme::on(theme::accent())).hover(|s| s.bg(theme::accent_hover())))
        .when(!primary, |d| d.border_1().border_color(theme::border_input()).bg(theme::bg_input()).text_color(theme::fg()).hover(|s| s.bg(theme::hover())))
        .child(label.into())
}

/// A quiet button with a chevron: the header's pickers (model, effort…) and the agent.
fn ghost(id: impl Into<ElementId>, label: String) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(24.))
        .px(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(theme::fg_dim())
        .hover(|s| s.bg(theme::hover()).text_color(theme::fg_bright()))
        .child(label)
        .child(ui::small_icon("chevron-down", theme::fg_muted()).size(px(10.)))
}

/// A rounded chip in the box's footer: the mode, the open file.
fn chip(id: impl Into<ElementId>, round: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(22.))
        .px(px(8.))
        .rounded(px(if round { 11. } else { 5. }))
        .border_1()
        .border_color(theme::border_input())
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(theme::fg_dim())
        .whitespace_nowrap()
        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
}

/// A card in the conversation: a tool call, the plan, a question.
fn card() -> Div {
    div().flex().flex_col().rounded(px(8.)).border_1().border_color(theme::border()).bg(theme::bg_input()).overflow_hidden()
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
        .px(px(10.))
        .py(px(8.))
        .font_family(theme::MONO_FONT)
        .text_size(px(11.5))
        .line_height(relative(1.4))
        .text_color(theme::fg_dim())
        .whitespace_normal()
        .child(shown)
}

/// A picture in the chat, `h` tall in its own proportions.
fn thumb(pic: &acp::Picture, h: f32) -> Div {
    div().flex_none().rounded(px(4.)).overflow_hidden().border_1().border_color(theme::border()).child(img(pic.clone()).h(px(h)).max_w(px(h * 3.)).object_fit(ObjectFit::Contain))
}

/// A tool call's icon and the word its card says (Read, Edit, Run…).
fn tool_kind(kind: ToolKind) -> (&'static str, &'static str) {
    match kind {
        ToolKind::Read => ("file", "Read"),
        ToolKind::Edit => ("pencil", "Edit"),
        ToolKind::Delete => ("trash", "Delete"),
        ToolKind::Move => ("arrow-right", "Move"),
        ToolKind::Search => ("search", "Search"),
        ToolKind::Execute => ("terminal", "Run"),
        ToolKind::Think => ("sparkle", "Think"),
        ToolKind::Fetch => ("globe", "Fetch"),
        ToolKind::SwitchMode => ("refresh", "Mode"),
        _ => ("braces", "Tool"),
    }
}

/// An edit's changed lines with 3 lines around each change ("⋯" between the parts), worked out once per edit.
/// A subagent: Claude Code's Agent (formerly Task) tool, by its name or the adapter's `_meta`.
fn is_subagent(t: &ToolCall) -> bool {
    let name = t.name.as_deref().or_else(|| t.meta.as_ref()?.get("claudeCode")?.get("toolName")?.as_str());
    matches!(name, Some("Agent" | "Task"))
}

/// A card's icon and word: a subagent's own, else its kind's.
fn tool_look(t: &ToolCall) -> (&'static str, &'static str) {
    if is_subagent(t) { ("list-tree", "Subagent") } else { tool_kind(t.kind) }
}

fn diff_lines(old: &str, new: &str) -> Vec<(similar::ChangeTag, String)> {
    let d = similar::TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for (g, ops) in d.grouped_ops(3).iter().enumerate() {
        if g > 0 {
            out.push((similar::ChangeTag::Equal, "⋯".to_string()));
        }
        out.extend(ops.iter().flat_map(|op| d.iter_changes(op)).map(|ch| (ch.tag(), ch.value().trim_end_matches('\n').to_string())));
    }
    out
}

/// Diff rows drawn at most (the rest is counted).
const DIFF_ROWS: usize = 400;

/// The lines an edit changes, removed red and added green.
fn diff_box(lines: &[(similar::ChangeTag, String)]) -> Div {
    let mut rows = Vec::new();
    for (tag, text) in lines.iter().take(DIFF_ROWS) {
        let (sign, bg) = match tag {
            similar::ChangeTag::Delete => ("-", theme::diff_del_line()),
            similar::ChangeTag::Insert => ("+", theme::diff_ins_line()),
            similar::ChangeTag::Equal => (" ", gpui::transparent_black()),
        };
        rows.push(div().bg(bg).px(px(10.)).whitespace_normal().child(format!("{sign} {text}")));
    }
    if lines.len() > DIFF_ROWS {
        rows.push(div().px(px(6.)).text_color(theme::fg_muted()).child(format!("… {} more lines", lines.len() - DIFF_ROWS)));
    }
    div().font_family(theme::MONO_FONT).text_size(px(11.5)).line_height(relative(1.5)).flex().flex_col().children(rows)
}

/// Tool output text without the Markdown fence agents wrap it in.
fn unfence(t: &str) -> String {
    let t = t.trim();
    match t.strip_prefix("```").and_then(|r| r.strip_suffix("```")) {
        Some(inner) => inner.split_once('\n').map_or("", |(_, body)| body).trim_end().to_string(),
        None => t.to_string(),
    }
}

/// What a tool card shows when open: its diffs and output. `diff(k, old, new)` gives diff k's lines.
fn tool_body(t: &ToolCall, diff: &dyn Fn(usize, &str, &str) -> Rc<Vec<(similar::ChangeTag, String)>>) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = Vec::new();
    for (k, c) in t.content.iter().enumerate() {
        match c {
            ToolCallContent::Diff(d) => out.push(diff_box(&diff(k, d.old_text.as_deref().unwrap_or(""), &d.new_text)).into_any_element()),
            ToolCallContent::Content(c) => {
                if let ContentBlock::Text(text) = &c.content {
                    let text = unfence(&text.text);
                    if !text.is_empty() {
                        out.push(mono_box(text).when(t.status == ToolCallStatus::Failed, |d| d.text_color(theme::git_deleted())).into_any_element());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

impl AgentView {
    fn render_item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if ix == self.log.items.len() {
            return div().w_full().px(px(16.)).children(self.status_row().map(|r| div().max_w(px(COLUMN)).mx_auto().py(px(6.)).child(r))).into_any_element();
        }
        // Markdown is parsed when a message's text changes, not on every frame.
        if let Some(Item::Text(text)) = self.log.items.get(ix)
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
            Item::User { text, images, context } => {
                let chips = context.iter().filter_map(|c| between_tags(c)).map(|c| {
                    div().flex().items_center().gap(px(4.)).h(px(20.)).px(px(6.)).rounded(px(5.)).border_1().border_color(theme::border_input()).font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_dim()).child(ui::small_icon("file", theme::fg_muted()).size(px(11.))).child(c)
                });
                let text2 = text.clone();
                // Your messages: a bubble on the right; the agent's answers take the column's width.
                div()
                    .group("user-msg")
                    .debug_selector(move || format!("user-msg-{ix}"))
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(2.))
                    .child(
                        div()
                            .max_w(relative(0.86))
                            .px(px(12.))
                            .py(px(8.))
                            .rounded_tl(px(10.))
                            .rounded_tr(px(10.))
                            .rounded_bl(px(10.))
                            .rounded_br(px(2.))
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::bg_input())
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .when(!text.is_empty(), |d| d.child(div().text_size(px(13.)).line_height(relative(1.5)).text_color(theme::fg_bright()).whitespace_normal().child(text.clone())))
                            .when(!images.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(6.)).children(images.iter().map(|p| thumb(p, 120.)))))
                            .when(!context.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(4.)).children(chips))),
                    )
                    .child(
                        div()
                            .flex()
                            .invisible()
                            .group_hover("user-msg", |s| s.visible())
                            .child(ui::icon_button(("edit-u", ix), "pencil").debug_selector(move || format!("user-edit-{ix}")).tooltip(ui::tooltip("Edit: back in the box")).on_click(cx.listener(move |v, _, w, cx| v.edit_message(ix, w, cx))))
                            .child(ui::icon_button(("retry-u", ix), "refresh").debug_selector(move || format!("user-retry-{ix}")).tooltip(ui::tooltip("Send again")).on_click(cx.listener(move |v, _, _, cx| v.retry(ix, cx))))
                            .child(ui::icon_button(("copy-u", ix), "copy").tooltip(ui::tooltip("Copy")).on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text2.clone())))),
                    )
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
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(theme::fg_muted())
                            .hover(|s| s.text_color(theme::fg_dim()))
                            .on_click(cx.listener(move |v, _, _, cx| v.toggle(ix, cx)))
                            .child(ui::small_icon("sparkle", theme::fg_muted()).size(px(12.)))
                            .child("Thought")
                            .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_muted()).size(px(11.))),
                    )
                    .when(open, |d| d.child(div().pl(px(12.)).border_l_2().border_color(theme::border()).text_size(px(12.)).text_color(theme::fg_dim()).whitespace_normal().child(text.clone())))
                    .into_any_element()
            }
            Item::Tool(t) => self.render_tool(ix, t, cx),
            // Asked inside its tool's card (when it has one): nothing of its own.
            Item::Ask(q) if self.tool_of(&q.tool_call_id).is_some() => return div().into_any_element(),
            Item::Ask(q) => self.render_ask(ix, q, cx),
            Item::Error(e) => div()
                .group("error-msg")
                .relative()
                .child({
                    let e = e.clone();
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .invisible()
                        .group_hover("error-msg", |s| s.visible())
                        .child(ui::icon_button(("copy-e", ix), "copy").tooltip(ui::tooltip("Copy")).on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(e.clone()))))
                })
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
        div().id(("item", ix)).w_full().px(px(16.)).py(px(6.)).child(div().max_w(px(COLUMN)).mx_auto().child(body)).into_any_element()
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

    /// Opens a file the agent mentioned: in the editor, or ⌘-click in its default app.
    fn opener(&self, cx: &mut Context<Self>) -> crate::preview::Opener {
        let weak = cx.entity().downgrade();
        Rc::new(move |path, line, window, cx| open_file(&weak, path, line, window, cx))
    }

    /// The tool calls (and subagents) going on: their items.
    fn running(&self) -> Vec<usize> {
        if !self.log.busy {
            return Vec::new();
        }
        self.log.items.iter().enumerate().filter(|(_, it)| matches!(it, Item::Tool(t) if matches!(t.status, ToolCallStatus::Pending | ToolCallStatus::InProgress))).map(|(i, _)| i).collect()
    }

    /// Scrolls to item `ix`, at the top (the list stops following the end).
    fn reveal(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: gpui::px(0.) });
        self.following.set(false);
        cx.notify();
    }

    /// The card of the tool call `id`, if the conversation shows one.
    fn tool_of(&self, id: &ToolCallId) -> Option<usize> {
        self.log.items.iter().rposition(|it| matches!(it, Item::Tool(t) if t.tool_call_id == *id))
    }

    /// The permission waiting on the tool call `id`: its item.
    fn ask_on(&self, id: &ToolCallId) -> Option<usize> {
        self.asks.keys().copied().find(|&k| matches!(self.log.items.get(k), Some(Item::Ask(q)) if q.tool_call_id == *id))
    }

    /// Draws a permission's tool card again (it shows the question, then its outcome).
    fn redraw_tool_of_ask(&self, ask: usize) {
        if let Some(Item::Ask(q)) = self.log.items.get(ask)
            && let Some(t) = self.tool_of(&q.tool_call_id)
        {
            self.list.splice(t..t + 1, 1);
        }
    }

    /// A tool call as a card: what it is and on what, how it went, its diff or output when open,
    /// and, while the agent waits for it, the permission's choices.
    fn render_tool(&self, ix: usize, t: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let ask = self.ask_on(&t.tool_call_id);
        let edit = t.kind == ToolKind::Edit;
        let open = self.toggled.contains(&ix) != (edit || ask.is_some()); // edits and questions show their diff unless closed
        let (icon, word) = tool_look(t);
        let place = t.locations.first().map(|l| (l.path.clone(), l.line.map(|n| n as usize)));
        let root = format!("{}/", self.root.display());
        let title = t.title.replace(&root, "");
        // "Read src/a.rs" under the word Read: the rest is enough.
        let what = title.strip_prefix(word).filter(|r| r.starts_with(' ')).map_or(title.clone(), |r| r.trim().to_string());
        let output: Vec<String> = t.content.iter().filter_map(|c| if let ToolCallContent::Content(c) = c && let ContentBlock::Text(x) = &c.content { Some(unfence(&x.text)) } else { None }).filter(|x| !x.is_empty()).collect();
        let (added, removed) = t.content.iter().filter_map(|c| if let ToolCallContent::Diff(d) = c { Some(d) } else { None }).fold((0, 0), |(a, r), d| {
            let lines = diff_lines(d.old_text.as_deref().unwrap_or(""), &d.new_text);
            (a + lines.iter().filter(|l| l.0 == similar::ChangeTag::Insert).count(), r + lines.iter().filter(|l| l.0 == similar::ChangeTag::Delete).count())
        });
        let state = match (ask, t.status) {
            (Some(_), _) => div().text_size(px(11.5)).text_color(theme::git_modified()).child("Waiting for you").into_any_element(),
            (_, ToolCallStatus::Completed) => ui::small_icon("check", theme::git_added()).into_any_element(),
            (_, ToolCallStatus::Failed) => ui::small_icon("close", theme::git_deleted()).into_any_element(),
            _ if self.log.busy => div().size(px(7.)).rounded_full().bg(theme::accent()).into_any_element(),
            _ => div().into_any_element(),
        };
        let header = div()
            .id(("tool", ix))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(ix, cx)))
            .child(ui::small_icon(icon, theme::fg_dim()))
            .child(div().flex_none().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme::fg_bright()).child(word))
            .child(
                div()
                    .id(("tool-title", ix))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(11.5))
                    .text_color(theme::fg_dim())
                    .map(|d| match place {
                        Some((path, line)) => {
                            let weak = cx.entity().downgrade();
                            d.hover(|s| s.underline().text_color(theme::accent())).tooltip(ui::tooltip_text(format!("Open {} (⌘-click: in its default app)", path.display()))).on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                open_file(&weak, path.clone(), line, window, cx)
                            })
                        }
                        // Cut short in the row (a long command): all of it on hover.
                        None => d.tooltip(ui::tooltip_text(title.clone())),
                    })
                    .child(what),
            )
            .when(added + removed > 0, |d| {
                d.child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::git_added()).child(format!("+{added}")))
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::git_deleted()).child(format!("−{removed}")))
            })
            .when(open && !output.is_empty(), |d| {
                let text = output.join("\n");
                d.child(ui::icon_button(("copy-tool", ix), "copy").tooltip(ui::tooltip("Copy the output")).on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                }))
            })
            .child(state)
            .when(!t.content.is_empty(), |d| d.child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_muted()).size(px(12.))));
        let body = tool_body(t, &self.differ(ix));
        let footer = ask.and_then(|a| if let Some(Item::Ask(q)) = self.log.items.get(a) { Some((a, q)) } else { None }).map(|(a, q)| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .py(px(8.))
                .border_t_1()
                .border_color(theme::border())
                .child(div().flex_1().min_w(px(140.)).text_size(px(12.)).text_color(theme::fg_dim()).child("Allow this? Or type below what to do instead."))
                .children(q.options.iter().enumerate().map(|(k, o)| {
                    let o2 = o.clone();
                    btn(("perm", a * 16 + k), o.name.clone(), o.kind == PermissionOptionKind::AllowOnce).on_click(cx.listener(move |v, _, _, cx| v.answer(a, o2.clone(), cx)))
                }))
        });
        card()
            .when(ask.is_some(), |d| d.border_color(theme::git_modified().opacity(0.55)))
            .child(header)
            // A long diff or output scrolls in the card: the choices below stay in reach.
            .when(open && !body.is_empty(), |d| d.child(div().id(("tool-body", ix)).max_h(px(260.)).overflow_y_scroll().border_t_1().border_color(theme::border()).bg(theme::bg_editor()).children(body)))
            .children(footer)
            .into_any_element()
    }

    /// A permission for a call the conversation has no card for: a card of its own.
    fn render_ask(&self, ix: usize, q: &acp::Ask, cx: &mut Context<Self>) -> AnyElement {
        let asks = if q.title.trim().is_empty() { format!("Allow {} to continue?", self.agent.name) } else { format!("{} wants to: {}", self.agent.name, q.title) };
        let head = div().flex().items_center().gap(px(8.)).px(px(10.)).py(px(8.)).child(ui::small_icon("warning", theme::git_modified())).child(div().flex_1().min_w_0().text_size(px(12.5)).text_color(theme::fg_bright()).whitespace_normal().child(asks));
        if let Some(a) = &q.answer {
            return card().child(head.child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(a.clone()))).into_any_element();
        }
        card()
            .border_color(theme::git_modified().opacity(0.55))
            .child(head)
            .child(div().flex().flex_wrap().gap(px(6.)).px(px(10.)).py(px(8.)).border_t_1().border_color(theme::border()).children(q.options.iter().enumerate().map(|(k, o)| {
                let o2 = o.clone();
                btn(("perm", ix * 16 + k), o.name.clone(), o.kind == PermissionOptionKind::AllowOnce).on_click(cx.listener(move |v, _, _, cx| v.answer(ix, o2.clone(), cx)))
            })))
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // As the tab narrows, the extras leave (plan limits, then context and cost, then the agent's name).
        let w = self.width.get();
        let usage = self.log.usage.as_ref();
        let cost = usage.and_then(|u| u.cost.as_ref()).filter(|c| c.amount > 0.).map(|c| if c.currency == "USD" { format!("${:.2}", c.amount) } else { format!("{:.2} {}", c.amount, c.currency) });
        let ctx = usage.filter(|u| u.size > 0).map(|u| (u.used as f64 / u.size as f64 * 100.).round() as u32);
        let limits = &self.log.limits;
        // The agent's pickers but the mode (that one sits in the box): model, effort…
        // shortcut: on/off pickers aren't shown; add a toggle when an agent sends one.
        let pickers: Vec<_> = self
            .log
            .config
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o.kind, SessionConfigKind::Select(_)) && matches!(o.category, Some(SessionConfigOptionCategory::Model | SessionConfigOptionCategory::ThoughtLevel)))
            .map(|(i, o)| {
                let now = current(o).and_then(|v| choices(o).into_iter().find(|c| c.value == *v)).map_or(o.name.clone(), |c| c.name.clone());
                ghost(("agent-opt", i), now).tooltip(ui::tooltip_text(o.description.clone().unwrap_or_else(|| o.name.clone()))).on_mouse_down(MouseButton::Left, cx.listener(move |v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Option(i), e.position, w, cx)))
            })
            .collect();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(36.))
            .px(px(10.))
            .border_b_1()
            .border_color(theme::border())
            // Nothing here breaks onto a second line; in a narrow split the title shortens first.
            .overflow_hidden()
            .whitespace_nowrap()
            .child(ui::small_icon("sparkle", theme::accent()))
            .child(div().id("agent-title").flex_1().min_w(px(48.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::fg_bright()).tooltip(ui::tooltip_text(self.full_title())).child(self.full_title()))
            .children(ctx.filter(|_| w >= 460.).map(|p| {
                div()
                    .id("agent-context")
                    .flex_none()
                    .px(px(4.))
                    .text_size(px(11.))
                    .text_color(if p >= 80 { theme::git_modified() } else { theme::fg_muted() })
                    .tooltip(ui::tooltip("Context used (/compact frees it)"))
                    .child(format!("{p}% context"))
            }))
            .when(!limits.is_empty() && w >= 620., |d| {
                d.child(
                    div()
                        .id("agent-limits")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .px(px(5.))
                        .h(px(22.))
                        .tooltip(ui::tooltip_text(limits.iter().map(|l| format!("{}: {:.0}% used{}", l.name, l.used * 100., resets(l.resets_at))).collect::<Vec<_>>().join("\n")))
                        .children(limits.iter().take(2).map(|l| {
                            let p = (l.used * 100.).clamp(0., 100.) as f32;
                            let color = if p >= 80. { theme::git_deleted() } else { theme::accent() };
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(px(11.))
                                .text_color(theme::fg_muted())
                                .child(l.name.clone())
                                .child(div().w(px(28.)).h(px(4.)).rounded(px(2.)).bg(theme::border()).child(div().h_full().rounded(px(2.)).w(relative(p / 100.)).bg(color)))
                                .child(format!("{p:.0}%"))
                        })),
                )
            })
            .children(cost.filter(|_| w >= 460.).map(|c| div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).child(c)))
            .children(Some(self.running().len()).filter(|n| *n > 0).map(|n| {
                chip("agent-running", true)
                    .debug_selector(|| "agent-running".into())
                    .tooltip(ui::tooltip("Going on now"))
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Running, e.position, w, cx)))
                    .child(div().size(px(6.)).rounded_full().bg(theme::accent()))
                    .child(if w >= 400. { format!("{n} running") } else { n.to_string() })
            }))
            .when(w >= 400., |d| d.children(pickers))
            .when(w >= 360., |d| d.child(ghost("agent-which", self.agent.name.to_string()).tooltip(ui::tooltip("The agent (/agent)")).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::Agent, e.position, w, cx)))))
            .child(ui::icon_button("agent-history", "history").tooltip(ui::tooltip("Past conversations")).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::History, e.position, w, cx))))
            .child(ui::icon_button("agent-new", "plus").tooltip(ui::tooltip("New conversation")).on_click(cx.listener(|v, _, _, cx| v.clear(cx))))
            .child(ui::icon_button("agent-more", "ellipsis").tooltip(ui::tooltip("More")).on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, w, cx| v.open_menu(Menu::More, e.position, w, cx))))
    }

    fn render_bottom(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let plan = &self.log.plan;
        let done = plan.iter().filter(|e| e.status == PlanEntryStatus::Completed).count();
        let plan_card = (!plan.is_empty()).then(|| {
            let rows = plan.iter().map(|e| {
                let mark = match e.status {
                    PlanEntryStatus::Completed => ui::small_icon("check", theme::git_added()).size(px(13.)).into_any_element(),
                    PlanEntryStatus::InProgress => div().size(px(13.)).flex().items_center().justify_center().child(div().size(px(7.)).rounded_full().bg(theme::accent())).into_any_element(),
                    _ => div().size(px(13.)).flex().items_center().justify_center().child(div().size(px(9.)).rounded_full().border_1().border_color(theme::fg_muted())).into_any_element(),
                };
                let text = div().flex_1().min_w_0().whitespace_normal().child(e.content.clone());
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .child(div().pt(px(2.)).child(mark))
                    .child(match e.status {
                        PlanEntryStatus::Completed => text.text_color(theme::fg_muted()).line_through(),
                        PlanEntryStatus::InProgress => text.text_color(theme::fg_bright()),
                        _ => text.text_color(theme::fg_dim()),
                    })
            });
            card()
                .child(
                    div()
                        .id("agent-tasks")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.tasks_open = !v.tasks_open;
                            cx.notify();
                        }))
                        .child(ui::small_icon("list-flat", theme::fg_dim()))
                        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme::fg_bright()).child("Plan"))
                        .child(div().flex_1().text_size(px(12.)).text_color(theme::fg_muted()).child(format!("{done} of {} done", plan.len())))
                        .child(ui::small_icon(if self.tasks_open { "chevron-down" } else { "chevron-right" }, theme::fg_muted()).size(px(12.))),
                )
                .when(self.tasks_open, |d| d.child(div().id("agent-plan").max_h(px(140.)).overflow_y_scroll().flex().flex_col().gap(px(4.)).px(px(10.)).pt(px(6.)).pb(px(8.)).border_t_1().border_color(theme::border()).children(rows)))
        });
        let queued = self.queue.iter().enumerate().map(|(i, m)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(10.))
                .h(px(28.))
                .rounded(px(6.))
                .border_1()
                .border_color(theme::border())
                .text_size(px(12.))
                .text_color(theme::fg_dim())
                .child(ui::small_icon("clock", theme::fg_muted()))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(format!("Queued: {}", m.text.lines().next().unwrap_or(""))))
                .child(ui::icon_button(("unqueue", i), "close").tooltip(ui::tooltip("Don't send (back in the box when it's empty)")).on_click(cx.listener(move |v, _, _, cx| {
                    if i < v.queue.len() {
                        let m = v.queue.remove(i);
                        if v.input.read(cx).text().trim().is_empty() {
                            v.input.update(cx, |e, cx| e.set_text(&m.text, cx));
                            v.images.extend(m.images);
                        }
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
        let toggle_share = cx.listener(|v, _, _, cx| {
            v.share_editor = !v.share_editor;
            cx.notify();
        });
        // The open file (and selection) going along, or a way to send it again.
        let context = editor_chip.map(|label| match self.share_editor {
            true => chip("agent-ctx", false)
                .font_family(theme::MONO_FONT)
                .text_size(px(11.))
                .tooltip(ui::tooltip("The open file and selection go with each message. Click to stop that."))
                .on_click(toggle_share)
                .child(ui::small_icon("file", theme::fg_muted()).size(px(12.)))
                .child(label)
                .child(ui::small_icon("close", theme::fg_muted()).size(px(10.))),
            false => chip("agent-ctx", false).tooltip(ui::tooltip("Send the open file and selection with each message")).on_click(toggle_share).child(ui::small_icon("plus", theme::fg_muted()).size(px(10.))).child("Add the open file"),
        });
        // The mode, in the box as the prototype has it (⇧⇥ switches): plan mode stands out.
        let mode = self.mode_id().map(|id| {
            let name = match self.mode_option() {
                Some(o) => choices(o).into_iter().find(|c| *c.value.0 == *id).map_or(id.clone(), |c| c.name.clone()),
                None => self.log.modes.iter().flat_map(|m| &m.available_modes).find(|m| *m.id.0 == *id).map_or(id.clone(), |m| m.name.clone()),
            };
            let low = id.to_ascii_lowercase();
            let (icon, color) = match () {
                _ if low.contains("plan") => ("list-flat", theme::gutter_modified()),
                _ if is_bypass(&low) => ("warning", theme::git_deleted()),
                _ if low.contains("edit") => ("pencil", theme::fg_dim()),
                _ if low.contains("auto") => ("sparkle", theme::fg_dim()),
                _ => ("eye", theme::fg_dim()),
            };
            let menu = match self.log.config.iter().position(|o| o.category == Some(SessionConfigOptionCategory::Mode)) {
                Some(i) => Menu::Option(i),
                None => Menu::Mode,
            };
            chip("agent-mode", true)
                .text_color(color)
                .when(color != theme::fg_dim(), |d| d.border_color(color.opacity(0.45)))
                .tooltip(ui::tooltip("Switch mode (⇧⇥)"))
                .on_mouse_down(MouseButton::Left, cx.listener(move |v, e: &MouseDownEvent, w, cx| v.open_menu(menu, e.position, w, cx)))
                .child(ui::small_icon(icon, color).size(px(12.)))
                .child(name)
        });
        let busy = self.log.busy;
        let send = div()
            .id("agent-send")
            .debug_selector(|| "agent-send".into())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(28.))
            .h(px(26.))
            .rounded(px(6.))
            .cursor_pointer()
            .map(|d| match busy {
                true => d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())).tooltip(ui::tooltip("Stop (Esc)")).on_click(cx.listener(|v, _, _, cx| v.interrupt(cx))).child(ui::small_icon("stop-filled", theme::fg()).size(px(12.))),
                false => d.bg(theme::accent()).hover(|s| s.bg(theme::accent_hover())).tooltip(ui::tooltip("Send (↵)")).on_click(cx.listener(|v, _, w, cx| v.submit(w, cx))).child(ui::small_icon("arrow-up", theme::on(theme::accent()))),
            });
        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .pl(px(8.))
            .pr(px(6.))
            .pt(px(4.))
            .pb(px(6.))
            .children(context)
            .children(mode)
            .child(div().flex_1())
            .child(ui::icon_button("agent-attach", "paperclip").tooltip(ui::tooltip("Attach a picture or file (or paste, or drop it here)")).on_click(cx.listener(|_, _, window, cx| {
                let pick = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(paths))) = pick.await else { return };
                    this.update_in(cx, |v, w, cx| v.add_paths(paths, w, cx)).ok();
                })
                .detach();
            })))
            .child(send);
        let pictures = (!self.images.is_empty()).then(|| {
            div().flex().flex_wrap().gap(px(6.)).px(px(10.)).pt(px(10.)).children(self.images.iter().enumerate().map(|(i, pic)| {
                div()
                    .group("pending-pic")
                    .relative()
                    .child(thumb(pic, 56.))
                    .child(
                        div()
                            .absolute()
                            .top(px(2.))
                            .right(px(2.))
                            .rounded(px(3.))
                            .bg(theme::bg_popup())
                            .invisible()
                            .group_hover("pending-pic", |s| s.visible())
                            .child(ui::icon_button(("unimage", i), "close").size(px(18.)).tooltip(ui::tooltip("Remove")).on_click(cx.listener(move |v, _, _, cx| {
                                if i < v.images.len() {
                                    v.images.remove(i);
                                }
                                cx.notify();
                            }))),
                    )
            }))
        });
        // Said at the end of the conversation; with nothing in it yet (no list), above the box.
        let status = self.log.items.is_empty().then(|| self.status_row()).flatten();
        let input = self.input.clone();
        div().flex_none().px(px(16.)).pt(px(6.)).pb(px(12.)).child(
            div()
                .w_full()
                .max_w(px(COLUMN + 8.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(plan_card)
                .children(queued)
                .children(status)
                .child(
                    div()
                        .id("agent-input")
                        .flex()
                        .flex_col()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(if self.input.read(cx).focus_target(cx).is_focused(window) { theme::accent() } else { theme::border_input() })
                        .bg(theme::bg_input())
                        // A click anywhere in the box (not just on its text) types into it.
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&input.read(cx).focus_target(cx), cx))
                        .children(pictures)
                        .child(div().px(px(12.)).pt(px(10.)).pb(px(4.)).min_h(px(44.)).child(self.input.clone()))
                        .child(footer),
                ),
        )
    }

    /// What an empty chat says instead of the welcome: a conversation on its way, or why the agent isn't there.
    fn empty_note(&self) -> Option<String> {
        if let Some(e) = &self.start_error {
            return Some(e.clone());
        }
        self.session.is_some().then(|| "Loading the conversation…".to_string())
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
            Menu::Option(i) => {
                let Some(o) = self.log.config.get(i) else { return base.into_any_element() };
                let now = current(o).cloned();
                let rows = choices(o).into_iter().enumerate().map(|(k, c)| {
                    let (id, value) = (o.id.clone(), c.value.clone());
                    menu_row(("opt", k), c.name.clone(), c.description.clone().unwrap_or_default(), now.as_ref() == Some(&c.value)).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        v.pick_option(id.clone(), value.clone(), cx);
                    }))
                });
                base.children(rows).into_any_element()
            }
            Menu::Mode => {
                let Some(m) = &self.log.modes else { return base.into_any_element() };
                base.children(m.available_modes.iter().enumerate().map(|(k, x)| {
                    let id = x.id.0.to_string();
                    menu_row(("mode", k), x.name.clone(), x.description.clone().unwrap_or_default(), x.id == m.current_mode_id).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = None;
                        v.set_mode(id.clone(), cx);
                    }))
                }))
                .into_any_element()
            }
            Menu::Agent => base
                .child(div().px(px(10.)).pb(px(4.)).text_size(px(11.)).text_color(theme::fg_muted()).child("Switching starts a new conversation"))
                .children(acp::AGENTS.iter().enumerate().map(|(k, a)| {
                    menu_row(("which", k), a.name, a.cmd.join(" "), a.id == self.agent.id).on_click(cx.listener(move |v, _, _, cx| v.switch_agent(a.id, cx)))
                }))
                .into_any_element(),
            Menu::More => {
                let compact = self.log.commands.iter().any(|c| c.name == "compact");
                let mut items: Vec<(&str, &str, u8)> = Vec::new();
                if compact {
                    items.push(("Compact conversation", "Summarize to free context (/compact)", 0));
                }
                items.push(("Export as Markdown…", "Save this conversation", 1));
                items.push(("Switch agent…", "Claude Code, Codex, Gemini… (/agent)", 3));
                if self.agent.id == "claude" {
                    items.push(("Continue in terminal", "claude --resume in a terminal tab", 2));
                }
                // The agent's other pickers, by name (the header has room for model and thinking only).
                let others = self.log.config.iter().enumerate().filter(|(_, o)| matches!(o.kind, SessionConfigKind::Select(_)) && !matches!(o.category, Some(SessionConfigOptionCategory::Mode | SessionConfigOptionCategory::Model | SessionConfigOptionCategory::ThoughtLevel))).map(|(i, o)| {
                    let now = current(o).and_then(|v| choices(o).into_iter().find(|c| c.value == *v)).map_or(String::new(), |c| c.name.clone());
                    menu_row(("more-opt", i), format!("{}: {now}", o.name), o.description.clone().unwrap_or_default(), false).on_click(cx.listener(move |v, _, _, cx| {
                        v.menu = v.menu.take().map(|(_, at)| (Menu::Option(i), at));
                        cx.notify();
                    }))
                });
                base.children(others).children(items.into_iter().map(|(label, detail, what)| {
                    menu_row(("more", what as usize), label, detail, false).on_click(cx.listener(move |v, _, _, cx| {
                        let at = v.menu.take().map(|(_, at)| at);
                        match what {
                            0 => v.say(Outgoing { text: "/compact".into(), images: Vec::new(), context: Vec::new() }, cx),
                            1 => v.export(cx),
                            3 => v.menu = at.map(|at| (Menu::Agent, at)),
                            _ => v.open_in_terminal(cx),
                        }
                        cx.notify();
                    }))
                }))
                .into_any_element()
            }
            Menu::Running => {
                let running = self.running();
                let root = format!("{}/", self.root.display());
                let note = running.is_empty().then(|| div().px(px(10.)).py(px(4.)).text_size(px(12.)).text_color(theme::fg_muted()).child("Nothing going on now"));
                base.children(note)
                    .children(running.into_iter().filter_map(|ix| if let Some(Item::Tool(t)) = self.log.items.get(ix) { Some((ix, t)) } else { None }).map(|(ix, t)| {
                        let (icon, word) = tool_look(t);
                        div()
                            .id(("running", ix))
                            .debug_selector(move || format!("running-row-{ix}"))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(10.))
                            .py(px(5.))
                            .mx(px(4.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::selected_focused()))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.menu = None;
                                v.reveal(ix, cx);
                            }))
                            .child(ui::small_icon(icon, theme::fg_dim()))
                            .child(div().flex_none().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(word))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_muted()).child(t.title.replace(&root, "")))
                    }))
                    .into_any_element()
            }
            Menu::History => {
                let can_list = self.conn.as_ref().map(|c| c.init.agent_capabilities.session_capabilities.list.is_some());
                let can_delete = self.conn.as_ref().is_some_and(|c| c.init.agent_capabilities.session_capabilities.delete.is_some());
                let shown = self.shown_sessions(cx);
                let rows = shown.iter().take(200).enumerate().map(|(i, s)| {
                    let id = s.session_id.clone();
                    let id2 = id.clone();
                    let title = s.title.clone().unwrap_or_else(|| "Untitled".into());
                    let here = self.session.as_ref() == Some(&s.session_id);
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
                        .when(here, |d| d.bg(theme::selected()))
                        .on_click(cx.listener(move |v, _, w, cx| {
                            v.load(id.clone(), cx);
                            w.focus(&v.input.read(cx).focus_target(cx), cx);
                        }))
                        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(12.)).child(title.clone()))
                        .children(s.updated_at.as_deref().and_then(acp::unix_time).map(|t| div().text_size(px(11.)).text_color(theme::fg_muted()).child(crate::util::relative_time(t))))
                        .when(can_delete && !here, |d| {
                            d.child(div().invisible().group_hover("session-row", |s| s.visible()).child(ui::icon_button(("del-session", i), "trash").tooltip(ui::tooltip("Delete")).on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                let (weak, id) = (cx.entity().downgrade(), id2.clone());
                                cx.emit(Request::Ask(crate::dialog::Ask {
                                    title: format!("Delete \"{title}\"?"),
                                    detail: "The agent deletes the conversation for good: this can't be undone.".into(),
                                    buttons: vec!["Delete".into()],
                                    danger: true,
                                    cancel_default: true,
                                    cancel_label: None,
                                    output: None,
                                    on_answer: Rc::new(move |_, _, cx| {
                                        weak.update(cx, |v, cx| v.delete_session(id.clone(), cx)).ok();
                                    }),
                                }));
                            }))))
                        })
                });
                let note = match (can_list, &self.start_error, &self.history_error) {
                    (None, Some(e), _) => Some(e.clone()),
                    (None, None, _) => Some(format!("Starting {}…", self.agent.name)),
                    (Some(false), ..) => Some(format!("{} doesn't list past conversations", self.agent.name)),
                    (Some(true), _, Some(e)) => Some(format!("Couldn't list them: {e}")),
                    (Some(true), ..) if self.history_loading && shown.is_empty() => Some("Loading…".into()),
                    (Some(true), ..) if shown.is_empty() => Some(if self.sessions.is_empty() { "No conversations yet".into() } else { "None match".into() }),
                    _ => None,
                };
                base.w(px(420.))
                    .child(div().px(px(8.)).pb(px(4.)).child(div().px(px(6.)).rounded(px(4.)).border_1().border_color(theme::border_input()).bg(theme::bg_input()).child(self.search.clone())))
                    .children(note.map(|n| div().px(px(10.)).py(px(4.)).text_size(px(12.)).text_color(theme::fg_muted()).child(n)))
                    .children(rows)
                    .into_any_element()
            }
        }
    }
}

/// " · resets in 4h 30m" for a limit's reset time.
fn resets(at: Option<i64>) -> String {
    let Some(s) = at.map(|t| t - crate::util::now()).filter(|s| *s > 0) else { return String::new() };
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    let left = if d > 0 { format!("{d}d {h}h") } else if h > 0 { format!("{h}h {m}m") } else { format!("{m}m") };
    format!(" · resets in {left}")
}

/// Opens a file in the editor at `line`, or with ⌘ held in the app macOS opens it with.
fn open_file(weak: &WeakEntity<AgentView>, path: PathBuf, line: Option<usize>, window: &mut Window, cx: &mut App) {
    if window.modifiers().platform {
        // A path from the agent's reply could point anywhere: only the project's files open in their
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.log.items.is_empty();
        let status = self.status();
        if status != self.shown_status {
            let n = self.log.items.len();
            self.list.splice(n..n + 1, 1);
            self.shown_status = status;
        }
        let log: AnyElement = if empty && let Some(note) = self.empty_note() {
            div().flex_1().flex().items_center().justify_center().px(px(24.)).text_center().text_size(px(12.)).text_color(theme::fg_muted()).whitespace_normal().child(note).into_any_element()
        } else if empty {
            let starters = STARTERS.iter().enumerate().map(|(i, (label, prompt))| {
                div()
                    .id(("starter", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::bg_input())
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .text_color(theme::fg())
                    .hover(|s| s.bg(theme::hover()).border_color(theme::border_input()))
                    .on_click(cx.listener(move |v, _, w, cx| {
                        v.input.update(cx, |e, cx| e.set_text(prompt, cx));
                        w.focus(&v.input.read(cx).focus_target(cx), cx);
                    }))
                    .child(div().flex_1().text_left().child(*label))
                    .child(ui::small_icon("arrow-right", theme::fg_muted()).size(px(12.)))
            });
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .px(px(24.))
                .text_center()
                .text_color(theme::fg_muted())
                .child(ui::icon("sparkle", theme::accent()).size(px(28.)))
                .child(div().text_size(px(16.)).font_weight(FontWeight::MEDIUM).text_color(theme::fg_bright()).child(self.agent.name))
                .child(div().max_w(px(420.)).text_size(px(12.5)).whitespace_normal().child("Ask about this project or have it make changes. It asks before it edits a file or runs a command."))
                .child(div().w_full().max_w(px(420.)).pt(px(6.)).flex().flex_col().gap(px(6.)).children(starters))
                .child(div().pt(px(4.)).text_size(px(11.5)).child("@ mention files · / commands · ⇧⇥ switch mode · Esc stop"))
                .into_any_element()
        } else {
            list(self.list.clone(), cx.processor(|v: &mut AgentView, ix: usize, w, cx| v.render_item(ix, w, cx))).flex_1().into_any_element()
        };
        let menu = self.menu.map(|(m, at)| {
            let up = match m {
                Menu::Mode => true,
                Menu::Option(i) => self.log.config.get(i).is_some_and(|o| o.category == Some(SessionConfigOptionCategory::Mode)),
                _ => false,
            };
            let corner = if up { Anchor::BottomLeft } else { Anchor::TopRight };
            deferred(anchored().anchor(corner).position(at).snap_to_window().child(self.render_menu(m, cx))).with_priority(2)
        });
        div()
            .id("agent-view")
            .key_context("AgentView")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_editor())
            .text_color(theme::fg())
            .text_size(theme::ui_text())
            .on_action(cx.listener(|v, _: &Interrupt, _, cx| v.escape(cx)))
            .on_action(cx.listener(Self::cycle_mode))
            .on_drop(cx.listener(|v, f: &ui::DraggedFile, w, cx| v.add_paths(vec![f.path.clone()], w, cx)))
            .on_drop(cx.listener(|v, p: &ExternalPaths, w, cx| v.add_paths(p.paths().to_vec(), w, cx)))
            .child({
                let width = self.width.clone();
                canvas(move |b, _, _| width.set(b.size.width / gpui::px(1.) / ui::scale()), |_, _, _, _| {}).absolute().size_full()
            })
            .child(self.render_header(cx))
            .child(log)
            .children((!empty && !self.following.get()).then(|| {
                // Scrolled up: a way back to the latest (it follows again from there).
                div().relative().h_0().child(
                    div()
                        .id("agent-latest")
                        .absolute()
                        .bottom(px(8.))
                        .right(px(16.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .h(px(24.))
                        .px(px(10.))
                        .rounded(px(12.))
                        .cursor_pointer()
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .shadow_md()
                        .text_size(px(11.5))
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.list.set_follow_mode(FollowMode::Tail);
                            v.following.set(true);
                            cx.notify();
                        }))
                        .child(if self.asks.is_empty() { "↓ Latest" } else { "↓ Waiting for your answer" }),
                )
            }))
            .child(self.render_bottom(window, cx))
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use similar::ChangeTag::*;

    /// A change deep in a long file is shown, with 3 lines around it, not the file from the top.
    #[test]
    fn diffs_show_the_changes_with_context() {
        let old: String = (1..=1000).map(|n| format!("line {n}\n")).collect();
        let new = old.replace("line 700\n", "line seven hundred\n").replace("line 2\n", "line two\n");
        let rows = super::diff_lines(&old, &new);
        assert!(rows.contains(&(Insert, "line seven hundred".into())) && rows.contains(&(Delete, "line 700".into())));
        assert!(rows.contains(&(Equal, "line 697".into())) && !rows.contains(&(Equal, "line 696".into())), "3 lines of context");
        assert!(rows.contains(&(Equal, "⋯".into())), "the two parts apart");
        assert!(rows.len() < 20, "{} rows", rows.len());
    }

    #[test]
    fn tool_output_loses_its_fence() {
        assert_eq!(super::unfence("```console\nname = \"oskl\"\n```"), "name = \"oskl\"");
        assert_eq!(super::unfence("plain"), "plain");
    }

    use super::{AgentView, Item};
    use agent_client_protocol::v1::schema::{ToolCall, ToolCallStatus, ToolKind};
    use gpui::{AppContext as _, Entity, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, Task, TestAppContext, VisualTestContext, div};

    /// A chat in a 320 px split, its agent "starting" (none runs) and working on a turn.
    struct Host(Entity<AgentView>);
    impl Render for Host {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div().w(gpui::px(320.)).h(gpui::px(600.)).child(self.0.clone())
        }
    }

    fn chat(cx: &mut TestAppContext) -> (Entity<AgentView>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::keybindings::init_kit(cx);
            crate::editor::bind_keys();
            super::bind_keys();
            crate::keybindings::apply(cx);
        });
        let mut view = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let v = cx.new(|cx| {
                let mut v = AgentView::new(std::env::temp_dir(), Some("claude"), None, Default::default(), window, cx);
                v._starting = Some(Task::ready(())); // never started: nothing runs npx
                v
            });
            view = Some(v.clone());
            Host(v)
        });
        (view.unwrap(), cx)
    }

    fn drawn(cx: &mut VisualTestContext, s: &'static str) -> Option<gpui::Bounds<gpui::Pixels>> {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds(s)
    }

    fn hover(cx: &mut VisualTestContext, s: &'static str) {
        let p = drawn(cx, s).unwrap_or_else(|| panic!("{s} is drawn")).center();
        cx.simulate_mouse_move(p, None, Modifiers::none());
        cx.run_until_parked();
    }

    fn click(cx: &mut VisualTestContext, s: &'static str) {
        let p = drawn(cx, s).unwrap_or_else(|| panic!("{s} is drawn")).center();
        cx.simulate_click(p, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn your_messages_go_back_in_the_box_or_again(cx: &mut TestAppContext) {
        let (v, cx) = chat(cx);
        v.update(cx, |v, _| {
            v.push(Item::User { text: "fix the rounding".into(), images: Vec::new(), context: vec!["<ide_selection>a.js</ide_selection>".into()] });
            v.push(Item::Text("Done.".into()));
            v.log.busy = true;
        });
        hover(cx, "user-msg-0"); // its buttons show on hover
        click(cx, "user-edit-0");
        assert_eq!(v.read_with(cx, |v, cx| v.input.read(cx).text()), "fix the rounding");
        hover(cx, "user-msg-0");
        click(cx, "user-retry-0");
        // The agent is working: it goes after the turn, selection and all.
        let queued = v.read_with(cx, |v, _| v.queue.iter().map(|m| (m.text.clone(), m.context.len())).collect::<Vec<_>>());
        assert_eq!(queued, vec![("fix the rounding".to_string(), 1)]);
    }

    #[gpui::test]
    fn running_calls_are_listed_and_the_turn_says_thinking(cx: &mut TestAppContext) {
        let (v, cx) = chat(cx);
        let mut sub = ToolCall::new("t2", "Find the callers").kind(ToolKind::Think).status(ToolCallStatus::InProgress);
        sub.meta = Some(serde_json::from_str(r#"{"claudeCode":{"toolName":"Agent"}}"#).unwrap());
        v.update(cx, |v, _| {
            v.push(Item::User { text: "go".into(), images: Vec::new(), context: Vec::new() });
            v.push(Item::Tool(ToolCall::new("t1", "Read cart.js").kind(ToolKind::Read).status(ToolCallStatus::Completed)));
            v.push(Item::Tool(sub));
            // Enough after it to scroll it out of sight.
            for n in 0..30 {
                v.push(Item::Text(format!("line {n}\n\nmore")));
            }
            v._starting = None; // as if started (nothing here focuses the box, which would start it)
        });
        assert!(drawn(cx, "agent-running").is_none(), "nothing runs while idle");
        assert!(drawn(cx, "agent-status").is_none());
        v.update(cx, |v, _| v.log.busy = true);
        assert!(drawn(cx, "agent-status").is_some(), "Thinking… at the end");
        assert_eq!(v.read_with(cx, |v, _| v.running()), vec![2], "the finished read isn't running");
        assert!(v.read_with(cx, |v, _| matches!(&v.log.items[2], Item::Tool(t) if super::is_subagent(t) && super::tool_look(t).1 == "Subagent")));
        // Nothing leaves the 320 px split.
        for s in ["agent-send", "agent-running", "agent-status"] {
            let b = drawn(cx, s).unwrap_or_else(|| panic!("{s} is drawn"));
            assert!(b.right() <= gpui::px(320.), "{s} ends at {:?}", b.right());
        }
        click(cx, "agent-running");
        assert_ne!(v.read_with(cx, |v, _| v.list.logical_scroll_top().item_ix), 2, "out of sight at first");
        click(cx, "running-row-2");
        assert!(!v.read_with(cx, |v, _| v.following.get()), "scrolled to the card, not following the end");
        assert_eq!(v.read_with(cx, |v, _| v.list.logical_scroll_top().item_ix), 2);
    }
}
