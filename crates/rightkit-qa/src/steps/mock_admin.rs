//! The `mock_admin` step: one request to an `/__admin/` path of the scenario's mock, to steer it
//! (a fault, a canned answer) without going through the engine. Platform calls belong to the engine;
//! this step reaches only the mock's admin surface.
//!
//! Fields: `id` (default `mock`), `method` (default `GET`), `path` (must start with `/__admin/`),
//! `body` (a JSON value, sent compact; `${...}` is substituted as in every step), `timeout_s`
//! (default 10). The mock is found, in order, as an in-process mock started by `mock_start` with
//! that `id`, then as the scenario variable `${<id>_url}` (and `${mock_url}` for the default id) that
//! the plugin's setup or a daemon-style step published for a mock it asked the runner to start.
//! Loopback only. Result `{ status, json }`, `json` being the body or null.
use super::{only, required, text};
use crate::http as qa_http;
use crate::scenario::{pv, Ctx};
use crate::util::{err, Error, Result};
use serde_json::{json, Map, Value};
use std::time::Duration;

pub(super) fn run(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    only(
        p,
        "mock_admin",
        &["id", "method", "path", "body", "timeout_s"],
    )?;
    let id = text(p, "id")?.unwrap_or_else(|| "mock".to_string());
    let base = if let Some(m) = ctx.mocks.get(&id) {
        m.base.clone()
    } else {
        let mut keys = vec![format!("{id}_url")];
        if id == "mock" {
            keys.push("mock_url".to_string());
        }
        keys.iter()
            .find_map(|k| ctx.vars.get(k).and_then(Value::as_str).map(str::to_string))
            .ok_or_else(|| {
                Error(format!(
                    "mock_admin needs a running mock `{id}`: start one with mock_start, or have the plugin's setup start it and set `{}`",
                    keys[0]
                ))
            })?
    };
    run_with(port_of(&base)?, p)
}

/// The loopback port of a mock's base URL (`http://127.0.0.1:PORT`, `http://localhost:PORT`).
pub(super) fn port_of(base: &str) -> Result<u16> {
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| Error(format!("the mock url {base} is not a plain http URL")))?;
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| Error(format!("the mock url {base} has no port")))?;
    if host != "127.0.0.1" && host != "localhost" {
        return err(format!("the mock url {base} is not on loopback"));
    }
    port.parse::<u16>()
        .map_err(|_| Error(format!("the mock url {base} has no usable port")))
}

pub(super) fn run_with(port: u16, p: &Map<String, Value>) -> Result<Value> {
    only(
        p,
        "mock_admin",
        &["id", "method", "path", "body", "timeout_s"],
    )?;
    let method = text(p, "method")?.unwrap_or_else(|| "GET".to_string());
    let path = required(p, "mock_admin", "path")?;
    if !path.starts_with("/__admin/")
        || path.contains("..")
        || path.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return err(format!(
            "mock_admin reaches only /__admin/ paths, got {path}: platform calls belong to the engine"
        ));
    }
    let body = pv(p, "body").map(Value::to_string);
    let timeout = pv(p, "timeout_s")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| Duration::from_secs_f64(s.min(300.0)))
        .unwrap_or(Duration::from_secs(10));
    let response = qa_http::request(
        qa_http::loopback(port),
        &method,
        &path,
        None,
        body.as_deref(),
        timeout,
    )?;
    let parsed = serde_json::from_str::<Value>(response.text().trim()).unwrap_or(Value::Null);
    Ok(json!({ "status": response.status, "json": parsed }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockServer, Route};

    fn params(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn an_admin_request_reaches_the_mock_and_returns_status_and_json() {
        let m = MockServer::start(vec![Route {
            method: Some("POST".into()),
            path: "/__admin/faults".into(),
            status: 200,
            body: json!({"queued": 1}),
            content_type: None,
        }])
        .unwrap();
        let port = port_of(&m.base).unwrap();
        let r = run_with(
            port,
            &params(json!({
                "method": "POST", "path": "/__admin/faults",
                "body": {"platform": "x", "count": 1},
            })),
        )
        .unwrap();
        assert_eq!(r["status"], 200);
        assert_eq!(r["json"]["queued"], 1);
        let seen = m.seen();
        assert_eq!(seen[0].json(), json!({"platform": "x", "count": 1}));
    }

    #[test]
    fn only_admin_paths_are_reachable_and_unknown_fields_are_refused() {
        for bad in [
            json!({"path": "/v1/messages"}),
            json!({"path": "/__admin/../v1"}),
            json!({"path": "/__admin/a b"}),
            json!({"path": "/__admin/x", "typo": true}),
            json!({}),
        ] {
            assert!(run_with(1, &params(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_base_url_must_be_plain_http_on_loopback_with_a_port() {
        assert_eq!(port_of("http://127.0.0.1:4455").unwrap(), 4455);
        assert_eq!(port_of("http://localhost:80/x").unwrap(), 80);
        for bad in [
            "https://127.0.0.1:1",
            "http://example.com:80",
            "http://127.0.0.1",
            "http://127.0.0.1:x",
        ] {
            assert!(port_of(bad).is_err(), "{bad}");
        }
    }
}
