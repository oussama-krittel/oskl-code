//! A request as code to paste elsewhere.

use super::send::{Ready, base64};
use super::{Auth, Body};

pub const LANGUAGES: [(&str, &str); 14] = [
    ("cURL", "sh"),
    ("HTTPie", "sh"),
    ("wget", "sh"),
    ("JavaScript fetch", "js"),
    ("Axios", "js"),
    ("Python requests", "py"),
    ("Go", "go"),
    ("Java HttpClient", "java"),
    ("C# HttpClient", "cs"),
    ("PHP cURL", "php"),
    ("Ruby", "rb"),
    ("Swift", "swift"),
    ("Rust reqwest", "rs"),
    ("PowerShell", "ps1"),
];

/// Headers with auth in them (what most snippets send), and the body as text.
fn parts(r: &Ready) -> (Vec<(String, String)>, Option<String>) {
    let mut h = r.headers.clone();
    let mut set = |k: &str, v: String| {
        h.retain(|(x, _)| !x.eq_ignore_ascii_case(k));
        h.push((k.to_string(), v));
    };
    match &r.auth {
        Auth::Basic { user, pass } => set("Authorization", format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()))),
        Auth::Bearer { token, prefix } => set("Authorization", format!("{} {token}", if prefix.is_empty() { "Bearer" } else { prefix })),
        Auth::Apikey { key, value, query: false } => set(key, value.clone()),
        _ => {}
    }
    let ct = match &r.body {
        Body::Json { .. } | Body::Graphql { .. } => Some("application/json"),
        Body::Xml { .. } => Some("application/xml"),
        Body::Text { .. } => Some("text/plain"),
        Body::Html { .. } => Some("text/html"),
        Body::Form { .. } => Some("application/x-www-form-urlencoded"),
        _ => None,
    };
    if let Some(ct) = ct
        && !h.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        h.push(("Content-Type".into(), ct.into()));
    }
    let body = match &r.body {
        Body::Json { text } | Body::Xml { text } | Body::Text { text } | Body::Html { text } => Some(text.clone()),
        Body::Form { rows } => Some(rows.iter().filter(|x| x.on).map(|x| format!("{}={}", super::send::encode_component(&x.key), super::send::encode_component(&x.value))).collect::<Vec<_>>().join("&")),
        Body::Graphql { query, variables } => {
            let vars: serde_json::Value = serde_json::from_str(variables).unwrap_or(serde_json::Value::Null);
            Some(serde_json::to_string_pretty(&serde_json::json!({ "query": query, "variables": vars })).unwrap_or_default())
        }
        _ => None,
    };
    (h, body)
}

fn url(r: &Ready) -> String {
    match &r.auth {
        Auth::Apikey { key, value, query: true } => format!("{}{}{}={}", r.url, if r.url.contains('?') { "&" } else { "?" }, super::send::encode_component(key), super::send::encode_component(value)),
        _ => r.url.clone(),
    }
}

