//! API tabs: a request and its response (HTTP, WebSocket, Server-Sent Events, GraphQL), an
//! environment's variables, a folder's (or collection's) settings, the collection runner,
//! and the cookie jar.

use crate::api::{self, Auth, Body, Collection, Environment, Kind, Row, SetVar, Test, codegen, run, send, tests, ws};
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::request::Request as WsRequest;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, MouseButton, MouseDownEvent, Pixels, Point, ScrollHandle, SharedString, Subscription, Task, Window,
    actions, anchored, deferred, div, img,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

actions!(api_view, [SendRequest, SaveRequest, CloseMenu]);

pub fn bind_keys() {
    crate::keybindings::collect([
        crate::keybindings::kb("cmd-enter", SendRequest, Some("ApiView")),
        crate::keybindings::kb("cmd-enter", SendRequest, Some("ApiView > Editor")),
        crate::keybindings::kb("cmd-s", SaveRequest, Some("ApiView")),
        crate::keybindings::kb("cmd-s", SaveRequest, Some("ApiView > Editor")),
        crate::keybindings::kb("escape", CloseMenu, Some("ApiView")),
    ]);
}

/// What an API tab shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ApiOpen {
    /// A saved request.
    Request { coll: String, id: String },
    /// A request not saved yet (new, from history, or imported), maybe meant for a folder.
    New { coll: Option<String>, folder: Option<String>, req: Option<Box<api::Request>> },
    /// A folder's (or, with the collection's id, the collection's) settings.
    Folder { coll: String, folder: String },
    Env(String),
    Runner { coll: String, folder: String },
    Cookies,
}

/// What API tabs ask of the workspace and the panel.
pub enum ApiRequest {
    Open(ApiOpen),
    /// Collections, environments or history changed: the panel reads them again.
    Changed,
    Close,
}

type Act = Rc<dyn Fn(&mut ApiView, &mut Window, &mut Context<ApiView>)>;

/// Saving a request that isn't in a collection: a name, and a place picked from a list
/// that can be searched (there may be many collections).
struct SaveDialog {
    name: Entity<Editor>,
    search: Entity<Editor>,
    /// The collection, and the folder in it (None: the collection itself).
    choice: Option<(String, Option<String>)>,
    list_open: bool,
    /// Naming a new collection (the box in the list), with its subscriptions.
    new_name: Option<(Entity<Editor>, Vec<Subscription>)>,
    _subs: Vec<Subscription>,
}

/// A place a request can be saved in: (collection, folder, "Collection / Folder", depth).
type Place = (String, Option<String>, String, usize);

fn places(roots: &[PathBuf]) -> Vec<Place> {
    fn walk(c: &Collection, f: &api::Folder, path: &str, depth: usize, out: &mut Vec<Place>) {
        for sub in &f.folders {
            let p = format!("{path} / {}", sub.name);
            out.push((c.id.clone(), Some(sub.id.clone()), p.clone(), depth));
            walk(c, sub, &p, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    for c in api::collections(roots) {
        out.push((c.id.clone(), None, c.name().to_string(), 0));
        walk(&c, &c.root, c.name(), 1, &mut out);
    }
    out
}

struct Menu {
    at: Point<Pixels>,
    items: Vec<(String, Act)>,
}

pub struct ApiView {
    focus: FocusHandle,
    pub open: ApiOpen,
    roots: Vec<PathBuf>,
    body: Body_,
    menu: Option<Menu>,
    /// Where the last button that opens a menu was pressed.
    menu_at: Option<Point<Pixels>>,
    /// "Save request": its name and where it goes, for a request not in a collection yet.
    save_dialog: Option<SaveDialog>,
    status: Option<Result<String, String>>,
    _subs: Vec<Subscription>,
}

#[allow(clippy::large_enum_variant)]
enum Body_ {
    Req(Box<ReqTab>),
    Env(Box<EnvTab>),
    Folder(Box<FolderTab>),
    Runner(Box<RunnerTab>),
    Cookies,
    Missing(String),
}

// ---------------------------------------------------------------- key/value tables

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KvId {
    Params,
    Path,
    Headers,
    Form,
    Multipart,
    Vars,
    FolderHeaders,
    FolderVars,
}

struct KvRow {
    on: bool,
    /// Multipart: a file. Variables: a secret.
    flag: bool,
    key: Entity<Editor>,
    value: Entity<Editor>,
    desc: Entity<Editor>,
    _subs: Vec<Subscription>,
}

struct Kv {
    id: KvId,
    rows: Vec<KvRow>,
}

impl Kv {
    fn rows(&self, cx: &App) -> Vec<Row> {
        self.rows
            .iter()
            .map(|r| Row { on: r.on, flag: r.flag, key: r.key.read(cx).text(), value: r.value.read(cx).text(), desc: r.desc.read(cx).text() })
            .filter(|r| !r.key.is_empty() || !r.value.is_empty())
            .collect()
    }

    fn is_blank(r: &KvRow, cx: &App) -> bool {
        r.key.read(cx).text().is_empty() && r.value.read(cx).text().is_empty() && r.desc.read(cx).text().is_empty()
    }
}

fn input(placeholder: &str, text: &str, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut ed = Editor::new(EditorMode::SingleLine, cx);
        ed.placeholder = placeholder.to_string().into();
        ed.set_text(text, cx);
        ed
    })
}

fn code_editor(text: &str, lang: &str, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut ed = Editor::new(EditorMode::Full, cx);
        ed.set_language(lang, cx);
        ed.set_text(text, cx);
        ed
    })
}

fn kv_row(id: KvId, r: &Row, cx: &mut Context<ApiView>) -> KvRow {
    let (kp, vp) = match id {
        KvId::Vars | KvId::FolderVars => ("Variable", "Value"),
        KvId::Headers | KvId::FolderHeaders => ("Header", "Value"),
        KvId::Multipart => ("Field", "Value or file"),
        _ => ("Key", "Value"),
    };
    let key = input(kp, &r.key, cx);
    let value = input(vp, &r.value, cx);
    let desc = input("Description", &r.desc, cx);
    if id == KvId::Vars && r.flag {
        value.update(cx, |e, _| e.secret = true);
    }
    let subs = [&key, &value, &desc].iter().map(|e| cx.subscribe(*e, move |v: &mut ApiView, _, ev: &EditorEvent, cx| {
        if matches!(ev, EditorEvent::Changed) {
            v.kv_changed(id, cx);
        }
    })).collect();
    KvRow { on: r.on, flag: r.flag, key, value, desc, _subs: subs }
}

fn make_kv(id: KvId, rows: &[Row], cx: &mut Context<ApiView>) -> Kv {
    let mut kv = Kv { id, rows: rows.iter().map(|r| kv_row(id, r, cx)).collect() };
    kv.rows.push(kv_row(id, &Row::default(), cx));
    kv
}

// ---------------------------------------------------------------- auth

const AUTH_FIELDS: [&str; 19] = [
    "user", "pass", "token", "prefix", "key", "value", "access_key", "secret_key", "session_token", "region", "service", "auth_url", "token_url", "client_id", "client_secret", "scope", "audience", "username", "password",
];
const SECRET_FIELDS: [&str; 5] = ["pass", "secret_key", "client_secret", "password", "session_token"];

struct AuthEd {
    kind: &'static str,
    /// The collection's own auth has nothing above it to inherit.
    allow_inherit: bool,
    query: bool,
    pkce: bool,
    basic: bool,
    grant: String,
    redirect: Entity<Editor>,
    f: HashMap<&'static str, Entity<Editor>>,
    token: Option<Result<String, String>>,
    task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl AuthEd {
    fn new(auth: &Auth, allow_inherit: bool, cx: &mut Context<ApiView>) -> AuthEd {
        let mut vals: HashMap<&'static str, String> = HashMap::new();
        let (mut query, mut pkce, mut basic, mut grant, mut redirect) = (false, true, false, "client_credentials".to_string(), "http://127.0.0.1:53682/callback".to_string());
        match auth {
            Auth::Basic { user, pass } | Auth::Digest { user, pass } => {
                vals.insert("user", user.clone());
                vals.insert("pass", pass.clone());
            }
            Auth::Bearer { token, prefix } => {
                vals.insert("token", token.clone());
                vals.insert("prefix", prefix.clone());
            }
            Auth::Apikey { key, value, query: q } => {
                vals.insert("key", key.clone());
                vals.insert("value", value.clone());
                query = *q;
            }
            Auth::Aws { access_key, secret_key, session_token, region, service } => {
                for (k, v) in [("access_key", access_key), ("secret_key", secret_key), ("session_token", session_token), ("region", region), ("service", service)] {
                    vals.insert(k, v.clone());
                }
            }
            Auth::Oauth2(o) => {
                for (k, v) in [("auth_url", &o.auth_url), ("token_url", &o.token_url), ("client_id", &o.client_id), ("client_secret", &o.client_secret), ("scope", &o.scope), ("audience", &o.audience), ("username", &o.username), ("password", &o.password), ("prefix", &o.prefix)] {
                    vals.insert(k, v.clone());
                }
                pkce = o.pkce;
                basic = o.basic_client_auth;
                grant = if o.grant.is_empty() { "client_credentials".into() } else { o.grant.clone() };
                if !o.redirect_uri.is_empty() {
                    redirect = o.redirect_uri.clone();
                }
            }
            _ => {}
        }
        let mut f = HashMap::new();
        let mut subs = Vec::new();
        for name in AUTH_FIELDS {
            let placeholder = match name {
                "prefix" => "Bearer",
                "key" => "X-API-Key",
                "region" => "us-east-1",
                "service" => "execute-api",
                "token" => "Token, or {{variable}}",
                _ => "",
            };
            let e = input(placeholder, vals.get(name).map(String::as_str).unwrap_or(""), cx);
            if SECRET_FIELDS.contains(&name) {
                e.update(cx, |e, _| e.secret = true);
            }
            subs.push(cx.subscribe(&e, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Changed) {
                    v.touch(cx);
                }
            }));
            f.insert(name, e);
        }
        let redirect = input("", &redirect, cx);
        subs.push(cx.subscribe(&redirect, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                v.touch(cx);
            }
        }));
        let kind = Auth::KINDS.iter().copied().find(|k| *k == auth.kind()).unwrap_or("None");
        let kind = if !allow_inherit && kind == "Inherit" { "None" } else { kind };
        AuthEd { kind, allow_inherit, query, pkce, basic, grant, redirect, f, token: None, task: None, _subs: subs }
    }

    fn get(&self, k: &str, cx: &App) -> String {
        self.f.get(k).map(|e| e.read(cx).text()).unwrap_or_default()
    }

    fn gather(&self, cx: &App) -> Auth {
        let g = |k: &str| self.get(k, cx);
        match self.kind {
            "None" => Auth::None,
            "Basic" => Auth::Basic { user: g("user"), pass: g("pass") },
            "Digest" => Auth::Digest { user: g("user"), pass: g("pass") },
            "Bearer" => Auth::Bearer { token: g("token"), prefix: if g("prefix").is_empty() { "Bearer".into() } else { g("prefix") } },
            "API Key" => Auth::Apikey { key: g("key"), value: g("value"), query: self.query },
            "AWS Signature" => Auth::Aws { access_key: g("access_key"), secret_key: g("secret_key"), session_token: g("session_token"), region: g("region"), service: g("service") },
            "OAuth 2.0" => Auth::Oauth2(api::OAuth2 {
                grant: self.grant.clone(),
                auth_url: g("auth_url"),
                token_url: g("token_url"),
                client_id: g("client_id"),
                client_secret: g("client_secret"),
                scope: g("scope"),
                audience: g("audience"),
                username: g("username"),
                password: g("password"),
                redirect_uri: self.redirect.read(cx).text(),
                pkce: self.pkce,
                basic_client_auth: self.basic,
                prefix: if g("prefix").is_empty() { "Bearer".into() } else { g("prefix") },
            }),
            _ => Auth::Inherit,
        }
    }
}

// ---------------------------------------------------------------- the request tab

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Params,
    Headers,
    Auth,
    Body,
    Tests,
    Vars,
    Scripts,
    Settings,
    Code,
    Docs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RespSection {
    Body,
    Headers,
    Cookies,
    Tests,
    Timeline,
    Console,
}

struct TestRow {
    on: bool,
    what: String,
    op: String,
    path: Entity<Editor>,
    value: Entity<Editor>,
    _subs: Vec<Subscription>,
}

struct VarRow {
    on: bool,
    from: String,
    scope: String,
    var: Entity<Editor>,
    path: Entity<Editor>,
    _subs: Vec<Subscription>,
}

/// A response on screen. Its temporary files go when it does.
struct Resp {
    r: send::Response,
    tests: Vec<tests::Outcome>,
    logs: Vec<String>,
    missing: Vec<String>,
    image: Option<PathBuf>,
    pretty: bool,
}

impl Drop for Resp {
    fn drop(&mut self) {
        if let Some(f) = self.r.body_file.take() {
            std::fs::remove_file(f).ok();
        }
        if let Some(f) = self.image.take() {
            std::fs::remove_file(f).ok();
        }
    }
}

struct Live {
    commands: tokio::sync::mpsc::UnboundedSender<ws::Command>,
    connected: bool,
    status: Option<u16>,
    events: Vec<ws::Event>,
    _task: Task<()>,
}

/// The most live messages kept on screen.
const MAX_EVENTS: usize = 1000;

struct ReqTab {
    coll: Option<String>,
    /// Where it goes when saved (a folder of `coll`).
    folder: Option<String>,
    /// Non-text parts (kinds, switches, settings that aren't typed).
    req: api::Request,
    saved: bool,
    dirty: bool,
    name: Entity<Editor>,
    url: Entity<Editor>,
    params: Kv,
    path: Kv,
    headers: Kv,
    body_text: Entity<Editor>,
    gql_vars: Entity<Editor>,
    form: Kv,
    multipart: Kv,
    binary: Entity<Editor>,
    auth: AuthEd,
    tests: Vec<TestRow>,
    vars: Vec<VarRow>,
    pre: Entity<Editor>,
    post: Entity<Editor>,
    docs: Entity<Editor>,
    s_timeout: Entity<Editor>,
    s_max: Entity<Editor>,
    s_proxy: Entity<Editor>,
    s_noproxy: Entity<Editor>,
    s_cert: Entity<Editor>,
    s_cert_pass: Entity<Editor>,
    s_ca: Entity<Editor>,
    section: Section,
    code_lang: usize,
    code: Entity<Editor>,
    resp: Option<Resp>,
    resp_body: Entity<Editor>,
    resp_section: RespSection,
    error: Option<String>,
    sending: Option<(Instant, tokio::task::AbortHandle, Task<()>)>,
    live: Option<Live>,
    ws_msg: Entity<Editor>,
    schema: Option<Result<Entity<Editor>, String>>,
    schema_task: Option<Task<()>>,
    stacked: bool,
    live_scroll: ScrollHandle,
}

fn body_lang(b: &Body) -> &'static str {
    match b {
        Body::Json { .. } => "json",
        Body::Xml { .. } | Body::Html { .. } => "html",
        _ => "",
    }
}

// ---------------------------------------------------------------- other tabs

struct EnvTab {
    env: Environment,
    name: Entity<Editor>,
    vars: Kv,
    dotenv: Entity<Editor>,
    dirty: bool,
}

struct FolderTab {
    coll: Collection,
    folder: String,
    name: Entity<Editor>,
    auth: AuthEd,
    headers: Kv,
    vars: Kv,
    pre: Entity<Editor>,
    post: Entity<Editor>,
    pre_requests: Vec<String>,
    dirty: bool,
    section: Section,
}

struct RunRow {
    name: String,
    method: String,
    status: Option<u16>,
    ms: u64,
    passed: usize,
    failed: Vec<String>,
    error: Option<String>,
}

struct RunnerTab {
    coll: Collection,
    folder: String,
    iterations: Entity<Editor>,
    delay: Entity<Editor>,
    data: Entity<Editor>,
    rows: Vec<RunRow>,
    running: Option<Task<()>>,
    started: Option<Instant>,
    took: Option<u64>,
    skip: std::collections::HashSet<String>,
}

impl EventEmitter<WsRequest> for ApiView {}

fn find_coll(roots: &[PathBuf], id: &str) -> Option<Collection> {
    api::collections(roots).into_iter().find(|c| c.id == id)
}

