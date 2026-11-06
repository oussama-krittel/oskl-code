//! Live connections: WebSocket (send and receive messages) and Server-Sent Events, and a
//! GraphQL server's schema (introspection) for the query editor.

use super::send::Ready;
use anyhow::{Result, anyhow};
use futures::channel::mpsc;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Connected; the server's answer's status and headers.
    Open { status: u16, headers: Vec<(String, String)> },
    /// A message from the server (WebSocket), or an event (SSE: its type and id).
    Message { text: String, binary: bool, event: String, id: String, ms: u64 },
    /// A message this end sent.
    Sent { text: String, ms: u64 },
    Closed { reason: String },
    Error(String),
}

/// What the UI sends a live connection.
pub enum Command {
    Text(String),
    Ping,
    Close,
}

/// A running connection: commands go in, events come out; dropping `commands` closes it.
pub struct Live {
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub events: mpsc::UnboundedReceiver<Event>,
}

/// Connects a WebSocket (on the database thread's runtime).
pub fn websocket(ready: Ready) -> Live {
    let (ctx, crx) = tokio::sync::mpsc::unbounded_channel();
    let (etx, erx) = mpsc::unbounded();
    crate::db::engine::runtime().spawn(async move {
        if let Err(e) = run_ws(ready, crx, etx.clone()).await {
            etx.unbounded_send(Event::Error(e.to_string())).ok();
        }
    });
    Live { commands: ctx, events: erx }
}

async fn run_ws(ready: Ready, mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>, events: mpsc::UnboundedSender<Event>) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderName, http::HeaderValue};
    let url = ready.url.replacen("http://", "ws://", 1).replacen("https://", "wss://", 1);
    let mut req = url.as_str().into_client_request().map_err(|e| anyhow!("{url}: {e}"))?;
    for (k, v) in &ready.headers {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            req.headers_mut().insert(k, v);
        }
    }
    match &ready.auth {
        super::Auth::Bearer { token, prefix } => {
            if let Ok(v) = HeaderValue::from_str(&format!("{} {token}", if prefix.is_empty() { "Bearer" } else { prefix })) {
                req.headers_mut().insert("authorization", v);
            }
        }
        super::Auth::Basic { user, pass } => {
            if let Ok(v) = HeaderValue::from_str(&format!("Basic {}", super::send::base64(format!("{user}:{pass}").as_bytes()))) {
                req.headers_mut().insert("authorization", v);
            }
        }
        super::Auth::Apikey { key, value, query: false } => {
            if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(key.as_bytes()), HeaderValue::from_str(value)) {
                req.headers_mut().insert(k, v);
            }
        }
        _ => {}
    }
    let connector = if ready.settings.verify_tls { None } else { native_tls::TlsConnector::builder().danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true).build().ok().map(tokio_tungstenite::Connector::NativeTls) };
    let (stream, resp) = tokio::time::timeout(std::time::Duration::from_secs(15), tokio_tungstenite::connect_async_tls_with_config(req, None, false, connector))
        .await
        .map_err(|_| anyhow!("no answer in 15s"))?
        .map_err(|e| anyhow!("{e}"))?;
    let start = Instant::now();
    events
        .unbounded_send(Event::Open { status: resp.status().as_u16(), headers: resp.headers().iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect() })
        .ok();
    let (mut tx, mut rx) = stream.split();
    loop {
        tokio::select! {
            cmd = commands.recv() => {
                let ms = start.elapsed().as_millis() as u64;
                match cmd {
                    Some(Command::Text(t)) => {
                        tx.send(Message::Text(t.clone().into())).await?;
                        events.unbounded_send(Event::Sent { text: t, ms }).ok();
                    }
                    Some(Command::Ping) => tx.send(Message::Ping(Vec::new().into())).await?,
                    Some(Command::Close) | None => {
                        tx.send(Message::Close(None)).await.ok();
                        events.unbounded_send(Event::Closed { reason: "closed".into() }).ok();
                        return Ok(());
                    }
                }
            }
            msg = rx.next() => {
                let ms = start.elapsed().as_millis() as u64;
                match msg {
                    Some(Ok(Message::Text(t))) => { events.unbounded_send(Event::Message { text: shorten(t.as_str()), binary: false, event: String::new(), id: String::new(), ms }).ok(); }
                    Some(Ok(Message::Binary(b))) => {
                        let text = match std::str::from_utf8(&b) { Ok(s) => shorten(s), Err(_) => format!("<{} bytes>", b.len()) };
                        events.unbounded_send(Event::Message { text, binary: true, event: String::new(), id: String::new(), ms }).ok();
                    }
                    Some(Ok(Message::Close(f))) => {
                        events.unbounded_send(Event::Closed { reason: f.map(|f| format!("{} {}", u16::from(f.code), f.reason)).unwrap_or_else(|| "the server closed it".into()) }).ok();
                        return Ok(());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(anyhow!("{e}")),
                    None => {
                        events.unbounded_send(Event::Closed { reason: "the connection ended".into() }).ok();
                        return Ok(());
                    }
                }
            }
        }
        if events.is_closed() {
            return Ok(());
        }
    }
}

