//! What MongoDB Compass does with a collection: find with its options (filter, project, sort,
//! collation, hint, max time, skip, limit), explain, indexes, schema from a sample, and
//! validation rules. Everything is written in the shell's relaxed syntax.

use super::mongo::{grid, parse_value, tree};
use super::{FindQuery, Page, TextStyle, Tree};
use anyhow::{Result, anyhow, bail};
use futures::TryStreamExt;
use mongodb::Client;
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{Collation, CountOptions, FindOptions, Hint};
use std::time::Duration;

const COUNT_CAP: u64 = 100_000;

/// A find, read from the query bar.
#[derive(Debug, Default)]
struct Find {
    filter: Document,
    projection: Option<Document>,
    sort: Option<Document>,
    collation: Option<Collation>,
    hint: Option<Hint>,
    max_time: Option<Duration>,
    skip: u64,
    limit: Option<u64>,
}

/// A document field of the bar (empty: none).
fn doc_field(name: &str, text: &str) -> Result<Option<Document>> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match parse_value(t).map_err(|e| anyhow!("{name}: {e}"))? {
        Bson::Document(d) => Ok(Some(d)),
        other => bail!("{name}: expected a document {{…}}, not {other}"),
    }
}

fn num_field(name: &str, text: &str) -> Result<Option<u64>> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<u64>().map(Some).map_err(|_| anyhow!("{name}: expected a whole number, not “{t}”"))
}

fn parse(q: &FindQuery) -> Result<Find> {
    let collation = match doc_field("Collation", &q.collation)? {
        Some(d) => Some(mongodb::bson::from_document::<Collation>(d).map_err(|e| anyhow!("Collation: {e}"))?),
        None => None,
    };
    let hint = match q.hint.trim() {
        "" => None,
        t => match parse_value(t).map_err(|e| anyhow!("Index Hint: {e}"))? {
            Bson::Document(d) => Some(Hint::Keys(d)),
            Bson::String(s) => Some(Hint::Name(s)),
            other => bail!("Index Hint: a document {{field: 1}} or an index name in quotes, not {other}"),
        },
    };
    Ok(Find {
        filter: doc_field("Filter", &q.filter)?.unwrap_or_default(),
        projection: doc_field("Project", &q.project)?,
        sort: doc_field("Sort", &q.sort)?,
        collation,
        hint,
        max_time: num_field("Max Time MS", &q.max_time_ms)?.filter(|n| *n > 0).map(Duration::from_millis),
        skip: num_field("Skip", &q.skip)?.unwrap_or(0),
        limit: num_field("Limit", &q.limit)?.filter(|n| *n > 0),
    })
}

/// Checks the bar without running it (the Find button stays usable meanwhile).
pub fn check(q: &FindQuery) -> Result<(), String> {
    parse(q).map(|_| ()).map_err(|e| e.to_string())
}

/// One page of a find: `offset` and `page` are within the find's own skip and limit. `column_sort` is
/// a table header clicked (when the bar has no sort).
pub async fn find_page(c: &Client, db: &str, coll: &str, q: &FindQuery, offset: usize, page: usize, column_sort: Option<&(String, bool)>) -> Result<Page> {
    let f = parse(q)?;
    let coll = c.database(db).collection::<Document>(coll);
    let offset = offset as u64;
    let take = match f.limit {
        Some(l) if offset >= l => 0,
        Some(l) => (l - offset).min(page as u64),
        None => page as u64,
    };
    let sort = f.sort.clone().or_else(|| column_sort.map(|(col, desc)| doc! { col: if *desc { -1 } else { 1 } }));
    let docs: Vec<Document> = if take == 0 {
        Vec::new()
    } else {
        let opts = FindOptions::builder()
            .projection(f.projection.clone())
            .sort(sort)
            .collation(f.collation.clone())
            .hint(f.hint.clone())
            .max_time(f.max_time)
            .skip(Some(f.skip + offset))
            .limit(Some(take as i64))
            .build();
        coll.find(f.filter.clone()).with_options(opts).await?.try_collect().await?
    };
    let plain = f.filter.is_empty() && f.collation.is_none() && f.hint.is_none();
    let (count, estimate) = if plain {
        (coll.estimated_document_count().await.unwrap_or(0), false)
    } else {
        let opts = CountOptions::builder().collation(f.collation.clone()).hint(f.hint.clone()).max_time(f.max_time).limit(Some(f.skip + COUNT_CAP + 1)).build();
        let n = coll.count_documents(f.filter.clone()).with_options(opts).await?;
        (n.min(f.skip + COUNT_CAP), n > f.skip + COUNT_CAP)
    };
    let mut total = count.saturating_sub(f.skip);
    if let Some(l) = f.limit {
        total = total.min(l);
    }
    Ok(Page { grid: grid(&docs), total: Some(total), estimate })
}

