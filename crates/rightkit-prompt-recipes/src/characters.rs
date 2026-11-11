//! studio.characters: reference picker, character lock (LoRA or reference
//! injection), aesthetic LoRA registry with licence vetting, LoRA base-model
//! lock, and character LoRA training on fal with an approval token and cost cap.
//!
//! Ported from the Media Console baseline a7271cf (`server/studio-characters.mjs`,
//! `studio-image-references.mjs`, `studio-image-locks.mjs`,
//! `studio-aesthetic-loras.mjs`, `character-training.mjs`) and the shared tools
//! `lib/aesthetic-lora-registry.mjs`, `lora-base-model-lock.mjs`,
//! `image-model-base-registry.mjs`, `pipelines/train-character-lora.mjs`.
//!
//! Storage (under `<data_dir>/characters/`):
//! - `<venture>/<character_id>/{metadata.json, refs/, training-set/, consent.json,
//!   lora/flux-character-fal.json, lora/training-log.json}`
//! - `_references/` shared reference images, `_uploads.json` upload cache
//!   (written by `rightkit_media_gen::fal_storage`, the only upload code; images go to the
//!   user's own fal storage and only when `lock` is called with `upload:true`),
//!   `_trainings/<training_id>.json` training jobs.
//!
//! Training spends real money on fal. `train` runs only with an app-minted
//! approval token (kind `character_training`, see `approvals.rs`): `train_preflight`
//! returns a `plan_id` (a hash of the character, trainer, steps, dataset file hashes,
//! cost, cap and trigger) and an `approval_challenge` bound to those inputs; the
//! running app mints the token, `train` consumes it once, and never above the cost
//! cap unless `override_cap` was part of the approved plan. A plan id alone, or
//! `operator_approved`, approves nothing.

#[cfg(feature = "commands")]
use std::collections::BTreeSet;
#[cfg(feature = "commands")]
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{RecipeError, RecipeResult};
#[cfg(feature = "commands")]
use rightkit_media_gen::catalogue::{Catalogue, Entry, InputType};
#[cfg(feature = "commands")]
use rightkit_media_gen::library::sha256_hex;
#[cfg(feature = "commands")]
use rightkit_media_gen::Context;
#[cfg(feature = "commands")]
use rightkit_media_gen::{credentials, ledger, store};

#[cfg(feature = "commands")]
use crate::recipe_catalogue::registry_served;
#[cfg(feature = "commands")]
use rightkit_media_gen::fal_storage::{
    fal_get, fal_key, fal_post_checked, http_error, is_image, redact, upload_bytes, upload_file,
};

#[cfg(feature = "commands")]
pub const COMMANDS: &[&str] = &[
    "studio.characters.list",
    "studio.characters.create",
    "studio.characters.add_image",
    "studio.characters.references",
    "studio.characters.lock",
    "studio.characters.loras",
    "studio.characters.base_lock",
    "studio.characters.trainers",
    "studio.characters.train_preflight",
    "studio.characters.train",
    "studio.characters.train_status",
];

const BASE_MODELS: &str = include_str!("../data/characters/base_models.json");
#[cfg(feature = "commands")]
const AESTHETIC: &str = include_str!("../data/characters/aesthetic_loras.json");
#[cfg(feature = "commands")]
const IMAGE_MODELS: &str = include_str!("../data/characters/image_models.json");
#[cfg(feature = "commands")]
const TRAINERS: &str = include_str!("../data/characters/trainers.json");

#[cfg(feature = "commands")]
const KNOWN_BRANDS: [&str; 8] = ["DD", "RH", "TS", "HR", "DV", "SS", "KDP", "NA"];
#[cfg(feature = "commands")]
const LORA_MANIFEST: &str = "flux-character-fal.json";

fn err(code: &str, message: impl Into<String>) -> RecipeError {
    RecipeError::new(code, message)
}

fn embedded(src: &str) -> Value {
    serde_json::from_str(src).expect("embedded characters data is valid JSON")
}

#[cfg(feature = "commands")]
fn io(e: std::io::Error) -> RecipeError {
    store::io_err(e).into()
}

#[cfg(feature = "commands")]
fn root(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("characters")
}

fn text<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(feature = "commands")]
fn flag(input: &Value, key: &str) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(feature = "commands")]
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !s.starts_with('.')
}

#[cfg(feature = "commands")]
fn write_json(path: &Path, v: &Value) -> RecipeResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    rightkit_fs::atomic_write(path, &serde_json::to_vec_pretty(v).unwrap_or_default())
        .map_err(|e| io(std::io::Error::other(e)))
}

#[cfg(feature = "commands")]
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

// ---------------------------------------------------------------------------
// Base-model lock (lora-base-model-lock.mjs, image-model-base-registry.mjs)
// ---------------------------------------------------------------------------

pub fn known_base_models() -> Vec<String> {
    embedded(BASE_MODELS)["base_models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Reject any base model id the registry does not know (write-time guard).
pub fn assert_known_base(base: &str, context: &str) -> RecipeResult<String> {
    let base = base.trim();
    let known = known_base_models();
    if known.iter().any(|k| k == base) {
        Ok(base.to_string())
    } else {
        Err(err(
            "unknown_base_model",
            format!(
                "Unknown base_model \"{}\" for {context}. Known: {}",
                if base.is_empty() { "(empty)" } else { base },
                known.join(", ")
            ),
        ))
    }
}

/// Every LoRA must name a known base model equal to the selected model's base.
pub fn normalize_lora_weights(loras: &[Value], selected_base: &str) -> RecipeResult<Vec<Value>> {
    let selected = assert_known_base(selected_base, "selected generation model")?;
    let mut out = Vec::new();
    for (i, l) in loras.iter().enumerate() {
        let path = text(l, "path")
            .ok_or_else(|| err("invalid_input", format!("loras[{i}].path is required")))?;
        let base = text(l, "base_model").ok_or_else(|| {
            err(
                "invalid_input",
                format!("loras[{i}].base_model is required"),
            )
        })?;
        let base = assert_known_base(base, &format!("loras[{i}].base_model"))?;
        if base != selected {
            return Err(err(
                "base_model_mismatch",
                format!("LoRA base_model {base} does not match selected generation base model {selected}"),
            ));
        }
        let mut o = Map::new();
        o.insert("path".into(), json!(path));
        if let Some(s) = l.get("scale").and_then(Value::as_f64) {
            o.insert("scale".into(), json!(s));
        }
        out.push(Value::Object(o));
    }
    Ok(out)
}

#[cfg(feature = "commands")]
fn model_entry(provider: &str, model: &str) -> Option<Value> {
    embedded(IMAGE_MODELS)["models"]
        .get(format!("{provider}/{model}"))
        .cloned()
}

// ---------------------------------------------------------------------------
// Engine routes: what the engine serves is read from the engine, not from a hand flag
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
/// One model row of the engine registry (the rows `models` lists), reduced to what the lock needs.
struct EngineRoute {
    id: String,
    kind: String,
    /// `references` of the model row: request param, adapter-validated maximum, upload mode.
    references: Option<(String, Option<u32>, String)>,
}

#[cfg(feature = "commands")]
/// Every model the engine's provider registry lists, with the `references` field the model list
/// carries (`Provider::references`, else the adapter's capabilities). Read once. No key, network or
/// keychain access: the registry is static.
fn engine_routes() -> &'static [EngineRoute] {
    static ROUTES: std::sync::OnceLock<Vec<EngineRoute>> = std::sync::OnceLock::new();
    ROUTES.get_or_init(|| {
        let mut out = Vec::new();
        for p in rightkit_media_gen::providers::registry() {
            for mi in p.models() {
                let kind = serde_json::to_value(mi.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                let references = p
                    .references(mi.model)
                    .or(p.capabilities(mi.model).references)
                    .map(|r| (r.param.to_string(), r.max, r.local_upload.to_string()));
                out.push(EngineRoute {
                    id: format!("{}/{}", p.id(), mi.model),
                    kind,
                    references,
                });
            }
        }
        out
    })
}

#[cfg(feature = "commands")]
fn engine_route(provider: &str, model: &str, kind: &str) -> Option<&'static EngineRoute> {
    let id = format!("{provider}/{model}");
    engine_routes()
        .iter()
        .find(|r| r.id == id && r.kind == kind)
}

#[cfg(feature = "commands")]
/// The declared engine catalogue entry (models.json or the user override) for a route.
fn declared_entry(provider: &str, model: &str) -> Option<&'static Entry> {
    Catalogue::get().find(provider, model)
}

#[cfg(feature = "commands")]
/// `Some(false)` when the engine reports the route's price and schema as not yet confirmed by a live run.
fn engine_reconciled(provider: &str, model: &str) -> Option<bool> {
    declared_entry(provider, model).and_then(|e| e.reconciled)
}

#[cfg(feature = "commands")]
const UNRECONCILED_NOTICE: &str = "price not yet confirmed by a live run";

#[cfg(feature = "commands")]
/// Routes of kind `image` the engine lists with a `references` field, as `provider/model`, sorted.
fn reference_capable_routes() -> Vec<String> {
    let mut routes: Vec<String> = engine_routes()
        .iter()
        .filter(|r| r.kind == "image" && r.references.is_some())
        .map(|r| r.id.clone())
        .collect();
    routes.sort();
    routes
}

