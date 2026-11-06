//! Bringing requests in (cURL, Postman 2.1, Insomnia v4, OpenAPI 3, Bruno, .env) and out
//! (cURL, Postman 2.1, OpenAPI 3).

use super::{Auth, Body, Collection, Environment, Folder, Item, Kind, Request, Row, new_id};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

/// A collection and its requests, not saved yet.
#[derive(Debug, Default)]
pub struct Imported {
    pub collection: Collection,
    pub requests: Vec<Request>,
    pub environments: Vec<Environment>,
}

impl Imported {
    fn item(&mut self, folder: Option<&str>, req: Request) {
        let method = match req.kind {
            Kind::Http => req.method.clone(),
            Kind::Websocket => "WS".into(),
            Kind::Sse => "SSE".into(),
        };
        let item = Item { id: req.id.clone(), name: req.name.clone(), method };
        self.collection.root.folder_or_self(folder).requests.push(item);
        self.requests.push(req);
    }

    /// Writes it all (the collection where `project` says).
    pub fn save(mut self, project: Option<std::path::PathBuf>) -> Result<Collection, String> {
        self.collection.project = project.clone();
        super::save_collection(&self.collection)?;
        let reqs = std::mem::take(&mut self.requests);
        // The tree has the entries already: this writes each request's file.
        let mut c = self.collection.clone();
        for r in &reqs {
            super::save_request(&mut c, r, None)?;
        }
        self.collection = c;
        for mut e in self.environments {
            e.project = project.clone();
            super::save_environment(&e)?;
        }
        Ok(self.collection)
    }
}

/// Whatever it is: a cURL command, or a Postman, Insomnia or OpenAPI file (JSON or YAML).
pub fn import_text(text: &str, name_hint: &str) -> Result<Imported> {
    let t = text.trim_start();
    if t.starts_with("curl ") || t.starts_with("curl\t") {
        let req = curl(t)?;
        let mut imp = Imported { collection: Collection::new(if name_hint.is_empty() { "Imported" } else { name_hint }, None), ..Default::default() };
        imp.item(None, req);
        return Ok(imp);
    }
    let v: Value = match serde_json::from_str(t) {
        Ok(v) => v,
        Err(_) => yaml_to_json(t)?,
    };
    if v.get("info").and_then(|i| i.get("schema")).and_then(Value::as_str).is_some_and(|s| s.contains("postman")) || v.get("item").is_some() {
        return postman(&v);
    }
    if v.get("_type").and_then(Value::as_str) == Some("export") || v.get("resources").is_some() {
        return insomnia(&v);
    }
    if v.get("openapi").is_some() || v.get("swagger").is_some() {
        return openapi(&v);
    }
    bail!("not a format this knows: cURL, Postman 2.1, Insomnia v4 or OpenAPI 3")
}

fn yaml_to_json(text: &str) -> Result<Value> {
    use yaml_rust2::{Yaml, YamlLoader};
    fn conv(y: &Yaml) -> Value {
        match y {
            Yaml::Real(s) => s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::String(s.clone())),
            Yaml::Integer(i) => json!(i),
            Yaml::String(s) => json!(s),
            Yaml::Boolean(b) => json!(b),
            Yaml::Array(a) => Value::Array(a.iter().map(conv).collect()),
            Yaml::Hash(h) => Value::Object(
                h.iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Yaml::String(s) => s.clone(),
                            Yaml::Integer(i) => i.to_string(),
                            Yaml::Real(r) => r.clone(),
                            Yaml::Boolean(b) => b.to_string(),
                            _ => String::new(),
                        };
                        (key, conv(v))
                    })
                    .collect(),
            ),
            _ => Value::Null,
        }
    }
    let docs = YamlLoader::load_from_str(text).map_err(|e| anyhow!("neither JSON nor YAML: {e}"))?;
    docs.first().map(conv).ok_or_else(|| anyhow!("empty"))
}

// ---------------------------------------------------------------- cURL

