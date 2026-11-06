//! Sending a request: variables filled in, auth applied (inherited from folders), cookies,
//! TLS and proxy settings, redirects. The response's first `MAX_IN_MEMORY` bytes are kept to
//! show; a bigger body goes whole to a temporary file.

use super::vars::Vars;
use super::{Auth, Body, Folder, OAuth2, Request, Row, Settings};
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// The most of a response's body kept in memory (to show); the rest is on disk.
pub const MAX_IN_MEMORY: usize = 2 * 1024 * 1024;

/// A request with its variables filled in and inherited settings applied: what's sent.
#[derive(Clone, Debug, Default)]
pub struct Ready {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    pub auth: Auth,
    pub settings: Settings,
    /// Variables nobody knows.
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    /// The first MAX_IN_MEMORY bytes (all of it, for most).
    pub body: Vec<u8>,
    /// Bigger: the whole body is here.
    pub body_file: Option<PathBuf>,
    pub size: u64,
    pub ms: u64,
    /// Until the headers came.
    pub ttfb_ms: u64,
    /// What was sent, after variables and auth (the timeline).
    pub sent: Sent,
    pub redirects: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Sent {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> String {
        self.header("content-type").unwrap_or("").to_lowercase()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// `:id` and `{id}` parts of a URL's path.
pub fn path_param_names(url: &str) -> Vec<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or("")).unwrap_or(path);
    let mut out = Vec::new();
    for seg in path.split('/') {
        let name = if let Some(n) = seg.strip_prefix(':') {
            n
        } else if let Some(n) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            n
        } else {
            continue;
        };
        if !n_ok(name) || out.iter().any(|o| o == name) {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

fn n_ok(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// The query string's parameters, as rows (the URL bar and the Params table follow each other).
pub fn query_rows(url: &str) -> Vec<Row> {
    let Some((_, q)) = url.split_once('?') else { return Vec::new() };
    let q = q.split('#').next().unwrap_or(q);
    q.split('&').filter(|p| !p.is_empty()).map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        Row::new(&decode(k), &decode(v))
    }).collect()
}

/// The URL with its query string rebuilt from rows (on ones only).
pub fn with_query(url: &str, rows: &[Row]) -> String {
    let (base, frag) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let base = base.split('?').next().unwrap_or(base);
    let q: Vec<String> = rows.iter().filter(|r| r.on && !r.key.is_empty()).map(|r| if r.value.is_empty() { r.key.clone() } else { format!("{}={}", r.key, r.value) }).collect();
    let mut out = base.to_string();
    if !q.is_empty() {
        out.push('?');
        out.push_str(&q.join("&"));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                Ok(x) => {
                    out.push(x);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encodes what a URL can't hold as is (spaces, quotes…), keeping what's already
/// encoded and its structure (/?&=#:).
pub fn encode_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.' | '_' | '~' | ':' | '/' | '?' | '#' | '[' | ']' | '@' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '%' => out.push(c),
            c => {
                let mut b = [0u8; 4];
                for x in c.encode_utf8(&mut b).bytes() {
                    out.push_str(&format!("%{x:02X}"));
                }
            }
        }
    }
    out
}

pub fn encode_component(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Fills in a request (and what its folders give it, innermost first) for sending.
pub fn prepare(req: &Request, folders_inner_first: &[&Folder], vars: &Vars) -> Ready {
    let mut missing = Vec::new();
    let r = |s: &str, m: &mut Vec<String>| vars.resolve(s, m);
    let mut url = r(req.url.trim(), &mut missing);
    // Path parameters.
    for p in req.path_params.iter().filter(|p| p.on && !p.key.is_empty()) {
        let v = encode_component(&r(&p.value, &mut missing));
        url = url.replace(&format!("/:{}", p.key), &format!("/{v}")).replace(&format!("{{{}}}", p.key), &v);
    }
    if !url.contains("://") && !url.is_empty() {
        url = format!("http://{url}");
    }
    if req.settings.encode_url {
        url = encode_url(&url);
    }
    // Headers: the outermost folder's first; nearer ones (and the request's) win.
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut add = |rows: &[Row], missing: &mut Vec<String>| {
        for h in rows.iter().filter(|h| h.on && !h.key.trim().is_empty()) {
            let k = r(h.key.trim(), missing);
            let v = r(&h.value, missing);
            headers.retain(|(hk, _)| !hk.eq_ignore_ascii_case(&k));
            headers.push((k, v));
        }
    };
    for f in folders_inner_first.iter().rev() {
        add(&f.headers, &mut missing);
    }
    add(&req.headers, &mut missing);
    // Auth: the request's, else the nearest folder's that says something.
    let mut auth = req.auth.clone();
    if auth == Auth::Inherit {
        auth = folders_inner_first.iter().map(|f| f.auth.clone()).find(|a| *a != Auth::Inherit).unwrap_or(Auth::None);
    }
    let auth = resolve_auth(&auth, vars, &mut missing);
    let body = match &req.body {
        Body::Json { text } => Body::Json { text: r(text, &mut missing) },
        Body::Xml { text } => Body::Xml { text: r(text, &mut missing) },
        Body::Text { text } => Body::Text { text: r(text, &mut missing) },
        Body::Html { text } => Body::Html { text: r(text, &mut missing) },
        Body::Form { rows } => Body::Form { rows: rows.iter().filter(|x| x.on).map(|x| Row { key: r(&x.key, &mut missing), value: r(&x.value, &mut missing), ..x.clone() }).collect() },
        Body::Multipart { rows } => Body::Multipart { rows: rows.iter().filter(|x| x.on).map(|x| Row { key: r(&x.key, &mut missing), value: r(&x.value, &mut missing), ..x.clone() }).collect() },
        Body::Binary { path } => Body::Binary { path: r(path, &mut missing) },
        Body::Graphql { query, variables } => Body::Graphql { query: r(query, &mut missing), variables: r(variables, &mut missing) },
        Body::None => Body::None,
    };
    let mut settings = req.settings.clone();
    settings.proxy = r(&settings.proxy, &mut missing);
    Ready { method: req.method.trim().to_uppercase(), url, headers, body, auth, settings, missing }
}

fn resolve_auth(a: &Auth, vars: &Vars, missing: &mut Vec<String>) -> Auth {
    let mut r = |s: &str| vars.resolve(s, missing);
    match a {
        Auth::Basic { user, pass } => Auth::Basic { user: r(user), pass: r(pass) },
        Auth::Bearer { token, prefix } => Auth::Bearer { token: r(token), prefix: r(prefix) },
        Auth::Apikey { key, value, query } => Auth::Apikey { key: r(key), value: r(value), query: *query },
        Auth::Digest { user, pass } => Auth::Digest { user: r(user), pass: r(pass) },
        Auth::Aws { access_key, secret_key, session_token, region, service } => {
            Auth::Aws { access_key: r(access_key), secret_key: r(secret_key), session_token: r(session_token), region: r(region), service: r(service) }
        }
        Auth::Oauth2(o) => Auth::Oauth2(OAuth2 {
            auth_url: r(&o.auth_url),
            token_url: r(&o.token_url),
            client_id: r(&o.client_id),
            client_secret: r(&o.client_secret),
            scope: r(&o.scope),
            audience: r(&o.audience),
            username: r(&o.username),
            password: r(&o.password),
            redirect_uri: r(&o.redirect_uri),
            prefix: r(&o.prefix),
            ..o.clone()
        }),
        other => other.clone(),
    }
}

fn client(s: &Settings) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .redirect(if s.follow_redirects { reqwest::redirect::Policy::limited(s.max_redirects.max(1) as usize) } else { reqwest::redirect::Policy::none() })
        .danger_accept_invalid_certs(!s.verify_tls)
        .danger_accept_invalid_hostnames(!s.verify_tls)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .zstd(true)
        .connect_timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(0);
    if s.timeout_ms > 0 {
        b = b.timeout(Duration::from_millis(s.timeout_ms));
    }
    match s.http.as_str() {
        "1.1" => b = b.http1_only(),
        "2" => b = b.http2_prior_knowledge(),
        _ => {}
    }
    match s.proxy.trim() {
        "" => {}
        "none" => b = b.no_proxy(),
        p => {
            let mut proxy = reqwest::Proxy::all(p).map_err(|e| anyhow!("proxy: {e}"))?;
            if !s.no_proxy.trim().is_empty() {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&s.no_proxy));
            }
            b = b.proxy(proxy);
        }
    }
    if !s.client_cert.trim().is_empty() {
        let der = std::fs::read(s.client_cert.trim()).map_err(|e| anyhow!("client certificate {}: {e}", s.client_cert))?;
        b = b.identity(reqwest::Identity::from_pkcs12_der(&der, &s.client_cert_pass).map_err(|e| anyhow!("client certificate: {e}"))?);
    }
    if !s.ca_cert.trim().is_empty() {
        let pem = std::fs::read(s.ca_cert.trim()).map_err(|e| anyhow!("CA certificate {}: {e}", s.ca_cert))?;
        for c in reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| anyhow!("CA certificate: {e}"))? {
            b = b.add_root_certificate(c);
        }
    }
    Ok(b.build()?)
}

