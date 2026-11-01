//! studio.eyes — the human-eyes gate (donor: GenRight `genright-studio/src/eyes.rs`).
//!
//! Principle: taste cannot be replaced by AI or jurors. The decision is a sidecar `<artifact>.eyes.json` next to the file,
//! keyed by a sha256 of the artifact bytes: change the bytes and the approval no
//! longer holds (`stale`). The sidecar shape is the original one, so the
//! Gallery's eyes filter and old pipelines read the same files.
//!
//! What is enforced, and where:
//! - Approval goes through the engine's commands with a token. `studio.eyes.approve` needs a token
//!   (`approvals` kind `eyes`) bound to the asset's path and content hash, minted with the launch
//!   secret the app passed its sidecar; a caller without it gets `approval_requires_app`. `reject`,
//!   `rework`, `request` and `status` stay open: they never release anything.
//! - [`gate_state`] reads `approved` only when the sidecar carries the verdict, a `hash` equal to
//!   the asset's current content hash, AND an `approval` record of the shape the engine writes
//!   (non-empty `channel`, `nonce`, `approved_at`, a 64-hex `input_hash`, and a `legacy_hash`
//!   equal to the asset's current content hash). A bare `{"verdict":"APPROVE", ...}`, or a record
//!   that is missing or does not match the bytes, reads `pending` with reason
//!   `approval_record_missing`. Sidecars written before the app gate carry no approval record, so
//!   they now read `pending` too (the previous rule accepted them on a hash match); they need a
//!   fresh approval.
//!
//!   LIMIT: this raises the cost of a casual forgery. It does not stop a process that can write
//!   files beside the asset and writes a well-formed record, because nothing on disk can be
//!   verified after the launch whose secret minted it. It is not a boundary against arbitrary
//!   local code (the same user can also start their own engine with a secret of their choosing; see
//!   the crate root). It binds approvals made through the engine's commands, which is how the agent
//!   plugin and other apps reach the host.
//! - Delivery reads the gate. the host's delivery step (for example a brief run that
//!   writes media into a delivery envelope under `<project>/exports/`) calls
//!   [`review_gate`] with the brief's `review.mode` before it places a file:
//!   `manual` and `auto_when_clean` refuse with `eyes_approval_required` unless
//!   every produced asset is `approved`; `none` does not require it and the
//!   result says `eyes_gate: "not_required"`. `auto_when_clean` is treated like
//!   `manual` because this crate holds no jury verdict (the jury is a shared
//!   RightKit capability, plan G6) and an unknown verdict blocks delivery.
//!   `studio.eyes.export_check` runs the same check on any files.
//! - Not enforced here: the library, the Gallery and the working state, which stay
//!   open to unapproved media; Production packs and the KDP run write plans and
//!   requests, not media; `studio.voices.export` writes a voice-sample batch the
//!   voice review owns.
//!
//! Dropped from the JS on purpose: `EYES_OFF` / `EYES_AUTO_APPROVE`. They let
//! an agent bypass the gate, which is the thing the gate exists to prevent.
//!
//! Commands: `studio.eyes.{status,request,approve,reject,rework,list_pending,export_check}`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use rightkit_media_gen::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "studio.eyes.status",
    "studio.eyes.request",
    "studio.eyes.approve",
    "studio.eyes.reject",
    "studio.eyes.rework",
    "studio.eyes.list_pending",
    "studio.eyes.export_check",
];

/// The approval kind (the crate root) that releases an asset.
pub const KIND: &str = "eyes";

pub const APPROVE: &str = "APPROVE";
pub const REJECT: &str = "REJECT";
pub const REWORK: &str = "REWORK";
pub const PENDING: &str = "PENDING";

const MEDIA_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "avif", "svg", "mp4", "mov", "webm", "m4v", "wav", "mp3",
    "flac", "m4a",
];

/// `<artifact>.eyes.json`
pub fn sidecar_path(artifact: &Path) -> PathBuf {
    let mut s = artifact.as_os_str().to_os_string();
    s.push(".eyes.json");
    PathBuf::from(s)
}

/// `sha256:` plus the first 16 hex digits of the artifact bytes, as the JS
/// gate records it. `None` when the file is missing.
pub fn file_hash(path: &Path) -> Option<String> {
    let hex = rightkit_fs::digest::sha256_file(path).ok()?;
    Some(format!("sha256:{}", &hex[..16]))
}

fn now_iso() -> String {
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    now.format(&fmt).unwrap_or_default()
}