/// Splits a shell command line into words (quotes, escapes, `\` line breaks).
pub fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') => {
                    chars.next_if_eq(&'\n');
                }
                Some(n) => {
                    cur.push(n);
                    has = true;
                }
                None => {}
            },
            '\'' => {
                has = true;
                for n in chars.by_ref() {
                    if n == '\'' {
                        break;
                    }
                    cur.push(n);
                }
            }
            '"' => {
                has = true;
                while let Some(n) = chars.next() {
                    match n {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => cur.push(chars.next().unwrap()),
                        '\\' if chars.peek() == Some(&'\n') => {
                            chars.next();
                        }
                        n => cur.push(n),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                // $'…' with \n, \t, \' escapes.
                chars.next();
                has = true;
                while let Some(n) = chars.next() {
                    match n {
                        '\'' => break,
                        '\\' => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('t') => cur.push('\t'),
                            Some('r') => cur.push('\r'),
                            Some(x) => cur.push(x),
                            None => {}
                        },
                        n => cur.push(n),
                    }
                }
            }
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// A request from a `curl …` command.
pub fn curl(cmd: &str) -> Result<Request> {
    let words = shell_words(cmd);
    if words.first().map(String::as_str) != Some("curl") {
        bail!("not a curl command");
    }
    let mut req = Request { name: "Imported".into(), ..Default::default() };
    let mut method: Option<String> = None;
    let mut data: Vec<String> = Vec::new();
    let mut form: Vec<Row> = Vec::new();
    let mut get = false;
    let mut urlencode = false;
    let mut i = 1;
    let next = |i: &mut usize| -> Option<String> {
        *i += 1;
        words.get(*i).cloned()
    };
    while i < words.len() {
        let w = words[i].clone();
        // --flag=value
        let (flag, inline) = match w.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (w.clone(), None),
        };
        let val = |i: &mut usize| inline.clone().or_else(|| next(i)).unwrap_or_default();
        match flag.as_str() {
            "-X" | "--request" => method = Some(val(&mut i).to_uppercase()),
            "-H" | "--header" => {
                let h = val(&mut i);
                if let Some((k, v)) = h.split_once(':') {
                    req.headers.push(Row::new(k.trim(), v.trim()));
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" | "--json" => {
                let d = val(&mut i);
                if flag == "--json" {
                    req.headers.push(Row::new("Content-Type", "application/json"));
                }
                data.push(d);
            }
            "--data-urlencode" => {
                let d = val(&mut i);
                urlencode = true;
                let (k, v) = d.split_once('=').unwrap_or(("", &d));
                data.push(if k.is_empty() { super::send::encode_component(v) } else { format!("{k}={}", super::send::encode_component(v)) });
            }
            "-F" | "--form" | "--form-string" => {
                let f = val(&mut i);
                let (k, v) = f.split_once('=').unwrap_or((&f, ""));
                let (file, v) = match v.strip_prefix('@') {
                    Some(p) => (true, p.split(';').next().unwrap_or(p).to_string()),
                    None => (false, v.to_string()),
                };
                form.push(Row { flag: file, ..Row::new(k, &v) });
            }
            "-u" | "--user" => {
                let u = val(&mut i);
                let (user, pass) = u.split_once(':').unwrap_or((&u, ""));
                req.auth = Auth::Basic { user: user.into(), pass: pass.into() };
            }
            "-A" | "--user-agent" => req.headers.push(Row::new("User-Agent", &val(&mut i))),
            "-b" | "--cookie" => req.headers.push(Row::new("Cookie", &val(&mut i))),
            "-e" | "--referer" => req.headers.push(Row::new("Referer", &val(&mut i))),
            "--url" => req.url = val(&mut i),
            "-k" | "--insecure" => req.settings.verify_tls = false,
            "-L" | "--location" => req.settings.follow_redirects = true,
            "-G" | "--get" => get = true,
            "-I" | "--head" => method = Some("HEAD".into()),
            "-x" | "--proxy" => req.settings.proxy = val(&mut i),
            "-m" | "--max-time" => req.settings.timeout_ms = (val(&mut i).parse::<f64>().unwrap_or(30.0) * 1000.0) as u64,
            "--http1.1" => req.settings.http = "1.1".into(),
            "--http2" | "--http2-prior-knowledge" => req.settings.http = "2".into(),
            "-o" | "--output" | "-w" | "--write-out" | "--connect-timeout" | "--retry" | "-T" | "--upload-file" | "-E" | "--cert" | "--cacert" | "--key" => {
                val(&mut i);
            }
            f if f.starts_with('-') => {}
            _ => {
                if req.url.is_empty() {
                    req.url = w.clone();
                }
            }
        }
        i += 1;
    }
    if get && !data.is_empty() {
        let q = data.join("&");
        req.url = if req.url.contains('?') { format!("{}&{q}", req.url) } else { format!("{}?{q}", req.url) };
        data.clear();
    }
    if !form.is_empty() {
        req.body = Body::Multipart { rows: form };
    } else if !data.is_empty() {
        let d = data.join("&");
        let ct = req.headers.iter().find(|h| h.key.eq_ignore_ascii_case("content-type")).map(|h| h.value.to_lowercase()).unwrap_or_default();
        req.body = if ct.contains("json") || (ct.is_empty() && !urlencode && serde_json::from_str::<Value>(&d).is_ok_and(|v| v.is_object() || v.is_array())) {
            Body::Json { text: pretty(&d) }
        } else if ct.contains("xml") {
            Body::Xml { text: d }
        } else if ct.contains("x-www-form-urlencoded") || ct.is_empty() {
            Body::Form { rows: d.split('&').map(|p| p.split_once('=').unwrap_or((p, ""))).map(|(k, v)| Row::new(&super::send::decode(k), &super::send::decode(v))).collect() }
        } else {
            Body::Text { text: d }
        };
        // The type comes from the body kind now.
        if matches!(req.body, Body::Json { .. } | Body::Form { .. }) {
            req.headers.retain(|h| !h.key.eq_ignore_ascii_case("content-type") || !(h.value.contains("json") || h.value.contains("x-www-form-urlencoded")));
        }
    }
    req.method = method.unwrap_or_else(|| if data.is_empty() && matches!(req.body, Body::None) { "GET".into() } else { "POST".into() });
    if let Some(a) = bearer_from_headers(&mut req.headers) {
        req.auth = a;
    }
    req.params = super::send::query_rows(&req.url);
    req.name = name_from_url(&req.method, &req.url);
    if req.url.is_empty() {
        bail!("the command has no URL");
    }
    Ok(req)
}

fn bearer_from_headers(h: &mut Vec<Row>) -> Option<Auth> {
    let i = h.iter().position(|r| r.key.eq_ignore_ascii_case("authorization") && r.value.to_lowercase().starts_with("bearer "))?;
    let token = h.remove(i).value[7..].trim().to_string();
    Some(Auth::Bearer { token, prefix: "Bearer".into() })
}

fn pretty(s: &str) -> String {
    serde_json::from_str::<Value>(s).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| s.to_string())
}

pub fn name_from_url(method: &str, url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map(|(_, r)| r).unwrap_or(path);
    let tail: Vec<&str> = path.split('/').skip(1).filter(|s| !s.is_empty()).collect();
    let t = tail.iter().rev().take(2).rev().cloned().collect::<Vec<_>>().join("/");
    if t.is_empty() { format!("{method} {}", path.split('/').next().unwrap_or("")) } else { format!("{method} /{t}") }
}

fn quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// A cURL command for a request, ready to send (variables filled in).
pub fn to_curl(r: &super::send::Ready) -> String {
    let mut parts = vec!["curl".to_string()];
    if r.method != "GET" || !matches!(r.body, Body::None) && r.method != "POST" {
        parts.push(format!("-X {}", r.method));
    }
    parts.push(quote(&r.url));
    let mut headers = r.headers.clone();
    match &r.auth {
        Auth::Basic { user, pass } => parts.push(format!("-u {}", quote(&format!("{user}:{pass}")))),
        Auth::Bearer { token, prefix } => headers.push(("Authorization".into(), format!("{} {token}", if prefix.is_empty() { "Bearer" } else { prefix }))),
        Auth::Apikey { key, value, query: false } => headers.push((key.clone(), value.clone())),
        Auth::Digest { user, pass } => parts.push(format!("--digest -u {}", quote(&format!("{user}:{pass}")))),
        _ => {}
    }
    let ct = match &r.body {
        Body::Json { .. } | Body::Graphql { .. } => Some("application/json"),
        Body::Xml { .. } => Some("application/xml"),
        Body::Text { .. } => Some("text/plain"),
        Body::Html { .. } => Some("text/html"),
        _ => None,
    };
    if let Some(ct) = ct
        && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("Content-Type".into(), ct.into()));
    }
    for (k, v) in &headers {
        parts.push(format!("-H {}", quote(&format!("{k}: {v}"))));
    }
    match &r.body {
        Body::Json { text } | Body::Xml { text } | Body::Text { text } | Body::Html { text } => parts.push(format!("--data-raw {}", quote(text))),
        Body::Form { rows } => {
            for x in rows.iter().filter(|x| x.on) {
                parts.push(format!("--data-urlencode {}", quote(&format!("{}={}", x.key, x.value))));
            }
        }
        Body::Multipart { rows } => {
            for x in rows.iter().filter(|x| x.on) {
                parts.push(format!("-F {}", quote(&format!("{}={}{}", x.key, if x.flag { "@" } else { "" }, x.value))));
            }
        }
        Body::Binary { path } => parts.push(format!("--data-binary {}", quote(&format!("@{path}")))),
        Body::Graphql { query, variables } => {
            let vars: Value = serde_json::from_str(variables).unwrap_or(Value::Null);
            parts.push(format!("--data-raw {}", quote(&json!({ "query": query, "variables": vars }).to_string())));
        }
        Body::None => {}
    }
    if !r.settings.verify_tls {
        parts.push("-k".into());
    }
    if r.settings.follow_redirects {
        parts.push("-L".into());
    }
    parts.join(" \\\n  ")
}

