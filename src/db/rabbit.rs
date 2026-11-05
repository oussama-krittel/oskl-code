//! RabbitMQ, through its management plugin's HTTP API (port 15672): queues, exchanges and
//! their bindings. Nothing stays open between calls. Queries are API calls:
//! `GET /api/queues`, or `POST /api/…` with a JSON body on the lines after it.

use super::{Conn, Grid, Node, NodeKind, Page, PageSpec, cell};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;

pub struct Api {
    agent: ureq::Agent,
    base: String,
    auth: String,
    /// Only this virtual host ("" for all).
    vhost: String,
}

/// A path segment, percent-encoded ("/" is "%2F", the default virtual host).
pub fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl Api {
    pub fn new(conn: &Conn, secret: &str) -> Result<Api> {
        let tls = ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).disable_verification(conn.trust_cert).build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .user_agent("OSKL Code")
            .build()
            .into();
        let host = if conn.host.contains(':') { format!("[{}]", conn.host) } else { conn.host.clone() };
        Ok(Api {
            agent,
            base: format!("{}://{host}:{}", if conn.tls { "https" } else { "http" }, conn.port),
            auth: format!("Basic {}", base64(format!("{}:{secret}", conn.user).as_bytes())),
            vhost: conn.database.clone(),
        })
    }

    /// Calls the API: the JSON it answers (or its error, readably).
    fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
        let path = if path.starts_with("/api") { path.to_string() } else { format!("/api{}{path}", if path.starts_with('/') { "" } else { "/" }) };
        let url = format!("{}{path}", self.base);
        let resp = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &self.auth).call(),
            ("DELETE", _) => self.agent.delete(&url).header("Authorization", &self.auth).call(),
            ("POST", b) => self.agent.post(&url).header("Authorization", &self.auth).header("Content-Type", "application/json").send(b.unwrap_or("{}")),
            ("PUT", b) => self.agent.put(&url).header("Authorization", &self.auth).header("Content-Type", "application/json").send(b.unwrap_or("{}")),
            (m, _) => bail!("{m}: use GET, POST, PUT or DELETE"),
        };
        let mut resp = resp.map_err(|e| match e {
            ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => anyhow!("nothing answers at {} — is the management plugin on? (rabbitmq-plugins enable rabbitmq_management)", self.base),
            e => anyhow!("{e}"),
        })?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let json: Value = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text.clone())) };
        match status {
            200..300 => Ok(json),
            401 => bail!("wrong user name or password (or the user isn't allowed to use the management API)"),
            404 => bail!("not found: {path}"),
            _ => bail!("{status}: {}", json.get("reason").and_then(Value::as_str).map(str::to_string).unwrap_or(text)),
        }
    }

    fn vhost_path(&self) -> String {
        if self.vhost.is_empty() { String::new() } else { format!("/{}", enc(&self.vhost)) }
    }

    pub fn version(&self) -> Result<String> {
        let o = self.call("GET", "/api/overview", None)?;
        Ok(format!("RabbitMQ {}", o.get("rabbitmq_version").and_then(Value::as_str).unwrap_or("?")))
    }

    pub fn children(&self, path: &[String]) -> Result<Vec<Node>> {
        let vh = self.vhost_path();
        let list = |what: &str, cols: &str| -> Result<Vec<Value>> {
            Ok(self.call("GET", &format!("/api/{what}{vh}?columns={cols}"), None)?.as_array().cloned().unwrap_or_default())
        };
        let s = |v: &Value, k: &str| v.get(k).map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).unwrap_or_default();
        Ok(match path {
            [] => {
                let (q, e) = (list("queues", "name")?.len(), list("exchanges", "name")?.len());
                vec![
                    Node { label: "Queues".into(), detail: q.to_string(), kind: NodeKind::Folder, path: vec!["Queues".into()] },
                    Node { label: "Exchanges".into(), detail: e.to_string(), kind: NodeKind::Folder, path: vec!["Exchanges".into()] },
                ]
            }
            [f] if f == "Queues" => {
                let mut qs = list("queues", "name,vhost,messages")?;
                qs.sort_by_key(|q| s(q, "name"));
                qs.iter()
                    .map(|q| {
                        let (name, vhost) = (s(q, "name"), s(q, "vhost"));
                        let label = if self.vhost.is_empty() && vhost != "/" { format!("{vhost} · {name}") } else { name.clone() };
                        Node { label, detail: q.get("messages").and_then(Value::as_u64).map(super::short_count).unwrap_or_default(), kind: NodeKind::Queue, path: vec!["Queues".into(), vhost, name] }
                    })
                    .collect()
            }
            [f] if f == "Exchanges" => {
                let mut es = list("exchanges", "name,vhost,type")?;
                es.sort_by_key(|e| s(e, "name"));
                es.iter()
                    .map(|e| {
                        let (name, vhost) = (s(e, "name"), s(e, "vhost"));
                        let shown = if name.is_empty() { "(AMQP default)".to_string() } else { name.clone() };
                        let label = if self.vhost.is_empty() && vhost != "/" { format!("{vhost} · {shown}") } else { shown };
                        Node { label, detail: s(e, "type"), kind: NodeKind::Exchange, path: vec!["Exchanges".into(), vhost, name] }
                    })
                    .collect()
            }
            _ => Vec::new(),
        })
    }

    pub fn page(&self, path: &[String], _spec: &PageSpec) -> Result<Page> {
        match path {
            [f, vhost, name] if f == "Queues" => {
                let q = self.call("GET", &format!("/api/queues/{}/{}", enc(vhost), enc(name)), None)?;
                const FIRST: &[&str] = &["state", "type", "messages", "messages_ready", "messages_unacknowledged", "consumers", "durable", "auto_delete", "exclusive", "arguments", "node", "memory"];
                let mut items: Vec<(String, Option<String>)> = FIRST.iter().filter_map(|k| q.get(*k).map(|v| (k.to_string(), show(v)))).collect();
                if let Some(rate) = q.pointer("/message_stats/publish_details/rate") {
                    items.push(("publish rate /s".into(), show(rate)));
                }
                if let Value::Object(m) = &q {
                    for (k, v) in m {
                        if !FIRST.contains(&k.as_str()) && !v.is_object() && !v.is_array() {
                            items.push((k.clone(), show(v)));
                        }
                    }
                }
                Ok(Page { total: Some(items.len() as u64), grid: Grid::pairs(items), estimate: false })
            }
            [_, vhost, name] => {
                let source = if name.is_empty() { "amq.default".to_string() } else { name.clone() };
                let b = self.call("GET", &format!("/api/exchanges/{}/{}/bindings/source", enc(vhost), enc(&source)), None)?;
                let g = grid(&b);
                Ok(Page { total: Some(g.rows.len() as u64), grid: g, estimate: false })
            }
            _ => bail!("not a queue or an exchange"),
        }
    }

    /// API calls: a line `METHOD /path`, then its JSON body (if any) on the lines after.
    pub fn query(&self, text: &str) -> Result<Vec<Grid>> {
        let mut out = Vec::new();
        let mut calls: Vec<(String, String, String)> = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            let mut parts = t.splitn(2, char::is_whitespace);
            let first = parts.next().unwrap_or("").to_uppercase();
            if matches!(first.as_str(), "GET" | "POST" | "PUT" | "DELETE") {
                calls.push((first, parts.next().unwrap_or("").trim().to_string(), String::new()));
            } else if let Some(c) = calls.last_mut() {
                c.2.push_str(line);
                c.2.push('\n');
            } else if !t.is_empty() && !t.starts_with('#') {
                bail!("start with a call, like: GET /api/queues");
            }
        }
        for (method, path, body) in calls {
            let body = body.trim();
            if !body.is_empty() {
                serde_json::from_str::<Value>(body).map_err(|e| anyhow!("{method} {path}: the body isn't JSON ({e})"))?;
            }
            let v = self.call(&method, &path, (!body.is_empty()).then_some(body))?;
            out.push(if v.is_null() { Grid::note(format!("{method} {path}: done")) } else { grid(&v) });
        }
        if out.is_empty() {
            out.push(Grid::note("Nothing to run"));
        }
        Ok(out)
    }
}

