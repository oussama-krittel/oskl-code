//! PostgreSQL (tokio-postgres). Queries use the simple protocol: any SQL runs as typed
//! (several statements, transactions, DDL), and values come back as text, ready to show.

use super::{Conn, Grid, Node, NodeKind, Page, PageSpec, cell};
use anyhow::{Result, anyhow};
use futures::StreamExt;
use tokio_postgres::{Client, SimpleQueryMessage, config::SslMode};

/// Counted exactly up to this many rows; past it, "100,000+" (or the server's estimate).
const COUNT_CAP: u64 = 100_000;

pub async fn connect(conn: &Conn, secret: &str, db: &str) -> Result<Client> {
    let mut cfg: tokio_postgres::Config = if conn.use_url {
        secret.parse().map_err(|e| anyhow!("connection string: {e}"))?
    } else {
        let mut c = tokio_postgres::Config::new();
        c.host(&conn.host).port(conn.port).user(&conn.user);
        if !secret.is_empty() {
            c.password(secret);
        }
        c
    };
    if !db.is_empty() {
        cfg.dbname(db);
    }
    cfg.application_name("OSKL Code").connect_timeout(super::engine::CONNECT_TIMEOUT);
    if !conn.use_url {
        cfg.ssl_mode(if conn.tls { SslMode::Require } else { SslMode::Prefer });
    }
    // Like libpq: "prefer" doesn't check the certificate; "require" does unless told to trust it.
    let check = conn.tls && !conn.trust_cert;
    let tls = native_tls::TlsConnector::builder().danger_accept_invalid_certs(!check).danger_accept_invalid_hostnames(!check).build()?;
    let (client, connection) = cfg.connect(postgres_native_tls::MakeTlsConnector::new(tls)).await?;
    tokio::spawn(async move {
        connection.await.ok();
    });
    Ok(client)
}

pub async fn version(c: &Client) -> Result<String> {
    let v = scalar(c, "SELECT version()").await?.unwrap_or_default();
    Ok(v.split(" on ").next().unwrap_or(&v).split(',').next().unwrap_or(&v).to_string())
}

/// `"name"`, quotes doubled.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `'text'`, quotes doubled.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A LIKE pattern finding `s` anywhere (its own % _ \ taken literally).
fn contains(s: &str) -> String {
    let esc = s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    lit(&format!("%{esc}%"))
}

/// Runs SQL and collects every statement's rows (capped) and outcome.
async fn run(c: &Client, sql: &str) -> Result<Vec<Grid>> {
    let stream = c.simple_query_raw(sql).await?;
    futures::pin_mut!(stream);
    let mut out = Vec::new();
    let mut cur: Option<Grid> = None;
    while let Some(msg) = stream.next().await {
        match msg? {
            SimpleQueryMessage::RowDescription(cols) => {
                cur = Some(Grid { columns: cols.iter().map(|c| c.name().to_string()).collect(), ..Default::default() });
            }
            SimpleQueryMessage::Row(row) => {
                if let Some(g) = cur.as_mut()
                    && g.rows.len() < super::MAX_ROWS
                {
                    g.push((0..row.len()).map(|i| row.get(i).and_then(cell)).collect());
                } else if let Some(g) = cur.as_mut() {
                    g.more = true;
                }
            }
            SimpleQueryMessage::CommandComplete(n) => match cur.take() {
                Some(mut g) => {
                    g.note = format!("{n} row{}", if n == 1 { "" } else { "s" });
                    out.push(g);
                }
                None => out.push(Grid::note(if n > 0 { format!("{n} row{} affected", if n == 1 { "" } else { "s" }) } else { "Done".to_string() })),
            },
            _ => {}
        }
    }
    if let Some(g) = cur {
        out.push(g);
    }
    Ok(out)
}

async fn scalar(c: &Client, sql: &str) -> Result<Option<String>> {
    Ok(run(c, sql).await?.into_iter().next().and_then(|g| g.rows.into_iter().next()).and_then(|r| r.into_iter().next()).flatten())
}

async fn rows(c: &Client, sql: &str) -> Result<Vec<Vec<Option<String>>>> {
    Ok(run(c, sql).await?.into_iter().next().map(|g| g.rows).unwrap_or_default())
}

