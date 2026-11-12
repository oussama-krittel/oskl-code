//! ⌘-click in JavaScript and TypeScript, answered by TypeScript itself, as in VS Code: the
//! project's own TypeScript (from its node_modules, else one installed globally). It knows types,
//! so `this.service.method`, `dto.field`, path aliases, `.js` imports of `.ts` files and package
//! exports all go where VS Code goes.
//!
//! TypeScript 6 and older answer through `tsserver.js` (its own protocol); TypeScript 7 (native)
//! through its language server (`tsc --lsp --stdio`). One server per TypeScript install, started
//! on the first ⌘ over a name and stopped after ten idle minutes (it holds the project in
//! memory). Without TypeScript or Node, callers fall back to the guesser in `definition`.

use crate::definition::Target;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];
const IDLE: Duration = Duration::from_secs(600);
/// The first question waits while TypeScript loads the project.
const WAIT: Duration = Duration::from_secs(45);

/// Whether TypeScript answers for this file.
pub fn handles(path: &Path) -> bool {
    path.extension().is_some_and(|e| EXTS.contains(&e.to_string_lossy().as_ref()))
}

/// What's at `pos` (a character index in `text`, the file's current content) leads to: the span
/// to underline (character range; None: the word) and where it goes. `Ok(None)`: nothing, as VS
/// Code shows no link there. `Err`: TypeScript couldn't be asked.
pub fn definition(root: Option<&Path>, file: &Path, text: &str, pos: usize) -> Result<Option<(Option<Range<usize>>, Target)>, String> {
    let project = root.or(file.parent()).unwrap_or(file);
    let server = server_for(file, project)?;
    server.sync(file, text, project)?;
    let (row, units) = to_units(text, pos);
    let (target, start, span) = match server.proto {
        Proto::Classic => {
            let body = server.ask("definitionAndBoundSpan", json!({ "file": file, "line": row + 1, "offset": units + 1 }))?;
            let Some(def) = body["definitions"].as_array().and_then(|d| d.first()).cloned() else { return Ok(None) };
            let Some(path) = def["file"].as_str().map(PathBuf::from) else { return Ok(None) };
            let at = |v: &Value| Some((v["line"].as_u64()?.checked_sub(1)? as usize, v["offset"].as_u64()?.checked_sub(1)? as usize));
            let span = at(&body["textSpan"]["start"]).zip(at(&body["textSpan"]["end"]));
            (path, at(&def["start"]), span)
        }
        Proto::Lsp => {
            let r = server.ask("textDocument/definition", json!({ "textDocument": { "uri": to_uri(file) }, "position": { "line": row, "character": units } }))?;
            let first = match &r {
                Value::Array(a) => a.first().cloned(),
                Value::Object(_) => Some(r.clone()),
                _ => None,
            };
            let Some(d) = first else { return Ok(None) };
            let at = |v: &Value| Some((v["line"].as_u64()? as usize, v["character"].as_u64()? as usize));
            if let Some(uri) = d["targetUri"].as_str() {
                let span = at(&d["originSelectionRange"]["start"]).zip(at(&d["originSelectionRange"]["end"]));
                (from_uri(uri), at(&d["targetSelectionRange"]["start"]), span)
            } else {
                let Some(uri) = d["uri"].as_str() else { return Ok(None) };
                (from_uri(uri), at(&d["range"]["start"]), None)
            }
        }
    };
    let span = span.and_then(|(a, b)| Some(from_units(text, a)?..from_units(text, b)?)).filter(|s| s.start <= pos && pos <= s.end && s.start < s.end);
    // The target's line and column, in characters.
    let (row, col) = match start {
        Some((r, u)) => {
            let t = if target == file { Some(text.to_string()) } else { std::fs::read_to_string(&target).ok() };
            (r, t.and_then(|t| t.split('\n').nth(r).map(|l| units_to_chars(l, u))).unwrap_or(0))
        }
        None => (0, 0),
    };
    Ok(Some((span, Target { path: target, row, col })))
}

/// A character index → (0-based line, UTF-16 units into it), how TypeScript counts.
fn to_units(text: &str, pos: usize) -> (usize, usize) {
    let (mut row, mut units) = (0, 0);
    for c in text.chars().take(pos) {
        if c == '\n' {
            row += 1;
            units = 0;
        } else {
            units += c.len_utf16();
        }
    }
    (row, units)
}

fn units_to_chars(line: &str, units: usize) -> usize {
    let mut n = 0;
    let mut col = 0;
    for c in line.chars() {
        if n >= units {
            break;
        }
        n += c.len_utf16();
        col += 1;
    }
    col
}