/// A string literal in the C family's style ("…" with escapes).
fn q(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn files(r: &Ready) -> Vec<(String, String, bool)> {
    match &r.body {
        Body::Multipart { rows } => rows.iter().filter(|x| x.on).map(|x| (x.key.clone(), x.value.clone(), x.flag)).collect(),
        _ => Vec::new(),
    }
}

pub fn generate(lang: &str, r: &Ready) -> String {
    let (h, body) = parts(r);
    let u = url(r);
    let m = r.method.as_str();
    let mp = files(r);
    let bin = match &r.body {
        Body::Binary { path } => Some(path.clone()),
        _ => None,
    };
    match lang {
        "cURL" => super::import::to_curl(r),
        "HTTPie" => {
            let mut s = format!("http {m} {}", sh(&u));
            for (k, v) in &h {
                s.push_str(&format!(" \\\n  {}", sh(&format!("{k}:{v}"))));
            }
            for (k, v, f) in &mp {
                s.push_str(&format!(" \\\n  {}", sh(&format!("{k}{}{v}", if *f { "@" } else { "=" }))));
            }
            if !mp.is_empty() {
                s = s.replacen("http ", "http --form ", 1);
            }
            match (&body, &bin) {
                (Some(b), _) => format!("printf %s {} | {s}", sh(b)),
                (_, Some(p)) => format!("{s} < {}", sh(p)),
                _ => s,
            }
        }
        "wget" => {
            let mut s = format!("wget --quiet --method {m} --output-document - {}", sh(&u));
            for (k, v) in &h {
                s.push_str(&format!(" \\\n  --header {}", sh(&format!("{k}: {v}"))));
            }
            if let Some(b) = &body {
                s.push_str(&format!(" \\\n  --body-data {}", sh(b)));
            }
            if let Some(p) = &bin {
                s.push_str(&format!(" \\\n  --body-file {}", sh(p)));
            }
            s
        }
        "JavaScript fetch" => {
            let mut s = String::new();
            if !mp.is_empty() {
                s.push_str("const form = new FormData();\n");
                for (k, v, f) in &mp {
                    if *f {
                        s.push_str(&format!("form.append({}, fileInput.files[0], {}); // {v}\n", q(k), q(v.rsplit('/').next().unwrap_or(v))));
                    } else {
                        s.push_str(&format!("form.append({}, {});\n", q(k), q(v)));
                    }
                }
                s.push('\n');
            }
            s.push_str(&format!("const response = await fetch({}, {{\n  method: {},\n  headers: {{\n", q(&u), q(m)));
            for (k, v) in &h {
                s.push_str(&format!("    {}: {},\n", q(k), q(v)));
            }
            s.push_str("  },\n");
            if let Some(b) = &body {
                s.push_str(&format!("  body: {},\n", q(b)));
            } else if !mp.is_empty() {
                s.push_str("  body: form,\n");
            }
            s.push_str("});\n\nconsole.log(response.status, await response.text());\n");
            s
        }
        "Axios" => {
            let mut s = String::from("import axios from \"axios\";\n\n");
            s.push_str(&format!("const response = await axios({{\n  method: {},\n  url: {},\n  headers: {{\n", q(&m.to_lowercase()), q(&u)));
            for (k, v) in &h {
                s.push_str(&format!("    {}: {},\n", q(k), q(v)));
            }
            s.push_str("  },\n");
            if let Some(b) = &body {
                s.push_str(&format!("  data: {},\n", q(b)));
            }
            s.push_str("});\n\nconsole.log(response.status, response.data);\n");
            s
        }
        "Python requests" => {
            let mut s = String::from("import requests\n\n");
            s.push_str(&format!("url = {}\nheaders = {{\n", q(&u)));
            for (k, v) in &h {
                s.push_str(&format!("    {}: {},\n", q(k), q(v)));
            }
            s.push_str("}\n");
            let mut extra = String::new();
            if let Some(b) = &body {
                s.push_str(&format!("data = {}\n", q(b)));
                extra.push_str(", data=data.encode()");
            }
            if !mp.is_empty() {
                s.push_str("files = {\n");
                for (k, v, f) in &mp {
                    if *f {
                        s.push_str(&format!("    {}: open({}, \"rb\"),\n", q(k), q(v)));
                    } else {
                        s.push_str(&format!("    {}: (None, {}),\n", q(k), q(v)));
                    }
                }
                s.push_str("}\n");
                extra.push_str(", files=files");
            }
            if let Some(p) = &bin {
                extra.push_str(&format!(", data=open({}, \"rb\")", q(p)));
            }
            s.push_str(&format!("\nresponse = requests.request({}, url, headers=headers{extra})\nprint(response.status_code, response.text)\n", q(m)));
            s
        }
        "Go" => {
            let mut s = String::from("package main\n\nimport (\n\t\"fmt\"\n\t\"io\"\n\t\"net/http\"\n");
            if body.is_some() {
                s.push_str("\t\"strings\"\n");
            }
            s.push_str(")\n\nfunc main() {\n");
            let b = match &body {
                Some(b) => {
                    s.push_str(&format!("\tbody := strings.NewReader({})\n", q(b)));
                    "body"
                }
                None => "nil",
            };
            s.push_str(&format!("\treq, err := http.NewRequest({}, {}, {b})\n\tif err != nil {{\n\t\tpanic(err)\n\t}}\n", q(m), q(&u)));
            for (k, v) in &h {
                s.push_str(&format!("\treq.Header.Set({}, {})\n", q(k), q(v)));
            }
            s.push_str("\n\tres, err := http.DefaultClient.Do(req)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tdefer res.Body.Close()\n\tdata, _ := io.ReadAll(res.Body)\n\tfmt.Println(res.Status, string(data))\n}\n");
            s
        }
        "Java HttpClient" => {
            let mut s = String::from("import java.net.URI;\nimport java.net.http.HttpClient;\nimport java.net.http.HttpRequest;\nimport java.net.http.HttpResponse;\n\n");
            s.push_str(&format!("HttpRequest request = HttpRequest.newBuilder()\n    .uri(URI.create({}))\n", q(&u)));
            for (k, v) in &h {
                s.push_str(&format!("    .header({}, {})\n", q(k), q(v)));
            }
            let pub_ = match &body {
                Some(b) => format!("HttpRequest.BodyPublishers.ofString({})", q(b)),
                None => "HttpRequest.BodyPublishers.noBody()".into(),
            };
            s.push_str(&format!("    .method({}, {pub_})\n    .build();\n\n", q(m)));
            s.push_str("HttpResponse<String> response = HttpClient.newHttpClient().send(request, HttpResponse.BodyHandlers.ofString());\nSystem.out.println(response.statusCode() + \" \" + response.body());\n");
            s
        }
        "C# HttpClient" => {
            let mut s = String::from("using var client = new HttpClient();\n");
            s.push_str(&format!("var request = new HttpRequestMessage(new HttpMethod({}), {});\n", q(m), q(&u)));
            let ct = h.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone());
            for (k, v) in h.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("content-type")) {
                s.push_str(&format!("request.Headers.TryAddWithoutValidation({}, {});\n", q(k), q(v)));
            }
            if let Some(b) = &body {
                s.push_str(&format!("request.Content = new StringContent({}, System.Text.Encoding.UTF8, {});\n", q(b), q(ct.as_deref().unwrap_or("text/plain").split(';').next().unwrap_or("text/plain"))));
            }
            s.push_str("var response = await client.SendAsync(request);\nConsole.WriteLine($\"{(int)response.StatusCode} {await response.Content.ReadAsStringAsync()}\");\n");
            s
        }
        "PHP cURL" => {
            let mut s = format!("<?php\n$ch = curl_init({});\ncurl_setopt_array($ch, [\n    CURLOPT_CUSTOMREQUEST => {},\n    CURLOPT_RETURNTRANSFER => true,\n    CURLOPT_FOLLOWLOCATION => {},\n    CURLOPT_HTTPHEADER => [\n", q(&u), q(m), r.settings.follow_redirects);
            for (k, v) in &h {
                s.push_str(&format!("        {},\n", q(&format!("{k}: {v}"))));
            }
            s.push_str("    ],\n");
            if let Some(b) = &body {
                s.push_str(&format!("    CURLOPT_POSTFIELDS => {},\n", q(b).replace('$', "\\$")));
            }
            s.push_str("]);\n$response = curl_exec($ch);\necho curl_getinfo($ch, CURLINFO_HTTP_CODE) . \" \" . $response;\ncurl_close($ch);\n");
            s
        }
        "Ruby" => {
            let mut s = String::from("require \"net/http\"\nrequire \"uri\"\n\n");
            s.push_str(&format!("uri = URI({})\nhttp = Net::HTTP.new(uri.host, uri.port)\nhttp.use_ssl = uri.scheme == \"https\"\n\n", q(&u)));
            let class = {
                let mut c = m.to_lowercase();
                if let Some(f) = c.get_mut(0..1) {
                    f.make_ascii_uppercase();
                }
                c
            };
            s.push_str(&format!("request = Net::HTTP::{class}.new(uri)\n"));
            for (k, v) in &h {
                s.push_str(&format!("request[{}] = {}\n", q(k), q(v)));
            }
            if let Some(b) = &body {
                s.push_str(&format!("request.body = {}\n", q(b).replace("#{", "\\#{")));
            }
            s.push_str("\nresponse = http.request(request)\nputs response.code, response.body\n");
            s
        }
        "Swift" => {
            let mut s = String::from("import Foundation\n\n");
            s.push_str(&format!("var request = URLRequest(url: URL(string: {})!)\nrequest.httpMethod = {}\n", q(&u), q(m)));
            for (k, v) in &h {
                s.push_str(&format!("request.setValue({}, forHTTPHeaderField: {})\n", q(v), q(k)));
            }
            if let Some(b) = &body {
                s.push_str(&format!("request.httpBody = {}.data(using: .utf8)\n", q(b)));
            }
            s.push_str("\nlet (data, response) = try await URLSession.shared.data(for: request)\nprint((response as! HTTPURLResponse).statusCode, String(decoding: data, as: UTF8.self))\n");
            s
        }
        "Rust reqwest" => {
            let mut s = String::from("let client = reqwest::Client::new();\n");
            s.push_str(&format!("let response = client\n    .request(reqwest::Method::from_bytes(b{})?, {})\n", q(m), q(&u)));
            for (k, v) in &h {
                s.push_str(&format!("    .header({}, {})\n", q(k), q(v)));
            }
            if let Some(b) = &body {
                s.push_str(&format!("    .body({})\n", q(b)));
            }
            s.push_str("    .send()\n    .await?;\nprintln!(\"{} {}\", response.status(), response.text().await?);\n");
            s
        }
        "PowerShell" => {
            let ps = |s: &str| format!("'{}'", s.replace('\'', "''"));
            let mut s = String::from("$headers = @{\n");
            for (k, v) in &h {
                s.push_str(&format!("    {} = {}\n", ps(k), ps(v)));
            }
            s.push_str("}\n");
            let mut extra = String::new();
            if let Some(b) = &body {
                s.push_str(&format!("$body = {}\n", ps(b)));
                extra.push_str(" -Body $body");
            }
            if let Some(p) = &bin {
                extra.push_str(&format!(" -InFile {}", ps(p)));
            }
            s.push_str(&format!("$response = Invoke-WebRequest -Uri {} -Method {m} -Headers $headers{extra}\n$response.StatusCode\n$response.Content\n", ps(&u)));
            s
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod snippets {
    use super::*;

    #[test]
    fn every_language_has_the_url_method_headers_and_body() {
        let r = Ready {
            method: "POST".into(),
            url: "https://x.io/orders".into(),
            headers: vec![("X-Team".into(), "core".into())],
            body: Body::Json { text: "{\"a\":\"b\"}".into() },
            auth: Auth::Bearer { token: "tok".into(), prefix: "Bearer".into() },
            ..Default::default()
        };
        for (lang, _) in LANGUAGES {
            let code = generate(lang, &r);
            assert!(code.contains("https://x.io/orders"), "{lang}: {code}");
            assert!(code.contains("X-Team") && code.contains("Bearer tok"), "{lang}: {code}");
            assert!(code.contains("POST") || code.contains("post") || code.contains("Post") || lang == "cURL", "{lang}: {code}");
            assert!(code.contains(r#"\"a\":\"b\""#) || code.contains(r#"{"a":"b"}"#), "{lang}: {code}");
        }
    }
}
