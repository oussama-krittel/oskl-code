//! Live Server: a small static file server on 127.0.0.1 for a folder of HTML, CSS and JS, and
//! a browser that reloads itself when a file in it changes (Server-Sent Events). No browser
//! inside the app and nothing added to it: std's networking, one thread taking connections
//! (each request answered on its own short-lived one), and the file watcher the app already
//! uses. Stopped (dropped), the thread, the watcher and the browsers' connections all go.

use anyhow::{Context as _, Result, bail};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the browsers listen for "reload".
const RELOAD_PATH: &str = "/__live_reload";
/// The first port tried, as in VS Code's Live Server; the next free one after it otherwise.
pub const DEFAULT_PORT: u16 = 5500;

/// Folders whose changes never reload the page (dependencies, builds, version control).
const IGNORED_DIRS: &[&str] = &["node_modules", ".git", "dist", "build", ".next", "target", ".cache", ".parcel-cache", ".svelte-kit", "coverage"];

/// Put into every HTML page served: reloads it when the server says so.
const CLIENT: &str = "<script>(()=>{const s=new EventSource('/__live_reload');s.onmessage=e=>{if(e.data==='reload')location.reload()};})();</script>";

pub struct LiveServer {
    pub root: PathBuf,
    pub port: u16,
    /// The page opened first, as a URL path ("/pages/dashboard.html").
    pub entry: String,
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl LiveServer {
    /// Serves `root` on the first free port from 5500; `entry`: the file to open first.
    pub fn start(root: PathBuf, entry: Option<&Path>) -> Result<Self> {
        let root = root.canonicalize().with_context(|| format!("{} isn't a folder that can be served", root.display()))?;
        if !root.is_dir() {
            bail!("{} isn't a folder", root.display());
        }
        let (listener, port) = bind_from(DEFAULT_PORT)?;
        let entry = match entry {
            Some(e) => url_path(&root, e).unwrap_or_else(|| "/".into()),
            None => "/".into(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let clients: Arc<Mutex<Vec<TcpStream>>> = Default::default();
        {
            let (root, stop, clients) = (root.clone(), stop.clone(), clients.clone());
            std::thread::Builder::new().name("live-server".into()).spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let (root, clients) = (root.clone(), clients.clone());
                    let _ = std::thread::Builder::new().name("live-server-request".into()).spawn(move || {
                        let _ = handle(conn, &root, &clients);
                    });
                }
            })?;
        }
        let watcher = watch(&root, clients.clone());
        Ok(Self { root, port, entry, stop, clients, _watcher: watcher })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// The first page's address.
    pub fn entry_url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.entry)
    }

    /// The address of `file` in it, if it's inside the root.
    pub fn url_of(&self, file: &Path) -> Option<String> {
        url_path(&self.root, file).map(|p| format!("http://127.0.0.1:{}{p}", self.port))
    }

    /// Browsers connected (waiting to reload).
    #[cfg(test)]
    pub fn connected(&self) -> usize {
        self.clients.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Tells every browser to reload (what a file change does).
    #[cfg(test)]
    pub fn reload(&self) {
        broadcast(&self.clients);
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The thread waits in accept: one last connection lets it see it's stopped.
        let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_millis(200));
        if let Ok(mut c) = self.clients.lock() {
            for s in c.drain(..) {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

/// The first free port from `first`, on 127.0.0.1 only (no one else can reach it).
fn bind_from(first: u16) -> Result<(TcpListener, u16)> {
    for port in first..first.saturating_add(100) {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            return Ok((l, port));
        }
    }
    bail!("No free port from {first} to {}", first.saturating_add(99))
}

/// `file`'s URL path under `root` ("/pages/a%20b.html"), if it's in it.
fn url_path(root: &Path, file: &Path) -> Option<String> {
    let file = file.canonicalize().ok()?;
    let rel = file.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| encode(&c.as_os_str().to_string_lossy())).collect();
    Some(format!("/{}", parts.join("/")))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file a request's path names, only if it's inside `root` (no "..", no link out of it).
pub fn resolve(root: &Path, url: &str) -> Option<PathBuf> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let path = decode(path);
    let mut out = root.to_path_buf();
    for c in Path::new(path.trim_start_matches('/')).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    // A link inside the root that leads out of it is refused too.
    let real = out.canonicalize().ok()?;
    real.starts_with(root).then_some(real)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn respond(conn: &mut TcpStream, status: &str, kind: &str, body: &[u8], head_only: bool) -> std::io::Result<()> {
    write!(conn, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n", body.len())?;
    if !head_only {
        conn.write_all(body)?;
    }
    conn.flush()
}

/// One request: a file, the folder's index.html, the reload stream, or 404.
fn handle(mut conn: TcpStream, root: &Path, clients: &Mutex<Vec<TcpStream>>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    // The rest of the head isn't needed: read up to the blank line.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    if method != "GET" && method != "HEAD" {
        return respond(&mut conn, "405 Method Not Allowed", "text/plain", b"Only GET and HEAD", false);
    }
    let head_only = method == "HEAD";
    if target.split('?').next() == Some(RELOAD_PATH) {
        conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n: connected\n\n")?;
        conn.flush()?;
        conn.set_read_timeout(None)?;
        if let Ok(mut c) = clients.lock() {
            c.push(conn);
        }
        return Ok(());
    }
    let Some(mut file) = resolve(root, target) else {
        return respond(&mut conn, "404 Not Found", "text/html; charset=utf-8", not_found(target).as_bytes(), head_only);
    };
    if file.is_dir() {
        // A folder without its closing slash: its relative links need it.
        let path = target.split('?').next().unwrap_or("/");
        if !path.ends_with('/') {
            let to = format!("{path}/");
            write!(conn, "HTTP/1.1 301 Moved Permanently\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return conn.flush();
        }
        file = file.join("index.html");
        if !file.is_file() {
            return respond(&mut conn, "404 Not Found", "text/html; charset=utf-8", not_found(target).as_bytes(), head_only);
        }
    }
    let kind = mime(&file);
    if kind.starts_with("text/html") {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        return respond(&mut conn, "200 OK", kind, inject(&text).as_bytes(), head_only);
    }
    let Ok(mut f) = std::fs::File::open(&file) else {
        return respond(&mut conn, "404 Not Found", "text/html; charset=utf-8", not_found(target).as_bytes(), head_only);
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {len}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")?;
    if !head_only {
        // Streamed, not read whole: a big video costs no memory.
        std::io::copy(&mut (&mut f).take(len), &mut conn)?;
    }
    conn.flush()
}

fn not_found(target: &str) -> String {
    let t = target.replace('&', "&amp;").replace('<', "&lt;");
    format!("<!doctype html><title>404</title><body style=\"font-family:system-ui;padding:2em\"><h1>404</h1><p>Nothing at <code>{t}</code> in the served folder.</p></body>")
}

/// The page with the reload script before `</body>` (or at its end).
fn inject(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    match lower.rfind("</body>") {
        Some(i) => format!("{}{CLIENT}{}", &html[..i], &html[i..]),
        None => format!("{html}{CLIENT}"),
    }
}

/// Tells every connected browser to reload, forgetting the ones that went away.
fn broadcast(clients: &Mutex<Vec<TcpStream>>) {
    if let Ok(mut c) = clients.lock() {
        c.retain_mut(|s| s.write_all(b"data: reload\n\n").and_then(|_| s.flush()).is_ok());
    }
}

/// A change under the root that should reload the page: not in a dependency, build or hidden
/// folder, not an editor's temporary file.
fn matters(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else { return false };
    let names: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let Some(last) = names.last() else { return false };
    if names.iter().any(|n| IGNORED_DIRS.contains(&n.as_str()) || (n.starts_with('.') && n.len() > 1)) {
        return false;
    }
    !(last.ends_with('~') || last.ends_with(".swp") || last.ends_with(".tmp") || last.starts_with(".#") || last.contains(".oskl-tmp") || last.ends_with(".crswap"))
}

/// Watches the root (file system events, no polling): changes that matter reload the browsers,
/// a burst of them (a save, a build) once.
fn watch(root: &Path, clients: Arc<Mutex<Vec<TcpStream>>>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let base = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !matches!(ev.kind, notify::EventKind::Access(_))
            && ev.paths.iter().any(|p| matters(&base, p))
        {
            let _ = tx.send(());
        }
    })
    .ok()?;
    watcher.watch(root, notify::RecursiveMode::Recursive).ok()?;
    // Ends when the watcher (and so the sender) goes.
    let _ = std::thread::Builder::new().name("live-server-reload".into()).spawn(move || {
        while rx.recv().is_ok() {
            std::thread::sleep(Duration::from_millis(120));
            while rx.try_recv().is_ok() {}
            broadcast(&clients);
        }
    });
    Some(watcher)
}

/// The HTML files under `root` (not in dependency or hidden folders), nearest first, at most `max`.
pub fn html_files(root: &Path, max: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = dirs.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            if p.is_dir() {
                if depth < 4 {
                    dirs.push_back((p, depth + 1));
                }
            } else if is_html(&p) {
                out.push(p);
                if out.len() >= max {
                    return out;
                }
            }
        }
    }
    out
}

