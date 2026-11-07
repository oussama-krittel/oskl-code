//! API requests: collections (folders of saved requests), environments, history, cookies.
//!
//! Everything is on disk and read when needed: a collection's tree is small (names and
//! methods), a request is read when it's opened, history is a capped file. Sending runs on
//! the database thread (see `send`), and big responses go to a temporary file, so memory
//! stays low however much is sent or received.

pub mod codegen;
pub mod import;
pub mod run;
pub mod script;
pub mod send;
pub mod tests;
pub mod vars;
pub mod ws;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn yes() -> bool {
    true
}

/// A key, its value and a note, switched on or off (params, headers, form fields, variables).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Row {
    #[serde(default = "yes")]
    pub on: bool,
    pub key: String,
    pub value: String,
    pub desc: String,
    /// Multipart: the value is a file's path. Variables: the value is a secret (Keychain).
    pub flag: bool,
}

impl Default for Row {
    fn default() -> Row {
        Row { on: true, key: String::new(), value: String::new(), desc: String::new(), flag: false }
    }
}

impl Row {
    pub fn new(key: &str, value: &str) -> Row {
        Row { key: key.into(), value: value.into(), ..Default::default() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Body {
    #[default]
    None,
    Json {
        text: String,
    },
    Xml {
        text: String,
    },
    Text {
        text: String,
    },
    Html {
        text: String,
    },
    /// application/x-www-form-urlencoded.
    Form {
        rows: Vec<Row>,
    },
    /// multipart/form-data: a row's `flag` makes its value a file.
    Multipart {
        rows: Vec<Row>,
    },
    /// A file sent as is.
    Binary {
        path: String,
    },
    Graphql {
        query: String,
        variables: String,
    },
}

impl Body {
    pub fn kind(&self) -> &'static str {
        match self {
            Body::None => "None",
            Body::Json { .. } => "JSON",
            Body::Xml { .. } => "XML",
            Body::Text { .. } => "Text",
            Body::Html { .. } => "HTML",
            Body::Form { .. } => "Form",
            Body::Multipart { .. } => "Multipart",
            Body::Binary { .. } => "Binary",
            Body::Graphql { .. } => "GraphQL",
        }
    }

    pub const KINDS: [&'static str; 9] = ["None", "JSON", "XML", "Text", "HTML", "Form", "Multipart", "Binary", "GraphQL"];

    /// The same text in another kind (switching keeps what was typed).
    pub fn switch(&self, kind: &str) -> Body {
        let text = match self {
            Body::Json { text } | Body::Xml { text } | Body::Text { text } | Body::Html { text } => text.clone(),
            Body::Graphql { query, .. } => query.clone(),
            _ => String::new(),
        };
        let rows = match self {
            Body::Form { rows } | Body::Multipart { rows } => rows.clone(),
            _ => Vec::new(),
        };
        match kind {
            "JSON" => Body::Json { text },
            "XML" => Body::Xml { text },
            "Text" => Body::Text { text },
            "HTML" => Body::Html { text },
            "Form" => Body::Form { rows },
            "Multipart" => Body::Multipart { rows },
            "Binary" => Body::Binary { path: String::new() },
            "GraphQL" => Body::Graphql { query: text, variables: String::new() },
            _ => Body::None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct OAuth2 {
    /// client_credentials, password, authorization_code, refresh_token.
    pub grant: String,
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: String,
    pub audience: String,
    pub username: String,
    pub password: String,
    /// Authorization code: where the browser comes back (a local port this app listens on).
    pub redirect_uri: String,
    pub pkce: bool,
    /// Send the client's id and secret as Basic auth (else in the form).
    pub basic_client_auth: bool,
    /// "Bearer" usually.
    pub prefix: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Auth {
    /// As the folder (or collection) above says.
    #[default]
    Inherit,
    None,
    Basic {
        user: String,
        pass: String,
    },
    Bearer {
        token: String,
        prefix: String,
    },
    /// A key sent in a header (or the query).
    Apikey {
        key: String,
        value: String,
        query: bool,
    },
    Digest {
        user: String,
        pass: String,
    },
    Oauth2(OAuth2),
    /// AWS Signature v4.
    Aws {
        access_key: String,
        secret_key: String,
        session_token: String,
        region: String,
        service: String,
    },
}

impl Auth {
    pub fn kind(&self) -> &'static str {
        match self {
            Auth::Inherit => "Inherit",
            Auth::None => "None",
            Auth::Basic { .. } => "Basic",
            Auth::Bearer { .. } => "Bearer",
            Auth::Apikey { .. } => "API Key",
            Auth::Digest { .. } => "Digest",
            Auth::Oauth2(_) => "OAuth 2.0",
            Auth::Aws { .. } => "AWS Signature",
        }
    }

    pub const KINDS: [&'static str; 8] = ["Inherit", "None", "Basic", "Bearer", "API Key", "Digest", "OAuth 2.0", "AWS Signature"];
}

/// A check on the response, without code.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Test {
    #[serde(default = "yes")]
    pub on: bool,
    /// What's checked: status, time, header, json, body, content-type, schema.
    pub what: String,
    /// The header's name, or the JSONPath.
    pub path: String,
    /// equal, not-equal, contains, not-contains, <, <=, >, >=, exists, not-exists, type, count,
    /// matches (a regex).
    pub op: String,
    pub value: String,
}

impl Default for Test {
    fn default() -> Test {
        Test { on: true, what: "status".into(), path: String::new(), op: "equal".into(), value: "200".into() }
    }
}

/// A variable set from the response (chaining requests).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SetVar {
    #[serde(default = "yes")]
    pub on: bool,
    pub var: String,
    /// json (a JSONPath), header, status, body, cookie.
    pub from: String,
    pub path: String,
    /// environment, global, collection.
    pub scope: String,
}

impl Default for SetVar {
    fn default() -> SetVar {
        SetVar { on: true, var: String::new(), from: "json".into(), path: "$.".into(), scope: "environment".into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub timeout_ms: u64,
    pub follow_redirects: bool,
    pub max_redirects: u32,
    pub verify_tls: bool,
    /// auto, 1.1, 2.
    pub http: String,
    /// Empty: the system's proxy settings. "none": no proxy.
    pub proxy: String,
    /// Hosts that skip the proxy, comma-separated.
    pub no_proxy: String,
    /// A client certificate (PKCS#12, .p12/.pfx) and its password.
    pub client_cert: String,
    pub client_cert_pass: String,
    /// Extra trusted certificate authority (PEM).
    pub ca_cert: String,
    /// Percent-encode the URL's characters that need it.
    pub encode_url: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            timeout_ms: 30_000,
            follow_redirects: true,
            max_redirects: 10,
            verify_tls: true,
            http: "auto".into(),
            proxy: String::new(),
            no_proxy: String::new(),
            client_cert: String::new(),
            client_cert_pass: String::new(),
            ca_cert: String::new(),
            encode_url: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Http,
    Websocket,
    /// Server-Sent Events.
    Sse,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Request {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub method: String,
    pub url: String,
    pub params: Vec<Row>,
    /// `:name` or `{name}` parts of the URL.
    pub path_params: Vec<Row>,
    pub headers: Vec<Row>,
    pub body: Body,
    pub auth: Auth,
    pub tests: Vec<Test>,
    pub set_vars: Vec<SetVar>,
    /// Requests (ids, same collection) sent first.
    pub pre_requests: Vec<String>,
    pub pre_script: String,
    pub post_script: String,
    pub settings: Settings,
    pub docs: String,
    /// The last response, kept when small (examples).
    pub example: Option<String>,
}

impl Default for Request {
    fn default() -> Request {
        Request {
            id: new_id(),
            name: "New Request".into(),
            kind: Kind::Http,
            method: "GET".into(),
            url: String::new(),
            params: Vec::new(),
            path_params: Vec::new(),
            headers: Vec::new(),
            body: Body::None,
            auth: Auth::Inherit,
            tests: Vec::new(),
            set_vars: Vec::new(),
            pre_requests: Vec::new(),
            pre_script: String::new(),
            post_script: String::new(),
            settings: Settings::default(),
            docs: String::new(),
            example: None,
        }
    }
}

pub const METHODS: [&str; 9] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT"];

/// A method's color (the usual ones), as hex.
pub fn method_color(m: &str) -> u32 {
    match m {
        "GET" => 0x3fb950,
        "POST" => 0xd29922,
        "PUT" => 0x388bfd,
        "PATCH" => 0xa371f7,
        "DELETE" => 0xf85149,
        "WS" => 0x39c5cf,
        "SSE" => 0xdb61a2,
        _ => 0x8b949e,
    }
}

/// A request's entry in its collection's tree (the request itself is its own file).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub method: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub folders: Vec<Folder>,
    pub requests: Vec<Item>,
    /// Shared by what's inside: auth (unless a request says otherwise), headers, variables,
    /// scripts, requests sent first.
    pub auth: Auth,
    pub headers: Vec<Row>,
    pub vars: Vec<Row>,
    pub pre_script: String,
    pub post_script: String,
    pub pre_requests: Vec<String>,
}

impl Default for Folder {
    fn default() -> Folder {
        Folder {
            id: new_id(),
            name: "New Folder".into(),
            folders: Vec::new(),
            requests: Vec::new(),
            auth: Auth::None,
            headers: Vec::new(),
            vars: Vec::new(),
            pre_script: String::new(),
            post_script: String::new(),
            pre_requests: Vec::new(),
        }
    }
}

impl Folder {
    pub fn find(&self, id: &str) -> Option<&Folder> {
        if self.id == id {
            return Some(self);
        }
        self.folders.iter().find_map(|f| f.find(id))
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Folder> {
        if self.id == id {
            return Some(self);
        }
        self.folders.iter_mut().find_map(|f| f.find_mut(id))
    }

    /// The folders from this one down to the one holding request `id` (empty: not here).
    pub fn chain_to(&self, id: &str) -> Vec<&Folder> {
        if self.requests.iter().any(|r| r.id == id) {
            return vec![self];
        }
        for f in &self.folders {
            let mut c = f.chain_to(id);
            if !c.is_empty() {
                c.insert(0, self);
                return c;
            }
        }
        Vec::new()
    }

    /// Takes a request's entry out (wherever it is).
    pub fn take(&mut self, id: &str) -> Option<Item> {
        if let Some(i) = self.requests.iter().position(|r| r.id == id) {
            return Some(self.requests.remove(i));
        }
        self.folders.iter_mut().find_map(|f| f.take(id))
    }

    /// Takes a folder out (wherever it is, but not this one).
    pub fn take_folder(&mut self, id: &str) -> Option<Folder> {
        if let Some(i) = self.folders.iter().position(|f| f.id == id) {
            return Some(self.folders.remove(i));
        }
        self.folders.iter_mut().find_map(|f| f.take_folder(id))
    }

    /// Every request's id, in order (for the runner).
    pub fn all_requests(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in &self.folders {
            out.extend(f.all_requests());
        }
        out.extend(self.requests.iter().map(|r| r.id.clone()));
        out
    }

    /// The folder `id` (if it's here), else this one.
    pub fn folder_or_self(&mut self, id: Option<&str>) -> &mut Folder {
        match id {
            Some(id) if self.find(id).is_some() => self.find_mut(id).unwrap(),
            _ => self,
        }
    }

    pub fn count(&self) -> usize {
        self.requests.len() + self.folders.iter().map(Folder::count).sum::<usize>()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Collection {
    pub id: String,
    /// Saved for this project (shown only in its windows), else for every window.
    pub project: Option<PathBuf>,
    /// The collection's own folder: its name, settings, and everything in it.
    pub root: Folder,
}

impl Collection {
    pub fn new(name: &str, project: Option<PathBuf>) -> Collection {
        let id = new_id();
        Collection { id: id.clone(), project, root: Folder { id, name: name.into(), ..Default::default() } }
    }

    pub fn name(&self) -> &str {
        &self.root.name
    }

    fn dir(&self) -> Option<PathBuf> {
        Some(base(self.project.as_deref())?.join("collections").join(&self.id))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Environment {
    pub id: String,
    pub name: String,
    /// The one everything sees, under the active one.
    pub global: bool,
    pub project: Option<PathBuf>,
    /// A row's `flag`: secret (its value is in the Keychain, not the file).
    pub vars: Vec<Row>,
    /// A linked .env file, read when sending.
    pub dotenv: String,
}

pub fn new_id() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros()).unwrap_or(0);
    format!("{t:x}{:04x}{:04x}", fastrand::u16(..), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff)
}

// ---------------------------------------------------------------- where things are kept

/// Where API data is kept: `~/.config/kite/api`, and under it `projects/<name>-<id>` for each
/// project's own (nothing is written into the projects themselves).
pub fn base(project: Option<&Path>) -> Option<PathBuf> {
    #[cfg(test)]
    let global = TEST_BASE.with(|b| b.borrow().clone()).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/kite/api")))?;
    #[cfg(not(test))]
    let global = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/kite/api"))?;
    match project {
        None => Some(global),
        Some(p) => {
            let dir = project_dir(&global, p);
            move_from_project(p, &dir);
            Some(dir)
        }
    }
}

/// A project's folder in the store: its name (to find it by eye) and a short id from its full
/// path (two projects with one name don't mix).
fn project_dir(global: &Path, project: &Path) -> PathBuf {
    let full = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
    let key = full.to_string_lossy();
    // FNV-1a: stable across runs and versions.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    let name: String = full.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).take(40).collect();
    global.join("projects").join(format!("{name}-{:08x}", h as u32))
}

/// Collections and environments that used to be kept in the project (`.oskl/api`) move to the
/// store, once; the project's `.oskl` folder goes if nothing else is in it.
fn move_from_project(project: &Path, to: &Path) {
    static DONE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::LazyLock::new(Default::default);
    if !DONE.lock().unwrap().insert(project.to_path_buf()) {
        return;
    }
    let old = project.join(".oskl/api");
    if !old.is_dir() {
        return;
    }
    if to.exists() {
        // Both: what's only in the old place is added.
        merge_dir(&old, to);
        std::fs::remove_dir_all(&old).ok();
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if std::fs::rename(&old, to).is_err() {
            merge_dir(&old, to);
            std::fs::remove_dir_all(&old).ok();
        }
    }
    std::fs::remove_dir(project.join(".oskl")).ok();
}

fn merge_dir(from: &Path, to: &Path) {
    let Ok(rd) = std::fs::read_dir(from) else { return };
    std::fs::create_dir_all(to).ok();
    for e in rd.flatten() {
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            merge_dir(&e.path(), &dest);
        } else if !dest.exists() {
            std::fs::copy(e.path(), &dest).ok();
        }
    }
}

#[cfg(test)]
thread_local! {
    pub static TEST_BASE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| e.to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// The collections saved for everyone and in these projects.
pub fn collections(roots: &[PathBuf]) -> Vec<Collection> {
    let mut out = Vec::new();
    let bases: Vec<(Option<PathBuf>, PathBuf)> =
        std::iter::once(None).chain(roots.iter().cloned().map(Some)).filter_map(|p| Some((p.clone(), base(p.as_deref())?))).collect();
    for (project, b) in bases {
        let Ok(dir) = std::fs::read_dir(b.join("collections")) else { continue };
        for e in dir.flatten() {
            if let Some(mut c) = read_json::<Collection>(&e.path().join("collection.json")) {
                c.project = project.clone();
                if !out.iter().any(|o: &Collection| o.id == c.id) {
                    out.push(c);
                }
            }
        }
    }
    out.sort_by_key(|c| c.name().to_lowercase());
    out
}

pub fn save_collection(c: &Collection) -> Result<(), String> {
    let dir = c.dir().ok_or("no home folder")?;
    // The project is where it's kept, not part of it (the file moves with the folder).
    let mut c = c.clone();
    c.project = None;
    write_json(&dir.join("collection.json"), &c)
}

pub fn delete_collection(c: &Collection) -> Result<(), String> {
    let dir = c.dir().ok_or("no home folder")?;
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

fn request_path(c: &Collection, id: &str) -> Option<PathBuf> {
    Some(c.dir()?.join("requests").join(format!("{id}.json")))
}

pub fn load_request(c: &Collection, id: &str) -> Option<Request> {
    read_json(&request_path(c, id)?)
}

/// Saves a request, and its name and method in the collection's tree (which is saved too).
pub fn save_request(c: &mut Collection, req: &Request, folder: Option<&str>) -> Result<(), String> {
    write_json(&request_path(c, &req.id).ok_or("no home folder")?, req)?;
    let method = match req.kind {
        Kind::Http => req.method.clone(),
        Kind::Websocket => "WS".into(),
        Kind::Sse => "SSE".into(),
    };
    let item = Item { id: req.id.clone(), name: req.name.clone(), method };
    if let Some(it) = find_item_mut(&mut c.root, &req.id) {
        *it = item;
    } else {
        c.root.folder_or_self(folder).requests.push(item);
    }
    save_collection(c)
}

fn find_item_mut<'a>(f: &'a mut Folder, id: &str) -> Option<&'a mut Item> {
    if let Some(i) = f.requests.iter_mut().find(|r| r.id == id) {
        return Some(i);
    }
    f.folders.iter_mut().find_map(|x| find_item_mut(x, id))
}

pub fn delete_request(c: &mut Collection, id: &str) -> Result<(), String> {
    c.root.take(id);
    if let Some(p) = request_path(c, id) {
        std::fs::remove_file(p).ok();
    }
    save_collection(c)
}

/// Deletes a folder and its requests' files.
pub fn delete_folder(c: &mut Collection, id: &str) -> Result<(), String> {
    if let Some(f) = c.root.take_folder(id) {
        for r in f.all_requests() {
            if let Some(p) = request_path(c, &r) {
                std::fs::remove_file(p).ok();
            }
        }
    }
    save_collection(c)
}

/// A copy of a request, saved next to it.
pub fn duplicate_request(c: &mut Collection, id: &str) -> Result<Request, String> {
    let mut r = load_request(c, id).ok_or("the request isn't there any more")?;
    r.id = new_id();
    r.name = format!("{} copy", r.name);
    let folder = c.root.chain_to(id).last().map(|f| f.id.clone());
    save_request(c, &r, folder.as_deref())?;
    Ok(r)
}

/// Moves a request into another folder (of the same collection), at its end.
pub fn move_request(c: &mut Collection, id: &str, to_folder: &str) -> Result<(), String> {
    let item = c.root.take(id).ok_or("not found")?;
    c.root.folder_or_self(Some(to_folder)).requests.push(item);
    save_collection(c)
}

// ---------------------------------------------------------------- environments

const ENV_SECRETS: &str = "OSKL Code API";

fn envs_path(project: Option<&Path>) -> Option<PathBuf> {
    Some(base(project)?.join("environments.json"))
}

/// The environments for everyone and these projects, the global one first.
pub fn environments(roots: &[PathBuf]) -> Vec<Environment> {
    let mut out: Vec<Environment> = Vec::new();
    for project in std::iter::once(None).chain(roots.iter().cloned().map(Some)) {
        let list: Vec<Environment> = envs_path(project.as_deref()).and_then(|p| read_json(&p)).unwrap_or_default();
        for mut e in list {
            e.project = project.clone();
            if !out.iter().any(|o| o.id == e.id) {
                out.push(e);
            }
        }
    }
    if !out.iter().any(|e| e.global) {
        out.insert(0, Environment { id: "global".into(), name: "Global".into(), global: true, ..Default::default() });
    }
    out.sort_by_key(|e| (!e.global, e.name.to_lowercase()));
    out
}

/// Saves an environment (secret values go to the Keychain, the file keeps their names).
pub fn save_environment(env: &Environment) -> Result<(), String> {
    let path = envs_path(env.project.as_deref()).ok_or("no home folder")?;
    let mut list: Vec<Environment> = read_json(&path).unwrap_or_default();
    let mut stored = env.clone();
    stored.project = None;
    for v in stored.vars.iter_mut().filter(|v| v.flag) {
        set_secret(&format!("{}/{}", env.id, v.key), &v.value)?;
        v.value.clear();
    }
    match list.iter_mut().find(|e| e.id == env.id) {
        Some(e) => *e = stored,
        None => list.push(stored),
    }
    write_json(&path, &list)
}

pub fn delete_environment(env: &Environment) -> Result<(), String> {
    let path = envs_path(env.project.as_deref()).ok_or("no home folder")?;
    let mut list: Vec<Environment> = read_json(&path).unwrap_or_default();
    list.retain(|e| e.id != env.id);
    for v in env.vars.iter().filter(|v| v.flag) {
        set_secret(&format!("{}/{}", env.id, v.key), "").ok();
    }
    write_json(&path, &list)
}

/// An environment with its secret values read back (for sending, or editing).
pub fn with_secrets(env: &Environment) -> Environment {
    let mut e = env.clone();
    for v in e.vars.iter_mut().filter(|v| v.flag && v.value.is_empty()) {
        v.value = secret(&format!("{}/{}", env.id, v.key));
    }
    e
}

#[cfg(test)]
static TEST_SECRETS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::LazyLock::new(Default::default);

fn secret(key: &str) -> String {
    #[cfg(test)]
    return TEST_SECRETS.lock().unwrap().get(key).cloned().unwrap_or_default();
    #[cfg(not(test))]
    crate::db::keychain::read(ENV_SECRETS, key).unwrap_or_default()
}

fn set_secret(key: &str, value: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        let _ = ENV_SECRETS;
        TEST_SECRETS.lock().unwrap().insert(key.into(), value.into());
        return Ok(());
    }
    #[cfg(not(test))]
    if value.is_empty() {
        crate::db::keychain::delete(ENV_SECRETS, key);
        Ok(())
    } else {
        crate::db::keychain::write(ENV_SECRETS, key, value)
    }
}

/// Which environment is active, per project ("" for windows without one).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct State {
    active: std::collections::HashMap<String, String>,
}

fn state_path() -> Option<PathBuf> {
    Some(base(None)?.join("state.json"))
}

pub fn active_env(project: Option<&Path>) -> Option<String> {
    let s: State = state_path().and_then(|p| read_json(&p)).unwrap_or_default();
    s.active.get(&project.map(|p| p.to_string_lossy().to_string()).unwrap_or_default()).cloned()
}

pub fn set_active_env(project: Option<&Path>, id: Option<&str>) {
    let Some(path) = state_path() else { return };
    let mut s: State = read_json(&path).unwrap_or_default();
    let k = project.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    match id {
        Some(id) => s.active.insert(k, id.to_string()),
        None => s.active.remove(&k),
    };
    write_json(&path, &s).ok();
}

// ---------------------------------------------------------------- history

/// The most requests kept in history, and for how long.
pub const HISTORY_MAX: usize = 500;
pub const HISTORY_DAYS: u64 = 30;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Visit {
    pub id: String,
    /// Seconds since 1970.
    pub at: u64,
    pub status: u16,
    pub ms: u64,
    pub size: u64,
    /// What was sent (as written, with its {{variables}}).
    pub request: Request,
}

fn history_path() -> Option<PathBuf> {
    Some(base(None)?.join("history.jsonl"))
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The history, newest first (old and extra entries are dropped).
pub fn history() -> Vec<Visit> {
    let Some(path) = history_path() else { return Vec::new() };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let cutoff = now_secs().saturating_sub(HISTORY_DAYS * 86_400);
    let mut out: Vec<Visit> = text.lines().rev().filter_map(|l| serde_json::from_str::<Visit>(l).ok()).filter(|v| v.at >= cutoff).collect();
    out.truncate(HISTORY_MAX);
    out
}

pub fn add_history(v: &Visit) {
    let Some(path) = history_path() else { return };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).ok();
    }
    let mut v = v.clone();
    v.request.example = None;
    let Ok(line) = serde_json::to_string(&v) else { return };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        writeln!(f, "{line}").ok();
    }
    // Now and then: rewritten without what's too old or too many (the file stays small).
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 2_000_000 || fastrand::u8(..) < 4 {
        let mut keep = history();
        keep.reverse();
        let text: String = keep.iter().filter_map(|v| serde_json::to_string(v).ok()).map(|l| l + "\n").collect();
        std::fs::write(&path, text).ok();
    }
}

pub fn delete_history(id: Option<&str>) {
    let Some(path) = history_path() else { return };
    match id {
        None => {
            std::fs::remove_file(&path).ok();
        }
        Some(id) => {
            let mut keep = history();
            keep.retain(|v| v.id != id);
            keep.reverse();
            let text: String = keep.iter().filter_map(|v| serde_json::to_string(v).ok()).map(|l| l + "\n").collect();
            std::fs::write(&path, text).ok();
        }
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("kite-api-{}", new_id()));
        TEST_BASE.with(|b| *b.borrow_mut() = Some(d.join("global")));
        d
    }

    #[test]
    fn collections_requests_and_folders_are_saved_as_files() {
        let d = temp();
        let mut c = Collection::new("Shop API", None);
        let mut f = Folder { name: "Orders".into(), ..Default::default() };
        let fid = f.id.clone();
        f.headers.push(Row::new("X-Team", "core"));
        c.root.folders.push(f);
        save_collection(&c).unwrap();
        let mut r = Request { name: "List orders".into(), url: "{{base}}/orders".into(), ..Default::default() };
        save_request(&mut c, &r, Some(&fid)).unwrap();
        r.method = "POST".into();
        save_request(&mut c, &r, Some(&fid)).unwrap();
        let back = collections(&[]);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].root.folders[0].requests, vec![Item { id: r.id.clone(), name: "List orders".into(), method: "POST".into() }], "saved once, with its method");
        assert_eq!(load_request(&back[0], &r.id).unwrap(), r);
        // A workspace one lives in the project.
        let proj = d.join("proj");
        let w = Collection::new("Local", Some(proj.clone()));
        save_collection(&w).unwrap();
        assert!(base(Some(&proj)).unwrap().join("collections").join(&w.id).join("collection.json").exists(), "kept in the store, under the project");
        assert!(!proj.join(".oskl").exists(), "nothing written into the project");
        assert_eq!(collections(&[proj.clone()]).len(), 2);
        assert_eq!(collections(&[]).len(), 1, "not in other windows");
        let copy = duplicate_request(&mut c, &r.id).unwrap();
        assert_eq!(c.root.folders[0].requests.len(), 2);
        assert_eq!(copy.name, "List orders copy");
        let root = c.root.id.clone();
        move_request(&mut c, &copy.id, &root).unwrap();
        assert_eq!(c.root.requests.len(), 1);
        delete_folder(&mut c, &fid).unwrap();
        assert!(load_request(&c, &r.id).is_none(), "its requests go with it");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn data_kept_in_a_project_moves_to_the_store() {
        let d = temp();
        let proj = d.join("old-proj");
        let old = proj.join(".oskl/api/collections/c1");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("collection.json"), r#"{"id":"c1","root":{"id":"c1","name":"Legacy"}}"#).unwrap();
        let found = collections(&[proj.clone()]);
        assert_eq!(found.iter().map(|c| c.name()).collect::<Vec<_>>(), ["Legacy"]);
        assert!(!proj.join(".oskl").exists(), "moved out of the project");
        assert!(d.join("global/projects").read_dir().unwrap().next().unwrap().unwrap().file_name().to_string_lossy().starts_with("old-proj-"));
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn environments_keep_secrets_out_of_the_file() {
        let d = temp();
        let mut e = Environment { id: new_id(), name: "Dev".into(), ..Default::default() };
        e.vars.push(Row::new("base", "http://localhost:3000"));
        e.vars.push(Row { flag: true, ..Row::new("token", "s3cret") });
        save_environment(&e).unwrap();
        let text = std::fs::read_to_string(d.join("global/environments.json")).unwrap();
        assert!(text.contains("localhost") && !text.contains("s3cret"), "{text}");
        let all = environments(&[]);
        assert_eq!(all[0].name, "Global", "a global one, first");
        let dev = all.iter().find(|x| x.id == e.id).unwrap();
        assert_eq!(with_secrets(dev).vars[1].value, "s3cret");
        set_active_env(None, Some(&e.id));
        assert_eq!(active_env(None).as_deref(), Some(e.id.as_str()));
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn history_is_capped_newest_first() {
        let d = temp();
        for i in 0..5 {
            add_history(&Visit { id: format!("v{i}"), at: now_secs(), status: 200, request: Request { url: format!("http://x/{i}"), ..Default::default() }, ..Default::default() });
        }
        add_history(&Visit { id: "old".into(), at: 1000, ..Default::default() });
        let h = history();
        assert_eq!(h.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["v4", "v3", "v2", "v1", "v0"], "newest first, the too-old one dropped");
        delete_history(Some("v3"));
        assert_eq!(history().len(), 4);
        delete_history(None);
        assert!(history().is_empty());
        std::fs::remove_dir_all(d).ok();
    }
}
