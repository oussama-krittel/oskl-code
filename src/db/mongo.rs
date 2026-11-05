//! MongoDB (the official driver, pool of at most 2). Queries are written like in mongosh,
//! `db.users.find({age: {$gt: 30}}).sort({name: 1})`, or as a command document; arguments
//! use the shell's relaxed syntax (bare keys, 'quotes', ObjectId(…), ISODate(…), /regex/).

use super::{Conn, Grid, Node, NodeKind, Page, PageSpec, Scope, Tree, cell};
use anyhow::{Result, anyhow, bail};
use futures::TryStreamExt;
use mongodb::Client;
use mongodb::bson::{Bson, Document, doc, oid::ObjectId};
use mongodb::options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions};

const COUNT_CAP: u64 = 100_000;

pub async fn connect(conn: &Conn, secret: &str) -> Result<Client> {
    let mut opts = if conn.use_url {
        match ClientOptions::parse(secret).await {
            Ok(o) => o,
            // mongodb+srv:// looks the servers up in DNS. The driver can't read some system DNS
            // settings (an IPv6 link-local server like fe80::…%en0, common on home routers):
            // then a public resolver finds them (cluster names, e.g. Atlas's, are public).
            Err(e) if e.to_string().contains("DNS") => ClientOptions::parse(secret)
                .resolver_config(mongodb::options::ResolverConfig::cloudflare())
                .await
                .map_err(|e2| anyhow!("connection string: {e2} (and with the system's DNS: {e})"))?,
            Err(e) => return Err(anyhow!("connection string: {e}")),
        }
    } else {
        let mut o = ClientOptions::builder().hosts(vec![ServerAddress::Tcp { host: conn.host.clone(), port: Some(conn.port) }]).build();
        if !conn.user.is_empty() {
            o.credential = Some(
                Credential::builder()
                    .username(conn.user.clone())
                    .password(secret.to_string())
                    .source((!conn.database.is_empty()).then(|| conn.database.clone()))
                    .build(),
            );
        }
        if conn.tls {
            o.tls = Some(Tls::Enabled(TlsOptions::builder().allow_invalid_certificates(conn.trust_cert).build()));
        }
        o.direct_connection = Some(conn.direct);
        o
    };
    // Small: a browser needs a connection or two, not the default 10.
    opts.max_pool_size = Some(2);
    opts.min_pool_size = Some(0);
    opts.max_idle_time = Some(std::time::Duration::from_secs(120));
    opts.app_name = Some("OSKL Code".into());
    opts.connect_timeout = Some(super::engine::CONNECT_TIMEOUT);
    opts.server_selection_timeout = Some(super::engine::CONNECT_TIMEOUT);
    let client = Client::with_options(opts)?;
    // The driver connects lazily: check now, so a wrong address or password says so.
    client.database("admin").run_command(doc! { "ping": 1 }).await?;
    Ok(client)
}

pub async fn version(c: &Client) -> Result<String> {
    let info = c.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
    Ok(format!("MongoDB {}", info.get_str("version").unwrap_or("?")))
}

/// How to write a collection's name after `db.` (or with getCollection when it isn't a name).
pub fn shell_name(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain { name.to_string() } else { format!("getCollection({})", serde_json::to_string(name).unwrap_or_default()) }
}

pub async fn children(c: &Client, path: &[String]) -> Result<Vec<Node>> {
    Ok(match path {
        [] => {
            let mut names = c.list_database_names().await?;
            names.sort_by_key(|n| (matches!(n.as_str(), "admin" | "config" | "local"), n.clone()));
            names.into_iter().map(|n| Node { label: n.clone(), detail: String::new(), kind: NodeKind::Database, path: vec![n] }).collect()
        }
        [db] => {
            let mut specs: Vec<_> = c.database(db).list_collections().await?.try_collect().await?;
            specs.sort_by(|a, b| a.name.cmp(&b.name));
            specs
                .into_iter()
                .map(|s| {
                    let view = matches!(s.collection_type, mongodb::results::CollectionType::View);
                    Node { label: s.name.clone(), detail: if view { "view".into() } else { String::new() }, kind: if view { NodeKind::View } else { NodeKind::Collection }, path: vec![db.clone(), s.name] }
                })
                .collect()
        }
        _ => Vec::new(),
    })
}