/// The recorded decision, if the sidecar exists and parses.
pub fn load_decision(artifact: &Path) -> Option<Value> {
    let text = fs::read_to_string(sidecar_path(artifact)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_decision(artifact: &Path, decision: &Value) -> GenResult<()> {
    let target = sidecar_path(artifact);
    let body = serde_json::to_string_pretty(decision)
        .map_err(|e| GenError::new("eyes_write", e.to_string()))?;
    rightkit_fs::atomic_write(&target, body.as_bytes())
        .map_err(|e| GenError::new("eyes_write", format!("{}: {e}", target.display())))
}

fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// True when the sidecar's `approval` record has the shape [`record_with`] writes and is bound to
/// the asset's current content hash. Shape only: it cannot prove who wrote it (module doc).
fn approval_record_valid(decision: &Value, current: Option<&str>) -> bool {
    let Some(record) = decision.get("approval").and_then(Value::as_object) else {
        return false;
    };
    let text = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let input_hash_ok = text("input_hash")
        .is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()));
    input_hash_ok
        && text("channel").is_some()
        && text("nonce").is_some()
        && text("approved_at").is_some()
        && matches!((text("legacy_hash"), current), (Some(bound), Some(now)) if bound == now)
}

/// Gate state for one artifact: `approved` is the only exportable state.
/// `none` (never reviewed) and `stale` (bytes changed since approval) block
/// like `pending`, and so does an APPROVE with no valid approval record
/// (`reason: "approval_record_missing"`).
pub fn gate_state(artifact: &Path) -> Value {
    let current = file_hash(artifact);
    let decision = load_decision(artifact);
    let verdict = decision
        .as_ref()
        .and_then(|d| d.get("verdict"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let recorded = decision
        .as_ref()
        .and_then(|d| d.get("hash"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let hash_matches = matches!((&current, &recorded), (Some(a), Some(b)) if a == b);
    let record_ok = decision
        .as_ref()
        .is_some_and(|d| approval_record_valid(d, current.as_deref()));
    let gate = match (decision.is_some(), verdict) {
        (false, _) => "none",
        (true, APPROVE) if hash_matches && record_ok => "approved",
        (true, APPROVE) if hash_matches => "pending",
        (true, APPROVE) => "stale",
        (true, REJECT) => "rejected",
        (true, REWORK) => "rework",
        _ => "pending",
    };
    json!({
        "path": artifact.to_string_lossy(),
        "sidecar_path": sidecar_path(artifact).to_string_lossy(),
        "exists": current.is_some(),
        "verdict": if decision.is_some() { json!(verdict) } else { Value::Null },
        "gate": gate,
        "export_allowed": gate == "approved",
        "reason": if verdict == APPROVE && hash_matches && !record_ok { json!("approval_record_missing") } else { Value::Null },
        "hash": current,
        "recorded_hash": recorded,
        "decided_at": decision.as_ref().and_then(|d| d.get("ts")),
        "notes": decision.as_ref().and_then(|d| d.get("notes")),
        "stage": decision.as_ref().and_then(|d| d.get("stage")),
    })
}

/// Record a verdict for the artifact's current bytes.
pub fn record(
    artifact: &Path,
    verdict: &str,
    stage: &str,
    notes: Option<&str>,
) -> GenResult<Value> {
    record_with(artifact, verdict, stage, notes, None, None)
}

/// [`record`] that can also store the app approval proof and insist the bytes are still the ones
/// that were approved (`expect_hash`).
fn record_with(
    artifact: &Path,
    verdict: &str,
    stage: &str,
    notes: Option<&str>,
    approval: Option<&Value>,
    expect_hash: Option<&str>,
) -> GenResult<Value> {
    let Some(hash) = file_hash(artifact) else {
        return Err(GenError::new(
            "asset_not_found",
            format!("{}: no such file", artifact.display()),
        ));
    };
    if expect_hash.is_some_and(|expected| expected != hash) {
        return Err(GenError::new(
            "approval_input_changed",
            "The asset changed while it was being approved; review it again.",
        ));
    }
    let prior = load_decision(artifact);
    let mut decision = json!({
        "stage": stage,
        "artifactPath": artifact.to_string_lossy(),
        "hash": hash,
        "ts": now_iso(),
        "verdict": verdict,
        "notes": notes.filter(|n| !n.is_empty()),
    });
    if let Some(proof) = approval {
        decision["approval"] = Value::Object(crate::approval_fields(proof));
    }
    // Keep the jury context an earlier pipeline recorded.
    for key in ["juryVerdict", "juryScore"] {
        if let Some(v) = prior
            .as_ref()
            .and_then(|p| p.get(key))
            .filter(|v| !v.is_null())
        {
            decision[key] = v.clone();
        }
    }
    save_decision(artifact, &decision)?;
    Ok(gate_state(artifact))
}

// ---------------------------------------------------------------------------
// App-gated approval
// ---------------------------------------------------------------------------

/// The approval draft id for an asset: the hex of its content hash, so a token minted for one set
/// of bytes cannot approve another.
pub fn draft_id_of(hash: &str) -> String {
    hash.trim_start_matches("sha256:").to_string()
}

/// `(inputs, hash)` the approval challenge is bound to for the artifact `params` names: its
/// resolved path and its content hash now. `draft_changed` when `draft_id` is not that hash.
pub fn approval_inputs(
    ctx: &Context,
    params: &Value,
    draft_id: &str,
) -> GenResult<(Value, String)> {
    let artifact = resolve_artifact(ctx, params)?;
    let Some(hash) = file_hash(&artifact) else {
        return Err(GenError::new(
            "asset_not_found",
            format!("{}: no such file", artifact.display()),
        ));
    };
    if draft_id != draft_id_of(&hash) {
        return Err(GenError::new(
            "draft_changed",
            "The asset is not the one that was shown; review it again.",
        ));
    }
    let path = fs::canonicalize(&artifact).unwrap_or(artifact);
    Ok((
        json!({"kind": KIND, "path": path.to_string_lossy(), "hash": hash}),
        hash,
    ))
}

fn approve(ctx: &Context, input: &Value, stage: &str, notes: Option<&str>) -> GenResult<Value> {
    // First, before the asset is read: a caller without the app's token learns nothing.
    let gate = crate::gate(input)?;
    let artifact = resolve_artifact(ctx, input)?;
    let draft_id = input
        .get("draft_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            GenError::new(
                "invalid_input",
                "approve needs the draft_id the challenge was issued for",
            )
        })?;
    let (inputs, hash) = approval_inputs(ctx, input, draft_id)?;
    let proof = crate::consume(ctx, &gate, KIND, draft_id, &inputs, &hash)?;
    record_with(&artifact, APPROVE, stage, notes, Some(&proof), Some(&hash))
}

// ---------------------------------------------------------------------------
// Delivery gate
// ---------------------------------------------------------------------------

/// Every artifact must be `approved` (a live approval of its current bytes). On refusal returns
/// `eyes_approval_required` listing each unapproved asset and its state, and leaves a PENDING
/// request on the ones never reviewed or gone stale so they appear in `list_pending`. Returns the
/// gate states when all are approved.
pub fn ensure_exportable(artifacts: &[PathBuf], stage: &str) -> GenResult<Vec<Value>> {
    let states: Vec<Value> = artifacts.iter().map(|a| gate_state(a)).collect();
    let blocked: Vec<Value> = states
        .iter()
        .filter(|s| s["export_allowed"] != true)
        .map(|s| {
            json!({
                "path": s["path"],
                "gate": s["gate"],
                "reason": s["reason"],
                "verdict": s["verdict"],
                "hash": s["hash"],
                "recorded_hash": s["recorded_hash"],
                "exists": s["exists"],
            })
        })
        .collect();
    if blocked.is_empty() {
        return Ok(states);
    }
    for (artifact, state) in artifacts.iter().zip(&states) {
        if state["exists"] == true && (state["gate"] == "none" || state["gate"] == "stale") {
            let _ = record(artifact, PENDING, stage, None);
        }
    }
    let names: Vec<String> = blocked
        .iter()
        .map(|b| {
            let path = b["path"].as_str().unwrap_or_default();
            let name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            match b["reason"].as_str() {
                Some(reason) => format!(
                    "{name} ({}: {reason})",
                    b["gate"].as_str().unwrap_or("unknown")
                ),
                None => format!("{name} ({})", b["gate"].as_str().unwrap_or("unknown")),
            }
        })
        .collect();
    Err(GenError::new(
        "eyes_approval_required",
        format!(
            "{} of {} assets need human-eyes approval in the app before delivery: {}",
            blocked.len(),
            states.len(),
            names.join(", ")
        ),
    )
    .with_details(json!({"unapproved": blocked, "total": states.len(), "approved": states.len() - blocked.len()})))
}

/// What the brief's `review.mode` (`manual`, `auto_when_clean`, `none`) requires of `artifacts`
/// before delivery. Returns the `review` block to report, or `eyes_approval_required`.
/// The gate decision is the contract's (`rightkit_contracts::eyes_gate`): `auto_when_clean` delivers without a
/// person only on a clean automated verdict for these exact assets, and a producer with none treats it as
/// `manual`. This crate has no verdict source, so `clean_verdict` is always false and both modes need a person.
pub fn review_gate(mode: &str, artifacts: &[PathBuf], stage: &str) -> GenResult<Value> {
    use rightkit_contracts::types::content_brief::ReviewMode;
    let parsed = match mode {
        "none" => ReviewMode::None,
        "manual" => ReviewMode::Manual,
        "auto_when_clean" => ReviewMode::AutoWhenClean,
        other => {
            return Err(GenError::new(
                "invalid_input",
                format!("review mode must be manual, auto_when_clean or none; got \"{other}\""),
            ))
        }
    };
    const NO_VERDICT: &str = "no automated review verdict is available; a person must approve";
    let approval = if parsed == ReviewMode::None {
        Ok(Vec::new())
    } else {
        ensure_exportable(artifacts, stage)
    };
    let gate = rightkit_contracts::eyes_gate(parsed, approval.is_ok(), false);
    let mut review = json!({"mode": mode, "eyes_gate": gate.map_or("required", |g| g.name())});
    if parsed == ReviewMode::AutoWhenClean {
        review["basis"] = json!(NO_VERDICT);
    }
    match approval {
        Ok(states) => {
            if parsed != ReviewMode::None {
                review["approved"] = Value::Array(
                    states.iter().map(|s| json!({"path": s["path"], "hash": s["hash"], "decided_at": s["decided_at"]})).collect(),
                );
            }
            Ok(review)
        }
        Err(mut error) => {
            if parsed == ReviewMode::AutoWhenClean {
                error.message = format!("{} ({NO_VERDICT})", error.message);
            }
            if let Some(Value::Object(details)) = error.details.as_mut() {
                details.insert("review".into(), review);
            }
            Err(error)
        }
    }
}

/// `studio.eyes.export_check {review_mode, paths?, asset_ids?}`: the delivery check on any files.
fn export_check(ctx: &Context, input: &Value, stage: &str) -> GenResult<Value> {
    let mode = input
        .get("review_mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let strings = |key: &str| -> Vec<String> {
        input
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut artifacts = Vec::new();
    for path in strings("paths") {
        artifacts.push(resolve_artifact(ctx, &json!({"path": path}))?);
    }
    for id in strings("asset_ids") {
        artifacts.push(resolve_artifact(ctx, &json!({"asset_id": id}))?);
    }
    if artifacts.is_empty() {
        return Err(GenError::new(
            "invalid_input",
            "paths or asset_ids is required",
        ));
    }
    let review = review_gate(mode, &artifacts, stage)?;
    Ok(
        json!({"review": review, "assets": artifacts.iter().map(|a| gate_state(a)).collect::<Vec<_>>()}),
    )
}

fn resolve_artifact(ctx: &Context, input: &Value) -> GenResult<PathBuf> {
    if let Some(p) = input
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let path = PathBuf::from(p);
        if path.to_string_lossy().ends_with(".eyes.json") {
            return Err(GenError::new(
                "invalid_input",
                "path must be the artifact, not its .eyes.json sidecar",
            ));
        }
        if !path.is_file() {
            return Err(GenError::new(
                "asset_not_found",
                format!("{p}: no such file"),
            ));
        }
        if !is_media(&path) {
            return Err(GenError::new(
                "invalid_input",
                format!("{p}: not an image, video or audio file"),
            ));
        }
        return Ok(path);
    }
    if let Some(id) = input
        .get("asset_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if id.contains('/') || id.contains('\\') || id.contains("..") {
            return Err(GenError::new("invalid_input", "asset_id is not a valid id"));
        }
        let dir = ctx.library_dir().join(id);
        let found = fs::read_dir(&dir).ok().and_then(|rd| {
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_media(p))
                .collect();
            files.sort();
            files.into_iter().next()
        });
        return found.ok_or_else(|| {
            GenError::new(
                "asset_not_found",
                format!("asset {id}: no file in the library"),
            )
        });
    }
    Err(GenError::new(
        "invalid_input",
        "path or asset_id is required",
    ))
}

