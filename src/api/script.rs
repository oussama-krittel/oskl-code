//! Pre-request and post-response scripts, in JavaScript (QuickJS, a small engine made for
//! each run and dropped after: a few MB while it runs, nothing after).
//!
//! What scripts see is `tc` (and `pm`, the same, for scripts written for Postman):
//! `tc.request` (method, url, headers, body: changeable before sending), `tc.response`
//! (status, headers, body, json(), time), `tc.getVar/setVar/setEnvVar/setGlobalVar`,
//! `tc.test(name, fn)`, `expect(x).to.equal(y)`-style checks, `console.log`.

use super::tests::Outcome;
use rquickjs::{Context, Runtime};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a script may run, and how much memory it may use.
pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const MEMORY: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptResult {
    pub logs: Vec<String>,
    /// (scope, name, value): request, environment, global, collection. An empty value with
    /// scope "unset" removes it.
    pub vars: Vec<(String, String, String)>,
    pub tests: Vec<Outcome>,
    /// The request as the script left it (pre-request scripts).
    pub request: Option<serde_json::Value>,
    pub error: Option<String>,
}

const PRELUDE: &str = r#"
(function(){
  const out = { logs: [], vars: [], tests: [] };
  globalThis.__out = out;
  const show = (a) => typeof a === 'string' ? a : (() => { try { return JSON.stringify(a, null, 2); } catch (e) { return String(a); } })();
  const log = (...a) => out.logs.push(a.map(show).join(' '));
  globalThis.console = { log, info: log, warn: log, error: log, debug: log };
  const vars = __in.vars;
  const setter = (scope) => (k, v) => { const s = v === undefined || v === null ? '' : String(v); vars[k] = s; out.vars.push([scope, String(k), s]); };
  const req = __in.request;
  const hdrs = (list) => ({
    get: (k) => { const h = list.find(x => x[0].toLowerCase() === String(k).toLowerCase()); return h ? h[1] : undefined; },
    set: (k, v) => { const i = list.findIndex(x => x[0].toLowerCase() === String(k).toLowerCase()); if (i >= 0) list[i][1] = String(v); else list.push([String(k), String(v)]); },
    add: (o) => { list.push([String(o.key), String(o.value)]); },
    upsert: (o) => { const i = list.findIndex(x => x[0].toLowerCase() === String(o.key).toLowerCase()); if (i >= 0) list[i][1] = String(o.value); else list.push([String(o.key), String(o.value)]); },
    remove: (k) => { const i = list.findIndex(x => x[0].toLowerCase() === String(k).toLowerCase()); if (i >= 0) list.splice(i, 1); },
    has: (k) => list.some(x => x[0].toLowerCase() === String(k).toLowerCase()),
    toObject: () => Object.fromEntries(list),
    all: () => list,
  });
  if (req) {
    req.headers = req.headers || [];
    const h = hdrs(req.headers);
    Object.defineProperty(req, 'header', { value: h, enumerable: false });
    req.setHeader = h.set; req.getHeader = h.get; req.removeHeader = h.remove;
  }
  let resp = __in.response;
  if (resp) {
    const h = hdrs(resp.headers);
    resp.json = () => JSON.parse(resp.body);
    resp.text = () => resp.body;
    resp.getHeader = h.get;
    resp.headersObject = h.toObject();
    resp.code = resp.status;
    resp.responseTime = resp.time;
    resp.to = { have: { status: (s) => expect(resp.status).to.equal(s) } };
  }
  class Fail extends Error {}
  function expect(actual) {
    const fail = (msg) => { throw new Fail(msg); };
    const eq = (a, b) => { try { return JSON.stringify(a) === JSON.stringify(b); } catch (e) { return a === b; } };
    const make = (neg) => {
      const ok = (cond, msg) => { if (neg ? cond : !cond) fail((neg ? 'not: ' : '') + msg); return chain; };
      const chain = {
        equal: (v) => ok(actual === v, `expected ${show(actual)} to equal ${show(v)}`),
        equals: (v) => ok(actual === v, `expected ${show(actual)} to equal ${show(v)}`),
        eql: (v) => ok(eq(actual, v), `expected ${show(actual)} to deeply equal ${show(v)}`),
        above: (v) => ok(actual > v, `expected ${show(actual)} to be above ${v}`),
        below: (v) => ok(actual < v, `expected ${show(actual)} to be below ${v}`),
        least: (v) => ok(actual >= v, `expected ${show(actual)} to be at least ${v}`),
        most: (v) => ok(actual <= v, `expected ${show(actual)} to be at most ${v}`),
        include: (v) => ok(typeof actual === 'string' ? actual.includes(v) : Array.isArray(actual) ? actual.some(x => eq(x, v)) : actual && typeof actual === 'object' && Object.keys(v).every(k => eq(actual[k], v[k])), `expected ${show(actual)} to include ${show(v)}`),
        contain: (v) => chain.include(v),
        property: (k, v) => ok(actual != null && k in Object(actual) && (v === undefined || eq(actual[k], v)), `expected ${show(actual)} to have property ${k}`),
        lengthOf: (n) => ok(actual != null && actual.length === n, `expected length ${actual == null ? 'none' : actual.length} to be ${n}`),
        match: (re) => ok(re.test(String(actual)), `expected ${show(actual)} to match ${re}`),
        a: (t) => ok((Array.isArray(actual) ? 'array' : actual === null ? 'null' : typeof actual) === String(t).toLowerCase(), `expected ${show(actual)} to be a ${t}`),
        an: (t) => chain.a(t),
        oneOf: (l) => ok(l.some(x => eq(x, actual)), `expected ${show(actual)} to be one of ${show(l)}`),
      };
      Object.defineProperty(chain, 'ok', { get: () => ok(!!actual, `expected ${show(actual)} to be truthy`) });
      Object.defineProperty(chain, 'true', { get: () => ok(actual === true, `expected ${show(actual)} to be true`) });
      Object.defineProperty(chain, 'false', { get: () => ok(actual === false, `expected ${show(actual)} to be false`) });
      Object.defineProperty(chain, 'null', { get: () => ok(actual === null, `expected ${show(actual)} to be null`) });
      Object.defineProperty(chain, 'undefined', { get: () => ok(actual === undefined, `expected ${show(actual)} to be undefined`) });
      Object.defineProperty(chain, 'empty', { get: () => ok(actual == null || (typeof actual === 'object' ? Object.keys(actual).length === 0 : String(actual).length === 0), `expected ${show(actual)} to be empty`) });
      Object.defineProperty(chain, 'exist', { get: () => ok(actual !== undefined && actual !== null, `expected a value`) });
      for (const w of ['to', 'be', 'been', 'is', 'that', 'which', 'and', 'has', 'have', 'with', 'at', 'of', 'same', 'deep', 'does']) Object.defineProperty(chain, w, { get: () => chain });
      chain.gt = chain.above; chain.lt = chain.below; chain.gte = chain.least; chain.lte = chain.most;
      chain.length = chain.lengthOf; chain.includes = chain.include; chain.contains = chain.include;
      return chain;
    };
    const c = make(false);
    Object.defineProperty(c, 'not', { get: () => make(true) });
    return c;
  }
  globalThis.expect = expect;
  const test = (name, fn) => {
    try { fn(); out.tests.push([String(name), true, '']); }
    catch (e) { out.tests.push([String(name), false, e && e.message ? e.message : String(e)]); }
  };
  const b64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  globalThis.btoa = (s) => { s = String(s); let o = ''; for (let i = 0; i < s.length; i += 3) { const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2); const n = (a << 16) | ((b || 0) << 8) | (c || 0); o += b64[(n >> 18) & 63] + b64[(n >> 12) & 63] + (isNaN(b) ? '=' : b64[(n >> 6) & 63]) + (isNaN(c) ? '=' : b64[n & 63]); } return o; };
  globalThis.atob = (s) => { s = String(s).replace(/=+$/, ''); let o = '', bits = 0, n = 0; for (const ch of s) { n = (n << 6) | b64.indexOf(ch); bits += 6; if (bits >= 8) { bits -= 8; o += String.fromCharCode((n >> bits) & 255); } } return o; };
  const tc = {
    request: req, response: resp,
    getVar: (k) => vars[k], variables: { get: (k) => vars[k], set: setter('request'), has: (k) => k in vars },
    setVar: setter('request'), setEnvVar: setter('environment'), setGlobalVar: setter('global'), setCollectionVar: setter('collection'),
    environment: { get: (k) => vars[k], set: setter('environment'), unset: (k) => out.vars.push(['unset', String(k), '']) },
    globals: { get: (k) => vars[k], set: setter('global') },
    collectionVariables: { get: (k) => vars[k], set: setter('collection') },
    test, expect, info: __in.info,
    uuid: () => 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, c => { const r = Math.random() * 16 | 0; return (c === 'x' ? r : (r & 3 | 8)).toString(16); }),
  };
  globalThis.tc = tc; globalThis.pm = tc;
})();
"#;

