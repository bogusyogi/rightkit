use rightkit_context_protocol::{SourceResolutionReceiptV1, SourceResolutionStatusV1};
use serde_json::{json, Value};

fn needs_resolution(candidate: &Value) -> bool {
    let kind = candidate
        .get("sourceKind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(kind.as_str(), "graph" | "vector")
}

fn valid_source_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && std::path::Path::new(value).components().all(|component| {
            !matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
}

#[rustfmt::skip] fn valid_hash(value: &str) -> bool {
    let (prefix, hex_len) = if value.starts_with("sha256:") { ("sha256:", 64) } else if value.starts_with("xxh128:") { ("xxh128:", 32) } else { return false; };
    value.len() == prefix.len() + hex_len && value[prefix.len()..].bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
#[rustfmt::skip] fn missing_status(candidate: &Value, provider: &str, generation: &str) -> SourceResolutionStatusV1 {
    let text = |key| candidate.get(key).and_then(Value::as_str).unwrap_or_default();
    if text("id").is_empty() { SourceResolutionStatusV1::MissingIdentity } else if text("provider").is_empty() && provider.is_empty() { SourceResolutionStatusV1::MissingProvider } else if !valid_hash(text("sourceHash")) { SourceResolutionStatusV1::MissingHash } else if generation.is_empty() { SourceResolutionStatusV1::MissingGeneration } else if !valid_source_path(text("sourceRef")) { SourceResolutionStatusV1::MissingPath } else if text("resolver").is_empty() { SourceResolutionStatusV1::ResolverUnavailable } else { SourceResolutionStatusV1::Unresolved }
}

fn unresolved(candidate: &Value, provider: &str, generation: &str) -> SourceResolutionReceiptV1 {
    SourceResolutionReceiptV1 {
        schema_version: 1,
        candidate_id: candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .into(),
        provider: candidate
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or(provider)
            .into(),
        status: missing_status(candidate, provider, generation),
        expected_hash: candidate
            .get("sourceHash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        resolved_hash: None,
        expected_generation: generation.into(),
        resolved_generation: None,
        expected_path: candidate
            .get("sourceRef")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        resolved_path: None,
        resolver: candidate
            .get("resolver")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        overlay_identity: None,
    }
}

#[rustfmt::skip] fn verify(
    mut receipt: SourceResolutionReceiptV1,
    candidate: &Value,
    provider: &str,
    generation: &str,
) -> SourceResolutionReceiptV1 {
    let id = candidate
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let candidate_provider = candidate
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or(provider);
    let hash = candidate
        .get("sourceHash")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = candidate
        .get("sourceRef")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let resolver = candidate.get("resolver").and_then(Value::as_str).unwrap_or_default();
    let missing = missing_status(candidate, provider, generation);
    receipt.status = if missing != SourceResolutionStatusV1::Unresolved { missing
    } else if receipt.candidate_id != id || receipt.provider != candidate_provider {
        SourceResolutionStatusV1::Unresolved
    } else if receipt.expected_hash != hash || receipt.resolved_hash.as_deref() != Some(hash) {
        SourceResolutionStatusV1::HashMismatch
    } else if receipt.expected_generation != generation
        || receipt.resolved_generation.as_deref() != Some(generation)
    {
        SourceResolutionStatusV1::GenerationMismatch
    } else if !valid_source_path(path)
        || !valid_source_path(&receipt.expected_path)
        || receipt.expected_path != path
        || receipt.resolved_path.as_deref().is_none_or(|value| !valid_source_path(value))
        || receipt.resolved_path.as_deref() != Some(path) {
        SourceResolutionStatusV1::PathMismatch
    } else if receipt.resolver != resolver { SourceResolutionStatusV1::Unresolved
    } else if receipt.is_exact_current_source() {
        SourceResolutionStatusV1::Resolved
    } else {
        SourceResolutionStatusV1::Unresolved
    };
    receipt
}

/// Strip resolution evidence before strict CCS parsing and reject graph/vector
/// candidates unless hash, generation, and path match current source exactly.
#[rustfmt::skip] pub fn gate_source_resolutions(candidate_set: &mut Value) -> Vec<SourceResolutionReceiptV1> {
    let provider = candidate_set
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // An overlay identity that is present but unreadable is not the same as
    // one that was never sent. Swallowing the parse left it as None, and every
    // receipt then compared unequal and was reported as a generation
    // mismatch — a wrong cause for a malformed field. `MissingIdentity` is
    // the status that says what actually happened.
    let raw_overlay_identity = candidate_set
        .get("freshness")
        .and_then(|freshness| freshness.get("overlayIdentity"));
    let overlay_identity_present = raw_overlay_identity.is_some_and(|value| !value.is_null());
    let overlay_identity = raw_overlay_identity
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let overlay_identity_malformed = overlay_identity_present && overlay_identity.is_none();
    let generation = candidate_set.as_object_mut().and_then(|object| object.remove("generationId")).and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
    let Some(candidates) = candidate_set
        .get_mut("candidates")
        .and_then(Value::as_array_mut)
    else {
        return Vec::new();
    };
    let mut receipts = Vec::new();
    let mut omissions = Vec::new();
    candidates.retain_mut(|candidate| {
        let raw = candidate.as_object_mut().and_then(|object| object.remove("sourceResolution"));
        if !needs_resolution(candidate) { return true; }
        let mut receipt = raw
            .and_then(|value| serde_json::from_value(value).ok())
            .map(|value| verify(value, candidate, &provider, &generation))
            .unwrap_or_else(|| unresolved(candidate, &provider, &generation));
        if overlay_identity_malformed {
            receipt.status = SourceResolutionStatusV1::MissingIdentity;
        } else if receipt.overlay_identity != overlay_identity {
            receipt.status = SourceResolutionStatusV1::GenerationMismatch;
        }
        receipt.overlay_identity = overlay_identity.clone();
        let admitted = receipt.status == SourceResolutionStatusV1::Resolved;
        if !admitted {
            omissions.push(json!({"id": receipt.candidate_id, "layer": candidate.get("layer").cloned().unwrap_or(Value::Null),
                "reason": format!("source_resolution_{}", receipt.status.as_str())}));
        }
        receipts.push(receipt);
        admitted
    });
    if !candidate_set.get("omissions").is_some_and(Value::is_array) { candidate_set["omissions"] = json!([]); }
    candidate_set["omissions"].as_array_mut().unwrap().extend(omissions);
    // Provider completion order is not an admission signal.  Keep both the
    // receipt side-channel and omission list canonical so equivalent provider
    // results serialize identically before the planner sees them.
    receipts.sort_by(|left, right| {
        left.candidate_id
            .cmp(&right.candidate_id)
            .then(left.provider.cmp(&right.provider))
            .then(left.status.as_str().cmp(right.status.as_str()))
    });
    if let Some(omissions) = candidate_set["omissions"].as_array_mut() {
        omissions.sort_by(|left, right| {
            let left_id = left.get("id").and_then(Value::as_str).unwrap_or_default();
            let right_id = right.get("id").and_then(Value::as_str).unwrap_or_default();
            left_id
                .cmp(right_id)
                .then_with(|| {
                    left.get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .cmp(right.get("reason").and_then(Value::as_str).unwrap_or_default())
                })
        });
    }
    receipts
}
