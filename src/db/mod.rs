//! Database connections: saved settings, passwords (in the macOS Keychain), and the drivers.
//!
//! Every driver runs on one background thread (see `engine`), so a slow server never holds up
//! the window, and an open connection costs little: a socket and a small buffer. Results are
//! capped (`MAX_ROWS`, `MAX_CELL`) so a big table never fills memory.

pub mod engine;
mod mongo;
mod mssql;
mod pg;
mod rabbit;
mod redisdb;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The most rows a result keeps (a page of a table is far fewer).
pub const MAX_ROWS: usize = 1000;
/// The longest a cell's text is kept (longer values end with "…").
pub const MAX_CELL: usize = 2000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub enum Kind {
    #[default]
    Postgres,
    SqlServer,
    MongoDb,
    Redis,
    RabbitMq,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Postgres, Kind::SqlServer, Kind::MongoDb, Kind::Redis, Kind::RabbitMq];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Postgres => "PostgreSQL",
            Kind::SqlServer => "SQL Server",
            Kind::MongoDb => "MongoDB",
            Kind::Redis => "Redis",
            Kind::RabbitMq => "RabbitMQ",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Kind::Postgres => "postgres",
            Kind::SqlServer => "sqlserver",
            Kind::MongoDb => "mongodb",
            Kind::Redis => "redis",
            Kind::RabbitMq => "rabbitmq",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Kind::Postgres => 5432,
            Kind::SqlServer => 1433,
            Kind::MongoDb => 27017,
            Kind::Redis => 6379,
            Kind::RabbitMq => 15672,
        }
    }

    pub fn default_user(self) -> &'static str {
        match self {
            Kind::Postgres => "postgres",
            Kind::SqlServer => "sa",
            Kind::RabbitMq => "guest",
            Kind::MongoDb | Kind::Redis => "",
        }
    }

    /// The database field's default: PostgreSQL's database, SQL Server's, MongoDB's
    /// authentication database, Redis's number, RabbitMQ's virtual host.
    pub fn default_database(self) -> &'static str {
        match self {
            Kind::Postgres => "postgres",
            Kind::SqlServer => "master",
            Kind::MongoDb => "admin",
            Kind::Redis => "0",
            Kind::RabbitMq => "/",
        }
    }

    /// The brand color of its icon.
    pub fn color(self) -> u32 {
        match self {
            Kind::Postgres => 0x336791,
            Kind::SqlServer => 0xcc2927,
            Kind::MongoDb => 0x47a248,
            Kind::Redis => 0xdc382d,
            Kind::RabbitMq => 0xff6600,
        }
    }
}

/// A saved connection. The password (or the whole connection string, which may hold one)
/// is in the Keychain, never in the file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Conn {
    pub id: String,
    pub name: String,
    /// Folders in the panel, "Parent/Sub".
    pub group: String,
    /// Saved for this workspace (its folder) only; None: everywhere.
    pub project: Option<PathBuf>,
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// See `Kind::default_database`.
    pub database: String,
    /// Encrypt the connection (TLS).
    pub tls: bool,
    /// Accept the server's certificate even if it isn't trusted (self-signed).
    pub trust_cert: bool,
    /// Connect with a connection string (PostgreSQL, SQL Server, MongoDB): it's the secret.
    pub use_url: bool,
    /// MongoDB: talk to this server only, not the rest of its replica set.
    pub direct: bool,
}

impl Conn {
    pub fn new(kind: Kind) -> Conn {
        Conn {
            id: new_id(),
            kind,
            host: "127.0.0.1".into(),
            port: kind.default_port(),
            user: kind.default_user().into(),
            database: kind.default_database().into(),
            ..Default::default()
        }
    }