/// Runs a script. `request` is what's about to be sent (or was sent), as
/// `{method, url, headers: [[k, v]], body}`; `response` the answer (post-response scripts).
pub fn run(source: &str, vars: &HashMap<String, String>, request: Option<serde_json::Value>, response: Option<serde_json::Value>, info: serde_json::Value) -> ScriptResult {
    if source.trim().is_empty() {
        return ScriptResult::default();
    }
    let input = serde_json::json!({ "vars": vars, "request": request, "response": response, "info": info });
    match eval(source, &input) {
        Ok(r) => r,
        Err(e) => ScriptResult { error: Some(e), ..Default::default() },
    }
}

fn eval(source: &str, input: &serde_json::Value) -> Result<ScriptResult, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(MEMORY);
    rt.set_max_stack_size(512 * 1024);
    let start = Instant::now();
    rt.set_interrupt_handler(Some(Box::new(move || start.elapsed() > TIMEOUT)));
    let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
    ctx.with(|ctx| {
        let fail = |ctx: &rquickjs::Ctx, e: rquickjs::Error| -> String {
            if matches!(e, rquickjs::Error::Exception) {
                let v = ctx.catch();
                if let Some(ex) = v.as_exception() {
                    let msg = ex.message().unwrap_or_default();
                    let line = ex.stack().and_then(|s| s.lines().find(|l| l.contains("script")).map(|l| l.trim().to_string()));
                    return match line {
                        Some(l) => format!("{msg} ({l})"),
                        None => msg,
                    };
                }
                if start.elapsed() > TIMEOUT {
                    return format!("the script ran over {}s", TIMEOUT.as_secs());
                }
                return format!("{v:?}");
            }
            e.to_string()
        };
        let set_input = format!("globalThis.__in = {};", input);
        ctx.eval::<(), _>(set_input).map_err(|e| fail(&ctx, e))?;
        ctx.eval::<(), _>(PRELUDE).map_err(|e| fail(&ctx, e))?;
        let mut opts = rquickjs::context::EvalOptions::default();
        opts.global = true;
        opts.strict = false;
        let script_err = ctx.eval_with_options::<(), _>(format!("(function(){{\n{source}\n}})();"), opts).err().map(|e| fail(&ctx, e));
        let out: String = ctx.eval("JSON.stringify({ out: globalThis.__out, request: globalThis.__in.request })").map_err(|e| fail(&ctx, e))?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        let o = &v["out"];
        let strs = |v: &serde_json::Value| v.as_array().cloned().unwrap_or_default();
        let s = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
        Ok(ScriptResult {
            logs: strs(&o["logs"]).iter().map(s).collect(),
            vars: strs(&o["vars"]).iter().map(|x| (s(&x[0]), s(&x[1]), s(&x[2]))).collect(),
            tests: strs(&o["tests"]).iter().map(|x| Outcome { name: s(&x[0]), pass: x[1].as_bool().unwrap_or(false), detail: s(&x[2]) }).collect(),
            request: Some(v["request"].clone()).filter(|r| !r.is_null()),
            error: script_err,
        })
    })
}