// ---------------------------------------------------------------- explain

#[derive(Clone, Debug, PartialEq)]
pub struct Explain {
    pub returned: i64,
    pub keys_examined: i64,
    pub docs_examined: i64,
    pub ms: i64,
    /// The winning plan, outermost stage first ("FETCH", "IXSCAN { age: 1 }"…).
    pub stages: Vec<String>,
    /// Indexes the plan uses.
    pub indexes: Vec<String>,
    /// Sorted in memory (no index gave the order).
    pub in_memory_sort: bool,
    /// The server's whole answer.
    pub raw: Tree,
}

fn int(d: &Document, k: &str) -> i64 {
    match d.get(k) {
        Some(Bson::Int32(n)) => *n as i64,
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Double(n)) => *n as i64,
        _ => 0,
    }
}

/// `{ age: 1, name: -1 }`, on one line.
pub fn keys_text(d: &Document) -> String {
    let parts: Vec<String> = d.iter().map(|(k, v)| format!("{}: {}", Tree::key(k, TextStyle::Shell), tree(v).short(TextStyle::Shell))).collect();
    if parts.is_empty() { "{}".into() } else { format!("{{ {} }}", parts.join(", ")) }
}

pub async fn explain(c: &Client, db: &str, coll: &str, q: &FindQuery) -> Result<Explain> {
    let f = parse(q)?;
    let mut find = doc! { "find": coll, "filter": f.filter };
    if let Some(p) = f.projection {
        find.insert("projection", p);
    }
    if let Some(s) = f.sort {
        find.insert("sort", s);
    }
    if let Some(c) = f.collation {
        find.insert("collation", mongodb::bson::to_document(&c)?);
    }
    match f.hint {
        Some(Hint::Keys(k)) => {
            find.insert("hint", k);
        }
        Some(Hint::Name(n)) => {
            find.insert("hint", n);
        }
        _ => {}
    }
    if f.skip > 0 {
        find.insert("skip", f.skip as i64);
    }
    if let Some(l) = f.limit {
        find.insert("limit", l as i64);
    }
    if let Some(t) = f.max_time {
        find.insert("maxTimeMS", t.as_millis() as i64);
    }
    let r = c.database(db).run_command(doc! { "explain": find, "verbosity": "executionStats" }).await?;
    let stats = r.get_document("executionStats").cloned().unwrap_or_default();
    let planner = r.get_document("queryPlanner").cloned().unwrap_or_default();
    let winning = planner.get_document("winningPlan").cloned().unwrap_or_default();
    // Newer servers (the slot-based engine) nest the plan in queryPlan.
    let mut node = winning.get_document("queryPlan").cloned().unwrap_or(winning);
    let (mut stages, mut indexes, mut in_memory_sort) = (Vec::new(), Vec::new(), false);
    loop {
        let stage = node.get_str("stage").unwrap_or("?").to_string();
        in_memory_sort |= stage == "SORT";
        match (node.get_str("indexName"), node.get_document("keyPattern")) {
            (Ok(name), Ok(keys)) => {
                indexes.push(name.to_string());
                stages.push(format!("{stage} {}", keys_text(keys)));
            }
            _ => stages.push(stage),
        }
        let next = node.get_document("inputStage").cloned().ok().or_else(|| node.get_array("inputStages").ok().and_then(|a| a.first()).and_then(|b| b.as_document().cloned()));
        match next {
            Some(n) => node = n,
            None => break,
        }
    }
    Ok(Explain {
        returned: int(&stats, "nReturned"),
        keys_examined: int(&stats, "totalKeysExamined"),
        docs_examined: int(&stats, "totalDocsExamined"),
        ms: int(&stats, "executionTimeMillis"),
        stages,
        indexes,
        in_memory_sort,
        raw: tree(&Bson::Document(r)),
    })
}

