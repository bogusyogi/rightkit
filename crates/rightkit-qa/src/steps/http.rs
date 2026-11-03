//! The `http` step: one plain-HTTP request to a daemon's public listener, the way a browser or a
//! webhook sender reaches it. Loopback only (see [`crate::http`]); no redirect is followed.
//!
//! Fields:
//! * `id` (default `daemon`): the daemon started by `start_daemon` with `public_port`, or `port`: a
//!   loopback port, for a listener the scenario did not start. Give one of them.
//! * `method` (default `GET`, upper-case letters), `path` (starts with `/`, query allowed).
//! * `headers`: a table of strings. A `Host` header replaces the default (`localhost`).
//! * at most one body: `body` (a string is sent as written, any other JSON value as compact JSON) or
//!   `form` (a table of strings, urlencoded, with `Content-Type: application/x-www-form-urlencoded`).
//! * `sign = { secret, header, prefix }`: adds an HMAC-SHA256 hex signature of the final body as a
//!   header (defaults `x-hub-signature-256` and `sha256=`), so a scenario needs no precomputed
//!   signature; a forged one is a different `secret`.
//! * `timeout_s` (default 10).
//! * `expect_status`: a status the answer must have, or the step fails with the status and the head
//!   of the body: a number (`403`), a class (`"2xx"`), or a list of either (`[200, "3xx"]`). Without
//!   it the step passes whatever the status is and a scenario asserts `status` itself with
//!   `[[step.expect]]` (a refusal check does that); give `expect_status` when a wrong status should
//!   fail at the step, before any later expectation reads a body that is not there (rightkit#942).
//!
//! Result `{ status, headers, body, json }`: `headers` has lowercased names, `body` is the text and
//! `json` the body parsed when it is JSON, else null. A body that needs an app-specific encoding (a
//! signed form field, say) is built by a plugin step and passed here as `body = "${saved.body}"`.
use super::{only, required, string_table, text, uint};
use crate::http as qa_http;
use crate::scenario::{pv, Ctx};
use crate::util::{err, Error, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

pub(super) fn run(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    let addr = target(ctx, p)?;
    run_with(addr, p)
}

/// The listener the step talks to.
fn target(ctx: &Ctx, p: &Map<String, Value>) -> Result<SocketAddr> {
    match (text(p, "id")?, uint(p, "port")?) {
        (Some(_), Some(_)) => err("http takes `id` or `port`, not both"),
        (None, Some(port)) => u16::try_from(port)
            .map(qa_http::loopback)
            .map_err(|_| Error("`port` must be 0 to 65535".into())),
        (id, None) => {
            let id = id.unwrap_or_else(|| "daemon".to_string());
            let daemon = ctx.steps.daemons.get(&id).ok_or_else(|| {
                Error(format!(
                    "no daemon `{id}` is running: add a start_daemon step first"
                ))
            })?;
            daemon.public.ok_or_else(|| {
                Error(format!(
                    "daemon `{id}` has no public listener: add `public_port = 0` to its start_daemon step"
                ))
            })
        }
    }
}

/// `application/x-www-form-urlencoded` of `pairs`: unreserved bytes and `*` as they are, a space as
/// `+`, every other byte `%XX`.
pub(crate) fn form_encode(pairs: &[(String, String)]) -> String {
    fn put(out: &mut String, text: &str) {
        for b in text.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
    }
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        put(&mut out, k);
        out.push('=');
        put(&mut out, v);
    }
    out
}

