//! The thread every database runs on: one small tokio runtime (current-thread), so all
//! connections share one OS thread. A connection opens the first time it's used and closes
//! after `IDLE` without use; the next use opens it again, so nothing waits to be cleaned up.

use super::{Conn, Grid, Kind, Node, Page, PageSpec, Scope};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long an unused connection stays open.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// How long to wait for a server to answer when connecting.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn runtime() -> &'static tokio::runtime::Handle {
    static RT: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    RT.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().thread_name("oskl-db").build().expect("database runtime");
        let handle = rt.handle().clone();
        std::thread::Builder::new()
            .name("oskl-db".into())
            .stack_size(1 << 20)
            .spawn(move || rt.block_on(close_idle()))
            .expect("database thread");
        handle
    })
}

/// Stops the work when what's waiting for it goes (a newer query, Stop, the tab closed).
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `work` on the database thread; awaitable from GPUI's executor. Dropping the returned
/// future stops the work.
pub fn run<T: Send + 'static>(work: impl Future<Output = Result<T>> + Send + 'static) -> impl Future<Output = Result<T, String>> {
    let task = AbortOnDrop(runtime().spawn(async move { tokio::time::timeout(Duration::from_secs(10 * 60), work).await.map_err(|_| anyhow!("timed out"))? }));
    async move {
        let mut task = task;
        match (&mut task.0).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(clean_error(&format!("{e:#}"))),
            Err(_) => Err("stopped".into()),
        }
    }
}

/// Error text without the drivers' repetition ("error: error: …").
fn clean_error(e: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in e.split(": ") {
        if parts.last() != Some(&p) && !matches!(p, "error" | "Error" | "db error") {
            parts.push(p);
        }
    }
    parts.join(": ")
}

pub(super) enum Client {
    Pg(tokio_postgres::Client),
    Mssql(tokio::sync::Mutex<super::mssql::Client>),
    Mongo(mongodb::Client),
    Redis(redis::aio::MultiplexedConnection),
    Rabbit(Arc<super::rabbit::Api>),
}

struct Session {
    client: Arc<Client>,
    used: Instant,
}

/// Open connections, by connection id and what they're on (a PostgreSQL database, a Redis
/// database number; empty for the rest).
static SESSIONS: LazyLock<Mutex<HashMap<(String, String), Session>>> = LazyLock::new(Default::default);

async fn close_idle() {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let gone: Vec<Session> = {
            let mut s = SESSIONS.lock().unwrap();
            let old: Vec<_> = s.iter().filter(|(_, v)| v.used.elapsed() > IDLE).map(|(k, _)| k.clone()).collect();
            old.into_iter().filter_map(|k| s.remove(&k)).collect()
        };
        drop(gone);
    }
}

/// Connections the user connected (from the panel). Only these are used: a tab never
/// connects on its own. One that closed while idle opens again when used.
static CONNECTED: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);

/// The user connected it.
pub fn open(id: &str) {
    CONNECTED.lock().unwrap().insert(id.to_string());
}

/// Closes a connection (it was changed, deleted, or disconnected).
pub fn forget(id: &str) {
    CONNECTED.lock().unwrap().remove(id);
    let gone: Vec<Session> = {
        let mut s = SESSIONS.lock().unwrap();
        let keys: Vec<_> = s.keys().filter(|k| k.0 == id).cloned().collect();
        keys.into_iter().filter_map(|k| s.remove(&k)).collect()
    };
    // Dropped on the database thread: some drivers close politely there.
    if !gone.is_empty() {
        runtime().spawn(async move { drop(gone) });
    }
}

/// A saved password (or connection string), read off the main thread.
pub async fn secret(id: String) -> Result<String> {
    Ok(tokio::task::spawn_blocking(move || super::secret(&id)).await?)
}

/// Whether the user has it connected.
pub fn connected(id: &str) -> bool {
    CONNECTED.lock().unwrap().contains(id)
}

/// Said when a tab needs a connection that isn't connected.
pub const NOT_CONNECTED: &str = "Not connected. Connect from the Databases panel.";

/// The open session for `conn` on `sub`, connecting if there's none (or it broke).
async fn session(conn: &Conn, sub: &str) -> Result<Arc<Client>> {
    if !connected(&conn.id) {
        return Err(anyhow!(NOT_CONNECTED));
    }
    let key = (conn.id.clone(), sub.to_string());
    if let Some(s) = SESSIONS.lock().unwrap().get_mut(&key) {
        let alive = match &*s.client {
            Client::Pg(c) => !c.is_closed(),
            _ => true,
        };
        if alive {
            s.used = Instant::now();
            return Ok(s.client.clone());
        }
    }
    // Off this thread: the Keychain may ask the user first (after an update of the app), and
    // other connections shouldn't wait for that answer.
    let id = conn.id.clone();
    let secret = tokio::task::spawn_blocking(move || super::secret(&id)).await?;
    let client = Arc::new(connect(conn, &secret, sub).await?);
    SESSIONS.lock().unwrap().insert(key, Session { client: client.clone(), used: Instant::now() });
    Ok(client)
}