/// Sends it; `cancel` stops it from outside (dropping the work).
pub async fn send(ready: Ready) -> Result<Response> {
    let client = client(&ready.settings)?;
    let mut ready = ready;
    // Tokens first (OAuth 2.0 gets or refreshes one).
    if let Auth::Oauth2(o) = &ready.auth {
        let token = oauth_token(o, &client).await?;
        let prefix = if o.prefix.is_empty() { "Bearer" } else { o.prefix.as_str() };
        set_header(&mut ready.headers, "Authorization", &format!("{prefix} {token}"));
    }
    let first = once(&client, &ready, None).await?;
    // Digest: the server says how, then the request goes again with the answer.
    if first.status == 401
        && let Auth::Digest { user, pass } = &ready.auth
        && let Some(challenge) = first.header("www-authenticate").filter(|h| h.to_lowercase().starts_with("digest"))
    {
        let header = digest(challenge, user, pass, &ready.method, &ready.url)?;
        return once(&client, &ready, Some(header)).await;
    }
    Ok(first)
}

fn set_header(h: &mut Vec<(String, String)>, k: &str, v: &str) {
    h.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
    h.push((k.to_string(), v.to_string()));
}

async fn once(client: &reqwest::Client, ready: &Ready, digest_header: Option<String>) -> Result<Response> {
    let method = reqwest::Method::from_bytes(ready.method.as_bytes()).map_err(|_| anyhow!("{} isn't a method", ready.method))?;
    let mut url = reqwest::Url::parse(&ready.url).map_err(|e| anyhow!("the URL {}: {e}", ready.url))?;
    let mut headers = ready.headers.clone();
    match &ready.auth {
        Auth::Basic { user, pass } => set_header(&mut headers, "Authorization", &format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()))),
        Auth::Bearer { token, prefix } => set_header(&mut headers, "Authorization", &format!("{} {token}", if prefix.is_empty() { "Bearer" } else { prefix })),
        Auth::Apikey { key, value, query } if !key.is_empty() => {
            if *query {
                url.query_pairs_mut().append_pair(key, value);
            } else {
                set_header(&mut headers, key, value);
            }
        }
        _ => {}
    }
    if let Some(d) = digest_header {
        set_header(&mut headers, "Authorization", &d);
    }
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        headers.push(("User-Agent".into(), "OSKL Code".into()));
    }
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
        headers.push(("Accept".into(), "*/*".into()));
    }
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        && let Some(c) = cookie_header(&url)
    {
        headers.push(("Cookie".into(), c));
    }
    // The body, and its type unless one was given.
    let has_ct = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    let mut sent_body = String::new();
    let mut body: Option<reqwest::Body> = None;
    let mut multipart: Option<reqwest::multipart::Form> = None;
    let mut ct: Option<&str> = None;
    match &ready.body {
        Body::None => {}
        Body::Json { text } => {
            sent_body = text.clone();
            body = Some(text.clone().into());
            ct = Some("application/json");
        }
        Body::Xml { text } => {
            sent_body = text.clone();
            body = Some(text.clone().into());
            ct = Some("application/xml");
        }
        Body::Text { text } => {
            sent_body = text.clone();
            body = Some(text.clone().into());
            ct = Some("text/plain");
        }
        Body::Html { text } => {
            sent_body = text.clone();
            body = Some(text.clone().into());
            ct = Some("text/html");
        }
        Body::Form { rows } => {
            let s: Vec<String> = rows.iter().filter(|r| !r.key.is_empty()).map(|r| format!("{}={}", encode_component(&r.key), encode_component(&r.value))).collect();
            sent_body = s.join("&");
            body = Some(sent_body.clone().into());
            ct = Some("application/x-www-form-urlencoded");
        }
        Body::Multipart { rows } => {
            let mut form = reqwest::multipart::Form::new();
            for r in rows.iter().filter(|r| !r.key.is_empty()) {
                if r.flag {
                    let path = PathBuf::from(r.value.trim());
                    let bytes = tokio::fs::read(&path).await.map_err(|e| anyhow!("{}: {e}", path.display()))?;
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(name.clone());
                    if !r.desc.trim().is_empty() {
                        part = part.mime_str(r.desc.trim()).map_err(|e| anyhow!("{}: {e}", r.desc))?;
                    }
                    form = form.part(r.key.clone(), part);
                    sent_body.push_str(&format!("{}: <file {name}>\n", r.key));
                } else {
                    form = form.text(r.key.clone(), r.value.clone());
                    sent_body.push_str(&format!("{}: {}\n", r.key, r.value));
                }
            }
            multipart = Some(form);
        }
        Body::Binary { path } => {
            if !path.trim().is_empty() {
                let f = tokio::fs::File::open(path.trim()).await.map_err(|e| anyhow!("{path}: {e}"))?;
                body = Some(reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(f)));
                sent_body = format!("<file {path}>");
                ct = Some("application/octet-stream");
            }
        }
        Body::Graphql { query, variables } => {
            let vars: serde_json::Value = if variables.trim().is_empty() { serde_json::Value::Null } else { serde_json::from_str(variables).map_err(|e| anyhow!("GraphQL variables aren't JSON: {e}"))? };
            let v = serde_json::json!({ "query": query, "variables": vars });
            sent_body = serde_json::to_string_pretty(&v)?;
            body = Some(v.to_string().into());
            ct = Some("application/json");
        }
    }
    if !has_ct && let Some(ct) = ct {
        headers.push(("Content-Type".into(), ct.into()));
    }
    // AWS Signature v4 signs everything above.
    if let Auth::Aws { access_key, secret_key, session_token, region, service } = &ready.auth {
        let payload = match &ready.body {
            Body::None | Body::Binary { .. } | Body::Multipart { .. } => String::new(),
            _ => sent_body.clone(),
        };
        aws_sign(&mut headers, &ready.method, &url, payload.as_bytes(), access_key, secret_key, session_token, region, service);
    }
    let mut rb = client.request(method, url.clone());
    for (k, v) in &headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    if let Some(f) = multipart {
        rb = rb.multipart(f);
    } else if let Some(b) = body {
        rb = rb.body(b);
    }
    let sent = Sent { method: ready.method.clone(), url: url.to_string(), headers: headers.clone(), body: sent_body.chars().take(20_000).collect() };
    let start = Instant::now();
    let resp = rb.send().await.map_err(explain)?;
    let ttfb_ms = start.elapsed().as_millis() as u64;
    let status = resp.status();
    let version = format!("{:?}", resp.version()).replace("HTTP/", "HTTP/");
    let final_url = resp.url().clone();
    let redirects = if final_url != url { vec![final_url.to_string()] } else { Vec::new() };
    let rheaders: Vec<(String, String)> = resp.headers().iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
    store_cookies(&final_url, rheaders.iter().filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.as_str()));
    // The body: in memory up to MAX_IN_MEMORY, then all of it on disk.
    let mut stream = resp.bytes_stream();
    let mut mem: Vec<u8> = Vec::new();
    let mut file: Option<(PathBuf, tokio::fs::File)> = None;
    let mut size = 0u64;
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(explain)?;
        size += chunk.len() as u64;
        match &mut file {
            Some((_, f)) => f.write_all(&chunk).await?,
            None if mem.len() + chunk.len() > MAX_IN_MEMORY => {
                let path = std::env::temp_dir().join(format!("kite-response-{}-{}", std::process::id(), super::new_id()));
                let mut f = tokio::fs::File::create(&path).await?;
                f.write_all(&mem).await?;
                f.write_all(&chunk).await?;
                let room = MAX_IN_MEMORY - mem.len();
                mem.extend_from_slice(&chunk[..room.min(chunk.len())]);
                file = Some((path, f));
            }
            None => mem.extend_from_slice(&chunk),
        }
    }
    if let Some((_, f)) = &mut file {
        f.flush().await?;
    }
    Ok(Response {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("").to_string(),
        version,
        headers: rheaders,
        body: mem,
        body_file: file.map(|(p, _)| p),
        size,
        ms: start.elapsed().as_millis() as u64,
        ttfb_ms,
        sent,
        redirects,
    })
}

