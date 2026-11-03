//! `media.*` — the modality-level command surface headless consumers
//! (other apps, scripts, agents) use to drive generation.
//!
//! It wraps the same `crate::api` functions as `generate`/`job.*`, so
//! request-id idempotency, the job store, detached runners, NDJSON progress
//! on stderr, spend reservations, and asset hashing behave identically; this
//! module only adds discovery (`media.modalities`), honest capability states
//! for every advertised modality, and route validation (`media.generate`).
//!
//! The command and route contract is the host engine's CLI media contract.

use crate::catalogue::{self, Catalogue};
use crate::providers::{self, Kind, Provider};
use crate::{api, credentials, Context, GenError, GenResult};
use serde_json::{json, Value};

/// Commands this module owns. Integration adds them to `commands::NAMES`, so
/// `doctor` capabilities, the CLI word map, and the sidecar `command`
/// request all see them automatically.
pub const COMMANDS: &[&str] = &[
    "media.modalities",
    "media.generate",
    "media.route",
    "media.route_prefs",
    "media.still_finish",
];

/// Every modality the CLI speaks about, in stable order. All of them map to
/// [`Kind`] and generate through the provider registry; a modality with no
/// registered route reports `unsupported` rather than failing later.
pub const MODALITIES: &[&str] = &[
    "image",
    "video",
    "voice",
    "avatar",
    "music",
    "sfx",
    "upscale",
    "lipsync",
    "text",
    "extend",
    "motion_control",
    "replace_object",
    "restyle",
    "transcribe",
];

fn kind_of(word: &str) -> Option<Kind> {
    match word {
        "image" => Some(Kind::Image),
        "video" => Some(Kind::Video),
        "voice" => Some(Kind::Voice),
        "avatar" => Some(Kind::Avatar),
        "music" => Some(Kind::Music),
        "sfx" => Some(Kind::Sfx),
        "upscale" => Some(Kind::Upscale),
        "lipsync" => Some(Kind::Lipsync),
        "text" => Some(Kind::Text),
        "extend" => Some(Kind::Extend),
        "motion_control" => Some(Kind::MotionControl),
        "replace_object" => Some(Kind::ReplaceObject),
        "restyle" => Some(Kind::Restyle),
        "transcribe" => Some(Kind::Transcribe),
        _ => None,
    }
}

/// Studio commands that produce or manage assets of a modality alongside the
/// generation route. Surfaced in `media.modalities` so a consumer can find
/// the voice lifecycle and avatar-management commands without guessing.
fn studio_routes(modality: &str) -> &'static [&'static str] {
    match modality {
        "voice" => &[
            "studio.voices.list",
            "studio.voices.import_voiceright",
            "studio.voices.verify",
            "studio.voices.preview",
            "studio.voices.presets",
            "studio.voices.design",
            "studio.voices.rename",
            "studio.voices.clone_elevenlabs",
            "studio.voices.clone_local",
            "studio.voices.batch",
            "studio.voices.batch_status",
            "studio.voices.delete",
            "studio.voices.set_default",
            "studio.voices.tier",
            "studio.voices.models_status",
            "studio.voices.install",
            "studio.voices.install_status",
            "studio.voices.export",
        ],
        "avatar" => &[
            "studio.avatars.create_twin",
            "studio.avatars.create_photo",
            "studio.avatars.create_prompt",
            "studio.avatars.status",
            "studio.avatars.list",
            "studio.avatars.routes",
            "studio.avatars.animate",
            "studio.avatars.lip_sync",
            "studio.avatars.review",
        ],
        "lipsync" => &[
            "studio.avatars.routes",
            "studio.avatars.lip_sync",
            "studio.avatars.animate",
        ],
        _ => &[],
    }
}

/// Why a modality has no generation route. Kept honest and specific: these
/// are absent provider capabilities, not hidden switches.
fn unsupported_reason(modality: &str) -> &'static str {
    match modality {
        "extend" | "motion_control" | "replace_object" | "restyle" => {
            "no model declared for this kind (see docs/CATALOGUE.md)"
        }
        _ => "no generation route",
    }
}

fn ready(credential: &str, installed: Option<bool>) -> bool {
    // `installed` is None only when the provider exposes no metadata check
    // at all; a keyless local provider then counts as potentially ready so
    // the modality is not reported unconfigured solely because nothing can
    // be verified without launching it.
    credential == "configured" || (credential == "not_required" && installed != Some(false))
}