// ---------------------------------------------------------------- Postman 2.1

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn rows(v: Option<&Value>) -> Vec<Row> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| Row { on: !x.get("disabled").and_then(Value::as_bool).unwrap_or(false), key: s(x, "key"), value: s(x, "value"), desc: description(x.get("description")), flag: s(x, "type") == "file" || s(x, "type") == "secret" }).collect())
        .unwrap_or_default()
}

fn description(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => o.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// Postman's `{{x}}` is ours, and `:id` too.
fn postman_auth(a: Option<&Value>) -> Auth {
    let Some(a) = a else { return Auth::Inherit };
    let t = s(a, "type");
    let get = |k: &str| -> String {
        a.get(&t).and_then(|l| match l {
            Value::Array(arr) => arr.iter().find(|x| s(x, "key") == k).map(|x| s(x, "value")),
            Value::Object(o) => o.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())),
            _ => None,
        })
        .unwrap_or_default()
    };
    match t.as_str() {
        "noauth" => Auth::None,
        "basic" => Auth::Basic { user: get("username"), pass: get("password") },
        "bearer" => Auth::Bearer { token: get("token"), prefix: "Bearer".into() },
        "apikey" => Auth::Apikey { key: get("key"), value: get("value"), query: get("in") == "query" },
        "digest" => Auth::Digest { user: get("username"), pass: get("password") },
        "awsv4" => Auth::Aws { access_key: get("accessKey"), secret_key: get("secretKey"), session_token: get("sessionToken"), region: get("region"), service: get("service") },
        "oauth2" => Auth::Oauth2(super::OAuth2 {
            grant: match get("grant_type").as_str() {
                "authorization_code" | "authorization_code_with_pkce" => "authorization_code".into(),
                "password_credentials" => "password".into(),
                _ => "client_credentials".into(),
            },
            auth_url: get("authUrl"),
            token_url: get("accessTokenUrl"),
            client_id: get("clientId"),
            client_secret: get("clientSecret"),
            scope: get("scope"),
            username: get("username"),
            password: get("password"),
            redirect_uri: get("redirect_uri"),
            pkce: get("grant_type").contains("pkce"),
            prefix: "Bearer".into(),
            ..Default::default()
        }),
        _ => Auth::Inherit,
    }
}

fn postman_script(events: Option<&Value>, which: &str) -> String {
    events
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|e| s(e, "listen") == which))
        .and_then(|e| e.get("script")?.get("exec"))
        .map(|x| match x {
            Value::Array(l) => l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
            Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .unwrap_or_default()
}

fn postman_request(item: &Value) -> Request {
    let r = item.get("request").cloned().unwrap_or(Value::Null);
    let mut req = Request { name: s(item, "name"), ..Default::default() };
    if let Value::String(url) = &r {
        req.url = url.clone();
        return req;
    }
    req.method = s(&r, "method").to_uppercase();
    if req.method.is_empty() {
        req.method = "GET".into();
    }
    match r.get("url") {
        Some(Value::String(u)) => req.url = u.clone(),
        Some(u @ Value::Object(_)) => {
            req.url = s(u, "raw");
            req.path_params = rows(u.get("variable"));
            req.params = rows(u.get("query"));
        }
        _ => {}
    }
    if req.params.is_empty() {
        req.params = super::send::query_rows(&req.url);
    }
    req.headers = rows(r.get("header"));
    req.auth = postman_auth(r.get("auth"));
    req.docs = description(r.get("description"));
    if let Some(b) = r.get("body") {
        let lang = b.get("options").and_then(|o| o.get("raw")).map(|r| s(r, "language")).unwrap_or_default();
        req.body = match s(b, "mode").as_str() {
            "raw" => {
                let text = s(b, "raw");
                match lang.as_str() {
                    "json" => Body::Json { text },
                    "xml" => Body::Xml { text },
                    "html" => Body::Html { text },
                    _ if serde_json::from_str::<Value>(&text).is_ok_and(|v| v.is_object() || v.is_array()) => Body::Json { text },
                    _ => Body::Text { text },
                }
            }
            "urlencoded" => Body::Form { rows: rows(b.get("urlencoded")) },
            "formdata" => Body::Multipart {
                rows: b.get("formdata").and_then(Value::as_array).map(|a| a.iter().map(|x| Row { on: !x.get("disabled").and_then(Value::as_bool).unwrap_or(false), key: s(x, "key"), value: if s(x, "type") == "file" { s(x, "src") } else { s(x, "value") }, desc: s(x, "contentType"), flag: s(x, "type") == "file" }).collect()).unwrap_or_default(),
            },
            "file" => Body::Binary { path: b.get("file").map(|f| s(f, "src")).unwrap_or_default() },
            "graphql" => Body::Graphql { query: b.get("graphql").map(|g| s(g, "query")).unwrap_or_default(), variables: b.get("graphql").map(|g| s(g, "variables")).unwrap_or_default() },
            _ => Body::None,
        };
    }
    req.pre_script = postman_script(item.get("event"), "prerequest");
    req.post_script = postman_script(item.get("event"), "test");
    req
}

fn postman_items(imp: &mut Imported, folder: &str, items: &[Value]) {
    for it in items {
        if let Some(sub) = it.get("item").and_then(Value::as_array) {
            let f = Folder {
                name: s(it, "name"),
                auth: postman_auth(it.get("auth")),
                pre_script: postman_script(it.get("event"), "prerequest"),
                post_script: postman_script(it.get("event"), "test"),
                ..Default::default()
            };
            let id = f.id.clone();
            let f = Folder { auth: if f.auth == Auth::None { Auth::Inherit } else { f.auth.clone() }, ..f };
            imp.collection.root.find_mut(folder).unwrap().folders.push(f);
            postman_items(imp, &id, sub);
        } else {
            let req = postman_request(it);
            imp.item(Some(folder), req);
        }
    }
}

pub fn postman(v: &Value) -> Result<Imported> {
    let info = v.get("info").cloned().unwrap_or(Value::Null);
    let name = s(&info, "name");
    let mut imp = Imported { collection: Collection::new(if name.is_empty() { "Postman" } else { &name }, None), ..Default::default() };
    let root_id = imp.collection.root.id.clone();
    imp.collection.root.auth = match postman_auth(v.get("auth")) {
        Auth::Inherit => Auth::None,
        a => a,
    };
    imp.collection.root.vars = rows(v.get("variable"));
    imp.collection.root.pre_script = postman_script(v.get("event"), "prerequest");
    imp.collection.root.post_script = postman_script(v.get("event"), "test");
    postman_items(&mut imp, &root_id, v.get("item").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]));
    Ok(imp)
}

/// A Postman environment file (`{"name", "values": […]}`).
pub fn postman_environment(v: &Value) -> Option<Environment> {
    let values = v.get("values")?.as_array()?;
    Some(Environment {
        id: new_id(),
        name: s(v, "name"),
        vars: values.iter().map(|x| Row { on: x.get("enabled").and_then(Value::as_bool).unwrap_or(true), key: s(x, "key"), value: s(x, "value"), desc: String::new(), flag: s(x, "type") == "secret" }).collect(),
        ..Default::default()
    })
}

