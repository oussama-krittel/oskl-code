//! Running a request the whole way: requests it needs first, pre-request scripts (the
//! folders', then its own), variables filled in, sending, checks, variables set from the
//! answer, post-response scripts. Shared by a request's Send and the collection runner.

use super::send::{self, Ready, Response};
use super::tests::{self, Outcome};
use super::vars::Vars;
use super::{Body, Collection, Environment, Folder, Request};
use std::collections::HashMap;

/// What's needed to run a request (read from disk on the UI's side, secrets added here).
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub req: Request,
    pub coll: Option<Collection>,
    pub env: Option<Environment>,
    pub global: Option<Environment>,
    /// Values for this run only (the runner's data file).
    pub extra: HashMap<String, String>,
    /// Skip the requests this one needs first (they ran already, in the runner).
    pub skip_pre_requests: bool,
}

#[derive(Debug, Default)]
pub struct Done {
    pub resp: Option<Response>,
    pub error: Option<String>,
    pub tests: Vec<Outcome>,
    pub logs: Vec<String>,
    /// (scope, name, value) to keep: environment, global, collection (or "unset").
    pub var_changes: Vec<(String, String, String)>,
    pub missing: Vec<String>,
    pub ready: Option<Ready>,
    /// The variables the request used: (name, value, where from), secrets masked.
    pub vars_used: Vec<(String, String, String)>,
}

/// The `{{names}}` a request uses, in order, once each.
fn names_used(r: &Request) -> Vec<String> {
    let text = serde_json::to_string(r).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("{{") {
        let after = &rest[i + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim().to_string();
        if !name.is_empty() && name.len() < 100 && !out.contains(&name) {
            out.push(name);
        }
        rest = &after[end + 2..];
    }
    out
}

/// The folders a request is in (outermost, the collection, first). A request not saved in
/// it yet gets the collection's settings.
pub fn chain<'a>(coll: &'a Collection, req_id: &str) -> Vec<&'a Folder> {
    let c = coll.root.chain_to(req_id);
    if c.is_empty() { vec![&coll.root] } else { c }
}

fn layer_for<'a>(scope: &str, env: Option<&'a Environment>) -> &'a str {
    match scope {
        "environment" => env.map(|e| e.name.as_str()).unwrap_or("global"),
        "global" => "global",
        "collection" => "collection",
        _ => "request",
    }
}

/// The request as a script sees it (before variables are filled in).
pub fn request_json(r: &Request) -> serde_json::Value {
    let body = match &r.body {
        Body::Json { text } | Body::Xml { text } | Body::Text { text } | Body::Html { text } => text.clone(),
        Body::Graphql { query, .. } => query.clone(),
        _ => String::new(),
    };
    serde_json::json!({
        "method": r.method,
        "url": r.url,
        "name": r.name,
        "headers": r.headers.iter().filter(|h| h.on && !h.key.is_empty()).map(|h| [h.key.clone(), h.value.clone()]).collect::<Vec<_>>(),
        "body": body,
    })
}

/// A script's changes to the request, back into it.
fn apply_request_json(r: &mut Request, v: &serde_json::Value) {
    if let Some(m) = v["method"].as_str() {
        r.method = m.to_uppercase();
    }
    if let Some(u) = v["url"].as_str() {
        r.url = u.to_string();
    }
    if let Some(h) = v["headers"].as_array() {
        let disabled: Vec<super::Row> = r.headers.iter().filter(|h| !h.on).cloned().collect();
        r.headers = h.iter().filter_map(|p| Some(super::Row::new(p.get(0)?.as_str()?, p.get(1)?.as_str()?))).collect();
        r.headers.extend(disabled);
    }
    if let Some(b) = v["body"].as_str() {
        match &mut r.body {
            Body::Json { text } | Body::Xml { text } | Body::Text { text } | Body::Html { text } => *text = b.to_string(),
            Body::Graphql { query, .. } => *query = b.to_string(),
            _ => {}
        }
    }
}

pub fn response_json(r: &Response) -> serde_json::Value {
    serde_json::json!({
        "status": r.status,
        "statusText": r.reason,
        "headers": r.headers.iter().map(|(k, v)| [k.clone(), v.clone()]).collect::<Vec<_>>(),
        "body": r.text(),
        "time": r.ms,
        "size": r.size,
    })
}