    /// The name shown: its own, else where it points.
    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        if self.use_url {
            return format!("{} connection", self.kind.label());
        }
        format!("{}@{}:{}", if self.user.is_empty() { self.kind.id() } else { &self.user }, self.host, self.port)
    }

    /// Whether it's shown in a window with these folders open.
    pub fn visible_in(&self, roots: &[PathBuf]) -> bool {
        self.project.as_ref().is_none_or(|p| roots.iter().any(|r| r == p))
    }
}

fn new_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    // The clock alone repeats (macOS counts microseconds): a counter keeps ids apart.
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:04x}{:04x}", n, std::process::id() & 0xffff, NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff)
}

// ---------------------------------------------------------------- saved connections

fn store_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/kite/connections.json"))
}

// Per test (thread): parallel tests don't see each other's connections.
#[cfg(test)]
thread_local! {
    static TEST_STORE: std::cell::RefCell<Vec<Conn>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// All saved connections (of every workspace).
pub fn load() -> Vec<Conn> {
    #[cfg(test)]
    return TEST_STORE.with(|s| s.borrow().clone());
    #[cfg(not(test))]
    store_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_all(all: &[Conn]) -> Result<(), String> {
    #[cfg(test)]
    {
        TEST_STORE.with(|s| *s.borrow_mut() = all.to_vec());
        return Ok(());
    }
    #[cfg(not(test))]
    {
        let path = store_path().ok_or("no home folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| e.to_string())
    }
}

/// Saves a connection (new or changed). `secret`: Some to store it (empty: forget it).
pub fn save(conn: &Conn, secret: Option<&str>) -> Result<(), String> {
    let mut all = load();
    match all.iter_mut().find(|c| c.id == conn.id) {
        Some(c) => *c = conn.clone(),
        None => all.push(conn.clone()),
    }
    save_all(&all)?;
    if let Some(s) = secret {
        set_secret(&conn.id, s)?;
    }
    engine::forget(&conn.id);
    Ok(())
}

pub fn delete(id: &str) -> Result<(), String> {
    let mut all = load();
    all.retain(|c| c.id != id);
    save_all(&all)?;
    set_secret(id, "").ok();
    engine::forget(id);
    Ok(())
}

// ---------------------------------------------------------------- Keychain

const KEYCHAIN_SERVICE: &str = "OSKL Code database";

#[cfg(test)]
static TEST_SECRETS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::LazyLock::new(Default::default);

/// The connection's password (or connection string); empty if none was saved.
pub fn secret(id: &str) -> String {
    #[cfg(test)]
    return TEST_SECRETS.lock().unwrap().get(id).cloned().unwrap_or_default();
    #[cfg(not(test))]
    security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, id).ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default()
}

fn set_secret(id: &str, secret: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        let mut s = TEST_SECRETS.lock().unwrap();
        if secret.is_empty() {
            s.remove(id);
        } else {
            s.insert(id.into(), secret.into());
        }
        return Ok(());
    }
    #[cfg(not(test))]
    {
        use security_framework::passwords::{delete_generic_password, set_generic_password};
        if secret.is_empty() {
            delete_generic_password(KEYCHAIN_SERVICE, id).ok();
            Ok(())
        } else {
            set_generic_password(KEYCHAIN_SERVICE, id, secret.as_bytes()).map_err(|e| format!("Keychain: {e}"))
        }
    }
}

// ---------------------------------------------------------------- what the drivers return

/// One thing in the tree under a connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub label: String,
    /// Shown dimmed after it (a count, a type).
    pub detail: String,
    pub kind: NodeKind,
    /// The path to it from the connection (database, schema, name…).
    pub path: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Database,
    Schema,
    Folder,
    Table,
    View,
    Collection,
    Key,
    Queue,
    Exchange,
}

impl NodeKind {
    /// Opens as a data view (else it expands).
    pub fn is_leaf(self) -> bool {
        matches!(self, NodeKind::Table | NodeKind::View | NodeKind::Collection | NodeKind::Key | NodeKind::Queue | NodeKind::Exchange)
    }