fn postman_auth_out(a: &Auth) -> Value {
    let kv = |pairs: &[(&str, &str)]| Value::Array(pairs.iter().map(|(k, v)| json!({ "key": k, "value": v, "type": "string" })).collect());
    match a {
        Auth::Inherit => Value::Null,
        Auth::None => json!({ "type": "noauth" }),
        Auth::Basic { user, pass } => json!({ "type": "basic", "basic": kv(&[("username", user), ("password", pass)]) }),
        Auth::Bearer { token, .. } => json!({ "type": "bearer", "bearer": kv(&[("token", token)]) }),
        Auth::Apikey { key, value, query } => json!({ "type": "apikey", "apikey": kv(&[("key", key), ("value", value), ("in", if *query { "query" } else { "header" })]) }),
        Auth::Digest { user, pass } => json!({ "type": "digest", "digest": kv(&[("username", user), ("password", pass)]) }),
        Auth::Aws { access_key, secret_key, session_token, region, service } => {
            json!({ "type": "awsv4", "awsv4": kv(&[("accessKey", access_key), ("secretKey", secret_key), ("sessionToken", session_token), ("region", region), ("service", service)]) })
        }
        Auth::Oauth2(o) => json!({ "type": "oauth2", "oauth2": kv(&[("grant_type", &o.grant), ("authUrl", &o.auth_url), ("accessTokenUrl", &o.token_url), ("clientId", &o.client_id), ("clientSecret", &o.client_secret), ("scope", &o.scope)]) }),
    }
}

fn rows_out(r: &[Row]) -> Value {
    Value::Array(r.iter().map(|x| json!({ "key": x.key, "value": x.value, "disabled": !x.on, "description": x.desc })).collect())
}

fn events_out(pre: &str, post: &str) -> Value {
    let mut e = Vec::new();
    for (listen, src) in [("prerequest", pre), ("test", post)] {
        if !src.trim().is_empty() {
            e.push(json!({ "listen": listen, "script": { "type": "text/javascript", "exec": src.lines().collect::<Vec<_>>() } }));
        }
    }
    Value::Array(e)
}

fn request_out(r: &Request) -> Value {
    let body = match &r.body {
        Body::None => Value::Null,
        Body::Json { text } => json!({ "mode": "raw", "raw": text, "options": { "raw": { "language": "json" } } }),
        Body::Xml { text } => json!({ "mode": "raw", "raw": text, "options": { "raw": { "language": "xml" } } }),
        Body::Html { text } => json!({ "mode": "raw", "raw": text, "options": { "raw": { "language": "html" } } }),
        Body::Text { text } => json!({ "mode": "raw", "raw": text, "options": { "raw": { "language": "text" } } }),
        Body::Form { rows } => json!({ "mode": "urlencoded", "urlencoded": rows_out(rows) }),
        Body::Multipart { rows } => json!({ "mode": "formdata", "formdata": rows.iter().map(|x| if x.flag { json!({ "key": x.key, "type": "file", "src": x.value, "disabled": !x.on }) } else { json!({ "key": x.key, "type": "text", "value": x.value, "disabled": !x.on }) }).collect::<Vec<_>>() }),
        Body::Binary { path } => json!({ "mode": "file", "file": { "src": path } }),
        Body::Graphql { query, variables } => json!({ "mode": "graphql", "graphql": { "query": query, "variables": variables } }),
    };
    let mut req = json!({
        "method": r.method,
        "header": rows_out(&r.headers),
        "url": { "raw": r.url, "query": rows_out(&super::send::query_rows(&r.url)), "variable": rows_out(&r.path_params) },
        "description": r.docs,
    });
    if !body.is_null() {
        req["body"] = body;
    }
    let a = postman_auth_out(&r.auth);
    if !a.is_null() {
        req["auth"] = a;
    }
    json!({ "name": r.name, "request": req, "event": events_out(&r.pre_script, &r.post_script) })
}

