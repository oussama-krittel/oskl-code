//! Redis (one multiplexed connection per database number). Keys are listed with SCAN, never
//! KEYS, so a big server isn't blocked; values are read in capped ranges.

use super::{Conn, Grid, Node, NodeKind, Page, PageSpec, cell};
use anyhow::{Result, anyhow, bail};
use redis::Value;
use redis::aio::MultiplexedConnection;

/// The most keys listed under a database (filter to find others).
const MAX_KEYS: usize = 500;

pub async fn connect(conn: &Conn, secret: &str, db: &str) -> Result<MultiplexedConnection> {
    let db: i64 = db.trim().parse().unwrap_or(0);
    let auth = match (conn.user.is_empty(), secret.is_empty()) {
        (_, true) => String::new(),
        (true, false) => format!(":{}@", enc(secret)),
        (false, false) => format!("{}:{}@", enc(&conn.user), enc(secret)),
    };
    let scheme = if conn.tls { "rediss" } else { "redis" };
    let insecure = if conn.tls && conn.trust_cert { "#insecure" } else { "" };
    let host = if conn.host.contains(':') { format!("[{}]", conn.host) } else { conn.host.clone() };
    let client = redis::Client::open(format!("{scheme}://{auth}{host}:{}/{db}{insecure}", conn.port))?;
    Ok(client.get_multiplexed_async_connection().await?)
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub async fn version(c: &mut MultiplexedConnection) -> Result<String> {
    let info: String = redis::cmd("INFO").arg("server").query_async(c).await?;
    let v = info.lines().find_map(|l| l.strip_prefix("redis_version:")).unwrap_or("?");
    Ok(format!("Redis {}", v.trim()))
}

/// A key or argument as typed in a query (quoted when it has spaces or quotes).
pub fn quote(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '"', '\'', '\\', '\t', '\n']) { s.to_string() } else { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")) }
}

async fn cmd<T: redis::FromRedisValue>(c: &mut MultiplexedConnection, args: &[&str]) -> Result<T> {
    let mut cmd = redis::cmd(args[0]);
    for a in &args[1..] {
        cmd.arg(*a);
    }
    Ok(cmd.query_async(c).await?)
}

pub async fn children(c: &mut MultiplexedConnection, conn: &Conn, path: &[String]) -> Result<Vec<Node>> {
    Ok(match path {
        [] => {
            // The databases with keys (and the connection's own, even if empty).
            let info: String = cmd(c, &["INFO", "keyspace"]).await?;
            let mut dbs: Vec<(i64, String)> = info
                .lines()
                .filter_map(|l| {
                    let (name, rest) = l.split_once(':')?;
                    let n: i64 = name.strip_prefix("db")?.parse().ok()?;
                    let keys = rest.split(',').find_map(|p| p.strip_prefix("keys="))?.to_string();
                    Some((n, keys))
                })
                .collect();
            let own: i64 = conn.database.parse().unwrap_or(0);
            if !dbs.iter().any(|(n, _)| *n == own) {
                dbs.push((own, "0".into()));
            }
            dbs.sort();
            dbs.into_iter()
                .map(|(n, keys)| Node { label: format!("db{n}"), detail: super::short_count(keys.parse().unwrap_or(0)), kind: NodeKind::Database, path: vec![format!("db{n}")] })
                .collect()
        }
        [db] => level(c, db, "").await?,
        [db, folder] => match folder.strip_prefix(FOLDER) {
            Some(prefix) => level(c, db, prefix).await?,
            None => Vec::new(),
        },
        _ => Vec::new(),
    })
}

/// Marks a folder's path part (a key prefix, not a key).
const FOLDER: &str = "\u{2}";