/// A regex matching `s` literally, ignoring case.
fn contains(s: &str) -> Bson {
    Bson::Document(doc! { "$regex": regex_escape(s), "$options": "i" })
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub async fn page(c: &Client, path: &[String], spec: &PageSpec) -> Result<Page> {
    let [db, name] = path else { bail!("not a collection") };
    let coll = c.database(db).collection::<Document>(name);
    let filter = if spec.search.trim().is_empty() {
        Document::new()
    } else if let Some(col) = &spec.column {
        doc! { col: contains(spec.search.trim()) }
    } else {
        // Any field: the ones a sample of documents has.
        let sample: Vec<Document> = coll.find(doc! {}).limit(50).await?.try_collect().await?;
        let keys = columns(&sample);
        doc! { "$or": keys.into_iter().map(|k| Bson::Document(doc! { k: contains(spec.search.trim()) })).collect::<Vec<_>>() }
    };
    let mut find = coll.find(filter.clone()).skip(spec.offset as u64).limit(spec.limit as i64);
    if let Some((col, desc)) = &spec.sort {
        find = find.sort(doc! { col: if *desc { -1 } else { 1 } });
    }
    let docs: Vec<Document> = find.await?.try_collect().await?;
    let (total, estimate) = if filter.is_empty() {
        (coll.estimated_document_count().await.unwrap_or(0), false)
    } else {
        let n = coll.count_documents(filter).limit(COUNT_CAP + 1).await?;
        (n.min(COUNT_CAP), n > COUNT_CAP)
    };
    Ok(Page { grid: grid(&docs), total: Some(total), estimate })
}

/// The top-level fields of documents, `_id` first, in the order they appear (at most 100).
fn columns(docs: &[Document]) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    for d in docs {
        for k in d.keys() {
            if cols.len() < 100 && !cols.iter().any(|c| c == k) {
                cols.push(k.clone());
            }
        }
    }
    if let Some(i) = cols.iter().position(|c| c == "_id") {
        let id = cols.remove(i);
        cols.insert(0, id);
    }
    cols
}

fn grid(docs: &[Document]) -> Grid {
    let mut g = Grid { columns: columns(docs), ..Default::default() };
    let mut trees = Vec::new();
    for d in docs {
        let row = g.columns.iter().map(|c| d.get(c).and_then(show)).collect();
        if g.push(row) {
            trees.push(tree(&Bson::Document(d.clone())));
        }
    }
    g.trees = Some(trees);
    g
}

/// The longest a string in a tree is kept.
const MAX_TREE_STR: usize = 100_000;

/// A value with its type, for the List / JSON views (strings past MAX_TREE_STR are cut).
pub fn tree(v: &Bson) -> Tree {
    match v {
        Bson::Document(d) => Tree::Object(d.iter().map(|(k, v)| (k.clone(), tree(v))).collect()),
        Bson::Array(a) => Tree::Array(a.iter().map(tree).collect()),
        Bson::String(s) if s.len() > MAX_TREE_STR => {
            let mut cut = MAX_TREE_STR;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            Tree::Str(format!("{}…", &s[..cut]))
        }
        Bson::String(s) => Tree::Str(s.clone()),
        Bson::Int32(n) => Tree::Num(n.to_string()),
        Bson::Double(f) => Tree::Typed("Double", if f.fract() == 0. && f.is_finite() && f.abs() < 1e15 { format!("{f:.0}") } else { f.to_string() }),
        Bson::Int64(n) => Tree::Typed("Long", n.to_string()),
        Bson::Decimal128(d) => Tree::Typed("Decimal128", d.to_string()),
        Bson::Boolean(b) => Tree::Bool(*b),
        Bson::Null | Bson::Undefined => Tree::Null,
        Bson::ObjectId(o) => Tree::Typed("ObjectId", o.to_hex()),
        Bson::DateTime(d) => Tree::Typed("Date", d.try_to_rfc3339_string().unwrap_or_else(|_| d.timestamp_millis().to_string())),
        Bson::RegularExpression(r) => Tree::Typed("Regex", format!("/{}/{}", r.pattern, r.options)),
        Bson::Timestamp(t) => Tree::Typed("Timestamp", format!("{}:{}", t.time, t.increment)),
        Bson::Binary(b) => match b.to_uuid() {
            Ok(u) if b.subtype == mongodb::bson::spec::BinarySubtype::Uuid => Tree::Typed("UUID", u.to_string()),
            _ => Tree::Typed("Binary", b.to_string()),
        },
        other => Tree::Str(other.clone().into_relaxed_extjson().to_string()),
    }
}

