//! The ONE L1 serialization contract, wire marker `L1C1` (train = eval = app).
//!
//! This is the Rust mirror of `scripts/local-model-training/serialize_l1.py`
//! (the authoritative reference) rendering the model input at LOCAL
//! INFERENCE time so train = serve. Every consumer (packet compiler, JS
//! parity fixture, this Rust app payload builder) must render the model
//! input through a byte-identical implementation; this module's unit test
//! asserts Rust matches the frozen fixture
//! (`scripts/local-model-training/l1c1_fixtures.jsonl`).
//!
//! Wire format (one user turn, no system message):
//!
//! ```text
//! L1C1
//! {"a":"OneNote","w":"Plan","f":"...","s":"...","r":"...","v":["..."],"x":[["heard","herd"]],"t":"raw transcript"}
//! ```
//!
//! Keys, fixed order: `k` app kind, `a` app, `w` window, `f` field text,
//! `s` selected text, `r` writing region, `v` vocabulary, `x` sound-alike
//! pairs, `t` transcript. Empty optional keys are omitted; `t` is always
//! present and last. Compact JSON separators, standard escaping, UTF-8 (no
//! ASCII escaping), no new tokenizer tokens.
//!
//! `k` (app kind) is DATA read from the training corpus row and is never
//! derived here — see `serialize_l1.py`'s design note. The
//! runtime `PolishContext` (`crate::l3_cleanup::PolishContext`) is the flat
//! local-inference shape and carries no app-kind field, so this serializer
//! always omits `k` at runtime.

use serde::Serialize;

use crate::l3_cleanup::PolishContext;

const WIRE_MARKER: &str = "L1C1";

/// Trim; treat a `None`/all-whitespace value as absent (mirrors Python's
/// `_clean` + "include scalar only if non-empty").
fn clean_opt(value: &Option<String>) -> Option<String> {
    let trimmed = value.as_deref().unwrap_or("").trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[derive(Serialize)]
struct L1Envelope<'a> {
    #[serde(rename = "k", skip_serializing_if = "Option::is_none")]
    k: Option<String>,
    #[serde(rename = "a", skip_serializing_if = "Option::is_none")]
    a: Option<String>,
    #[serde(rename = "w", skip_serializing_if = "Option::is_none")]
    w: Option<String>,
    #[serde(rename = "f", skip_serializing_if = "Option::is_none")]
    f: Option<String>,
    #[serde(rename = "s", skip_serializing_if = "Option::is_none")]
    s: Option<String>,
    #[serde(rename = "r", skip_serializing_if = "Option::is_none")]
    r: Option<String>,
    #[serde(rename = "v", skip_serializing_if = "Vec::is_empty")]
    v: Vec<String>,
    #[serde(rename = "x", skip_serializing_if = "Vec::is_empty")]
    x: Vec<Vec<String>>,
    #[serde(rename = "t")]
    t: &'a str,
}