// ---------------------------------------------------------------- indexes

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Index {
    pub name: String,
    /// Its keys, `{ email: 1 }`.
    pub keys: String,
    /// Regular, compound, text, geospatial, hashed, wildcard.
    pub kind: String,
    pub size: Option<u64>,
    /// How often it was used, and since when.
    pub ops: Option<u64>,
    pub since: Option<String>,
    /// unique, sparse, partial, TTL…, hidden.
    pub props: Vec<String>,
}

fn index_kind(keys: &Document) -> &'static str {
    let values: Vec<String> = keys.values().map(|v| v.as_str().map(str::to_string).unwrap_or_default()).collect();
    if keys.keys().any(|k| k.contains("$**")) {
        "Wildcard"
    } else if values.iter().any(|v| v == "text") {
        "Text"
    } else if values.iter().any(|v| v == "2dsphere" || v == "2d") {
        "Geospatial"
    } else if values.iter().any(|v| v == "hashed") {
        "Hashed"
    } else if keys.len() > 1 {
        "Compound"
    } else {
        "Regular"
    }
}

pub async fn indexes(c: &Client, db: &str, coll: &str) -> Result<Vec<Index>> {
    let database = c.database(db);
    let r = database.run_command(doc! { "listIndexes": coll }).await?;
    let list: Vec<Document> = r.get_document("cursor").and_then(|c| c.get_array("firstBatch")).map(|b| b.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default();
    let collection = database.collection::<Document>(coll);
    // Sizes and use: not every user may read them, and the list is useful without.
    let sizes: Document = async {
        let mut cur = collection.aggregate([doc! { "$collStats": { "storageStats": {} } }]).await?;
        Ok::<_, anyhow::Error>(cur.try_next().await?.and_then(|d| d.get_document("storageStats").ok().and_then(|s| s.get_document("indexSizes").ok()).cloned()).unwrap_or_default())
    }
    .await
    .unwrap_or_default();
    let usage: Vec<Document> = async {
        let cur = collection.aggregate([doc! { "$indexStats": {} }]).await?;
        Ok::<_, anyhow::Error>(cur.try_collect().await?)
    }
    .await
    .unwrap_or_default();
    Ok(list
        .into_iter()
        .map(|ix| {
            let name = ix.get_str("name").unwrap_or("?").to_string();
            let keys = ix.get_document("key").cloned().unwrap_or_default();
            let mut props = Vec::new();
            if ix.get_bool("unique").unwrap_or(false) {
                props.push("unique".to_string());
            }
            if ix.get_bool("sparse").unwrap_or(false) {
                props.push("sparse".into());
            }
            if ix.contains_key("partialFilterExpression") {
                props.push("partial".into());
            }
            if ix.contains_key("expireAfterSeconds") {
                props.push(format!("TTL {}s", int(&ix, "expireAfterSeconds")));
            }
            if ix.contains_key("collation") {
                props.push("collation".into());
            }
            if ix.get_bool("hidden").unwrap_or(false) {
                props.push("hidden".into());
            }
            let used = usage.iter().find(|u| u.get_str("name") == Ok(name.as_str())).and_then(|u| u.get_document("accesses").ok());
            Index {
                size: sizes.get(&name).map(|_| int(&sizes, &name) as u64),
                ops: used.map(|a| int(a, "ops") as u64),
                since: used.and_then(|a| a.get_datetime("since").ok()).and_then(|d| d.try_to_rfc3339_string().ok()).map(|s| s.chars().take(10).collect()),
                kind: index_kind(&keys).into(),
                keys: keys_text(&keys),
                name,
                props,
            }
        })
        .collect())
}

/// Creates an index from its keys `{ email: 1 }` and options `{ unique: true, … }`; its name.
pub async fn create_index(c: &Client, db: &str, coll: &str, keys: &str, options: &str) -> Result<String> {
    let keys = doc_field("Fields", keys)?.filter(|d| !d.is_empty()).ok_or_else(|| anyhow!("Fields: which fields, e.g. {{ email: 1 }}"))?;
    let mut index = doc_field("Options", options)?.unwrap_or_default();
    if !index.contains_key("name") {
        let name = keys.iter().map(|(k, v)| format!("{k}_{}", tree(v).short(TextStyle::Shell).trim_matches('\''))).collect::<Vec<_>>().join("_");
        index.insert("name", name);
    }
    let name = index.get_str("name").unwrap_or_default().to_string();
    index.insert("key", keys);
    c.database(db).run_command(doc! { "createIndexes": coll, "indexes": [index] }).await?;
    Ok(name)
}

pub async fn drop_index(c: &Client, db: &str, coll: &str, name: &str) -> Result<()> {
    if name == "_id_" {
        bail!("The _id index can't be dropped");
    }
    c.database(db).run_command(doc! { "dropIndexes": coll, "index": name }).await?;
    Ok(())
}

// ---------------------------------------------------------------- schema

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Field {
    /// `addr.city`.
    pub path: String,
    pub name: String,
    pub depth: usize,
    /// Documents that have it.
    pub count: u64,
    /// Its types with how often each, most common first.
    pub types: Vec<(String, u64)>,
    /// A few of its values (scalars).
    pub samples: Vec<String>,
    /// Numbers and dates: the smallest and largest seen.
    pub range: Option<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    pub sampled: u64,
    pub fields: Vec<Field>,
}

#[derive(Default)]
struct Acc {
    count: u64,
    types: Vec<(String, u64)>,
    samples: Vec<String>,
    min: Option<(f64, String)>,
    max: Option<(f64, String)>,
}

fn order_key(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(n) => Some(*n),
        Bson::DateTime(d) => Some(d.timestamp_millis() as f64),
        _ => None,
    }
}