/// The keys under `prefix`: those sharing their next part (up to a `:`) as one folder (when
/// two or more do, like other Redis tools), the rest as keys with their types.
async fn level(c: &mut MultiplexedConnection, db: &str, prefix: &str) -> Result<Vec<Node>> {
    let keys = scan(c, &format!("{}*", glob_escape(prefix))).await?;
    let groups = group(&keys, prefix);
    let leaves: Vec<&String> = groups.iter().filter_map(|g| if let Group::Key(k) = g { Some(k) } else { None }).collect();
    // Their types, in one round trip.
    let mut pipe = redis::pipe();
    for k in &leaves {
        pipe.cmd("TYPE").arg(k.as_str());
    }
    let types: Vec<String> = if leaves.is_empty() { Vec::new() } else { pipe.query_async(c).await? };
    let mut types = types.into_iter();
    let capped = keys.len() >= MAX_KEYS;
    Ok(groups
        .into_iter()
        .map(|g| match g {
            Group::Folder(name, n) => Node {
                label: name.clone(),
                detail: format!("{n}{}", if capped { "+" } else { "" }),
                kind: NodeKind::Folder,
                path: vec![db.to_string(), format!("{FOLDER}{prefix}{name}:")],
            },
            Group::Key(k) => Node { label: k.clone(), detail: types.next().unwrap_or_default(), kind: NodeKind::Key, path: vec![db.to_string(), k] },
        })
        .collect())
}

#[derive(Debug, PartialEq)]
enum Group {
    Folder(String, usize),
    Key(String),
}

/// Folders first (by name), then keys.
fn group(keys: &[String], prefix: &str) -> Vec<Group> {
    let mut parts: std::collections::BTreeMap<String, Vec<&String>> = Default::default();
    let mut singles = Vec::new();
    for k in keys {
        let rest = k.strip_prefix(prefix).unwrap_or(k);
        match rest.split_once(':') {
            Some((head, _)) => parts.entry(head.to_string()).or_default().push(k),
            None => singles.push(k.clone()),
        }
    }
    let mut out = Vec::new();
    for (head, ks) in parts {
        if ks.len() >= 2 {
            out.push(Group::Folder(head, ks.len()));
        } else {
            singles.extend(ks.into_iter().cloned());
        }
    }
    singles.sort();
    out.extend(singles.into_iter().map(Group::Key));
    out
}

/// Up to MAX_KEYS key names matching `pattern`, sorted.
async fn scan(c: &mut MultiplexedConnection, pattern: &str) -> Result<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    let mut cursor = "0".to_string();
    // Bounded: a few hundred round trips at most, even on a huge, sparse keyspace.
    for _ in 0..200 {
        let (next, batch): (String, Vec<Vec<u8>>) = cmd(c, &["SCAN", &cursor, "MATCH", pattern, "COUNT", "1000"]).await?;
        keys.extend(batch.into_iter().map(|k| String::from_utf8_lossy(&k).into_owned()));
        cursor = next;
        if cursor == "0" || keys.len() >= MAX_KEYS {
            break;
        }
    }
    keys.sort();
    keys.dedup();
    keys.truncate(MAX_KEYS);
    Ok(keys)
}