/// Render the L1C1 wire payload for local inference from the live
/// `PolishContext` plus the raw transcript. Byte-identical to
/// `serialize_l1.py::serialize_l1` for every input the flat runtime context
/// can represent (`k` / app-kind is data-only and never present here).
pub fn serialize_l1(context: &PolishContext, transcript: &str) -> String {
    let v: Vec<String> = context
        .vocabulary
        .iter()
        .map(|term| term.trim().to_string())
        .filter(|term| !term.is_empty())
        .collect();

    let mut x: Vec<Vec<String>> = Vec::new();
    for (term, alts) in &context.sound_alikes {
        let term_clean = term.trim();
        if term_clean.is_empty() {
            continue;
        }
        for alt in alts {
            let alt_clean = alt.trim();
            if alt_clean.is_empty() {
                continue;
            }
            x.push(vec![term_clean.to_string(), alt_clean.to_string()]);
        }
    }

    let envelope = L1Envelope {
        k: None,
        a: clean_opt(&context.app_name),
        w: clean_opt(&context.window_title),
        f: clean_opt(&context.field_text),
        s: clean_opt(&context.selected_text),
        r: clean_opt(&context.writing_region),
        v,
        x,
        t: transcript,
    };

    let body = serde_json::to_string(&envelope)
        .expect("L1C1 envelope is plain data and cannot fail to serialize");
    format!("{WIRE_MARKER}\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use std::path::Path;

    /// Keys that only exist in the July-27 nested `bounded_context` schema
    /// (or are otherwise not representable by the flat runtime
    /// `PolishContext`). Fixture rows carrying any of these are skipped —
    /// see the task contract in `serialize_l1.py`'s docstring for the two
    /// schemas this normalizes.
    const NESTED_ONLY_KEYS: [&str; 6] = [
        "app_kind",
        "focused_control",
        "app_window_title",
        "document_selection_excerpt",
        "recent_repaired_text",
        "context_resolution_terms",
    ];

    fn nested_only_keys_present(bounded_context: &Value) -> bool {
        NESTED_ONLY_KEYS
            .iter()
            .any(|key| bounded_context.get(key).is_some())
    }

    fn build_context_from_flat(bounded_context: &Value) -> PolishContext {
        let get_str = |key: &str| -> Option<String> {
            bounded_context
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };

        let vocabulary: Vec<String> = bounded_context
            .get("vocabulary")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let sound_alikes: Vec<(String, Vec<String>)> = bounded_context
            .get("sound_alikes")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|pair| {
                        let pair_arr = pair.as_array()?;
                        if pair_arr.len() != 2 {
                            return None;
                        }
                        let term = pair_arr[0].as_str()?.to_string();
                        let alts: Vec<String> = if let Some(alt_arr) = pair_arr[1].as_array() {
                            alt_arr
                                .iter()
                                .filter_map(|a| a.as_str().map(str::to_string))
                                .collect()
                        } else if let Some(alt_str) = pair_arr[1].as_str() {
                            vec![alt_str.to_string()]
                        } else {
                            Vec::new()
                        };
                        Some((term, alts))
                    })
                    .collect()
            })
            .unwrap_or_default();

        PolishContext {
            app_name: get_str("app_name"),
            window_title: get_str("window_title"),
            field_text: get_str("field_text"),
            selected_text: get_str("selected_text"),
            field_context_available: false,
            vocabulary,
            writing_region: get_str("writing_region"),
            sound_alikes,
        }
    }

    #[test]
    fn matches_frozen_python_fixture_for_flat_rows() {
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/l1c1_fixtures.jsonl");
        let content = fs::read_to_string(&fixture_path)
            .unwrap_or_else(|e| panic!("failed to read fixture {fixture_path:?}: {e}"));

        let mut flat_rows_checked = 0usize;
        let empty_bc = Value::Object(serde_json::Map::new());

        for (idx, line) in content.lines().enumerate() {
            let line_no = idx + 1;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let fixture: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {line_no}: invalid JSON: {e}"));

            let row = &fixture["row"];
            let expected = fixture["expected"]
                .as_str()
                .unwrap_or_else(|| panic!("line {line_no}: missing `expected`"));
            let transcript = row["input_text"]
                .as_str()
                .unwrap_or_else(|| panic!("line {line_no}: missing `input_text`"));

            let bounded_context = row.get("bounded_context").unwrap_or(&empty_bc);
            if nested_only_keys_present(bounded_context) {
                // Nested-schema-only row — not representable by the flat
                // runtime PolishContext. Skip per the task contract.
                continue;
            }

            let context = build_context_from_flat(bounded_context);
            let actual = serialize_l1(&context, transcript);

            assert_eq!(
                actual, expected,
                "line {line_no} mismatch for input {transcript:?}"
            );

            flat_rows_checked += 1;
        }

        assert!(
            flat_rows_checked >= 8,
            "expected at least 8 flat fixture rows exercised, got {flat_rows_checked} \
             (a silent skip-everything bug would pass vacuously without this floor)"
        );
    }
}