/// A collection as a Postman 2.1 file (its requests read from disk).
pub fn to_postman(c: &Collection) -> Value {
    fn folder(c: &Collection, f: &Folder) -> Vec<Value> {
        let mut out: Vec<Value> = f
            .folders
            .iter()
            .map(|sub| {
                let mut v = json!({ "name": sub.name, "item": folder(c, sub), "event": events_out(&sub.pre_script, &sub.post_script) });
                let a = postman_auth_out(&sub.auth);
                if !a.is_null() {
                    v["auth"] = a;
                }
                v
            })
            .collect();
        out.extend(f.requests.iter().filter_map(|it| super::load_request(c, &it.id)).map(|r| request_out(&r)));
        out
    }
    let mut v = json!({
        "info": { "_postman_id": c.id, "name": c.name(), "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
        "item": folder(c, &c.root),
        "variable": rows_out(&c.root.vars),
        "event": events_out(&c.root.pre_script, &c.root.post_script),
    });
    let a = postman_auth_out(&c.root.auth);
    if !a.is_null() {
        v["auth"] = a;
    }
    v
}

// ---------------------------------------------------------------- Insomnia v4

pub fn insomnia(v: &Value) -> Result<Imported> {
    let res = v.get("resources").and_then(Value::as_array).ok_or_else(|| anyhow!("an Insomnia export has resources"))?;
    let ws = res.iter().find(|r| s(r, "_type") == "workspace");
    let name = ws.map(|w| s(w, "name")).unwrap_or_else(|| "Insomnia".into());
    let mut imp = Imported { collection: Collection::new(&name, None), ..Default::default() };
    let root_id = imp.collection.root.id.clone();
    // Insomnia's ids → ours.
    let mut ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(w) = ws {
        ids.insert(s(w, "_id"), root_id.clone());
    }
    let ins_auth = |a: Option<&Value>| -> Auth {
        let Some(a) = a.filter(|a| a.is_object()) else { return Auth::Inherit };
        match s(a, "type").as_str() {
            "basic" => Auth::Basic { user: s(a, "username"), pass: s(a, "password") },
            "bearer" => Auth::Bearer { token: s(a, "token"), prefix: if s(a, "prefix").is_empty() { "Bearer".into() } else { s(a, "prefix") } },
            "digest" => Auth::Digest { user: s(a, "username"), pass: s(a, "password") },
            "apikey" => Auth::Apikey { key: s(a, "key"), value: s(a, "value"), query: s(a, "addTo") == "queryParams" },
            "none" => Auth::None,
            _ => Auth::Inherit,
        }
    };
    let ins_rows = |v: Option<&Value>| -> Vec<Row> {
        v.and_then(Value::as_array).map(|a| a.iter().map(|x| Row { on: !x.get("disabled").and_then(Value::as_bool).unwrap_or(false), key: s(x, "name"), value: s(x, "value"), desc: s(x, "description"), flag: s(x, "type") == "file" }).collect()).unwrap_or_default()
    };
    // Folders first (in order: a folder's parent may come after it).
    let groups: Vec<&Value> = res.iter().filter(|r| s(r, "_type") == "request_group").collect();
    let mut left = groups.clone();
    for _ in 0..groups.len() + 1 {
        left.retain(|g| {
            let Some(parent) = ids.get(&s(g, "parentId")).cloned() else { return true };
            let f = Folder { name: s(g, "name"), auth: ins_auth(g.get("authentication")), headers: ins_rows(g.get("headers")), ..Default::default() };
            ids.insert(s(g, "_id"), f.id.clone());
            imp.collection.root.find_mut(&parent).unwrap().folders.push(f);
            false
        });
    }
    for r in res.iter().filter(|r| matches!(s(r, "_type").as_str(), "request" | "websocket_request")) {
        let mut req = Request { name: s(r, "name"), method: s(r, "method").to_uppercase(), url: s(r, "url"), headers: ins_rows(r.get("headers")), auth: ins_auth(r.get("authentication")), docs: s(r, "description"), ..Default::default() };
        if s(r, "_type") == "websocket_request" {
            req.kind = Kind::Websocket;
        }
        if req.method.is_empty() {
            req.method = "GET".into();
        }
        let q = ins_rows(r.get("parameters"));
        if !q.is_empty() {
            req.url = super::send::with_query(&req.url, &q);
        }
        req.params = super::send::query_rows(&req.url);
        if let Some(b) = r.get("body").filter(|b| b.is_object()) {
            let mime = s(b, "mimeType");
            let text = s(b, "text");
            req.body = match mime.as_str() {
                "application/json" => Body::Json { text },
                "application/xml" | "text/xml" => Body::Xml { text },
                "application/x-www-form-urlencoded" => Body::Form { rows: ins_rows(b.get("params")) },
                "multipart/form-data" => Body::Multipart { rows: b.get("params").and_then(Value::as_array).map(|a| a.iter().map(|x| Row { on: !x.get("disabled").and_then(Value::as_bool).unwrap_or(false), key: s(x, "name"), value: if s(x, "type") == "file" { s(x, "fileName") } else { s(x, "value") }, desc: String::new(), flag: s(x, "type") == "file" }).collect()).unwrap_or_default() },
                "application/graphql" => {
                    let g: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    Body::Graphql { query: s(&g, "query"), variables: g.get("variables").map(|v| serde_json::to_string_pretty(v).unwrap_or_default()).unwrap_or_default() }
                }
                "application/octet-stream" => Body::Binary { path: s(b, "fileName") },
                "" if text.is_empty() => Body::None,
                _ => Body::Text { text },
            };
        }
        let folder = ids.get(&s(r, "parentId")).cloned();
        imp.item(folder.as_deref(), req);
    }
    for e in res.iter().filter(|r| s(r, "_type") == "environment") {
        let vars: Vec<Row> = e.get("data").and_then(Value::as_object).map(|o| o.iter().map(|(k, v)| Row::new(k, &v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect()).unwrap_or_default();
        if vars.is_empty() {
            continue;
        }
        imp.environments.push(Environment { id: new_id(), name: s(e, "name"), vars, ..Default::default() });
    }
    Ok(imp)
}

// ---------------------------------------------------------------- OpenAPI 3 (and Swagger 2)

fn example_of(schema: &Value, root: &Value, depth: usize) -> Value {
    if depth > 6 {
        return Value::Null;
    }
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        let target = r.trim_start_matches('#').split('/').filter(|p| !p.is_empty()).fold(Some(root), |v, k| v.and_then(|v| v.get(k)));
        return target.map(|t| example_of(t, root, depth + 1)).unwrap_or(Value::Null);
    }
    if let Some(e) = schema.get("example") {
        return e.clone();
    }
    if let Some(e) = schema.get("default") {
        return e.clone();
    }
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        let mut out = serde_json::Map::new();
        for s in all {
            if let Value::Object(o) = example_of(s, root, depth + 1) {
                out.extend(o);
            }
        }
        return Value::Object(out);
    }
    if let Some(one) = schema.get("oneOf").or(schema.get("anyOf")).and_then(Value::as_array).and_then(|a| a.first()) {
        return example_of(one, root, depth + 1);
    }
    if let Some(e) = schema.get("enum").and_then(Value::as_array).and_then(|a| a.first()) {
        return e.clone();
    }
    match schema.get("type").and_then(Value::as_str).unwrap_or(if schema.get("properties").is_some() { "object" } else { "" }) {
        "object" => Value::Object(schema.get("properties").and_then(Value::as_object).map(|p| p.iter().map(|(k, v)| (k.clone(), example_of(v, root, depth + 1))).collect()).unwrap_or_default()),
        "array" => Value::Array(schema.get("items").map(|i| vec![example_of(i, root, depth + 1)]).unwrap_or_default()),
        "integer" | "number" => json!(0),
        "boolean" => json!(false),
        "string" => json!(match schema.get("format").and_then(Value::as_str) {
            Some("date-time") => "2026-01-01T00:00:00Z",
            Some("date") => "2026-01-01",
            Some("email") => "user@example.com",
            Some("uuid") => "00000000-0000-0000-0000-000000000000",
            _ => "string",
        }),
        _ => Value::Null,
    }
}

pub fn openapi(v: &Value) -> Result<Imported> {
    let info = v.get("info").cloned().unwrap_or(Value::Null);
    let title = s(&info, "title");
    let mut imp = Imported { collection: Collection::new(if title.is_empty() { "OpenAPI" } else { &title }, None), ..Default::default() };
    let base = if let Some(sv) = v.get("servers").and_then(Value::as_array).and_then(|a| a.first()) {
        let mut u = s(sv, "url");
        if let Some(vars) = sv.get("variables").and_then(Value::as_object) {
            for (k, d) in vars {
                u = u.replace(&format!("{{{k}}}"), &s(d, "default"));
            }
        }
        u
    } else if v.get("swagger").is_some() {
        let scheme = v.get("schemes").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str).unwrap_or("https");
        format!("{scheme}://{}{}", s(v, "host"), s(v, "basePath"))
    } else {
        String::new()
    };
    imp.collection.root.vars.push(Row::new("baseUrl", base.trim_end_matches('/')));
    // Security: the first scheme, as the collection's auth.
    let schemes = v.get("components").and_then(|c| c.get("securitySchemes")).or(v.get("securityDefinitions")).and_then(Value::as_object);
    if let Some((_, sc)) = schemes.and_then(|m| m.iter().next()) {
        imp.collection.root.auth = match (s(sc, "type").as_str(), s(sc, "scheme").to_lowercase().as_str()) {
            ("http", "basic") | ("basic", _) => Auth::Basic { user: String::new(), pass: String::new() },
            ("http", "bearer") => Auth::Bearer { token: "{{token}}".into(), prefix: "Bearer".into() },
            ("apiKey", _) => Auth::Apikey { key: s(sc, "name"), value: "{{apiKey}}".into(), query: s(sc, "in") == "query" },
            ("oauth2", _) => {
                let flows = sc.get("flows").cloned().unwrap_or(Value::Null);
                let (grant, f) = if let Some(f) = flows.get("clientCredentials") { ("client_credentials", f.clone()) } else if let Some(f) = flows.get("authorizationCode") { ("authorization_code", f.clone()) } else if let Some(f) = flows.get("password") { ("password", f.clone()) } else { ("client_credentials", sc.clone()) };
                Auth::Oauth2(super::OAuth2 { grant: grant.into(), auth_url: s(&f, "authorizationUrl"), token_url: s(&f, "tokenUrl"), pkce: true, prefix: "Bearer".into(), redirect_uri: "http://127.0.0.1:53682/callback".into(), ..Default::default() })
            }
            _ => Auth::None,
        };
    }
    let mut folders: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let root_id = imp.collection.root.id.clone();
    let Some(paths) = v.get("paths").and_then(Value::as_object) else { return Ok(imp) };
    for (path, item) in paths {
        let shared = item.get("parameters").and_then(Value::as_array).cloned().unwrap_or_default();
        for m in ["get", "post", "put", "patch", "delete", "head", "options", "trace"] {
            let Some(op) = item.get(m) else { continue };
            let tag = op.get("tags").and_then(Value::as_array).and_then(|t| t.first()).and_then(Value::as_str).map(str::to_string);
            let folder = match tag {
                Some(t) => folders
                    .entry(t.clone())
                    .or_insert_with(|| {
                        let f = Folder { name: t.clone(), auth: Auth::Inherit, ..Default::default() };
                        let id = f.id.clone();
                        imp.collection.root.folders.push(f);
                        id
                    })
                    .clone(),
                None => root_id.clone(),
            };
            let summary = s(op, "summary");
            let mut req = Request {
                name: if summary.is_empty() { format!("{} {path}", m.to_uppercase()) } else { summary },
                method: m.to_uppercase(),
                url: format!("{{{{baseUrl}}}}{}", path),
                docs: s(op, "description"),
                ..Default::default()
            };
            let params: Vec<Value> = shared.iter().chain(op.get("parameters").and_then(Value::as_array).into_iter().flatten()).cloned().collect();
            let mut query = Vec::new();
            for p in &params {
                let p = if let Some(r) = p.get("$ref").and_then(Value::as_str) { r.trim_start_matches('#').split('/').filter(|x| !x.is_empty()).fold(Some(v), |v, k| v.and_then(|v| v.get(k))).cloned().unwrap_or(Value::Null) } else { p.clone() };
                let name = s(&p, "name");
                let ex = p.get("example").or(p.get("schema").and_then(|s| s.get("example").or(s.get("default")))).map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())).unwrap_or_default();
                let row = Row { on: p.get("required").and_then(Value::as_bool).unwrap_or(false), desc: s(&p, "description"), ..Row::new(&name, &ex) };
                match s(&p, "in").as_str() {
                    "path" => req.path_params.push(Row { on: true, ..row }),
                    "query" => query.push(row),
                    "header" => req.headers.push(row),
                    _ => {}
                }
            }
            req.params = query.clone();
            if query.iter().any(|q| q.on) {
                req.url = super::send::with_query(&req.url, &query);
            }
            if let Some(content) = op.get("requestBody").and_then(|b| b.get("content")).and_then(Value::as_object) {
                if let Some(j) = content.iter().find(|(k, _)| k.contains("json")).map(|(_, v)| v) {
                    let ex = j.get("example").cloned().unwrap_or_else(|| j.get("schema").map(|sc| example_of(sc, v, 0)).unwrap_or(Value::Null));
                    req.body = Body::Json { text: serde_json::to_string_pretty(&ex).unwrap_or_default() };
                } else if let Some(f) = content.get("application/x-www-form-urlencoded").or(content.get("multipart/form-data")) {
                    let rows: Vec<Row> = f.get("schema").and_then(|sc| sc.get("properties")).and_then(Value::as_object).map(|p| p.keys().map(|k| Row::new(k, "")).collect()).unwrap_or_default();
                    req.body = if content.contains_key("multipart/form-data") { Body::Multipart { rows } } else { Body::Form { rows } };
                } else if content.keys().any(|k| k.contains("xml")) {
                    req.body = Body::Xml { text: String::new() };
                }
            }
            req.auth = Auth::Inherit;
            imp.item(Some(&folder), req);
        }
    }
    Ok(imp)
}