/// A key's value as rows (by its type), or — for a database — its keys.
pub async fn page(c: &mut MultiplexedConnection, path: &[String], spec: &PageSpec) -> Result<Page> {
    let [_, key] = path else { bail!("not a key") };
    let kind: String = cmd(c, &["TYPE", key]).await?;
    let ttl: i64 = cmd(c, &["TTL", key]).await?;
    let (lo, hi) = (spec.offset, spec.offset + spec.limit.max(1) - 1);
    let (lo_s, hi_s) = (lo.to_string(), hi.to_string());
    let (grid, total): (Grid, Option<u64>) = match kind.as_str() {
        "string" => {
            let v: Value = cmd(c, &["GET", key]).await?;
            (Grid { columns: vec!["value".into()], rows: vec![vec![text(&v)]], ..Default::default() }, Some(1))
        }
        "list" => {
            let items: Vec<Value> = cmd(c, &["LRANGE", key, &lo_s, &hi_s]).await?;
            let n: u64 = cmd(c, &["LLEN", key]).await?;
            let mut g = Grid { columns: vec!["index".into(), "value".into()], ..Default::default() };
            for (i, v) in items.iter().enumerate() {
                g.push(vec![Some((lo + i).to_string()), text(v)]);
            }
            (g, Some(n))
        }
        "zset" => {
            let items: Vec<Value> = cmd(c, &["ZRANGE", key, &lo_s, &hi_s, "WITHSCORES"]).await?;
            let n: u64 = cmd(c, &["ZCARD", key]).await?;
            let mut g = Grid { columns: vec!["member".into(), "score".into()], ..Default::default() };
            for pair in items.chunks(2) {
                g.push(vec![text(&pair[0]), pair.get(1).and_then(text)]);
            }
            (g, Some(n))
        }
        "hash" | "set" => {
            // Scanned (no ranges for these): everything up to the page's end, then the page.
            let (scan_cmd, cols): (&str, Vec<String>) = if kind == "hash" { ("HSCAN", vec!["field".into(), "value".into()]) } else { ("SSCAN", vec!["member".into()]) };
            let pattern = if spec.search.trim().is_empty() { "*".to_string() } else { format!("*{}*", glob_escape(spec.search.trim())) };
            let mut items: Vec<Value> = Vec::new();
            let mut cursor = "0".to_string();
            let want = (hi + 1) * if kind == "hash" { 2 } else { 1 };
            for _ in 0..200 {
                let (next, batch): (String, Vec<Value>) = cmd(c, &[scan_cmd, key, &cursor, "MATCH", &pattern, "COUNT", "500"]).await?;
                items.extend(batch);
                cursor = next;
                if cursor == "0" || items.len() >= want {
                    break;
                }
            }
            let n: u64 = cmd(c, &[if kind == "hash" { "HLEN" } else { "SCARD" }, key]).await?;
            let mut g = Grid { columns: cols, ..Default::default() };
            let per = if kind == "hash" { 2 } else { 1 };
            for chunk in items.chunks(per).skip(lo).take(spec.limit.max(1)) {
                g.push(chunk.iter().map(text).collect());
            }
            (g, Some(if spec.search.trim().is_empty() { n } else { (items.len() / per) as u64 }))
        }
        "stream" => {
            let items: Value = cmd(c, &["XRANGE", key, "-", "+", "COUNT", &(hi + 1).to_string()]).await?;
            let n: u64 = cmd(c, &["XLEN", key]).await?;
            let mut g = Grid { columns: vec!["id".into(), "fields".into()], ..Default::default() };
            if let Value::Array(entries) = items {
                for e in entries.iter().skip(lo) {
                    if let Value::Array(parts) = e {
                        g.push(vec![parts.first().and_then(text), parts.get(1).and_then(text)]);
                    }
                }
            }
            (g, Some(n))
        }
        "none" => bail!("the key {key} doesn't exist (any more)"),
        other => (Grid::note(format!("{other} values aren't shown here; query them with their commands")), None),
    };
    let mut grid = grid;
    grid.note = format!("{kind} · {}", match ttl {
        -1 => "no expiry".to_string(),
        -2 => "gone".to_string(),
        s => format!("expires in {s}s"),
    });
    Ok(Page { grid, total, estimate: false })
}

fn glob_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "*?[]\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Commands, one per line; each gives a result.
pub async fn query(c: &mut MultiplexedConnection, text: &str) -> Result<Vec<Grid>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let args = split(line)?;
        let mut cmd = redis::cmd(&args[0]);
        for a in &args[1..] {
            cmd.arg(a.as_bytes());
        }
        let v: Value = match cmd.query_async(c).await {
            Ok(v) => v,
            Err(e) => bail!("{}: {e}", args[0].to_uppercase()),
        };
        out.push(grid(&v));
    }
    if out.is_empty() {
        out.push(Grid::note("Nothing to run"));
    }
    Ok(out)
}

