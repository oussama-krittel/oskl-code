//! Help while writing queries: suggestions for MongoDB (collections after `db.`, methods,
//! `$` operators, field names, shell helpers) and SQL (keywords, functions, tables, columns).

use crate::complete::{Item, Kind};
use std::cell::RefCell;
use std::rc::Rc;

/// Names the suggestions draw on, filled in as they're learned (collections, tables, fields…).
#[derive(Default)]
pub struct Names {
    /// MongoDB collections, or SQL tables and views.
    pub tables: Vec<String>,
    /// Field (or column) names seen.
    pub fields: Vec<String>,
}

pub type Shared = Rc<RefCell<Names>>;

pub type Suggest = Rc<dyn Fn(&str) -> Option<(String, Vec<Item>)>>;

fn item(label: &str, kind: Kind, suffix: &'static str, back: usize) -> Item {
    Item { label: label.to_string(), kind, suffix, back }
}

/// The word being typed at the end of `before` (letters, digits, `_`, `$`).
fn word(before: &str) -> &str {
    let start = before.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).map(|i| i + before[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0);
    &before[start..]
}

/// Whether the end of `before` is inside a string or after a line comment (no suggestions there).
/// `sql`: -- comments and 'strings'; else // comments and '…', "…", `…` strings.
fn in_text(before: &str, sql: bool) -> bool {
    let mut quote: Option<char> = None;
    let mut chars = before.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(_) if c == '\\' && !sql => {
                chars.next();
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || (!sql && (c == '"' || c == '`')) => quote = Some(c),
            None if sql && c == '-' && chars.peek() == Some(&'-') => return true,
            None if !sql && c == '/' && chars.peek() == Some(&'/') => return true,
            None => {}
        }
    }
    quote.is_some()
}

/// Those starting with `prefix` (ignoring case) first, then those containing it.
fn pick(prefix: &str, all: Vec<Item>) -> Vec<Item> {
    let p = prefix.to_lowercase();
    let (mut starts, mut has): (Vec<Item>, Vec<Item>) = (Vec::new(), Vec::new());
    for it in all {
        let l = it.label.to_lowercase();
        if l == p {
            continue;
        }
        if l.starts_with(&p) {
            starts.push(it);
        } else if p.len() > 1 && l.contains(&p) {
            has.push(it);
        }
    }
    starts.extend(has);
    let mut seen = std::collections::HashSet::new();
    starts.retain(|i| seen.insert(i.label.clone()));
    starts.truncate(50);
    starts
}

const COLL_METHODS: &[(&str, &str, usize)] = &[
    ("find", "({})", 2),
    ("findOne", "({})", 2),
    ("aggregate", "([])", 2),
    ("countDocuments", "({})", 2),
    ("estimatedDocumentCount", "()", 0),
    ("distinct", "('')", 2),
    ("insertOne", "({})", 2),
    ("insertMany", "([])", 2),
    ("updateOne", "({}, {$set: {}})", 13),
    ("updateMany", "({}, {$set: {}})", 13),
    ("replaceOne", "({}, {})", 6),
    ("deleteOne", "({})", 2),
    ("deleteMany", "({})", 2),
    ("getIndexes", "()", 0),
    ("stats", "()", 0),
    ("drop", "()", 0),
];
const CURSOR_METHODS: &[(&str, &str, usize)] = &[("sort", "({})", 2), ("limit", "()", 1), ("skip", "()", 1), ("projection", "({})", 2), ("count", "()", 0), ("toArray", "()", 0)];
const DB_METHODS: &[(&str, &str, usize)] = &[("getCollection", "('')", 2), ("getSiblingDB", "('')", 2), ("runCommand", "({})", 2), ("adminCommand", "({})", 2), ("getCollectionNames", "()", 0), ("createCollection", "('')", 2), ("stats", "()", 0), ("version", "()", 0)];