async fn connect(conn: &Conn, secret: &str, sub: &str) -> Result<Client> {
    let work = async {
        Ok::<_, anyhow::Error>(match conn.kind {
            Kind::Postgres => Client::Pg(super::pg::connect(conn, secret, sub).await?),
            Kind::SqlServer => Client::Mssql(tokio::sync::Mutex::new(super::mssql::connect(conn, secret).await?)),
            Kind::MongoDb => Client::Mongo(super::mongo::connect(conn, secret).await?),
            Kind::Redis => Client::Redis(super::redisdb::connect(conn, secret, sub).await?),
            Kind::RabbitMq => Client::Rabbit(Arc::new(super::rabbit::Api::new(conn, secret)?)),
        })
    };
    tokio::time::timeout(CONNECT_TIMEOUT, work).await.map_err(|_| anyhow!("no answer from {}:{} in {}s", conn.host, conn.port, CONNECT_TIMEOUT.as_secs()))?
}

/// Which session a tree path or scope needs.
fn sub_for(conn: &Conn, first: Option<&str>) -> String {
    match conn.kind {
        Kind::Postgres => first.map(str::to_string).unwrap_or_else(|| conn.database.clone()),
        Kind::Redis => first.and_then(|d| d.strip_prefix("db")).map(str::to_string).unwrap_or_else(|| conn.database.clone()),
        _ => String::new(),
    }
}

/// Connects with settings not saved yet (the form's Test button): the server's version.
pub async fn test(conn: Conn, secret: String) -> Result<String> {
    let sub = sub_for(&conn, None);
    let client = connect(&conn, &secret, &sub).await?;
    match &client {
        Client::Pg(c) => super::pg::version(c).await,
        Client::Mssql(c) => super::mssql::version(&mut *c.lock().await).await,
        Client::Mongo(c) => super::mongo::version(c).await,
        Client::Redis(c) => super::redisdb::version(&mut c.clone()).await,
        Client::Rabbit(api) => {
            let api = api.clone();
            tokio::task::spawn_blocking(move || api.version()).await?
        }
    }
}

/// What's under `path` in the connection's tree ([] is the connection itself).
pub async fn children(conn: Conn, path: Vec<String>) -> Result<Vec<Node>> {
    let client = session(&conn, &sub_for(&conn, path.first().map(String::as_str))).await?;
    match &*client {
        Client::Pg(c) => super::pg::children(c, &conn, &path).await,
        Client::Mssql(c) => super::mssql::children(&mut *c.lock().await, &path).await,
        Client::Mongo(c) => super::mongo::children(c, &path).await,
        Client::Redis(c) => super::redisdb::children(&mut c.clone(), &conn, &path).await,
        Client::Rabbit(api) => {
            let api = api.clone();
            tokio::task::spawn_blocking(move || api.children(&path)).await?
        }
    }
}

/// A page of the table, collection, key or queue at `path`.
pub async fn page(conn: Conn, path: Vec<String>, spec: PageSpec) -> Result<Page> {
    let client = session(&conn, &sub_for(&conn, path.first().map(String::as_str))).await?;
    match &*client {
        Client::Pg(c) => super::pg::page(c, &path, &spec).await,
        Client::Mssql(c) => super::mssql::page(&mut *c.lock().await, &path, &spec).await,
        Client::Mongo(c) => super::mongo::page(c, &path, &spec).await,
        Client::Redis(c) => super::redisdb::page(&mut c.clone(), &path, &spec).await,
        Client::Rabbit(api) => {
            let api = api.clone();
            tokio::task::spawn_blocking(move || api.page(&path, &spec)).await?
        }
    }
}

/// Runs `work` with a MongoDB connection's client (Compass's tabs).
pub async fn mongo<T, F, Fut>(conn: Conn, work: F) -> Result<T>
where
    F: FnOnce(mongodb::Client) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let client = session(&conn, "").await?;
    match &*client {
        Client::Mongo(c) => work(c.clone()).await,
        _ => Err(anyhow!("not a MongoDB connection")),
    }
}

/// Runs a query (SQL, a MongoDB shell command, Redis commands, RabbitMQ API calls): one
/// result per statement.
pub async fn query(conn: Conn, scope: Scope, text: String) -> Result<Vec<Grid>> {
    let client = session(&conn, &sub_for(&conn, scope.database.as_deref())).await?;
    match &*client {
        Client::Pg(c) => super::pg::query(c, &text).await,
        Client::Mssql(c) => super::mssql::query(&mut *c.lock().await, scope.database.as_deref(), &text).await,
        Client::Mongo(c) => super::mongo::query(c, &scope, &text).await,
        Client::Redis(c) => super::redisdb::query(&mut c.clone(), &text).await,
        Client::Rabbit(api) => {
            let api = api.clone();
            tokio::task::spawn_blocking(move || api.query(&text)).await?
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dropped_work_stops() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DONE: AtomicBool = AtomicBool::new(false);
        let work = super::run(async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            DONE.store(true, Ordering::SeqCst);
            Ok(())
        });
        drop(work);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!DONE.load(Ordering::SeqCst), "stopped when nobody waits for it");
    }

    #[test]
    fn driver_errors_read_cleanly() {
        assert_eq!(super::clean_error("db error: error: relation \"x\" does not exist"), "relation \"x\" does not exist");
    }
}