pub async fn children(c: &Client, _conn: &Conn, path: &[String]) -> Result<Vec<Node>> {
    let text = |r: &[Option<String>], i: usize| r.get(i).cloned().flatten().unwrap_or_default();
    Ok(match path {
        [] => rows(c, "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname")
            .await?
            .into_iter()
            .map(|r| {
                let name = text(&r, 0);
                Node { label: name.clone(), detail: String::new(), kind: NodeKind::Database, path: vec![name] }
            })
            .collect(),
        [db] => rows(
            c,
            "SELECT n.nspname, count(c.oid) FROM pg_namespace n LEFT JOIN pg_class c ON c.relnamespace = n.oid AND c.relkind IN ('r','p','v','m','f') \
             WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' GROUP BY n.nspname ORDER BY n.nspname = 'public' DESC, n.nspname",
        )
        .await?
        .into_iter()
        .map(|r| {
            let name = text(&r, 0);
            Node { label: name.clone(), detail: text(&r, 1), kind: NodeKind::Schema, path: vec![db.clone(), name] }
        })
        .collect(),
        [db, schema] => rows(
            c,
            &format!(
                "SELECT c.relname, c.relkind::text, c.reltuples::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relkind IN ('r','p','v','m','f') ORDER BY c.relname",
                lit(schema)
            ),
        )
        .await?
        .into_iter()
        .map(|r| {
            let name = text(&r, 0);
            let view = matches!(text(&r, 1).as_str(), "v" | "m");
            let n: i64 = text(&r, 2).parse().unwrap_or(-1);
            Node {
                label: name.clone(),
                detail: if view { "view".into() } else if n > 0 { super::short_count(n as u64) } else { String::new() },
                kind: if view { NodeKind::View } else { NodeKind::Table },
                path: vec![db.clone(), schema.clone(), name],
            }
        })
        .collect(),
        _ => Vec::new(),
    })
}

pub async fn page(c: &Client, path: &[String], spec: &PageSpec) -> Result<Page> {
    let [_, schema, table] = path else { return Err(anyhow!("not a table")) };
    let from = format!("{}.{} AS t", ident(schema), ident(table));
    let filter = if spec.search.trim().is_empty() {
        String::new()
    } else {
        let what = spec.column.as_deref().map(|c| format!("t.{}::text", ident(c))).unwrap_or_else(|| "t::text".into());
        format!(" WHERE {what} ILIKE {} ESCAPE '\\'", contains(spec.search.trim()))
    };
    let order = spec.sort.as_ref().map(|(c, desc)| format!(" ORDER BY t.{} {} NULLS LAST", ident(c), if *desc { "DESC" } else { "ASC" })).unwrap_or_default();
    let grid = run(c, &format!("SELECT * FROM {from}{filter}{order} LIMIT {} OFFSET {}", spec.limit, spec.offset)).await?.into_iter().next().unwrap_or_default();
    // Big tables (unsearched): the server's estimate. Otherwise counted, up to COUNT_CAP.
    let estimate: i64 = if filter.is_empty() {
        scalar(c, &format!("SELECT reltuples::bigint FROM pg_class WHERE oid = {}::regclass", lit(&format!("{}.{}", ident(schema), ident(table)))))
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1)
    } else {
        -1
    };
    let (total, est) = if estimate > COUNT_CAP as i64 {
        (estimate as u64, true)
    } else {
        let n: u64 = scalar(c, &format!("SELECT count(*) FROM (SELECT 1 FROM {from}{filter} LIMIT {}) x", COUNT_CAP + 1)).await?.and_then(|s| s.parse().ok()).unwrap_or(0);
        (n.min(COUNT_CAP), n > COUNT_CAP)
    };
    Ok(Page { grid: Grid { note: String::new(), more: false, ..grid }, total: Some(total), estimate: est })
}

pub async fn query(c: &Client, text: &str) -> Result<Vec<Grid>> {
    if text.trim().is_empty() {
        return Ok(vec![Grid::note("Nothing to run")]);
    }
    run(c, text).await
}
