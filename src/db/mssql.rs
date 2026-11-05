//! SQL Server (tiberius, SQL Server login). Queries run as one batch, like in SSMS.

use super::{Conn, Grid, Node, NodeKind, Page, PageSpec, cell};
use anyhow::{Result, anyhow};
use futures::StreamExt;
use tiberius::{AuthMethod, ColumnData, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub type Client = tiberius::Client<Compat<TcpStream>>;

const COUNT_CAP: u64 = 100_000;

pub async fn connect(conn: &Conn, secret: &str) -> Result<Client> {
    let mut cfg = if conn.use_url {
        Config::from_ado_string(secret).map_err(|e| anyhow!("connection string: {e}"))?
    } else {
        let mut c = Config::new();
        c.host(&conn.host);
        c.port(conn.port);
        c.authentication(AuthMethod::sql_server(&conn.user, secret));
        if !conn.database.is_empty() {
            c.database(&conn.database);
        }
        // Off still encrypts the login (the password); Required encrypts everything.
        c.encryption(if conn.tls { EncryptionLevel::Required } else { EncryptionLevel::Off });
        if conn.trust_cert || !conn.tls {
            c.trust_cert();
        }
        c
    };
    cfg.application_name("OSKL Code");
    let mut addr = cfg.get_addr();
    for _ in 0..2 {
        let tcp = TcpStream::connect(&addr).await?;
        tcp.set_nodelay(true).ok();
        match Client::connect(cfg.clone(), tcp.compat_write()).await {
            Ok(c) => return Ok(c),
            // Azure sends the client on to the server that has the database.
            Err(tiberius::error::Error::Routing { host, port }) => {
                cfg.host(&host);
                cfg.port(port);
                addr = format!("{host}:{port}");
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(anyhow!("the server kept redirecting"))
}

pub async fn version(c: &mut Client) -> Result<String> {
    let v = scalar(c, "SELECT @@VERSION").await?.unwrap_or_default();
    Ok(v.lines().next().unwrap_or(&v).split(" (").next().unwrap_or(&v).trim().to_string())
}

/// `[name]`, brackets doubled.
pub fn ident(s: &str) -> String {
    format!("[{}]", s.replace(']', "]]"))
}

/// `N'text'`, quotes doubled.
fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

fn contains(s: &str) -> String {
    let esc = s.replace('[', "[[]").replace('%', "[%]").replace('_', "[_]");
    lit(&format!("%{esc}%"))
}

async fn run(c: &mut Client, sql: &str) -> Result<Vec<Grid>> {
    let mut stream = c.simple_query(sql).await?;
    let mut out: Vec<Grid> = Vec::new();
    let mut cur: Option<Grid> = None;
    while let Some(item) = stream.next().await {
        match item? {
            QueryItem::Metadata(m) => {
                if let Some(g) = cur.take() {
                    out.push(g);
                }
                cur = Some(Grid { columns: m.columns().iter().map(|c| c.name().to_string()).collect(), ..Default::default() });
            }
            QueryItem::Row(row) => {
                if let Some(g) = cur.as_mut() {
                    if g.rows.len() < super::MAX_ROWS {
                        g.push(row.into_iter().map(|d| text(&d)).collect());
                    } else {
                        g.more = true;
                    }
                }
            }
        }
    }
    out.extend(cur);
    for g in &mut out {
        g.note = format!("{} row{}{}", g.rows.len(), if g.rows.len() == 1 { "" } else { "s" }, if g.more { "+" } else { "" });
    }
    if out.is_empty() {
        out.push(Grid::note("Done"));
    }
    Ok(out)
}

async fn scalar(c: &mut Client, sql: &str) -> Result<Option<String>> {
    Ok(run(c, sql).await?.into_iter().next().and_then(|g| g.rows.into_iter().next()).and_then(|r| r.into_iter().next()).flatten())
}

async fn rows(c: &mut Client, sql: &str) -> Result<Vec<Vec<Option<String>>>> {
    Ok(run(c, sql).await?.into_iter().next().map(|g| g.rows).unwrap_or_default())
}

pub async fn children(c: &mut Client, path: &[String]) -> Result<Vec<Node>> {
    let t = |r: &[Option<String>], i: usize| r.get(i).cloned().flatten().unwrap_or_default();
    Ok(match path {
        [] => rows(c, "SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 AND state = 0 ORDER BY CASE WHEN database_id > 4 THEN 0 ELSE 1 END, name")
            .await?
            .into_iter()
            .map(|r| Node { label: t(&r, 0), detail: String::new(), kind: NodeKind::Database, path: vec![t(&r, 0)] })
            .collect(),
        [db] => {
            let d = ident(db);
            rows(c, &format!("SELECT s.name, count(o.object_id) FROM {d}.sys.schemas s JOIN {d}.sys.objects o ON o.schema_id = s.schema_id AND o.type IN ('U','V') GROUP BY s.name ORDER BY CASE s.name WHEN 'dbo' THEN 0 ELSE 1 END, s.name"))
                .await?
                .into_iter()
                .map(|r| Node { label: t(&r, 0), detail: t(&r, 1), kind: NodeKind::Schema, path: vec![db.clone(), t(&r, 0)] })
                .collect()
        }
        [db, schema] => {
            let d = ident(db);
            rows(
                c,
                &format!(
                    "SELECT o.name, o.type, (SELECT SUM(p.rows) FROM {d}.sys.partitions p WHERE p.object_id = o.object_id AND p.index_id IN (0,1)) \
                     FROM {d}.sys.objects o JOIN {d}.sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = {} AND o.type IN ('U','V') ORDER BY o.name",
                    lit(schema)
                ),
            )
            .await?
            .into_iter()
            .map(|r| {
                let view = t(&r, 1).trim() == "V";
                let n: u64 = t(&r, 2).parse().unwrap_or(0);
                Node {
                    label: t(&r, 0),
                    detail: if view { "view".into() } else if n > 0 { super::short_count(n) } else { String::new() },
                    kind: if view { NodeKind::View } else { NodeKind::Table },
                    path: vec![db.clone(), schema.clone(), t(&r, 0)],
                }
            })
            .collect()
        }
        _ => Vec::new(),
    })
}

pub async fn page(c: &mut Client, path: &[String], spec: &PageSpec) -> Result<Page> {
    let [db, schema, table] = path else { return Err(anyhow!("not a table")) };
    let from = format!("{}.{}.{} AS t", ident(db), ident(schema), ident(table));
    let filter = if spec.search.trim().is_empty() {
        String::new()
    } else {
        let pat = contains(spec.search.trim());
        let cols: Vec<String> = match &spec.column {
            Some(col) => vec![col.clone()],
            None => rows(c, &format!("SELECT c.name FROM {d}.sys.columns c WHERE c.object_id = OBJECT_ID({}) ORDER BY c.column_id", lit(&format!("{}.{}.{}", ident(db), ident(schema), ident(table))), d = ident(db)))
                .await?
                .into_iter()
                .filter_map(|r| r.into_iter().next().flatten())
                .collect(),
        };
        let any: Vec<String> = cols.iter().map(|col| format!("TRY_CAST(t.{} AS nvarchar(max)) LIKE {pat}", ident(col))).collect();
        if any.is_empty() { String::new() } else { format!(" WHERE {}", any.join(" OR ")) }
    };
    let order = spec.sort.as_ref().map(|(col, desc)| format!("t.{} {}", ident(col), if *desc { "DESC" } else { "ASC" })).unwrap_or_else(|| "(SELECT NULL)".into());
    let grid = run(c, &format!("SELECT * FROM {from}{filter} ORDER BY {order} OFFSET {} ROWS FETCH NEXT {} ROWS ONLY", spec.offset, spec.limit.max(1))).await?.into_iter().next().unwrap_or_default();
    let n: u64 = scalar(c, &format!("SELECT count(*) FROM (SELECT TOP {} 1 AS x FROM {from}{filter}) q", COUNT_CAP + 1)).await?.and_then(|s| s.parse().ok()).unwrap_or(0);
    Ok(Page { grid: Grid { note: String::new(), ..grid }, total: Some(n.min(COUNT_CAP)), estimate: n > COUNT_CAP })
}

pub async fn query(c: &mut Client, db: Option<&str>, text: &str) -> Result<Vec<Grid>> {
    if text.trim().is_empty() {
        return Ok(vec![Grid::note("Nothing to run")]);
    }
    // A query tab opened on a database runs there.
    let sql = match db {
        Some(d) => format!("USE {};\n{text}", ident(d)),
        None => text.to_string(),
    };
    run(c, &sql).await
}

/// A value as text (None is NULL).
fn text(d: &ColumnData<'static>) -> Option<String> {
    match d {
        ColumnData::U8(v) => v.map(|v| v.to_string()),
        ColumnData::I16(v) => v.map(|v| v.to_string()),
        ColumnData::I32(v) => v.map(|v| v.to_string()),
        ColumnData::I64(v) => v.map(|v| v.to_string()),
        ColumnData::F32(v) => v.map(|v| v.to_string()),
        ColumnData::F64(v) => v.map(|v| v.to_string()),
        ColumnData::Bit(v) => v.map(|v| if v { "1" } else { "0" }.to_string()),
        ColumnData::String(v) => v.as_ref().and_then(|s| cell(s.as_ref())),
        ColumnData::Guid(v) => v.map(|v| v.to_string().to_uppercase()),
        ColumnData::Binary(v) => v.as_ref().and_then(|b| {
            let mut s = String::from("0x");
            for x in b.iter().take(super::MAX_CELL / 2) {
                s.push_str(&format!("{x:02X}"));
            }
            if b.len() > super::MAX_CELL / 2 {
                s.push('…');
            }
            Some(s)
        }),
        ColumnData::Numeric(v) => v.map(|v| v.to_string()),
        ColumnData::Xml(v) => v.as_ref().and_then(|x| cell(x.to_string())),
        ColumnData::DateTime(v) => v.map(|v| datetime(v.days() as i64 - DAYS_1900, v.seconds_fragments() as u64 * 10_000_000 / 300, 3)),
        ColumnData::SmallDateTime(v) => v.map(|v| datetime(v.days() as i64 - DAYS_1900, v.seconds_fragments() as u64 * 60 * 10_000_000, 0)),
        ColumnData::Date(v) => v.map(|v| date(v.days() as i64 - DAYS_1)),
        ColumnData::Time(v) => v.map(|v| time(ticks(v.increments(), v.scale()), v.scale())),
        ColumnData::DateTime2(v) => v.map(|v| datetime(v.date().days() as i64 - DAYS_1, ticks(v.time().increments(), v.time().scale()), v.time().scale())),
        ColumnData::DateTimeOffset(v) => v.map(|v| {
            let d = v.datetime2();
            let (scale, off) = (d.time().scale(), v.offset() as i64);
            // Stored in UTC: shown at its own offset.
            let t = ticks(d.time().increments(), scale) as i64 + off * 60 * 10_000_000;
            let day = d.date().days() as i64 - DAYS_1 + t.div_euclid(DAY_TICKS);
            format!("{} {}{:02}:{:02}", datetime(day, t.rem_euclid(DAY_TICKS) as u64, scale), if off < 0 { '-' } else { '+' }, off.abs() / 60, off.abs() % 60)
        }),
    }
}

/// Days from 1900-01-01 and 0001-01-01 to 1970-01-01.
const DAYS_1900: i64 = 25_567;
const DAYS_1: i64 = 719_162;
const DAY_TICKS: i64 = 86_400 * 10_000_000;

/// Increments of 10^-scale seconds as 100ns ticks.
fn ticks(increments: u64, scale: u8) -> u64 {
    increments * 10u64.pow(7u32.saturating_sub(scale as u32)) / 10u64.pow((scale as u32).saturating_sub(7))
}

/// A date from days since 1970-01-01 (H. Hinnant's civil_from_days).
fn date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

fn time(ticks: u64, scale: u8) -> String {
    let secs = ticks / 10_000_000;
    let frac = ticks % 10_000_000;
    let base = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if scale == 0 {
        return base;
    }
    let digits = format!("{frac:07}");
    format!("{base}.{}", &digits[..(scale as usize).min(7)])
}

fn datetime(days: i64, ticks: u64, scale: u8) -> String {
    format!("{} {}", date(days), time(ticks, scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_times_read_like_sql_server_shows_them() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(-DAYS_1900), "1900-01-01");
        assert_eq!(date(19_723), "2024-01-01");
        assert_eq!(time(ticks(4_567_891, 3), 3), "01:16:07.891");
        assert_eq!(time(ticks(36_610, 0), 0), "10:10:10");
        assert_eq!(ident("we]ird"), "[we]]ird]");
        assert_eq!(contains("50%_off"), "N'%50[%][_]off%'");
    }
}