fn set(vars: &mut Vars, done: &mut Done, env: Option<&Environment>, scope: &str, name: &str, value: &str) {
    if scope == "unset" {
        done.var_changes.push(("unset".into(), name.into(), String::new()));
        return;
    }
    vars.set(layer_for(scope, env), name, value);
    if scope != "request" {
        done.var_changes.push((scope.into(), name.into(), value.into()));
    }
}

fn run_script(src: &str, vars: &mut Vars, done: &mut Done, env: Option<&Environment>, request: Option<serde_json::Value>, response: Option<serde_json::Value>, info: serde_json::Value, label: &str) -> Option<serde_json::Value> {
    if src.trim().is_empty() {
        return None;
    }
    let r = super::script::run(src, &vars.flat(), request, response, info);
    done.logs.extend(r.logs);
    if let Some(e) = r.error {
        done.logs.push(format!("✕ {label}: {e}"));
    }
    for (scope, name, value) in r.vars {
        set(vars, done, env, &scope, &name, &value);
    }
    done.tests.extend(r.tests);
    r.request
}

/// Runs it (on the database thread's runtime).
pub async fn execute(plan: Plan) -> Done {
    let mut done = Done::default();
    // Secret values come from the Keychain (which may ask): off this thread.
    let (env, global) = {
        let (e, g) = (plan.env.clone(), plan.global.clone());
        tokio::task::spawn_blocking(move || (e.map(|e| super::with_secrets(&e)), g.map(|g| super::with_secrets(&g)))).await.unwrap_or((None, None))
    };
    let folders: Vec<&Folder> = plan.coll.as_ref().map(|c| chain(c, &plan.req.id)).unwrap_or_default();
    let inner_first: Vec<&Folder> = folders.iter().rev().copied().collect();
    let var_rows: Vec<&[super::Row]> = inner_first.iter().map(|f| f.vars.as_slice()).collect();
    let mut vars = Vars::new(&var_rows, env.as_ref(), global.as_ref());
    for (k, v) in &plan.extra {
        vars.set("request", k, v);
    }
    let info = serde_json::json!({ "requestName": plan.req.name, "requestId": plan.req.id, "collection": plan.coll.as_ref().map(|c| c.name().to_string()) });

    // Requests that run first (their "set variable" rules give this one what it needs).
    if !plan.skip_pre_requests
        && let Some(coll) = &plan.coll
    {
        let ids: Vec<String> = folders.iter().flat_map(|f| f.pre_requests.iter()).chain(plan.req.pre_requests.iter()).filter(|id| **id != plan.req.id).cloned().collect();
        for id in ids.iter().take(10) {
            let Some(pre) = super::load_request(coll, id) else {
                done.logs.push(format!("✕ the request to send first ({id}) isn't in the collection any more"));
                continue;
            };
            let pf = chain(coll, &pre.id);
            let pf_inner: Vec<&Folder> = pf.iter().rev().copied().collect();
            let ready = send::prepare(&pre, &pf_inner, &vars);
            match send::send(ready).await {
                Ok(r) => {
                    done.logs.push(format!("→ {} {}: {}", pre.method, pre.name, r.status));
                    for (var, scope, value) in tests::set_vars(&pre.set_vars, &r) {
                        set(&mut vars, &mut done, env.as_ref(), &scope, &var, &value);
                    }
                    if let Some(f) = r.body_file {
                        std::fs::remove_file(f).ok();
                    }
                }
                Err(e) => done.logs.push(format!("✕ {} {}: {e}", pre.method, pre.name)),
            }
        }
    }

    // Pre-request scripts: the collection's, the folders', then the request's.
    let mut req = plan.req.clone();
    let mut rj = request_json(&req);
    for (i, f) in folders.iter().enumerate() {
        let label = if i == 0 { "collection pre-request script".to_string() } else { format!("{} pre-request script", f.name) };
        if let Some(changed) = run_script(&f.pre_script, &mut vars, &mut done, env.as_ref(), Some(rj.clone()), None, info.clone(), &label) {
            rj = changed;
        }
    }
    if let Some(changed) = run_script(&req.pre_script.clone(), &mut vars, &mut done, env.as_ref(), Some(rj.clone()), None, info.clone(), "pre-request script") {
        rj = changed;
    }
    apply_request_json(&mut req, &rj);

    let ready = send::prepare(&req, &inner_first, &vars);
    let secret: std::collections::HashSet<&str> = env.iter().chain(global.iter()).flat_map(|e| e.vars.iter()).filter(|r| r.flag).map(|r| r.key.as_str()).collect();
    for name in names_used(&req) {
        if let Some((value, scope)) = vars.get(&name) {
            let shown = if secret.contains(name.as_str()) { "••••••••".to_string() } else { value };
            done.vars_used.push((name, shown, scope));
        } else if name.starts_with('$') {
            done.vars_used.push((name, String::new(), "dynamic".into()));
        }
    }
    done.missing = ready.missing.clone();
    done.ready = Some(ready.clone());
    let resp = match send::send(ready).await {
        Ok(r) => r,
        Err(e) => {
            done.error = Some(format!("{e:#}"));
            return done;
        }
    };

    // Checks, variables from the answer, then post-response scripts.
    done.tests.extend(tests::run(&req.tests, &resp, &vars));
    for (var, scope, value) in tests::set_vars(&req.set_vars, &resp) {
        set(&mut vars, &mut done, env.as_ref(), &scope, &var, &value);
    }
    let has_post = folders.iter().any(|f| !f.post_script.trim().is_empty()) || !req.post_script.trim().is_empty();
    if has_post {
        let rjson = response_json(&resp);
        for (i, f) in folders.iter().enumerate() {
            let label = if i == 0 { "collection post-response script".to_string() } else { format!("{} post-response script", f.name) };
            run_script(&f.post_script, &mut vars, &mut done, env.as_ref(), Some(rj.clone()), Some(rjson.clone()), info.clone(), &label);
        }
        run_script(&req.post_script, &mut vars, &mut done, env.as_ref(), Some(rj.clone()), Some(rjson), info, "post-response script");
    }
    done.resp = Some(resp);
    done
}