    pub fn icon(self) -> &'static str {
        match self {
            NodeKind::Database => "database",
            NodeKind::Schema | NodeKind::Folder => "folder",
            NodeKind::Table => "table",
            NodeKind::View => "eye",
            NodeKind::Collection => "collection",
            NodeKind::Key => "key",
            NodeKind::Queue => "queue",
            NodeKind::Exchange => "exchange",
        }
    }
}

/// A result: columns and rows of text (None is NULL), capped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// More rows came than were kept.
    pub more: bool,
    /// What happened when there are no rows ("3 rows affected", "OK").
    pub note: String,
}

impl Grid {
    pub fn note(note: impl Into<String>) -> Grid {
        Grid { note: note.into(), ..Default::default() }
    }

    /// Adds a row unless the result is full (then it's only counted as more).
    pub fn push(&mut self, row: Vec<Option<String>>) -> bool {
        if self.rows.len() >= MAX_ROWS {
            self.more = true;
            return false;
        }
        self.rows.push(row);
        true
    }

    /// A two-column field/value grid.
    pub fn pairs(items: impl IntoIterator<Item = (String, Option<String>)>) -> Grid {
        let mut g = Grid { columns: vec!["field".into(), "value".into()], ..Default::default() };
        for (k, v) in items {
            g.push(vec![Some(k), v]);
        }
        g
    }
}

/// A cell's text, cut at `MAX_CELL`.
pub fn cell(s: impl Into<String>) -> Option<String> {
    let mut s: String = s.into();
    if s.len() > MAX_CELL {
        let mut cut = MAX_CELL;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    Some(s)
}

/// 1234 → "1.2k", 5_600_000 → "5.6M".
pub fn short_count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

/// One page of a table or collection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageSpec {
    pub offset: usize,
    pub limit: usize,
    /// Rows where this text appears (in `column`, or in any).
    pub search: String,
    pub column: Option<String>,
    /// Sorted by this column (true: descending).
    pub sort: Option<(String, bool)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub grid: Grid,
    /// How many there are in all (with the search); `estimate` when it's the server's guess.
    pub total: Option<u64>,
    pub estimate: bool,
}

/// What a query runs in: the database (and MongoDB collection) its tab was opened on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    pub database: Option<String>,
    pub collection: Option<String>,
}