/// A collection as an OpenAPI 3 file (paths from its requests).
pub fn to_openapi(c: &Collection) -> Value {
    let mut paths = serde_json::Map::new();
    let base = c.root.vars.iter().find(|v| v.key == "baseUrl").map(|v| v.value.clone());
    fn walk(c: &Collection, f: &Folder, tag: Option<&str>, out: &mut Vec<(Option<String>, Request)>) {
        for it in &f.requests {
            if let Some(r) = super::load_request(c, &it.id) {
                out.push((tag.map(str::to_string), r));
            }
        }
        for sub in &f.folders {
            walk(c, sub, Some(&sub.name), out);
        }
    }
    let mut all = Vec::new();
    walk(c, &c.root, None, &mut all);
    for (tag, r) in all.into_iter().filter(|(_, r)| r.kind == Kind::Http) {
        let url = r.url.split(['?', '#']).next().unwrap_or("").to_string();
        let path = url.replace("{{baseUrl}}", "");
        let path = match path.split_once("://") {
            Some((_, rest)) => rest.find('/').map(|i| rest[i..].to_string()).unwrap_or_else(|| "/".into()),
            None => path.trim_start_matches(|c| c != '/').to_string(),
        };
        // :id → {id}
        let path: String = path.split('/').map(|seg| seg.strip_prefix(':').map(|n| format!("{{{n}}}")).unwrap_or_else(|| seg.to_string())).collect::<Vec<_>>().join("/");
        let path = if path.is_empty() { "/".to_string() } else { path };
        let mut params: Vec<Value> = super::send::path_param_names(&path).iter().map(|n| json!({ "name": n, "in": "path", "required": true, "schema": { "type": "string" } })).collect();
        params.extend(super::send::query_rows(&r.url).iter().map(|q| json!({ "name": q.key, "in": "query", "schema": { "type": "string" }, "example": q.value })));
        params.extend(r.headers.iter().filter(|h| h.on && !h.key.eq_ignore_ascii_case("content-type")).map(|h| json!({ "name": h.key, "in": "header", "schema": { "type": "string" } })));
        let mut op = json!({ "summary": r.name, "parameters": params, "responses": { "200": { "description": "OK" } } });
        if !r.docs.is_empty() {
            op["description"] = json!(r.docs);
        }
        if let Some(t) = tag {
            op["tags"] = json!([t]);
        }
        match &r.body {
            Body::Json { text } => {
                let ex: Value = serde_json::from_str(text).unwrap_or(Value::String(text.clone()));
                op["requestBody"] = json!({ "content": { "application/json": { "example": ex } } });
            }
            Body::Form { rows } => op["requestBody"] = json!({ "content": { "application/x-www-form-urlencoded": { "schema": { "type": "object", "properties": rows.iter().map(|x| (x.key.clone(), json!({ "type": "string" }))).collect::<serde_json::Map<_, _>>() } } } }),
            Body::Multipart { rows } => op["requestBody"] = json!({ "content": { "multipart/form-data": { "schema": { "type": "object", "properties": rows.iter().map(|x| (x.key.clone(), if x.flag { json!({ "type": "string", "format": "binary" }) } else { json!({ "type": "string" }) })).collect::<serde_json::Map<_, _>>() } } } }),
            Body::Xml { .. } => op["requestBody"] = json!({ "content": { "application/xml": {} } }),
            Body::Text { .. } => op["requestBody"] = json!({ "content": { "text/plain": {} } }),
            _ => {}
        }
        let entry = paths.entry(path).or_insert_with(|| json!({}));
        entry[r.method.to_lowercase()] = op;
    }
    let mut v = json!({ "openapi": "3.0.3", "info": { "title": c.name(), "version": "1.0.0" }, "paths": paths });
    if let Some(b) = base {
        v["servers"] = json!([{ "url": b }]);
    }
    v
}