/// `media.modalities` `{probe?}` → `{modalities:[…], probed, catalogue}`.
///
/// Per modality: `state` is `available` (at least one ready provider route),
/// `unconfigured` (routes exist but none are ready — keys missing or local
/// engines not installed), or `unsupported` (no route exists at all). Per
/// provider: `credential` is `configured` | `unconfigured` | `not_required`
/// (keyless local engine). Readiness is reported at two honest depths:
///
/// - `installed` — metadata-only presence check (`runtime_ready`): files and
///   manifests on disk, never launches a worker, never touches the network.
///   Always evaluated for providers that expose it, `probe` or not.
/// - `access` — the deeper live check (`test_key`, e.g. a local runtime's
///   `/health` call), only when the input is `{"probe":true}`; `null`
///   otherwise. A provider can be `installed:true` while `access` is false
///   (files present, daemon down) — consumers must not conflate the two.
///
/// `external` marks keyless providers whose runtime is an external scripted
/// worker (e.g. a Python venv) rather than code bundled in the app, so a
/// consumer can treat their output as unqualified for native-quality gates.
///
/// A model declared in the catalogue (`docs/CATALOGUE.md`) also lists `roles`,
/// `settings` and `pricing`; `catalogue` reports how many entries loaded, the
/// user file, and every load error.
pub fn modalities(ctx: &Context, input: &Value) -> GenResult<Value> {
    modalities_with(ctx, &providers::registry(), input)
}

pub fn modalities_with(
    ctx: &Context,
    registry: &[Box<dyn Provider>],
    input: &Value,
) -> GenResult<Value> {
    let probe = input.get("probe").and_then(Value::as_bool).unwrap_or(false);
    let cat = Catalogue::get();
    let today = catalogue::today();
    let mut out = Vec::with_capacity(MODALITIES.len());
    for &m in MODALITIES {
        let kind = kind_of(m);
        let mut providers_json = Vec::new();
        let mut any_ready = false;
        if let Some(kind) = kind {
            for p in registry {
                let models: Vec<Value> = p
                    .models()
                    .iter()
                    .filter(|mi| mi.kind == kind)
                    .map(|mi| {
                        let mut v = serde_json::to_value(mi).unwrap_or(Value::Null);
                        if let (Some(o), Some(r)) = (v.as_object_mut(), p.references(mi.model)) {
                            o.insert("references".into(), json!(r));
                        }
                        if let (Some(o), Some(e)) = (v.as_object_mut(), cat.find(p.id(), mi.model))
                        {
                            o.extend(e.describe(today));
                        }
                        v
                    })
                    .collect();
                if models.is_empty() {
                    continue;
                }
                // Keyless providers get the metadata-only runtime check for
                // free on every call; keyed providers only report whether a
                // credential exists (never a secret read).
                let runtime = p.runtime_ready().map(|r| match r {
                    Ok(()) => json!({"installed": true}),
                    Err(e) => json!({"installed": false, "code": e.code}),
                });
                let installed = runtime.as_ref().map(|r| r["installed"] == true);
                let credential = if p.needs_key() {
                    if credentials::status(ctx, p.id()) {
                        "configured"
                    } else {
                        "unconfigured"
                    }
                } else {
                    "not_required"
                };
                let access = if probe && !p.needs_key() {
                    Some(match p.test_key("") {
                        Ok(()) => json!({"ok": true}),
                        Err(e) => json!({"ok": false, "code": e.code, "message": e.message}),
                    })
                } else {
                    None
                };
                any_ready |= ready(credential, installed);
                providers_json.push(json!({
                    "id": p.id(),
                    "needs_key": p.needs_key(),
                    "credential": credential,
                    "installed": installed,
                    "access": access,
                    "external": if p.needs_key() { Value::Null } else { json!(p.runtime_is_external()) },
                    "ready": ready(credential, installed),
                    "models": models,
                }));
            }
        }
        let state = if providers_json.is_empty() {
            "unsupported"
        } else if any_ready {
            "available"
        } else {
            "unconfigured"
        };
        let mut entry = json!({
            "kind": m,
            "state": state,
            "providers": providers_json,
            "studio_commands": studio_routes(m),
        });
        if state == "unsupported" {
            entry["reason"] = json!(unsupported_reason(m));
        } else {
            entry["generate"] = json!(format!("media generate {m}"));
        }
        out.push(entry);
    }
    Ok(json!({ "modalities": out, "probed": probe, "catalogue": cat.summary() }))
}

/// Route validation shared by `media.generate`: resolve `kind` to a provider
/// [`Kind`], and when `provider` is absent infer it from `model` — exactly
/// one match fills it in, zero or many produce honest errors.
fn resolve_route(registry: &[Box<dyn Provider>], input: &mut Value) -> GenResult<Kind> {
    let word = input
        .get("kind")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GenError::new("invalid_input", "missing field: kind"))?;
    let Some(kind) = kind_of(word) else {
        let supported: Vec<&str> = MODALITIES
            .iter()
            .copied()
            .filter(|m| kind_of(m).is_some())
            .collect();
        return Err(if MODALITIES.contains(&word) {
            GenError::new(
                "unsupported_kind",
                format!("{word}: {}", unsupported_reason(word)),
            )
        } else {
            GenError::new(
                "unknown_kind",
                format!("unknown kind {word:?}; supported: {}", supported.join(", ")),
            )
        });
    };
    let has_provider = input
        .get("provider")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if !has_provider {
        let model = input.get("model").and_then(Value::as_str).unwrap_or("");
        if model.is_empty() {
            return Err(GenError::new("invalid_input", "missing field: model"));
        }
        let matches: Vec<&str> = registry
            .iter()
            .filter(|p| {
                p.models()
                    .iter()
                    .any(|mi| mi.model == model && mi.kind == kind)
            })
            .map(|p| p.id())
            .collect();
        match matches.as_slice() {
            [one] => input["provider"] = json!(one),
            [] => {
                return Err(GenError::new(
                    "unknown_model",
                    format!("no provider has a {word} model {model:?}"),
                ))
            }
            many => {
                return Err(GenError::new(
                    "ambiguous_route",
                    format!(
                        "model {model:?} is offered by {}; pass provider explicitly",
                        many.join(", ")
                    ),
                ))
            }
        }
    }
    Ok(kind)
}