/// The most of one message kept (a huge one is cut, with its size said).
pub const MAX_MESSAGE: usize = 256 * 1024;

fn shorten(s: &str) -> String {
    if s.len() <= MAX_MESSAGE {
        return s.to_string();
    }
    let mut end = MAX_MESSAGE;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes in all)", &s[..end], s.len())
}

/// Listens to Server-Sent Events (a GET with `Accept: text/event-stream`).
pub fn sse(ready: Ready) -> Live {
    let (ctx, mut crx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let (etx, erx) = mpsc::unbounded();
    crate::db::engine::runtime().spawn(async move {
        let work = run_sse(ready, etx.clone());
        tokio::select! {
            r = work => if let Err(e) = r { etx.unbounded_send(Event::Error(e.to_string())).ok(); },
            _ = async { while let Some(c) = crx.recv().await { if matches!(c, Command::Close) { break } } } => {
                etx.unbounded_send(Event::Closed { reason: "closed".into() }).ok();
            }
        }
    });
    Live { commands: ctx, events: erx }
}

async fn run_sse(mut ready: Ready, events: mpsc::UnboundedSender<Event>) -> Result<()> {
    use futures_util::StreamExt;
    if !ready.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
        ready.headers.push(("Accept".into(), "text/event-stream".into()));
    }
    let mut settings = ready.settings.clone();
    settings.timeout_ms = 0;
    let mut b = reqwest::Client::builder().danger_accept_invalid_certs(!settings.verify_tls).connect_timeout(std::time::Duration::from_secs(15));
    if settings.proxy == "none" {
        b = b.no_proxy();
    }
    let client = b.build()?;
    let mut rb = client.request(reqwest::Method::from_bytes(ready.method.as_bytes()).unwrap_or(reqwest::Method::GET), &ready.url);
    for (k, v) in &ready.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    match &ready.auth {
        super::Auth::Bearer { token, prefix } => rb = rb.header("Authorization", format!("{} {token}", if prefix.is_empty() { "Bearer" } else { prefix })),
        super::Auth::Basic { user, pass } => rb = rb.basic_auth(user, Some(pass)),
        super::Auth::Apikey { key, value, query: false } => rb = rb.header(key.as_str(), value.as_str()),
        _ => {}
    }
    let resp = rb.send().await?;
    let start = Instant::now();
    events.unbounded_send(Event::Open { status: resp.status().as_u16(), headers: resp.headers().iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect() }).ok();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut parser = SseParser::default();
    while let Some(chunk) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk?));
        while let Some(i) = buf.find('\n') {
            let line: String = buf.drain(..=i).collect();
            if let Some((event, id, data)) = parser.line(line.trim_end_matches(['\n', '\r'])) {
                events.unbounded_send(Event::Message { text: shorten(&data), binary: false, event, id, ms: start.elapsed().as_millis() as u64 }).ok();
            }
        }
        if events.is_closed() {
            return Ok(());
        }
        // A line that never ends is cut, not kept growing.
        if buf.len() > MAX_MESSAGE * 4 {
            buf.clear();
        }
    }
    events.unbounded_send(Event::Closed { reason: "the server ended the stream".into() }).ok();
    Ok(())
}

/// The event-stream format: `field: value` lines, a blank line ends an event.
#[derive(Default)]
pub struct SseParser {
    event: String,
    id: String,
    data: Vec<String>,
}

