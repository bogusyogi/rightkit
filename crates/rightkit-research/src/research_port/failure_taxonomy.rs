//! Port of `research-core/failure_taxonomy.py`: deterministic Research
//! failure classification from real stage receipts.
//!
//! `payload` is represented as `serde_json::Value` to mirror the Python's
//! loosely-typed `dict` payloads across the many stages; field access below
//! mirrors `payload.get(...)` with the same defaults.

use std::collections::BTreeSet;

/// All classes the Python module can emit (`CLASSES`), kept for parity /
/// documentation and for callers that want to validate against the closed
/// set.
pub const CLASSES: &[&str] = &[
    "route_unresolved",
    "approval_pending",
    "gate_block",
    "budget_exceeded",
    "provider_unavailable",
    "primary_source_missing",
    "source_independent_lt_2",
    "retracted_doi_undisclosed",
    "retraction_unknown",
    "notebooklm_answer_ledger",
    "patch_hunk_overflow",
    "criminal_consumer_cross",
    "personal_medical_default",
    "citation_unbound",
    "citation_unsupported",
    "gap_unresolved",
];

/// Production entry point — same stage dispatch as `failure_taxonomy.classify`.
/// Returns a sorted, deduplicated list of classes.
pub fn classify(stage: &str, payload: &serde_json::Value) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let get = |key: &str| payload.get(key);
    let str_or_empty = |v: Option<&serde_json::Value>| -> String {
        v.and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default()
    };
    let bool_default_true =
        |v: Option<&serde_json::Value>| v.and_then(|v| v.as_bool()).unwrap_or(true);
    let bool_or_false =
        |v: Option<&serde_json::Value>| v.and_then(|v| v.as_bool()).unwrap_or(false);
    let array =
        |v: Option<&serde_json::Value>| v.and_then(|v| v.as_array()).cloned().unwrap_or_default();

    if stage == "route" {
        let route_truthy = match get("route") {
            None => false,
            Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => !s.is_empty(),
            Some(serde_json::Value::Object(m)) => !m.is_empty(),
            Some(serde_json::Value::Array(a)) => !a.is_empty(),
            Some(_) => true,
        };
        if !route_truthy {
            out.insert("route_unresolved".into());
        }
        let gate_verdicts = array(get("gate_verdicts"));
        if gate_verdicts
            .iter()
            .any(|v| v.get("verdict").and_then(|x| x.as_str()) == Some("ask"))
        {
            out.insert("approval_pending".into());
        }
        if gate_verdicts
            .iter()
            .any(|v| v.get("verdict").and_then(|x| x.as_str()) == Some("block"))
        {
            out.insert("gate_block".into());
        }
    }
    if stage == "budget" && !bool_default_true(get("ok")) {
        out.insert("budget_exceeded".into());
    }
    if stage == "provider" && get("status").and_then(|v| v.as_str()) == Some("unavailable") {
        out.insert("provider_unavailable".into());
    }
    if stage == "ledger" {
        for v in array(get("verdicts")) {
            let reasons: String = v
                .get("reasons")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let text = format!("{} {}", reasons, str_or_empty(v.get("reason")));
            if text.contains("primary-source") {
                out.insert("primary_source_missing".into());
            }
            if v.get("verdict").and_then(|x| x.as_str()) == Some("downgrade")
                && text.contains("independent")
            {
                out.insert("source_independent_lt_2".into());
            }
        }
    }
    if stage == "retraction" {
        for row in array(get("results")) {
            let retracted = bool_or_false(row.get("retracted"));
            let disclosed = bool_or_false(row.get("disclosed"));
            if retracted && !disclosed {
                out.insert("retracted_doi_undisclosed".into());
            }
            if row.get("status").and_then(|x| x.as_str()) == Some("unknown") {
                out.insert("retraction_unknown".into());
            }
        }
    }
    if stage == "notebooklm"
        && bool_or_false(get("promoted_to_ledger"))
        && !bool_or_false(get("underlying_opened"))
    {
        out.insert("notebooklm_answer_ledger".into());
    }
    if stage == "patch"
        && !bool_or_false(get("ok"))
        && str_or_empty(get("reason")).to_lowercase().contains("hunk")
    {
        out.insert("patch_hunk_overflow".into());
    }
    if stage == "domain" {
        if bool_or_false(get("criminal")) && bool_or_false(get("consumer_refs_loaded")) {
            out.insert("criminal_consumer_cross".into());
        }
        if get("patient_kind").and_then(|x| x.as_str()) == Some("anonymous")
            && bool_or_false(get("history_loaded"))
        {
            out.insert("personal_medical_default".into());
        }
    }
    if stage == "citecheck" {
        if bool_or_false(get("unbound")) {
            out.insert("citation_unbound".into());
        }
        if bool_or_false(get("unsupported")) || bool_or_false(get("dangling")) {
            out.insert("citation_unsupported".into());
        }
    }
    if stage == "gap" && !bool_default_true(get("complete")) {
        out.insert("gap_unresolved".into());
    }
    out.into_iter().collect()
}