// ---------------------------------------------------------------- Bruno

/// A Bruno collection's folder (`.bru` files, folders as folders).
pub fn bruno(dir: &std::path::Path) -> Result<Imported> {
    let name = std::fs::read_to_string(dir.join("bruno.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| s(&v, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Bruno".into()));
    let mut imp = Imported { collection: Collection::new(&name, None), ..Default::default() };
    let root = imp.collection.root.id.clone();
    fn walk(imp: &mut Imported, dir: &std::path::Path, folder: &str) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let fname = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if p.is_dir() {
                if fname == "environments" {
                    for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                        if let Ok(text) = std::fs::read_to_string(e.path()) {
                            let blocks = bru_blocks(&text);
                            let vars = blocks.iter().find(|(n, _)| n == "vars").map(|(_, b)| bru_rows(b)).unwrap_or_default();
                            imp.environments.push(Environment { id: new_id(), name: e.path().file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), vars, ..Default::default() });
                        }
                    }
                    continue;
                }
                if fname.starts_with('.') || fname == "node_modules" {
                    continue;
                }
                let f = Folder { name: fname.clone(), auth: Auth::Inherit, ..Default::default() };
                let id = f.id.clone();
                imp.collection.root.find_mut(folder).unwrap().folders.push(f);
                walk(imp, &p, &id);
            } else if fname.ends_with(".bru") && fname != "folder.bru" && fname != "collection.bru" {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    imp.item(Some(folder), bru_request(&text, &fname));
                }
            }
        }
    }
    walk(&mut imp, dir, &root);
    Ok(imp)
}

/// `name { … }` blocks of a .bru file.
fn bru_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        let Some(head) = l.trim_end().strip_suffix('{') else { continue };
        if l.starts_with(' ') {
            continue;
        }
        let mut body = String::new();
        for b in lines.by_ref() {
            if b.trim_end() == "}" && !b.starts_with(' ') {
                break;
            }
            body.push_str(b.strip_prefix("  ").unwrap_or(b));
            body.push('\n');
        }
        out.push((head.trim().to_string(), body));
    }
    out
}

fn bru_rows(b: &str) -> Vec<Row> {
    b.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (k, v) = l.split_once(':')?;
            let (on, k) = match k.strip_prefix('~') {
                Some(k) => (false, k),
                None => (true, k),
            };
            Some(Row { on, ..Row::new(k.trim(), v.trim()) })
        })
        .collect()
}

fn bru_request(text: &str, fname: &str) -> Request {
    let blocks = bru_blocks(text);
    let get = |n: &str| blocks.iter().find(|(k, _)| k == n).map(|(_, b)| b.clone());
    let meta = get("meta").map(|m| bru_rows(&m)).unwrap_or_default();
    let mut req = Request { name: meta.iter().find(|r| r.key == "name").map(|r| r.value.clone()).unwrap_or_else(|| fname.trim_end_matches(".bru").to_string()), ..Default::default() };
    for m in ["get", "post", "put", "patch", "delete", "head", "options"] {
        if let Some(b) = get(m) {
            req.method = m.to_uppercase();
            let rows = bru_rows(&b);
            req.url = rows.iter().find(|r| r.key == "url").map(|r| r.value.clone()).unwrap_or_default();
            let body = rows.iter().find(|r| r.key == "body").map(|r| r.value.clone()).unwrap_or_default();
            let auth = rows.iter().find(|r| r.key == "auth").map(|r| r.value.clone()).unwrap_or_default();
            req.body = match body.as_str() {
                "json" => Body::Json { text: get("body:json").unwrap_or_default().trim_end().to_string() },
                "xml" => Body::Xml { text: get("body:xml").unwrap_or_default().trim_end().to_string() },
                "text" => Body::Text { text: get("body:text").unwrap_or_default().trim_end().to_string() },
                "formUrlEncoded" => Body::Form { rows: get("body:form-urlencoded").map(|b| bru_rows(&b)).unwrap_or_default() },
                "multipartForm" => Body::Multipart { rows: get("body:multipart-form").map(|b| bru_rows(&b)).unwrap_or_default() },
                "graphql" => Body::Graphql { query: get("body:graphql").unwrap_or_default(), variables: get("body:graphql:vars").unwrap_or_default() },
                _ => Body::None,
            };
            let ar = |n: &str| get(n).map(|b| bru_rows(&b)).unwrap_or_default();
            let val = |rows: &[Row], k: &str| rows.iter().find(|r| r.key == k).map(|r| r.value.clone()).unwrap_or_default();
            req.auth = match auth.as_str() {
                "bearer" => Auth::Bearer { token: val(&ar("auth:bearer"), "token"), prefix: "Bearer".into() },
                "basic" => {
                    let r = ar("auth:basic");
                    Auth::Basic { user: val(&r, "username"), pass: val(&r, "password") }
                }
                "none" => Auth::None,
                _ => Auth::Inherit,
            };
        }
    }
    req.headers = get("headers").map(|b| bru_rows(&b)).unwrap_or_default();
    req.params = get("params:query").or(get("query")).map(|b| bru_rows(&b)).unwrap_or_else(|| super::send::query_rows(&req.url));
    req.path_params = get("params:path").map(|b| bru_rows(&b)).unwrap_or_default();
    req.pre_script = get("script:pre-request").unwrap_or_default().trim_end().replace("bru.", "tc.").replace("req.", "tc.request.");
    req.post_script = get("tests").or(get("script:post-response")).unwrap_or_default().trim_end().replace("bru.", "tc.").replace("res.", "tc.response.");
    req.docs = get("docs").unwrap_or_default();
    req
}

/// A .env file as an environment.
pub fn dotenv_environment(path: &std::path::Path) -> Environment {
    let mut vars: Vec<(String, String)> = super::vars::dotenv(&path.to_string_lossy()).into_iter().collect();
    vars.sort();
    Environment { id: new_id(), name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| ".env".into()), vars: vars.iter().map(|(k, v)| Row::new(k, v)).collect(), ..Default::default() }
}

#[cfg(test)]
mod formats {
    use super::*;

