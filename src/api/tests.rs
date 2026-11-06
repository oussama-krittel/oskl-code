//! Checks on a response without code (status, time, headers, JSONPath, JSON Schema…) and
//! variables set from it.

use super::send::Response;
use super::vars::Vars;
use super::{SetVar, Test};

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub name: String,
    pub pass: bool,
    /// Why it failed (what was there).
    pub detail: String,
}

pub const WHATS: [&str; 7] = ["status", "time", "header", "json", "body", "content-type", "schema"];
pub const OPS: [&str; 13] = ["equal", "not-equal", "contains", "not-contains", "<", "<=", ">", ">=", "exists", "not-exists", "type", "count", "matches"];

/// The values at a JSONPath (`$.a.b[0]`; `a.b` works too).
pub fn json_path(v: &serde_json::Value, path: &str) -> Result<Vec<serde_json::Value>, String> {
    let p = path.trim();
    let p = if p.starts_with('$') { p.to_string() } else if p.starts_with('[') { format!("${p}") } else { format!("$.{p}") };
    let q = serde_json_path::JsonPath::parse(&p).map_err(|e| format!("JSONPath {p}: {e}"))?;
    Ok(q.query(v).all().into_iter().cloned().collect())
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn as_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Compares what was found with the test's value.
fn compare(found: Option<&str>, json: Option<&serde_json::Value>, op: &str, want: &str) -> bool {
    let num = |s: &str| s.trim().parse::<f64>().ok();
    match op {
        "exists" => found.is_some(),
        "not-exists" => found.is_none(),
        _ => {
            let Some(f) = found else { return false };
            match op {
                "equal" => f == want || (num(f).is_some() && num(f) == num(want)) || json.is_some_and(|j| serde_json::from_str::<serde_json::Value>(want).is_ok_and(|w| &w == j)),
                "not-equal" => !(f == want || (num(f).is_some() && num(f) == num(want))),
                "contains" => f.contains(want),
                "not-contains" => !f.contains(want),
                "<" | "<=" | ">" | ">=" => match (num(f), num(want)) {
                    (Some(a), Some(b)) => match op {
                        "<" => a < b,
                        "<=" => a <= b,
                        ">" => a > b,
                        _ => a >= b,
                    },
                    _ => false,
                },
                "type" => json.map(type_name).unwrap_or("string") == want.trim().to_lowercase(),
                "count" => {
                    let n = match json {
                        Some(serde_json::Value::Array(a)) => a.len(),
                        Some(serde_json::Value::Object(o)) => o.len(),
                        _ => f.chars().count(),
                    };
                    num(want).is_some_and(|w| w as usize == n)
                }
                "matches" => regex::Regex::new(want).is_ok_and(|r| r.is_match(f)),
                _ => false,
            }
        }
    }
}

pub fn run(tests: &[Test], resp: &Response, vars: &Vars) -> Vec<Outcome> {
    let json = resp.json();
    tests
        .iter()
        .filter(|t| t.on)
        .map(|t| {
            let mut missing = Vec::new();
            let want = vars.resolve(&t.value, &mut missing);
            let name = match t.what.as_str() {
                "header" | "json" => format!("{} {} {} {}", t.what, t.path, t.op, want),
                "schema" => "matches the JSON Schema".to_string(),
                _ => format!("{} {} {}", t.what, t.op, want),
            };
            let (pass, detail) = match t.what.as_str() {
                "status" => {
                    let s = resp.status.to_string();
                    (compare(Some(&s), None, &t.op, &want), format!("status {s}"))
                }
                "time" => {
                    let s = resp.ms.to_string();
                    (compare(Some(&s), None, &t.op, &want), format!("{s} ms"))
                }
                "content-type" => {
                    let c = resp.header("content-type").map(str::to_string);
                    (compare(c.as_deref(), None, &t.op, &want), c.unwrap_or_else(|| "no Content-Type".into()))
                }
                "header" => {
                    let h = resp.header(&t.path).map(str::to_string);
                    (compare(h.as_deref(), None, &t.op, &want), h.unwrap_or_else(|| "not there".into()))
                }
                "body" => {
                    let b = resp.text();
                    let shown: String = b.chars().take(120).collect();
                    (compare(Some(&b), None, &t.op, &want), shown)
                }
                "json" => match &json {
                    None => (false, "the body isn't JSON".into()),
                    Some(j) => match json_path(j, &t.path) {
                        Err(e) => (false, e),
                        Ok(found) => {
                            let first = found.first();
                            let text = first.map(as_text);
                            let mut pass = compare(text.as_deref(), first, &t.op, &want);
                            // count on a path with many matches counts the matches.
                            if t.op == "count" && found.len() != 1 {
                                pass = want.trim().parse::<usize>().is_ok_and(|n| n == found.len());
                            }
                            (pass, text.map(|s| s.chars().take(120).collect()).unwrap_or_else(|| "nothing there".into()))
                        }
                    },
                },
                "schema" => match (&json, serde_json::from_str::<serde_json::Value>(&want)) {
                    (None, _) => (false, "the body isn't JSON".into()),
                    (_, Err(e)) => (false, format!("the schema isn't JSON: {e}")),
                    (Some(j), Ok(schema)) => match jsonschema::validator_for(&schema) {
                        Err(e) => (false, format!("the schema: {e}")),
                        Ok(v) => {
                            let errs: Vec<String> = v.iter_errors(j).take(3).map(|e| format!("{} at {}", e, e.instance_path())).collect();
                            (errs.is_empty(), errs.join("; "))
                        }
                    },
                },
                other => (false, format!("{other}: unknown check")),
            };
            Outcome { name, pass, detail: if pass { String::new() } else { detail } }
        })
        .collect()
}

/// What each "set variable" takes from the response: (variable, scope, value).
pub fn set_vars(rules: &[SetVar], resp: &Response) -> Vec<(String, String, String)> {
    let json = resp.json();
    rules
        .iter()
        .filter(|r| r.on && !r.var.trim().is_empty())
        .filter_map(|r| {
            let v = match r.from.as_str() {
                "status" => Some(resp.status.to_string()),
                "body" => Some(resp.text()),
                "header" => resp.header(&r.path).map(str::to_string),
                "cookie" => resp
                    .headers
                    .iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
                    .filter_map(|(_, v)| v.split(';').next()?.split_once('=').map(|(a, b)| (a.trim().to_string(), b.trim().to_string())))
                    .find(|(k, _)| *k == r.path.trim())
                    .map(|(_, v)| v),
                _ => json.as_ref().and_then(|j| json_path(j, &r.path).ok()).and_then(|f| f.first().map(as_text)),
            }?;
            Some((r.var.trim().to_string(), r.scope.clone(), v))
        })
        .collect()
}

#[cfg(test)]
mod checks {
    use super::*;

    fn resp() -> Response {
        Response {
            status: 201,
            ms: 42,
            headers: vec![("Content-Type".into(), "application/json; charset=utf-8".into()), ("Set-Cookie".into(), "sid=xyz; Path=/".into())],
            body: br#"{"id":7,"user":{"name":"Ada","roles":["admin","dev"]},"items":[{"p":1},{"p":2}]}"#.to_vec(),
            ..Default::default()
        }
    }

    fn t(what: &str, path: &str, op: &str, value: &str) -> Test {
        Test { on: true, what: what.into(), path: path.into(), op: op.into(), value: value.into() }
    }

    #[test]
    fn checks_pass_and_fail_with_reasons() {
        let tests = [
            t("status", "", "equal", "201"),
            t("time", "", "<", "500"),
            t("content-type", "", "contains", "json"),
            t("json", "$.user.name", "equal", "Ada"),
            t("json", "user.roles", "count", "2"),
            t("json", "$.items[*].p", "count", "2"),
            t("json", "$.id", "type", "number"),
            t("json", "$.missing", "not-exists", ""),
            t("header", "content-type", "matches", "^application/"),
            t("schema", "", "", r#"{"type":"object","required":["id"],"properties":{"id":{"type":"integer"}}}"#),
            t("status", "", "equal", "200"),
            t("schema", "", "", r#"{"required":["nope"]}"#),
        ];
        let out = run(&tests, &resp(), &Vars::default());
        let passed: Vec<bool> = out.iter().map(|o| o.pass).collect();
        assert_eq!(passed, [true, true, true, true, true, true, true, true, true, true, false, false], "{out:#?}");
        assert_eq!(out[10].detail, "status 201");
        assert!(out[11].detail.contains("nope"), "{}", out[11].detail);
    }

    #[test]
    fn variables_come_from_the_response() {
        let rules = [
            SetVar { var: "uid".into(), from: "json".into(), path: "$.id".into(), ..Default::default() },
            SetVar { var: "who".into(), from: "json".into(), path: "user.name".into(), scope: "global".into(), ..Default::default() },
            SetVar { var: "sid".into(), from: "cookie".into(), path: "sid".into(), ..Default::default() },
        ];
        assert_eq!(
            set_vars(&rules, &resp()),
            [("uid".into(), "environment".into(), "7".into()), ("who".into(), "global".into(), "Ada".into()), ("sid".into(), "environment".into(), "xyz".into())]
        );
    }
}