/// MongoDB's operators: query, update, aggregation stages and expressions.
pub const MONGO_OPERATORS: &[&str] = &[
    "$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$nin", "$and", "$or", "$nor", "$not", "$exists", "$type", "$regex", "$options", "$expr", "$elemMatch", "$size",
    "$all", "$mod", "$text", "$search", "$where", "$jsonSchema", "$set", "$unset", "$inc", "$mul", "$rename", "$min", "$max", "$push", "$pull", "$pullAll", "$addToSet",
    "$pop", "$each", "$position", "$slice", "$currentDate", "$setOnInsert", "$match", "$group", "$project", "$sort", "$limit", "$skip", "$lookup", "$unwind", "$count",
    "$facet", "$addFields", "$replaceRoot", "$replaceWith", "$sample", "$sortByCount", "$bucket", "$out", "$merge", "$unionWith", "$graphLookup", "$densify",
    "$setWindowFields", "$sum", "$avg", "$first", "$last", "$push", "$concat", "$toString", "$toInt", "$toDate", "$dateToString", "$cond", "$ifNull", "$switch",
    "$size", "$arrayElemAt", "$filter", "$map", "$reduce", "$substr", "$toLower", "$toUpper", "$split", "$year", "$month", "$dayOfMonth", "$subtract", "$add",
    "$multiply", "$divide", "$round",
];

const MONGO_HELPERS: &[(&str, &str, usize)] = &[("ObjectId", "('')", 2), ("ISODate", "('')", 2), ("NumberLong", "()", 1), ("NumberInt", "()", 1), ("NumberDecimal", "('')", 2), ("UUID", "('')", 2)];