/// A value as text: plain for scalars, compact JSON for documents and arrays. None: null
/// (a missing field is shown empty, too).
fn show(v: &Bson) -> Option<String> {
    match v {
        Bson::Null | Bson::Undefined => None,
        Bson::String(s) => cell(s.as_str()),
        Bson::Int32(n) => Some(n.to_string()),
        Bson::Int64(n) => Some(n.to_string()),
        Bson::Double(n) => Some(n.to_string()),
        Bson::Boolean(b) => Some(b.to_string()),
        Bson::ObjectId(o) => Some(o.to_hex()),
        Bson::DateTime(d) => Some(d.try_to_rfc3339_string().unwrap_or_else(|_| d.to_string())),
        Bson::Decimal128(d) => Some(d.to_string()),
        other => cell(other.clone().into_relaxed_extjson().to_string()),
    }
}

// ---------------------------------------------------------------- queries

pub async fn query(c: &Client, scope: &Scope, text: &str) -> Result<Vec<Grid>> {
    let mut db = scope.database.clone().unwrap_or_else(|| "test".into());
    let mut out = Vec::new();
    for stmt in statements(text) {
        let s = stmt.trim();
        if let Some(name) = s.strip_prefix("use ") {
            db = name.trim().trim_matches(['"', '\'']).to_string();
            out.push(Grid::note(format!("Switched to {db}")));
            continue;
        }
        match s {
            "show dbs" | "show databases" => {
                let names = c.list_database_names().await?;
                out.push(Grid::pairs(names.into_iter().map(|n| ("database".to_string(), Some(n)))));
                continue;
            }
            "show collections" | "show tables" => {
                let names = c.database(&db).list_collection_names().await?;
                out.push(Grid { columns: vec!["collection".into()], rows: names.into_iter().map(|n| vec![Some(n)]).collect(), ..Default::default() });
                continue;
            }
            _ => {}
        }
        out.push(run_statement(c, &db, scope.collection.as_deref(), s).await?);
    }
    if out.is_empty() {
        out.push(Grid::note("Nothing to run"));
    }
    Ok(out)
}