    #[test]
    fn curl_commands_become_requests() {
        let r = curl(r#"curl -X POST 'https://api.x.io/v1/orders?limit=5' \
  -H 'Content-Type: application/json' -H "Authorization: Bearer abc.def" \
  --data-raw '{"item":"pen","qty":2}' -k --compressed"#)
        .unwrap();
        assert_eq!((r.method.as_str(), r.url.as_str()), ("POST", "https://api.x.io/v1/orders?limit=5"));
        assert_eq!(r.auth, Auth::Bearer { token: "abc.def".into(), prefix: "Bearer".into() });
        assert!(matches!(&r.body, Body::Json { text } if text.contains("\"qty\": 2")));
        assert!(!r.settings.verify_tls);
        assert_eq!(r.params, [Row::new("limit", "5")]);
        let f = curl("curl https://x.io/up -F name=kite -F file=@/tmp/a.png -u ada:pw").unwrap();
        assert_eq!(f.method, "POST");
        assert!(matches!(&f.body, Body::Multipart { rows } if rows.len() == 2 && rows[1].flag && rows[1].value == "/tmp/a.png"));
        assert_eq!(f.auth, Auth::Basic { user: "ada".into(), pass: "pw".into() });
        let g = curl("curl -G https://x.io/s -d q=rust -d page=2").unwrap();
        assert_eq!((g.method.as_str(), g.url.as_str()), ("GET", "https://x.io/s?q=rust&page=2"));
    }

    #[test]
    fn requests_become_curl_commands_and_back() {
        let ready = super::super::send::Ready {
            method: "PUT".into(),
            url: "https://x.io/a b".into(),
            headers: vec![("X-K".into(), "it's".into())],
            body: Body::Json { text: "{\"a\":1}".into() },
            auth: Auth::Basic { user: "u".into(), pass: "p".into() },
            settings: Default::default(),
            missing: vec![],
        };
        let c = to_curl(&ready);
        let back = curl(&c).unwrap();
        assert_eq!((back.method.as_str(), back.url.as_str()), ("PUT", "https://x.io/a b"));
        assert_eq!(back.headers[0], Row::new("X-K", "it's"));
        assert_eq!(back.auth, Auth::Basic { user: "u".into(), pass: "p".into() });
    }

    #[test]
    fn postman_collections_come_in_with_folders_auth_and_scripts() {
        let v: Value = serde_json::from_str(r#"{
          "info": {"name": "Shop", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
          "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}"}]},
          "variable": [{"key": "base", "value": "https://shop.io"}],
          "item": [
            {"name": "Orders", "item": [
              {"name": "Create", "request": {"method": "POST", "url": {"raw": "{{base}}/orders/:id?x=1", "variable": [{"key": "id", "value": "7"}]},
                "header": [{"key": "X-A", "value": "1", "disabled": true}],
                "body": {"mode": "raw", "raw": "{\"a\":1}", "options": {"raw": {"language": "json"}}}},
               "event": [{"listen": "test", "script": {"exec": ["pm.test('ok', () => {});"]}}]}
            ]},
            {"name": "Ping", "request": "https://shop.io/ping"}
          ]}"#).unwrap();
        let imp = import_text(&v.to_string(), "").unwrap();
        assert_eq!(imp.collection.name(), "Shop");
        assert_eq!(imp.collection.root.auth, Auth::Bearer { token: "{{token}}".into(), prefix: "Bearer".into() });
        assert_eq!(imp.collection.root.folders[0].requests[0].method, "POST");
        let create = &imp.requests[0];
        assert_eq!(create.path_params, [Row::new("id", "7")]);
        assert!(!create.headers[0].on);
        assert_eq!(create.post_script, "pm.test('ok', () => {});");
        assert!(matches!(create.body, Body::Json { .. }));
        assert_eq!(imp.requests[1].url, "https://shop.io/ping");
    }

    #[test]
    fn openapi_yaml_becomes_a_collection_by_tag() {
        let y = r##"
openapi: 3.0.0
info: {title: Pets}
servers: [{url: "https://pets.io/v1"}]
components:
  securitySchemes:
    key: {type: apiKey, in: header, name: X-Key}
  schemas:
    Pet: {type: object, properties: {name: {type: string}, age: {type: integer}}}
paths:
  /pets/{petId}:
    get:
      tags: [pets]
      summary: Get a pet
      parameters: [{name: petId, in: path, required: true}]
  /pets:
    post:
      tags: [pets]
      requestBody: {content: {application/json: {schema: {$ref: "#/components/schemas/Pet"}}}}
"##;
        let imp = import_text(y, "").unwrap();
        assert_eq!(imp.collection.name(), "Pets");
        assert_eq!(imp.collection.root.vars[0], Row::new("baseUrl", "https://pets.io/v1"));
        assert_eq!(imp.collection.root.auth, Auth::Apikey { key: "X-Key".into(), value: "{{apiKey}}".into(), query: false });
        assert_eq!(imp.collection.root.folders[0].name, "pets");
        assert_eq!(imp.requests[0].url, "{{baseUrl}}/pets/{petId}");
        assert_eq!(imp.requests[0].path_params[0].key, "petId");
        assert!(matches!(&imp.requests[1].body, Body::Json { text } if text.contains("\"age\": 0")));
    }

    #[test]
    fn insomnia_exports_come_in() {
        let v = json!({ "_type": "export", "resources": [
            { "_id": "wrk_1", "_type": "workspace", "name": "Ins" },
            { "_id": "fld_1", "_type": "request_group", "parentId": "wrk_1", "name": "Users" },
            { "_id": "req_1", "_type": "request", "parentId": "fld_1", "name": "Me", "method": "GET", "url": "{{ base }}/me", "headers": [{ "name": "Accept", "value": "application/json" }], "authentication": { "type": "bearer", "token": "t" } },
            { "_id": "env_1", "_type": "environment", "name": "Base", "data": { "base": "https://ins.io" } }
        ]});
        let imp = import_text(&v.to_string(), "").unwrap();
        assert_eq!(imp.collection.root.folders[0].requests[0].name, "Me");
        assert_eq!(imp.requests[0].auth, Auth::Bearer { token: "t".into(), prefix: "Bearer".into() });
        assert_eq!(imp.environments[0].vars, [Row::new("base", "https://ins.io")]);
    }

    #[test]
    fn bruno_files_come_in() {
        let r = bru_request("meta {\n  name: Login\n  type: http\n}\n\npost {\n  url: {{base}}/login\n  body: json\n  auth: none\n}\n\nheaders {\n  X-A: 1\n  ~X-B: 2\n}\n\nbody:json {\n  {\n    \"u\": \"ada\"\n  }\n}\n", "login.bru");
        assert_eq!((r.name.as_str(), r.method.as_str(), r.url.as_str()), ("Login", "POST", "{{base}}/login"));
        assert_eq!(r.headers, [Row::new("X-A", "1"), Row { on: false, ..Row::new("X-B", "2") }]);
        assert!(matches!(&r.body, Body::Json { text } if text.contains("\"u\": \"ada\"")), "{:?}", r.body);
        assert_eq!(r.auth, Auth::None);
    }
}