impl ApiView {
    pub fn new(open: ApiOpen, roots: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut v = ApiView { focus: cx.focus_handle(), open: open.clone(), roots: roots.clone(), body: Body_::Missing(String::new()), menu: None, menu_at: None, save_dialog: None, status: None, _subs: Vec::new() };
        v.body = match open {
            ApiOpen::Request { coll, id } => match find_coll(&roots, &coll).and_then(|c| api::load_request(&c, &id).map(|r| (c, r))) {
                Some((c, r)) => {
                    let folder = c.root.chain_to(&id).last().map(|f| f.id.clone());
                    Body_::Req(Box::new(v.req_tab(Some(c.id.clone()), folder, r, true, cx)))
                }
                None => Body_::Missing("This request was deleted.".into()),
            },
            ApiOpen::New { coll, folder, req } => {
                let r = req.map(|r| *r).unwrap_or_default();
                Body_::Req(Box::new(v.req_tab(coll, folder, r, false, cx)))
            }
            ApiOpen::Folder { coll, folder } => match find_coll(&roots, &coll) {
                Some(c) if c.root.find(&folder).is_some() => Body_::Folder(Box::new(v.folder_tab(c, folder, cx))),
                _ => Body_::Missing("This folder was deleted.".into()),
            },
            ApiOpen::Env(id) => match api::environments(&roots).into_iter().find(|e| e.id == id) {
                Some(e) => Body_::Env(Box::new(v.env_tab(e, cx))),
                None => Body_::Missing("This environment was deleted.".into()),
            },
            ApiOpen::Runner { coll, folder } => match find_coll(&roots, &coll) {
                Some(c) => Body_::Runner(Box::new(RunnerTab {
                    coll: c,
                    folder,
                    iterations: input("1", "1", cx),
                    delay: input("0", "0", cx),
                    data: input("A CSV or JSON file: one run per row (optional)", "", cx),
                    rows: Vec::new(),
                    running: None,
                    started: None,
                    took: None,
                    skip: Default::default(),
                })),
                None => Body_::Missing("This collection was deleted.".into()),
            },
            ApiOpen::Cookies => Body_::Cookies,
        };
        v
    }