/// Opens `target` (a file or an address) in the default browser. In tests it's only noted.
pub fn open_external(target: &str) -> std::io::Result<()> {
    #[cfg(test)]
    {
        OPENED.with(|o| o.borrow_mut().push(target.to_string()));
        Ok(())
    }
    #[cfg(not(test))]
    {
        std::process::Command::new("open").arg(target).spawn().map(|_| ())
    }
}

#[cfg(test)]
thread_local! {
    /// What `open_external` was asked to open.
    pub static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn is_html(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("html" | "htm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, path: &str) -> (String, Vec<u8>) {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(c, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut all = Vec::new();
        c.read_to_end(&mut all).unwrap();
        let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&all[..at]).into_owned(), all[at + 4..].to_vec())
    }

    #[test]
    fn serves_pages_and_assets_inside_its_root_and_reloads_on_change() {
        let base = std::env::temp_dir().join(format!("oskl-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("site");
        std::fs::create_dir_all(root.join("pages")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("index.html"), "<html><body><h1>Home</h1></body></html>").unwrap();
        std::fs::write(root.join("pages/about me.html"), "<p>About</p>").unwrap();
        std::fs::write(root.join("style.css"), "h1{color:red}").unwrap();
        std::fs::write(root.join("logo.png"), [137u8, 80, 78, 71]).unwrap();
        std::fs::write(base.join("secret.txt"), "no").unwrap();

        // Something already on 5500 (or its neighbours): the next free port.
        let _taken = TcpListener::bind(("127.0.0.1", DEFAULT_PORT));
        let s = LiveServer::start(root.clone(), Some(&root.join("pages/about me.html"))).unwrap();
        assert!(s.port >= DEFAULT_PORT);
        assert_eq!(s.entry, "/pages/about%20me.html");

        let (head, body) = get(s.port, "/");
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("text/html"), "{head}");
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("<h1>Home</h1>") && body.contains("/__live_reload"), "index.html at /, with the reload script");
        assert!(body.find("__live_reload").unwrap() < body.find("</body>").unwrap());
        let (head, _) = get(s.port, "/pages/about%20me.html?x=1#top");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let (head, body) = get(s.port, "/style.css");
        assert!(head.contains("text/css") && body == b"h1{color:red}");
        let (head, body) = get(s.port, "/logo.png");
        assert!(head.contains("image/png") && body.len() == 4);
        let (head, _) = get(s.port, "/pages");
        assert!(head.starts_with("HTTP/1.1 301") && head.contains("Location: /pages/"), "{head}");
        let (head, _) = get(s.port, "/missing.js");
        assert!(head.starts_with("HTTP/1.1 404"));
        // Nothing outside the root, however it's asked for.
        for bad in ["/../secret.txt", "/%2e%2e/secret.txt", "/pages/../../secret.txt", "/..%2fsecret.txt"] {
            let (head, body) = get(s.port, bad);
            assert!(head.starts_with("HTTP/1.1 404") && !String::from_utf8_lossy(&body).contains("no"), "{bad}: {head}");
        }

        // A browser waiting on the reload stream hears "reload".
        let mut sse = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        write!(sse, "GET /__live_reload HTTP/1.1\r\n\r\n").unwrap();
        sse.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 256];
        let n = sse.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("text/event-stream"));
        for _ in 0..50 {
            if s.connected() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        s.reload();
        let n = sse.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("data: reload"));

        // What reloads and what doesn't.
        assert!(matters(&s.root, &s.root.join("style.css")));
        assert!(!matters(&s.root, &s.root.join("node_modules/x/a.js")));
        assert!(!matters(&s.root, &s.root.join(".git/index")));
        assert!(!matters(&s.root, &s.root.join("page.html~")));

        // Stopped: the port is free again and the browsers' streams end.
        let port = s.port;
        drop(s);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sse.read(&mut buf).unwrap_or(0), 0, "stream closed");
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok(), "port given back");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn finds_html_files_but_not_in_dependencies() {
        let root = std::env::temp_dir().join(format!("oskl-live-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("pages")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.html"), "").unwrap();
        std::fs::write(root.join("pages/a.html"), "").unwrap();
        std::fs::write(root.join("b.htm"), "").unwrap();
        let found: Vec<String> = html_files(&root, 10).iter().map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(found, ["b.htm", "pages/a.html"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