/// Media files under `<library>/<asset_id>/` whose gate is not `approved`.
pub fn list_pending(ctx: &Context, limit: usize) -> Vec<Value> {
    let mut rows = Vec::new();
    let Ok(top) = fs::read_dir(ctx.library_dir()) else {
        return rows;
    };
    let mut dirs: Vec<PathBuf> = top
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = files
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_media(p))
            .collect();
        files.sort();
        for file in files {
            let state = gate_state(&file);
            if state["gate"] != "approved" {
                let mut row = state;
                row["asset_id"] = json!(dir.file_name().and_then(|n| n.to_str()));
                rows.push(row);
                if rows.len() >= limit {
                    return rows;
                }
            }
        }
    }
    rows
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    let notes = input.get("notes").and_then(Value::as_str).map(str::trim);
    let stage = input
        .get("stage")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("app ui");
    match name {
        "studio.eyes.status" => Ok(gate_state(&resolve_artifact(ctx, input)?)),
        // Halt-and-resume: make sure the artifact carries a PENDING request,
        // unless it already has a live decision (approved or rejected).
        "studio.eyes.request" => {
            let artifact = resolve_artifact(ctx, input)?;
            let state = gate_state(&artifact);
            if state["gate"] == "none" || state["gate"] == "stale" {
                return record(&artifact, PENDING, stage, notes);
            }
            Ok(state)
        }
        "studio.eyes.approve" => approve(ctx, input, stage, notes),
        "studio.eyes.export_check" => export_check(ctx, input, stage),
        "studio.eyes.reject" => {
            if notes.is_none_or(str::is_empty) {
                return Err(GenError::new(
                    "invalid_input",
                    "a reject needs a reason in notes",
                ));
            }
            record(&resolve_artifact(ctx, input)?, REJECT, stage, notes)
        }
        "studio.eyes.rework" => {
            if notes.is_none_or(str::is_empty) {
                return Err(GenError::new(
                    "invalid_input",
                    "a rework request needs what to change in notes",
                ));
            }
            record(&resolve_artifact(ctx, input)?, REWORK, stage, notes)
        }
        "studio.eyes.list_pending" => {
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(200)
                .clamp(1, 2000) as usize;
            let rows = list_pending(ctx, limit);
            Ok(json!({"count": rows.len(), "truncated": rows.len() >= limit, "pending": rows}))
        }
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory holding one asset whose bytes are `first bytes`.
    fn temp_asset(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rightkit-eyes-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot.png");
        fs::write(&file, b"first bytes").unwrap();
        file
    }

    /// The proof `approvals::consume` returns, for the bytes now on disk.
    fn proof_for(hash: &str) -> Value {
        json!({
            "channel": "sidecar_secret",
            "parent_pid": 1,
            "parent_exe": "demo-app",
            "approved_by": "tester",
            "approved_at": "2026-10-16T00:00:00.000Z",
            "approved_unix": 1,
            "input_hash": "0".repeat(64),
            "legacy_hash": hash,
            "nonce": "00",
            "kind": KIND,
        })
    }

    #[test]
    fn bare_verdict_is_pending_and_an_app_record_is_approved() {
        let file = temp_asset("record");
        let hash = file_hash(&file).unwrap();

        // A sidecar that only says APPROVE, with the right hash, is not an approval.
        save_decision(&file, &json!({"verdict": APPROVE, "hash": hash, "ts": "t"})).unwrap();
        let bare = gate_state(&file);
        assert_eq!(bare["gate"], "pending");
        assert_eq!(bare["export_allowed"], false);
        assert_eq!(bare["reason"], "approval_record_missing");

        // The same verdict written with the app's record, bound to these bytes, is approved.
        record_with(
            &file,
            APPROVE,
            "test",
            None,
            Some(&proof_for(&hash)),
            Some(&hash),
        )
        .unwrap();
        let approved = gate_state(&file);
        assert_eq!(approved["gate"], "approved");
        assert_eq!(approved["export_allowed"], true);
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_record_for_other_bytes_does_not_approve() {
        let file = temp_asset("other");
        let hash = file_hash(&file).unwrap();
        record_with(
            &file,
            APPROVE,
            "test",
            None,
            Some(&proof_for(&hash)),
            Some(&hash),
        )
        .unwrap();
        fs::write(&file, b"second bytes").unwrap();
        // The bytes changed: the recorded hash no longer matches, so the approval is stale.
        assert_eq!(gate_state(&file)["gate"], "stale");
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }
}