/// The statements of a script: split at `;` and at lines starting a new `db.` / `use` /
/// `show`, outside brackets and strings.
fn statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut depth, mut quote): (i32, Option<char>) = (0, None);
    let mut chars = text.chars().peekable();
    let mut line_start = true;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            cur.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if line_start && depth == 0 && !c.is_whitespace() && !cur.trim().is_empty() {
            let rest: String = std::iter::once(c).chain(chars.clone().take(4)).collect();
            if rest.starts_with("db.") || rest.starts_with("use ") || rest.starts_with("show ") {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !c.is_whitespace() {
            line_start = false;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            ';' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            '/' if chars.peek() == Some(&'/') => {
                // A comment, to the end of the line.
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                line_start = true;
                cur.push('\n');
                continue;
            }
            '\n' => line_start = true,
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// One call of a `db.…` chain: its name and arguments.
#[derive(Debug, PartialEq)]
struct Call {
    name: String,
    args: Option<Vec<Bson>>,
}

async fn run_statement(c: &Client, db: &str, coll: Option<&str>, s: &str) -> Result<Grid> {
    // A bare document is a command (or, in a collection's tab, a filter); a bare array a pipeline.
    if !s.starts_with("db.") {
        let v = Parser::new(s).whole()?;
        return match (v, coll) {
            (Bson::Array(stages), Some(coll)) => cursor(c.database(db).collection::<Document>(coll).aggregate(docs(stages)?).await?).await,
            (Bson::Document(d), Some(coll)) if !is_command(&d) => cursor(c.database(db).collection::<Document>(coll).find(d).limit(super::MAX_ROWS as i64).await?).await,
            (Bson::Document(d), _) => command(c, db, d).await,
            _ => bail!("write a db.… call, a command {{…}}, or a pipeline […]"),
        };
    }
    let mut calls = chain(&s[3..])?;
    let mut db = c.database(db);
    // db.method(…), or db.name / db.getCollection(name), then the collection's method.
    let first = calls.remove(0);
    let coll_name = match (first.name.as_str(), first.args) {
        ("getSiblingDB", Some(a)) => {
            db = c.database(&str_arg(&a, 0)?);
            if calls.is_empty() {
                bail!("db.getSiblingDB(…): then what?");
            }
            let next = calls.remove(0);
            if next.name == "getCollection" { str_arg(next.args.as_deref().unwrap_or_default(), 0)? } else { return db_call(c, &db, next).await }
        }
        ("getCollection", Some(a)) => str_arg(&a, 0)?,
        (_, Some(a)) => return db_call(c, &db, Call { name: first.name, args: Some(a) }).await,
        (name, None) => name.to_string(),
    };
    let coll = db.collection::<Document>(&coll_name);
    if calls.is_empty() {
        bail!("db.{coll_name}: what to do with it? (find, aggregate, updateOne…)")
    }
    let method = calls.remove(0);
    let args = method.args.unwrap_or_default();
    let arg = |i: usize| -> Result<Document> {
        match args.get(i) {
            None => Ok(Document::new()),
            Some(Bson::Document(d)) => Ok(d.clone()),
            Some(other) => bail!("argument {} of {} should be a document, not {other}", i + 1, method.name),
        }
    };
    match method.name.as_str() {
        "find" => {
            let mut find = coll.find(arg(0)?).projection(arg(1)?);
            let mut limit = super::MAX_ROWS as i64;
            for m in calls {
                let a = m.args.unwrap_or_default();
                match m.name.as_str() {
                    "limit" => limit = num_arg(&a)?.clamp(1, super::MAX_ROWS as i64),
                    "skip" => find = find.skip(num_arg(&a)?.max(0) as u64),
                    "sort" => find = find.sort(doc_arg(&a)?),
                    "projection" => find = find.projection(doc_arg(&a)?),
                    "count" | "itcount" | "size" => {
                        return Ok(count_grid(coll.count_documents(arg(0)?).await?));
                    }
                    "pretty" | "toArray" => {}
                    other => bail!("find(…).{other}(…) isn't supported here"),
                }
            }
            cursor(find.limit(limit).await?).await
        }
        "findOne" => Ok(grid(&coll.find_one(arg(0)?).projection(arg(1)?).await?.into_iter().collect::<Vec<_>>())),
        "aggregate" => cursor(coll.aggregate(docs(args.into_iter().next().map(array).transpose()?.unwrap_or_default())?).await?).await,
        "countDocuments" | "count" => Ok(count_grid(coll.count_documents(arg(0)?).await?)),
        "estimatedDocumentCount" => Ok(count_grid(coll.estimated_document_count().await?)),
        "distinct" => {
            let field = str_arg(&args, 0)?;
            let values = coll.distinct(&field, arg(1)?).await?;
            Ok(Grid { columns: vec![field], rows: values.iter().map(|v| vec![show(v)]).collect(), ..Default::default() })
        }
        "insertOne" => {
            let r = coll.insert_one(arg(0)?).await?;
            Ok(Grid::note(format!("Inserted 1 document · _id {}", show(&r.inserted_id).unwrap_or_default())))
        }
        "insertMany" => {
            let list = docs(args.into_iter().next().map(array).transpose()?.unwrap_or_default())?;
            let r = coll.insert_many(list).await?;
            Ok(Grid::note(format!("Inserted {} documents", r.inserted_ids.len())))
        }
        "updateOne" | "updateMany" => {
            let (filter, update) = (arg(0)?, args.get(1).cloned().ok_or_else(|| anyhow!("{} needs a filter and an update", method.name))?);
            let update: mongodb::options::UpdateModifications = match update {
                Bson::Document(d) => d.into(),
                Bson::Array(stages) => docs(stages)?.into(),
                other => bail!("the update should be a document or a pipeline, not {other}"),
            };
            let r = if method.name == "updateOne" { coll.update_one(filter, update).await? } else { coll.update_many(filter, update).await? };
            Ok(Grid::note(format!("Matched {} · modified {}{}", r.matched_count, r.modified_count, r.upserted_id.map(|i| format!(" · upserted {}", show(&i).unwrap_or_default())).unwrap_or_default())))
        }
        "replaceOne" => {
            let r = coll.replace_one(arg(0)?, arg(1)?).await?;
            Ok(Grid::note(format!("Matched {} · modified {}", r.matched_count, r.modified_count)))
        }
        "deleteOne" => Ok(Grid::note(format!("Deleted {}", coll.delete_one(arg(0)?).await?.deleted_count))),
        "deleteMany" => Ok(Grid::note(format!("Deleted {}", coll.delete_many(arg(0)?).await?.deleted_count))),
        "drop" => {
            coll.drop().await?;
            Ok(Grid::note(format!("Dropped {coll_name}")))
        }
        "getIndexes" => cursor(coll.list_indexes().await?.map_ok(|m| mongodb::bson::to_document(&m).unwrap_or_default())).await,
        "stats" => command(c, db.name(), doc! { "collStats": &coll_name }).await,
        other => bail!("db.{coll_name}.{other}(…) isn't supported here"),
    }
}

async fn db_call(c: &Client, db: &mongodb::Database, call: Call) -> Result<Grid> {
    let args = call.args.unwrap_or_default();
    match call.name.as_str() {
        "getCollectionNames" => {
            let mut names = db.list_collection_names().await?;
            names.sort();
            Ok(Grid { columns: vec!["collection".into()], rows: names.into_iter().map(|n| vec![Some(n)]).collect(), ..Default::default() })
        }
        "runCommand" => command(c, db.name(), doc_arg(&args)?).await,
        "adminCommand" => command(c, "admin", doc_arg(&args)?).await,
        "stats" => command(c, db.name(), doc! { "dbStats": 1 }).await,
        "version" => Ok(Grid::note(version(c).await?)),
        "serverStatus" => command(c, "admin", doc! { "serverStatus": 1 }).await,
        "dropDatabase" => {
            db.drop().await?;
            Ok(Grid::note(format!("Dropped {}", db.name())))
        }
        "createCollection" => {
            let name = str_arg(&args, 0)?;
            db.create_collection(&name).await?;
            Ok(Grid::note(format!("Created {name}")))
        }
        other => bail!("db.{other}(…) isn't supported here"),
    }
}

/// Whether a document is a command (its first key names one) rather than a filter.
fn is_command(d: &Document) -> bool {
    const COMMANDS: &[&str] = &[
        "find", "aggregate", "count", "distinct", "insert", "update", "delete", "findAndModify", "createIndexes", "dropIndexes", "listIndexes", "create", "drop", "collStats",
        "dbStats", "listCollections", "listDatabases", "explain", "ping", "serverStatus", "buildInfo", "renameCollection", "currentOp", "killOp", "hello", "isMaster",
    ];
    d.keys().next().is_some_and(|k| COMMANDS.contains(&k.as_str()))
}

async fn command(c: &Client, db: &str, cmd: Document) -> Result<Grid> {
    let r = c.database(db).run_command(cmd).await?;
    // A cursor's first batch is the rows; anything else is one row.
    if let Ok(cur) = r.get_document("cursor")
        && let Ok(batch) = cur.get_array("firstBatch")
    {
        let list: Vec<Document> = batch.iter().filter_map(|b| b.as_document().cloned()).collect();
        let mut g = grid(&list);
        g.more = cur.get_i64("id").is_ok_and(|id| id != 0);
        return Ok(g);
    }
    Ok(grid(&[r]))
}

async fn cursor(mut c: impl futures::Stream<Item = mongodb::error::Result<Document>> + Unpin) -> Result<Grid> {
    let mut list = Vec::new();
    while let Some(d) = c.try_next().await? {
        if list.len() == super::MAX_ROWS {
            let mut g = grid(&list);
            g.more = true;
            return Ok(g);
        }
        list.push(d);
    }
    let mut g = grid(&list);
    g.note = format!("{} document{}", list.len(), if list.len() == 1 { "" } else { "s" });
    Ok(g)
}

fn count_grid(n: u64) -> Grid {
    Grid { columns: vec!["count".into()], rows: vec![vec![Some(n.to_string())]], ..Default::default() }
}

fn docs(list: Vec<Bson>) -> Result<Vec<Document>> {
    list.into_iter().map(|b| b.as_document().cloned().ok_or_else(|| anyhow!("expected documents, found {b}"))).collect()
}

fn array(b: Bson) -> Result<Vec<Bson>> {
    match b {
        Bson::Array(a) => Ok(a),
        other => bail!("expected an array […], found {other}"),
    }
}

fn str_arg(a: &[Bson], i: usize) -> Result<String> {
    a.get(i).and_then(|b| b.as_str()).map(str::to_string).ok_or_else(|| anyhow!("expected a name in quotes"))
}

fn num_arg(a: &[Bson]) -> Result<i64> {
    match a.first() {
        Some(Bson::Int32(n)) => Ok(*n as i64),
        Some(Bson::Int64(n)) => Ok(*n),
        Some(Bson::Double(n)) => Ok(*n as i64),
        _ => bail!("expected a number"),
    }
}

fn doc_arg(a: &[Bson]) -> Result<Document> {
    match a.first() {
        Some(Bson::Document(d)) => Ok(d.clone()),
        _ => bail!("expected a document {{…}}"),
    }
}

/// `users.find({…}).limit(5)` → [users, find({…}), limit(5)].
fn chain(s: &str) -> Result<Vec<Call>> {
    let mut p = Parser::new(s);
    let mut calls = Vec::new();
    loop {
        p.space();
        let name = p.ident().ok_or_else(|| anyhow!("expected a name at “{}”", p.rest_preview()))?;
        p.space();
        let args = if p.eat('(') { Some(p.list(')')?) } else { None };
        calls.push(Call { name, args });
        p.space();
        if p.eat('.') {
            continue;
        }
        p.eat(';');
        p.space();
        if !p.done() {
            bail!("unexpected “{}”", p.rest_preview());
        }
        return Ok(calls);
    }
}

/// The mongo shell's relaxed JSON.
struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser { s, i: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn rest_preview(&self) -> String {
        self.rest().chars().take(30).collect()
    }

    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn space(&mut self) {
        loop {
            let r = self.rest();
            let t = r.trim_start();
            self.i += r.len() - t.len();
            if t.starts_with("//") {
                self.i += t.find('\n').unwrap_or(t.len());
            } else {
                return;
            }
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.space();
        if self.peek() == Some(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        let r = self.rest();
        let n = r.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(r.len());
        if n == 0 || r.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        self.i += n;
        Some(r[..n].to_string())
    }

    /// The text is exactly one value.
    fn whole(&mut self) -> Result<Bson> {
        let v = self.value()?;
        self.eat(';');
        self.space();
        if !self.done() {
            bail!("unexpected “{}”", self.rest_preview());
        }
        Ok(v)
    }

    /// Values up to `end`, comma-separated (a trailing comma is fine).
    fn list(&mut self, end: char) -> Result<Vec<Bson>> {
        let mut out = Vec::new();
        loop {
            if self.eat(end) {
                return Ok(out);
            }
            out.push(self.value()?);
            if !self.eat(',') {
                if self.eat(end) {
                    return Ok(out);
                }
                bail!("expected ',' or '{end}' at “{}”", self.rest_preview());
            }
        }
    }

    fn value(&mut self) -> Result<Bson> {
        self.space();
        match self.peek() {
            None => bail!("unexpected end"),
            Some('{') => {
                self.i += 1;
                let mut d = Document::new();
                loop {
                    if self.eat('}') {
                        return Ok(Bson::Document(d));
                    }
                    self.space();
                    let key = match self.peek() {
                        Some('"' | '\'') => self.string()?,
                        _ => {
                            let r = self.rest();
                            let n = r.find(|c: char| !(c.is_alphanumeric() || "_$.-".contains(c))).unwrap_or(r.len());
                            if n == 0 {
                                bail!("expected a field name at “{}”", self.rest_preview());
                            }
                            self.i += n;
                            r[..n].to_string()
                        }
                    };
                    if !self.eat(':') {
                        bail!("expected ':' after {key}");
                    }
                    let v = self.value()?;
                    d.insert(key, v);
                    if !self.eat(',') {
                        if self.eat('}') {
                            return Ok(Bson::Document(d));
                        }
                        bail!("expected ',' or '}}' at “{}”", self.rest_preview());
                    }
                }
            }
            Some('[') => {
                self.i += 1;
                Ok(Bson::Array(self.list(']')?))
            }
            Some('"' | '\'') => Ok(Bson::String(self.string()?)),
            Some('/') => {
                let r = self.rest();
                let mut end = None;
                let mut esc = false;
                for (k, c) in r.char_indices().skip(1) {
                    if esc {
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '/' {
                        end = Some(k);
                        break;
                    }
                }
                let end = end.ok_or_else(|| anyhow!("unclosed /regex/"))?;
                let pattern = r[1..end].to_string();
                let after = &r[end + 1..];
                let flags: String = after.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                self.i += end + 1 + flags.len();
                Ok(Bson::RegularExpression(mongodb::bson::Regex { pattern, options: flags }))
            }
            Some(c) if c == '-' || c == '.' || c.is_ascii_digit() => {
                let r = self.rest();
                let n = r.find(|c: char| !(c.is_ascii_digit() || "+-.eE".contains(c))).unwrap_or(r.len());
                let t = &r[..n];
                self.i += n;
                if let Ok(i) = t.parse::<i64>() {
                    return Ok(i32::try_from(i).map(Bson::Int32).unwrap_or(Bson::Int64(i)));
                }
                t.parse::<f64>().map(Bson::Double).map_err(|_| anyhow!("not a number: {t}"))
            }
            Some(_) => {
                let name = self.ident().ok_or_else(|| anyhow!("unexpected “{}”", self.rest_preview()))?;
                let name = if name == "new" {
                    self.space();
                    self.ident().ok_or_else(|| anyhow!("expected a name after new"))?
                } else {
                    name
                };
                match name.as_str() {
                    "true" => return Ok(Bson::Boolean(true)),
                    "false" => return Ok(Bson::Boolean(false)),
                    "null" => return Ok(Bson::Null),
                    "undefined" => return Ok(Bson::Undefined),
                    _ => {}
                }
                if !self.eat('(') {
                    bail!("unknown name {name}");
                }
                let args = self.list(')')?;
                let text = args.first().and_then(|a| a.as_str()).map(str::to_string);
                match name.as_str() {
                    "ObjectId" => Ok(match text {
                        Some(t) => Bson::ObjectId(ObjectId::parse_str(&t).map_err(|_| anyhow!("not an ObjectId: {t}"))?),
                        None => Bson::ObjectId(ObjectId::new()),
                    }),
                    "ISODate" | "Date" => {
                        let t = match (text, args.first()) {
                            (Some(t), _) => t,
                            (None, Some(n)) => return Ok(Bson::DateTime(mongodb::bson::DateTime::from_millis(num_arg(std::slice::from_ref(n))?))),
                            (None, None) => return Ok(Bson::DateTime(mongodb::bson::DateTime::now())),
                        };
                        let full = if t.len() == 10 { format!("{t}T00:00:00Z") } else if t.ends_with('Z') || t[10..].contains(['+', '-']) { t.clone() } else { format!("{t}Z") };
                        Ok(Bson::DateTime(mongodb::bson::DateTime::parse_rfc3339_str(&full).map_err(|_| anyhow!("not a date: {t}"))?))
                    }
                    "NumberInt" => Ok(Bson::Int32(text.and_then(|t| t.parse().ok()).map(Ok).unwrap_or_else(|| num_arg(&args).map(|n| n as i32))?)),
                    "NumberLong" => Ok(Bson::Int64(text.and_then(|t| t.parse().ok()).map(Ok).unwrap_or_else(|| num_arg(&args))?)),
                    "NumberDecimal" => Ok(Bson::Decimal128(text.unwrap_or_default().parse().map_err(|_| anyhow!("not a decimal"))?)),
                    "UUID" => Ok(Bson::Binary(mongodb::bson::Binary::from_uuid(mongodb::bson::Uuid::parse_str(text.unwrap_or_default()).map_err(|_| anyhow!("not a UUID"))?))),
                    other => bail!("{other}(…) isn't supported here"),
                }
            }
        }
    }

    fn string(&mut self) -> Result<String> {
        let q = self.peek().unwrap_or('"');
        self.i += 1;
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((k, c)) = chars.next() {
            match c {
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).map(|(_, c)| c).collect();
                        out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
                    }
                    Some(c) => out.push(c),
                    None => break,
                },
                c if c == q => {
                    self.i += k + 1;
                    return Ok(out);
                }
                c => out.push(c),
            }
        }
        bail!("unclosed string")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_syntax_parses() {
        let v = Parser::new("{name: 'Ann', age: {$gt: 30}, _id: ObjectId(\"64b7f0c2a1b2c3d4e5f60718\"), at: ISODate(\"2024-01-02\"), tags: ['a', \"b\",], re: /^an/i, n: 2.5, big: 5000000000}").whole().unwrap();
        let d = v.as_document().unwrap();
        assert_eq!(d.get_str("name").unwrap(), "Ann");
        assert_eq!(d.get_document("age").unwrap().get_i32("$gt").unwrap(), 30);
        assert_eq!(d.get_object_id("_id").unwrap().to_hex(), "64b7f0c2a1b2c3d4e5f60718");
        assert_eq!(d.get_datetime("at").unwrap().try_to_rfc3339_string().unwrap(), "2024-01-02T00:00:00Z");
        assert_eq!(d.get_array("tags").unwrap().len(), 2);
        assert!(matches!(d.get("re"), Some(Bson::RegularExpression(r)) if r.pattern == "^an" && r.options == "i"));
        assert_eq!(d.get_f64("n").unwrap(), 2.5);
        assert_eq!(d.get_i64("big").unwrap(), 5_000_000_000);
    }

    #[test]
    fn calls_and_statements_split() {
        let c = chain("users.find({a: 1}, {b: 0}).sort({a: -1}).limit(5)").unwrap();
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["users", "find", "sort", "limit"]);
        assert_eq!(c[1].args.as_ref().unwrap().len(), 2);
        assert!(c[0].args.is_none());
        let s = statements("db.a.find({x: ';'})\ndb.b.insertOne({\n  y: 1\n});\n// a comment\nshow collections");
        assert_eq!(s.iter().map(|s| s.trim()).collect::<Vec<_>>(), ["db.a.find({x: ';'})", "db.b.insertOne({\n  y: 1\n})", "show collections"]);
        assert!(is_command(&doc! { "ping": 1 }) && !is_command(&doc! { "name": "x" }));
    }

    #[test]
    fn documents_print_like_mongosh_and_as_json() {
        use super::super::TextStyle;
        let id = ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap();
        let d = doc! { "_id": id, "name": "Ann's", "n": 3, "big": 5_000_000_000i64, "x": 2.5, "w": 3.0, "at": mongodb::bson::DateTime::from_millis(0), "tags": ["a", "b"], "addr": { "city": "Oslo" }, "bad key": Bson::Null };
        let t = tree(&Bson::Document(d.clone()));
        assert_eq!(
            t.text(TextStyle::Shell),
            "{\n  _id: ObjectId('64b7f0c2a1b2c3d4e5f60718'),\n  name: 'Ann\\'s',\n  n: 3,\n  big: Long('5000000000'),\n  x: 2.5,\n  w: 3,\n  at: ISODate('1970-01-01T00:00:00Z'),\n  tags: [\n    'a',\n    'b'\n  ],\n  addr: {\n    city: 'Oslo'\n  },\n  'bad key': null\n}"
        );
        assert!(t.text(TextStyle::Json).starts_with("{\n  \"_id\": {\"$oid\": \"64b7f0c2a1b2c3d4e5f60718\"},\n  \"name\": \"Ann's\","), "{}", t.text(TextStyle::Json));
        let Tree::Object(f) = &t else { panic!() };
        let types: Vec<_> = f.iter().map(|(_, v)| v.type_name()).collect();
        assert_eq!(types, ["ObjectId", "String", "Int32", "Int64", "Double", "Double", "Date", "Array", "Object", "Null"]);
        assert_eq!(f[7].1.short(TextStyle::Shell), "[] 2 elements");
        assert_eq!(f[8].1.short(TextStyle::Shell), "{} 1 field");
        let g = grid(&[d]);
        assert_eq!(g.trees.as_ref().map(|d| d.len()), Some(1), "each row keeps its document");
    }

    #[test]
    fn documents_become_rows() {
        let g = grid(&[doc! { "name": "a", "_id": 1, "tags": ["x"] }, doc! { "_id": 2, "extra": Bson::Null }]);
        assert_eq!(g.columns, ["_id", "name", "tags", "extra"]);
        assert_eq!(g.rows[0], vec![Some("1".into()), Some("a".into()), Some("[\"x\"]".into()), None]);
        assert_eq!(g.rows[1][1], None, "missing fields are empty");
        assert_eq!(shell_name("users"), "users");
        assert_eq!(shell_name("my-coll"), "getCollection(\"my-coll\")");
    }
}

