// Mechanism 8 (book §P2-3, 3-8): lenient summary parsing.
//
// Sibling include! module of compactor.rs -- shares its module scope, no
// mod/use headers of its own. `repos/cli/goose/.../structured.rs` (the
// book's cited prior art) does not exist in this workspace, so this is
// implemented from the book's prose description, not read from source.

/// A compaction summary as it may arrive from a model under prompt-schema
/// drift. text and obligations coerce string/number/bool/array/null
/// shapes into their expected type rather than failing; any field this
/// struct does not know about is captured in extra rather than causing
/// the whole summary to be discarded (mirrors goose's
/// #[serde(flatten)] extra pattern).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LenientCompactionSummary {
    #[serde(default, deserialize_with = "coerce_to_string")]
    pub text: String,
    #[serde(default, deserialize_with = "coerce_to_string_vec")]
    pub obligations: Vec<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

fn coerce_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn coerce_to_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(coerce_value_to_string(&value))
}

fn coerce_to_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items.iter().map(coerce_value_to_string).collect(),
        other => vec![coerce_value_to_string(&other)],
    })
}

/// Mechanism 8 entry point: parse a raw JSON summary payload leniently. A
/// malformed/mistyped known field is coerced (see LenientCompactionSummary
/// field docs) rather than causing the whole summary to be discarded.
pub fn parse_lenient_summary(raw: &serde_json::Value) -> Result<LenientCompactionSummary, String> {
    serde_json::from_value(raw.clone()).map_err(|error| error.to_string())
}