#[cfg(test)]
mod scripts {
    use super::*;

    #[test]
    fn scripts_test_set_variables_and_change_the_request() {
        let vars: HashMap<String, String> = [("base".to_string(), "http://x".to_string())].into();
        let req = serde_json::json!({ "method": "GET", "url": "http://x/a", "headers": [["Accept", "*/*"]], "body": "" });
        let pre = run("tc.request.setHeader('X-Sig', btoa('ab')); tc.request.url += '?t=1'; tc.setEnvVar('seen', tc.getVar('base')); console.log('hi', {a: 1});", &vars, Some(req), None, serde_json::json!({}));
        assert_eq!(pre.error, None);
        let r = pre.request.unwrap();
        assert_eq!(r["url"], "http://x/a?t=1");
        assert_eq!(r["headers"][1], serde_json::json!(["X-Sig", "YWI="]));
        assert_eq!(pre.vars, [("environment".to_string(), "seen".to_string(), "http://x".to_string())]);
        assert_eq!(pre.logs[0], "hi {\n  \"a\": 1\n}");

        let resp = serde_json::json!({ "status": 200, "headers": [["Content-Type", "application/json"]], "body": "{\"items\":[1,2,3],\"name\":\"oskl\"}", "time": 12 });
        let post = run(
            r#"
            pm.test("status ok", () => pm.response.to.have.status(200));
            tc.test("three items", () => expect(tc.response.json().items).to.have.lengthOf(3));
            tc.test("name", () => expect(tc.response.json().name).to.not.equal("oskl"));
            tc.test("type", () => expect(tc.response.json()).to.be.an("object").and.have.property("name", "oskl"));
            tc.setVar("n", tc.response.json().items.length);
            "#,
            &vars,
            None,
            Some(resp),
            serde_json::json!({}),
        );
        assert_eq!(post.error, None);
        assert_eq!(post.tests.iter().map(|t| t.pass).collect::<Vec<_>>(), [true, true, false, true], "{:?}", post.tests);
        assert!(post.tests[2].detail.contains("not: expected"), "{}", post.tests[2].detail);
        assert_eq!(post.vars[0].2, "3");
    }

    #[test]
    fn errors_endless_loops_and_memory_hogs_stop() {
        let v = HashMap::new();
        let e = run("nope();", &v, None, None, serde_json::json!({}));
        assert!(e.error.unwrap().contains("nope"), "the error says what");
        let start = Instant::now();
        let e = run("while (true) {}", &v, None, None, serde_json::json!({}));
        assert!(e.error.is_some() && start.elapsed() < TIMEOUT + Duration::from_secs(2));
        let e = run("let a = []; while (true) a.push('x'.repeat(1e6));", &v, None, None, serde_json::json!({}));
        assert!(e.error.is_some(), "out of memory, not the app");
    }
}