fn show(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => cell(s.as_str()),
        other => cell(other.to_string()),
    }
}

/// JSON as rows: an array of objects one per row, an object field/value, the rest one cell.
fn grid(v: &Value) -> Grid {
    match v {
        Value::Array(items) => {
            let mut g = Grid::default();
            for it in items {
                if let Value::Object(m) = it {
                    for k in m.keys() {
                        if g.columns.len() < 100 && !g.columns.contains(k) {
                            g.columns.push(k.clone());
                        }
                    }
                }
            }
            if g.columns.is_empty() {
                g.columns.push("value".into());
                for it in items {
                    g.push(vec![show(it)]);
                }
            } else {
                for it in items {
                    let row = g.columns.iter().map(|c| it.get(c).and_then(show)).collect();
                    g.push(row);
                }
            }
            if items.is_empty() {
                g.note = "(empty)".into();
            }
            g
        }
        Value::Object(m) => Grid::pairs(m.iter().map(|(k, v)| (k.clone(), show(v)))),
        other => Grid { columns: vec!["value".into()], rows: vec![vec![show(other)]], ..Default::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_pieces() {
        assert_eq!(base64(b"guest:guest"), "Z3Vlc3Q6Z3Vlc3Q=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(enc("/"), "%2F");
        let g = grid(&serde_json::json!([{"name": "a", "messages": 3}, {"name": "b", "extra": {"x": 1}}]));
        assert_eq!(g.columns, ["name", "messages", "extra"]);
        assert_eq!(g.rows[1], vec![Some("b".into()), None, Some("{\"x\":1}".into())]);
    }
}