/// `media.generate` — same submission semantics as the `generate` command
/// (`{kind, provider?, model, prompt, params?, request_id?, wait?,
/// wait_timeout_s?, dry_run?}`), with kind validation, provider inference,
/// and `unsupported_kind`/`unknown_kind` errors for modalities without a
/// route. `--dry-run` validates and estimates without storing or sending.
pub fn generate(ctx: &Context, input: &Value, dry_run: bool) -> GenResult<Value> {
    generate_with(ctx, &providers::registry(), input, dry_run)
}

pub fn generate_with(
    ctx: &Context,
    registry: &[Box<dyn Provider>],
    input: &Value,
    dry_run: bool,
) -> GenResult<Value> {
    let mut body = input.clone();
    // Request-level routing: a request naming no provider and model is routed from saved
    // selections and preferences (alternatives and reasons); a bare model served by two providers
    // is resolved only by a saved selection. Everything else is left to `resolve_route`.
    // The engine's record travels beside the body (never inside it): a caller's `route` key is refused.
    let route = crate::routing::route_generate(ctx, registry, &mut body)?;
    resolve_route(registry, &mut body)?;
    // Same rules as commands.rs "generate": the app may also pass
    // `dry_run:true` inside the input.
    let dry_run = dry_run
        || body
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if !dry_run && api::wants_wait(&body) {
        if matches!(
            body["provider"].as_str(),
            Some("local-image" | "local-blender")
        ) {
            return crate::runner::wait_local_job_routed(ctx, &body, route.as_ref());
        }
        let record = api::generate_wait_routed_with(
            ctx,
            registry,
            &body,
            &crate::jobs::RunOptions::default(),
            route.as_ref(),
        )?;
        if record["timed_out"] == true {
            if let Some(job_id) = record.get("job_id").and_then(Value::as_str) {
                crate::runner::spawn_job_runner(ctx, job_id);
            }
        }
        return Ok(record);
    }
    let result = api::generate_routed_with(ctx, registry, &body, dry_run, route.as_ref())?;
    if !dry_run {
        if let Some(job_id) = result.get("job_id").and_then(Value::as_str) {
            crate::runner::spawn_job_runner(ctx, job_id);
        }
    }
    Ok(result)
}

/// Dispatch one `media.*` command. Returns `None` for names this module does
/// not own so the caller can fall through to the studio router.
pub fn dispatch(
    ctx: &Context,
    name: &str,
    input: &Value,
    dry_run: bool,
) -> Option<GenResult<Value>> {
    if !COMMANDS.contains(&name) {
        return None;
    }
    match name {
        "media.modalities" => Some(modalities(ctx, input)),
        "media.generate" => Some(generate(ctx, input, dry_run)),
        "media.route" => Some(crate::routing::route_command(
            ctx,
            &providers::registry(),
            input,
        )),
        "media.route_prefs" => Some(crate::routing::prefs_command(ctx, input)),
        "media.still_finish" => Some(
            rightkit_compose::still_finish::run(
                input,
                dry_run
                    || input
                        .get("dry_run")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
            )
            .map_err(|e| GenError::new(e.code, e.message)),
        ),
        _ => None,
    }
}

/// Map `media …` CLI words to a command name plus input, mirroring
/// the engine word map: `media modalities` → (`media.modalities`, {});
/// `media generate <kind>` → (`media.generate`, kind merged into the input).
/// `None` when the first word is not `media`.
pub fn from_cli(words: &[String], mut input: Value) -> Option<(String, Value)> {
    if words.first().map(String::as_str) != Some("media") {
        return None;
    }
    if !input.is_object() {
        input = json!({});
    }
    match words.get(1).map(String::as_str) {
        Some("generate") => {
            if let Some(kind) = words.get(2) {
                input["kind"] = json!(kind);
            }
            Some(("media.generate".to_string(), input))
        }
        Some("route") => {
            if let Some(kind) = words.get(2) {
                input["kind"] = json!(kind);
            }
            Some(("media.route".to_string(), input))
        }
        Some(sub) => Some((format!("media.{}", sub.replace('-', "_")), input)),
        // Bare `media` is the discovery surface.
        None => Some(("media.modalities".to_string(), input)),
    }
}