/// A transport error, readably (the cause chain, without repeats).
fn explain(e: reqwest::Error) -> anyhow::Error {
    let mut parts: Vec<String> = Vec::new();
    let mut cur: Option<&dyn std::error::Error> = Some(&e);
    while let Some(err) = cur {
        let s = err.to_string();
        if !parts.iter().any(|p| p.contains(&s) || s.contains(p.as_str())) {
            parts.push(s);
        }
        cur = err.source();
    }
    let text = parts.join(": ");
    if e.is_timeout() {
        anyhow!("timed out ({text})")
    } else if e.is_connect() {
        anyhow!("couldn't connect: {text}")
    } else {
        anyhow!(text)
    }
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------- Digest

fn md5_hex(s: &str) -> String {
    use md5::Digest;
    hex(&md5::Md5::digest(s.as_bytes()))
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(b))
}

/// The reply to a `WWW-Authenticate: Digest …` challenge (RFC 7616: MD5 or SHA-256, qop auth).
pub fn digest(challenge: &str, user: &str, pass: &str, method: &str, url: &str) -> Result<String> {
    let params = parse_auth_params(challenge.trim_start_matches(|c: char| c.is_alphabetic()).trim());
    let realm = params.get("realm").cloned().unwrap_or_default();
    let nonce = params.get("nonce").cloned().ok_or_else(|| anyhow!("the Digest challenge has no nonce"))?;
    let algo = params.get("algorithm").cloned().unwrap_or_else(|| "MD5".into());
    let qop = params.get("qop").map(|q| if q.split(',').any(|x| x.trim() == "auth") { "auth".to_string() } else { q.clone() });
    let uri = reqwest::Url::parse(url).map(|u| format!("{}{}", u.path(), u.query().map(|q| format!("?{q}")).unwrap_or_default())).unwrap_or_else(|_| "/".into());
    let h = |s: &str| if algo.to_uppercase().starts_with("SHA-256") { sha256_hex(s.as_bytes()) } else { md5_hex(s) };
    let cnonce = hex(&fastrand::u64(..).to_le_bytes());
    let nc = "00000001";
    let ha1 = h(&format!("{user}:{realm}:{pass}"));
    let ha2 = h(&format!("{method}:{uri}"));
    let response = match &qop {
        Some(q) => h(&format!("{ha1}:{nonce}:{nc}:{cnonce}:{q}:{ha2}")),
        None => h(&format!("{ha1}:{nonce}:{ha2}")),
    };
    let mut out = format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm={algo}, response=\"{response}\"");
    if let Some(q) = qop {
        out.push_str(&format!(", qop={q}, nc={nc}, cnonce=\"{cnonce}\""));
    }
    if let Some(o) = params.get("opaque") {
        out.push_str(&format!(", opaque=\"{o}\""));
    }
    Ok(out)
}