    fn req_tab(&mut self, coll: Option<String>, folder: Option<String>, r: api::Request, saved: bool, cx: &mut Context<Self>) -> ReqTab {
        let name = input("Request name", &r.name, cx);
        let url = input("https://api.example.com/items  —  or paste a cURL command", &r.url, cx);
        let (text, vars) = match &r.body {
            Body::Json { text } | Body::Xml { text } | Body::Text { text } | Body::Html { text } => (text.clone(), String::new()),
            Body::Graphql { query, variables } => (query.clone(), variables.clone()),
            _ => (String::new(), String::new()),
        };
        let body_text = code_editor(&text, if matches!(r.body, Body::Graphql { .. }) { "" } else { body_lang(&r.body) }, cx);
        let gql_vars = code_editor(&vars, "json", cx);
        let (form_rows, mp_rows, bin) = match &r.body {
            Body::Form { rows } => (rows.clone(), Vec::new(), String::new()),
            Body::Multipart { rows } => (Vec::new(), rows.clone(), String::new()),
            Body::Binary { path } => (Vec::new(), Vec::new(), path.clone()),
            _ => (Vec::new(), Vec::new(), String::new()),
        };
        let params_rows = if r.params.is_empty() { send::query_rows(&r.url) } else { r.params.clone() };
        let path_rows = {
            let mut rows = r.path_params.clone();
            for n in send::path_param_names(&r.url) {
                if !rows.iter().any(|p| p.key == n) {
                    rows.push(Row::new(&n, ""));
                }
            }
            rows
        };
        let s = r.settings.clone();
        let mut t = ReqTab {
            coll,
            folder,
            saved,
            dirty: false,
            name,
            url,
            params: make_kv(KvId::Params, &params_rows, cx),
            path: Kv { id: KvId::Path, rows: path_rows.iter().map(|p| kv_row(KvId::Path, p, cx)).collect() },
            headers: make_kv(KvId::Headers, &r.headers, cx),
            body_text,
            gql_vars,
            form: make_kv(KvId::Form, &form_rows, cx),
            multipart: make_kv(KvId::Multipart, &mp_rows, cx),
            binary: input("/path/to/file", &bin, cx),
            auth: AuthEd::new(&r.auth, true, cx),
            tests: Vec::new(),
            vars: Vec::new(),
            pre: code_editor(&r.pre_script, "js", cx),
            post: code_editor(&r.post_script, "js", cx),
            docs: cx.new(|cx| {
                let mut e = Editor::new(EditorMode::Full, cx);
                e.set_text(&r.docs, cx);
                e
            }),
            s_timeout: input("30000", &s.timeout_ms.to_string(), cx),
            s_max: input("10", &s.max_redirects.to_string(), cx),
            s_proxy: input("System settings (or http://host:port, socks5://…, none)", &s.proxy, cx),
            s_noproxy: input("localhost, .internal", &s.no_proxy, cx),
            s_cert: input("/path/to/client.p12", &s.client_cert, cx),
            s_cert_pass: input("Certificate password", &s.client_cert_pass, cx),
            s_ca: input("/path/to/ca.pem", &s.ca_cert, cx),
            section: if r.body != Body::None && r.method != "GET" { Section::Body } else { Section::Params },
            code_lang: 0,
            code: code_editor("", "", cx),
            resp: None,
            resp_body: cx.new(|cx| {
                let mut e = Editor::new(EditorMode::Full, cx);
                e.read_only = true;
                e
            }),
            resp_section: RespSection::Body,
            error: None,
            sending: None,
            live: None,
            ws_msg: code_editor("", "json", cx),
            schema: None,
            schema_task: None,
            stacked: false,
            live_scroll: ScrollHandle::new(),
            req: r.clone(),
        };
        t.s_cert_pass.update(cx, |e, _| e.secret = true);
        t.code.update(cx, |e, _| e.read_only = true);
        for test in &r.tests {
            let row = self.test_row(test, cx);
            t.tests.push(row);
        }
        for sv in &r.set_vars {
            let row = self.var_row(sv, cx);
            t.vars.push(row);
        }
        // Typing anywhere marks it changed; the URL and Params follow each other.
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&t.url, |v: &mut ApiView, _, ev: &EditorEvent, cx| match ev {
            EditorEvent::Changed => v.url_changed(cx),
            EditorEvent::Submit => v.send(cx),
            _ => {}
        }));
        for e in [&t.name, &t.body_text, &t.gql_vars, &t.binary, &t.pre, &t.post, &t.docs, &t.s_timeout, &t.s_max, &t.s_proxy, &t.s_noproxy, &t.s_cert, &t.s_cert_pass, &t.s_ca] {
            subs.push(cx.subscribe(e, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Changed) {
                    v.touch(cx);
                }
            }));
        }
        subs.push(cx.subscribe(&t.ws_msg, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Submit) {
                v.ws_send(cx);
            }
        }));
        self._subs.extend(subs);
        t
    }

    fn test_row(&mut self, t: &Test, cx: &mut Context<Self>) -> TestRow {
        let path = input("$.data.id  or a header's name", &t.path, cx);
        let value = input("Value", &t.value, cx);
        let subs = [&path, &value].iter().map(|e| cx.subscribe(*e, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                v.touch(cx);
            }
        })).collect();
        TestRow { on: t.on, what: t.what.clone(), op: t.op.clone(), path, value, _subs: subs }
    }

    fn var_row(&mut self, s: &SetVar, cx: &mut Context<Self>) -> VarRow {
        let var = input("Variable", &s.var, cx);
        let path = input("$.token", &s.path, cx);
        let subs = [&var, &path].iter().map(|e| cx.subscribe(*e, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                v.touch(cx);
            }
        })).collect();
        VarRow { on: s.on, from: s.from.clone(), scope: s.scope.clone(), var, path, _subs: subs }
    }

    fn env_tab(&mut self, e: Environment, cx: &mut Context<Self>) -> EnvTab {
        let full = api::with_secrets(&e);
        let name = input("Environment name", &e.name, cx);
        let dotenv = input("/path/to/.env (optional)", &e.dotenv, cx);
        for ed in [&name, &dotenv] {
            let s = cx.subscribe(ed, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Changed) {
                    v.touch(cx);
                }
            });
            self._subs.push(s);
        }
        EnvTab { vars: make_kv(KvId::Vars, &full.vars, cx), env: e, name, dotenv, dirty: false }
    }

    fn folder_tab(&mut self, c: Collection, folder: String, cx: &mut Context<Self>) -> FolderTab {
        let f = c.root.find(&folder).cloned().unwrap_or_default();
        let is_root = folder == c.root.id;
        let name = input("Name", &f.name, cx);
        let pre = code_editor(&f.pre_script, "js", cx);
        let post = code_editor(&f.post_script, "js", cx);
        for ed in [&name, &pre, &post] {
            let s = cx.subscribe(ed, |v: &mut ApiView, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Changed) {
                    v.touch(cx);
                }
            });
            self._subs.push(s);
        }
        FolderTab {
            auth: AuthEd::new(&f.auth, !is_root, cx),
            headers: make_kv(KvId::FolderHeaders, &f.headers, cx),
            vars: make_kv(KvId::FolderVars, &f.vars, cx),
            pre_requests: f.pre_requests.clone(),
            coll: c,
            folder,
            name,
            pre,
            post,
            dirty: false,
            section: Section::Auth,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        match &self.body {
            Body_::Req(t) => t.url.read(cx).focus(window),
            Body_::Env(t) => t.name.read(cx).focus(window),
            Body_::Folder(t) => t.name.read(cx).focus(window),
            _ => window.focus(&self.focus),
        }
    }

    /// The tab's title, its tooltip, and whether it has unsaved changes.
    pub fn title(&self, cx: &App) -> (String, String, bool) {
        match &self.body {
            Body_::Req(t) => {
                let name = t.name.read(cx).text();
                let name = if name.trim().is_empty() { "Request".to_string() } else { name };
                (name, t.url.read(cx).text(), t.dirty || !t.saved)
            }
            Body_::Env(t) => (format!("Env: {}", t.env.name), String::new(), t.dirty),
            Body_::Folder(t) => (format!("{}: settings", t.coll.root.find(&t.folder).map(|f| f.name.clone()).unwrap_or_default()), String::new(), t.dirty),
            Body_::Runner(t) => (format!("Run: {}", t.coll.root.find(&t.folder).map(|f| f.name.clone()).unwrap_or_default()), String::new(), false),
            Body_::Cookies => ("Cookies".into(), String::new(), false),
            Body_::Missing(_) => ("API".into(), String::new(), false),
        }
    }

    /// The method badge's text for the tab (requests).
    pub fn method(&self) -> Option<String> {
        match &self.body {
            Body_::Req(t) => Some(match t.req.kind {
                Kind::Http => t.req.method.clone(),
                Kind::Websocket => "WS".into(),
                Kind::Sse => "SSE".into(),
            }),
            _ => None,
        }
    }

    pub fn shows(&self, open: &ApiOpen) -> bool {
        !matches!(open, ApiOpen::New { .. }) && &self.open == open
    }

    /// Unsaved changes (closing asks first).
    pub fn is_dirty(&self) -> bool {
        match &self.body {
            Body_::Req(t) => t.dirty && t.saved,
            Body_::Env(t) => t.dirty,
            Body_::Folder(t) => t.dirty,
            _ => false,
        }
    }

    fn project(&self) -> Option<PathBuf> {
        self.roots.first().cloned()
    }

    fn touch(&mut self, cx: &mut Context<Self>) {
        match &mut self.body {
            Body_::Req(t) => t.dirty = true,
            Body_::Env(t) => t.dirty = true,
            Body_::Folder(t) => t.dirty = true,
            _ => {}
        }
        cx.notify();
    }

    fn kv_mut(&mut self, id: KvId) -> Option<&mut Kv> {
        match (&mut self.body, id) {
            (Body_::Req(t), KvId::Params) => Some(&mut t.params),
            (Body_::Req(t), KvId::Path) => Some(&mut t.path),
            (Body_::Req(t), KvId::Headers) => Some(&mut t.headers),
            (Body_::Req(t), KvId::Form) => Some(&mut t.form),
            (Body_::Req(t), KvId::Multipart) => Some(&mut t.multipart),
            (Body_::Env(t), KvId::Vars) => Some(&mut t.vars),
            (Body_::Folder(t), KvId::FolderHeaders) => Some(&mut t.headers),
            (Body_::Folder(t), KvId::FolderVars) => Some(&mut t.vars),
            _ => None,
        }
    }

    /// A table changed: there's always an empty row last, and Params rewrite the URL.
    fn kv_changed(&mut self, id: KvId, cx: &mut Context<Self>) {
        let needs_row = self.kv_mut(id).is_some_and(|kv| id != KvId::Path && kv.rows.last().is_none_or(|r| !Kv::is_blank(r, cx)));
        if needs_row {
            let row = kv_row(id, &Row::default(), cx);
            if let Some(kv) = self.kv_mut(id) {
                kv.rows.push(row);
            }
        }
        if id == KvId::Params {
            self.params_changed(cx);
        }
        self.touch(cx);
    }

    fn params_changed(&mut self, cx: &mut Context<Self>) {
        let Body_::Req(t) = &mut self.body else { return };
        let url = t.url.read(cx).text();
        let rows = t.params.rows(cx);
        let on: Vec<(String, String)> = rows.iter().filter(|r| r.on && !r.key.is_empty()).map(|r| (r.key.clone(), r.value.clone())).collect();
        let now: Vec<(String, String)> = send::query_rows(&url).into_iter().map(|r| (r.key, r.value)).collect();
        if on != now {
            let new = send::with_query(&url, &rows);
            t.url.update(cx, |e, cx| e.set_text(&new, cx));
        }
    }

    /// The URL changed: Params and path parameters follow it (and a pasted cURL fills it all in).
    fn url_changed(&mut self, cx: &mut Context<Self>) {
        let Body_::Req(t) = &mut self.body else { return };
        let url = t.url.read(cx).text();
        if url.trim_start().starts_with("curl ") {
            if let Ok(mut r) = api::import::curl(&url) {
                r.id = t.req.id.clone();
                let keep_name = t.name.read(cx).text();
                if !keep_name.is_empty() && keep_name != "New Request" {
                    r.name = keep_name;
                }
                let (coll, folder, saved) = (t.coll.clone(), t.folder.clone(), t.saved);
                let mut nt = self.req_tab(coll, folder, r, saved, cx);
                nt.dirty = true;
                self.body = Body_::Req(Box::new(nt));
                self.status = Some(Ok("Filled in from the cURL command.".into()));
                cx.notify();
            }
            return;
        }
        t.dirty = true;
        // Query → Params (the disabled rows stay as they are).
        let parsed = send::query_rows(&url);
        let enabled: Vec<usize> = t.params.rows.iter().enumerate().filter(|(_, r)| r.on && !Kv::is_blank(r, cx)).map(|(i, _)| i).collect();
        let current: Vec<(String, String)> = enabled.iter().map(|i| (t.params.rows[*i].key.read(cx).text(), t.params.rows[*i].value.read(cx).text())).collect();
        let wanted: Vec<(String, String)> = parsed.iter().map(|r| (r.key.clone(), r.value.clone())).collect();
        if current != wanted {
            for (n, (k, v)) in wanted.iter().enumerate() {
                match enabled.get(n) {
                    Some(ix) => {
                        let r = &t.params.rows[*ix];
                        if r.key.read(cx).text() != *k {
                            r.key.update(cx, |e, cx| e.set_text(k, cx));
                        }
                        if r.value.read(cx).text() != *v {
                            r.value.update(cx, |e, cx| e.set_text(v, cx));
                        }
                    }
                    None => {
                        let row = kv_row(KvId::Params, &Row::new(k, v), cx);
                        let at = t.params.rows.len().saturating_sub(1);
                        t.params.rows.insert(at, row);
                    }
                }
            }
            for ix in enabled.iter().skip(wanted.len()).rev() {
                t.params.rows.remove(*ix);
            }
        }
        // Path parameters: one row per :name / {name}.
        let names = send::path_param_names(&url);
        let have: Vec<String> = t.path.rows.iter().map(|r| r.key.read(cx).text()).collect();
        if names != have {
            let mut old: HashMap<String, KvRow> = t.path.rows.drain(..).map(|r| (r.key.read(cx).text(), r)).collect();
            for n in names {
                let row = old.remove(&n).unwrap_or_else(|| kv_row(KvId::Path, &Row::new(&n, ""), cx));
                t.path.rows.push(row);
            }
        }
        cx.notify();
    }

    /// The request as typed (what's saved, and what's sent).
    fn gather(t: &ReqTab, cx: &App) -> api::Request {
        let mut r = t.req.clone();
        r.name = t.name.read(cx).text();
        r.url = t.url.read(cx).text().trim().to_string();
        r.params = t.params.rows(cx);
        r.path_params = t.path.rows(cx);
        r.headers = t.headers.rows(cx);
        let text = t.body_text.read(cx).text();
        r.body = match &t.req.body {
            Body::None => Body::None,
            Body::Json { .. } => Body::Json { text },
            Body::Xml { .. } => Body::Xml { text },
            Body::Text { .. } => Body::Text { text },
            Body::Html { .. } => Body::Html { text },
            Body::Form { .. } => Body::Form { rows: t.form.rows(cx) },
            Body::Multipart { .. } => Body::Multipart { rows: t.multipart.rows(cx) },
            Body::Binary { .. } => Body::Binary { path: t.binary.read(cx).text() },
            Body::Graphql { .. } => Body::Graphql { query: text, variables: t.gql_vars.read(cx).text() },
        };
        r.auth = t.auth.gather(cx);
        r.tests = t.tests.iter().map(|x| Test { on: x.on, what: x.what.clone(), op: x.op.clone(), path: x.path.read(cx).text(), value: x.value.read(cx).text() }).collect();
        r.set_vars = t.vars.iter().map(|x| SetVar { on: x.on, var: x.var.read(cx).text(), from: x.from.clone(), path: x.path.read(cx).text(), scope: x.scope.clone() }).filter(|s| !s.var.is_empty()).collect();
        r.pre_script = t.pre.read(cx).text();
        r.post_script = t.post.read(cx).text();
        r.docs = t.docs.read(cx).text();
        r.settings.timeout_ms = t.s_timeout.read(cx).text().trim().parse().unwrap_or(30_000);
        r.settings.max_redirects = t.s_max.read(cx).text().trim().parse().unwrap_or(10);
        r.settings.proxy = t.s_proxy.read(cx).text().trim().to_string();
        r.settings.no_proxy = t.s_noproxy.read(cx).text().trim().to_string();
        r.settings.client_cert = t.s_cert.read(cx).text().trim().to_string();
        r.settings.client_cert_pass = t.s_cert_pass.read(cx).text();
        r.settings.ca_cert = t.s_ca.read(cx).text().trim().to_string();
        r
    }

    fn envs(&self) -> (Vec<Environment>, Option<Environment>, Option<Environment>) {
        let all = api::environments(&self.roots);
        let active_id = api::active_env(self.project().as_deref());
        let active = active_id.and_then(|id| all.iter().find(|e| e.id == id && !e.global).cloned());
        let global = all.iter().find(|e| e.global).cloned();
        (all, active, global)
    }

    // ------------------------------------------------------------ sending

    fn send(&mut self, cx: &mut Context<Self>) {
        let roots = self.roots.clone();
        let (_, env, global) = self.envs();
        let Body_::Req(t) = &mut self.body else { return };
        if t.sending.is_some() {
            return;
        }
        let req = Self::gather(t, cx);
        if req.url.is_empty() {
            t.error = Some("Type a URL first.".into());
            cx.notify();
            return;
        }
        match req.kind {
            Kind::Websocket | Kind::Sse => return self.connect_live(cx),
            Kind::Http => {}
        }
        let coll = t.coll.as_ref().and_then(|id| find_coll(&roots, id));
        let env_id = env.as_ref().map(|e| e.id.clone());
        let plan = run::Plan { req: req.clone(), coll, env, global, ..Default::default() };
        let task = crate::db::engine::runtime().spawn(run::execute(plan));
        let abort = task.abort_handle();
        t.error = None;
        let started = Instant::now();
        let gpui_task = cx.spawn(async move |this, cx| {
            let done = task.await;
            let done = match done {
                Ok(d) => d,
                Err(_) => run::Done { error: Some("Cancelled.".into()), ..Default::default() },
            };
            let changes = done.var_changes.clone();
            let roots2 = roots.clone();
            let coll_id = this.update(cx, |v, _| if let Body_::Req(t) = &v.body { t.coll.clone() } else { None }).ok().flatten();
            // Variables a run set are kept (off this thread: secret ones are in the Keychain).
            let kept = cx
                .background_executor()
                .spawn(async move {
                    let mut coll = coll_id.and_then(|id| find_coll(&roots2, &id));
                    run::keep_changes(&changes, &roots2, env_id.as_deref(), coll.as_mut())
                })
                .await;
            this.update(cx, |v, cx| {
                v.finish(req, done, cx);
                if kept {
                    cx.emit(WsRequest::Api(ApiRequest::Changed));
                }
            })
            .ok();
        });
        t.sending = Some((started, abort, gpui_task));
        // A ticking clock while it's on its way.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                let going = this.update(cx, |v, cx| {
                    cx.notify();
                    matches!(&v.body, Body_::Req(t) if t.sending.is_some())
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Body_::Req(t) = &mut self.body {
            if let Some((_, abort, _)) = t.sending.take() {
                abort.abort();
                t.error = Some("Cancelled.".into());
            }
            if let Some(l) = &t.live {
                l.commands.send(ws::Command::Close).ok();
            }
        }
        cx.notify();
    }

    fn finish(&mut self, sent: api::Request, done: run::Done, cx: &mut Context<Self>) {
        let Body_::Req(t) = &mut self.body else { return };
        t.sending = None;
        let mut status = 0;
        let (mut ms, mut size) = (0, 0);
        match done.resp {
            None => {
                t.error = Some(done.error.unwrap_or_else(|| "No answer.".into()));
                t.resp = None;
                if !done.logs.is_empty() {
                    t.error = Some(format!("{}\n\n{}", t.error.clone().unwrap_or_default(), done.logs.join("\n")));
                }
            }
            Some(r) => {
                status = r.status;
                ms = r.ms;
                size = r.size;
                let ct = r.content_type();
                let image = if ct.starts_with("image/") && r.body_file.is_none() {
                    let ext = ct.trim_start_matches("image/").split([';', '+']).next().unwrap_or("png").to_string();
                    let p = std::env::temp_dir().join(format!("kite-image-{}.{ext}", api::new_id()));
                    std::fs::write(&p, &r.body).ok().map(|_| p)
                } else {
                    None
                };
                let resp = Resp { r, tests: done.tests, logs: done.logs, missing: done.missing, image, pretty: true };
                Self::show_body(&t.resp_body, &resp, cx);
                if resp.tests.iter().any(|x| !x.pass) && t.resp_section == RespSection::Body {
                    // Failures are worth seeing; the body is one click away.
                }
                t.resp = Some(resp);
                t.error = None;
            }
        }
        api::add_history(&api::Visit { id: api::new_id(), at: api::now_secs(), status, ms, size, request: sent });
        cx.emit(WsRequest::Api(ApiRequest::Changed));
        cx.notify();
    }

    /// The body into the viewer: JSON indented (unless Raw), highlighted when not huge.
    fn show_body(viewer: &Entity<Editor>, resp: &Resp, cx: &mut App) {
        let ct = resp.r.content_type();
        let text = resp.r.text();
        let (shown, lang) = if ct.contains("json") || (text.trim_start().starts_with(['{', '[']) && !ct.contains("html")) {
            let pretty = if resp.pretty { serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()) } else { None };
            (pretty.unwrap_or(text), "json")
        } else if ct.contains("html") || ct.contains("xml") {
            (text, "html")
        } else if ct.contains("javascript") {
            (text, "js")
        } else if ct.contains("css") {
            (text, "css")
        } else {
            (text, "")
        };
        let lang = if shown.len() > 512 * 1024 { "" } else { lang };
        viewer.update(cx, |e, cx| {
            e.set_language(lang, cx);
            e.set_text(&shown, cx);
            // From the top.
            e.go_to(0, 0, cx);
        });
    }

    // ------------------------------------------------------------ WebSocket and SSE

    fn connect_live(&mut self, cx: &mut Context<Self>) {
        let (_, env, global) = self.envs();
        let roots = self.roots.clone();
        let Body_::Req(t) = &mut self.body else { return };
        if let Some(l) = &t.live
            && l.connected
        {
            l.commands.send(ws::Command::Close).ok();
            return;
        }
        let req = Self::gather(t, cx);
        let coll = t.coll.as_ref().and_then(|id| find_coll(&roots, id));
        let env = env.map(|e| api::with_secrets(&e));
        let global = global.map(|g| api::with_secrets(&g));
        let folders = coll.as_ref().map(|c| run::chain(c, &req.id)).unwrap_or_default();
        let inner: Vec<&api::Folder> = folders.iter().rev().copied().collect();
        let rows: Vec<&[Row]> = inner.iter().map(|f| f.vars.as_slice()).collect();
        let vars = api::vars::Vars::new(&rows, env.as_ref(), global.as_ref());
        let ready = send::prepare(&req, &inner, &vars);
        let mut live = if req.kind == Kind::Websocket { ws::websocket(ready) } else { ws::sse(ready) };
        let mut events = std::mem::replace(&mut live.events, futures::channel::mpsc::unbounded().1);
        let task = cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(ev) = events.next().await {
                // Whatever else came meanwhile, in one redraw.
                let mut batch = vec![ev];
                while let Ok(more) = events.try_recv() {
                    batch.push(more);
                    if batch.len() > 200 {
                        break;
                    }
                }
                let ok = this.update(cx, |v, cx| {
                    if let Body_::Req(t) = &mut v.body
                        && let Some(l) = &mut t.live
                    {
                        for ev in batch {
                            match &ev {
                                ws::Event::Open { status, .. } => {
                                    l.connected = true;
                                    l.status = Some(*status);
                                }
                                ws::Event::Closed { .. } | ws::Event::Error(_) => l.connected = false,
                                _ => {}
                            }
                            l.events.push(ev);
                        }
                        if l.events.len() > MAX_EVENTS {
                            let extra = l.events.len() - MAX_EVENTS;
                            l.events.drain(..extra);
                        }
                        t.live_scroll.scroll_to_bottom();
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        });
        t.live = Some(Live { commands: live.commands, connected: false, status: None, events: Vec::new(), _task: task });
        t.error = None;
        api::add_history(&api::Visit { id: api::new_id(), at: api::now_secs(), request: req, ..Default::default() });
        cx.notify();
    }

    fn ws_send(&mut self, cx: &mut Context<Self>) {
        let Body_::Req(t) = &mut self.body else { return };
        let Some(l) = &t.live else { return };
        if !l.connected {
            return;
        }
        let text = t.ws_msg.read(cx).text();
        if text.is_empty() {
            return;
        }
        l.commands.send(ws::Command::Text(text)).ok();
        cx.notify();
    }

    fn fetch_schema(&mut self, cx: &mut Context<Self>) {
        let (_, env, global) = self.envs();
        let Body_::Req(t) = &mut self.body else { return };
        let req = Self::gather(t, cx);
        let env = env.map(|e| api::with_secrets(&e));
        let global = global.map(|g| api::with_secrets(&g));
        let vars = api::vars::Vars::new(&[], env.as_ref(), global.as_ref());
        let ready = send::prepare(&req, &[], &vars);
        let work = crate::db::engine::run(async move { ws::fetch_schema(ready).await });
        t.schema = None;
        t.schema_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Body_::Req(t) = &mut v.body {
                    t.schema = Some(match r {
                        Ok(s) => {
                            let text = ws::schema_text(&s);
                            let ed = cx.new(|cx| {
                                let mut e = Editor::new(EditorMode::Full, cx);
                                e.read_only = true;
                                e.set_text(&text, cx);
                                e
                            });
                            Ok(ed)
                        }
                        Err(e) => Err(e),
                    });
                    t.schema_task = None;
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ------------------------------------------------------------ saving

    fn save(&mut self, _: &SaveRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.save_now(Some(window), cx);
    }

    fn save_now(&mut self, _window: Option<&mut Window>, cx: &mut Context<Self>) {
        let roots = self.roots.clone();
        let project = self.project();
        match &mut self.body {
            Body_::Req(t) => {
                let req = Self::gather(t, cx);
                let coll = match t.coll.as_ref().and_then(|id| find_coll(&roots, id)) {
                    Some(c) => Some(c),
                    None => {
                        // Not in a collection yet: pick one (or make the first).
                        let all = api::collections(&roots);
                        if all.is_empty() {
                            let c = Collection::new("My Requests", project);
                            api::save_collection(&c).ok();
                            Some(c)
                        } else {
                            self.save_menu(cx);
                            return;
                        }
                    }
                };
                let Some(mut c) = coll else { return };
                let Body_::Req(t) = &mut self.body else { return };
                match api::save_request(&mut c, &req, t.folder.as_deref()) {
                    Ok(()) => {
                        t.coll = Some(c.id.clone());
                        t.saved = true;
                        t.dirty = false;
                        t.req = req.clone();
                        self.open = ApiOpen::Request { coll: c.id.clone(), id: req.id.clone() };
                        self.status = None;
                        cx.emit(WsRequest::Api(ApiRequest::Changed));
                    }
                    Err(e) => self.status = Some(Err(e)),
                }
            }
            Body_::Env(t) => {
                let mut e = t.env.clone();
                e.name = t.name.read(cx).text();
                e.vars = t.vars.rows(cx);
                e.dotenv = t.dotenv.read(cx).text().trim().to_string();
                match api::save_environment(&e) {
                    Ok(()) => {
                        t.env = e;
                        t.dirty = false;
                        cx.emit(WsRequest::Api(ApiRequest::Changed));
                    }
                    Err(err) => self.status = Some(Err(err)),
                }
            }
            Body_::Folder(t) => {
                let mut c = find_coll(&roots, &t.coll.id).unwrap_or_else(|| t.coll.clone());
                let auth = t.auth.gather(cx);
                let (name, headers, vars, pre, post) = (t.name.read(cx).text(), t.headers.rows(cx), t.vars.rows(cx), t.pre.read(cx).text(), t.post.read(cx).text());
                if let Some(f) = c.root.find_mut(&t.folder) {
                    f.name = if name.trim().is_empty() { "Untitled".into() } else { name };
                    f.auth = auth;
                    f.headers = headers;
                    f.vars = vars;
                    f.pre_script = pre;
                    f.post_script = post;
                    f.pre_requests = t.pre_requests.clone();
                }
                match api::save_collection(&c) {
                    Ok(()) => {
                        t.coll = c;
                        t.dirty = false;
                        cx.emit(WsRequest::Api(ApiRequest::Changed));
                    }
                    Err(e) => self.status = Some(Err(e)),
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// "Save request": asks for its name and where it goes.
    fn save_menu(&mut self, cx: &mut Context<Self>) {
        let Body_::Req(t) = &self.body else { return };
        let name = t.name.read(cx).text();
        let name = input("Request name", &name, cx);
        let search = input("Search collections and folders…", "", cx);
        let first = places(&self.roots).into_iter().next().map(|p| (p.0, p.1));
        let subs = vec![
            cx.subscribe(&name, |v: &mut ApiView, _, ev: &EditorEvent, cx| match ev {
                EditorEvent::Submit => v.save_dialog_done(cx),
                EditorEvent::Cancel => v.close_save_dialog(cx),
                _ => {}
            }),
            cx.subscribe(&search, |v: &mut ApiView, _, ev: &EditorEvent, cx| match ev {
                EditorEvent::Changed => {
                    if let Some(d) = &mut v.save_dialog {
                        d.list_open = true;
                    }
                    cx.notify();
                }
                // Enter picks the first match.
                EditorEvent::Submit => {
                    let first = v.save_dialog.as_ref().map(|d| d.search.read(cx).text().to_lowercase()).map(|q| places(&v.roots).into_iter().find(|p| p.2.to_lowercase().contains(&q)));
                    if let (Some(Some(p)), Some(d)) = (first, &mut v.save_dialog) {
                        d.choice = Some((p.0, p.1));
                        d.list_open = false;
                    }
                    cx.notify();
                }
                EditorEvent::Cancel => v.close_save_dialog(cx),
                _ => {}
            }),
        ];
        self.save_dialog = Some(SaveDialog { name, search, choice: first, list_open: false, new_name: None, _subs: subs });
        self.menu = None;
        cx.notify();
    }

    fn close_save_dialog(&mut self, cx: &mut Context<Self>) {
        self.save_dialog = None;
        cx.notify();
    }

    fn save_dialog_done(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.save_dialog.take() else { return };
        let name = d.name.read(cx).text();
        let Some((coll, folder)) = d.choice else {
            self.save_dialog = Some(d);
            return;
        };
        if let Body_::Req(t) = &mut self.body {
            if !name.trim().is_empty() {
                t.name.update(cx, |e, cx| e.set_text(name.trim(), cx));
            }
            t.coll = Some(coll);
            t.folder = folder;
        }
        self.save_now(None, cx);
    }

    /// "New collection": a name box in the list, the cursor in it.
    fn new_collection_for_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &mut self.save_dialog else { return };
        let input = input("Collection name", "", cx);
        let subs = vec![cx.subscribe(&input, |v: &mut ApiView, _, ev: &EditorEvent, cx| match ev {
            EditorEvent::Submit => v.create_collection_for_save(cx),
            EditorEvent::Cancel => {
                if let Some(d) = &mut v.save_dialog {
                    d.new_name = None;
                }
                cx.notify();
            }
            _ => {}
        })];
        input.read(cx).focus(window);
        d.new_name = Some((input, subs));
        cx.notify();
    }

    /// The typed name makes the collection, and it's where the request goes.
    fn create_collection_for_save(&mut self, cx: &mut Context<Self>) {
        let project = self.project();
        let Some(d) = &mut self.save_dialog else { return };
        let Some((input, _)) = &d.new_name else { return };
        let name = input.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let c = Collection::new(&name, project);
        api::save_collection(&c).ok();
        d.choice = Some((c.id.clone(), None));
        d.new_name = None;
        d.list_open = false;
        cx.emit(WsRequest::Api(ApiRequest::Changed));
        cx.notify();
    }

    fn render_save_dialog(&self, d: &SaveDialog, cx: &mut Context<Self>) -> AnyElement {
        let all = places(&self.roots);
        let chosen = d.choice.as_ref().and_then(|(c, f)| all.iter().find(|p| &p.0 == c && &p.1 == f)).map(|p| p.2.clone());
        let q = d.search.read(cx).text().to_lowercase();
        let shown: Vec<&Place> = all.iter().filter(|p| q.is_empty() || p.2.to_lowercase().contains(&q)).collect();
        let list = d.list_open.then(|| {
            div()
                .flex()
                .flex_col()
                .mt(px(4.))
                .rounded(px(5.))
                .border_1()
                .border_color(theme::border_input())
                .bg(theme::bg_popup())
                .child(div().p(px(6.)).border_b_1().border_color(theme::border()).child(field(&d.search)))
                .child(
                    div()
                        .id("api-save-places")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .py(px(4.))
                        .when(shown.is_empty(), |el| el.child(div().px(px(12.)).py(px(6.)).text_color(theme::fg_muted()).child("Nothing matches")))
                        .children(shown.iter().enumerate().map(|(i, p)| {
                            let on = d.choice.as_ref().is_some_and(|(c, f)| *c == p.0 && *f == p.1);
                            let pick = (p.0.clone(), p.1.clone());
                            let label = if q.is_empty() { p.2.rsplit(" / ").next().unwrap_or(&p.2).to_string() } else { p.2.clone() };
                            div()
                                .id(("api-save-place", i))
                                .debug_selector(move || format!("api-save-place-{i}"))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(26.))
                                .pl(px(10. + if q.is_empty() { p.3 as f32 * 16. } else { 0. }))
                                .pr(px(10.))
                                .mx(px(4.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .when(on, |el| el.bg(theme::selected_focused()))
                                .hover(|st| st.bg(theme::hover()))
                                .on_click(cx.listener(move |v, _, w, cx| {
                                    if let Some(d) = &mut v.save_dialog {
                                        d.choice = Some(pick.clone());
                                        d.list_open = false;
                                        d.name.read(cx).focus(w);
                                    }
                                    cx.notify();
                                }))
                                .child(ui::small_icon(if p.1.is_none() { "collection" } else { "folder" }, if p.1.is_none() { theme::accent() } else { theme::fg_dim() }))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
                        })),
                )
                .child(match &d.new_name {
                    // Naming it: a box with the cursor in it (Enter makes it, Esc doesn't).
                    Some((input, _)) => div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(5.))
                        .border_t_1()
                        .border_color(theme::border())
                        .child(ui::small_icon("collection", theme::accent()))
                        .child(div().flex_1().min_w_0().child(field(input)))
                        .child(button("api-save-new-coll-ok", "Create", None, true).on_click(cx.listener(|v, _, _, cx| v.create_collection_for_save(cx))))
                        .into_any_element(),
                    None => div()
                        .id("api-save-new-coll")
                        .debug_selector(|| "api-save-new-coll".into())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(12.))
                        .h(px(30.))
                        .border_t_1()
                        .border_color(theme::border())
                        .cursor_pointer()
                        .text_color(theme::accent())
                        .hover(|st| st.bg(theme::hover()))
                        .on_click(cx.listener(|v, _, w, cx| v.new_collection_for_save(w, cx)))
                        .child(ui::small_icon("plus", theme::accent()))
                        .child("New collection")
                        .into_any_element(),
                })
        });
        let card = div()
            .id("api-save-dialog")
            .debug_selector(|| "api-save-dialog".into())
            .occlude()
            .w(px(460.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(18.))
            .rounded(px(8.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(px(15.)).text_color(theme::fg_bright()).font_weight(FontWeight::MEDIUM).child("Save request"))
            .child(div().flex().flex_col().gap(px(5.)).child(div().text_color(theme::fg_dim()).child("Name")).child(field(&d.name).h(px(30.))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(div().text_color(theme::fg_dim()).child("Save in"))
                    .child(
                        div()
                            .id("api-save-in")
                            .debug_selector(|| "api-save-in".into())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(30.))
                            .px(px(8.))
                            .rounded(px(4.))
                            .bg(theme::bg_input())
                            .border_1()
                            .border_color(if d.list_open { theme::focus_border() } else { theme::border_input() })
                            .cursor_pointer()
                            .on_click(cx.listener(|v, _, w, cx| {
                                if let Some(d) = &mut v.save_dialog {
                                    d.list_open = !d.list_open;
                                    if d.list_open {
                                        d.search.read(cx).focus(w);
                                    }
                                }
                                cx.notify();
                            }))
                            .child(ui::small_icon("collection", theme::accent()))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(if chosen.is_some() { theme::fg() } else { theme::fg_muted() }).child(chosen.clone().unwrap_or_else(|| "Choose a collection…".into())))
                            .child(ui::small_icon("chevron-down", theme::fg_dim())),
                    )
                    .children(list),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .pt(px(4.))
                    .child(button("api-save-cancel", "Cancel", None, false).on_click(cx.listener(|v, _, _, cx| v.close_save_dialog(cx))))
                    .child(button("api-save-ok", "Save", Some("check"), true).when(d.choice.is_none(), |el| el.opacity(0.5)).on_click(cx.listener(|v, _, _, cx| v.save_dialog_done(cx)))),
            );
        // Centered over this tab, the rest dimmed (a click outside closes it).
        div()
            .id("api-save-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::ca(0x00000066))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, _, cx| v.close_save_dialog(cx)))
            .child(card)
            .into_any_element()
    }

    fn send_action(&mut self, _: &SendRequest, _: &mut Window, cx: &mut Context<Self>) {
        match &self.body {
            Body_::Runner(_) => self.run_collection(cx),
            _ => self.send(cx),
        }
    }

    fn close_menu(&mut self, _: &CloseMenu, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() || self.save_dialog.take().is_some() {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn open_menu(&mut self, at: Point<Pixels>, items: Vec<(String, Act)>, cx: &mut Context<Self>) {
        self.menu = Some(Menu { at, items });
        cx.notify();
    }

    // ------------------------------------------------------------ the runner

    fn run_collection(&mut self, cx: &mut Context<Self>) {
        let roots = self.roots.clone();
        let (_, env, global) = self.envs();
        let Body_::Runner(t) = &mut self.body else { return };
        if t.running.is_some() {
            t.running = None;
            cx.notify();
            return;
        }
        let Some(coll) = find_coll(&roots, &t.coll.id) else { return };
        t.coll = coll.clone();
        let Some(folder) = coll.root.find(&t.folder) else { return };
        let ids: Vec<String> = folder.all_requests().into_iter().filter(|id| !t.skip.contains(id)).collect();
        let iterations: usize = t.iterations.read(cx).text().trim().parse().unwrap_or(1).clamp(1, 1000);
        let delay: u64 = t.delay.read(cx).text().trim().parse().unwrap_or(0);
        let data_path = t.data.read(cx).text().trim().to_string();
        let data = match read_data(&data_path) {
            Ok(d) => d,
            Err(e) => {
                self.status = Some(Err(e));
                cx.notify();
                return;
            }
        };
        let runs = if data.is_empty() { iterations } else { data.len() };
        t.rows.clear();
        t.started = Some(Instant::now());
        t.took = None;
        let env_id = env.as_ref().map(|e| e.id.clone());
        t.running = Some(cx.spawn(async move |this, cx| {
            for it in 0..runs {
                for id in &ids {
                    let Some(req) = api::load_request(&coll, id) else { continue };
                    // Each request sees what the ones before it kept.
                    let (env, global, coll_now) = {
                        let all = api::environments(&roots);
                        (env_id.as_ref().and_then(|i| all.iter().find(|e| &e.id == i).cloned()), all.iter().find(|e| e.global).cloned().or(global.clone()), find_coll(&roots, &coll.id))
                    };
                    let extra = data.get(it).cloned().unwrap_or_default();
                    let plan = run::Plan { req: req.clone(), coll: coll_now, env, global, extra, skip_pre_requests: false };
                    let done = crate::db::engine::runtime().spawn(run::execute(plan)).await.unwrap_or_default();
                    let changes = done.var_changes.clone();
                    let (roots2, env2, cid) = (roots.clone(), env_id.clone(), coll.id.clone());
                    cx.background_executor()
                        .spawn(async move {
                            let mut c = find_coll(&roots2, &cid);
                            run::keep_changes(&changes, &roots2, env2.as_deref(), c.as_mut())
                        })
                        .await;
                    let row = RunRow {
                        name: if runs > 1 { format!("{} (#{})", req.name, it + 1) } else { req.name.clone() },
                        method: req.method.clone(),
                        status: done.resp.as_ref().map(|r| r.status),
                        ms: done.resp.as_ref().map(|r| r.ms).unwrap_or(0),
                        passed: done.tests.iter().filter(|x| x.pass).count(),
                        failed: done.tests.iter().filter(|x| !x.pass).map(|x| format!("{}: {}", x.name, x.detail)).collect(),
                        error: done.error.clone(),
                    };
                    drop(done);
                    let going = this.update(cx, |v, cx| {
                        let Body_::Runner(t) = &mut v.body else { return false };
                        t.rows.push(row);
                        cx.notify();
                        t.running.is_some()
                    });
                    if !matches!(going, Ok(true)) {
                        return;
                    }
                    if delay > 0 {
                        cx.background_executor().timer(std::time::Duration::from_millis(delay)).await;
                    }
                }
            }
            this.update(cx, |v, cx| {
                if let Body_::Runner(t) = &mut v.body {
                    t.running = None;
                    t.took = t.started.map(|s| s.elapsed().as_millis() as u64);
                }
                cx.emit(WsRequest::Api(ApiRequest::Changed));
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

#[cfg(test)]
impl ApiView {
    pub fn set_url(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Body_::Req(t) = &self.body {
            t.url.update(cx, |e, cx| e.set_text(text, cx));
        }
    }

    pub fn url_text(&self, cx: &App) -> String {
        match &self.body {
            Body_::Req(t) => t.url.read(cx).text(),
            _ => String::new(),
        }
    }

    /// The Params table's rows (key, value), and the path parameters' names.
    pub fn params(&self, cx: &App) -> (Vec<(String, String)>, Vec<String>) {
        match &self.body {
            Body_::Req(t) => (t.params.rows(cx).into_iter().map(|r| (r.key, r.value)).collect(), t.path.rows.iter().map(|r| r.key.read(cx).text()).collect()),
            _ => Default::default(),
        }
    }

    pub fn set_param(&mut self, row: usize, value: &str, cx: &mut Context<Self>) {
        if let Body_::Req(t) = &self.body
            && let Some(r) = t.params.rows.get(row)
        {
            r.value.update(cx, |e, cx| e.set_text(value, cx));
        }
    }

    /// The response's status and the body as shown.
    pub fn response(&self, cx: &App) -> Option<(u16, String, usize)> {
        match &self.body {
            Body_::Req(t) => t.resp.as_ref().map(|r| (r.r.status, t.resp_body.read(cx).text(), r.tests.iter().filter(|x| x.pass).count())),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<String> {
        match &self.body {
            Body_::Req(t) => t.error.clone(),
            _ => None,
        }
    }

    pub fn add_test(&mut self, what: &str, op: &str, value: &str, cx: &mut Context<Self>) {
        let row = self.test_row(&Test { on: true, what: what.into(), path: String::new(), op: op.into(), value: value.into() }, cx);
        if let Body_::Req(t) = &mut self.body {
            t.tests.push(row);
        }
    }

    pub fn save_dialog_open(&self) -> bool {
        self.save_dialog.is_some()
    }

    pub fn save_dialog_search(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(d) = &self.save_dialog {
            d.search.update(cx, |e, cx| e.set_text(text, cx));
        }
    }

    pub fn saved_in(&self) -> Option<(Option<String>, Option<String>)> {
        match &self.body {
            Body_::Req(t) if t.saved => Some((t.coll.clone(), t.folder.clone())),
            _ => None,
        }
    }

    pub fn set_name(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Body_::Req(t) = &self.body {
            t.name.update(cx, |e, cx| e.set_text(name, cx));
        }
    }
}

/// A runner data file: CSV (a header row) or JSON (an array of objects).
fn read_data(path: &str) -> Result<Vec<HashMap<String, String>>, String> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    if path.to_lowercase().ends_with(".json") || text.trim_start().starts_with('[') {
        let v: Vec<serde_json::Map<String, serde_json::Value>> = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        return Ok(v.into_iter().map(|o| o.into_iter().map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect()).collect());
    }
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let head: Vec<String> = lines.next().map(csv_fields).unwrap_or_default();
    Ok(lines.map(|l| head.iter().cloned().zip(csv_fields(l)).collect()).collect())
}

fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------- drawing

fn button(id: impl Into<SharedString>, label: &str, icon: Option<&str>, primary: bool) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(28.))
        .px(px(12.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(px(12.5))
        .when(primary, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).font_weight(FontWeight::MEDIUM).hover(|s| s.bg(theme::accent_hover())))
        .when(!primary, |d| d.bg(theme::bg_input()).border_1().border_color(theme::border_input()).text_color(theme::fg()).hover(|s| s.bg(theme::hover())))
        .when_some(icon, |d, i| d.child(ui::small_icon(i, if primary { theme::on(theme::accent()) } else { theme::fg() })))
        .when(!label.is_empty(), |d| d.child(label.to_string()))
}

fn field(e: &Entity<Editor>) -> gpui::Div {
    div().h(px(26.)).px(px(6.)).flex().items_center().min_w_0().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).child(div().flex_1().min_w_0().child(e.clone()))
}

fn code_box(e: &Entity<Editor>) -> gpui::Div {
    div().flex_1().min_h(px(120.)).rounded(px(4.)).border_1().border_color(theme::border_input()).overflow_hidden().child(e.clone())
}

fn switch(id: impl Into<SharedString>, on: bool) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .w(px(30.))
        .h(px(16.))
        .px(px(2.))
        .rounded(px(8.))
        .cursor_pointer()
        .bg(if on { theme::accent() } else { theme::badge() })
        .when(on, |d| d.justify_end())
        .child(div().size(px(12.)).rounded(px(6.)).bg(theme::c(0xffffff)))
}

fn check(id: impl Into<SharedString>, on: bool) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(15.))
        .rounded(px(3.))
        .cursor_pointer()
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border_input() })
        .when(on, |d| d.bg(theme::accent()).child(ui::small_icon("check", theme::on(theme::accent())).size(px(11.))))
}

fn icon_btn(id: impl Into<SharedString>, icon: &str, tip: &'static str) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .tooltip(ui::tooltip(tip))
        .child(ui::small_icon(icon, theme::fg_dim()))
}

fn dropdown(id: impl Into<SharedString>, label: impl Into<SharedString>, color: Option<gpui::Hsla>) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .px(px(8.))
        .rounded(px(4.))
        .bg(theme::bg_input())
        .border_1()
        .border_color(theme::border_input())
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .child(div().text_color(color.unwrap_or(theme::fg())).when(color.is_some(), |d| d.font_weight(FontWeight::BOLD)).child(label.into()))
        .child(ui::small_icon("chevron-down", theme::fg_dim()))
}

fn sub_tab(id: impl Into<SharedString>, label: &str, count: Option<String>, active: bool) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(30.))
        .px(px(8.))
        .cursor_pointer()
        .border_b_2()
        .border_color(if active { theme::accent() } else { gpui::transparent_black() })
        .text_color(if active { theme::fg_bright() } else { theme::fg_dim() })
        .hover(|s| s.text_color(theme::fg_bright()))
        .child(label.to_string())
        .when_some(count, |d, c| d.child(div().text_size(px(10.5)).text_color(theme::accent()).child(c)))
}

fn method_color(m: &str) -> gpui::Hsla {
    theme::c(api::method_color(m))
}

fn status_color(s: u16) -> gpui::Hsla {
    match s {
        200..=299 => theme::git_added(),
        300..=399 => theme::c(0x388bfd),
        400..=499 => theme::c(0xd29922),
        _ => theme::git_deleted(),
    }
}

pub fn human_size(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.),
    }
}

fn label(text: &str) -> gpui::Div {
    div().w(px(150.)).flex_none().text_color(theme::fg_dim()).child(text.to_string())
}

/// Menu items from labels; picking one calls `set` with it.
fn pick(labels: &[&str], set: impl Fn(&mut ApiView, &str, &mut Context<ApiView>) + 'static) -> Vec<(String, Act)> {
    let set = Rc::new(set);
    labels
        .iter()
        .map(|l| {
            let (set, l2) = (set.clone(), l.to_string());
            (l.to_string(), Rc::new(move |v: &mut ApiView, _: &mut Window, cx: &mut Context<ApiView>| set(v, &l2, cx)) as Act)
        })
        .collect()
}

/// Opens `items` at the pointer when pressed.
fn opens(el: gpui::Stateful<gpui::Div>, items: Vec<(String, Act)>, cx: &mut Context<ApiView>) -> gpui::Stateful<gpui::Div> {
    el.on_mouse_down(
        MouseButton::Left,
        cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            v.open_menu(ev.position, items.clone(), cx);
        }),
    )
}

#[derive(Clone, Copy)]
enum AuthOwner {
    Req,
    Folder,
}

impl ApiView {
    fn auth_mut(&mut self, o: AuthOwner) -> Option<&mut AuthEd> {
        match (&mut self.body, o) {
            (Body_::Req(t), AuthOwner::Req) => Some(&mut t.auth),
            (Body_::Folder(t), AuthOwner::Folder) => Some(&mut t.auth),
            _ => None,
        }
    }

    fn render_kv(&self, kv: &Kv, cx: &mut Context<Self>) -> AnyElement {
        let id = kv.id;
        let n = kv.rows.len();
        let fixed_keys = id == KvId::Path;
        let head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(24.))
            .text_size(px(11.))
            .text_color(theme::fg_muted())
            .child(div().w(px(15.)))
            .child(div().flex_1().child(match id {
                KvId::Vars | KvId::FolderVars => "VARIABLE",
                KvId::Headers | KvId::FolderHeaders => "HEADER",
                KvId::Path => "PATH PARAMETER",
                _ => "KEY",
            }))
            .child(div().flex_1().child("VALUE"))
            .when(id != KvId::Path, |d| d.child(div().flex_1().child("DESCRIPTION")))
            .child(div().w(px(if matches!(id, KvId::Multipart | KvId::Vars) { 74. } else { 22. })));
        let rows = kv.rows.iter().enumerate().map(|(i, r)| {
            let last = i + 1 == n && !fixed_keys;
            let blank = last && Kv::is_blank(r, cx);
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .py(px(2.))
                .child(if blank {
                    div().w(px(15.)).into_any_element()
                } else {
                    check(format!("kv-{id:?}-on-{i}"), r.on)
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(kv) = v.kv_mut(id)
                                && let Some(r) = kv.rows.get_mut(i)
                            {
                                r.on = !r.on;
                            }
                            v.kv_changed(id, cx);
                        }))
                        .into_any_element()
                })
                .child(div().flex_1().min_w_0().child(if fixed_keys { div().h(px(26.)).flex().items_center().px(px(6.)).text_color(theme::fg()).child(r.key.read(cx).text()) } else { field(&r.key) }))
                .child(div().flex_1().min_w_0().child(field(&r.value)))
                .when(id != KvId::Path, |d| d.child(div().flex_1().min_w_0().child(field(&r.desc))))
                .when(id == KvId::Multipart, |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("kv-file-{i}")))
                            .w(px(46.))
                            .flex()
                            .justify_center()
                            .text_size(px(11.))
                            .py(px(3.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme::border_input())
                            .text_color(if r.flag { theme::accent() } else { theme::fg_dim() })
                            .hover(|s| s.bg(theme::hover()))
                            .tooltip(ui::tooltip("Send the value as text, or the file at that path"))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                if let Some(kv) = v.kv_mut(id)
                                    && let Some(r) = kv.rows.get_mut(i)
                                {
                                    r.flag = !r.flag;
                                }
                                v.touch(cx);
                            }))
                            .child(if r.flag { "File" } else { "Text" }),
                    )
                })
                .when(id == KvId::Vars, |d| {
                    let secret = r.flag;
                    d.child(
                        icon_btn(format!("kv-secret-{i}"), "lock", "Secret: kept in the Keychain, hidden here").when(secret, |d| d.bg(theme::selected_focused())).on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(kv) = v.kv_mut(id)
                                && let Some(r) = kv.rows.get_mut(i)
                            {
                                r.flag = !r.flag;
                                let s = r.flag;
                                r.value.update(cx, |e, cx| {
                                    e.secret = s;
                                    cx.notify();
                                });
                            }
                            v.touch(cx);
                        })),
                    )
                })
                .child(if blank || fixed_keys {
                    div().w(px(22.)).into_any_element()
                } else {
                    icon_btn(format!("kv-{id:?}-del-{i}"), "trash", "Remove")
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(kv) = v.kv_mut(id)
                                && i < kv.rows.len()
                            {
                                kv.rows.remove(i);
                            }
                            v.kv_changed(id, cx);
                        }))
                        .into_any_element()
                })
        });
        div().flex().flex_col().child(head).children(rows).into_any_element()
    }

    fn render_auth(&self, a: &AuthEd, owner: AuthOwner, cx: &mut Context<Self>) -> AnyElement {
        let kinds: Vec<&str> = Auth::KINDS.iter().copied().filter(|k| a.allow_inherit || *k != "Inherit").collect();
        let kind_menu = pick(&kinds, move |v, k, cx| {
            if let Some(a) = v.auth_mut(owner) {
                a.kind = Auth::KINDS.iter().copied().find(|x| *x == k).unwrap_or("None");
            }
            v.touch(cx);
        });
        let row = |name: &str, key: &'static str| div().flex().items_center().gap(px(10.)).child(label(name)).child(div().flex_1().max_w(px(520.)).child(field(&a.f[key])));
        let toggle = |id: &'static str, text: &'static str, on: bool, set: fn(&mut AuthEd), cx: &mut Context<Self>| {
            div().flex().items_center().gap(px(10.)).child(label(text)).child(switch(id, on).on_click(cx.listener(move |v, _, _, cx| {
                if let Some(a) = v.auth_mut(owner) {
                    set(a);
                }
                v.touch(cx);
            })))
        };
        let mut body = div().flex().flex_col().gap(px(8.));
        match a.kind {
            "Inherit" => {
                body = body.child(div().text_color(theme::fg_dim()).child(match owner {
                    AuthOwner::Req => "Uses the auth of the folder (or collection) it's in.",
                    AuthOwner::Folder => "Uses the auth of the folder (or collection) above.",
                }))
            }
            "None" => body = body.child(div().text_color(theme::fg_dim()).child("No auth.")),
            "Basic" | "Digest" => body = body.child(row("Username", "user")).child(row("Password", "pass")),
            "Bearer" => body = body.child(row("Token", "token")).child(row("Prefix", "prefix")),
            "API Key" => {
                body = body.child(row("Key", "key")).child(row("Value", "value")).child(toggle("auth-apikey-query", "Send in the query", a.query, |a| a.query = !a.query, cx))
            }
            "AWS Signature" => {
                body = body.child(row("Access key", "access_key")).child(row("Secret key", "secret_key")).child(row("Region", "region")).child(row("Service", "service")).child(row("Session token", "session_token"))
            }
            "OAuth 2.0" => {
                let grants = ["client_credentials", "authorization_code", "password", "refresh_token"];
                let grant_menu = pick(&grants, move |v, g, cx| {
                    if let Some(a) = v.auth_mut(owner) {
                        a.grant = g.to_string();
                    }
                    v.touch(cx);
                });
                let g = a.grant.clone();
                body = body.child(div().flex().items_center().gap(px(10.)).child(label("Grant type")).child(opens(dropdown("auth-grant", g.replace('_', " "), None), grant_menu, cx)));
                if g == "authorization_code" {
                    body = body.child(row("Authorization URL", "auth_url")).child(div().flex().items_center().gap(px(10.)).child(label("Redirect URL")).child(div().flex_1().max_w(px(520.)).child(field(&a.redirect))));
                }
                body = body.child(row("Token URL", "token_url")).child(row("Client ID", "client_id")).child(row("Client secret", "client_secret"));
                if g == "password" {
                    body = body.child(row("Username", "username")).child(row("Password", "password"));
                }
                body = body.child(row("Scope", "scope")).child(row("Audience", "audience")).child(row("Header prefix", "prefix"));
                if g == "authorization_code" {
                    body = body.child(toggle("auth-pkce", "PKCE", a.pkce, |a| a.pkce = !a.pkce, cx));
                }
                body = body.child(toggle("auth-basic", "Client auth in a header", a.basic, |a| a.basic = !a.basic, cx));
                let status = a.token.clone();
                body = body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(160.))
                        .child(button("auth-get-token", if a.task.is_some() { "Getting token…" } else { "Get new token" }, Some("key"), true).on_click(cx.listener(move |v, _, _, cx| v.get_token(owner, cx))))
                        .child(button("auth-forget-token", "Forget token", None, false).on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(a) = v.auth_mut(owner)
                                && let Auth::Oauth2(o) = a.gather(cx)
                            {
                                send::forget_token(&o);
                                a.token = Some(Ok("Forgotten: the next request gets a new one.".into()));
                            }
                            cx.notify();
                        }))),
                );
                if let Some(s) = status {
                    let (ok, text) = match s {
                        Ok(t) => (true, t),
                        Err(e) => (false, e),
                    };
                    body = body.child(div().pl(px(160.)).text_size(px(12.)).text_color(if ok { theme::git_added() } else { theme::git_deleted() }).child(text));
                }
                body = body.child(div().pl(px(160.)).text_size(px(11.)).text_color(theme::fg_muted()).child("Tokens are kept in your Keychain and refreshed when they expire."));
            }
            _ => {}
        }
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().flex().items_center().gap(px(10.)).child(label("Type")).child(opens(dropdown("auth-kind", a.kind, None), kind_menu, cx)))
            .child(body)
            .into_any_element()
    }

    fn get_token(&mut self, owner: AuthOwner, cx: &mut Context<Self>) {
        let (_, env, global) = self.envs();
        let Some(a) = self.auth_mut(owner) else { return };
        let Auth::Oauth2(o) = a.gather(cx) else { return };
        let env = env.map(|e| api::with_secrets(&e));
        let global = global.map(|g| api::with_secrets(&g));
        let vars = api::vars::Vars::new(&[], env.as_ref(), global.as_ref());
        let fake = api::Request { auth: Auth::Oauth2(o), ..Default::default() };
        let ready = send::prepare(&fake, &[], &vars);
        let Auth::Oauth2(o) = ready.auth else { return };
        let work = crate::db::engine::run(send::fetch_token(o, ready.settings));
        a.token = None;
        a.task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(a) = v.auth_mut(owner) {
                    a.task = None;
                    a.token = Some(r.map(|t| {
                        let tail: String = t.access.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
                        format!("Got a token (…{tail}){}", if t.expires > 0 { format!(", valid for {} min", t.expires.saturating_sub(api::now_secs()) / 60) } else { String::new() })
                    }));
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_request(&self, t: &ReqTab, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let kind = t.req.kind;
        let method = match kind {
            Kind::Http => t.req.method.clone(),
            Kind::Websocket => "WS".into(),
            Kind::Sse => "SSE".into(),
        };
        let mut methods: Vec<&str> = api::METHODS.to_vec();
        methods.extend(["WS", "SSE"]);
        let method_menu = pick(&methods, |v, m, cx| {
            if let Body_::Req(t) = &mut v.body {
                match m {
                    "WS" => t.req.kind = Kind::Websocket,
                    "SSE" => {
                        t.req.kind = Kind::Sse;
                        t.req.method = "GET".into();
                    }
                    m => {
                        t.req.kind = Kind::Http;
                        t.req.method = m.to_string();
                    }
                }
                t.live = None;
            }
            v.touch(cx);
        });
        let (envs, active, _) = self.envs();
        let mut env_items: Vec<(String, Act)> = vec![(
            "No environment".into(),
            Rc::new(|v: &mut ApiView, _: &mut Window, cx: &mut Context<ApiView>| {
                api::set_active_env(v.project().as_deref(), None);
                cx.emit(WsRequest::Api(ApiRequest::Changed));
                cx.notify();
            }) as Act,
        )];
        for e in envs.iter().filter(|e| !e.global) {
            let id = e.id.clone();
            env_items.push((
                e.name.clone(),
                Rc::new(move |v: &mut ApiView, _: &mut Window, cx: &mut Context<ApiView>| {
                    api::set_active_env(v.project().as_deref(), Some(&id));
                    cx.emit(WsRequest::Api(ApiRequest::Changed));
                    cx.notify();
                }),
            ));
        }
        let live_on = t.live.as_ref().is_some_and(|l| l.connected);
        let (send_label, send_icon) = match (kind, t.sending.is_some(), live_on) {
            (_, true, _) => ("Cancel", "stop"),
            (Kind::Http, false, _) => ("Send", "play"),
            (_, false, true) => ("Disconnect", "stop"),
            (_, false, false) => ("Connect", "play"),
        };
        let where_ = t.coll.as_ref().and_then(|id| api::collections(&self.roots).into_iter().find(|c| c.id == *id)).map(|c| {
            let mut path: Vec<String> = c.root.chain_to(&t.req.id).iter().map(|f| f.name.clone()).collect();
            if path.is_empty() {
                path.push(c.name().to_string());
            }
            path.join(" / ")
        });
        let top = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().w(px(260.)).child(field(&t.name)))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.5)).text_color(theme::fg_muted()).child(match &where_ {
                Some(w) => format!("{w}{}", if t.dirty && t.saved { "  •  unsaved changes" } else { "" }),
                None => "Not saved yet".into(),
            }))
            .child(opens(dropdown("api-env", active.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| "No environment".into()), None), env_items, cx))
            .child(icon_btn("api-layout", if t.stacked { "split" } else { "inline" }, "Response beside / below").on_click(cx.listener(|v, _, _, cx| {
                if let Body_::Req(t) = &mut v.body {
                    t.stacked = !t.stacked;
                }
                cx.notify();
            })));
        let url_bar = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(opens(dropdown("api-method", method.clone(), Some(method_color(&method))).w(px(96.)).justify_between(), method_menu, cx))
            .child(div().flex_1().min_w_0().child(field(&t.url).h(px(30.))))
            .child(button("api-send", send_label, Some(send_icon), true).on_click(cx.listener(move |v, _, _, cx| {
                let busy = matches!(&v.body, Body_::Req(t) if t.sending.is_some() || t.live.as_ref().is_some_and(|l| l.connected));
                if busy { v.cancel(cx) } else { v.send(cx) }
            })))
            .child(
                button("api-save", "Save", Some("check"), false)
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, ev: &MouseDownEvent, _, _| v.menu_at = Some(ev.position)))
                    .on_click(cx.listener(|v, _, _, cx| v.save_now(None, cx))),
            );
        let count = |n: usize| (n > 0).then(|| n.to_string());
        let sections = [
            (Section::Params, "Params", count(t.params.rows(cx).iter().filter(|r| r.on).count() + t.path.rows.len())),
            (Section::Headers, "Headers", count(t.headers.rows(cx).iter().filter(|r| r.on).count())),
            (Section::Auth, "Auth", (t.auth.kind != "Inherit" && t.auth.kind != "None").then(|| "•".to_string())),
            (Section::Body, "Body", (t.req.body != Body::None).then(|| "•".to_string())),
            (Section::Tests, "Tests", count(t.tests.len())),
            (Section::Vars, "Set Vars", count(t.vars.len())),
            (Section::Scripts, "Scripts", (!t.pre.read(cx).text().trim().is_empty() || !t.post.read(cx).text().trim().is_empty()).then(|| "•".to_string())),
            (Section::Settings, "Settings", None),
            (Section::Code, "Code", None),
            (Section::Docs, "Docs", None),
        ];
        let tabs = div().id("api-sections").flex().flex_none().items_center().gap(px(2.)).border_b_1().border_color(theme::border()).overflow_x_scroll().children(sections.into_iter().map(|(s, name, c)| {
            sub_tab(format!("api-sec-{name}"), name, c, t.section == s).on_click(cx.listener(move |v, _, _, cx| {
                if let Body_::Req(t) = &mut v.body {
                    t.section = s;
                }
                if s == Section::Code {
                    v.refresh_code(cx);
                }
                cx.notify();
            }))
        }));
        let section = self.render_section(t, cx);
        let req_pane = div().flex().flex_col().flex_1().min_w_0().min_h_0().child(tabs).child(div().id("api-sec-body").flex_1().min_h_0().overflow_y_scroll().pt(px(10.)).child(section));
        let resp_pane = self.render_response(t, window, cx);
        let missing = t.resp.as_ref().map(|r| r.missing.clone()).unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .size_full()
            .px(px(16.))
            .pt(px(12.))
            .pb(px(8.))
            .gap(px(10.))
            .child(top)
            .child(url_bar)
            .when(!missing.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(theme::c(0xd29922))
                        .child(ui::small_icon("warning", theme::c(0xd29922)))
                        .child(format!("Not set: {} (sent as written). Set them in an environment, the collection, or a script.", missing.iter().map(|m| format!("{{{{{m}}}}}")).collect::<Vec<_>>().join(", "))),
                )
            })
            .when_some(self.status.clone(), |d, s| {
                let (ok, text) = match s {
                    Ok(t) => (true, t),
                    Err(e) => (false, e),
                };
                d.child(div().text_size(px(12.)).text_color(if ok { theme::fg_dim() } else { theme::git_deleted() }).child(text))
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(14.))
                    .when(t.stacked, |d| d.flex_col())
                    .child(req_pane)
                    .child(if t.stacked { div().h(px(1.)).bg(theme::border()) } else { div().w(px(1.)).bg(theme::border()) })
                    .child(resp_pane),
            )
            .into_any_element()
    }

    fn refresh_code(&mut self, cx: &mut Context<Self>) {
        let (_, env, global) = self.envs();
        let roots = self.roots.clone();
        let Body_::Req(t) = &mut self.body else { return };
        let req = Self::gather(t, cx);
        // Secret values stay {{variables}} in code meant to be copied elsewhere.
        let coll = t.coll.as_ref().and_then(|id| find_coll(&roots, id));
        let folders = coll.as_ref().map(|c| run::chain(c, &req.id)).unwrap_or_default();
        let inner: Vec<&api::Folder> = folders.iter().rev().copied().collect();
        let rows: Vec<&[Row]> = inner.iter().map(|f| f.vars.as_slice()).collect();
        let vars = api::vars::Vars::new(&rows, env.as_ref(), global.as_ref());
        let ready = send::prepare(&req, &inner, &vars);
        let (lang, ext) = codegen::LANGUAGES[t.code_lang];
        let code = codegen::generate(lang, &ready);
        let hl = match ext {
            "js" => "js",
            "py" => "py",
            "go" => "go",
            "rs" => "rs",
            "sh" => "sh",
            _ => "",
        };
        t.code.update(cx, |e, cx| {
            e.set_language(hl, cx);
            e.set_text(&code, cx);
        });
    }

    fn render_section(&self, t: &ReqTab, cx: &mut Context<Self>) -> AnyElement {
        let hint = |text: &str| div().text_size(px(11.5)).text_color(theme::fg_muted()).child(text.to_string());
        match t.section {
            Section::Params => div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(div().flex().flex_col().gap(px(4.)).child(div().text_color(theme::fg_dim()).child("Query parameters")).child(self.render_kv(&t.params, cx)))
                .when(!t.path.rows.is_empty(), |d| d.child(div().flex().flex_col().gap(px(4.)).child(div().text_color(theme::fg_dim()).child("Path parameters")).child(self.render_kv(&t.path, cx))))
                .child(hint("Use {{variable}} anywhere. :id or {id} in the URL's path adds a path parameter."))
                .into_any_element(),
            Section::Headers => div().flex().flex_col().gap(px(8.)).child(self.render_kv(&t.headers, cx)).child(hint("Folders' and the collection's headers are sent too (these win).")).into_any_element(),
            Section::Auth => self.render_auth(&t.auth, AuthOwner::Req, cx),
            Section::Body => {
                let current = t.req.body.kind();
                let kinds = div().flex().flex_wrap().gap(px(4.)).children(Body::KINDS.iter().map(|k| {
                    let on = *k == current;
                    let k2 = *k;
                    div()
                        .id(SharedString::from(format!("api-body-{k}")))
                        .debug_selector(move || format!("api-body-{k2}"))
                        .px(px(9.))
                        .py(px(3.))
                        .rounded(px(4.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(theme::selected_focused()).text_color(theme::fg_bright()))
                        .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.bg(theme::hover())))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Body_::Req(t) = &mut v.body {
                                t.req.body = t.req.body.switch(k2);
                                let lang = if matches!(t.req.body, Body::Graphql { .. }) { "" } else { body_lang(&t.req.body) };
                                t.body_text.update(cx, |e, cx| e.set_language(lang, cx));
                                if k2 == "JSON" && !t.headers.rows(cx).iter().any(|h| h.key.eq_ignore_ascii_case("content-type")) {
                                    // The type is sent by itself.
                                }
                            }
                            v.touch(cx);
                        }))
                        .child(*k)
                }));
                let content: AnyElement = match &t.req.body {
                    Body::None => hint("This request has no body.").into_any_element(),
                    Body::Json { .. } | Body::Xml { .. } | Body::Text { .. } | Body::Html { .. } => div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .h(px(420.))
                        .child(code_box(&t.body_text))
                        .when(matches!(t.req.body, Body::Json { .. }), |d| {
                            d.child(div().flex().gap(px(6.)).child(button("api-format", "Format", Some("braces"), false).on_click(cx.listener(|v, _, _, cx| {
                                if let Body_::Req(t) = &mut v.body {
                                    let text = t.body_text.read(cx).text();
                                    match serde_json::from_str::<serde_json::Value>(&text) {
                                        Ok(j) => {
                                            let p = serde_json::to_string_pretty(&j).unwrap_or(text);
                                            t.body_text.update(cx, |e, cx| e.set_text(&p, cx));
                                            v.status = None;
                                        }
                                        Err(e) => v.status = Some(Err(format!("Not valid JSON: {e}"))),
                                    }
                                }
                                cx.notify();
                            }))))
                        })
                        .into_any_element(),
                    Body::Form { .. } => self.render_kv(&t.form, cx),
                    Body::Multipart { .. } => div().flex().flex_col().gap(px(6.)).child(self.render_kv(&t.multipart, cx)).child(hint("“File” sends the file at the path typed as the value.")).into_any_element(),
                    Body::Binary { .. } => div().flex().items_center().gap(px(8.)).child(div().flex_1().child(field(&t.binary))).child(button("api-binary-pick", "Choose…", Some("folder"), false).on_click(cx.listener(|v, _, _, cx| v.pick_file(cx)))).into_any_element(),
                    Body::Graphql { .. } => {
                        let schema = match &t.schema {
                            None if t.schema_task.is_some() => Some(hint("Asking the server for its schema…").into_any_element()),
                            None => None,
                            Some(Err(e)) => Some(div().text_size(px(12.)).text_color(theme::git_deleted()).child(e.clone()).into_any_element()),
                            Some(Ok(ed)) => Some(div().flex().flex_col().gap(px(4.)).h(px(300.)).child(div().text_color(theme::fg_dim()).child("Schema")).child(code_box(ed)).into_any_element()),
                        };
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().flex().items_center().child(div().flex_1().text_color(theme::fg_dim()).child("Query")).child(button("api-schema", "Load schema", Some("refresh"), false).on_click(cx.listener(|v, _, _, cx| v.fetch_schema(cx)))))
                            .child(div().h(px(260.)).flex().child(code_box(&t.body_text)))
                            .child(div().text_color(theme::fg_dim()).child("Variables (JSON)"))
                            .child(div().h(px(110.)).flex().child(code_box(&t.gql_vars)))
                            .children(schema)
                            .into_any_element()
                    }
                };
                div().flex().flex_col().gap(px(10.)).child(kinds).child(content).into_any_element()
            }
            Section::Tests => {
                let rows = t.tests.iter().enumerate().map(|(i, r)| {
                    let what_menu = pick(&tests::WHATS, move |v, w, cx| {
                        if let Body_::Req(t) = &mut v.body
                            && let Some(r) = t.tests.get_mut(i)
                        {
                            r.what = w.to_string();
                        }
                        v.touch(cx);
                    });
                    let op_menu = pick(&tests::OPS, move |v, o, cx| {
                        if let Body_::Req(t) = &mut v.body
                            && let Some(r) = t.tests.get_mut(i)
                        {
                            r.op = o.to_string();
                        }
                        v.touch(cx);
                    });
                    let needs_path = matches!(r.what.as_str(), "header" | "json");
                    let schema = r.what == "schema";
                    let no_value = matches!(r.op.as_str(), "exists" | "not-exists") && !schema;
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(check(format!("api-test-on-{i}"), r.on).on_click(cx.listener(move |v, _, _, cx| {
                            if let Body_::Req(t) = &mut v.body
                                && let Some(r) = t.tests.get_mut(i)
                            {
                                r.on = !r.on;
                            }
                            v.touch(cx);
                        })))
                        .child(opens(dropdown(format!("api-test-what-{i}"), r.what.clone(), None).w(px(118.)).justify_between(), what_menu, cx))
                        .when(needs_path, |d| d.child(div().flex_1().min_w_0().child(field(&r.path))))
                        .when(!schema, |d| d.child(opens(dropdown(format!("api-test-op-{i}"), r.op.clone(), None).w(px(118.)).justify_between(), op_menu, cx)))
                        .when(!no_value, |d| d.child(div().flex_1().min_w_0().child(field(&r.value))))
                        .child(icon_btn(format!("api-test-del-{i}"), "trash", "Remove").on_click(cx.listener(move |v, _, _, cx| {
                            if let Body_::Req(t) = &mut v.body
                                && i < t.tests.len()
                            {
                                t.tests.remove(i);
                            }
                            v.touch(cx);
                        })))
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children(rows)
                    .child(div().flex().child(button("api-test-add", "Add check", Some("plus"), false).on_click(cx.listener(|v, _, _, cx| {
                        let row = v.test_row(&Test::default(), cx);
                        if let Body_::Req(t) = &mut v.body {
                            t.tests.push(row);
                        }
                        v.touch(cx);
                    }))))
                    .child(hint("Checks run on every response, no code needed. json takes a JSONPath ($.items[0].id); schema takes a JSON Schema as its value. For more, write tc.test(…) in Scripts."))
                    .into_any_element()
            }
            Section::Vars => {
                let rows = t.vars.iter().enumerate().map(|(i, r)| {
                    let from_menu = pick(&["json", "header", "cookie", "status", "body"], move |v, f, cx| {
                        if let Body_::Req(t) = &mut v.body
                            && let Some(r) = t.vars.get_mut(i)
                        {
                            r.from = f.to_string();
                        }
                        v.touch(cx);
                    });
                    let scope_menu = pick(&["environment", "global", "collection"], move |v, s, cx| {
                        if let Body_::Req(t) = &mut v.body
                            && let Some(r) = t.vars.get_mut(i)
                        {
                            r.scope = s.to_string();
                        }
                        v.touch(cx);
                    });
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(check(format!("api-var-on-{i}"), r.on).on_click(cx.listener(move |v, _, _, cx| {
                            if let Body_::Req(t) = &mut v.body
                                && let Some(r) = t.vars.get_mut(i)
                            {
                                r.on = !r.on;
                            }
                            v.touch(cx);
                        })))
                        .child(div().w(px(170.)).child(field(&r.var)))
                        .child(div().text_color(theme::fg_muted()).child("from"))
                        .child(opens(dropdown(format!("api-var-from-{i}"), r.from.clone(), None).w(px(90.)).justify_between(), from_menu, cx))
                        .when(matches!(r.from.as_str(), "json" | "header" | "cookie"), |d| d.child(div().flex_1().min_w_0().child(field(&r.path))))
                        .when(!matches!(r.from.as_str(), "json" | "header" | "cookie"), |d| d.child(div().flex_1()))
                        .child(div().text_color(theme::fg_muted()).child("into"))
                        .child(opens(dropdown(format!("api-var-scope-{i}"), r.scope.clone(), None).w(px(120.)).justify_between(), scope_menu, cx))
                        .child(icon_btn(format!("api-var-del-{i}"), "trash", "Remove").on_click(cx.listener(move |v, _, _, cx| {
                            if let Body_::Req(t) = &mut v.body
                                && i < t.vars.len()
                            {
                                t.vars.remove(i);
                            }
                            v.touch(cx);
                        })))
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children(rows)
                    .child(div().flex().child(button("api-var-add", "Add variable", Some("plus"), false).on_click(cx.listener(|v, _, _, cx| {
                        let row = v.var_row(&SetVar::default(), cx);
                        if let Body_::Req(t) = &mut v.body {
                            t.vars.push(row);
                        }
                        v.touch(cx);
                    }))))
                    .child(hint("Takes values from each response (a token, an id) for the requests after it: chain them with {{name}}."))
                    .into_any_element()
            }
            Section::Scripts => div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_color(theme::fg_dim()).child("Pre-request (JavaScript)"))
                .child(div().h(px(200.)).flex().child(code_box(&t.pre)))
                .child(div().text_color(theme::fg_dim()).child("Post-response (JavaScript)"))
                .child(div().h(px(220.)).flex().child(code_box(&t.post)))
                .child(hint("tc.request (url, method, headers, setHeader), tc.response (status, headers, json(), text(), time), tc.getVar / setVar / setEnvVar / setGlobalVar, tc.test(name, fn) with expect(x).to.equal(y), console.log. pm.* works too. 5 s and 32 MB at most."))
                .into_any_element(),
            Section::Settings => {
                let s = &t.req.settings;
                let toggle = |id: &'static str, text: &'static str, on: bool, set: fn(&mut api::Settings), cx: &mut Context<Self>| {
                    div().flex().items_center().gap(px(10.)).child(label(text)).child(switch(id, on).on_click(cx.listener(move |v, _, _, cx| {
                        if let Body_::Req(t) = &mut v.body {
                            set(&mut t.req.settings);
                        }
                        v.touch(cx);
                    })))
                };
                let row = |name: &str, e: &Entity<Editor>, w: f32| div().flex().items_center().gap(px(10.)).child(label(name)).child(div().w(px(w)).child(field(e)));
                let http_menu = pick(&["auto", "1.1", "2"], |v, h, cx| {
                    if let Body_::Req(t) = &mut v.body {
                        t.req.settings.http = h.to_string();
                    }
                    v.touch(cx);
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(row("Timeout (ms)", &t.s_timeout, 120.))
                    .child(toggle("api-set-redirects", "Follow redirects", s.follow_redirects, |s| s.follow_redirects = !s.follow_redirects, cx))
                    .when(s.follow_redirects, |d| d.child(row("Most redirects", &t.s_max, 120.)))
                    .child(toggle("api-set-tls", "Check certificates", s.verify_tls, |s| s.verify_tls = !s.verify_tls, cx))
                    .child(toggle("api-set-encode", "Encode the URL", s.encode_url, |s| s.encode_url = !s.encode_url, cx))
                    .child(div().flex().items_center().gap(px(10.)).child(label("HTTP version")).child(opens(dropdown("api-set-http", s.http.clone(), None), http_menu, cx)))
                    .child(row("Proxy", &t.s_proxy, 420.))
                    .child(row("No proxy for", &t.s_noproxy, 420.))
                    .child(row("Client certificate", &t.s_cert, 420.))
                    .child(row("Its password", &t.s_cert_pass, 240.))
                    .child(row("Extra CA (PEM)", &t.s_ca, 420.))
                    .into_any_element()
            }
            Section::Code => {
                let langs: Vec<&str> = codegen::LANGUAGES.iter().map(|(l, _)| *l).collect();
                let menu = pick(&langs, |v, l, cx| {
                    if let Body_::Req(t) = &mut v.body {
                        t.code_lang = codegen::LANGUAGES.iter().position(|(x, _)| *x == l).unwrap_or(0);
                    }
                    v.refresh_code(cx);
                    cx.notify();
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(opens(dropdown("api-code-lang", codegen::LANGUAGES[t.code_lang].0, None), menu, cx))
                            .child(button("api-code-refresh", "Refresh", Some("refresh"), false).on_click(cx.listener(|v, _, _, cx| {
                                v.refresh_code(cx);
                                cx.notify();
                            })))
                            .child(button("api-code-copy", "Copy", Some("copy"), false).on_click(cx.listener(|v, _, _, cx| {
                                if let Body_::Req(t) = &v.body {
                                    cx.write_to_clipboard(ClipboardItem::new_string(t.code.read(cx).text()));
                                    v.status = Some(Ok("Copied.".into()));
                                }
                                cx.notify();
                            }))),
                    )
                    .child(div().h(px(420.)).flex().child(code_box(&t.code)))
                    .child(hint("Secret variables stay as {{name}}."))
                    .into_any_element()
            }
            Section::Docs => div().flex().flex_col().gap(px(6.)).child(div().h(px(420.)).flex().child(code_box(&t.docs))).child(hint("Notes about this request (Markdown).")).into_any_element(),
        }
    }

    fn pick_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|p| p.into_iter().next()) else { return };
            this.update(cx, |v, cx| {
                if let Body_::Req(t) = &mut v.body {
                    t.binary.update(cx, |e, cx| e.set_text(&path.to_string_lossy(), cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_response(&self, t: &ReqTab, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let pane = div().flex().flex_col().flex_1().min_w_0().min_h_0().gap(px(8.));
        if let Some(l) = &t.live {
            return self.render_live(t, l, cx);
        }
        if let Some((started, _, _)) = &t.sending {
            return pane
                .items_center()
                .justify_center()
                .child(div().text_color(theme::fg_dim()).child(format!("Sending…  {:.1} s", started.elapsed().as_secs_f32())))
                .child(button("api-cancel", "Cancel", Some("stop"), false).on_click(cx.listener(|v, _, _, cx| v.cancel(cx))))
                .into_any_element();
        }
        let Some(resp) = &t.resp else {
            return pane
                .items_center()
                .justify_center()
                .gap(px(10.))
                .when_some(t.error.clone(), |d, e| {
                    d.child(div().id("api-error").max_w(px(560.)).max_h(px(400.)).overflow_y_scroll().p(px(10.)).rounded(px(5.)).bg(theme::bg_input()).text_size(px(12.5)).text_color(theme::git_deleted()).child(e))
                })
                .when(t.error.is_none(), |d| {
                    d.child(ui::icon("api", theme::fg_muted()).size(px(40.)))
                        .child(div().text_color(theme::fg_dim()).child(if t.req.kind == Kind::Http { "Send the request to see its response (⌘↵)" } else { "Connect to see messages (⌘↵)" }))
                })
                .into_any_element();
        };
        let r = &resp.r;
        let passed = resp.tests.iter().filter(|x| x.pass).count();
        let failed = resp.tests.len() - passed;
        let cookies: Vec<&str> = r.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.as_str()).collect();
        let summary = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .text_size(px(12.5))
            .child(div().px(px(8.)).py(px(2.)).rounded(px(4.)).bg(status_color(r.status).opacity(0.18)).text_color(status_color(r.status)).font_weight(FontWeight::BOLD).child(format!("{} {}", r.status, r.reason)))
            .child(div().text_color(theme::fg_dim()).child(format!("{} ms", r.ms)))
            .child(div().text_color(theme::fg_dim()).child(human_size(r.size)))
            .child(div().text_color(theme::fg_muted()).child(r.version.clone()))
            .when(!resp.tests.is_empty(), |d| {
                d.child(div().text_color(if failed == 0 { theme::git_added() } else { theme::git_deleted() }).child(format!("{passed}/{} checks passed", resp.tests.len())))
            });
        let count = |n: usize| (n > 0).then(|| n.to_string());
        let tabs = [
            (RespSection::Body, "Body", None),
            (RespSection::Headers, "Headers", count(r.headers.len())),
            (RespSection::Cookies, "Cookies", count(cookies.len())),
            (RespSection::Tests, "Tests", (!resp.tests.is_empty()).then(|| format!("{passed}/{}", resp.tests.len()))),
            (RespSection::Timeline, "Timeline", None),
            (RespSection::Console, "Console", count(resp.logs.len())),
        ];
        let tabs = div().flex().items_center().gap(px(2.)).border_b_1().border_color(theme::border()).children(tabs.into_iter().map(|(s, name, c)| {
            sub_tab(format!("api-resp-{name}"), name, c, t.resp_section == s).on_click(cx.listener(move |v, _, _, cx| {
                if let Body_::Req(t) = &mut v.body {
                    t.resp_section = s;
                }
                cx.notify();
            }))
        }));
        let kv_list = |rows: Vec<(String, String)>| {
            div().flex().flex_col().children(rows.into_iter().enumerate().map(|(i, (k, v))| {
                div()
                    .id(("api-kv", i))
                    .flex()
                    .gap(px(12.))
                    .py(px(4.))
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(px(12.5))
                    .child(div().w(px(200.)).flex_none().text_color(theme::fg_dim()).child(k))
                    .child(div().flex_1().min_w_0().text_color(theme::fg()).font_family(theme::MONO_FONT).child(v))
            }))
        };
        let content: AnyElement = match t.resp_section {
            RespSection::Body => {
                let tools = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(button("api-resp-pretty", if resp.pretty { "Raw" } else { "Pretty" }, Some("braces"), false).on_click(cx.listener(|v, _, _, cx| {
                        if let Body_::Req(t) = &mut v.body
                            && let Some(r) = &mut t.resp
                        {
                            r.pretty = !r.pretty;
                            Self::show_body(&t.resp_body, r, cx);
                        }
                        cx.notify();
                    })))
                    .child(button("api-resp-copy", "Copy", Some("copy"), false).on_click(cx.listener(|v, _, _, cx| {
                        if let Body_::Req(t) = &v.body {
                            cx.write_to_clipboard(ClipboardItem::new_string(t.resp_body.read(cx).text()));
                            v.status = Some(Ok("Response copied.".into()));
                        }
                        cx.notify();
                    })))
                    .child(button("api-resp-save", "Save to file…", Some("arrow-down"), false).on_click(cx.listener(|v, _, _, cx| v.save_response(cx))))
                    .when(r.body_file.is_some(), |d| d.child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(format!("Showing the first {} of {}", human_size(api::send::MAX_IN_MEMORY as u64), human_size(r.size)))));
                let shown: AnyElement = match &resp.image {
                    Some(p) => div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(8.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme::border())
                        .child(img(p.clone()).max_w_full().max_h_full().object_fit(gpui::ObjectFit::Contain))
                        .into_any_element(),
                    None => div().flex_1().min_h_0().flex().child(code_box(&t.resp_body)).into_any_element(),
                };
                div().flex().flex_col().flex_1().min_h_0().gap(px(6.)).child(tools).child(shown).into_any_element()
            }
            RespSection::Headers => div().id("api-resp-h").flex_1().overflow_y_scroll().child(kv_list(r.headers.clone())).into_any_element(),
            RespSection::Cookies => {
                let rows: Vec<(String, String)> = cookies
                    .iter()
                    .map(|c| {
                        let (nv, attrs) = c.split_once(';').unwrap_or((c, ""));
                        let (n, v) = nv.split_once('=').unwrap_or((nv, ""));
                        (n.trim().to_string(), format!("{}{}", v.trim(), if attrs.trim().is_empty() { String::new() } else { format!("   ({})", attrs.trim()) }))
                    })
                    .collect();
                div()
                    .id("api-resp-c")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(if rows.is_empty() { div().text_color(theme::fg_dim()).child("No cookies were set.") } else { kv_list(rows) })
                    .child(div().flex().child(button("api-cookie-jar", "All cookies…", Some("cookie"), false).on_click(cx.listener(|_, _, _, cx| cx.emit(WsRequest::Api(ApiRequest::Open(ApiOpen::Cookies)))))))
                    .into_any_element()
            }
            RespSection::Tests => div()
                .id("api-resp-t")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(4.))
                .when(resp.tests.is_empty(), |d| d.child(div().text_color(theme::fg_dim()).child("No checks: add some in Tests, or tc.test(…) in Scripts.")))
                .children(resp.tests.iter().map(|x| {
                    div()
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .py(px(3.))
                        .child(ui::small_icon(if x.pass { "check" } else { "close" }, if x.pass { theme::git_added() } else { theme::git_deleted() }))
                        .child(div().flex_1().min_w_0().flex().flex_col().child(div().text_color(theme::fg()).child(x.name.clone())).when(!x.detail.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(x.detail.clone()))))
                }))
                .into_any_element(),
            RespSection::Timeline => {
                let s = &r.sent;
                let mut lines: Vec<(String, String)> = vec![("Request".into(), format!("{} {}", s.method, s.url))];
                lines.extend(s.headers.iter().map(|(k, v)| (format!("  {k}"), if k.eq_ignore_ascii_case("authorization") && v.len() > 16 { format!("{}…", &v[..v.char_indices().nth(12).map(|(i, _)| i).unwrap_or(v.len())]) } else { v.clone() })));
                if !s.body.is_empty() {
                    lines.push(("Body".into(), s.body.chars().take(2000).collect()));
                }
                for to in &r.redirects {
                    lines.push(("Redirected to".into(), to.clone()));
                }
                lines.push(("Response".into(), format!("{} {} {}", r.version, r.status, r.reason)));
                lines.push(("Headers after".into(), format!("{} ms", r.ttfb_ms)));
                lines.push(("Done after".into(), format!("{} ms", r.ms)));
                lines.push(("Size".into(), human_size(r.size)));
                div().id("api-resp-tl").flex_1().overflow_y_scroll().child(kv_list(lines)).into_any_element()
            }
            RespSection::Console => div()
                .id("api-resp-con")
                .flex_1()
                .overflow_y_scroll()
                .p(px(8.))
                .rounded(px(4.))
                .bg(theme::bg_input())
                .font_family(theme::MONO_FONT)
                .text_size(px(12.))
                .when(resp.logs.is_empty(), |d| d.child(div().text_color(theme::fg_muted()).child("console.log(…) from scripts shows here.")))
                .children(resp.logs.iter().map(|l| div().text_color(if l.starts_with('✕') { theme::git_deleted() } else { theme::fg() }).child(l.clone())))
                .into_any_element(),
        };
        pane.child(summary).child(tabs).child(div().flex().flex_col().flex_1().min_h_0().child(content)).into_any_element()
    }

    fn render_live(&self, t: &ReqTab, l: &Live, cx: &mut Context<Self>) -> AnyElement {
        let ws_kind = t.req.kind == Kind::Websocket;
        let state = if l.connected { format!("Connected{}", l.status.map(|s| format!(" ({s})")).unwrap_or_default()) } else if l.events.is_empty() { "Connecting…".into() } else { "Disconnected".into() };
        let log = div().id("api-live-log").track_scroll(&t.live_scroll).flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(px(2.)).children(l.events.iter().enumerate().map(|(i, e)| {
            let (arrow, color, text, meta) = match e {
                ws::Event::Message { text, event, id, ms, .. } => ("↓", theme::fg(), text.clone(), format!("{}{}{}", if event.is_empty() || event == "message" { String::new() } else { format!("{event}  ") }, if id.is_empty() { String::new() } else { format!("id {id}  ") }, format_ms(*ms))),
                ws::Event::Sent { text, ms } => ("↑", theme::accent(), text.clone(), format_ms(*ms)),
                ws::Event::Open { status, .. } => ("•", theme::git_added(), format!("Connected ({status})"), String::new()),
                ws::Event::Closed { reason } => ("•", theme::fg_muted(), format!("Closed: {reason}"), String::new()),
                ws::Event::Error(e) => ("✕", theme::git_deleted(), e.clone(), String::new()),
            };
            let copy = text.clone();
            div()
                .id(("api-live", i))
                .group("api-live-row")
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(6.))
                .py(px(3.))
                .rounded(px(3.))
                .hover(|s| s.bg(theme::hover()))
                .child(div().w(px(12.)).text_color(color).child(arrow))
                .child(div().flex_1().min_w_0().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(color).child(text))
                .child(div().flex_none().text_size(px(10.5)).text_color(theme::fg_muted()).child(meta))
                .child(
                    icon_btn(format!("api-live-copy-{i}"), "copy", "Copy").invisible().group_hover("api-live-row", |s| s.visible()).on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    })),
                )
        }));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(ui::small_icon("dot", if l.connected { theme::git_added() } else { theme::fg_muted() }))
                    .child(div().flex_1().text_color(theme::fg()).child(state))
                    .child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(format!("{} messages", l.events.iter().filter(|e| matches!(e, ws::Event::Message { .. } | ws::Event::Sent { .. })).count())))
                    .child(button("api-live-clear", "Clear", Some("trash"), false).on_click(cx.listener(|v, _, _, cx| {
                        if let Body_::Req(t) = &mut v.body
                            && let Some(l) = &mut t.live
                        {
                            l.events.clear();
                        }
                        cx.notify();
                    }))),
            )
            .child(log)
            .when(ws_kind, |d| {
                d.child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .h(px(90.))
                        .child(code_box(&t.ws_msg))
                        .child(div().flex().flex_col().gap(px(6.)).child(button("api-ws-send", "Send", Some("arrow-up"), true).on_click(cx.listener(|v, _, _, cx| v.ws_send(cx)))).child(button("api-ws-ping", "Ping", None, false).on_click(cx.listener(|v, _, _, cx| {
                            if let Body_::Req(t) = &v.body
                                && let Some(l) = &t.live
                            {
                                l.commands.send(ws::Command::Ping).ok();
                            }
                            cx.notify();
                        })))),
                )
            })
            .into_any_element()
    }

    fn save_response(&mut self, cx: &mut Context<Self>) {
        let Body_::Req(t) = &self.body else { return };
        let Some(resp) = &t.resp else { return };
        let ext = match resp.r.content_type() {
            c if c.contains("json") => "json",
            c if c.contains("html") => "html",
            c if c.contains("xml") => "xml",
            c if c.contains("png") => "png",
            c if c.contains("jpeg") => "jpg",
            c if c.contains("pdf") => "pdf",
            _ => "txt",
        };
        let name = format!("response.{ext}");
        let (body, file) = (if resp.r.body_file.is_some() { Vec::new() } else { resp.r.body.clone() }, resp.r.body_file.clone());
        let dir = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_default();
        let ask = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Some(target) = ask.await.ok().and_then(|r| r.ok()).flatten() else { return };
            let r = match file {
                Some(f) => std::fs::copy(f, &target).map(|_| ()),
                None => std::fs::write(&target, body),
            };
            this.update(cx, |v, cx| {
                v.status = Some(r.map(|_| format!("Saved to {}", target.display())).map_err(|e| e.to_string()));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_env(&self, t: &EnvTab, cx: &mut Context<Self>) -> AnyElement {
        let (_, active, _) = self.envs();
        let is_active = active.as_ref().is_some_and(|a| a.id == t.env.id);
        let id = t.env.id.clone();
        div()
            .id("api-env-tab")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(900.))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(24.))
                    .py(px(22.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::icon("globe", theme::accent()).size(px(22.)))
                            .child(div().w(px(280.)).child(field(&t.name)))
                            .when(t.env.global, |d| d.child(ui::badge("global", theme::badge())))
                            .when(t.env.project.is_some(), |d| d.child(ui::badge("workspace", theme::badge())))
                            .child(div().flex_1())
                            .when(!t.env.global, |d| {
                                d.child(button("api-env-activate", if is_active { "Active" } else { "Use this one" }, Some("check"), false).on_click(cx.listener(move |v, _, _, cx| {
                                    api::set_active_env(v.project().as_deref(), if is_active { None } else { Some(&id) });
                                    cx.emit(WsRequest::Api(ApiRequest::Changed));
                                    cx.notify();
                                })))
                            })
                            .child(button("api-env-save", "Save", Some("check"), true).on_click(cx.listener(|v, _, _, cx| v.save_now(None, cx))))
                            .when(!t.env.global, |d| d.child(button("api-env-delete", "Delete", Some("trash"), false).on_click(cx.listener(|v, _, _, cx| v.ask_delete_env(cx))))),
                    )
                    .child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(if t.env.global {
                        "Global variables: seen by every request, under the active environment's."
                    } else {
                        "Use them as {{name}} in URLs, headers, bodies and auth. The lock keeps a value in your Keychain (not in the file)."
                    }))
                    .child(self.render_kv(&t.vars, cx))
                    .child(div().flex().items_center().gap(px(10.)).child(label(".env file")).child(div().flex_1().child(field(&t.dotenv))))
                    .child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child("Its KEY=value lines are variables too (read when sending)."))
                    .child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(format!("Made fresh each time: {}", api::vars::DYNAMIC.iter().map(|(n, _)| format!("{{{{{n}}}}}")).collect::<Vec<_>>().join("  ")))),
            )
            .into_any_element()
    }

    fn ask_delete_env(&mut self, cx: &mut Context<Self>) {
        let Body_::Env(t) = &self.body else { return };
        let env = t.env.clone();
        let weak = cx.entity().downgrade();
        cx.emit(WsRequest::Ask(crate::dialog::Ask {
            title: format!("Delete the environment “{}”?", env.name),
            detail: "Its variables (and secret values in the Keychain) are removed.".into(),
            buttons: vec!["Delete".into()],
            danger: true,
            cancel_default: false,
            cancel_label: None,
            on_answer: Rc::new(move |_, _, cx: &mut App| {
                api::delete_environment(&env).ok();
                if let Some(v) = weak.upgrade() {
                    v.update(cx, |_, cx| {
                        cx.emit(WsRequest::Api(ApiRequest::Changed));
                        cx.emit(WsRequest::Api(ApiRequest::Close));
                    });
                }
            }),
        }));
    }

    fn render_folder(&self, t: &FolderTab, cx: &mut Context<Self>) -> AnyElement {
        let is_root = t.folder == t.coll.root.id;
        let sections = [(Section::Auth, "Auth"), (Section::Headers, "Headers"), (Section::Vars, "Variables"), (Section::Scripts, "Scripts"), (Section::Params, "Send first")];
        let tabs = div().flex().items_center().gap(px(2.)).border_b_1().border_color(theme::border()).children(sections.into_iter().map(|(s, name)| {
            sub_tab(format!("api-fsec-{name}"), name, None, t.section == s).on_click(cx.listener(move |v, _, _, cx| {
                if let Body_::Folder(t) = &mut v.body {
                    t.section = s;
                }
                cx.notify();
            }))
        }));
        let hint = |text: &str| div().text_size(px(11.5)).text_color(theme::fg_muted()).child(text.to_string());
        let content: AnyElement = match t.section {
            Section::Auth => self.render_auth(&t.auth, AuthOwner::Folder, cx),
            Section::Headers => div().flex().flex_col().gap(px(6.)).child(self.render_kv(&t.headers, cx)).child(hint("Sent with every request inside.")).into_any_element(),
            Section::Vars => div().flex().flex_col().gap(px(6.)).child(self.render_kv(&t.vars, cx)).child(hint("Seen by every request inside; nearer folders and environments win.")).into_any_element(),
            Section::Scripts => div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_color(theme::fg_dim()).child("Pre-request, for every request inside"))
                .child(div().h(px(200.)).flex().child(code_box(&t.pre)))
                .child(div().text_color(theme::fg_dim()).child("Post-response, for every request inside"))
                .child(div().h(px(200.)).flex().child(code_box(&t.post)))
                .into_any_element(),
            _ => {
                // Requests sent before each one inside (a login, say).
                let mut all: Vec<(String, String, String)> = Vec::new();
                fn walk(f: &api::Folder, out: &mut Vec<(String, String, String)>) {
                    for r in &f.requests {
                        out.push((r.id.clone(), r.method.clone(), r.name.clone()));
                    }
                    for s in &f.folders {
                        walk(s, out);
                    }
                }
                walk(&t.coll.root, &mut all);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(hint("Checked requests are sent first, before each request inside (their Set Vars give it a token, say)."))
                    .children(all.into_iter().enumerate().map(|(i, (id, m, name))| {
                        let on = t.pre_requests.contains(&id);
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(check(format!("api-pre-{i}"), on).on_click(cx.listener(move |v, _, _, cx| {
                                if let Body_::Folder(t) = &mut v.body {
                                    if on {
                                        t.pre_requests.retain(|x| *x != id);
                                    } else {
                                        t.pre_requests.push(id.clone());
                                    }
                                }
                                v.touch(cx);
                            })))
                            .child(div().w(px(52.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(method_color(&m)).child(m))
                            .child(div().text_color(theme::fg()).child(name))
                    }))
                    .into_any_element()
            }
        };
        let (coll, folder) = (t.coll.id.clone(), t.folder.clone());
        div()
            .id("api-folder-tab")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(900.))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(24.))
                    .py(px(22.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::icon(if is_root { "collection" } else { "folder" }, theme::accent()).size(px(22.)))
                            .child(div().w(px(300.)).child(field(&t.name)))
                            .child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child(if is_root { "Collection" } else { "Folder" }))
                            .child(div().flex_1())
                            .child(button("api-folder-run", "Run", Some("play"), false).on_click(cx.listener(move |_, _, _, cx| cx.emit(WsRequest::Api(ApiRequest::Open(ApiOpen::Runner { coll: coll.clone(), folder: folder.clone() }))))))
                            .child(button("api-folder-save", "Save", Some("check"), true).on_click(cx.listener(|v, _, _, cx| v.save_now(None, cx)))),
                    )
                    .child(tabs)
                    .child(content),
            )
            .into_any_element()
    }

    fn render_runner(&self, t: &RunnerTab, cx: &mut Context<Self>) -> AnyElement {
        let Some(folder) = t.coll.root.find(&t.folder) else { return div().into_any_element() };
        let mut reqs: Vec<(String, String, String)> = Vec::new();
        fn walk(f: &api::Folder, out: &mut Vec<(String, String, String)>) {
            for s in &f.folders {
                walk(s, out);
            }
            for r in &f.requests {
                out.push((r.id.clone(), r.method.clone(), r.name.clone()));
            }
        }
        walk(folder, &mut reqs);
        let total_pass: usize = t.rows.iter().map(|r| r.passed).sum();
        let total_fail: usize = t.rows.iter().map(|r| r.failed.len() + r.error.is_some() as usize).sum();
        let running = t.running.is_some();
        div()
            .id("api-runner")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(980.))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(24.))
                    .py(px(22.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::icon("play", theme::accent()).size(px(20.)))
                            .child(div().flex_1().text_size(px(18.)).text_color(theme::fg_bright()).child(format!("Run {}", folder.name)))
                            .child(button("api-run", if running { "Stop" } else { "Run" }, Some(if running { "stop" } else { "play" }), true).on_click(cx.listener(|v, _, _, cx| v.run_collection(cx)))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().text_color(theme::fg_dim()).child("Iterations"))
                            .child(div().w(px(70.)).child(field(&t.iterations)))
                            .child(div().text_color(theme::fg_dim()).child("Delay (ms)"))
                            .child(div().w(px(80.)).child(field(&t.delay)))
                            .child(div().text_color(theme::fg_dim()).child("Data"))
                            .child(div().flex_1().child(field(&t.data))),
                    )
                    .child(
                        div().flex().flex_col().gap(px(2.)).child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("REQUESTS, IN ORDER")).children(reqs.into_iter().enumerate().map(|(i, (id, m, name))| {
                            let on = !t.skip.contains(&id);
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .py(px(2.))
                                .child(check(format!("api-run-on-{i}"), on).on_click(cx.listener(move |v, _, _, cx| {
                                    if let Body_::Runner(t) = &mut v.body
                                        && !t.skip.remove(&id)
                                    {
                                        t.skip.insert(id.clone());
                                    }
                                    cx.notify();
                                })))
                                .child(div().w(px(52.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(method_color(&m)).child(m))
                                .child(div().text_color(if on { theme::fg() } else { theme::fg_muted() }).child(name))
                        })),
                    )
                    .when(!t.rows.is_empty(), |d| {
                        d.child(div().h(px(1.)).bg(theme::border())).child(
                            div()
                                .flex()
                                .gap(px(16.))
                                .text_size(px(12.5))
                                .child(div().text_color(theme::fg()).child(format!("{} sent", t.rows.len())))
                                .child(div().text_color(theme::git_added()).child(format!("{total_pass} passed")))
                                .child(div().text_color(if total_fail > 0 { theme::git_deleted() } else { theme::fg_muted() }).child(format!("{total_fail} failed")))
                                .when_some(t.took, |d, ms| d.child(div().text_color(theme::fg_muted()).child(format!("in {}", format_ms(ms))))),
                        )
                    })
                    .children(t.rows.iter().map(|r| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .py(px(5.))
                            .border_b_1()
                            .border_color(theme::border())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(div().w(px(52.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(method_color(&r.method)).child(r.method.clone()))
                                    .child(div().flex_1().text_color(theme::fg()).child(r.name.clone()))
                                    .child(match r.status {
                                        Some(s) => div().text_color(status_color(s)).child(s.to_string()),
                                        None => div().text_color(theme::git_deleted()).child("failed"),
                                    })
                                    .child(div().w(px(70.)).text_color(theme::fg_muted()).child(format!("{} ms", r.ms)))
                                    .child(div().w(px(80.)).text_color(if r.failed.is_empty() { theme::git_added() } else { theme::git_deleted() }).child(if r.passed + r.failed.len() == 0 { String::new() } else { format!("{}/{} ✓", r.passed, r.passed + r.failed.len()) })),
                            )
                            .when_some(r.error.clone(), |d, e| d.child(div().pl(px(62.)).text_size(px(11.5)).text_color(theme::git_deleted()).child(e)))
                            .children(r.failed.iter().map(|f| div().pl(px(62.)).text_size(px(11.5)).text_color(theme::git_deleted()).child(format!("✕ {f}"))))
                    })),
            )
            .into_any_element()
    }

    fn render_cookies(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut jar = send::cookies();
        jar.sort_by(|a, b| (a.domain.as_str(), a.name.as_str()).cmp(&(b.domain.as_str(), b.name.as_str())));
        let mut domains: Vec<String> = jar.iter().map(|c| c.domain.clone()).collect();
        domains.dedup();
        div()
            .id("api-cookies")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(900.))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .px(px(24.))
                    .py(px(22.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::icon("cookie", theme::accent()).size(px(22.)))
                            .child(div().flex_1().text_size(px(18.)).text_color(theme::fg_bright()).child("Cookies"))
                            .child(button("api-cookies-clear", "Delete all", Some("trash"), false).on_click(cx.listener(|_, _, _, cx| {
                                send::delete_cookie(None, None);
                                cx.notify();
                            }))),
                    )
                    .child(div().text_size(px(11.5)).text_color(theme::fg_muted()).child("Cookies servers set are sent back to them, like a browser does. Session cookies last until the app quits."))
                    .when(jar.is_empty(), |d| d.child(div().text_color(theme::fg_dim()).child("No cookies yet.")))
                    .children(domains.into_iter().enumerate().map(|(di, d)| {
                        let dom = d.clone();
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().flex_1().text_color(theme::fg_bright()).font_weight(FontWeight::MEDIUM).child(d.clone()))
                                    .child(icon_btn(format!("api-cookie-dom-{di}"), "trash", "Delete this site's cookies").on_click(cx.listener(move |_, _, _, cx| {
                                        send::delete_cookie(Some(&dom), None);
                                        cx.notify();
                                    }))),
                            )
                            .children(jar.iter().filter(|c| c.domain == d).enumerate().map(|(ci, c)| {
                                let (dd, nn) = (c.domain.clone(), c.name.clone());
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .pl(px(12.))
                                    .py(px(3.))
                                    .text_size(px(12.5))
                                    .child(div().w(px(180.)).text_color(theme::fg()).child(c.name.clone()))
                                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme::MONO_FONT).text_color(theme::fg_dim()).child(c.value.clone()))
                                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(format!("{}{}{}", c.path, if c.secure { " · secure" } else { "" }, if c.expires == 0 { " · session".to_string() } else { String::new() })))
                                    .child(icon_btn(format!("api-cookie-{di}-{ci}"), "close", "Delete").on_click(cx.listener(move |_, _, _, cx| {
                                        send::delete_cookie(Some(&dd), Some(&nn));
                                        cx.notify();
                                    })))
                            }))
                    })),
            )
            .into_any_element()
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_ref()?;
        let items = m.items.iter().enumerate().map(|(i, (label, act))| {
            let act = act.clone();
            div()
                .id(("api-menu-item", i))
                .debug_selector(move || format!("api-menu-item-{i}"))
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(3.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::selected_focused()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |v, _, w, cx| {
                    v.menu = None;
                    act(v, w, cx);
                    cx.notify();
                }))
                .child(label.clone())
        });
        let list = div()
            .id("api-menu")
            .occlude()
            .min_w(px(160.))
            .whitespace_nowrap()
            .max_h(px(360.))
            .overflow_y_scroll()
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(12.5))
            .text_color(theme::fg())
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.menu = None;
                cx.notify();
            }))
            .children(items);
        Some(deferred(anchored().position(m.at).snap_to_window().child(list)).with_priority(2).into_any_element())
    }
}

fn format_ms(ms: u64) -> String {
    if ms < 1000 { format!("{ms} ms") } else if ms < 60_000 { format!("{:.1} s", ms as f64 / 1000.) } else { format!("{}m {}s", ms / 60_000, ms / 1000 % 60) }
}

impl Focusable for ApiView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ApiView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.body {
            Body_::Req(t) => self.render_request(t, window, cx),
            Body_::Env(t) => self.render_env(t, cx),
            Body_::Folder(t) => self.render_folder(t, cx),
            Body_::Runner(t) => self.render_runner(t, cx),
            Body_::Cookies => self.render_cookies(cx),
            Body_::Missing(m) => div().size_full().flex().items_center().justify_center().text_color(theme::fg_dim()).child(m.clone()).into_any_element(),
        };
        let menu = self.render_menu(cx);
        let save = self.save_dialog.as_ref().map(|d| self.render_save_dialog(d, cx));
        div()
            .key_context("ApiView")
            .relative()
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::send_action))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_menu))
            .size_full()
            .bg(theme::bg_editor())
            .text_color(theme::fg())
            .text_size(px(13.))
            .child(body)
            .children(save)
            .children(menu)
    }
}
