//! `{{variables}}`: their values (request, folder, collection, environment, global, .env,
//! the system's), and the dynamic ones (`{{$uuid}}`, `{{$timestamp}}`, random data).

use super::{Environment, Row};
use std::collections::HashMap;

/// Where variables are looked up, most specific first.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    /// (scope name, its values), in order: request (scripts), folders (innermost first),
    /// collection, environment, global, .env.
    pub layers: Vec<(String, HashMap<String, String>)>,
}

pub fn map(rows: &[Row]) -> HashMap<String, String> {
    rows.iter().filter(|r| r.on && !r.key.is_empty()).map(|r| (r.key.clone(), r.value.clone())).collect()
}

/// A .env file's `KEY=value` lines.
pub fn dotenv(path: &str) -> HashMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(v);
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

impl Vars {
    /// The usual layers: folders (innermost first, collection last), the active environment,
    /// the global one, and their .env files.
    pub fn new(folders_inner_first: &[&[Row]], active: Option<&Environment>, global: Option<&Environment>) -> Vars {
        let mut v = Vars { layers: vec![("request".into(), HashMap::new())] };
        for (i, f) in folders_inner_first.iter().enumerate() {
            v.layers.push((if i + 1 == folders_inner_first.len() { "collection".into() } else { "folder".into() }, map(f)));
        }
        if let Some(e) = active {
            v.layers.push((e.name.clone(), map(&e.vars)));
        }
        if let Some(g) = global {
            v.layers.push(("global".into(), map(&g.vars)));
        }
        for e in active.into_iter().chain(global) {
            if !e.dotenv.is_empty() {
                v.layers.push((".env".into(), dotenv(&e.dotenv)));
            }
        }
        v
    }

    /// A variable's value and where it's from (system environment variables last).
    pub fn get(&self, name: &str) -> Option<(String, String)> {
        for (scope, m) in &self.layers {
            if let Some(v) = m.get(name) {
                return Some((v.clone(), scope.clone()));
            }
        }
        std::env::var(name).ok().map(|v| (v, "system".into()))
    }

    /// Every variable's value (the nearest layer's), for scripts.
    pub fn flat(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (_, m) in self.layers.iter().rev() {
            out.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        out
    }

    /// Sets one for the rest of this run (scripts, "set from response").
    pub fn set(&mut self, scope: &str, name: &str, value: &str) {
        let ix = self.layers.iter().position(|(s, _)| s == scope).unwrap_or(0);
        self.layers[ix].1.insert(name.to_string(), value.to_string());
    }

    /// `{{name}}` replaced everywhere (values may hold variables too, a few levels deep).
    /// Unknown ones stay as written, and are listed.
    pub fn resolve(&self, text: &str, missing: &mut Vec<String>) -> String {
        let mut out = text.to_string();
        for _ in 0..4 {
            if !out.contains("{{") {
                break;
            }
            out = self.once(&out, missing);
        }
        out
    }

    fn once(&self, text: &str, missing: &mut Vec<String>) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                out.push_str(&rest[start..]);
                return out;
            };
            let name = after[..end].trim();
            match dynamic(name).or_else(|| self.get(name).map(|(v, _)| v)) {
                Some(v) => out.push_str(&v),
                None => {
                    if !missing.iter().any(|m| m == name) {
                        missing.push(name.to_string());
                    }
                    out.push_str(&rest[start..start + 2 + end + 2]);
                }
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        out
    }
}

/// The dynamic variables: new each time they're used.
pub const DYNAMIC: &[(&str, &str)] = &[
    ("$uuid", "a random UUID v4"),
    ("$guid", "a random UUID v4"),
    ("$timestamp", "seconds since 1970"),
    ("$timestampMs", "milliseconds since 1970"),
    ("$isoTimestamp", "now, as ISO 8601 (UTC)"),
    ("$date", "today, YYYY-MM-DD"),
    ("$randomInt", "0 to 1000"),
    ("$randomFloat", "0 to 1"),
    ("$randomBoolean", "true or false"),
    ("$randomFirstName", "a first name"),
    ("$randomLastName", "a last name"),
    ("$randomFullName", "a full name"),
    ("$randomEmail", "an email address"),
    ("$randomUserName", "a user name"),
    ("$randomPhone", "a phone number"),
    ("$randomCity", "a city"),
    ("$randomCountry", "a country"),
    ("$randomColor", "a color name"),
    ("$randomHexColor", "#rrggbb"),
    ("$randomWord", "a word"),
    ("$randomSentence", "a sentence"),
    ("$randomIP", "an IPv4 address"),
    ("$randomUrl", "a URL"),
    ("$randomAlphaNumeric", "8 letters and digits"),
];