#[cfg(feature = "commands")]
/// Where the studio table and the engine disagree. The studio table (`image_models.json`) holds only
/// studio facts (LoRA base model); whether a route is served is the engine's answer, and a table that
/// claims otherwise is reported here, never silently picked. Empty when they agree.
pub fn route_inconsistencies() -> Vec<Value> {
    let table = embedded(IMAGE_MODELS);
    let mut out = Vec::new();
    for (id, v) in table["models"].as_object().into_iter().flatten() {
        let kind = v["kind"].as_str().unwrap_or("image");
        let Some((provider, model)) = id.split_once('/') else {
            out.push(json!({"code": "invalid_route_id", "route": id, "message": format!("studio route id \"{id}\" is not provider/model")}));
            continue;
        };
        let served = registry_served(provider, model, kind);
        if v["in_engine"].as_bool() != Some(served) {
            out.push(json!({
                "code": "served_mismatch",
                "route": id,
                "studio_in_engine": v["in_engine"],
                "engine_serves": served,
                "message": format!("studio table says in_engine={} for {id}, the engine {} it", v["in_engine"], if served { "serves" } else { "does not list" }),
            }));
        }
        if served
            && v.get("lora").is_some_and(|l| !l.is_null())
            && lora_input(provider, model).is_none()
        {
            out.push(json!({
                "code": "lora_not_declared",
                "route": id,
                "message": format!("studio table gives {id} a LoRA base model, but the engine entry declares no json `loras` input"),
            }));
        }
        if kind == "image"
            && v.get("references").is_some_and(|r| !r.is_null())
            && engine_route(provider, model, kind).is_some_and(|r| r.references.is_none())
        {
            out.push(json!({
                "code": "references_mismatch",
                "route": id,
                "message": format!("studio table says {id} takes reference images, the engine model list declares none"),
            }));
        }
    }
    // Reverse: an image route the engine serves with a `loras` input but the studio table has no
    // LoRA base model for. Its base compatibility is unknown, so it cannot be locked.
    for e in Catalogue::get().entries() {
        let kind = serde_json::to_value(e.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        if kind == "image" && lora_input(&e.provider, &e.model).is_some() {
            let id = format!("{}/{}", e.provider, e.model);
            if model_entry(&e.provider, &e.model)
                .is_none_or(|t| t["lora"]["base_model"].as_str().is_none())
            {
                out.push(json!({
                    "code": "lora_base_unknown",
                    "route": id,
                    "message": format!("the engine serves {id} with a `loras` input, but the studio table names no LoRA base model for it"),
                }));
            }
        }
    }
    out
}

#[cfg(feature = "commands")]
/// The `loras` input the engine entry declares (type json), with its byte cap.
fn lora_input(provider: &str, model: &str) -> Option<(&'static str, Option<usize>)> {
    let e = declared_entry(provider, model)?;
    let i = e
        .inputs
        .iter()
        .find(|i| i.param == "loras" && i.ty == InputType::Json && i.send)?;
    Some((i.param.as_str(), i.max_bytes))
}

#[cfg(feature = "commands")]
/// LoRAs only run on routes the engine serves AND declares a `loras` input for, at the studio table's
/// base model for that route.
pub fn assert_loras_compatible(loras: &[Value], provider: &str, model: &str) -> RecipeResult<()> {
    if loras.is_empty() {
        return Ok(());
    }
    let entry = model_entry(provider, model);
    let base = entry
        .as_ref()
        .and_then(|e| e["lora"]["base_model"].as_str())
        .ok_or_else(|| {
            err(
                "lora_unsupported_model",
                format!("Model \"{provider}/{model}\" does not support LoRA weights"),
            )
        })?;
    if !registry_served(provider, model, "image") {
        return Err(err(
            "route_not_served",
            format!("{provider}/{model} is in the studio table with a LoRA base model, but the engine does not serve it (reported inconsistency); no LoRA can run there."),
        ));
    }
    if lora_input(provider, model).is_none() {
        return Err(err(
            "lora_unsupported_model",
            format!("The engine serves {provider}/{model} but declares no `loras` input for it, so it cannot take LoRA weights."),
        ));
    }
    normalize_lora_weights(loras, base).map(|_| ())
}

#[cfg(feature = "commands")]
/// Every LoRA must be an https URL the provider can fetch. A local file is refused, never uploaded.
fn assert_loras_hosted(loras: &[Value], whose: &str) -> RecipeResult<()> {
    for l in loras {
        let path = l["path"].as_str().unwrap_or("");
        if !path.starts_with("https://") {
            return Err(err(
                "lora_not_hosted",
                format!(
                    "The LoRA for {whose} is \"{path}\", not an https URL the provider can fetch, so it cannot be sent. \
                     This app does not upload LoRA weights: the app's only upload (a lock with upload:true) sends reference images to your own fal storage, and character training already returns a hosted URL. \
                     Host the file yourself on your own fal storage or another https host, put that https URL in the character's lora/{LORA_MANIFEST}, and lock again."
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "commands")]
fn base_lock(input: &Value) -> RecipeResult<Value> {
    let provider = text(input, "provider").unwrap_or("");
    let model = text(input, "model").unwrap_or("");
    let entries = embedded(IMAGE_MODELS);
    if model.is_empty() {
        let rows: Vec<Value> = entries["models"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        let (p, mo) = k.split_once('/').unwrap_or(("", k));
                        let kind = v["kind"].as_str().unwrap_or("image");
                        let route = engine_route(p, mo, kind);
                        json!({
                            "id": k,
                            "kind": v["kind"],
                            "in_engine": registry_served(p, mo, kind),
                            "studio_in_engine": v["in_engine"],
                            "lora_base_model": v["lora"]["base_model"],
                            "accepts_references": route.is_some_and(|r| r.references.is_some()),
                            "reconciled": engine_reconciled(p, mo),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(
            json!({"base_models": known_base_models(), "models": rows, "inconsistencies": route_inconsistencies()}),
        );
    }
    let loras: Vec<Value> = input
        .get("loras")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_loras_compatible(&loras, provider, model)?;
    let base = model_entry(provider, model)
        .and_then(|e| e["lora"]["base_model"].as_str().map(String::from));
    Ok(
        json!({"ok": true, "model": format!("{provider}/{model}"), "base_model": base, "loras": loras.len()}),
    )
}

// ---------------------------------------------------------------------------
// Characters and references
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
struct Character {
    id: String,
    character_id: String,
    venture: String,
    dir: PathBuf,
    project_slug: String,
    brand: String,
    description: String,
    has_lora: bool,
    lora_base_model: Option<String>,
    lora_url: Option<String>,
    lora_scale: f64,
    refs: Vec<String>,
    training_photos: usize,
    identifiable: bool,
    has_consent: bool,
}

#[cfg(feature = "commands")]
impl Character {
    fn public(&self) -> Value {
        json!({
            "id": self.id,
            "character_id": self.character_id,
            "venture": self.venture,
            "project_slug": self.project_slug,
            "brand": self.brand,
            "description": self.description,
            "has_lora": self.has_lora,
            "lora_base_model": self.lora_base_model,
            "lora_scale": self.lora_scale,
            "refs": self.refs,
            "refs_count": self.refs.len(),
            "training_photos": self.training_photos,
            "identifiable": self.identifiable,
            "has_consent": self.has_consent,
        })
    }
}

#[cfg(feature = "commands")]
fn normalize_brand(value: Option<&str>, venture: &str) -> String {
    let b = value.unwrap_or("").trim().to_ascii_uppercase();
    if KNOWN_BRANDS.contains(&b.as_str()) {
        return b;
    }
    let v = venture.trim().to_ascii_uppercase();
    if KNOWN_BRANDS.contains(&v.as_str()) {
        v
    } else {
        "NA".into()
    }
}

#[cfg(feature = "commands")]
fn slug(value: &str) -> String {
    let mut out = String::new();
    for c in value.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() {
        "studio-session".into()
    } else {
        t
    }
}

#[cfg(feature = "commands")]
fn list_images(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_image(p))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(feature = "commands")]
fn read_lora_manifest(
    dir: &Path,
    diagnostics: &mut Vec<Value>,
) -> (Option<String>, Option<String>, f64) {
    let path = dir.join("lora").join(LORA_MANIFEST);
    if !path.exists() {
        return (None, None, 1.0);
    }
    let parsed = (|| -> RecipeResult<(String, String, f64)> {
        let m = read_json(&path)
            .ok_or_else(|| err("bad_manifest", "LoRA manifest is not valid JSON"))?;
        // A hosted https URL is what the provider can fetch; a local path is kept as written so the
        // lock can refuse it by name instead of hiding the LoRA (`lora_not_hosted`).
        let url = ["url", "lora_url", "hosted_lora_url", "path", "local_path"]
            .iter()
            .find_map(|k| text(&m, k))
            .unwrap_or("")
            .to_string();
        if url.is_empty() {
            return Err(err("bad_manifest", "LoRA manifest is missing a hosted URL"));
        }
        let base = assert_known_base(m["base_model"].as_str().unwrap_or(""), "LoRA manifest")?;
        Ok((url, base, m["scale"].as_f64().unwrap_or(1.0)))
    })();
    match parsed {
        Ok((url, base, scale)) => (Some(url), Some(base), scale),
        Err(e) => {
            diagnostics.push(json!({"severity": "warning", "path": path.to_string_lossy(), "message": e.message}));
            (None, None, 1.0)
        }
    }
}

#[cfg(feature = "commands")]
fn read_character(
    venture: &str,
    dir_name: &str,
    dir: &Path,
    diagnostics: &mut Vec<Value>,
) -> Character {
    let meta_path = dir.join("metadata.json");
    let meta = if meta_path.exists() {
        read_json(&meta_path).unwrap_or_else(|| {
            diagnostics.push(json!({"severity": "warning", "path": meta_path.to_string_lossy(), "message": "metadata.json is not valid JSON"}));
            json!({})
        })
    } else {
        json!({})
    };
    let cid = text(&meta, "character_id")
        .filter(|c| valid_id(c))
        .unwrap_or(dir_name)
        .to_string();
    let (lora_url, lora_base, lora_scale) = read_lora_manifest(dir, diagnostics);
    let refs: Vec<String> = list_images(&dir.join("refs"))
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Character {
        id: format!("{venture}/{cid}"),
        character_id: cid,
        venture: venture.to_string(),
        dir: dir.to_path_buf(),
        project_slug: slug(text(&meta, "project_slug").unwrap_or(venture)),
        brand: normalize_brand(text(&meta, "brand"), venture),
        description: text(&meta, "description").unwrap_or("").to_string(),
        has_lora: lora_url.is_some(),
        lora_base_model: lora_base,
        lora_url,
        lora_scale,
        refs,
        training_photos: list_images(&dir.join("training-set")).len(),
        identifiable: meta["identifiable"].as_bool() == Some(true),
        has_consent: dir.join("consent.json").exists(),
    }
}

#[cfg(feature = "commands")]
fn read_characters(ctx: &Context) -> (Vec<Character>, Vec<Value>) {
    let mut diagnostics = Vec::new();
    let mut out = Vec::new();
    let mut ventures: Vec<PathBuf> = std::fs::read_dir(root(ctx))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    ventures.sort();
    for v in ventures {
        let vname = v
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if vname.starts_with('_') || vname.starts_with('.') {
            continue;
        }
        let mut chars: Vec<PathBuf> = std::fs::read_dir(&v)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        chars.sort();
        for c in chars {
            let cname = c
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            out.push(read_character(&vname, &cname, &c, &mut diagnostics));
        }
    }
    (out, diagnostics)
}

#[cfg(feature = "commands")]
fn resolve_character(ctx: &Context, requested: &str) -> RecipeResult<Character> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(err("invalid_input", "character_id is required"));
    }
    let (all, _) = read_characters(ctx);
    if let Some(i) = all.iter().position(|c| c.id == requested) {
        return Ok(all.into_iter().nth(i).unwrap());
    }
    let simple: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, c)| c.character_id == requested)
        .map(|(i, _)| i)
        .collect();
    match simple.len() {
        1 => Ok(all.into_iter().nth(simple[0]).unwrap()),
        0 => Err(err(
            "unknown_character",
            format!("Unknown character_id \"{requested}\""),
        )),
        _ => Err(err(
            "ambiguous_character",
            format!(
                "Ambiguous character_id \"{requested}\". Use a venture-qualified id like {}.",
                all[simple[0]].id
            ),
        )),
    }
}

#[cfg(feature = "commands")]
fn list(ctx: &Context) -> RecipeResult<Value> {
    let (chars, diagnostics) = read_characters(ctx);
    Ok(json!({
        "characters": chars.iter().map(Character::public).collect::<Vec<_>>(),
        "root": root(ctx).to_string_lossy(),
        "diagnostics": diagnostics,
    }))
}

#[cfg(feature = "commands")]
fn create(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let cid = text(input, "character_id").unwrap_or("");
    if !valid_id(cid) {
        return Err(err(
            "invalid_input",
            "character_id must contain only letters, numbers, dots, underscores, or hyphens",
        ));
    }
    let venture = text(input, "venture").unwrap_or("na").to_ascii_lowercase();
    if !valid_id(&venture) || venture.starts_with('_') {
        return Err(err(
            "invalid_input",
            "venture must contain only letters, numbers, dots, underscores, or hyphens",
        ));
    }
    let dir = root(ctx).join(&venture).join(cid);
    if dir.exists() {
        return Err(err(
            "exists",
            format!("character {venture}/{cid} already exists"),
        ));
    }
    std::fs::create_dir_all(dir.join("refs")).map_err(io)?;
    std::fs::create_dir_all(dir.join("training-set")).map_err(io)?;
    let meta = json!({
        "character_id": cid,
        "brand": normalize_brand(text(input, "brand"), &venture),
        "project_slug": slug(text(input, "project_slug").unwrap_or(&venture)),
        "description": text(input, "description").unwrap_or(""),
        "identifiable": flag(input, "identifiable"),
    });
    write_json(&dir.join("metadata.json"), &meta)?;
    if flag(input, "consent") {
        write_json(
            &dir.join("consent.json"),
            &json!({"consented": true, "recorded_at": store::now()}),
        )?;
    }
    let c = read_character(&venture, cid, &dir, &mut Vec::new());
    Ok(json!({"character": c.public()}))
}

#[cfg(feature = "commands")]
fn add_image(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let src =
        PathBuf::from(text(input, "path").ok_or_else(|| err("invalid_input", "path is required"))?);
    if !src.is_file() {
        return Err(err(
            "invalid_input",
            format!("image does not exist: {}", src.display()),
        ));
    }
    if !is_image(&src) {
        return Err(err(
            "invalid_input",
            format!("not an image file: {}", src.display()),
        ));
    }
    let kind = text(input, "kind").unwrap_or("reference");
    let dest_dir = match (text(input, "character_id"), kind) {
        (Some(c), "reference") => resolve_character(ctx, c)?.dir.join("refs"),
        (Some(c), "training") => resolve_character(ctx, c)?.dir.join("training-set"),
        (None, "reference") => root(ctx).join("_references"),
        _ => {
            return Err(err(
                "invalid_input",
                "kind must be reference or training (training needs character_id)",
            ))
        }
    };
    std::fs::create_dir_all(&dest_dir).map_err(io)?;
    let bytes = std::fs::read(&src).map_err(io)?;
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let dest = dest_dir.join(format!("{}-{}.{ext}", slug(stem), &sha256_hex(&bytes)[..8]));
    rightkit_fs::atomic_write(&dest, &bytes).map_err(|e| io(std::io::Error::other(e)))?;
    // A reference or training image is supplied material: record origin `supplied`, its source,
    // and the character's recorded consent (if any) as a reference. Evidence and rights stay
    // `unknown`: nothing here establishes either. This file lives in the character store, not the
    // library, so the facts sit in a `<file>.facts.json` beside it and in the result.
    let consent_refs: Vec<String> = text(input, "character_id")
        .and_then(|_| dest_dir.parent().map(|d| d.join("consent.json")))
        .filter(|p| read_json(p).is_some_and(|c| c["consented"] == true))
        .map(|p| vec![p.to_string_lossy().into_owned()])
        .unwrap_or_default();
    let facts = rightkit_media_gen::provenance::supplied_unknown(
        Some(src.to_string_lossy().into_owned()),
        consent_refs,
    );
    rightkit_media_gen::provenance::write_sibling_facts(&dest, &facts)?;
    Ok(json!({"path": dest.to_string_lossy(), "kind": kind, "facts": facts.to_value()}))
}

#[cfg(feature = "commands")]
fn references(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(160)
        .clamp(1, 1000) as usize;
    let mut refs: Vec<Value> = Vec::new();
    let mut roots: Vec<String> = Vec::new();
    fn push(
        roots: &mut Vec<String>,
        refs: &mut Vec<Value>,
        limit: usize,
        dir: PathBuf,
        source: &str,
        character: Option<&str>,
    ) {
        roots.push(dir.to_string_lossy().into_owned());
        for p in list_images(&dir) {
            if refs.len() >= limit {
                break;
            }
            refs.push(json!({
                "path": p.to_string_lossy(),
                "label": p.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "source": source,
                "character_id": character,
            }));
        }
    }
    if let Some(c) = text(input, "character_id") {
        let ch = resolve_character(ctx, c)?;
        push(
            &mut roots,
            &mut refs,
            limit,
            ch.dir.join("refs"),
            "character",
            Some(&ch.id),
        );
    }
    push(
        &mut roots,
        &mut refs,
        limit,
        root(ctx).join("_references"),
        "shared",
        None,
    );
    // Generated images in the library: <library>/<asset_id>/<asset_id>.<ext>.
    let lib = ctx.library_dir();
    roots.push(lib.to_string_lossy().into_owned());
    let mut assets: Vec<PathBuf> = std::fs::read_dir(&lib)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    assets.sort();
    for a in assets {
        for p in list_images(&a) {
            if refs.len() >= limit {
                break;
            }
            refs.push(json!({
                "path": p.to_string_lossy(),
                "label": p.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "source": "library",
                "character_id": Value::Null,
            }));
        }
    }
    let cap = embedded(IMAGE_MODELS)["reference_limit"]
        .as_u64()
        .unwrap_or(14);
    Ok(json!({"references": refs, "reference_cap": cap, "roots": roots, "diagnostics": []}))
}

// ---------------------------------------------------------------------------
// Aesthetic LoRAs (aesthetic-lora-registry.mjs)
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
fn aesthetic_registry() -> (Vec<Value>, Vec<String>) {
    let d = embedded(AESTHETIC);
    let loras = d["loras"].as_array().cloned().unwrap_or_default();
    let brands = d["commercial_brands"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    (loras, brands)
}

#[cfg(feature = "commands")]
fn licence_status(e: &Value) -> &'static str {
    if e["commercial_ok"].as_bool() != Some(true) {
        "blocked_noncommercial"
    } else if e["vetting_status"].as_str() != Some("vetted") {
        "unvetted"
    } else {
        "cleared"
    }
}

#[cfg(feature = "commands")]
fn public_aesthetic(e: &Value, commercial: &[String], brand: Option<&str>) -> RecipeResult<Value> {
    let id = e["id"].as_str().unwrap_or("").trim().to_string();
    let base = assert_known_base(
        e["base_model"].as_str().unwrap_or(""),
        &format!("aesthetic LoRA {id}"),
    )?;
    let status = licence_status(e);
    let is_commercial = brand
        .map(|b| commercial.iter().any(|c| c.eq_ignore_ascii_case(b)))
        .unwrap_or(false);
    let blocked = if e["vetting_status"].as_str() != Some("vetted") {
        Some(format!(
            "not vetted for generation use ({})",
            e["vetting_status"].as_str().unwrap_or("unknown")
        ))
    } else if is_commercial && e["commercial_ok"].as_bool() != Some(true) {
        Some("not cleared for commercial brand output".to_string())
    } else {
        None
    };
    Ok(json!({
        "id": id,
        "name": e["name"].as_str().unwrap_or(&id),
        "description": e["description"].as_str().unwrap_or(""),
        "source_url": e["source_url"].as_str().unwrap_or(""),
        "license": e["license"].as_str().unwrap_or("").to_ascii_lowercase(),
        "commercial_ok": e["commercial_ok"].as_bool() == Some(true),
        "base_model": base,
        "default_scale": e["default_scale"].as_f64().unwrap_or(1.0),
        "vetting_status": e["vetting_status"].as_str().unwrap_or(""),
        "source_verified_at": e["source_verified_at"].as_str().unwrap_or(""),
        "test_renders": e.get("test_renders").cloned().unwrap_or(json!([])),
        "licence_status": status,
        "usable": blocked.is_none(),
        "blocked_reason": blocked,
    }))
}

#[cfg(feature = "commands")]
fn loras(input: &Value) -> RecipeResult<Value> {
    let (reg, commercial) = aesthetic_registry();
    let brand = text(input, "brand");
    let rows: RecipeResult<Vec<Value>> = reg
        .iter()
        .map(|e| public_aesthetic(e, &commercial, brand))
        .collect();
    Ok(json!({"aesthetic_loras": rows?, "diagnostics": []}))
}

#[cfg(feature = "commands")]
/// Resolve an aesthetic LoRA for generation: unknown, unknown base, non
/// commercial on a commercial brand, and unvetted entries are all refused.
fn resolve_aesthetic(
    registry: &[Value],
    commercial: &[String],
    id: &str,
    brand: Option<&str>,
) -> RecipeResult<Value> {
    let id = id.trim();
    let e = registry
        .iter()
        .find(|c| c["id"].as_str() == Some(id))
        .ok_or_else(|| err("unknown_lora", format!("Unknown aesthetic LoRA \"{id}\"")))?;
    let base = assert_known_base(
        e["base_model"].as_str().unwrap_or(""),
        &format!("aesthetic LoRA {id}"),
    )?;
    let is_commercial = brand
        .map(|b| commercial.iter().any(|c| c.eq_ignore_ascii_case(b)))
        .unwrap_or(false);
    if is_commercial && e["commercial_ok"].as_bool() != Some(true) {
        return Err(err(
            "lora_not_commercial",
            format!("Aesthetic LoRA \"{id}\" is not cleared for commercial brand output"),
        ));
    }
    if e["vetting_status"].as_str() != Some("vetted") {
        return Err(err(
            "lora_not_vetted",
            format!("Aesthetic LoRA \"{id}\" is not vetted for generation use"),
        ));
    }
    let path = text(e, "path").ok_or_else(|| {
        err(
            "invalid_input",
            format!("Aesthetic LoRA \"{id}\" is missing a path"),
        )
    })?;
    let mut p = public_aesthetic(e, commercial, brand)?;
    p["path"] = json!(path);
    p["base_model"] = json!(base);
    Ok(p)
}

// ---------------------------------------------------------------------------
// Character lock
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
/// Typed refusal for a character with no usable LoRA on a route that takes no reference images.
/// Locking it would generate without identity, so the lock refuses instead of dropping the references.
fn reference_route_refusal(provider: &str, model: &str, character_id: &str) -> RecipeError {
    let head = if model == "fal-ai/flux-2-max" {
        "The served FLUX.2 Pro route (fal-ai/flux-2-max) takes no reference images".to_string()
    } else {
        format!("{provider}/{model} takes no reference images")
    };
    let routes = reference_capable_routes();
    let tail = if routes.is_empty() {
        "use a route that accepts reference images".to_string()
    } else {
        format!(
            "use a route that accepts reference images; the engine lists {}",
            routes.join(", ")
        )
    };
    err(
        "reference_route_unsupported",
        format!("{head}, and character \"{character_id}\" has no trained LoRA, so its reference images cannot lock identity there; {tail}."),
    )
}

#[cfg(feature = "commands")]
fn diag(d: &mut Vec<Value>, severity: &str, message: String) {
    d.push(json!({"severity": severity, "message": message}));
}

#[cfg(feature = "commands")]
/// The two sibling routes of one LoRA family (`X` and `X/edit`): both must carry a LoRA base model in
/// the studio table. A character LoRA with caller-supplied reference images belongs on the `/edit`
/// route (its `image_urls` input), without them on the plain route. Resolved between the pair only,
/// reported in the lock result (`requested_model`), never to another model family.
fn lora_sibling(provider: &str, model: &str, with_refs: bool) -> RecipeResult<Option<String>> {
    let base = model.strip_suffix("/edit").unwrap_or(model);
    let edit = format!("{base}/edit");
    let has_lora =
        |m: &str| model_entry(provider, m).is_some_and(|e| e["lora"]["base_model"].is_string());
    if !(has_lora(base) && has_lora(&edit)) {
        return Ok(None);
    }
    let target = if with_refs { edit } else { base.to_string() };
    if target != model && !registry_served(provider, &target, "image") {
        return Err(err(
            "route_not_served",
            format!(
                "A character LoRA {} needs {provider}/{target}, which the engine does not serve.",
                if with_refs {
                    "with reference images"
                } else {
                    "without reference images"
                }
            ),
        ));
    }
    Ok(Some(target))
}

#[cfg(feature = "commands")]
/// Output size input (`width` / `height`) of a declared route: the caller's value, else the entry's
/// default, within the entry's declared `min`/`max`. `None` when the entry declares no such input.
fn dimension(input: &Value, entry: &Entry, name: &str) -> RecipeResult<Option<Value>> {
    let Some(i) = entry.inputs.iter().find(|i| i.param == name && i.send) else {
        return Ok(None);
    };
    let v = match input.get(name) {
        Some(v) if !v.is_null() => v.clone(),
        _ => match &i.default {
            Some(d) => d.clone(),
            None => return Ok(None),
        },
    };
    let n = v
        .as_i64()
        .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
        .ok_or_else(|| {
            err(
                "invalid_input",
                format!("{name} must be a whole number of pixels"),
            )
        })?;
    if let Some(min) = i.min {
        if (n as f64) < min {
            return Err(err(
                "invalid_input",
                format!("{name} {n} is below the route's minimum {min}"),
            ));
        }
    }
    if let Some(max) = i.max {
        if (n as f64) > max {
            return Err(err(
                "invalid_input",
                format!("{name} {n} is above the route's maximum {max}"),
            ));
        }
    }
    Ok(Some(json!(n)))
}

#[cfg(feature = "commands")]
fn lock(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let (reg, commercial) = aesthetic_registry();
    lock_with(ctx, input, &reg, &commercial)
}

#[cfg(feature = "commands")]
fn lock_with(
    ctx: &Context,
    input: &Value,
    registry: &[Value],
    commercial: &[String],
) -> RecipeResult<Value> {
    let provider = text(input, "provider").unwrap_or("");
    let requested_model = text(input, "model").unwrap_or("");
    let brand = text(input, "brand");
    let engine_kind = if text(input, "kind") == Some("video") {
        "video"
    } else {
        "image"
    };
    let cap = embedded(IMAGE_MODELS)["reference_limit"]
        .as_u64()
        .unwrap_or(14) as usize;
    let mut diagnostics: Vec<Value> = Vec::new();

    let character = match text(input, "character_id") {
        Some(c) => Some(resolve_character(ctx, c)?),
        None => None,
    };
    let aesthetic = match text(input, "aesthetic_lora_id") {
        Some(id) => Some(resolve_aesthetic(
            registry,
            commercial,
            id,
            brand.or(character.as_ref().map(|c| c.brand.as_str())),
        )?),
        None => None,
    };

    // The caller's own references (deduped). The engine's cap for the route applies to these as a
    // refusal; references the lock adds from the character's folder are only trimmed.
    let mut caller_refs: Vec<String> = input
        .get("references")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut seen = BTreeSet::new();
    caller_refs.retain(|r| seen.insert(r.clone()));

    // A LoRA on a paired route family resolves between the pair; any other model is used as named.
    let mut model_owned = requested_model.to_string();
    if character.as_ref().is_some_and(|c| c.has_lora) || aesthetic.is_some() {
        if let Some(target) = lora_sibling(provider, requested_model, !caller_refs.is_empty())? {
            if target != requested_model {
                diag(
                    &mut diagnostics,
                    "notice",
                    format!("LoRA route {provider}/{requested_model} resolved to its sibling {provider}/{target} ({}).", if caller_refs.is_empty() { "no reference images" } else { "reference images supplied" }),
                );
                model_owned = target;
            }
        }
    }
    let model = model_owned.as_str();
    let entry = model_entry(provider, model);
    let declared = declared_entry(provider, model);
    let engine_ref: Option<(String, Option<usize>, bool)> =
        engine_route(provider, model, engine_kind)
            .and_then(|r| r.references.as_ref())
            .map(|(p, m, _)| (p.clone(), m.map(|m| m as usize), true));
    // Video first-frame routes keep their studio-table spec (the engine model list has no shape).
    let ref_spec = engine_ref.or_else(|| {
        entry
            .as_ref()
            .and_then(|e| e.get("references"))
            .filter(|r| !r.is_null())
            .map(|s| {
                (
                    s["param"].as_str().unwrap_or("image_urls").to_string(),
                    s["max"].as_u64().map(|m| m as usize),
                    s["shape"].as_str() == Some("array"),
                )
            })
    });

    let mut loras: Vec<Value> = Vec::new();
    let mut use_character_lora = false;
    if let Some(c) = character.as_ref().filter(|c| c.has_lora) {
        let l = json!({"path": c.lora_url, "scale": c.lora_scale, "base_model": c.lora_base_model});
        let checked =
            assert_loras_compatible(std::slice::from_ref(&l), provider, model).and_then(|_| {
                assert_loras_hosted(
                    std::slice::from_ref(&l),
                    &format!("character \"{}\"", c.character_id),
                )
            });
        match checked {
            Ok(()) => {
                loras.push(l);
                use_character_lora = true;
            }
            Err(e) if flag(input, "fallback_to_references") && !c.refs.is_empty() => {
                diag(
                    &mut diagnostics,
                    "warning",
                    format!(
                        "Character LoRA not used ({}); falling back to reference images.",
                        e.message
                    ),
                );
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(a) = aesthetic.as_ref() {
        loras.push(
            json!({"path": a["path"], "scale": a["default_scale"], "base_model": a["base_model"]}),
        );
    }
    assert_loras_compatible(&loras, provider, model)?;
    assert_loras_hosted(&loras, "a selected LoRA")?;

    // Reference injection: the caller's picks plus, when the character has no usable LoRA, the
    // character's own references (deduped, capped).
    let mut refs: Vec<String> = caller_refs.clone();
    // A character with no usable LoRA identifies only through its reference images.
    let character_refs_only = character
        .as_ref()
        .filter(|c| !use_character_lora && !c.refs.is_empty());
    if let Some(c) = character.as_ref().filter(|_| !use_character_lora) {
        refs.extend(c.refs.iter().cloned());
    }
    let mut seen = BTreeSet::new();
    refs.retain(|r| seen.insert(r.clone()));
    let engine_max = ref_spec.as_ref().and_then(|s| s.1);
    if let Some(m) = engine_max {
        if caller_refs.len() > m {
            return Err(err(
                "too_many_references",
                format!(
                    "{} reference images were supplied, but {provider}/{model} takes at most {m}.",
                    caller_refs.len()
                ),
            ));
        }
    }
    let max = engine_max.unwrap_or(cap).min(cap);
    if refs.len() > max {
        diag(
            &mut diagnostics,
            "warning",
            format!(
                "{} references available; using the first {max}.",
                refs.len()
            ),
        );
        refs.truncate(max);
    }

    let mut params = Map::new();
    let mut pending: Vec<String> = Vec::new();
    if !loras.is_empty() {
        // The `loras` input the engine entry declares is json (opaque, capped at `max_bytes`); each row is
        // `{path, scale}`: an https URL of the LoRA file and its strength.
        let (param, max_bytes) = lora_input(provider, model).unwrap_or(("loras", None));
        let rows: Vec<Value> = loras
            .iter()
            .map(|l| json!({"path": l["path"], "scale": l["scale"]}))
            .collect();
        let size = serde_json::to_vec(&rows).map(|b| b.len()).unwrap_or(0);
        if let Some(cap_bytes) = max_bytes.filter(|m| size > *m) {
            return Err(err("lora_too_large", format!("The loras value is {size} bytes; {provider}/{model} accepts at most {cap_bytes}.")));
        }
        params.insert(param.into(), json!(rows));
        // The route takes an explicit output size; the lock states it within the declared bounds.
        if let Some(e) = declared {
            for name in ["width", "height"] {
                if let Some(v) = dimension(input, e, name)? {
                    params.insert(name.into(), v);
                }
            }
        }
    }
    if !refs.is_empty() {
        match ref_spec.as_ref() {
            None => {
                if let Some(c) = character_refs_only {
                    return Err(reference_route_refusal(provider, model, &c.character_id));
                }
                diag(
                    &mut diagnostics,
                    "warning",
                    format!(
                        "{} does not accept reference images in the engine yet; {} reference(s) not injected.",
                        if model.is_empty() { "This model".to_string() } else { format!("{provider}/{model}") },
                        refs.len()
                    ),
                );
                refs.clear();
            }
            Some((param, _, is_array)) => {
                let mut urls = Vec::new();
                for r in &refs {
                    if r.starts_with("http://") || r.starts_with("https://") {
                        urls.push(r.clone());
                    } else if flag(input, "upload") {
                        urls.push(upload_file(ctx, Path::new(r))?);
                    } else {
                        pending.push(r.clone());
                    }
                }
                if !pending.is_empty() {
                    diag(
                        &mut diagnostics,
                        "warning",
                        format!(
                            "{} local reference(s) are not uploaded. Lock with upload:true uploads each image to your own fal storage under your fal key so the provider can read it; nothing is sent to any other host.",
                            pending.len()
                        ),
                    );
                }
                if !urls.is_empty() {
                    if *is_array {
                        params.insert(param.clone(), json!(urls));
                    } else {
                        params.insert(param.clone(), json!(urls[0]));
                    }
                }
            }
        }
    } else if character
        .as_ref()
        .map(|c| !c.has_lora && c.refs.is_empty())
        .unwrap_or(false)
    {
        diag(
            &mut diagnostics,
            "warning",
            "This character has no LoRA and no reference images; nothing to lock.".into(),
        );
    }

    // Every input the route requires that this lock fills (reference lists) is present.
    let ready = pending.is_empty()
        && declared.is_none_or(|e| {
            e.inputs
                .iter()
                .filter(|i| i.required && i.ty == InputType::UrlList && i.send)
                .all(|i| params.contains_key(&i.param))
        });
    if !ready && pending.is_empty() {
        diag(
            &mut diagnostics,
            "warning",
            format!("{provider}/{model} requires reference images and none were given."),
        );
    }

    // The engine's own word on the route: served, and whether its price has met a live run yet.
    let reconciled = engine_reconciled(provider, model);
    if reconciled == Some(false) {
        diag(
            &mut diagnostics,
            "notice",
            format!("{provider}/{model}: {UNRECONCILED_NOTICE}."),
        );
    }
    let route_id = format!("{provider}/{model}");
    let route_inconsistencies: Vec<Value> = route_inconsistencies()
        .into_iter()
        .filter(|i| i["route"] == route_id.as_str())
        .collect();
    for i in &route_inconsistencies {
        diag(
            &mut diagnostics,
            "warning",
            i["message"]
                .as_str()
                .unwrap_or("route inconsistency")
                .to_string(),
        );
    }

    let mode = if !loras.is_empty() {
        "lora"
    } else if !refs.is_empty() {
        "reference"
    } else {
        "none"
    };
    Ok(json!({
        "mode": mode,
        "kind": text(input, "kind"),
        "model": route_id,
        "requested_model": format!("{provider}/{requested_model}"),
        "route": {
            "id": route_id,
            "served": registry_served(provider, model, engine_kind),
            "reconciled": reconciled,
            "reconciled_notice": if reconciled == Some(false) { json!(UNRECONCILED_NOTICE) } else { Value::Null },
            "inconsistencies": route_inconsistencies,
        },
        "reconciled": reconciled,
        "ready": ready,
        "character": character.as_ref().map(Character::public),
        "aesthetic_lora": aesthetic,
        "loras": loras,
        "references": refs,
        "pending_upload": pending,
        "params": Value::Object(params),
        "diagnostics": diagnostics,
    }))
}

// ---------------------------------------------------------------------------
// fal HTTP (storage upload, training queue)
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
fn queue_base() -> String {
    rightkit_media_gen::providers::base_url_override(
        "RIGHTKIT_FAL_QUEUE_BASE",
        "https://queue.fal.run",
    )
}

// The fal storage upload (initiate + PUT + 7-day content-hash cache) and the fal
// REST helpers live in `rightkit_media_gen::fal_storage`, the single implementation also
// used by NanoBanana reference images. Uploads go to the user's own fal storage.

// ---------------------------------------------------------------------------
// Training
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
fn trainers() -> Value {
    embedded(TRAINERS)
}

#[cfg(feature = "commands")]
fn find_trainer(id: Option<&str>) -> RecipeResult<Value> {
    let t = trainers();
    let id = id
        .unwrap_or_else(|| t["default_trainer"].as_str().unwrap_or(""))
        .to_string();
    t["trainers"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["id"].as_str() == Some(id.as_str())))
        .cloned()
        .ok_or_else(|| err("unknown_trainer", format!("Unknown trainer \"{id}\"")))
}

fn round_usd(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub fn estimate_training_cost(trainer: &Value, steps: u64) -> f64 {
    let per = trainer["usd_per_step"].as_f64().unwrap_or(0.0);
    let min = trainer["min_usd"].as_f64().unwrap_or(0.0);
    round_usd((steps as f64 * per).max(min))
}

#[cfg(feature = "commands")]
struct Dataset {
    dir: PathBuf,
    images: Vec<PathBuf>,
    captions: Vec<PathBuf>,
    hash: String,
}

#[cfg(feature = "commands")]
fn inspect_dataset(dir: &Path) -> Dataset {
    let images = list_images(dir);
    let mut captions: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("txt"))
                .collect()
        })
        .unwrap_or_default();
    captions.sort();
    let mut lines = String::new();
    for p in images.iter().chain(captions.iter()) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let h = std::fs::read(p).map(|b| sha256_hex(&b)).unwrap_or_default();
        lines.push_str(&format!("{name}:{h}\n"));
    }
    Dataset {
        dir: dir.to_path_buf(),
        images,
        captions,
        hash: format!("sha256:{}", sha256_hex(lines.as_bytes())),
    }
}

#[cfg(feature = "commands")]
/// Everything `train` needs, with blockers instead of errors for things the
/// user can still fix in the dialog (cost cap, key, daily cap).
fn plan(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let character = resolve_character(ctx, text(input, "character_id").unwrap_or(""))?;
    let trainer = find_trainer(text(input, "trainer"))?;
    let defaults = trainers();
    let steps = match input.get("steps") {
        None | Some(Value::Null) => defaults["default_steps"].as_u64().unwrap_or(1000),
        Some(v) => v.as_u64().filter(|s| *s > 0).ok_or_else(|| {
            err(
                "invalid_input",
                "Character LoRA steps must be a positive integer",
            )
        })?,
    };
    let cap = input
        .get("cap_usd")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| defaults["default_cap_usd"].as_f64().unwrap_or(10.0));
    let override_cap = flag(input, "override_cap");
    let allow_extreme = flag(input, "allow_extreme_dataset_size");

    let ds = inspect_dataset(&character.dir.join("training-set"));
    let n = ds.images.len();
    let range = defaults["photo_range"]
        .as_array()
        .map(|a| {
            (
                a[0].as_u64().unwrap_or(15) as usize,
                a[1].as_u64().unwrap_or(30) as usize,
            )
        })
        .unwrap_or((15, 30));
    if n == 0 {
        return Err(err(
            "dataset_size",
            "Character LoRA training requires at least one training photo; found 0.",
        ));
    }
    if !allow_extreme && (n < range.0 || n > range.1) {
        return Err(err(
            "dataset_size",
            format!("Character LoRA training requires {}-{} photos; found {n}. Pass allow_extreme_dataset_size to override.", range.0, range.1),
        ));
    }
    if character.identifiable && !character.has_consent {
        return Err(err(
            "consent_required",
            format!(
                "consent.json is required for identifiable character training: {}",
                character.dir.join("consent.json").display()
            ),
        ));
    }

    let cost = estimate_training_cost(&trainer, steps);
    let exceeds = cost > cap + 1e-9;
    let trigger = text(input, "trigger_word")
        .map(String::from)
        .unwrap_or_else(|| format!("chr_{}", character.character_id.replace(['.', '-'], "_")));
    let mut blockers: Vec<String> = Vec::new();
    if exceeds && !override_cap {
        blockers.push(format!("Predicted cost ${cost:.2} exceeds the ${cap:.2} per-character cap. Raise it with override_cap."));
    }
    let key_present = credentials::get(ctx, "fal")
        .map(|k| k.is_some())
        .unwrap_or(false);
    if !key_present {
        blockers.push("No fal API key is configured.".into());
    }
    let daily = store::open(ctx).and_then(|conn| {
        ledger::check(&conn, cost).map(|_| ledger::status(&conn).unwrap_or(Value::Null))
    });
    let (daily_ok, daily_note) = match &daily {
        Ok(_) => (true, Value::Null),
        Err(e) => {
            blockers.push(e.message.clone());
            (false, json!(e.message))
        }
    };

    let token_src = format!(
        "rightkit-train-v1|{}|{}|{steps}|{}|{cost:.2}|{cap:.2}|{override_cap}|{trigger}",
        character.id,
        trainer["id"].as_str().unwrap_or(""),
        ds.hash
    );
    let plan_id = format!("ap1_{}", &sha256_hex(token_src.as_bytes())[..32]);
    Ok(json!({
        "character_id": character.id,
        "trainer": {
            "id": trainer["id"], "label": trainer["label"], "endpoint": trainer["endpoint"],
            "usd_per_step": trainer["usd_per_step"], "price_note": trainer["price_note"],
        },
        "base_model": trainer["base_model"],
        "steps": steps,
        "trigger_word": trigger,
        "predicted_cost_usd": cost,
        "cap_usd": cap,
        "override_cap": override_cap,
        "exceeds_cap": exceeds,
        "dataset": {
            "path": ds.dir.to_string_lossy(),
            "photo_count": n,
            "caption_count": ds.captions.len(),
            "hash": ds.hash,
            "extreme": n < range.0 || n > range.1,
        },
        "daily_cap_ok": daily_ok,
        "daily_cap_note": daily_note,
        "key_present": key_present,
        "blockers": blockers,
        "ready": blockers.is_empty(),
        "plan_id": plan_id,
        "approval_note": "Training spends predicted_cost_usd on fal. Only the app can approve it: approval_challenge is the single-use challenge for this exact plan, and the engine starts training only with the token the app mints for it.",
    }))
}

#[cfg(feature = "commands")]
/// The inputs a `character_training` approval is bound to: everything that decides what is
/// trained and what it costs. Any change (a photo or caption edited, steps, trainer, cost,
/// cap, trigger) changes the hash, so a token never covers a different run.
fn training_approval_view(plan: &Value) -> Value {
    json!({
        "plan_id": plan["plan_id"],
        "character_id": plan["character_id"],
        "trainer": plan["trainer"]["id"],
        "endpoint": plan["trainer"]["endpoint"],
        "base_model": plan["base_model"],
        "steps": plan["steps"],
        "trigger_word": plan["trigger_word"],
        "predicted_cost_usd": plan["predicted_cost_usd"],
        "cap_usd": plan["cap_usd"],
        "override_cap": plan["override_cap"],
        "dataset_hash": plan["dataset"]["hash"],
        "photo_count": plan["dataset"]["photo_count"],
    })
}

#[cfg(feature = "commands")]
/// For `studio.approvals.challenge` (kind `character_training`): the approval inputs and the
/// displayed hash of the plan `params` describe right now. `draft_id` must be that plan's id, so
/// a challenge can only be requested for the plan the user was shown, and only while it is ready.
pub fn training_approval_inputs(
    ctx: &Context,
    params: &Value,
    draft_id: &str,
) -> RecipeResult<(Value, String)> {
    let p = plan(ctx, params)?;
    let plan_id = p["plan_id"].as_str().unwrap_or("");
    if plan_id != draft_id {
        return Err(err("draft_changed", "The training plan is not the one that was shown (character, trainer, steps, dataset, cost or cap changed); run train_preflight again."));
    }
    if p["ready"].as_bool() != Some(true) {
        return Err(err("training_not_ready", "This training plan has blockers (cost cap, fal key or daily cap); resolve them in train_preflight first."));
    }
    Ok((training_approval_view(&p), plan_id.to_string()))
}

#[cfg(feature = "commands")]
/// A stored (uncompressed) zip archive, written by RightKit's deterministic stored-zip writer.
fn zip_stored(files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<u8>> {
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    rightkit_fs::stored_zip_bytes(&entries)
}

#[cfg(feature = "commands")]
fn training_path(ctx: &Context, id: &str) -> PathBuf {
    root(ctx).join("_trainings").join(format!("{id}.json"))
}

#[cfg(feature = "commands")]
fn terminal(status: &str) -> bool {
    matches!(status, "complete" | "failed")
}

#[cfg(feature = "commands")]
/// The plan id a training record was started for (`approval_token` on records written before
/// approval moved to the app-minted token).
fn plan_of(rec: &Value) -> Option<&str> {
    rec["plan_id"]
        .as_str()
        .or_else(|| rec["approval_token"].as_str())
}

#[cfg(feature = "commands")]
/// A training submit that may have reached fal and cannot be repeated blindly. Not
/// terminal, never retried: only `train_status` with `resolve` leaves it. The same
/// rule as a held generate job (`outcome_unknown` in the media-gen job store).
const HELD: &str = "outcome_unknown";

#[cfg(feature = "commands")]
/// The `outcome_unknown` refusal for a held training record: what is held, how much
/// stays reserved, and how to resolve it.
fn held_error(rec: &Value) -> RecipeError {
    let reserved = rec["reserved_usd"].as_f64().unwrap_or(0.0);
    if rec["held_settled"].as_bool() == Some(true) {
        // fal finished the training and charged; its result could not be used. Settled, not retried.
        return RecipeError::new(
            HELD,
            format!(
                "training {} for {} finished at fal but its result could not be used ({}); ${reserved:.2} is counted as spent and it is not retried. Call train_status with training_id {} and resolve=\"abandon\" to close it, then train again if you want.",
                rec["training_id"].as_str().unwrap_or(""),
                rec["character_id"].as_str().unwrap_or(""),
                rec["held_cause"]["message"].as_str().unwrap_or("no usable result"),
                rec["training_id"].as_str().unwrap_or(""),
            ),
        )
        .with_details(json!({
            "training_id": rec["training_id"],
            "reservation_id": rec["reservation_id"],
            "reserved_usd": rec["reserved_usd"],
            "held_at": rec["held_at"],
            "resolve": ["abandon"],
        }));
    }
    RecipeError::new(
        HELD,
        format!(
            "training {} for {} was sent to fal at {} and its outcome is unknown ({}); ${reserved:.2} stays reserved as {} and it is not retried. Check your fal dashboard, then call train_status with training_id {} and resolve=\"release\" (nothing started at fal) or resolve=\"abandon\" (it started, or may have; the amount stays counted as spent).",
            rec["training_id"].as_str().unwrap_or(""),
            rec["character_id"].as_str().unwrap_or(""),
            rec["held_at"].as_str().unwrap_or(""),
            rec["held_cause"]["message"].as_str().unwrap_or("no answer"),
            rec["reservation_id"].as_str().unwrap_or(""),
            rec["training_id"].as_str().unwrap_or(""),
        ),
    )
    .with_details(json!({
        "training_id": rec["training_id"],
        "reservation_id": rec["reservation_id"],
        "reserved_usd": rec["reserved_usd"],
        "held_at": rec["held_at"],
        "resolve": ["release", "abandon"],
    }))
}

#[cfg(feature = "commands")]
/// `train_status` with `resolve` on a held training: `release` (the user checked fal and
/// nothing started) reconciles the reservation to zero; `abandon` (it started, or may
/// have) keeps the reserved amount as spent. Either ends the record as `failed`, so a
/// new approved plan may train again. Without `resolve` the held record is returned.
fn resolve_held(
    ctx: &Context,
    path: &Path,
    mut rec: Value,
    resolve: Option<&str>,
) -> RecipeResult<Value> {
    let settled = rec["held_settled"].as_bool() == Some(true);
    let Some(action) = resolve else {
        rec["resolve"] = if settled {
            json!(["abandon"])
        } else {
            json!(["release", "abandon"])
        };
        return Ok(rec);
    };
    if !matches!(action, "release" | "abandon") {
        return Err(err(
            "invalid_input",
            "resolve must be \"release\" or \"abandon\"",
        ));
    }
    if settled && action == "release" {
        return Err(err("invalid_input", "fal finished this training and charged for it, so it cannot be released; use resolve=\"abandon\""));
    }
    let job_id = rec["reservation_id"].as_str().unwrap_or("").to_string();
    let mut conn = store::open(ctx)?;
    let reserved = ledger::attempt_reserved(&conn, &job_id, 1)?
        .or(rec["reserved_usd"].as_f64())
        .unwrap_or(0.0);
    let (spent, code, message) = if action == "release" {
        (0.0, "outcome_unknown_released", format!("released by the user; nothing was started at fal, so the ${reserved:.2} reservation was released"))
    } else {
        (reserved, "outcome_unknown_abandoned", format!("abandoned by the user; fal may have started it, so ${reserved:.2} stays counted as spent"))
    };
    ledger::reconcile(&mut conn, &job_id, 1, spent)?;
    rec["status"] = json!("failed");
    rec["error"] = json!({"code": code, "message": message});
    rec["updated_at"] = json!(store::now());
    write_json(path, &rec)?;
    Ok(rec)
}

#[cfg(feature = "commands")]
fn train_preflight(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let mut p = plan(ctx, input)?;
    p["approval_challenge"] = Value::Null;
    if p["ready"].as_bool() == Some(true) {
        let plan_id = p["plan_id"].as_str().unwrap_or("").to_string();
        p["approval_challenge"] = rightkit_media_gen_approvals::issue_labeled(
            ctx,
            "character_training",
            &plan_id,
            &training_approval_view(&p),
            &plan_id,
        )?;
    }
    Ok(p)
}

#[cfg(feature = "commands")]
fn train(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    // Spend gate: only a token the running app minted starts a paid run. Checked before the plan
    // is read, so a caller without the app's secret learns nothing; `operator_approved`, a plan id
    // or the old echoed token approve nothing.
    let gate = rightkit_media_gen_approvals::gate(input)?;
    let p = plan(ctx, input)?;
    let cost = p["predicted_cost_usd"].as_f64().unwrap_or(0.0);
    if p["exceeds_cap"].as_bool() == Some(true) && !flag(input, "override_cap") {
        return Err(err(
            "cost_cap",
            format!("Predicted fal training cost ${cost:.2} exceeds the per-character cap ${:.2}. Set override_cap=true to proceed.", p["cap_usd"].as_f64().unwrap_or(0.0)),
        ));
    }
    let key = fal_key(ctx)?;
    let character = resolve_character(ctx, p["character_id"].as_str().unwrap_or(""))?;
    let trainer = find_trainer(p["trainer"]["id"].as_str())?;
    let plan_id = p["plan_id"].as_str().unwrap_or("");

    // Verify and consume the single-use token for exactly this plan; it approves one run.
    let approval = rightkit_media_gen_approvals::consume(
        ctx,
        &gate,
        "character_training",
        plan_id,
        &training_approval_view(&p),
        plan_id,
    )?;
    let approval = Value::Object(rightkit_media_gen_approvals::approval_fields(&approval));

    // One live training per approved plan: a retry returns it instead of paying twice.
    let token = plan_id;
    if let Ok(rd) = std::fs::read_dir(root(ctx).join("_trainings")) {
        for e in rd.filter_map(|e| e.ok()) {
            if let Some(r) = read_json(&e.path()) {
                // A held training (outcome unknown) blocks a new one for the same plan or character.
                if r["status"].as_str() == Some(HELD)
                    && (plan_of(&r) == Some(token)
                        || r["character_id"].as_str() == Some(character.id.as_str()))
                {
                    return Err(held_error(&r));
                }
                if plan_of(&r) == Some(token) && !terminal(r["status"].as_str().unwrap_or("")) {
                    let mut r = r;
                    r["deduplicated"] = json!(true);
                    return Ok(r);
                }
            }
        }
    }

    let training_id = format!(
        "train_{}",
        &sha256_hex(format!("{token}|{}", store::now()).as_bytes())[..12]
    );
    let job_id = format!("training:{training_id}");
    let mut conn = store::open(ctx)?;
    ledger::reserve(&mut conn, &job_id, 1, cost)?;
    let fail = |conn: &mut _, e: RecipeError| {
        let _ = ledger::reconcile(conn, &job_id, 1, 0.0);
        e
    };

    let ds = inspect_dataset(&character.dir.join("training-set"));
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for pth in ds.images.iter().chain(ds.captions.iter()) {
        let name = pth
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        match std::fs::read(pth) {
            Ok(b) => files.push((name, b)),
            Err(e) => return Err(fail(&mut conn, io(e))),
        }
    }
    let zip = match zip_stored(&files) {
        Ok(z) => z,
        Err(e) => return Err(fail(&mut conn, io(e))),
    };
    let zip_url = match upload_bytes(
        &key,
        &zip,
        &format!("{}-training-set.zip", character.character_id),
        "application/zip",
    ) {
        Ok(u) => u,
        Err(e) => return Err(fail(&mut conn, e.into())),
    };

    let mut body = Map::new();
    body.insert(
        trainer["images_field"]
            .as_str()
            .unwrap_or("images_data_url")
            .into(),
        json!(zip_url),
    );
    body.insert("steps".into(), p["steps"].clone());
    if trainer["trigger_word"].as_bool() == Some(true) {
        body.insert("trigger_word".into(), p["trigger_word"].clone());
    }
    if trainer["default_caption"].as_bool() == Some(true) {
        body.insert(
            "default_caption".into(),
            json!(format!("consistent character {}", character.character_id)),
        );
    }
    if let Some(extra) = trainer["extra_input"].as_object() {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    let endpoint = trainer["endpoint"].as_str().unwrap_or("");
    let submitted = match fal_post_checked(
        &format!("{}/{endpoint}", queue_base()),
        &key,
        &Value::Object(body),
    ) {
        Ok(v) => v,
        // Connect failure or a definite rejection: nothing was started, so release the reservation.
        Err((e, false)) => return Err(fail(&mut conn, e.into())),
        // The POST may have been accepted (timeout or drop after send, 408/429/5xx, unreadable
        // reply): keep the reservation at its estimate, record the training as held and never
        // send it again on its own.
        Err((cause, true)) => {
            let now = store::now();
            let held = json!({
                "training_id": training_id,
                "status": HELD,
                "character_id": character.id,
                "trainer": trainer["id"],
                "endpoint": endpoint,
                "base_model": trainer["base_model"],
                "steps": p["steps"],
                "predicted_cost_usd": cost,
                "plan_id": token,
                "approval": approval,
                "dataset_hash": p["dataset"]["hash"],
                "photo_count": p["dataset"]["photo_count"],
                "uploaded_dataset_url": zip_url,
                "request_id": Value::Null,
                "reservation_id": job_id,
                "reserved_usd": cost,
                "held_at": now,
                "held_cause": {"code": cause.code, "message": cause.message},
                "resolve": ["release", "abandon"],
                "created_at": now,
                "updated_at": now,
            });
            let mut e = held_error(&held);
            if let Err(w) = write_json(&training_path(ctx, &training_id), &held) {
                e.message.push_str(&format!(" The held record could not be written ({}); the reservation {job_id} still stays counted.", w.message));
            }
            return Err(e);
        }
    };
    let record = json!({
        "training_id": training_id,
        "status": "submitted",
        "character_id": character.id,
        "trainer": trainer["id"],
        "endpoint": endpoint,
        "base_model": trainer["base_model"],
        "steps": p["steps"],
        "predicted_cost_usd": cost,
        "plan_id": token,
        "approval": approval,
        "dataset_hash": p["dataset"]["hash"],
        "photo_count": p["dataset"]["photo_count"],
        "uploaded_dataset_url": zip_url,
        "request_id": submitted["request_id"],
        "status_url": submitted["status_url"],
        "response_url": submitted["response_url"],
        "created_at": store::now(),
        "updated_at": store::now(),
    });
    write_json(&training_path(ctx, &training_id), &record)?;
    Ok(record)
}

#[cfg(feature = "commands")]
fn extract_url(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.starts_with("http://") || s.starts_with("https://") => {
            Some(s.clone())
        }
        Value::Object(o) => ["url", "file_url", "download_url"]
            .iter()
            .find_map(|k| o.get(*k).and_then(extract_url)),
        Value::Array(a) => a.iter().find_map(extract_url),
        _ => None,
    }
}

#[cfg(feature = "commands")]
fn train_status(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let id = match text(input, "training_id") {
        Some(i) => i.to_string(),
        None => {
            // Latest training for a character.
            let c = resolve_character(ctx, text(input, "character_id").unwrap_or(""))?;
            let mut best: Option<Value> = None;
            if let Ok(rd) = std::fs::read_dir(root(ctx).join("_trainings")) {
                for e in rd.filter_map(|e| e.ok()) {
                    if let Some(r) = read_json(&e.path()) {
                        if r["character_id"].as_str() == Some(c.id.as_str())
                            && best
                                .as_ref()
                                .map(|b| r["created_at"].as_str() > b["created_at"].as_str())
                                .unwrap_or(true)
                        {
                            best = Some(r);
                        }
                    }
                }
            }
            if text(input, "resolve").is_some() {
                return Err(err("invalid_input", "resolve needs a training_id"));
            }
            return best
                .ok_or_else(|| err("training_not_found", format!("no training for {}", c.id)));
        }
    };
    if !valid_id(&id) {
        return Err(err("invalid_input", "invalid training_id"));
    }
    let path = training_path(ctx, &id);
    let mut rec = read_json(&path)
        .ok_or_else(|| err("training_not_found", format!("unknown training {id}")))?;
    if terminal(rec["status"].as_str().unwrap_or("")) {
        return Ok(rec);
    }
    // No request id was returned, so fal's queue cannot be asked about it: the user decides.
    if rec["status"].as_str() == Some(HELD) {
        return resolve_held(ctx, &path, rec, text(input, "resolve"));
    }
    let key = fal_key(ctx)?;
    let status_url = rec["status_url"].as_str().unwrap_or("").to_string();
    let (code, status, raw) = fal_get(&status_url, &key)?;
    if !(200..300).contains(&code) {
        return Err(http_error(code, &raw, &key).into());
    }
    let fal_status = status["status"].as_str().unwrap_or("").to_string();
    rec["fal_status"] = json!(fal_status);
    rec["updated_at"] = json!(store::now());
    if fal_status == "COMPLETED" {
        let (rc, result, rraw) = fal_get(rec["response_url"].as_str().unwrap_or(""), &key)?;
        let job_id = format!("training:{id}");
        let mut conn = store::open(ctx)?;
        // fal reported COMPLETED, so the training ran and is charged. A transient answer (408, 429,
        // 5xx) leaves the record as it is for the next status call; any other reply that yields no
        // LoRA URL holds the training with the reservation SETTLED at the prediction. It is never
        // released to zero on a failed or unusable result fetch.
        if matches!(rc, 408 | 429 | 500..=599) {
            rec["note"] = json!(format!(
                "fal reported the training complete but the result fetch answered {rc}; try again"
            ));
            write_json(&path, &rec)?;
            return Ok(rec);
        }
        let url = if (200..300).contains(&rc) {
            extract_url(&result["diffusers_lora_file"])
                .or_else(|| extract_url(&result["lora_file"]))
        } else {
            None
        };
        match url {
            Some(url) => {
                let c = resolve_character(ctx, rec["character_id"].as_str().unwrap_or(""))?;
                let base =
                    assert_known_base(rec["base_model"].as_str().unwrap_or(""), "training record")?;
                let manifest = json!({
                    "schema_version": "1.0.0",
                    "provider": "fal",
                    "endpoint": rec["endpoint"],
                    "url": url,
                    "scale": 1,
                    "base_model": base,
                    "trained_at": store::now(),
                    "dataset_hash": rec["dataset_hash"],
                    "dataset_photo_count": rec["photo_count"],
                    "steps": rec["steps"],
                    "cost_usd": rec["predicted_cost_usd"],
                    "config_url": extract_url(&result["config_file"]),
                });
                write_json(&c.dir.join("lora").join(LORA_MANIFEST), &manifest)?;
                write_json(
                    &c.dir.join("lora").join("training-log.json"),
                    &json!({"status": "completed", "training": rec, "manifest": manifest}),
                )?;
                let mut meta = read_json(&c.dir.join("metadata.json")).unwrap_or_else(|| json!({}));
                meta["training_audit"] = json!({
                    "trainer": rec["trainer"], "steps": rec["steps"], "base_model": base,
                    "dataset_hash": rec["dataset_hash"], "photo_count": rec["photo_count"],
                    "cost_usd": rec["predicted_cost_usd"], "trained_at": store::now(), "output": url,
                });
                write_json(&c.dir.join("metadata.json"), &meta)?;
                let _ = ledger::reconcile(
                    &mut conn,
                    &job_id,
                    1,
                    rec["predicted_cost_usd"].as_f64().unwrap_or(0.0),
                );
                rec["status"] = json!("complete");
                rec["lora_url"] = json!(url);
            }
            None => {
                let spent = rec["predicted_cost_usd"].as_f64().unwrap_or(0.0);
                let _ = ledger::reconcile(&mut conn, &job_id, 1, spent);
                let cause = format!(
                    "result fetch answered {rc}: {}",
                    redact(&rraw.chars().take(300).collect::<String>(), &key)
                );
                rec["status"] = json!(HELD);
                rec["held_at"] = json!(store::now());
                rec["held_settled"] = json!(true);
                rec["held_cause"] = json!({"code": "training_result_unusable", "message": cause});
                rec["reservation_id"] = json!(job_id);
                rec["reserved_usd"] = json!(spent);
                rec["resolve"] = json!(["abandon"]);
            }
        }
    } else {
        rec["status"] = json!(if fal_status == "IN_PROGRESS" {
            "running"
        } else {
            "submitted"
        });
    }
    write_json(&path, &rec)?;
    Ok(rec)
}

#[cfg(feature = "commands")]
pub fn handle(ctx: &Context, name: &str, input: &Value) -> RecipeResult<Value> {
    match name {
        "studio.characters.list" => list(ctx),
        "studio.characters.create" => create(ctx, input),
        "studio.characters.add_image" => add_image(ctx, input),
        "studio.characters.references" => references(ctx, input),
        "studio.characters.lock" => lock(ctx, input),
        "studio.characters.loras" => loras(input),
        "studio.characters.base_lock" => base_lock(input),
        "studio.characters.trainers" => Ok(trainers()),
        "studio.characters.train_preflight" => train_preflight(ctx, input),
        "studio.characters.train" => train(ctx, input),
        "studio.characters.train_status" => train_status(ctx, input),
        _ => Err(RecipeError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