/// Suggestions for MongoDB's shell syntax (the Shell tab, pipelines, the query bar, validation rules).
pub fn mongo(names: Shared) -> Suggest {
    Rc::new(move |before: &str| {
        // In a comment: nothing. In a string: only `$` (a field path or operator in an aggregation).
        let w = word(before);
        if in_text(before, false) && !w.starts_with('$') {
            return None;
        }
        let head = &before[..before.len() - w.len()];
        let n = names.borrow();
        let methods = |list: &[(&str, &'static str, usize)]| list.iter().map(|(m, s, b)| item(m, Kind::Word, s, *b)).collect::<Vec<_>>();
        // db.<collection>  ·  db.<collection>.<method>  ·  ).<cursor method>
        if head.ends_with("db.") && !head.ends_with(".db.") {
            let mut all: Vec<Item> = n.tables.iter().map(|t| item(t, Kind::Type, "", 0)).collect();
            all.extend(methods(DB_METHODS));
            return Some((w.to_string(), pick(w, all))).filter(|(_, l)| !l.is_empty());
        }
        if let Some(rest) = head.strip_suffix('.') {
            let target = word(rest);
            let before_target = &rest[..rest.len() - target.len()];
            if !target.is_empty() && before_target.ends_with("db.") {
                return Some((w.to_string(), pick(w, methods(COLL_METHODS)))).filter(|(_, l)| !l.is_empty());
            }
            if rest.ends_with(')') {
                return Some((w.to_string(), pick(w, methods(CURSOR_METHODS)))).filter(|(_, l)| !l.is_empty());
            }
            return None;
        }
        if w.starts_with('$') {
            let ops = MONGO_OPERATORS.iter().map(|o| item(o, Kind::Keyword, "", 0)).collect();
            return Some((w.to_string(), pick(w, ops))).filter(|(_, l)| !l.is_empty());
        }
        if w.is_empty() {
            return None;
        }
        // A field name, a helper, a literal.
        let mut all: Vec<Item> = n.fields.iter().map(|f| item(f, Kind::Attr, "", 0)).collect();
        all.extend(MONGO_HELPERS.iter().map(|(h, s, b)| item(h, Kind::Word, s, *b)));
        all.extend(["true", "false", "null", "db"].iter().map(|k| item(k, Kind::Keyword, "", 0)));
        Some((w.to_string(), pick(w, all))).filter(|(_, l)| !l.is_empty())
    })
}

const SQL_FUNCTIONS: &[&str] = &[
    "count", "sum", "avg", "min", "max", "coalesce", "nullif", "lower", "upper", "length", "trim", "substring", "concat", "replace", "now", "current_date",
    "current_timestamp", "date_trunc", "extract", "to_char", "round", "floor", "ceil", "abs", "row_number", "rank", "dense_rank", "lag", "lead", "string_agg",
    "array_agg", "json_agg", "jsonb_build_object", "getdate", "isnull", "len", "cast", "convert",
];

/// Suggestions for SQL: keywords, functions, tables (first after FROM, JOIN, INTO, UPDATE…) and columns.
pub fn sql(names: Shared) -> Suggest {
    Rc::new(move |before: &str| {
        let w = word(before);
        if w.is_empty() || w.starts_with('$') || in_text(before, true) {
            return None;
        }
        let head = before[..before.len() - w.len()].trim_end().to_uppercase();
        let n = names.borrow();
        let tables = n.tables.iter().map(|t| item(t, Kind::Type, "", 0));
        let wants_table = ["FROM", "JOIN", "INTO", "UPDATE", "TABLE"].iter().any(|k| head.ends_with(k));
        // Upper or lower case keywords, as the user writes them.
        let lower = w.chars().all(|c| !c.is_uppercase());
        let keywords = crate::highlight::SQL_KEYWORDS.iter().map(|k| item(&if lower { k.to_lowercase() } else { k.to_string() }, Kind::Keyword, "", 0));
        let mut all: Vec<Item> = Vec::new();
        if wants_table {
            all.extend(tables);
            all.extend(keywords);
        } else {
            all.extend(n.fields.iter().map(|f| item(f, Kind::Attr, "", 0)));
            all.extend(keywords);
            all.extend(SQL_FUNCTIONS.iter().map(|f| item(f, Kind::Word, "()", 1)));
            all.extend(tables);
        }
        Some((w.to_string(), pick(w, all))).filter(|(_, l)| !l.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(s: &Suggest, before: &str) -> Vec<String> {
        s(before).map(|(_, l)| l.into_iter().map(|i| i.label).collect()).unwrap_or_default()
    }

    #[test]
    fn mongo_suggests_by_place() {
        let names: Shared = Rc::new(RefCell::new(Names { tables: vec!["users".into(), "advances".into()], fields: vec!["status".into(), "stage".into()] }));
        let s = mongo(names);
        assert_eq!(labels(&s, "db.ad")[0], "advances");
        assert!(labels(&s, "db.").contains(&"users".to_string()), "every collection right after db.");
        assert_eq!(labels(&s, "db.users.fi"), ["find", "findOne"]);
        assert_eq!(labels(&s, "db.users.find({}).li"), ["limit"]);
        assert_eq!(labels(&s, "{ age: { $g"), ["$gt", "$gte", "$group", "$graphLookup"]);
        assert_eq!(labels(&s, "{ sta"), ["status", "stage"]);
        assert_eq!(labels(&s, "{ _id: Obj"), ["ObjectId"]);
        assert!(s("{ ").is_none(), "nothing typed, nothing shown");
        assert!(s("{ name: 'sta").is_none(), "not in a string");
        assert!(s("// sta").is_none(), "not in a comment");
        let (_, l) = s("db.users.upd").unwrap();
        assert_eq!((l[0].suffix, l[0].back), ("({}, {$set: {}})", 13), "the cursor lands in the filter");
    }

    #[test]
    fn sql_suggests_tables_after_from() {
        let names: Shared = Rc::new(RefCell::new(Names { tables: vec!["orders".into(), "order_items".into()], fields: vec!["ordered_at".into()] }));
        let s = sql(names);
        assert_eq!(labels(&s, "SELECT * FROM ord")[..2], ["orders", "order_items"]);
        assert_eq!(labels(&s, "SELECT ord")[0], "ordered_at", "columns first elsewhere");
        assert_eq!(labels(&s, "sel"), ["select"], "lower case as typed");
        assert_eq!(labels(&s, "SEL"), ["SELECT"]);
        assert!(labels(&s, "SELECT coun").contains(&"count".to_string()));
        assert!(s("SELECT * FROM t WHERE name = 'ord").is_none(), "not in a string");
        assert!(s("SELECT 1 -- ord").is_none(), "not in a comment");
        assert!(!labels(&s, "SELECT 'it''s' FROM ord").is_empty(), "after a string with '' in it");
    }
}