const FIRST: &[&str] = &["Ann", "Bob", "Cyrus", "Dina", "Eli", "Fatima", "Gus", "Hana", "Ivan", "Julia", "Karim", "Lea", "Maya", "Noah", "Omar", "Priya"];
const LAST: &[&str] = &["Lee", "Martin", "Nakamura", "Okafor", "Petrov", "Quinn", "Rossi", "Silva", "Tanaka", "Ueda", "Weber", "Young"];
const CITIES: &[&str] = &["Paris", "Berlin", "Tokyo", "Lisbon", "Casablanca", "Montreal", "Madrid", "Oslo", "Seoul", "Lima"];
const COUNTRIES: &[&str] = &["France", "Germany", "Japan", "Portugal", "Morocco", "Canada", "Spain", "Norway", "Korea", "Peru"];
const COLORS: &[&str] = &["red", "green", "blue", "orange", "purple", "teal", "yellow", "black", "white", "gray"];
const WORDS: &[&str] = &["alpha", "bridge", "cloud", "delta", "ember", "forest", "glow", "harbor", "island", "jungle", "oskl", "lunar", "meadow", "nova", "orbit", "pixel"];

fn pick(list: &[&str]) -> String {
    list[fastrand::usize(..list.len())].to_string()
}

pub fn uuid() -> String {
    let mut b = [0u8; 16];
    for x in b.iter_mut() {
        *x = fastrand::u8(..);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Now as ISO 8601, UTC (no date library needed).
pub fn iso_now() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (y, m, day) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", t / 3600, t / 60 % 60, t % 60, d.subsec_millis())
}

/// (year, month, day) from days since 1970-01-01.
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

fn dynamic(name: &str) -> Option<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Some(match name {
        "$uuid" | "$guid" | "$randomUUID" => uuid(),
        "$timestamp" => now.as_secs().to_string(),
        "$timestampMs" => now.as_millis().to_string(),
        "$isoTimestamp" => iso_now(),
        "$date" => iso_now()[..10].to_string(),
        "$randomInt" => fastrand::u32(0..=1000).to_string(),
        "$randomFloat" => format!("{:.4}", fastrand::f64()),
        "$randomBoolean" => fastrand::bool().to_string(),
        "$randomFirstName" => pick(FIRST),
        "$randomLastName" => pick(LAST),
        "$randomFullName" => format!("{} {}", pick(FIRST), pick(LAST)),
        "$randomEmail" => format!("{}.{}{}@example.com", pick(FIRST).to_lowercase(), pick(LAST).to_lowercase(), fastrand::u16(1..999)),
        "$randomUserName" => format!("{}{}", pick(FIRST).to_lowercase(), fastrand::u16(1..9999)),
        "$randomPhone" => format!("+1-{:03}-{:03}-{:04}", fastrand::u16(200..999), fastrand::u16(100..999), fastrand::u16(0..9999)),
        "$randomCity" => pick(CITIES),
        "$randomCountry" => pick(COUNTRIES),
        "$randomColor" => pick(COLORS),
        "$randomHexColor" => format!("#{:06x}", fastrand::u32(0..0xffffff)),
        "$randomWord" => pick(WORDS),
        "$randomSentence" => {
            let n = fastrand::usize(4..9);
            let mut s: Vec<String> = (0..n).map(|_| pick(WORDS)).collect();
            s[0] = format!("{}{}", s[0][..1].to_uppercase(), &s[0][1..]);
            format!("{}.", s.join(" "))
        }
        "$randomIP" => format!("{}.{}.{}.{}", fastrand::u8(1..255), fastrand::u8(..), fastrand::u8(..), fastrand::u8(1..255)),
        "$randomUrl" => format!("https://{}.example.com/{}", pick(WORDS), pick(WORDS)),
        "$randomAlphaNumeric" => (0..8).map(|_| fastrand::alphanumeric()).collect(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_resolve_by_precedence_and_nest() {
        let env = Environment { name: "Dev".into(), vars: vec![Row::new("host", "localhost:3000"), Row::new("base", "http://{{host}}"), Row::new("who", "env")], ..Default::default() };
        let global = Environment { name: "Global".into(), vars: vec![Row::new("who", "global"), Row::new("g", "1")], ..Default::default() };
        let folder = vec![Row::new("who", "folder")];
        let coll = vec![Row::new("api", "v2")];
        let v = Vars::new(&[&folder, &coll], Some(&env), Some(&global));
        let mut missing = Vec::new();
        assert_eq!(v.resolve("{{base}}/{{api}}/users?w={{who}}&g={{g}}&x={{nope}}", &mut missing), "http://localhost:3000/v2/users?w=folder&g=1&x={{nope}}");
        assert_eq!(missing, ["nope"]);
        assert_eq!(v.get("who").unwrap().1, "folder");
        let id = v.resolve("{{$uuid}}", &mut missing);
        assert_eq!((id.len(), &id[14..15]), (36, "4"));
        assert!(v.resolve("{{$isoTimestamp}}", &mut missing).ends_with('Z'));
        assert_eq!(civil(19_723), (2024, 1, 1));
    }
}