fn parse_auth_params(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = s;
    while !rest.is_empty() {
        rest = rest.trim_start_matches([',', ' ']);
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_lowercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            value = r[..end].to_string();
            rest = &r[(end + 1).min(r.len())..];
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        out.insert(key, value);
    }
    out
}

// ---------------------------------------------------------------- AWS Signature v4

fn hmac256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{KeyInit, Mac};
    let mut m = <hmac::Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

#[allow(clippy::too_many_arguments)]
fn aws_sign(headers: &mut Vec<(String, String)>, method: &str, url: &reqwest::Url, payload: &[u8], access: &str, secret: &str, session: &str, region: &str, service: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (y, mo, d) = super::vars::civil(now.div_euclid(86_400));
    let t = now.rem_euclid(86_400);
    let date = format!("{y:04}{mo:02}{d:02}");
    let amz = format!("{date}T{:02}{:02}{:02}Z", t / 3600, t / 60 % 60, t % 60);
    let payload_hash = sha256_hex(payload);
    let host = url.host_str().unwrap_or("").to_string() + &url.port().map(|p| format!(":{p}")).unwrap_or_default();
    set_header(headers, "X-Amz-Date", &amz);
    set_header(headers, "X-Amz-Content-Sha256", &payload_hash);
    if !session.is_empty() {
        set_header(headers, "X-Amz-Security-Token", session);
    }
    let mut canon: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())).collect();
    canon.push(("host".into(), host));
    canon.sort();
    canon.dedup_by(|a, b| a.0 == b.0);
    let signed: Vec<&str> = canon.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let mut q: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (encode_component(&k), encode_component(&v))).collect();
    q.sort();
    let query: Vec<String> = q.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let path = if url.path().is_empty() { "/" } else { url.path() };
    let creq = format!("{method}\n{path}\n{}\n{}\n\n{signed}\n{payload_hash}", query.join("&"), canon.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join("\n"));
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let sts = format!("AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}", sha256_hex(creq.as_bytes()));
    let k = hmac256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac256(&k, region.as_bytes());
    let k = hmac256(&k, service.as_bytes());
    let k = hmac256(&k, b"aws4_request");
    let sig = hex(&hmac256(&k, sts.as_bytes()));
    set_header(headers, "Authorization", &format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={sig}"));
}