pub(super) fn run_with(addr: SocketAddr, p: &Map<String, Value>) -> Result<Value> {
    only(
        p,
        "http",
        &[
            "id",
            "port",
            "method",
            "path",
            "headers",
            "body",
            "form",
            "sign",
            "timeout_s",
            "expect_status",
        ],
    )?;
    let method = text(p, "method")?
        .unwrap_or_else(|| "GET".to_string())
        .to_ascii_uppercase();
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return err(format!("`method` {method} must be upper-case letters"));
    }
    let path = required(p, "http", "path")?;
    if !path.starts_with('/') || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return err("`path` must start with / and hold no whitespace");
    }
    let mut headers: Vec<(String, String)> = string_table(p, "headers")?;
    if pv(p, "body").is_some() && pv(p, "form").is_some() {
        return err("give at most one of `body` and `form`");
    }
    let mut content_type: Option<&str> = None;
    let body: Option<String> = match (pv(p, "body"), pv(p, "form")) {
        (Some(Value::String(s)), _) => Some(s.clone()),
        (Some(other), _) => Some(other.to_string()),
        (None, Some(_)) => {
            content_type = Some("application/x-www-form-urlencoded");
            Some(form_encode(&string_table(p, "form")?))
        }
        (None, None) => None,
    };
    if let Some(sign) = pv(p, "sign") {
        let table = sign
            .as_object()
            .ok_or_else(|| Error("`sign` must be a table".into()))?;
        if let Some(name) = table
            .keys()
            .find(|k| !["secret", "header", "prefix"].contains(&k.as_str()))
        {
            return err(format!(
                "`sign.{name}` is unknown (fields: secret, header, prefix)"
            ));
        }
        let pick = |key: &str| -> Result<Option<String>> {
            match table.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => err(format!("`sign.{key}` must be a string")),
            }
        };
        let secret = pick("secret")?.ok_or_else(|| Error("`sign` needs string `secret`".into()))?;
        let header = pick("header")?.unwrap_or_else(|| "x-hub-signature-256".to_string());
        let prefix = pick("prefix")?.unwrap_or_else(|| "sha256=".to_string());
        let signature = crate::util::hmac_sha256_hex(
            secret.as_bytes(),
            body.as_deref().unwrap_or("").as_bytes(),
        );
        headers.push((header, format!("{prefix}{signature}")));
    }
    if let Some(ct) = content_type {
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        {
            headers.push(("Content-Type".to_string(), ct.to_string()));
        }
    }
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        headers.push(("Host".to_string(), "localhost".to_string()));
    }
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let sent = (body.is_some() || method != "GET").then(|| body.clone().unwrap_or_default());
    let timeout = pv(p, "timeout_s")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| Duration::from_secs_f64(s.min(300.0)))
        .unwrap_or(Duration::from_secs(10));
    let answer = qa_http::request_with(addr, &method, &path, None, &refs, sent.as_deref(), timeout)
        .map_err(|e| Error(format!("request to the listener {addr}: {}", e.0)))?;
    let body_text = answer.text();
    if let Some(want) = pv(p, "expect_status") {
        let accepts = |w: &Value| -> Result<bool> {
            match w {
                Value::Number(n) => Ok(n.as_u64() == Some(u64::from(answer.status))),
                Value::String(c) if c.len() == 3 && c.ends_with("xx") && c.as_bytes()[0].is_ascii_digit() => {
                    Ok(u64::from(c.as_bytes()[0] - b'0') == u64::from(answer.status) / 100)
                }
                _ => err("`expect_status` must be a status number, a class such as \"2xx\", or a list of them"),
            }
        };
        let ok = match want {
            Value::Array(list) if !list.is_empty() => {
                let mut any = false;
                for w in list {
                    any |= accepts(w)?;
                }
                any
            }
            other => accepts(other)?,
        };
        if !ok {
            return err(format!(
                "the listener {addr} answered {} to {method} {path}, not {want}: {}",
                answer.status,
                crate::util::tail(body_text.trim(), 300)
            ));
        }
    }
    let mut answer_headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in &answer.headers {
        answer_headers.insert(name.to_ascii_lowercase(), value.clone());
    }
    let parsed = serde_json::from_str::<Value>(body_text.trim()).unwrap_or(Value::Null);
    Ok(json!({
        "status": answer.status,
        "headers": answer_headers,
        "body": body_text,
        "json": parsed,
    }))
}