impl SseParser {
    /// A line; an event when it ended one: (type, id, data).
    pub fn line(&mut self, line: &str) -> Option<(String, String, String)> {
        if line.is_empty() {
            if self.data.is_empty() {
                self.event.clear();
                return None;
            }
            let ev = (if self.event.is_empty() { "message".to_string() } else { std::mem::take(&mut self.event) }, self.id.clone(), self.data.join("\n"));
            self.data.clear();
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').map(|(f, v)| (f, v.strip_prefix(' ').unwrap_or(v))).unwrap_or((line, ""));
        match field {
            "event" => self.event = value.to_string(),
            "data" => self.data.push(value.to_string()),
            "id" => self.id = value.to_string(),
            _ => {}
        }
        None
    }
}

// ---------------------------------------------------------------- GraphQL schema

pub const INTROSPECTION: &str = "query IntrospectionQuery { __schema { queryType { name } mutationType { name } subscriptionType { name } types { kind name description fields(includeDeprecated: true) { name description args { name type { ...T } defaultValue } type { ...T } } inputFields { name type { ...T } } enumValues(includeDeprecated: true) { name } } } } fragment T on __Type { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } }";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    pub query: String,
    pub mutation: String,
    pub subscription: String,
    pub types: Vec<Type>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Type {
    pub kind: String,
    pub name: String,
    pub description: String,
    /// (name, its type as written: `[User!]!`, arguments as written).
    pub fields: Vec<(String, String, String)>,
}

fn type_ref(t: &serde_json::Value) -> String {
    match t["kind"].as_str() {
        Some("NON_NULL") => format!("{}!", type_ref(&t["ofType"])),
        Some("LIST") => format!("[{}]", type_ref(&t["ofType"])),
        _ => t["name"].as_str().unwrap_or("?").to_string(),
    }
}

pub fn parse_schema(v: &serde_json::Value) -> Result<Schema> {
    let s = v.pointer("/data/__schema").ok_or_else(|| anyhow!("the server didn't send a schema: {}", v.get("errors").map(|e| e.to_string()).unwrap_or_default()))?;
    let name = |p: &str| s.pointer(p).and_then(|n| n.as_str()).unwrap_or("").to_string();
    let mut types: Vec<Type> = s["types"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| !t["name"].as_str().unwrap_or("").starts_with("__"))
        .map(|t| {
            let fields = t["fields"].as_array().or(t["inputFields"].as_array()).into_iter().flatten().map(|f| {
                let args: Vec<String> = f["args"].as_array().into_iter().flatten().map(|a| format!("{}: {}", a["name"].as_str().unwrap_or(""), type_ref(&a["type"]))).collect();
                (f["name"].as_str().unwrap_or("").to_string(), type_ref(&f["type"]), args.join(", "))
            });
            let mut fields: Vec<_> = fields.collect();
            fields.extend(t["enumValues"].as_array().into_iter().flatten().map(|e| (e["name"].as_str().unwrap_or("").to_string(), String::new(), String::new())));
            Type { kind: t["kind"].as_str().unwrap_or("").to_string(), name: t["name"].as_str().unwrap_or("").to_string(), description: t["description"].as_str().unwrap_or("").to_string(), fields }
        })
        .collect();
    let (q, m, sub) = (name("/queryType/name"), name("/mutationType/name"), name("/subscriptionType/name"));
    // The roots first, then the rest by name.
    types.sort_by_key(|t| (![&q, &m, &sub].contains(&&t.name), t.kind == "SCALAR", t.name.to_lowercase()));
    Ok(Schema { query: q, mutation: m, subscription: sub, types })
}

/// Asks a GraphQL server for its schema.
pub async fn fetch_schema(mut ready: Ready) -> Result<Schema> {
    ready.method = "POST".into();
    ready.body = super::Body::Graphql { query: INTROSPECTION.into(), variables: String::new() };
    let resp = super::send::send(ready).await?;
    let v: serde_json::Value = serde_json::from_slice(&resp.body).map_err(|_| anyhow!("status {}: the answer isn't JSON", resp.status))?;
    parse_schema(&v)
}

/// The schema as SDL-like text (for showing, and for completing names).
pub fn schema_text(s: &Schema) -> String {
    let mut out = String::new();
    for t in &s.types {
        if t.kind == "SCALAR" {
            out.push_str(&format!("scalar {}\n\n", t.name));
            continue;
        }
        if !t.description.is_empty() {
            out.push_str(&format!("\"\"\"{}\"\"\"\n", t.description.replace('\n', " ")));
        }
        let kw = match t.kind.as_str() {
            "INPUT_OBJECT" => "input",
            "ENUM" => "enum",
            "INTERFACE" => "interface",
            "UNION" => "union",
            _ => "type",
        };
        out.push_str(&format!("{kw} {} {{\n", t.name));
        for (n, ty, args) in &t.fields {
            match (ty.is_empty(), args.is_empty()) {
                (true, _) => out.push_str(&format!("  {n}\n")),
                (false, true) => out.push_str(&format!("  {n}: {ty}\n")),
                (false, false) => out.push_str(&format!("  {n}({args}): {ty}\n")),
            }
        }
        out.push_str("}\n\n");
    }
    out
}

#[cfg(test)]
mod live {
    use super::*;

    #[test]
    fn event_streams_split_into_events() {
        let mut p = SseParser::default();
        let mut got = Vec::new();
        for l in [": hello", "event: tick", "id: 1", "data: a", "data: b", "", "data: plain", ""] {
            if let Some(e) = p.line(l) {
                got.push(e);
            }
        }
        assert_eq!(got, [("tick".into(), "1".into(), "a\nb".into()), ("message".into(), "1".into(), "plain".into())]);
    }

    #[test]
    fn schemas_read_from_introspection() {
        let v = serde_json::json!({ "data": { "__schema": {
            "queryType": { "name": "Query" }, "mutationType": null, "subscriptionType": null,
            "types": [
                { "kind": "OBJECT", "name": "User", "fields": [{ "name": "id", "args": [], "type": { "kind": "NON_NULL", "ofType": { "kind": "SCALAR", "name": "ID" } } }] },
                { "kind": "OBJECT", "name": "Query", "fields": [{ "name": "users", "args": [{ "name": "first", "type": { "kind": "SCALAR", "name": "Int" } }], "type": { "kind": "LIST", "ofType": { "kind": "OBJECT", "name": "User" } } }] },
                { "kind": "OBJECT", "name": "__Type", "fields": [] }
            ] } } });
        let s = parse_schema(&v).unwrap();
        assert_eq!(s.types[0].name, "Query", "the root first");
        assert_eq!(s.types[0].fields[0], ("users".into(), "[User]".into(), "first: Int".into()));
        assert!(schema_text(&s).contains("type User {\n  id: ID!\n}"));
    }
}