// ---------------------------------------------------------------- OAuth 2.0

/// Tokens got this session (and kept in the Keychain), by client and token URL.
static TOKENS: LazyLock<Mutex<HashMap<String, Token>>> = LazyLock::new(Default::default);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Token {
    pub access: String,
    pub refresh: String,
    /// Seconds since 1970 (0: doesn't say).
    pub expires: u64,
}

fn token_key(o: &OAuth2) -> String {
    format!("oauth/{}@{}", o.client_id, o.token_url)
}

pub fn cached_token(o: &OAuth2) -> Option<Token> {
    if let Some(t) = TOKENS.lock().unwrap().get(&token_key(o)) {
        return Some(t.clone());
    }
    #[cfg(not(test))]
    {
        let t: Token = serde_json::from_str(&crate::db::keychain::read("OSKL Code API", &token_key(o))?).ok()?;
        TOKENS.lock().unwrap().insert(token_key(o), t.clone());
        return Some(t);
    }
    #[allow(unreachable_code)]
    None
}

fn keep_token(o: &OAuth2, t: &Token) {
    TOKENS.lock().unwrap().insert(token_key(o), t.clone());
    #[cfg(not(test))]
    if let Ok(s) = serde_json::to_string(t) {
        crate::db::keychain::write("OSKL Code API", &token_key(o), &s).ok();
    }
}

pub fn forget_token(o: &OAuth2) {
    TOKENS.lock().unwrap().remove(&token_key(o));
    #[cfg(not(test))]
    crate::db::keychain::delete("OSKL Code API", &token_key(o));
}

/// A valid access token: the kept one, refreshed when it expired, else a new one.
pub async fn oauth_token(o: &OAuth2, client: &reqwest::Client) -> Result<String> {
    let now = super::now_secs();
    if let Some(t) = cached_token(o) {
        if t.expires == 0 || t.expires > now + 30 {
            return Ok(t.access);
        }
        if !t.refresh.is_empty() {
            if let Ok(t2) = token_request(o, client, &[("grant_type", "refresh_token"), ("refresh_token", &t.refresh)]).await {
                return Ok(t2.access);
            }
        }
    }
    Ok(get_token(o, client).await?.access)
}

/// Gets a new token, by the grant chosen.
pub async fn get_token(o: &OAuth2, client: &reqwest::Client) -> Result<Token> {
    match o.grant.as_str() {
        "password" => token_request(o, client, &[("grant_type", "password"), ("username", &o.username), ("password", &o.password)]).await,
        "authorization_code" => authorization_code(o, client).await,
        "refresh_token" => {
            let refresh = cached_token(o).map(|t| t.refresh).filter(|r| !r.is_empty()).ok_or_else(|| anyhow!("no refresh token yet: get a token first"))?;
            token_request(o, client, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await
        }
        _ => token_request(o, client, &[("grant_type", "client_credentials")]).await,
    }
}

async fn token_request(o: &OAuth2, client: &reqwest::Client, extra: &[(&str, &str)]) -> Result<Token> {
    if o.token_url.trim().is_empty() {
        bail!("OAuth 2.0: the token URL is needed");
    }
    let mut form: Vec<(String, String)> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    if !o.scope.is_empty() {
        form.push(("scope".into(), o.scope.clone()));
    }
    if !o.audience.is_empty() {
        form.push(("audience".into(), o.audience.clone()));
    }
    let mut rb = client.post(o.token_url.trim()).header("Accept", "application/json").header("Content-Type", "application/x-www-form-urlencoded");
    if o.basic_client_auth {
        rb = rb.header("Authorization", format!("Basic {}", base64(format!("{}:{}", encode_component(&o.client_id), encode_component(&o.client_secret)).as_bytes())));
    } else {
        form.push(("client_id".into(), o.client_id.clone()));
        if !o.client_secret.is_empty() {
            form.push(("client_secret".into(), o.client_secret.clone()));
        }
    }
    let body: Vec<String> = form.iter().map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v))).collect();
    let resp = rb.body(body.join("&")).send().await.map_err(explain)?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let access = v.get("access_token").and_then(|t| t.as_str()).map(str::to_string);
    let Some(access) = access.filter(|_| status.is_success()) else {
        let why = v.get("error_description").or(v.get("error")).and_then(|e| e.as_str()).map(str::to_string).unwrap_or(text);
        bail!("OAuth 2.0 token: {status}: {why}");
    };
    let expires = v.get("expires_in").and_then(|e| e.as_u64().or_else(|| e.as_str().and_then(|s| s.parse().ok()))).map(|s| super::now_secs() + s).unwrap_or(0);
    let refresh = v.get("refresh_token").and_then(|t| t.as_str()).map(str::to_string).or_else(|| cached_token(o).map(|t| t.refresh)).unwrap_or_default();
    let t = Token { access, refresh, expires };
    keep_token(o, &t);
    Ok(t)
}

