//! The engine check row and the readiness roll-up engines publish in their doctor report.

use serde_json::{json, Map, Value};

/// One engine check: `{id, required, status, detail}` plus an optional `location`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Stable machine id.
    pub id: String,
    /// Whether a non-`ok` status makes the engine not ready.
    pub required: bool,
    /// `ok`, or an app-defined failure word (`failed`, `missing`, `not_paired`, ...).
    pub status: String,
    /// What was found.
    pub detail: String,
    /// Where the checked item lives (separator-normalised by the caller), when it has a place.
    pub location: Option<String>,
}

impl Row {
    /// A row with an arbitrary status word.
    pub fn new(
        id: impl Into<String>,
        required: bool,
        status: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            required,
            status: status.into(),
            detail: detail.into(),
            location: None,
        }
    }

    /// A passing row.
    pub fn ok(id: &str, required: bool, detail: impl Into<String>) -> Self {
        Self::new(id, required, "ok", detail)
    }

    /// A failed row.
    pub fn failed(id: &str, required: bool, detail: impl Into<String>) -> Self {
        Self::new(id, required, "failed", detail)
    }

    /// A row for something absent on this computer.
    pub fn unavailable(id: &str, required: bool, detail: impl Into<String>) -> Self {
        Self::new(id, required, "unavailable", detail)
    }

    /// `ok` when `result` is `Ok`, `failed` otherwise; the text is the detail either way.
    pub fn from_result(id: &str, required: bool, result: Result<String, String>) -> Self {
        match result {
            Ok(detail) => Self::ok(id, required, detail),
            Err(detail) => Self::failed(id, required, detail),
        }
    }

    /// Attach a location.
    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Whether the status is `ok`.
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    /// The row as JSON; `location` is present only when set.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("required".into(), json!(self.required));
        map.insert("status".into(), json!(self.status));
        map.insert("detail".into(), json!(self.detail));
        if let Some(location) = &self.location {
            map.insert("location".into(), json!(location));
        }
        Value::Object(map)
    }
}

/// Whether every required row is `ok`.
pub fn all_required_ok(rows: &[Row]) -> bool {
    rows.iter().all(|r| !r.required || r.is_ok())
}

/// `{"status": "ready" | "not_ready", "checks": [...]}`: ready when every required row is `ok`.
pub fn readiness(rows: &[Row]) -> Value {
    json!({
        "status": if all_required_ok(rows) { "ready" } else { "not_ready" },
        "checks": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
    })
}

/// Whether a report's `readiness.status` is `ready`.
pub fn is_ready(report: &Value) -> bool {
    report["readiness"]["status"] == "ready"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_required_rows_decide_readiness() {
        let rows = vec![
            Row::ok("a", true, "fine"),
            Row::unavailable("b", false, "no gpu"),
        ];
        let r = readiness(&rows);
        assert_eq!(r["status"], "ready");
        assert!(is_ready(&json!({ "readiness": r })));
        let rows = vec![Row::failed("a", true, "broken")];
        assert_eq!(readiness(&rows)["status"], "not_ready");
    }

    #[test]
    fn location_is_optional_and_results_map_to_rows() {
        let row = Row::from_result("db", true, Ok("opens".into())).at("runtime/app/db");
        let json = row.to_json();
        assert_eq!(json["location"], "runtime/app/db");
        let bare = Row::from_result("db", true, Err("locked".into()));
        assert!(bare.to_json().get("location").is_none());
        assert_eq!(bare.status, "failed");
    }
}