/// (0-based line, UTF-16 units) → a character index in `text`.
fn from_units(text: &str, (row, units): (usize, usize)) -> Option<usize> {
    let line = text.split('\n').nth(row)?;
    let start: usize = text.split('\n').take(row).map(|l| l.chars().count() + 1).sum();
    Some(start + units_to_chars(line, units))
}

fn to_uri(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn from_uri(uri: &str) -> PathBuf {
    let s = uri.strip_prefix("file://").unwrap_or(uri).as_bytes();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && let Some(b) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    PathBuf::from(String::from_utf8_lossy(&out).to_string())
}

// ------------------------------------------------------------ finding TypeScript

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Proto {
    /// `node tsserver.js` (TypeScript 6 and older).
    Classic,
    /// `tsc --lsp --stdio` (TypeScript 7).
    Lsp,
}

/// How to start a TypeScript: the protocol, the program and its arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Launch {
    proto: Proto,
    program: PathBuf,
    args: Vec<String>,
}

/// The project's TypeScript (the nearest node_modules up from the file), else a global one.
fn find_typescript(file: &Path) -> Option<Launch> {
    let mut d = file.parent();
    while let Some(dir) = d {
        let pkg = dir.join("node_modules/typescript");
        if pkg.join("package.json").is_file()
            && let Some(l) = launch_for(&pkg)
        {
            return Some(l);
        }
        d = dir.parent();
    }
    // `npm i -g typescript`: its bin links into the package.
    for bin in ["tsserver", "tsc"] {
        if let Some(p) = crate::tools::search_path().into_iter().map(|p| p.join(bin)).find(|p| p.is_file())
            && let Ok(real) = std::fs::canonicalize(p)
            && let Some(pkg) = real.parent().and_then(Path::parent)
            && pkg.join("package.json").is_file()
            && let Some(l) = launch_for(pkg)
        {
            return Some(l);
        }
    }
    None
}

fn launch_for(pkg: &Path) -> Option<Launch> {
    let node = || crate::tools::search_path().into_iter().map(|p| p.join("node")).find(|p| p.is_file());
    let classic = pkg.join("lib/tsserver.js");
    if classic.is_file() {
        return Some(Launch { proto: Proto::Classic, program: node()?, args: vec![classic.to_string_lossy().into()] });
    }
    // TypeScript 7: the native compiler, from @typescript/typescript-<os>-<arch> beside it.
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        a => a,
    };
    let native = format!("@typescript/typescript-{}-{arch}/lib/tsc", std::env::consts::OS);
    let own = pkg.join("node_modules").join(&native);
    if own.is_file() {
        return Some(Launch { proto: Proto::Lsp, program: own, args: vec!["--lsp".into(), "--stdio".into()] });
    }
    let mut d = pkg.parent();
    while let Some(dir) = d {
        for p in [dir.join(&native), dir.join("node_modules").join(&native)] {
            if p.is_file() {
                return Some(Launch { proto: Proto::Lsp, program: p, args: vec!["--lsp".into(), "--stdio".into()] });
            }
        }
        d = dir.parent();
    }
    let bin = pkg.join("bin/tsc");
    if bin.is_file() {
        return Some(Launch { proto: Proto::Lsp, program: node()?, args: vec![bin.to_string_lossy().into(), "--lsp".into(), "--stdio".into()] });
    }
    None
}

// ------------------------------------------------------------ the server

struct Server {
    proto: Proto,
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    state: Mutex<State>,
    replies: Arc<(Mutex<HashMap<u64, Value>>, Condvar)>,
    alive: Arc<Mutex<bool>>,
    used: Mutex<Instant>,
}

struct State {
    seq: u64,
    /// What TypeScript has for each file we've told it about (the editor's text, not the disk's),
    /// and its version.
    open: HashMap<PathBuf, (i64, String)>,
}

fn servers() -> &'static Mutex<HashMap<Launch, Arc<Server>>> {
    static S: OnceLock<Mutex<HashMap<Launch, Arc<Server>>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// TypeScript installs that wouldn't start (not tried again).
fn broken() -> &'static Mutex<HashSet<Launch>> {
    static B: OnceLock<Mutex<HashSet<Launch>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn server_for(file: &Path, project: &Path) -> Result<Arc<Server>, String> {
    let launch = find_typescript(file).ok_or("TypeScript isn't installed")?;
    if broken().lock().unwrap().contains(&launch) {
        return Err("TypeScript didn't start".into());
    }
    let mut all = servers().lock().unwrap();
    if let Some(s) = all.get(&launch).filter(|s| *s.alive.lock().unwrap()) {
        return Ok(s.clone());
    }
    match Server::start(&launch, project) {
        Ok(s) => {
            let s = Arc::new(s);
            all.insert(launch.clone(), s.clone());
            reap_when_idle(launch, s.clone());
            Ok(s)
        }
        Err(e) => {
            broken().lock().unwrap().insert(launch);
            Err(e)
        }
    }
}