/// The text of a query that a data view's Query tab starts with.
pub fn starter_query(kind: Kind, path: &[String]) -> String {
    let name = path.last().cloned().unwrap_or_default();
    match kind {
        Kind::Postgres => match path {
            [_, schema, table] => format!("SELECT *\nFROM {}.{}\nLIMIT 100;", pg::ident(schema), pg::ident(table)),
            _ => "SELECT 1;".into(),
        },
        Kind::SqlServer => match path {
            [_, schema, table] => format!("SELECT TOP 100 *\nFROM {}.{};", mssql::ident(schema), mssql::ident(table)),
            _ => "SELECT 1;".into(),
        },
        Kind::MongoDb if path.len() == 2 => format!("db.{}.find({{}}).limit(100)", mongo::shell_name(&name)),
        Kind::MongoDb => "db.getCollectionNames()".into(),
        Kind::Redis if path.len() == 2 => format!("TYPE {}\nTTL {}", redisdb::quote(&name), redisdb::quote(&name)),
        Kind::Redis => "INFO keyspace".into(),
        Kind::RabbitMq => match path {
            [folder, vhost, name] if folder == "Queues" => format!("GET /api/queues/{}/{}", rabbit::enc(vhost), rabbit::enc(name)),
            [_, vhost, name] => format!("GET /api/exchanges/{}/{}/bindings/source", rabbit::enc(vhost), rabbit::enc(name)),
            _ => "GET /api/overview".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_are_saved_without_their_passwords() {
        let mut c = Conn::new(Kind::Postgres);
        c.name = "Local".into();
        save(&c, Some("s3cret")).unwrap();
        assert_eq!(load(), vec![c.clone()]);
        assert_eq!(secret(&c.id), "s3cret");
        assert!(!serde_json::to_string(&load()).unwrap().contains("s3cret"), "the password is in the Keychain only");
        let mut w = Conn::new(Kind::Redis);
        w.project = Some("/work/a".into());
        save(&w, None).unwrap();
        let roots = [PathBuf::from("/work/b")];
        assert!(c.visible_in(&roots) && !w.visible_in(&roots), "workspace connections show only in their workspace");
        assert!(w.visible_in(&[PathBuf::from("/work/a")]));
        delete(&c.id).unwrap();
        assert_eq!(load(), vec![w]);
        assert_eq!(secret(&c.id), "", "its password goes too");
    }

    #[test]
    fn grids_are_capped() {
        let mut g = Grid::default();
        for i in 0..MAX_ROWS + 5 {
            g.push(vec![Some(i.to_string())]);
        }
        assert_eq!(g.rows.len(), MAX_ROWS);
        assert!(g.more);
        let long = cell("x".repeat(MAX_CELL + 10)).unwrap();
        assert!(long.ends_with('…') && long.len() <= MAX_CELL + 3);
    }
}

/// Against real servers (run with KITE_DB_LIVE=1; see the ports below).
#[cfg(test)]
mod live {
    use super::*;

    fn live() -> bool {
        std::env::var_os("KITE_DB_LIVE").is_some()
    }

    fn conn(kind: Kind, port: u16, user: &str, pw: &str, db: &str) -> Conn {
        let mut c = Conn::new(kind);
        c.port = port;
        c.user = user.into();
        c.database = db.into();
        set_secret(&c.id, pw).unwrap();
        c
    }

    fn wait<T: Send + 'static>(f: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static) -> Result<T, String> {
        futures::executor::block_on(engine::run(f))
    }

    fn labels(n: &[Node]) -> Vec<String> {
        n.iter().map(|n| n.label.clone()).collect()
    }

    fn spec(limit: usize) -> PageSpec {
        PageSpec { limit, ..Default::default() }
    }

    #[test]
    fn postgres() {
        if !live() {
            return;
        }
        let c = conn(Kind::Postgres, 55432, "postgres", "testpw", "postgres");
        let v = wait(engine::test(c.clone(), "testpw".into())).unwrap();
        assert!(v.starts_with("PostgreSQL 16"), "{v}");
        assert!(wait(engine::test(c.clone(), "wrong".into())).unwrap_err().contains("password"), "a wrong password says so");
        let dbs = wait(engine::children(c.clone(), vec![])).unwrap();
        assert!(labels(&dbs).contains(&"crm".into()), "{dbs:?}");
        let schemas = wait(engine::children(c.clone(), vec!["crm".into()])).unwrap();
        assert_eq!(labels(&schemas), ["public", "sales"], "public first");
        let rels = wait(engine::children(c.clone(), vec!["crm".into(), "public".into()])).unwrap();
        assert_eq!(rels.iter().map(|n| (n.label.as_str(), n.kind)).collect::<Vec<_>>(), [("cheap", NodeKind::View), ("fruit_order", NodeKind::Table)]);
        let path: Vec<String> = vec!["crm".into(), "public".into(), "fruit_order".into()];
        let p = wait(engine::page(c.clone(), path.clone(), spec(5))).unwrap();
        assert_eq!(p.grid.columns, ["name", "qty", "note", "at"]);
        assert_eq!((p.grid.rows.len(), p.total, p.estimate), (5, Some(11), false));
        let p = wait(engine::page(c.clone(), path.clone(), PageSpec { limit: 25, search: "AN".into(), sort: Some(("qty".into(), true)), ..Default::default() })).unwrap();
        let names: Vec<_> = p.grid.rows.iter().map(|r| r[0].clone().unwrap()).collect();
        assert_eq!(names, ["Banana", "Orange", "Mango"], "searched in every column, ignoring case, sorted by qty descending");
        let p = wait(engine::page(c.clone(), path.clone(), PageSpec { limit: 25, search: "1".into(), column: Some("qty".into()), ..Default::default() })).unwrap();
        assert_eq!(p.total, Some(4), "10, 15, 12, 18");
        let big = wait(engine::page(c.clone(), vec!["crm".into(), "sales".into(), "big".into()], spec(50))).unwrap();
        assert_eq!(big.grid.rows.len(), 50);
        assert!(big.estimate && big.total.unwrap() > 200_000, "a big table's count is the server's estimate: {:?}", big.total);
        let scope = Scope { database: Some("crm".into()), collection: None };
        let r = wait(engine::query(c.clone(), scope.clone(), "UPDATE fruit_order SET qty = 7 WHERE name = 'Kiwi'; SELECT name, qty FROM fruit_order WHERE name = 'Kiwi'; SELECT * FROM sales.big".into())).unwrap();
        assert_eq!(r[0].note, "1 row affected");
        assert_eq!(r[1].rows, vec![vec![Some("Kiwi".into()), Some("7".into())]]);
        assert_eq!((r[2].rows.len(), r[2].more), (MAX_ROWS, true), "a huge result keeps only the first rows");
        let e = wait(engine::query(c.clone(), scope, "SELECT nope FROM fruit_order".into())).unwrap_err();
        assert!(e.contains("column \"nope\" does not exist"), "{e}");
        assert!(engine::connected(&c.id));
        engine::forget(&c.id);
        assert!(!engine::connected(&c.id));
    }

    #[test]
    fn redis() {
        if !live() {
            return;
        }
        let c = conn(Kind::Redis, 56379, "", "redispw", "0");
        assert!(wait(engine::test(c.clone(), "redispw".into())).unwrap().starts_with("Redis "));
        let r = wait(engine::query(
            c.clone(),
            Scope::default(),
            "FLUSHDB\nSET user:1 \"Ann Lee\"\nHSET session:abc ip 1.2.3.4 agent \"Safari 18\"\nRPUSH jobs a b c\nZADD rank 3 x 1 y\nEXPIRE user:1 600\nGET user:1".into(),
        ))
        .unwrap();
        assert_eq!(r.last().unwrap().rows, vec![vec![Some("Ann Lee".into())]]);
        let dbs = wait(engine::children(c.clone(), vec![])).unwrap();
        assert_eq!(dbs[0].label, "db0");
        assert_eq!(dbs[0].detail, "4");
        let keys = wait(engine::children(c.clone(), vec!["db0".into()])).unwrap();
        assert_eq!(labels(&keys), ["jobs", "rank", "session:abc", "user:1"]);
        let h = wait(engine::page(c.clone(), vec!["db0".into(), "session:abc".into()], spec(25))).unwrap();
        assert_eq!(h.grid.columns, ["field", "value"]);
        assert_eq!(h.grid.rows.len(), 2);
        assert!(h.grid.note.starts_with("hash · no expiry"), "{}", h.grid.note);
        let z = wait(engine::page(c.clone(), vec!["db0".into(), "rank".into()], spec(25))).unwrap();
        assert_eq!(z.grid.rows, vec![vec![Some("y".into()), Some("1".into())], vec![Some("x".into()), Some("3".into())]]);
        let l = wait(engine::page(c.clone(), vec!["db0".into(), "jobs".into()], PageSpec { offset: 1, limit: 1, ..Default::default() })).unwrap();
        assert_eq!((l.grid.rows.clone(), l.total), (vec![vec![Some("1".into()), Some("b".into())]], Some(3)));
        let s = wait(engine::page(c.clone(), vec!["db0".into(), "user:1".into()], spec(25))).unwrap();
        assert!(s.grid.note.contains("expires in"), "{}", s.grid.note);
        assert!(wait(engine::query(c.clone(), Scope::default(), "NOSUCH x".into())).unwrap_err().contains("NOSUCH"));
    }

    #[test]
    fn mongodb() {
        if !live() {
            return;
        }
        let c = conn(Kind::MongoDb, 57017, "root", "mongopw", "admin");
        assert!(wait(engine::test(c.clone(), "mongopw".into())).unwrap().starts_with("MongoDB 7"));
        let scope = Scope { database: Some("shop".into()), collection: None };
        let r = wait(engine::query(
            c.clone(),
            scope.clone(),
            "db.users.drop()\ndb.users.insertMany([{name: 'Ann', age: 34, tags: ['a']}, {name: 'Bob', age: 25}, {name: 'Cy', age: 41, at: ISODate('2024-05-01')}])\n\
             db.users.updateOne({name: 'Bob'}, {$set: {age: 26}})\ndb.users.find({age: {$gt: 30}}, {_id: 0}).sort({age: -1})\ndb.users.countDocuments({})"
                .into(),
        ))
        .unwrap();
        assert_eq!(r[1].note, "Inserted 3 documents");
        assert_eq!(r[2].note, "Matched 1 · modified 1");
        assert_eq!(r[3].columns, ["name", "age", "at", "tags"], "in the order they first appear (Cy, the oldest, comes first)");
        assert_eq!(r[3].rows.iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>(), ["Cy", "Ann"]);
        assert_eq!(r[3].rows[0][2].as_deref(), Some("2024-05-01T00:00:00Z"));
        assert_eq!(r[4].rows, vec![vec![Some("3".into())]]);
        let dbs = wait(engine::children(c.clone(), vec![])).unwrap();
        assert_eq!(dbs[0].label, "shop", "your databases before admin/config/local");
        let colls = wait(engine::children(c.clone(), vec!["shop".into()])).unwrap();
        assert_eq!(labels(&colls), ["users"]);
        let path: Vec<String> = vec!["shop".into(), "users".into()];
        let p = wait(engine::page(c.clone(), path.clone(), PageSpec { limit: 2, sort: Some(("age".into(), false)), ..Default::default() })).unwrap();
        assert_eq!((p.total, p.grid.rows.len()), (Some(3), 2));
        assert_eq!(p.grid.columns[0], "_id");
        let p = wait(engine::page(c.clone(), path.clone(), PageSpec { limit: 25, search: "bo".into(), ..Default::default() })).unwrap();
        assert_eq!(p.total, Some(1), "searched in any field");
        let in_coll = Scope { database: Some("shop".into()), collection: Some("users".into()) };
        let r = wait(engine::query(c.clone(), in_coll.clone(), "{name: 'Ann'}".into())).unwrap();
        assert_eq!(r[0].rows.len(), 1, "a bare filter in a collection's tab finds");
        let r = wait(engine::query(c.clone(), in_coll, "[{$group: {_id: null, avg: {$avg: '$age'}}}]".into())).unwrap();
        assert_eq!(r[0].columns, ["_id", "avg"], "a bare pipeline aggregates");
        let e = wait(engine::query(c.clone(), scope, "db.users.find({name: })".into())).unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn sql_server() {
        if !live() {
            return;
        }
        let c = conn(Kind::SqlServer, 51433, "sa", "Kite_Test_pw1", "master");
        let v = wait(engine::test(c.clone(), "Kite_Test_pw1".into())).unwrap();
        assert!(v.starts_with("Microsoft") && v.contains("SQL"), "{v}");
        wait(engine::query(
            c.clone(),
            Scope::default(),
            "IF DB_ID('crm') IS NULL CREATE DATABASE crm;".into(),
        ))
        .unwrap();
        let scope = Scope { database: Some("crm".into()), collection: None };
        let r = wait(engine::query(
            c.clone(),
            scope.clone(),
            "DROP TABLE IF EXISTS dbo.fruit; CREATE TABLE dbo.fruit (id int identity primary key, name nvarchar(50), qty int, price decimal(6,2), at datetime2(3), d date, u uniqueidentifier);\n\
             INSERT INTO dbo.fruit (name, qty, price, at, d, u) VALUES (N'Apple', 10, 1.25, '2024-03-04 05:06:07.891', '2024-03-04', 'C56A4180-65AA-42EC-A945-5FD21DEC0538'), (N'Banana', 20, NULL, NULL, NULL, NULL), (N'Mango', 15, 2.5, NULL, NULL, NULL);\n\
             SELECT name, qty, price, at, d, u FROM dbo.fruit ORDER BY id;"
                .into(),
        ))
        .unwrap();
        let g = r.last().unwrap();
        assert_eq!(g.columns, ["name", "qty", "price", "at", "d", "u"]);
        assert_eq!(g.rows[0], vec![Some("Apple".into()), Some("10".into()), Some("1.25".into()), Some("2024-03-04 05:06:07.891".into()), Some("2024-03-04".into()), Some("C56A4180-65AA-42EC-A945-5FD21DEC0538".into())]);
        assert_eq!(g.rows[1][2], None, "NULL");
        let dbs = wait(engine::children(c.clone(), vec![])).unwrap();
        assert_eq!(dbs[0].label, "crm", "your databases before the system ones");
        let schemas = wait(engine::children(c.clone(), vec!["crm".into()])).unwrap();
        assert_eq!(labels(&schemas), ["dbo"]);
        let tables = wait(engine::children(c.clone(), vec!["crm".into(), "dbo".into()])).unwrap();
        assert_eq!(labels(&tables), ["fruit"]);
        let path: Vec<String> = vec!["crm".into(), "dbo".into(), "fruit".into()];
        let p = wait(engine::page(c.clone(), path.clone(), PageSpec { limit: 2, offset: 1, sort: Some(("qty".into(), true)), ..Default::default() })).unwrap();
        assert_eq!(p.total, Some(3));
        assert_eq!(p.grid.rows.iter().map(|r| r[1].clone().unwrap()).collect::<Vec<_>>(), ["Mango", "Apple"], "page 2 of qty descending");
        let p = wait(engine::page(c.clone(), path, PageSpec { limit: 25, search: "an".into(), ..Default::default() })).unwrap();
        assert_eq!(p.total, Some(2), "Banana, Mango");
        assert!(wait(engine::query(c.clone(), scope, "SELECT nope FROM dbo.fruit".into())).unwrap_err().contains("nope"));
    }

    #[test]
    fn rabbitmq() {
        if !live() {
            return;
        }
        let c = conn(Kind::RabbitMq, 15673, "guest", "guest", "/");
        assert!(wait(engine::test(c.clone(), "guest".into())).unwrap().starts_with("RabbitMQ 3"));
        let r = wait(engine::query(
            c.clone(),
            Scope::default(),
            "PUT /api/queues/%2F/orders\n{\"durable\": true}\nPOST /api/exchanges/%2F/amq.default/publish\n{\"properties\": {}, \"routing_key\": \"orders\", \"payload\": \"hello\", \"payload_encoding\": \"string\"}".into(),
        ))
        .unwrap();
        assert_eq!(r[1].rows, vec![vec![Some("routed".into()), Some("true".into())]]);
        let top = wait(engine::children(c.clone(), vec![])).unwrap();
        assert_eq!(labels(&top), ["Queues", "Exchanges"]);
        let qs = wait(engine::children(c.clone(), vec!["Queues".into()])).unwrap();
        assert!(labels(&qs).contains(&"orders".into()), "{qs:?}");
        let ex = wait(engine::children(c.clone(), vec!["Exchanges".into()])).unwrap();
        assert!(labels(&ex).contains(&"(AMQP default)".into()));
        let q = wait(engine::page(c.clone(), vec!["Queues".into(), "/".into(), "orders".into()], spec(25))).unwrap();
        assert_eq!(q.grid.rows[0][0].as_deref(), Some("state"));
        let bad = conn(Kind::RabbitMq, 15673, "guest", "nope", "/");
        assert!(wait(engine::test(bad, "nope".into())).unwrap_err().contains("wrong user name or password"));
        let off = conn(Kind::RabbitMq, 15999, "guest", "guest", "/");
        assert!(wait(engine::test(off, "guest".into())).unwrap_err().contains("management plugin"));
    }
}

/// What connections cost (KITE_DB_LIVE=1, all five servers): printed, and kept small.
#[cfg(test)]
mod live_memory {
    use super::*;

    fn footprint_kb() -> f64 {
        let out = std::process::Command::new("footprint").arg(std::process::id().to_string()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find(|l| l.contains("Footprint:")).unwrap_or("").to_string();
        let v: Vec<&str> = line.split("Footprint:").nth(1).unwrap_or("").split_whitespace().collect();
        let n: f64 = v.first().and_then(|n| n.parse().ok()).unwrap_or(0.);
        if v.get(1) == Some(&"MB") { n * 1024. } else { n }
    }

    #[test]
    fn connections_cost_little() {
        if std::env::var_os("KITE_DB_LIVE").is_none() {
            return;
        }
        fn wait<T>(f: impl std::future::Future<Output = T>) -> T {
            futures::executor::block_on(f)
        }
        let mk = |kind: Kind, port: u16, user: &str, pw: &str, db: &str| {
            let mut c = Conn::new(kind);
            c.port = port;
            c.user = user.into();
            c.database = db.into();
            set_secret(&c.id, pw).unwrap();
            c
        };
        let conns = [
            mk(Kind::Postgres, 55432, "postgres", "testpw", "postgres"),
            mk(Kind::SqlServer, 51433, "sa", "Kite_Test_pw1", "master"),
            mk(Kind::MongoDb, 57017, "root", "mongopw", "admin"),
            mk(Kind::Redis, 56379, "", "redispw", "0"),
            mk(Kind::RabbitMq, 15673, "guest", "guest", "/"),
        ];
        // The runtime and TLS start once, whatever the first connection.
        wait(engine::run(async { Ok::<(), anyhow::Error>(()) })).unwrap();
        let start = footprint_kb();
        let mut last = start;
        for c in &conns {
            let path = match c.kind {
                Kind::Postgres => vec!["crm".to_string(), "public".into()],
                Kind::SqlServer => vec!["crm".to_string(), "dbo".into()],
                Kind::MongoDb => vec!["shop".to_string()],
                Kind::Redis => vec!["db0".to_string()],
                Kind::RabbitMq => vec!["Queues".to_string()],
            };
            wait(engine::run(engine::children(c.clone(), vec![]))).unwrap();
            wait(engine::run(engine::children(c.clone(), path))).unwrap();
            let now = footprint_kb();
            eprintln!("[db memory] {}: +{:.1} MB", c.kind.label(), (now - last) / 1024.);
            last = now;
        }
        // A big result: 1000 rows kept.
        let big = wait(engine::run(engine::query(conns[0].clone(), Scope { database: Some("crm".into()), collection: None }, "SELECT * FROM sales.big".into()))).unwrap();
        assert_eq!(big[0].rows.len(), MAX_ROWS);
        let after_query = footprint_kb();
        eprintln!("[db memory] 250,000-row SELECT (1000 kept): +{:.1} MB", (after_query - last) / 1024.);
        eprintln!("[db memory] all five open: +{:.1} MB", (after_query - start) / 1024.);
        drop(big);
        for c in &conns {
            engine::forget(&c.id);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        eprintln!("[db memory] after closing: +{:.1} MB", (footprint_kb() - start) / 1024.);
        assert!(after_query - start < 60. * 1024., "five connections and a big result stay well under 60 MB");
    }
}