/// Keeps what a run set: in the environment, the global one, or the collection.
pub fn keep_changes(changes: &[(String, String, String)], roots: &[std::path::PathBuf], env_id: Option<&str>, coll: Option<&mut Collection>) -> bool {
    if changes.is_empty() {
        return false;
    }
    let envs = super::environments(roots);
    let mut active = env_id.and_then(|id| envs.iter().find(|e| e.id == id).cloned());
    let mut global = envs.iter().find(|e| e.global).cloned();
    let mut coll = coll;
    let (mut env_changed, mut global_changed, mut coll_changed) = (false, false, false);
    let upsert = |rows: &mut Vec<super::Row>, k: &str, v: &str| match rows.iter_mut().find(|r| r.key == k) {
        Some(r) => r.value = v.to_string(),
        None => rows.push(super::Row::new(k, v)),
    };
    for (scope, k, v) in changes {
        match scope.as_str() {
            "environment" if active.is_some() => {
                upsert(&mut active.as_mut().unwrap().vars, k, v);
                env_changed = true;
            }
            "environment" | "global" => {
                if let Some(g) = global.as_mut() {
                    upsert(&mut g.vars, k, v);
                    global_changed = true;
                }
            }
            "collection" => {
                if let Some(c) = coll.as_deref_mut() {
                    upsert(&mut c.root.vars, k, v);
                    coll_changed = true;
                }
            }
            "unset" => {
                if let Some(e) = active.as_mut() {
                    e.vars.retain(|r| r.key != *k);
                    env_changed = true;
                }
            }
            _ => {}
        }
    }
    // Secret values were cleared in what was read: keep them as they are in the Keychain.
    let restore = |e: &Environment| {
        let mut e2 = super::with_secrets(e);
        for (scope, k, v) in changes {
            if scope == "environment" || scope == "global" {
                if let Some(r) = e2.vars.iter_mut().find(|r| r.key == *k) {
                    r.value = v.clone();
                }
            }
        }
        e2
    };
    if env_changed && let Some(e) = &active {
        super::save_environment(&restore(e)).ok();
    }
    if global_changed && let Some(g) = &global {
        super::save_environment(&restore(g)).ok();
    }
    if coll_changed && let Some(c) = coll {
        super::save_collection(c).ok();
    }
    true
}