/// The authorization-code grant: the browser asks the user, then comes back to a port here.
async fn authorization_code(o: &OAuth2, client: &reqwest::Client) -> Result<Token> {
    let redirect = if o.redirect_uri.is_empty() { "http://127.0.0.1:53682/callback".to_string() } else { o.redirect_uri.clone() };
    let ru = reqwest::Url::parse(&redirect).map_err(|e| anyhow!("redirect URL: {e}"))?;
    let port = ru.port_or_known_default().unwrap_or(80);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| anyhow!("can't listen on port {port} for the redirect: {e}"))?;
    let state = super::new_id();
    let verifier: String = (0..64).map(|_| fastrand::alphanumeric()).collect();
    let challenge = {
        use sha2::Digest;
        base64(&sha2::Sha256::digest(verifier.as_bytes())).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
    };
    let mut auth = reqwest::Url::parse(o.auth_url.trim()).map_err(|e| anyhow!("authorization URL: {e}"))?;
    {
        let mut q = auth.query_pairs_mut();
        q.append_pair("response_type", "code").append_pair("client_id", &o.client_id).append_pair("redirect_uri", &redirect).append_pair("state", &state);
        if !o.scope.is_empty() {
            q.append_pair("scope", &o.scope);
        }
        if !o.audience.is_empty() {
            q.append_pair("audience", &o.audience);
        }
        if o.pkce {
            q.append_pair("code_challenge", &challenge).append_pair("code_challenge_method", "S256");
        }
    }
    std::process::Command::new("open").arg(auth.as_str()).spawn().map_err(|e| anyhow!("can't open the browser: {e}"))?;
    // The browser comes back with ?code=…&state=…
    let (mut sock, _) = tokio::time::timeout(Duration::from_secs(300), listener.accept()).await.map_err(|_| anyhow!("no answer from the browser in 5 minutes"))??;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 8192];
    let n = sock.read(&mut buf).await?;
    let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let back = reqwest::Url::parse(&format!("http://localhost{target}"))?;
    let q: HashMap<String, String> = back.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let ok = q.get("state") == Some(&state) && q.contains_key("code");
    let page = if ok { "Signed in. You can close this tab and go back to OSKL Code." } else { "Sign-in failed. Go back to OSKL Code." };
    sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<html><body style=\"font-family:-apple-system;padding:40px\">{page}</body></html>").as_bytes()).await.ok();
    if !ok {
        bail!("OAuth 2.0: {}", q.get("error_description").or(q.get("error")).cloned().unwrap_or_else(|| "the browser didn't send a code".into()));
    }
    let code = q["code"].clone();
    let mut extra: Vec<(&str, &str)> = vec![("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect)];
    if o.pkce {
        extra.push(("code_verifier", &verifier));
    }
    token_request(o, client, &extra).await
}

/// For the Auth tab's "Get token" button.
pub async fn fetch_token(o: OAuth2, settings: Settings) -> Result<Token> {
    let c = client(&settings)?;
    get_token(&o, &c).await
}

// ---------------------------------------------------------------- cookies

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Seconds since 1970; 0 lasts the session.
    pub expires: u64,
    pub secure: bool,
    pub http_only: bool,
    /// Set without a Domain attribute: this host only.
    pub host_only: bool,
}

static JAR: LazyLock<Mutex<Vec<Cookie>>> = LazyLock::new(|| Mutex::new(load_jar()));

fn jar_path() -> Option<PathBuf> {
    Some(super::base(None)?.join("cookies.json"))
}

fn load_jar() -> Vec<Cookie> {
    #[cfg(test)]
    return Vec::new();
    #[allow(unreachable_code)]
    jar_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_jar(jar: &[Cookie]) {
    #[cfg(test)]
    {
        let _ = jar;
        return;
    }
    #[allow(unreachable_code)]
    if let Some(p) = jar_path() {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).ok();
        }
        // Session cookies aren't kept.
        let keep: Vec<&Cookie> = jar.iter().filter(|c| c.expires != 0).collect();
        std::fs::write(p, serde_json::to_vec_pretty(&keep).unwrap_or_default()).ok();
    }
}

pub fn cookies() -> Vec<Cookie> {
    let now = super::now_secs();
    let mut j = JAR.lock().unwrap();
    j.retain(|c| c.expires == 0 || c.expires > now);
    j.clone()
}

pub fn delete_cookie(domain: Option<&str>, name: Option<&str>) {
    let mut j = JAR.lock().unwrap();
    j.retain(|c| !(domain.is_none_or(|d| d == c.domain) && name.is_none_or(|n| n == c.name)));
    save_jar(&j);
}

fn cookie_header(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?.to_lowercase();
    let path = url.path();
    let secure = url.scheme() == "https";
    let now = super::now_secs();
    let j = JAR.lock().unwrap();
    let pairs: Vec<String> = j
        .iter()
        .filter(|c| c.expires == 0 || c.expires > now)
        .filter(|c| if c.host_only { host == c.domain } else { host == c.domain || host.ends_with(&format!(".{}", c.domain)) })
        .filter(|c| path.starts_with(&c.path))
        .filter(|c| !c.secure || secure)
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

/// `Set-Cookie` lines into the jar.
pub fn store_cookies<'a>(url: &reqwest::Url, lines: impl Iterator<Item = &'a str>) {
    let host = url.host_str().unwrap_or("").to_lowercase();
    let mut changed = false;
    let mut j = JAR.lock().unwrap();
    for line in lines {
        let mut parts = line.split(';');
        let Some((name, value)) = parts.next().and_then(|nv| nv.split_once('=')) else { continue };
        let mut c = Cookie { name: name.trim().into(), value: value.trim().into(), domain: host.clone(), path: "/".into(), expires: 0, secure: false, http_only: false, host_only: true };
        for a in parts {
            let (k, v) = a.split_once('=').unwrap_or((a, ""));
            match k.trim().to_lowercase().as_str() {
                "domain" if !v.trim().is_empty() => {
                    c.domain = v.trim().trim_start_matches('.').to_lowercase();
                    c.host_only = false;
                }
                "path" if v.trim().starts_with('/') => c.path = v.trim().into(),
                "max-age" => {
                    if let Ok(s) = v.trim().parse::<i64>() {
                        c.expires = if s <= 0 { 1 } else { super::now_secs() + s as u64 };
                    }
                }
                "expires" if c.expires == 0 => c.expires = http_date(v.trim()).unwrap_or(0),
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                _ => {}
            }
        }
        j.retain(|x| !(x.name == c.name && x.domain == c.domain && x.path == c.path));
        j.push(c);
        changed = true;
    }
    if changed {
        save_jar(&j);
    }
}

