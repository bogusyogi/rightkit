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