/// Stops the server after ten minutes without a question (TypeScript holds the whole project).
fn reap_when_idle(key: Launch, s: Arc<Server>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(60));
            if !*s.alive.lock().unwrap() {
                break;
            }
            if s.used.lock().unwrap().elapsed() > IDLE {
                s.stop();
                break;
            }
        }
        let mut all = servers().lock().unwrap();
        if all.get(&key).is_some_and(|x| Arc::ptr_eq(x, &s)) {
            all.remove(&key);
        }
    });
}

fn write_msg(stdin: &Mutex<ChildStdin>, proto: Proto, msg: &Value) -> Result<(), String> {
    let body = msg.to_string();
    let out = match proto {
        Proto::Classic => body + "\n",
        Proto::Lsp => format!("Content-Length: {}\r\n\r\n{body}", body.len()),
    };
    let mut w = stdin.lock().unwrap();
    w.write_all(out.as_bytes()).and_then(|_| w.flush()).map_err(|e| e.to_string())
}

impl Server {
    fn start(l: &Launch, project: &Path) -> Result<Server, String> {
        let mut cmd = crate::tools::command(&l.program);
        cmd.args(&l.args).current_dir(project).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if l.proto == Proto::Classic {
            cmd.args(["--disableAutomaticTypingAcquisition", "--suppressDiagnosticEvents", "--noGetErrOnBackgroundUpdate", "--locale", "en"]);
        }
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or("no stdin")?));
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let replies: Arc<(Mutex<HashMap<u64, Value>>, Condvar)> = Default::default();
        let alive = Arc::new(Mutex::new(true));
        {
            let (replies, alive, stdin, proto) = (replies.clone(), alive.clone(), stdin.clone(), l.proto);
            std::thread::spawn(move || {
                read_messages(stdout, proto, &stdin, &replies);
                *alive.lock().unwrap() = false;
                replies.1.notify_all();
            });
        }
        let s = Server { proto: l.proto, child: Mutex::new(child), stdin, state: Mutex::new(State { seq: 0, open: HashMap::new() }), replies, alive, used: Mutex::new(Instant::now()) };
        let started = match l.proto {
            // Plain JavaScript files outside any tsconfig too, as in VS Code.
            Proto::Classic => s.send(
                "compilerOptionsForInferredProjects",
                json!({ "options": { "allowJs": true, "checkJs": false, "jsx": "preserve", "module": "esnext", "moduleResolution": "bundler", "target": "es2022", "allowSyntheticDefaultImports": true, "allowNonTsExtensions": true, "resolveJsonModule": true } }),
            ),
            Proto::Lsp => {
                let root = to_uri(project);
                let caps = json!({ "textDocument": { "definition": { "linkSupport": true } }, "workspace": { "workspaceFolders": true, "configuration": true } });
                s.ask("initialize", json!({ "processId": std::process::id(), "rootUri": root, "capabilities": caps, "workspaceFolders": [{ "uri": root, "name": "project" }] }))
                    .and_then(|_| s.notify("initialized", json!({})))
            }
        };
        if let Err(e) = started {
            s.stop();
            return Err(e);
        }
        Ok(s)
    }

    fn stop(&self) {
        let mut c = self.child.lock().unwrap();
        c.kill().ok();
        c.wait().ok();
        *self.alive.lock().unwrap() = false;
    }

    fn next_seq(&self) -> u64 {
        let mut st = self.state.lock().unwrap();
        st.seq += 1;
        st.seq
    }

    /// A tsserver request whose answer isn't waited for.
    fn send(&self, command: &str, args: Value) -> Result<(), String> {
        let seq = self.next_seq();
        write_msg(&self.stdin, self.proto, &json!({ "seq": seq, "type": "request", "command": command, "arguments": args }))
    }

    /// A language-server notification.
    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        write_msg(&self.stdin, self.proto, &json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Asks and waits for the answer.
    fn ask(&self, command: &str, args: Value) -> Result<Value, String> {
        *self.used.lock().unwrap() = Instant::now();
        let seq = self.next_seq();
        let msg = match self.proto {
            Proto::Classic => json!({ "seq": seq, "type": "request", "command": command, "arguments": args }),
            Proto::Lsp => json!({ "jsonrpc": "2.0", "id": seq, "method": command, "params": args }),
        };
        write_msg(&self.stdin, self.proto, &msg)?;
        let (lock, ready) = &*self.replies;
        let start = Instant::now();
        let mut map = lock.lock().unwrap();
        loop {
            if let Some(r) = map.remove(&seq) {
                return match self.proto {
                    Proto::Classic if r["success"].as_bool() == Some(true) => Ok(r["body"].clone()),
                    Proto::Classic => Err(r["message"].as_str().unwrap_or("TypeScript failed").to_string()),
                    Proto::Lsp if r.get("error").is_some() => Err(r["error"]["message"].as_str().unwrap_or("TypeScript failed").to_string()),
                    Proto::Lsp => Ok(r["result"].clone()),
                };
            }
            if !*self.alive.lock().unwrap() {
                return Err("TypeScript stopped".into());
            }
            let left = WAIT.checked_sub(start.elapsed()).ok_or("TypeScript is still loading the project")?;
            map = ready.wait_timeout(map, left).unwrap().0;
        }
    }

    /// Gives TypeScript the editor's text of `file` (unsaved changes included).
    fn sync(&self, file: &Path, text: &str, project: &Path) -> Result<(), String> {
        let known = self.state.lock().unwrap().open.get(file).cloned();
        let version = match &known {
            Some((_, old)) if old == text => return Ok(()),
            Some((v, _)) => v + 1,
            None => 1,
        };
        // The end of the old text, to replace all of it.
        let end = |old: &str| (old.split('\n').count(), old.rsplit('\n').next().unwrap_or("").encode_utf16().count());
        match (self.proto, known) {
            (Proto::Classic, Some((_, old))) => {
                let (lines, last) = end(&old);
                self.send(
                    "updateOpen",
                    json!({ "changedFiles": [{ "fileName": file, "textChanges": [{ "start": { "line": 1, "offset": 1 }, "end": { "line": lines, "offset": last + 1 }, "newText": text }] }] }),
                )?;
            }
            (Proto::Classic, None) => self.send("updateOpen", json!({ "openFiles": [{ "file": file, "fileContent": text, "projectRootPath": project }] }))?,
            (Proto::Lsp, Some((_, old))) => {
                let (lines, last) = end(&old);
                self.notify(
                    "textDocument/didChange",
                    json!({ "textDocument": { "uri": to_uri(file), "version": version }, "contentChanges": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": lines - 1, "character": last } }, "text": text }] }),
                )?;
            }
            (Proto::Lsp, None) => {
                let lang = match file.extension().and_then(|e| e.to_str()) {
                    Some("tsx") => "typescriptreact",
                    Some("jsx") => "javascriptreact",
                    Some("js" | "mjs" | "cjs") => "javascript",
                    _ => "typescript",
                };
                self.notify("textDocument/didOpen", json!({ "textDocument": { "uri": to_uri(file), "languageId": lang, "version": version, "text": text } }))?;
            }
        }
        self.state.lock().unwrap().open.insert(file.to_path_buf(), (version, text.to_string()));
        Ok(())
    }
}

/// `Content-Length: n` + blank line + n bytes of JSON, over and over. Answers to questions are
/// kept for whoever is waiting; the language server's own questions get empty answers; events
/// are dropped.
fn read_messages(stdout: impl Read, proto: Proto, stdin: &Mutex<ChildStdin>, replies: &(Mutex<HashMap<u64, Value>>, Condvar)) {
    let mut r = BufReader::new(stdout);
    loop {
        let mut len = None;
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let l = line.trim();
            if l.is_empty() {
                if len.is_some() {
                    break;
                }
                continue;
            }
            if let Some(n) = l.strip_prefix("Content-Length:") {
                len = n.trim().parse::<usize>().ok();
            }
        }
        let mut body = vec![0; len.unwrap_or(0)];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&body) else { continue };
        let answer = match proto {
            Proto::Classic if msg["type"] == "response" && msg["command"] != "updateOpen" && msg["command"] != "compilerOptionsForInferredProjects" => msg["request_seq"].as_u64(),
            Proto::Classic => None,
            Proto::Lsp => match (msg.get("id"), msg.get("method")) {
                // The server asks something (settings, progress…): answer so it carries on.
                (Some(id), Some(method)) => {
                    let result = if method == "workspace/configuration" { json!(vec![Value::Null; msg["params"]["items"].as_array().map_or(0, Vec::len)]) } else { Value::Null };
                    write_msg(stdin, proto, &json!({ "jsonrpc": "2.0", "id": id, "result": result })).ok();
                    None
                }
                (Some(id), None) => id.as_u64(),
                _ => None,
            },
        };
        if let Some(seq) = answer {
            replies.0.lock().unwrap().insert(seq, msg);
            replies.1.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_in_utf16_and_uris() {
        let text = "a😀b\nxy";
        assert_eq!(to_units(text, 2), (0, 3), "the emoji is two UTF-16 units");
        assert_eq!(to_units(text, 5), (1, 1));
        assert_eq!(from_units(text, (0, 3)), Some(2));
        assert_eq!(from_units(text, (1, 1)), Some(5));
        let p = Path::new("/Users/me/My code/ä.ts");
        assert_eq!(to_uri(p), "file:///Users/me/My%20code/%C3%A4.ts");
        assert_eq!(from_uri(&to_uri(p)), p);
    }

    /// The real thing: with OSKL_TEST_TYPESCRIPT set to a node_modules that has TypeScript in it
    /// (6 or older, or 7), else a global install; skipped without one.
    #[test]
    fn typescript_answers_like_vs_code() {
        let d = std::env::temp_dir().join(format!("oskl-tsserver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/services")).unwrap();
        std::fs::create_dir_all(d.join("src/models/dto")).unwrap();
        let file = d.join("src/controller.ts");
        if let Some(nm) = std::env::var_os("OSKL_TEST_TYPESCRIPT") {
            std::os::unix::fs::symlink(nm, d.join("node_modules")).unwrap();
        }
        if find_typescript(&file).is_none() {
            eprintln!("no TypeScript here: skipped");
            return;
        }
        std::fs::write(d.join("tsconfig.json"), r##"{ "compilerOptions": { "module": "nodenext", "moduleResolution": "nodenext", "strict": true, "paths": { "@app/*": ["./src/*"] } } }"##).unwrap();
        std::fs::write(d.join("package.json"), r#"{ "type": "module" }"#).unwrap();
        std::fs::write(d.join("src/services/order.service.ts"), "export class OrderService {\n  async listOrders(q: string): Promise<string> {\n    return q;\n  }\n}\n").unwrap();
        std::fs::write(d.join("src/models/dto/order.dto.ts"), "export class ListOrdersQueryDto {\n  page?: number;\n}\n").unwrap();
        std::fs::write(d.join("src/models/dto/index.ts"), "export * from './order.dto.js';\n").unwrap();
        let text = "import { OrderService } from '@app/services/order.service.js';\nimport { ListOrdersQueryDto } from './models/dto/index.js';\n\nexport class C {\n  constructor(private readonly orderService: OrderService) {}\n  run(query: ListOrdersQueryDto) {\n    return this.orderService.listOrders(String(query.page));\n  }\n}\n";
        // On disk, as files in a project are (the editor sends its own, unsaved text).
        std::fs::write(&file, text).unwrap();
        let at = |t: &str, needle: &str, nth: usize| t[..t.match_indices(needle).nth(nth).unwrap().0].chars().count() + 1;
        let go = |t: &str, pos: usize| definition(Some(&d), &file, t, pos).unwrap().map(|(_, t)| (t.path.file_name().unwrap().to_string_lossy().to_string(), t.row, t.col));
        assert_eq!(go(text, at(text, "listOrders", 0)), Some(("order.service.ts".into(), 1, 8)), "a method through an injected service");
        assert_eq!(go(text, at(text, "page", 0)), Some(("order.dto.ts".into(), 1, 2)), "a field through a parameter's type");
        assert_eq!(go(text, at(text, "ListOrdersQueryDto", 0)), Some(("order.dto.ts".into(), 0, 13)), "through index.js → index.ts and export *");
        assert_eq!(go(text, at(text, "OrderService", 0)), Some(("order.service.ts".into(), 0, 13)), "a path alias");
        assert_eq!(go(text, at(text, "orderService", 1)), Some(("controller.ts".into(), 4, 31)), "this.orderService: the constructor's property");
        assert_eq!(go(text, at(text, "./models", 0) + 1).map(|t| t.0), Some("index.ts".into()), "the import's path opens the module");
        // Unsaved changes count: a method typed in (not saved) is found.
        let edited = text.replace("  run(", "  other() { return 1; }\n  run(").replace("String(query.page)", "String(this.other())");
        assert_eq!(go(&edited, at(&edited, "other", 1)), Some(("controller.ts".into(), 5, 2)), "a method typed but not saved");
        std::fs::remove_dir_all(&d).ok();
    }
}