/// "Wed, 21 Oct 2026 07:28:00 GMT" → seconds since 1970.
fn http_date(s: &str) -> Option<u64> {
    let p: Vec<&str> = s.split([' ', '-', ':', ',']).filter(|x| !x.is_empty()).collect();
    let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let (d, m, y, hh, mm, ss) = (p.get(1)?.parse::<i64>().ok()?, months.iter().position(|x| p.get(2).is_some_and(|m| m.to_lowercase().starts_with(x)))? as i64 + 1, p.get(3)?.parse::<i64>().ok()?, p.get(4)?.parse::<i64>().ok()?, p.get(5)?.parse::<i64>().ok()?, p.get(6)?.parse::<i64>().ok()?);
    let y = if y < 100 { y + 2000 } else { y };
    // Days from civil (inverse of vars::civil).
    let (yy, mm2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss).max(0) as u64)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn urls_params_and_paths() {
        assert_eq!(path_param_names("https://x.io/users/:id/posts/{post}?a=1"), ["id", "post"]);
        let rows = query_rows("https://x.io/a?q=hello+world&tag=a%26b&flag");
        assert_eq!(rows.iter().map(|r| (r.key.as_str(), r.value.as_str())).collect::<Vec<_>>(), [("q", "hello world"), ("tag", "a&b"), ("flag", "")]);
        let mut r = rows.clone();
        r[1].on = false;
        assert_eq!(with_query("https://x.io/a?old=1#top", &r), "https://x.io/a?q=hello world&flag#top");
        assert_eq!(encode_url("https://x.io/a b?q=é"), "https://x.io/a%20b?q=%C3%A9");
    }

    #[test]
    fn folders_give_headers_and_auth_and_variables_fill_in() {
        let mut coll = Folder { auth: Auth::Bearer { token: "{{token}}".into(), prefix: "Bearer".into() }, ..Default::default() };
        coll.headers.push(Row::new("X-App", "kite"));
        let mut folder = Folder { auth: Auth::Inherit, ..Default::default() };
        folder.headers.push(Row::new("X-App", "orders"));
        let req = Request { url: "{{base}}/orders/:id".into(), path_params: vec![Row::new("id", "a b")], headers: vec![Row::new("Accept", "application/json")], ..Default::default() };
        let env = super::super::Environment { name: "Dev".into(), vars: vec![Row::new("base", "localhost:3000"), Row::new("token", "t0k")], ..Default::default() };
        let vars = Vars::new(&[&folder.vars[..], &coll.vars[..]], Some(&env), None);
        let ready = prepare(&req, &[&folder, &coll], &vars);
        assert_eq!(ready.url, "http://localhost:3000/orders/a%20b");
        assert_eq!(ready.headers, [("X-App".to_string(), "orders".to_string()), ("Accept".to_string(), "application/json".to_string())], "the nearer folder wins");
        assert_eq!(ready.auth, Auth::Bearer { token: "t0k".into(), prefix: "Bearer".into() }, "inherited through the folder");
        assert!(ready.missing.is_empty());
    }

    #[test]
    fn digest_answers_the_challenge() {
        // RFC 2617's example.
        let h = digest("Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"", "Mufasa", "Circle Of Life", "GET", "http://h/dir/index.html").unwrap();
        assert!(h.starts_with("Digest username=\"Mufasa\", realm=\"testrealm@host.com\""), "{h}");
        assert!(h.contains("uri=\"/dir/index.html\"") && h.contains("qop=auth") && h.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""), "{h}");
        assert_eq!(md5_hex("Mufasa:testrealm@host.com:Circle Of Life"), "939e7578ed9e3c518a452acee763bce9");
    }

    #[test]
    fn cookies_match_their_domain_and_path() {
        let u = reqwest::Url::parse("https://api.example.com/v1/login").unwrap();
        store_cookies(&u, ["sid=abc; Path=/; HttpOnly", "pref=dark; Domain=example.com; Path=/v1; Max-Age=3600", "gone=1; Max-Age=0"].into_iter());
        let h = cookie_header(&reqwest::Url::parse("https://api.example.com/v1/me").unwrap()).unwrap();
        assert!(h.contains("sid=abc") && h.contains("pref=dark") && !h.contains("gone"), "{h}");
        let other = cookie_header(&reqwest::Url::parse("https://www.example.com/v2").unwrap());
        assert_eq!(other, None, "sid is for api. only; pref for /v1 only");
        assert_eq!(http_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        delete_cookie(Some("api.example.com"), None);
        delete_cookie(Some("example.com"), None);
    }

    #[test]
    fn aws_signature_is_well_formed() {
        let mut h = vec![("Content-Type".to_string(), "application/json".to_string())];
        aws_sign(&mut h, "GET", &reqwest::Url::parse("https://s3.amazonaws.com/bucket?list-type=2").unwrap(), b"", "AKID", "SECRET", "", "us-east-1", "s3");
        let auth = h.iter().find(|(k, _)| k == "Authorization").unwrap().1.clone();
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKID/") && auth.contains("/us-east-1/s3/aws4_request") && auth.contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"), "{auth}");
    }

    /// A tiny HTTP server for the tests: /echo, /digest, /redirect, /big, /cookie.
    pub(crate) fn server() -> u16 {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::db::engine::runtime().spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap().port()).unwrap();
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head, body) = loop {
                        let n = s.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..i]).to_string();
                            let len: usize = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
                            while buf.len() < i + 4 + len {
                                let n = s.read(&mut chunk).await.unwrap_or(0);
                                if n == 0 {
                                    break;
                                }
                                buf.extend_from_slice(&chunk[..n]);
                            }
                            break (head, String::from_utf8_lossy(&buf[i + 4..]).to_string());
                        }
                    };
                    let mut lines = head.lines();
                    let first = lines.next().unwrap_or("").to_string();
                    let mut parts = first.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let headers: serde_json::Map<String, serde_json::Value> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), serde_json::json!(v.trim()))).collect();
                    let auth = headers.get("authorization").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let (status, extra, body): (&str, String, Vec<u8>) = match path.split('?').next().unwrap() {
                        "/digest" if !auth.starts_with("Digest ") => ("401 Unauthorized", "WWW-Authenticate: Digest realm=\"kite\", nonce=\"abc\", qop=\"auth\"\r\n".into(), b"no".to_vec()),
                        "/digest" => {
                            let ok = auth.contains("username=\"ada\"") && auth.contains("uri=\"/digest\"");
                            (if ok { "200 OK" } else { "403 Forbidden" }, String::new(), b"in".to_vec())
                        }
                        "/redirect" => ("302 Found", "Location: /echo?from=redirect\r\n".into(), Vec::new()),
                        "/big" => ("200 OK", "Content-Type: text/plain\r\n".into(), vec![b'x'; 3 * 1024 * 1024]),
                        "/cookie" => ("200 OK", "Set-Cookie: flavor=oat; Path=/\r\n".into(), b"set".to_vec()),
                        _ => ("200 OK", "Content-Type: application/json\r\n".into(), serde_json::json!({ "method": method, "path": path, "headers": headers, "body": body }).to_string().into_bytes()),
                    };
                    let resp = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    s.write_all(resp.as_bytes()).await.ok();
                    s.write_all(&body).await.ok();
                });
            }
        });
        rx.recv().unwrap()
    }

    fn go(r: Ready) -> Response {
        futures::executor::block_on(crate::db::engine::run(send(r))).unwrap()
    }

    #[test]
    fn requests_go_out_and_answers_come_back() {
        let port = server();
        let base = format!("http://127.0.0.1:{port}");
        // A JSON body, headers, a query, auth.
        let req = Request { method: "POST".into(), url: format!("{base}/echo?q=1"), headers: vec![Row::new("X-Kite", "yes")], body: Body::Json { text: "{\"a\":1}".into() }, auth: Auth::Apikey { key: "k".into(), value: "v w".into(), query: true }, ..Default::default() };
        let r = go(prepare(&req, &[], &Vars::default()));
        assert_eq!(r.status, 200);
        let j = r.json().unwrap();
        assert_eq!(j["method"], "POST");
        assert_eq!(j["path"], "/echo?q=1&k=v+w");
        assert_eq!(j["headers"]["x-kite"], "yes");
        assert_eq!(j["headers"]["content-type"], "application/json");
        assert_eq!(j["body"], "{\"a\":1}");
        assert_eq!(r.sent.headers.iter().find(|(k, _)| k == "X-Kite").unwrap().1, "yes");
        // Form fields.
        let req = Request { method: "PUT".into(), url: format!("{base}/echo"), body: Body::Form { rows: vec![Row::new("a b", "c&d")] }, ..Default::default() };
        let j = go(prepare(&req, &[], &Vars::default())).json().unwrap();
        assert_eq!((j["body"].as_str(), j["headers"]["content-type"].as_str()), (Some("a%20b=c%26d"), Some("application/x-www-form-urlencoded")));
        // Digest: asked, then answered.
        let req = Request { url: format!("{base}/digest"), auth: Auth::Digest { user: "ada".into(), pass: "pw".into() }, ..Default::default() };
        assert_eq!(go(prepare(&req, &[], &Vars::default())).status, 200);
        // Redirects followed, or not.
        let mut req = Request { url: format!("{base}/redirect"), ..Default::default() };
        let r = go(prepare(&req, &[], &Vars::default()));
        assert_eq!((r.status, r.redirects.len()), (200, 1));
        req.settings.follow_redirects = false;
        assert_eq!(go(prepare(&req, &[], &Vars::default())).status, 302);
        // A big body: 2 MB kept, all of it on disk.
        let req = Request { url: format!("{base}/big"), ..Default::default() };
        let r = go(prepare(&req, &[], &Vars::default()));
        assert_eq!((r.size, r.body.len()), (3 * 1024 * 1024, MAX_IN_MEMORY));
        let f = r.body_file.clone().unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().len(), 3 * 1024 * 1024);
        std::fs::remove_file(f).ok();
        // Cookies come back on the next request.
        go(prepare(&Request { url: format!("{base}/cookie"), ..Default::default() }, &[], &Vars::default()));
        let j = go(prepare(&Request { url: format!("{base}/echo"), ..Default::default() }, &[], &Vars::default())).json().unwrap();
        assert_eq!(j["headers"]["cookie"], "flavor=oat");
        delete_cookie(Some("127.0.0.1"), None);
        // Nobody there.
        let err = futures::executor::block_on(crate::db::engine::run(send(prepare(&Request { url: "http://127.0.0.1:1/".into(), ..Default::default() }, &[], &Vars::default())))).unwrap_err();
        assert!(err.contains("connect"), "{err}");
    }
}