/// A reply as rows: arrays one per row, maps field/value, the rest one cell.
fn grid(v: &Value) -> Grid {
    match v {
        Value::Array(items) | Value::Set(items) => {
            let mut g = Grid { columns: vec!["#".into(), "value".into()], ..Default::default() };
            for (i, x) in items.iter().enumerate() {
                g.push(vec![Some((i + 1).to_string()), text(x)]);
            }
            if items.is_empty() {
                g.note = "(empty)".into();
            }
            g
        }
        Value::Map(pairs) => Grid::pairs(pairs.iter().map(|(k, v)| (text(k).unwrap_or_default(), text(v)))),
        Value::Okay => Grid::note("OK"),
        Value::Nil => Grid { columns: vec!["value".into()], rows: vec![vec![None]], ..Default::default() },
        // INFO and friends: one line per row.
        Value::BulkString(b) if b.contains(&b'\n') => {
            let s = String::from_utf8_lossy(b);
            let mut g = Grid { columns: vec!["line".into()], ..Default::default() };
            for l in s.lines() {
                g.push(vec![Some(l.to_string())]);
            }
            g
        }
        other => Grid { columns: vec!["value".into()], rows: vec![vec![text(other)]], ..Default::default() },
    }
}

/// A reply as one cell.
fn text(v: &Value) -> Option<String> {
    match v {
        Value::Nil => None,
        Value::Int(n) => Some(n.to_string()),
        Value::BulkString(b) => cell(String::from_utf8_lossy(b).into_owned()),
        Value::SimpleString(s) => cell(s.as_str()),
        Value::Okay => Some("OK".into()),
        Value::Double(d) => Some(d.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        Value::VerbatimString { text, .. } => cell(text.as_str()),
        Value::Array(items) | Value::Set(items) => cell(format!("[{}]", items.iter().map(|x| text(x).unwrap_or_else(|| "nil".into())).collect::<Vec<_>>().join(", "))),
        Value::Map(pairs) => cell(format!("{{{}}}", pairs.iter().map(|(k, v)| format!("{}: {}", text(k).unwrap_or_default(), text(v).unwrap_or_else(|| "nil".into()))).collect::<Vec<_>>().join(", "))),
        Value::ServerError(e) => Some(format!("(error) {e:?}")),
        other => cell(format!("{other:?}")),
    }
}

/// Splits a command line like redis-cli: spaces separate, quotes group, \ escapes.
fn split(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some('n') => cur.push('\n'),
                Some('t') => cur.push('\t'),
                Some(o) => cur.push(o),
                None => {}
            },
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(anyhow!("unclosed quote"));
    }
    if started {
        out.push(cur);
    }
    if out.is_empty() {
        bail!("empty command");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_like_redis_cli() {
        assert_eq!(split(r#"SET "a key" 'it''s' "line\nbreak" plain"#).unwrap(), ["SET", "a key", "its", "line\nbreak", "plain"]);
        assert_eq!(split(r#"SET k """#).unwrap(), ["SET", "k", ""]);
        assert!(split("GET \"open").is_err());
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("user:1"), "user:1");
        assert_eq!(enc("p@ss:w/rd"), "p%40ss%3Aw%2Frd");
    }

    #[test]
    fn keys_sharing_a_prefix_become_folders() {
        let keys: Vec<String> = ["user:1", "user:2", "config:maintenance", "leaderboard", "cache:page:1", "cache:page:2", "cache:x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            group(&keys, ""),
            [Group::Folder("cache".into(), 3), Group::Folder("user".into(), 2), Group::Key("config:maintenance".into()), Group::Key("leaderboard".into())],
            "one key with a prefix stays a key"
        );
        let under: Vec<String> = keys.iter().filter(|k| k.starts_with("cache:")).cloned().collect();
        assert_eq!(group(&under, "cache:"), [Group::Folder("page".into(), 2), Group::Key("cache:x".into())]);
    }

    #[test]
    fn replies_become_rows() {
        let g = grid(&Value::Array(vec![Value::BulkString(b"a".to_vec()), Value::Nil]));
        assert_eq!(g.rows, vec![vec![Some("1".into()), Some("a".into())], vec![Some("2".into()), None]]);
        assert_eq!(grid(&Value::Okay).note, "OK");
        assert_eq!(grid(&Value::Int(5)).rows, vec![vec![Some("5".into())]]);
    }
}