fn walk(d: &Document, prefix: &str, seen: &mut std::collections::BTreeMap<String, Acc>) {
    for (k, v) in d {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        let acc = seen.entry(path.clone()).or_default();
        acc.count += 1;
        let t = tree(v);
        let ty = t.type_name().to_string();
        match acc.types.iter_mut().find(|(n, _)| *n == ty) {
            Some((_, n)) => *n += 1,
            None => acc.types.push((ty, 1)),
        }
        if !matches!(v, Bson::Document(_) | Bson::Array(_)) && acc.samples.len() < 5 {
            let s = t.short(TextStyle::Shell);
            let s = if s.chars().count() > 60 { s.chars().take(59).collect::<String>() + "…" } else { s };
            if !acc.samples.contains(&s) {
                acc.samples.push(s);
            }
        }
        if let Some(n) = order_key(v) {
            let shown = t.short(TextStyle::Shell);
            if acc.min.as_ref().is_none_or(|m| n < m.0) {
                acc.min = Some((n, shown.clone()));
            }
            if acc.max.as_ref().is_none_or(|m| n > m.0) {
                acc.max = Some((n, shown));
            }
        }
        match v {
            Bson::Document(sub) => walk(sub, &path, seen),
            // Documents in arrays: their fields, as `tags.name`.
            Bson::Array(items) => {
                for item in items.iter().take(50) {
                    if let Bson::Document(sub) = item {
                        walk(sub, &path, seen);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The fields of a sample of the documents a filter finds (like Compass, 1,000 at most).
pub async fn schema(c: &Client, db: &str, coll: &str, filter: &str, size: u32) -> Result<Schema> {
    let filter = doc_field("Filter", filter)?.unwrap_or_default();
    let cur = c.database(db).collection::<Document>(coll).aggregate([doc! { "$match": filter }, doc! { "$sample": { "size": size as i64 } }]).await?;
    let docs: Vec<Document> = cur.try_collect().await?;
    Ok(analyze(&docs))
}

pub(super) fn analyze(docs: &[Document]) -> Schema {
    let mut seen = std::collections::BTreeMap::new();
    for d in docs {
        walk(d, "", &mut seen);
    }
    let mut fields: Vec<Field> = seen
        .into_iter()
        .map(|(path, mut a)| {
            a.types.sort_by(|x, y| y.1.cmp(&x.1));
            let depth = path.matches('.').count();
            Field {
                name: path.rsplit('.').next().unwrap_or(&path).to_string(),
                depth,
                count: a.count,
                types: a.types,
                samples: a.samples,
                range: a.min.zip(a.max).map(|(lo, hi)| (lo.1, hi.1)),
                path,
            }
        })
        .collect();
    // _id first, then by name, each field's own fields right under it.
    fields.sort_by(|a, b| {
        let key = |f: &Field| f.path.split('.').map(|p| (p != "_id", p.to_string())).collect::<Vec<_>>();
        key(a).cmp(&key(b))
    });
    Schema { sampled: docs.len() as u64, fields }
}

// ---------------------------------------------------------------- a database's collections

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollStat {
    pub name: String,
    /// view, time-series, capped, clustered, validation…
    pub props: Vec<String>,
    pub storage: Option<u64>,
    pub data: Option<u64>,
    pub docs: Option<u64>,
    pub avg: Option<u64>,
    pub indexes: Option<u64>,
    pub index_size: Option<u64>,
}

/// The collections of a database with their sizes, like Compass's database page.
pub async fn collections(c: &Client, db: &str) -> Result<Vec<CollStat>> {
    let database = c.database(db);
    let specs: Vec<mongodb::results::CollectionSpecification> = database.list_collections().await?.try_collect().await?;
    let stats = specs.iter().map(|s| {
        let database = database.clone();
        let (name, view) = (s.name.clone(), matches!(s.collection_type, mongodb::results::CollectionType::View));
        async move {
            if view {
                return Document::new();
            }
            async {
                let mut cur = database.collection::<Document>(&name).aggregate([doc! { "$collStats": { "storageStats": {} } }]).await?;
                Ok::<_, anyhow::Error>(cur.try_next().await?.and_then(|d| d.get_document("storageStats").ok().cloned()).unwrap_or_default())
            }
            .await
            .unwrap_or_default()
        }
    });
    let stats = futures::future::join_all(stats).await;
    let mut out: Vec<CollStat> = specs
        .into_iter()
        .zip(stats)
        .map(|(s, st)| {
            let mut props = Vec::new();
            match s.collection_type {
                mongodb::results::CollectionType::View => props.push("view".to_string()),
                mongodb::results::CollectionType::Timeseries => props.push("time-series".into()),
                _ => {}
            }
            if s.options.capped.unwrap_or(false) {
                props.push("capped".into());
            }
            if s.options.clustered_index.is_some() {
                props.push("clustered".into());
            }
            if s.options.validator.as_ref().is_some_and(|v| !v.is_empty()) {
                props.push("validation".into());
            }
            let n = |k: &str| st.get(k).map(|_| int(&st, k).max(0) as u64);
            CollStat { name: s.name, props, storage: n("storageSize"), data: n("size"), docs: n("count"), avg: n("avgObjSize").or(st.get("count").map(|_| 0)), indexes: n("nindexes"), index_size: n("totalIndexSize") }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub async fn create_collection(c: &Client, db: &str, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.starts_with("system.") || name.contains('$') || name.contains('\0') {
        bail!("Not a collection name: “{name}”");
    }
    c.database(db).create_collection(name).await?;
    Ok(())
}

pub async fn drop_collection(c: &Client, db: &str, name: &str) -> Result<()> {
    c.database(db).collection::<Document>(name).drop().await?;
    Ok(())
}

// ---------------------------------------------------------------- validation

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Validation {
    /// The rules, in the shell's syntax (`{}`: none).
    pub validator: String,
    /// off, moderate, strict.
    pub level: String,
    /// error, warn.
    pub action: String,
}

pub async fn validation(c: &Client, db: &str, coll: &str) -> Result<Validation> {
    let mut cur = c.database(db).list_collections().filter(doc! { "name": coll }).await?;
    let spec = cur.try_next().await?.ok_or_else(|| anyhow!("No collection {coll}"))?;
    let opts = spec.options;
    Ok(Validation {
        validator: opts.validator.map(|v| tree(&Bson::Document(v)).text(TextStyle::Shell)).unwrap_or_else(|| "{}".into()),
        level: opts.validation_level.map(|l| format!("{l:?}").to_lowercase()).unwrap_or_else(|| "strict".into()),
        action: opts.validation_action.map(|a| format!("{a:?}").to_lowercase()).unwrap_or_else(|| "error".into()),
    })
}

pub async fn set_validation(c: &Client, db: &str, coll: &str, v: &Validation) -> Result<()> {
    let rules = doc_field("Validator", &v.validator)?.unwrap_or_default();
    if !["off", "moderate", "strict"].contains(&v.level.as_str()) || !["error", "warn"].contains(&v.action.as_str()) {
        bail!("Level is off, moderate or strict; action is error or warn");
    }
    c.database(db).run_command(doc! { "collMod": coll, "validator": rules, "validationLevel": &v.level, "validationAction": &v.action }).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_reads_like_compass() {
        let q = FindQuery { filter: "{age: {$gt: 30}}".into(), project: "{name: 1}".into(), sort: "{age: -1}".into(), hint: "'age_1'".into(), skip: "5".into(), limit: "10".into(), ..Default::default() };
        let f = parse(&q).unwrap();
        assert_eq!(f.filter, doc! { "age": { "$gt": 30 } });
        assert_eq!((f.skip, f.limit), (5, Some(10)));
        assert!(matches!(f.hint, Some(Hint::Name(n)) if n == "age_1"));
        assert!(parse(&FindQuery { collation: "{locale: 'fr', strength: 2}".into(), ..Default::default() }).unwrap().collation.is_some());
        assert_eq!(check(&FindQuery { filter: "{age: }".into(), ..Default::default() }).unwrap_err().split(':').next(), Some("Filter"));
        assert_eq!(check(&FindQuery { limit: "ten".into(), ..Default::default() }).unwrap_err(), "Limit: expected a whole number, not “ten”");
        assert!(parse(&FindQuery::default()).unwrap().filter.is_empty(), "an empty bar finds everything");
    }

    #[test]
    fn a_schema_counts_fields_types_and_ranges() {
        let s = analyze(&[
            doc! { "_id": 1, "name": "Ann", "age": 34, "addr": { "city": "Oslo" }, "tags": [{ "t": "a" }] },
            doc! { "_id": 2, "name": "Bob", "age": "old" },
            doc! { "_id": 3, "age": 20 },
        ]);
        assert_eq!(s.sampled, 3);
        let paths: Vec<&str> = s.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["_id", "addr", "addr.city", "age", "name", "tags", "tags.t"]);
        let age = &s.fields[3];
        assert_eq!((age.count, age.types.clone()), (3, vec![("Int32".to_string(), 2), ("String".to_string(), 1)]));
        assert_eq!(age.range, Some(("20".into(), "34".into())));
        assert_eq!(s.fields[2].depth, 1);
        assert_eq!(keys_text(&doc! { "age": 1, "name": -1 }), "{ age: 1, name: -1 }");
        assert_eq!(index_kind(&doc! { "a": 1, "b": 1 }), "Compound");
        assert_eq!(index_kind(&doc! { "body": "text" }), "Text");
    }
}

/// Against the test MongoDB (KITE_DB_LIVE=1, port 57017).
#[cfg(test)]
mod live {
    use super::super::{Conn, Kind, Scope, engine};
    use super::*;

    fn wait<T: Send + 'static>(f: impl std::future::Future<Output = Result<T>> + Send + 'static) -> Result<T, String> {
        futures::executor::block_on(engine::run(f))
    }

    #[test]
    fn compass_tabs_work_on_a_real_server() {
        if std::env::var_os("KITE_DB_LIVE").is_none() {
            return;
        }
        let mut c = Conn::new(Kind::MongoDb);
        c.port = 57017;
        c.user = "root".into();
        crate::db::save(&c, Some("mongopw")).unwrap();
        engine::open(&c.id);
        let docs: Vec<String> = (0..30).map(|i| format!("{{n: {i}, name: 'p{i}', group: {}}}", i % 3)).collect();
        wait(engine::query(c.clone(), Scope { database: Some("kite_test".into()), collection: None }, format!("db.compass.drop()\ndb.compass.insertMany([{}])", docs.join(", ")))).unwrap();
        let on = |c: &Conn| (c.clone(), "kite_test".to_string(), "compass".to_string());
        let (cc, db, coll) = on(&c);
        wait(engine::mongo(cc, move |cl| async move { create_index(&cl, &db, &coll, "{n: 1}", "").await })).unwrap();

        // Find with skip and limit: pages within them, and a total that counts them.
        let (cc, db, coll) = on(&c);
        let q = FindQuery { filter: "{group: 1}".into(), sort: "{n: -1}".into(), skip: "2".into(), limit: "5".into(), project: "{_id: 0}".into(), ..Default::default() };
        let qq = q.clone();
        let p = wait(engine::mongo(cc, move |cl| async move { find_page(&cl, &db, &coll, &qq, 0, 3, None).await })).unwrap();
        assert_eq!(p.total, Some(5));
        assert_eq!(p.grid.rows.iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>(), ["22", "19", "16"], "{:?}", p.grid);
        let (cc, db, coll) = on(&c);
        let qq = q.clone();
        let p = wait(engine::mongo(cc, move |cl| async move { find_page(&cl, &db, &coll, &qq, 3, 3, None).await })).unwrap();
        assert_eq!(p.grid.rows.len(), 2, "the limit ends the last page");

        // Explain: the index it used.
        let (cc, db, coll) = on(&c);
        let e = wait(engine::mongo(cc, move |cl| async move { explain(&cl, &db, &coll, &FindQuery { filter: "{n: {$gt: 25}}".into(), ..Default::default() }).await })).unwrap();
        assert_eq!((e.returned, e.indexes.clone()), (4, vec!["n_1".to_string()]), "{e:?}");
        assert!(e.stages.iter().any(|s| s.starts_with("IXSCAN { n: 1 }")), "{:?}", e.stages);

        // Indexes: listed, created, dropped.
        let (cc, db, coll) = on(&c);
        let name = wait(engine::mongo(cc, move |cl| async move { create_index(&cl, &db, &coll, "{name: 1, group: -1}", "{unique: true}").await })).unwrap();
        assert_eq!(name, "name_1_group_-1");
        let (cc, db, coll) = on(&c);
        let list = wait(engine::mongo(cc, move |cl| async move { indexes(&cl, &db, &coll).await })).unwrap();
        let ix = list.iter().find(|i| i.name == "name_1_group_-1").expect("the new index");
        assert_eq!((ix.kind.as_str(), ix.keys.as_str(), ix.props.clone()), ("Compound", "{ name: 1, group: -1 }", vec!["unique".to_string()]));
        assert!(list.iter().any(|i| i.name == "_id_" && i.size.is_some()), "{list:?}");
        let (cc, db, coll) = on(&c);
        wait(engine::mongo(cc, move |cl| async move { drop_index(&cl, &db, &coll, "name_1_group_-1").await })).unwrap();

        // Schema of a sample.
        let (cc, db, coll) = on(&c);
        let s = wait(engine::mongo(cc, move |cl| async move { schema(&cl, &db, &coll, "{}", 1000).await })).unwrap();
        assert_eq!(s.sampled, 30);
        assert_eq!(s.fields.iter().find(|f| f.path == "n").unwrap().range, Some(("0".into(), "29".into())));

        // Validation: set, read back, cleared.
        let (cc, db, coll) = on(&c);
        let v = Validation { validator: "{n: {$type: 'int'}}".into(), level: "moderate".into(), action: "warn".into() };
        wait(engine::mongo(cc, move |cl| async move { set_validation(&cl, &db, &coll, &v).await })).unwrap();
        let (cc, db, coll) = on(&c);
        let v = wait(engine::mongo(cc, move |cl| async move { validation(&cl, &db, &coll).await })).unwrap();
        assert_eq!((v.level.as_str(), v.action.as_str()), ("moderate", "warn"));
        assert!(v.validator.contains("$type: 'int'"), "{}", v.validator);
        crate::db::delete(&c.id).ok();
    }
}
