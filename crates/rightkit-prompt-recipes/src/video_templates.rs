//! studio.video_templates: the video quality system ported from the Media
//! Console (a7271cf) and the workspace video pipeline.
//!
//! - Venture shot-contract templates (built-in starters plus
//!   `<Content>/<venture>/shot_contract_templates/*.json`), merge, validator
//!   and the per-backend contract renderers (`tools/pipelines/video/lib/prompt-contract.mjs`).
//! - Camera-move (40) and ad-pattern (14) libraries, plus the audio,
//!   transition, text-overlay, QA-rubric, motion, continuity and aesthetic
//!   recipe libraries (`tools/recipes/video/*.json`, embedded under
//!   `data/video_templates/`).
//! - The prompt composer (`video-prompt-composer.mjs`).
//! - The guarded preflight: a draft that validates the shot contract and the
//!   start/end frames, composes the prompt, estimates spend through the
//!   engine's own dry run, and is recorded as a draft that must be approved by
//!   the user in the app before a run (`studio.video_templates.approve`, which
//!   needs the app-minted token of `approvals`).
//!
//! Commands are named `studio.video_templates.<verb>`.

use std::collections::BTreeMap;
#[cfg(feature = "commands")]
use std::path::{Path, PathBuf};
#[cfg(feature = "commands")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::{RecipeError, RecipeResult};
#[cfg(feature = "commands")]
use rightkit_media_gen::Context;

#[cfg(feature = "commands")]
pub const COMMANDS: &[&str] = &[
    "studio.video_templates.templates",
    "studio.video_templates.template",
    "studio.video_templates.camera_moves",
    "studio.video_templates.ad_patterns",
    "studio.video_templates.camera_params",
    "studio.video_templates.recipes",
    "studio.video_templates.validate",
    "studio.video_templates.render",
    "studio.video_templates.preflight",
    "studio.video_templates.approve",
    "studio.video_templates.draft",
];

pub const CONTRACT_VERSION: &str = "1.1.0";
pub const RENDERER_VERSION: &str = "1.1.0";

const UNSAFE_MERGE_KEYS: &[&str] = &["__proto__", "constructor", "prototype"];

const REQUIRED_TOP_FIELDS: &[&str] = &[
    "primary_action",
    "subject_focus",
    "source_frame_read",
    "role_blocking",
    "device_role",
    "allowed_contact_surfaces",
    "motion_budget",
    "time_segments",
    "lighting_lock",
    "audio_lock",
    "negative_prompt",
];
const REQUIRED_ROLE_BLOCKING_FIELDS: &[&str] =
    &["hands_left", "hands_right", "gaze", "body_orientation"];
const REQUIRED_SEGMENT_FIELDS: &[&str] =
    &["start_s", "end_s", "camera", "action", "forbidden_zone"];
const ALLOWED_DEVICE_ROLES: &[&str] = &[
    "absent",
    "passive_background_prop",
    "primary_interaction",
    "spatially_separated",
];
const ALLOWED_AUDIO_MODES: &[&str] = &[
    "silent",
    "ambient",
    "foley",
    "dialogue",
    "lipsync_to_external_vo",
];
const LIPSYNC_CAPABLE_MODEL_PREFIXES: &[&str] = &["infinitetalk"];
const DEFAULT_CAMERA_LOCK: &str = "Locked-off tripod, fixed frame, fixed lens. Camera does not move at all: no pan, tilt, zoom, dolly, roll, rack focus, handheld sway, or reframing. Background remains fixed.";
const DEFAULT_LIGHTING_LOCK: &str = "constant exposure throughout, no time-of-day drift, no relight, source from same direction as initial frame.";
const MIN_NEGATIVES: usize = 3;
const MAX_NEGATIVES: usize = 12;

// ---------------------------------------------------------------------------
// Embedded data
// ---------------------------------------------------------------------------

struct Lib {
    #[cfg_attr(not(feature = "commands"), allow(dead_code))]
    src: &'static str,
    json: Value,
}

macro_rules! lib {
    ($name:ident, $file:literal) => {
        fn $name() -> &'static Lib {
            static CELL: OnceLock<Lib> = OnceLock::new();
            CELL.get_or_init(|| {
                let src: &'static str = include_str!(concat!("../data/video_templates/", $file));
                let json = serde_json::from_str(src).expect(concat!($file, " is valid JSON"));
                let stem = $file.strip_suffix(".json").unwrap_or($file);
                Lib {
                    src,
                    json: crate::brands::with_defaults(&format!("video/{stem}"), json),
                }
            })
        }
    };
}

lib!(camera_moves_lib, "camera_moves.json");
lib!(ad_patterns_lib, "ad_patterns.json");
lib!(aesthetics_lib, "aesthetics.json");
lib!(audio_lib, "audio.json");
lib!(transitions_lib, "transitions.json");
lib!(text_overlays_lib, "text_overlays.json");
lib!(qa_rubrics_lib, "qa_rubrics.json");
lib!(subject_motion_lib, "subject_motion.json");
lib!(scene_motion_lib, "scene_motion.json");
lib!(continuity_lib, "continuity.json");
lib!(quirks_lib, "per_model_quirks.json");
#[cfg(feature = "commands")]
lib!(templates_lib, "shot_contract_templates.json");
lib!(routes_lib, "model_routes.json");
lib!(camera_params_lib, "camera_params.json");

#[cfg(feature = "commands")]
type LibraryEntry = (&'static str, &'static str, fn() -> &'static Lib);

#[cfg(feature = "commands")]
/// (library id, label, loader). The order is the order shown in the UI.
const LIBRARIES: &[LibraryEntry] = &[
    ("camera_moves", "Camera moves", camera_moves_lib),
    ("ad_patterns", "Ad patterns", ad_patterns_lib),
    ("aesthetics", "Aesthetics", aesthetics_lib),
    ("audio", "Audio recipes", audio_lib),
    ("transitions", "Transitions", transitions_lib),
    ("text_overlays", "Text overlays", text_overlays_lib),
    ("qa_rubrics", "QA rubrics", qa_rubrics_lib),
    ("subject_motion", "Subject motion", subject_motion_lib),
    ("scene_motion", "Scene motion", scene_motion_lib),
    ("continuity", "Continuity locks", continuity_lib),
];

#[cfg(feature = "commands")]
fn library(id: &str) -> Option<&'static Lib> {
    LIBRARIES
        .iter()
        .find(|(k, _, _)| *k == id)
        .map(|(_, _, f)| f())
}

// ---------------------------------------------------------------------------
// Small value helpers
// ---------------------------------------------------------------------------

fn is_object(v: &Value) -> bool {
    v.is_object()
}

fn nonblank(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn num_to_js(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    match n.as_f64() {
        Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
        Some(f) => f.to_string(),
        None => n.to_string(),
    }
}

/// JS `${value}` for the shapes a contract holds; empty for missing values.
fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => num_to_js(n),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|x| !x.is_null())
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => true,
    }
}

#[cfg(feature = "commands")]
fn title_case_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn bad(code: &str, message: impl Into<String>) -> RecipeError {
    RecipeError::new(code, message)
}

#[cfg(feature = "commands")]
fn to_slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(feature = "commands")]
/// Sort `keys` by where each first appears in the JSON source, which keeps the
/// authored order even when `serde_json` maps are alphabetical.
fn source_order(src: &str, keys: &mut [String]) {
    keys.sort_by_key(|k| src.find(&format!("\"{k}\":")).unwrap_or(usize::MAX));
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for the current instant.
fn iso_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let t = rightkit_time::DateTime::from_unix(now.as_secs() as i64, 0)
        .expect("offset 0 is always valid");
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        t.second,
        now.subsec_millis()
    )
}

#[cfg(feature = "commands")]
fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// JSON with sorted keys and JS number formatting, for stable hashing.
fn stable_stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Number(n) => num_to_js(n),
        Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(stable_stringify).collect::<Vec<_>>().join(",")
        ),
        Value::Object(o) => {
            let sorted: BTreeMap<&String, &Value> = o.iter().collect();
            format!(
                "{{{}}}",
                sorted
                    .iter()
                    .map(|(k, v)| format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        stable_stringify(v)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Ventures, content root, templates
// ---------------------------------------------------------------------------

pub fn normalize_venture(venture: &str) -> RecipeResult<String> {
    let n = venture.trim().to_lowercase();
    // A venture is a host brand code in lower case, or `na` (no brand).
    if !crate::brands::is_known(&n) {
        return Err(bad(
            "invalid_input",
            format!("Invalid video venture \"{venture}\""),
        ));
    }
    Ok(n)
}

#[cfg(feature = "commands")]
fn normalize_template_slug(slug: &str) -> RecipeResult<String> {
    let n = slug.trim();
    let ok = !n.is_empty()
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(bad(
            "invalid_input",
            format!("Invalid shot_contract_template \"{slug}\""),
        ));
    }
    Ok(n.to_string())
}

#[cfg(feature = "commands")]
/// Input `content_root`, else `RIGHTKIT_CONTENT_ROOT`, else `<data_dir>/Content`.
fn content_root(ctx: &Context, input: &Value) -> PathBuf {
    if let Some(p) = nonblank(input.get("content_root")) {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("RIGHTKIT_CONTENT_ROOT").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    ctx.data_dir.join("Content")
}

#[cfg(feature = "commands")]
/// Refuse any path that escapes `root`.
fn resolve_inside(root: &Path, parts: &[&str]) -> RecipeResult<PathBuf> {
    let mut candidate = root.to_path_buf();
    for p in parts {
        candidate.push(p);
    }
    let escapes = candidate
        .strip_prefix(root)
        .map(|rel| {
            rel.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        })
        .unwrap_or(true);
    if escapes {
        return Err(bad(
            "invalid_input",
            format!(
                "Refusing to read outside video template root: {}",
                candidate.display()
            ),
        ));
    }
    Ok(candidate)
}

#[cfg(feature = "commands")]
fn unwrap_shot_contract(parsed: &Value) -> &Value {
    match parsed.get("shot_contract") {
        Some(c) if truthy(Some(c)) => c,
        _ => parsed,
    }
}

#[cfg(feature = "commands")]
fn has_time_segments(contract: &Value) -> bool {
    contract
        .get("time_segments")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
}

#[derive(Debug, Clone)]
pub struct Template {
    pub slug: String,
    pub name: String,
    pub path: Option<String>,
    pub summary: String,
    pub source: &'static str,
    pub contract: Value,
}

impl Template {
    #[cfg(feature = "commands")]
    fn to_json(&self, with_contract: bool) -> Value {
        let mut v = json!({
            "slug": self.slug,
            "name": self.name,
            "path": self.path,
            "summary": self.summary,
            "source": self.source,
        });
        if with_contract {
            v["contract"] = self.contract.clone();
        }
        v
    }
}

#[cfg(feature = "commands")]
fn template_from_parsed(
    slug: &str,
    parsed: &Value,
    path: Option<String>,
    source: &'static str,
) -> Option<Template> {
    let contract = unwrap_shot_contract(parsed);
    if !has_time_segments(contract) {
        return None;
    }
    let name = nonblank(parsed.get("name"))
        .or_else(|| nonblank(parsed.get("_template_name")))
        .unwrap_or_else(|| title_case_slug(slug));
    let summary = nonblank(parsed.get("summary"))
        .or_else(|| nonblank(parsed.get("_purpose")))
        .or_else(|| nonblank(contract.get("primary_action")))
        .unwrap_or_default();
    Some(Template {
        slug: slug.to_string(),
        name,
        path,
        summary,
        source,
        contract: contract.clone(),
    })
}

#[cfg(feature = "commands")]
/// Templates stored on disk for one venture, with the files that were skipped.
pub fn list_disk_templates(
    content_root: &Path,
    venture: &str,
) -> RecipeResult<(Vec<Template>, Vec<Value>)> {
    let venture = normalize_venture(venture)?;
    let dir = resolve_inside(content_root, &[&venture, "shot_contract_templates"])?;
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok((out, skipped));
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()));
        let slug = name
            .trim_end_matches(".json")
            .trim_end_matches(".JSON")
            .to_string();
        match parsed {
            Ok(p) => match template_from_parsed(&slug, &p, Some(to_slash(&path)), "content") {
                Some(t) => out.push(t),
                None => skipped
                    .push(json!({"path": to_slash(&path), "reason": "missing time_segments"})),
            },
            Err(reason) => skipped.push(json!({"path": to_slash(&path), "reason": reason})),
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok((out, skipped))
}

#[cfg(feature = "commands")]
fn builtin_templates(venture: &str) -> Vec<Template> {
    let list = templates_lib()
        .json
        .get("templates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    list.iter()
        .filter(|t| {
            t.get("ventures")
                .and_then(Value::as_array)
                .is_some_and(|vs| {
                    vs.iter()
                        .any(|v| v.as_str() == Some("*") || v.as_str() == Some(venture))
                })
        })
        .filter_map(|t| {
            let slug = t.get("slug").and_then(Value::as_str)?;
            template_from_parsed(slug, t, None, "builtin")
        })
        .collect()
}

#[cfg(feature = "commands")]
/// Built-in starters plus the venture's on-disk templates; disk wins on a slug clash.
pub fn list_templates(
    content_root: &Path,
    venture: &str,
) -> RecipeResult<(Vec<Template>, Vec<Value>)> {
    let venture = normalize_venture(venture)?;
    let (disk, skipped) = list_disk_templates(content_root, &venture)?;
    let mut all: Vec<Template> = builtin_templates(&venture)
        .into_iter()
        .filter(|b| !disk.iter().any(|d| d.slug == b.slug))
        .collect();
    all.extend(disk);
    all.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok((all, skipped))
}

#[cfg(feature = "commands")]
pub fn read_template(content_root: &Path, venture: &str, slug: &str) -> RecipeResult<Template> {
    let venture = normalize_venture(venture)?;
    let slug = normalize_template_slug(slug)?;
    let path = resolve_inside(
        content_root,
        &[&venture, "shot_contract_templates", &format!("{slug}.json")],
    )?;
    if path.exists() {
        let parsed: Value = std::fs::read_to_string(&path)
            .map_err(|e| bad("io", e.to_string()))
            .and_then(|s| {
                serde_json::from_str(&s).map_err(|e| bad("invalid_input", e.to_string()))
            })?;
        return template_from_parsed(&slug, &parsed, Some(to_slash(&path)), "content").ok_or_else(
            || {
                bad(
                    "invalid_input",
                    format!("shot_contract_template \"{slug}\" is invalid: time_segments required"),
                )
            },
        );
    }
    builtin_templates(&venture)
        .into_iter()
        .find(|t| t.slug == slug)
        .ok_or_else(|| {
            bad(
                "not_found",
                format!("shot_contract_template \"{slug}\" not found for venture \"{venture}\""),
            )
        })
}

// ---------------------------------------------------------------------------
// Deep merge (arrays replace, objects merge, unsafe keys refused)
// ---------------------------------------------------------------------------

fn assert_no_unsafe_merge_keys(v: &Value, trail: &mut Vec<String>) -> RecipeResult<()> {
    match v {
        Value::Object(o) => {
            for (k, child) in o {
                trail.push(k.clone());
                if UNSAFE_MERGE_KEYS.contains(&k.as_str()) {
                    return Err(bad(
                        "invalid_input",
                        format!("Unsafe override key \"{k}\" at {}", trail.join(".")),
                    ));
                }
                assert_no_unsafe_merge_keys(child, trail)?;
                trail.pop();
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                trail.push(i.to_string());
                assert_no_unsafe_merge_keys(child, trail)?;
                trail.pop();
            }
        }
        _ => {}
    }
    Ok(())
}

fn merge_plain(template: Option<&Value>, over: Option<&Value>) -> Option<Value> {
    let template = template.filter(|v| !v.is_null());
    let over = over.filter(|v| !v.is_null());
    let Some(over) = over else {
        return template.cloned();
    };
    let Some(template) = template else {
        return Some(over.clone());
    };
    match (template, over) {
        (Value::Object(t), Value::Object(o)) => {
            let mut out: Map<String, Value> = t.clone();
            for (k, v) in o {
                match merge_plain(t.get(k), Some(v)) {
                    Some(m) => {
                        out.insert(k.clone(), m);
                    }
                    None => {
                        out.remove(k);
                    }
                }
            }
            Some(Value::Object(out))
        }
        _ => Some(over.clone()),
    }
}

pub fn merge_shot_contract(template: &Value, over: &Value) -> RecipeResult<Value> {
    assert_no_unsafe_merge_keys(over, &mut Vec::new())?;
    Ok(merge_plain(Some(template), Some(over)).unwrap_or(Value::Null))
}

// ---------------------------------------------------------------------------
// Shot-contract validator
// ---------------------------------------------------------------------------

fn is_blank_string(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.trim().is_empty())
}

/// Validate a `shot_contract`. `Ok` carries advisory warnings; `Err` is the
/// same message the baseline validator threw.
pub fn validate_shot_contract(contract: &Value, shot_id: &str) -> Result<Vec<String>, String> {
    if !is_object(contract) {
        return Err(format!(
            "shot_contract MISSING for shot \"{shot_id}\". Every shot must declare a shot_contract block with: {}. See recipes/video/prompt-contract.json for the schema and an example.",
            REQUIRED_TOP_FIELDS.join(", ")
        ));
    }
    let mut missing: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for f in REQUIRED_TOP_FIELDS {
        match field(contract, f) {
            None => missing.push((*f).into()),
            Some(v) if is_blank_string(v) => empty.push((*f).into()),
            Some(Value::Array(a)) if a.is_empty() => empty.push((*f).into()),
            _ => {}
        }
    }

    if let Some(rb) = contract.get("role_blocking").filter(|v| v.is_object()) {
        for f in REQUIRED_ROLE_BLOCKING_FIELDS {
            match field(rb, f) {
                None => missing.push(format!("role_blocking.{f}")),
                Some(v) if is_blank_string(v) => empty.push(format!("role_blocking.{f}")),
                _ => {}
            }
        }
    }

    if truthy(contract.get("device_role")) {
        let ok = contract
            .get("device_role")
            .and_then(Value::as_str)
            .is_some_and(|r| ALLOWED_DEVICE_ROLES.contains(&r));
        if !ok {
            return Err(format!(
                "shot_contract.device_role for shot \"{shot_id}\" is \"{}\" — must be one of: {}. Even shots with NO device must declare 'absent' explicitly so we verify the model isn't tempted to invent one.",
                text(contract.get("device_role")),
                ALLOWED_DEVICE_ROLES.join(", ")
            ));
        }
    }

    if let Some(segs) = contract.get("time_segments").and_then(Value::as_array) {
        for (i, seg) in segs.iter().enumerate() {
            if !is_object(seg) {
                missing.push(format!("time_segments[{i}]"));
                continue;
            }
            for f in REQUIRED_SEGMENT_FIELDS {
                match field(seg, f) {
                    None => missing.push(format!("time_segments[{i}].{f}")),
                    Some(v) if is_blank_string(v) => empty.push(format!("time_segments[{i}].{f}")),
                    _ => {}
                }
            }
        }
    }

    let negatives = contract.get("negative_prompt").and_then(Value::as_array);
    if let Some(n) = negatives {
        if n.len() < MIN_NEGATIVES {
            empty.push(format!(
                "negative_prompt (has {}, need ≥{MIN_NEGATIVES} — include action-class AND artifact-class negatives)",
                n.len()
            ));
        }
    }

    if let Some(audio) = contract.get("audio_lock").filter(|v| v.is_object()) {
        if !truthy(audio.get("mode")) {
            missing.push("audio_lock.mode".into());
        } else if !audio
            .get("mode")
            .and_then(Value::as_str)
            .is_some_and(|m| ALLOWED_AUDIO_MODES.contains(&m))
        {
            return Err(format!(
                "shot_contract.audio_lock.mode for shot \"{shot_id}\" is \"{}\" — must be one of: {}.",
                text(audio.get("mode")),
                ALLOWED_AUDIO_MODES.join(", ")
            ));
        }
    }

    if contract
        .get("allowed_contact_surfaces")
        .and_then(Value::as_array)
        .is_some_and(|a| a.is_empty())
    {
        empty.push("allowed_contact_surfaces (must have ≥1 entry — use \"none\" if no new contact allowed)".into());
    }

    if let Some(objs) = contract.get("tracked_objects").and_then(Value::as_array) {
        for (i, obj) in objs.iter().enumerate() {
            if !is_object(obj) {
                missing.push(format!("tracked_objects[{i}]"));
                continue;
            }
            for f in ["name", "allowed_state_change", "forbidden_state_change"] {
                let v = obj.get(f);
                if !truthy(v) || v.is_some_and(is_blank_string) {
                    missing.push(format!("tracked_objects[{i}].{f}"));
                }
            }
        }
    }

    if let Some(n) = negatives {
        if n.len() > MAX_NEGATIVES {
            warnings.push(format!(
                "negative_prompt has {} entries (>{MAX_NEGATIVES}) — negative overload reduces compliance. Consider pushing object-state controls into tracked_objects instead.",
                n.len()
            ));
        }
    }

    if !missing.is_empty() || !empty.is_empty() {
        let mut lines = vec![
            format!("shot_contract VALIDATION FAILED for shot \"{shot_id}\"."),
            "This is the methodology gate locked 2026-05-05 after $50 of HR ad failures."
                .to_string(),
            String::new(),
        ];
        if !missing.is_empty() {
            lines.push(format!("MISSING required fields: {}", missing.join(", ")));
        }
        if !empty.is_empty() {
            lines.push(format!("EMPTY required fields: {}", empty.join(", ")));
        }
        lines.push(String::new());
        lines.push("See tools/recipes/video/prompt-contract.json for the schema.".to_string());
        return Err(lines.join("\n"));
    }
    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Model routes (renderer family, quirks, reference shape)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Route {
    pub model: String,
    pub family: String,
    pub quirks: Option<String>,
    pub ref_kind: String,
    pub min_refs: u64,
    pub max_refs: u64,
    pub end_frame: String,
}

impl Route {
    #[cfg(feature = "commands")]
    fn to_json(&self) -> Value {
        json!({
            "model": self.model, "family": self.family, "quirks": self.quirks,
            "ref_kind": self.ref_kind, "min_refs": self.min_refs, "max_refs": self.max_refs,
            "end_frame": self.end_frame,
        })
    }
}

/// Exact, case-insensitive model-id match against `model_routes.json`. A model
/// with no entry, or an entry without a family, is refused; nothing is guessed
/// from substrings.
pub fn resolve_route(model: &str) -> RecipeResult<Route> {
    let key = model.trim().to_lowercase();
    let routes = routes_lib()
        .json
        .get("routes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for r in &routes {
        let m = r
            .get("match")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let Some(family) = r.get("family").and_then(Value::as_str) else {
            continue;
        };
        if !m.is_empty() && key == m {
            return Ok(Route {
                model: model.to_string(),
                family: family.into(),
                quirks: r.get("quirks").and_then(Value::as_str).map(String::from),
                ref_kind: r
                    .get("ref_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .into(),
                min_refs: r.get("min_refs").and_then(Value::as_u64).unwrap_or(0),
                max_refs: r.get("max_refs").and_then(Value::as_u64).unwrap_or(0),
                end_frame: r
                    .get("end_frame")
                    .and_then(Value::as_str)
                    .unwrap_or("unsupported")
                    .into(),
            });
        }
    }
    Err(bad(
        "unsupported_model",
        format!("prompt-contract: unknown model family for \"{model}\": no exact route in data/video_templates/model_routes.json. Add an explicit route for this engine model id before using it with the video quality system."),
    ))
}

fn model_supports_lipsync(model: &str) -> bool {
    let k = model.to_lowercase();
    LIPSYNC_CAPABLE_MODEL_PREFIXES
        .iter()
        .any(|p| k.starts_with(p))
}

// ---------------------------------------------------------------------------
// Contract renderers (veo / kling / kling_cinema_studio / seedance)
// ---------------------------------------------------------------------------

fn render_role_blocking(rb: &Value) -> String {
    let g = |k: &str| nonblank(rb.get(k));
    let mut parts = Vec::new();
    if let Some(v) = g("body_orientation") {
        parts.push(format!("Body: {v}"));
    }
    if let Some(v) = g("head_position") {
        parts.push(format!("head {v}"));
    }
    if let Some(v) = g("gaze") {
        parts.push(format!("gaze {v}"));
    }
    if let Some(v) = g("hands_left") {
        parts.push(format!("left hand {v}"));
    }
    if let Some(v) = g("hands_right") {
        parts.push(format!("right hand {v}"));
    }
    format!("{}.", parts.join("; "))
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| text(Some(x))).collect())
        .unwrap_or_default()
}

fn render_allowed_surfaces(v: Option<&Value>) -> String {
    let arr = str_list(v);
    if arr.is_empty() {
        return String::new();
    }
    if arr.len() == 1 && arr[0] == "none" {
        return "No new surface contact allowed.".into();
    }
    format!("Allowed contact surfaces: {}.", arr.join(", "))
}

fn render_tracked_objects(v: Option<&Value>) -> String {
    let Some(arr) = v.and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return String::new();
    };
    arr.iter()
        .map(|o| {
            format!(
                "{}: allowed state changes — {}; forbidden state changes — {}.",
                text(o.get("name")),
                text(o.get("allowed_state_change")),
                text(o.get("forbidden_state_change"))
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_identity_lock(v: Option<&Value>) -> String {
    let Some(id) = v.filter(|x| x.is_object()) else {
        return String::new();
    };
    let mut bits = Vec::new();
    if let Some(a) = nonblank(id.get("appearance_descriptor")) {
        bits.push(format!("Identity lock: {a}"));
    }
    if let Some(c) = nonblank(id.get("character_id")) {
        bits.push(format!("(character_id={c})"));
    }
    bits.join(" ")
}

fn render_audio_lock(v: Option<&Value>) -> String {
    let Some(a) = v.filter(|x| x.is_object()) else {
        return String::new();
    };
    let mode = text(a.get("mode"));
    let base = match mode.as_str() {
        "silent" => "Output is silent — no audio track.".to_string(),
        "ambient" => "Ambient atmospheric audio only — no dialogue, no foley.".to_string(),
        "foley" => "Action-driven foley audio — no dialogue or music.".to_string(),
        "dialogue" => "Subject speaks dialogue (model generates voice).".to_string(),
        "lipsync_to_external_vo" => "Subject mouths to externally-mixed VO.".to_string(),
        _ => format!("Audio mode: {mode}."),
    };
    match nonblank(a.get("description")) {
        Some(d) => format!("{base} {d}"),
        None => base,
    }
}

fn segments(contract: &Value) -> Vec<&Value> {
    contract
        .get("time_segments")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn segs_paragraphs(contract: &Value) -> String {
    segments(contract)
        .iter()
        .map(|s| {
            format!(
                "From {}-{}s: camera {} Action: {} Forbidden: {}",
                text(s.get("start_s")),
                text(s.get("end_s")),
                text(s.get("camera")),
                text(s.get("action")),
                text(s.get("forbidden_zone"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn segs_semicolons(contract: &Value) -> String {
    segments(contract)
        .iter()
        .map(|s| {
            format!(
                "({}-{}s) {} (forbidden: {})",
                text(s.get("start_s")),
                text(s.get("end_s")),
                text(s.get("action")),
                text(s.get("forbidden_zone"))
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn to_timestamp(v: Option<&Value>) -> String {
    let secs = v.and_then(Value::as_f64).unwrap_or(0.0).floor() as i64;
    format!("00:{secs:02}")
}

fn segs_timestamp_blocks(contract: &Value) -> String {
    segments(contract)
        .iter()
        .map(|s| {
            format!(
                "[{}-{}] {}. {} Forbidden: {}.",
                to_timestamp(s.get("start_s")),
                to_timestamp(s.get("end_s")),
                text(s.get("action")),
                text(s.get("camera")),
                text(s.get("forbidden_zone"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn negatives_comma(contract: &Value) -> String {
    str_list(contract.get("negative_prompt")).join(", ")
}

fn negatives_no_x(contract: &Value) -> String {
    str_list(contract.get("negative_prompt"))
        .into_iter()
        .map(|n| {
            if n.len() >= 3 && n[..3].eq_ignore_ascii_case("no ") {
                n
            } else {
                format!("no {n}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_nonempty(lines: Vec<String>) -> String {
    lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn camera_lock(contract: &Value) -> String {
    nonblank(contract.get("camera_lock")).unwrap_or_else(|| DEFAULT_CAMERA_LOCK.into())
}

fn lighting_lock(contract: &Value) -> String {
    nonblank(contract.get("lighting_lock")).unwrap_or_else(|| DEFAULT_LIGHTING_LOCK.into())
}

fn model_override<'a>(contract: &'a Value, family: &str) -> Option<&'a Value> {
    contract
        .get("model_overrides")
        .and_then(|o| o.get(family))
        .filter(|v| !v.is_null())
}

fn render_for_veo(c: &Value) -> String {
    let rb = render_role_blocking(c.get("role_blocking").unwrap_or(&Value::Null));
    let end_state = nonblank(c.get("end_state"))
        .map(|e| format!("End state: {e}."))
        .unwrap_or_default();
    let extra = model_override(c, "veo")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    join_nonempty(vec![
        format!(
            "Animate the source frame. {}.",
            text(c.get("primary_action"))
        ),
        format!(
            "Source frame already shows: {}",
            text(c.get("source_frame_read"))
        ),
        render_identity_lock(c.get("identity_lock")),
        rb,
        format!(
            "Device role: {}. Motion budget: {}.",
            text(c.get("device_role")),
            text(c.get("motion_budget"))
        ),
        render_allowed_surfaces(c.get("allowed_contact_surfaces")),
        render_tracked_objects(c.get("tracked_objects")),
        segs_paragraphs(c),
        format!("Camera: {}", camera_lock(c)),
        format!("Lighting: {}", lighting_lock(c)),
        render_audio_lock(c.get("audio_lock")),
        end_state,
        format!("Negative: {}.", negatives_comma(c)),
        extra,
    ])
}

fn kling_cam(c: &Value) -> String {
    model_override(c, "kling")
        .and_then(|o| nonblank(o.get("camera")))
        .unwrap_or_else(|| camera_lock(c))
}

/// The ACTION body shared by both Kling layouts.
fn kling_action(c: &Value) -> String {
    let rb = render_role_blocking(c.get("role_blocking").unwrap_or(&Value::Null));
    let end_state = nonblank(c.get("end_state"))
        .map(|e| format!(" End state: {e}."))
        .unwrap_or_default();
    format!(
        "{}. Source frame shows: {} {} {}. Motion budget: {}. Device is {}. {} {}{}",
        text(c.get("primary_action")),
        text(c.get("source_frame_read")),
        rb,
        segs_semicolons(c),
        text(c.get("motion_budget")),
        text(c.get("device_role")),
        render_allowed_surfaces(c.get("allowed_contact_surfaces")),
        render_tracked_objects(c.get("tracked_objects")),
        end_state
    )
}

fn render_for_kling(c: &Value) -> String {
    let ov = model_override(c, "kling");
    let o = |k: &str| ov.and_then(|v| nonblank(v.get(k)));
    let identity = render_identity_lock(c.get("identity_lock"));
    let audio = render_audio_lock(c.get("audio_lock"));
    let id_part = if identity.is_empty() {
        String::new()
    } else {
        format!(". {identity}")
    };
    [
        format!(
            "SCENE: {}",
            o("scene").unwrap_or_else(|| "(motion-only — image establishes scene)".into())
        ),
        format!(
            "CHARACTERS: {}",
            o("characters").unwrap_or_else(|| format!(
                "{}{} (motion-only — image establishes appearance)",
                text(c.get("subject_focus")),
                id_part
            ))
        ),
        format!("ACTION: {}", o("action").unwrap_or_else(|| kling_action(c))),
        format!("CAMERA: {}. Lighting: {}", kling_cam(c), lighting_lock(c)),
        format!(
            "AUDIO/STYLE: {}",
            o("audio_style")
                .or_else(|| Some(audio).filter(|a| !a.is_empty()))
                .unwrap_or_else(|| "(motion-only — image establishes style)".into())
        ),
        format!("NEGATIVE: {}.", negatives_no_x(c)),
    ]
    .join("\n")
}

fn render_for_kling_cinema_studio(c: &Value) -> String {
    let ov = model_override(c, "kling");
    let o = |k: &str| ov.and_then(|v| nonblank(v.get(k)));
    let identity = render_identity_lock(c.get("identity_lock"));
    let audio = render_audio_lock(c.get("audio_lock"));
    let id_part = if identity.is_empty() {
        String::new()
    } else {
        format!(". {identity}")
    };
    [
        format!("CAMERA: {}", kling_cam(c)),
        format!("SUBJECT: {}{}", text(c.get("subject_focus")), id_part),
        format!("ACTION: {}", kling_action(c)),
        format!(
            "ENVIRONMENT: {}",
            o("scene").unwrap_or_else(|| "(motion-only — image establishes environment)".into())
        ),
        format!("LIGHTING: {}", lighting_lock(c)),
        format!(
            "STYLE: {}",
            o("audio_style")
                .or_else(|| Some(audio).filter(|a| !a.is_empty()))
                .unwrap_or_else(|| "cinematic editorial portrait style".into())
        ),
        format!("NEGATIVE: {}.", negatives_no_x(c)),
    ]
    .join("\n")
}

fn render_for_seedance(c: &Value) -> String {
    let ov = model_override(c, "seedance");
    let o = |k: &str| ov.and_then(|v| nonblank(v.get(k)));
    let rb = render_role_blocking(c.get("role_blocking").unwrap_or(&Value::Null));
    let end_state = nonblank(c.get("end_state"))
        .map(|e| format!("End state: {e}."))
        .unwrap_or_default();
    let camera_model = o("camera_model")
        .map(|m| format!(" Shot on {m}."))
        .unwrap_or_default();
    let mut tail = String::new();
    if let Some(d) = o("dialogue") {
        tail.push_str(&format!("\nDialogue: {d}"));
    }
    if let Some(s) = o("sfx") {
        tail.push_str(&format!("\nSFX: {s}"));
    }
    if let Some(t) = o("technical_specs") {
        tail.push_str(&format!("\n{t}"));
    }
    join_nonempty(vec![
        segs_timestamp_blocks(c),
        "Maintain exact appearance from reference image, consistent character throughout, no deformation or drift.".into(),
        format!("Source frame already shows: {}", text(c.get("source_frame_read"))),
        render_identity_lock(c.get("identity_lock")),
        rb,
        format!("Motion budget: {}. Device role: {}.", text(c.get("motion_budget")), text(c.get("device_role"))),
        render_allowed_surfaces(c.get("allowed_contact_surfaces")),
        render_tracked_objects(c.get("tracked_objects")),
        render_audio_lock(c.get("audio_lock")),
        end_state,
        format!("{}{}", camera_lock(c), camera_model),
        format!("Lighting: {}", lighting_lock(c)),
        format!("Negatives: {}.", negatives_no_x(c)),
        tail,
    ])
}

/// Wan 2.2 (wan-ultra, wan-hd). The engine takes one start frame (`image_url`)
/// and no end frame; quirks `wan-2-2` reward a short, single-subject motion with
/// a fixed camera and punish stacked verbs and long descriptions, so this stays compact.
fn render_for_wan(c: &Value) -> String {
    join_nonempty(vec![
        format!(
            "Animate the source frame. {}.",
            text(c.get("primary_action"))
        ),
        format!(
            "Source frame already shows: {}.",
            text(c.get("source_frame_read"))
        ),
        render_identity_lock(c.get("identity_lock")),
        format!("Motion budget: {}.", text(c.get("motion_budget"))),
        render_allowed_surfaces(c.get("allowed_contact_surfaces")),
        format!("Camera: {}", camera_lock(c)),
        format!("Lighting: {}", lighting_lock(c)),
        format!("Negative: {}.", negatives_no_x(c)),
    ])
}

/// HappyHorse 1.0 image-to-video (happy-horse). The engine takes one start
/// frame and drops `negative_prompt` for this model, so NO-X constraints are
/// carried in the prompt text. No per-model quirks entry exists for it, so the
/// layout follows the veo contract renderer without family-specific tuning.
fn render_for_happy(c: &Value) -> String {
    let rb = render_role_blocking(c.get("role_blocking").unwrap_or(&Value::Null));
    let end_state = nonblank(c.get("end_state"))
        .map(|e| format!("End state: {e}."))
        .unwrap_or_default();
    let extra = model_override(c, "happy")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    join_nonempty(vec![
        format!(
            "Animate the source frame. {}.",
            text(c.get("primary_action"))
        ),
        format!(
            "Source frame already shows: {}",
            text(c.get("source_frame_read"))
        ),
        render_identity_lock(c.get("identity_lock")),
        rb,
        format!(
            "Device role: {}. Motion budget: {}.",
            text(c.get("device_role")),
            text(c.get("motion_budget"))
        ),
        render_allowed_surfaces(c.get("allowed_contact_surfaces")),
        render_tracked_objects(c.get("tracked_objects")),
        segs_paragraphs(c),
        format!("Camera: {}", camera_lock(c)),
        format!("Lighting: {}", lighting_lock(c)),
        render_audio_lock(c.get("audio_lock")),
        end_state,
        format!("Negative: {}.", negatives_no_x(c)),
        extra,
    ])
}

/// FNV-style 32-bit mix over UTF-16 code units, as the baseline's audit hash.
pub fn compute_render_hash(contract: &Value, family: &str) -> String {
    let payload = stable_stringify(&json!({
        "family": family,
        "contract_version": CONTRACT_VERSION,
        "renderer_version": RENDERER_VERSION,
        "contract": contract,
    }));
    let units: Vec<u16> = payload.encode_utf16().collect();
    let len = units.len() as u32;
    let mut h1: u32 = 0xdead_beef ^ len;
    let mut h2: u32 = 0x41c6_ce57 ^ len;
    for c in units {
        let c = c as u32;
        h1 = (h1 ^ c).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ c).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    let s = format!("rc-{h2:08x}{h1:08x}");
    s.chars().take(19).collect()
}

pub struct Rendered {
    pub prompt: String,
    pub family: String,
    pub meta: Value,
}

/// Render a validated contract for `model`. Does not re-validate.
pub fn render_shot_contract(
    contract: &Value,
    model: &str,
    cinema_studio: bool,
    extra_warnings: &[String],
) -> RecipeResult<Rendered> {
    let route = resolve_route(model)?;
    let mut family = route.family.clone();
    let cinema = cinema_studio
        || contract
            .get("model_overrides")
            .and_then(|o| o.get("cinema_studio"))
            .and_then(Value::as_bool)
            == Some(true);
    if cinema && family == "kling" {
        family = "kling_cinema_studio".into();
    }
    let mut warnings: Vec<String> = extra_warnings.to_vec();
    warnings.extend(str_list(contract.get("_warnings")));
    if contract
        .get("audio_lock")
        .and_then(|a| a.get("mode"))
        .and_then(Value::as_str)
        == Some("lipsync_to_external_vo")
        && !model_supports_lipsync(model)
    {
        return Err(bad(
            "invalid_contract",
            format!(
                "shot_contract.audio_lock.mode = \"lipsync_to_external_vo\" is incompatible with model \"{model}\" (family \"{family}\"). Only InfiniteTalk is wired to external audio. Veo / Kling / Seedance / HappyHorse may hallucinate mouth motion (V2 HR ad failure class). Either change audio_lock.mode to one of: silent, ambient, foley, dialogue; change the model to infinitetalk; or face the subject AWAY from camera so no mouth is visible (back-of-shoulder / OTS / tight-on-hands framing)."
            ),
        ));
    }
    if family == "seedance" {
        if let Some(cm) =
            model_override(contract, "seedance").and_then(|o| nonblank(o.get("camera_model")))
        {
            warnings.push(format!("seedance.camera_model=\"{cm}\" is EXPERIMENTAL — not verified from Seedance primary docs. Adapter-specific until proven by API behavior."));
        }
    }
    let prompt = match family.as_str() {
        "veo" => render_for_veo(contract),
        "kling" => render_for_kling(contract),
        "kling_cinema_studio" => render_for_kling_cinema_studio(contract),
        "seedance" => render_for_seedance(contract),
        "wan" => render_for_wan(contract),
        "happy" => render_for_happy(contract),
        other => {
            return Err(bad(
                "unsupported_model",
                format!("prompt-contract: no renderer for family \"{other}\"."),
            ))
        }
    };
    let meta = json!({
        "contract_version": CONTRACT_VERSION,
        "renderer_version": RENDERER_VERSION,
        "family": family,
        "model_key": model,
        "render_hash": compute_render_hash(contract, &family),
        "rendered_at": iso_now(),
        "warnings": warnings,
        "retry_recommended": family == "kling_cinema_studio",
    });
    Ok(Rendered {
        prompt,
        family,
        meta,
    })
}

// ---------------------------------------------------------------------------
// Recipe libraries
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
fn normalize_recipe_item(slug: &str, item: &Value, extras: &[(&str, Value)]) -> Value {
    let mut o: Map<String, Value> = item.as_object().cloned().unwrap_or_default();
    for (k, v) in extras {
        o.insert((*k).into(), v.clone());
    }
    let name = nonblank(item.get("name")).unwrap_or_else(|| slug.to_string());
    let fragment = nonblank(item.get("prompt_fragment"))
        .or_else(|| nonblank(item.get("fragment")))
        .unwrap_or_default();
    let description = nonblank(item.get("description"))
        .or_else(|| nonblank(item.get("use")))
        .unwrap_or_default();
    o.insert("slug".into(), json!(slug));
    o.insert("name".into(), json!(name));
    o.insert("prompt_fragment".into(), json!(fragment));
    o.insert("description".into(), json!(description));
    Value::Object(o)
}

#[cfg(feature = "commands")]
fn ordered_keys(src: &str, obj: &Map<String, Value>) -> Vec<String> {
    let mut keys: Vec<String> = obj.keys().cloned().collect();
    source_order(src, &mut keys);
    keys
}

#[cfg(feature = "commands")]
/// Every item of a library, flattened, in the authored order.
fn flatten_library(lib: &Lib) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(cats) = lib.json.get("categories").and_then(Value::as_object) {
        for ck in ordered_keys(lib.src, cats) {
            let group = &cats[&ck];
            let label = nonblank(group.get("_label")).unwrap_or_else(|| ck.clone());
            if let Some(items) = group.get("items").and_then(Value::as_object) {
                for ik in ordered_keys(lib.src, items) {
                    out.push(normalize_recipe_item(
                        &ik,
                        &items[&ik],
                        &[("category", json!(ck)), ("category_label", json!(label))],
                    ));
                }
            }
        }
    } else if let Some(items) = lib.json.get("items").and_then(Value::as_object) {
        for ik in ordered_keys(lib.src, items) {
            let mut extras = Vec::new();
            if let Some(kind) = items[&ik].get("kind").cloned() {
                extras.push(("namespace", kind));
            }
            out.push(normalize_recipe_item(&ik, &items[&ik], &extras));
        }
    }
    out
}

#[cfg(feature = "commands")]
fn category_meta(lib: &Lib) -> Vec<Value> {
    let Some(cats) = lib.json.get("categories").and_then(Value::as_object) else {
        return Vec::new();
    };
    ordered_keys(lib.src, cats)
        .into_iter()
        .map(|k| {
            let g = &cats[&k];
            json!({
                "key": k,
                "label": nonblank(g.get("_label")).unwrap_or_else(|| k.clone()),
                "use_when": g.get("_use_when").cloned().unwrap_or(Value::Null),
                "count": g.get("items").and_then(Value::as_object).map(|i| i.len()).unwrap_or(0),
            })
        })
        .collect()
}

#[cfg(feature = "commands")]
fn library_view(id: &str, lib: &Lib) -> Value {
    let items = flatten_library(lib);
    let meta = lib.json.get("_meta");
    let mut v = json!({
        "library": id,
        "count": items.len(),
        "principle": meta.and_then(|m| m.get("principle")).cloned().unwrap_or(Value::Null),
        "categories": category_meta(lib),
        "brand_defaults": lib.json.get("_brand_defaults").cloned().unwrap_or(Value::Null),
        "items": items,
    });
    for extra in ["rules", "safe_zones"] {
        if let Some(x) = lib.json.get(extra) {
            v[extra] = x.clone();
        }
    }
    v
}

fn find_in_categories(lib: &Lib, key: &str) -> Option<(String, Value)> {
    let cats = lib.json.get("categories").and_then(Value::as_object)?;
    for (ck, group) in cats {
        if let Some(item) = group.get("items").and_then(|i| i.get(key)) {
            return Some((ck.clone(), item.clone()));
        }
    }
    None
}

#[cfg(feature = "commands")]
fn brand_default(lib: &Lib, brand: &str) -> Option<Value> {
    lib.json
        .get("_brand_defaults")
        .and_then(|d| d.get(brand))
        .cloned()
}

fn frag(item: &Value) -> String {
    nonblank(item.get("fragment")).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Prompt composer
// ---------------------------------------------------------------------------

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn compile_negatives(negatives: &[String], quirks_key: Option<&str>) -> (String, String) {
    if negatives.is_empty() {
        return (String::new(), "constraints_no_x".into());
    }
    let q = quirks_lib();
    let strategy = quirks_key
        .and_then(|k| q.json.get("models").and_then(|m| m.get(k)))
        .and_then(|s| s.get("negative_strategy"))
        .and_then(Value::as_str)
        .unwrap_or("constraints_no_x")
        .to_string();
    if strategy == "rewrite_to_positive" {
        let map = q.json.get("_constraint_rewrite_map");
        let rewritten: Vec<String> = negatives
            .iter()
            .map(|raw| {
                let lower = raw.to_lowercase().trim().to_string();
                let stripped = if let Some(rest) = lower.strip_prefix("no ") {
                    rest.trim().to_string()
                } else {
                    lower.clone()
                };
                let hit = |k: &str| {
                    map.and_then(|m| m.get(k))
                        .and_then(Value::as_str)
                        .map(String::from)
                };
                hit(&lower)
                    .or_else(|| hit(&stripped))
                    .or_else(|| hit(&format!("no {stripped}")))
                    .unwrap_or_else(|| format!("without {stripped}"))
            })
            .collect();
        return (
            format!("Frame characteristics: {}.", rewritten.join(", ")),
            strategy,
        );
    }
    let list: Vec<String> = negatives
        .iter()
        .map(|n| {
            if n.starts_with("NO ") {
                n.clone()
            } else {
                format!("NO {n}")
            }
        })
        .collect();
    (format!("Constraints: {}.", list.join(", ")), strategy)
}

/// Port of `composePrompt`. Inputs use the request field names.
pub fn compose_prompt(p: &Value) -> Result<(String, Value), String> {
    let subject = nonblank(p.get("subject")).ok_or("composePrompt: subject is required")?;
    let setting = nonblank(p.get("setting")).ok_or("composePrompt: setting is required")?;
    let action = nonblank(p.get("action")).ok_or("composePrompt: action is required")?;
    let camera_key = nonblank(p.get("camera_move")).ok_or(
        "composePrompt: cameraMove key required. List with: studio video_templates camera_moves",
    )?;
    let aesthetic_key = nonblank(p.get("aesthetic")).ok_or(
        "composePrompt: aesthetic key required. List with: studio video_templates recipes",
    )?;
    let brand = nonblank(p.get("brand"));
    let target = nonblank(p.get("model"));

    let (cat, move_spec) = find_in_categories(camera_moves_lib(), &camera_key)
        .ok_or_else(|| format!("Unknown camera move \"{camera_key}\". List available with: studio video_templates camera_moves"))?;
    let aest_spec = aesthetics_lib()
        .json
        .get("items")
        .and_then(|i| i.get(&aesthetic_key))
        .cloned()
        .ok_or_else(|| format!("Unknown aesthetic \"{aesthetic_key}\". List available with: studio video_templates recipes aesthetics"))?;

    let mut meta = json!({
        "schema_version": "1.0.0",
        "brand": brand,
        "camera_move": {"key": camera_key, "category": cat},
        "aesthetic": {"key": aesthetic_key},
        "brand_defaults_used": [],
        "has_product_spec": nonblank(p.get("product_spec")).is_some(),
        "has_negatives": !strings(p.get("negatives")).is_empty(),
        "target_model": target,
        "negative_strategy": Value::Null,
        "subject_motion": nonblank(p.get("subject_motion")),
        "scene_motion": nonblank(p.get("scene_motion")),
        "transition": nonblank(p.get("transition")),
        "continuity_locks": strings(p.get("continuity_locks")),
        "text_overlay": nonblank(p.get("text_overlay")),
        "audio_recipe": nonblank(p.get("audio_recipe")),
        "qa_rubric": nonblank(p.get("qa_rubric")),
    });

    let mut parts: Vec<String> = Vec::new();
    let mood_prefix = nonblank(p.get("mood"))
        .map(|m| format!("{m} "))
        .unwrap_or_default();
    let shot_prefix = nonblank(p.get("shot_type"))
        .map(|s| format!("{s} of"))
        .unwrap_or_else(|| "shot of".into());
    parts.push(format!(
        "A {mood_prefix}{shot_prefix} {subject} in {setting}."
    ));

    let move_fragment = frag(&move_spec).replace("[SUBJECT]", &subject);
    let timing_hint = p
        .get("duration")
        .filter(|v| v.is_number())
        .map(|d| format!(" over {} seconds", text(Some(d))))
        .unwrap_or_default();
    parts.push(format!(
        "The camera: {move_fragment}. Action: {action}{timing_hint}."
    ));

    if let Some(key) = nonblank(p.get("marketing_template")) {
        let spec = ad_patterns_lib()
            .json
            .get("items")
            .and_then(|i| i.get(&key))
            .ok_or_else(|| format!("Unknown ad pattern \"{key}\". List available with: studio video_templates ad_patterns"))?;
        parts.push(frag(spec));
        meta["ad_pattern"] = json!({"key": key});
    }
    if let Some(spec) = nonblank(p.get("product_spec")) {
        parts.push(format!("Product spec: {spec}."));
    }
    let colors = strings(p.get("colors"));
    let psych = nonblank(p.get("psych_effect"));
    if !colors.is_empty() || psych.is_some() {
        let palette = if colors.is_empty() {
            String::new()
        } else {
            format!("Color palette features {}", colors.join(", "))
        };
        let psych = psych
            .map(|e| format!(", reinforcing a {e} mood"))
            .unwrap_or_default();
        parts.push(format!("{palette}{psych}."));
    }
    if let Some(l) = nonblank(p.get("lighting")) {
        parts.push(format!("Lighting: {l}."));
    }
    if let Some(key) = nonblank(p.get("subject_motion")) {
        let (c, item) = find_in_categories(subject_motion_lib(), &key).ok_or_else(|| format!("Unknown subjectMotion \"{key}\". List with: studio video_templates recipes subject_motion"))?;
        parts.push(format!("{}.", frag(&item).replace("[SUBJECT]", &subject)));
        meta["subject_motion"] = json!({"key": key, "category": c});
    }
    if let Some(key) = nonblank(p.get("scene_motion")) {
        let (c, item) = find_in_categories(scene_motion_lib(), &key).ok_or_else(|| format!("Unknown sceneMotion \"{key}\". List with: studio video_templates recipes scene_motion"))?;
        parts.push(format!("{}.", frag(&item).replace("[SUBJECT]", &subject)));
        meta["scene_motion"] = json!({"key": key, "category": c});
    }
    let locks = strings(p.get("continuity_locks"));
    if !locks.is_empty() {
        let lock_types = continuity_lib()
            .json
            .get("lock_types")
            .and_then(Value::as_object);
        let mut clauses = Vec::new();
        let mut keys = Vec::new();
        for k in &locks {
            if !lock_types.is_some_and(|t| t.contains_key(k)) {
                let valid = lock_types
                    .map(|t| t.keys().cloned().collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                return Err(format!("Unknown continuityLock \"{k}\". Valid: {valid}"));
            }
            clauses.push(format!(
                "Continuity: preserve {} across the shot",
                k.replace('_', " ")
            ));
            keys.push(json!({"key": k}));
        }
        parts.push(format!("{}.", clauses.join(". ")));
        meta["continuity_locks"] = Value::Array(keys);
    }
    parts.push(frag(&aest_spec));

    if let Some(key) = nonblank(p.get("transition")) {
        let (c, item) = find_in_categories(transitions_lib(), &key).ok_or_else(|| format!("Unknown transition \"{key}\". List with: studio video_templates recipes transitions"))?;
        meta["transition"] = json!({"key": key, "category": c, "applies_to": item.get("applies_to").cloned().unwrap_or(Value::Null)});
    }
    if let Some(key) = nonblank(p.get("text_overlay")) {
        let (c, _) = find_in_categories(text_overlays_lib(), &key).ok_or_else(|| format!("Unknown textOverlay \"{key}\". List with: studio video_templates recipes text_overlays"))?;
        meta["text_overlay"] = json!({"key": key, "category": c});
    }
    if let Some(key) = nonblank(p.get("audio_recipe")) {
        let lib = audio_lib();
        let ns = ["voice_over_styles", "sfx_recipes", "music_bed_styles"]
            .into_iter()
            .find(|ns| lib.json.get(ns).and_then(|n| n.get(&key)).is_some());
        let Some(ns) = ns else {
            return Err(format!("Unknown audioRecipe \"{key}\". Search namespaces: voice_over_styles, sfx_recipes, music_bed_styles"));
        };
        meta["audio_recipe"] = json!({"key": key, "namespace": ns});
    }
    if let Some(key) = nonblank(p.get("qa_rubric")) {
        let rubrics = qa_rubrics_lib()
            .json
            .get("rubrics_by_shot_type")
            .and_then(Value::as_object);
        let Some(r) = rubrics.and_then(|r| r.get(&key)) else {
            let valid = rubrics
                .map(|r| r.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            return Err(format!("Unknown qaRubric \"{key}\". Valid: {valid}"));
        };
        meta["qa_rubric"] = json!({"key": key, "ship_threshold": r.get("ship_threshold").cloned().unwrap_or(Value::Null)});
    }

    let negatives = strings(p.get("negatives"));
    if !negatives.is_empty() {
        let quirks_key = target
            .as_deref()
            .and_then(|m| resolve_route(m).ok())
            .and_then(|r| r.quirks);
        let (compiled, strategy) = compile_negatives(&negatives, quirks_key.as_deref());
        if !compiled.is_empty() {
            parts.push(compiled);
        }
        meta["negative_strategy"] = json!(strategy);
    }
    Ok((parts.join(" "), meta))
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
fn require_string(input: &Value, key: &str) -> RecipeResult<String> {
    nonblank(input.get(key)).ok_or_else(|| bad("invalid_input", format!("{key} is required")))
}

pub fn slugify(value: &str) -> RecipeResult<String> {
    let mut out = String::new();
    let mut dash = false;
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    let slug: String = out
        .chars()
        .take(80)
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        return Err(bad("invalid_input", "project_slug is required"));
    }
    Ok(slug)
}

#[cfg(feature = "commands")]
const REF_FIELDS: &[&str] = &["start_frame_url", "end_frame_url", "ingredient_refs"];

#[cfg(feature = "commands")]
fn present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(_) => true,
    }
}

#[cfg(feature = "commands")]
fn reject_unexpected_refs(input: &Value, allowed: &[&str], ref_kind: &str) -> RecipeResult<()> {
    for f in REF_FIELDS {
        if !allowed.contains(f) && present(input.get(f)) {
            return Err(bad(
                "invalid_input",
                format!("{f} is not accepted for {ref_kind} video model"),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "commands")]
/// A reference must be an https URL of an image (the engine takes URLs).
fn normalize_image_ref(raw: Option<&Value>, field: &str) -> RecipeResult<String> {
    let Some(s) = raw
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(bad("invalid_input", format!("{field} is required")));
    };
    if !s.to_lowercase().starts_with("https://") || s.chars().any(char::is_whitespace) {
        return Err(bad(
            "invalid_input",
            format!("{field} must be an https URL (the engine takes URLs, not local paths): {s}"),
        ));
    }
    let path_part = s.split(['?', '#']).next().unwrap_or(s);
    let last = path_part.rsplit('/').next().unwrap_or("");
    if let Some((_, ext)) = last.rsplit_once('.') {
        if !["png", "jpg", "jpeg", "webp"].contains(&ext.to_lowercase().as_str()) {
            return Err(bad(
                "invalid_input",
                format!("{field} must be an image reference"),
            ));
        }
    }
    Ok(s.to_string())
}

#[cfg(feature = "commands")]
/// Validate the frame references the route needs. Returns `(refs, engine params)`.
fn resolve_refs(input: &Value, route: &Route) -> RecipeResult<(Value, Map<String, Value>)> {
    let mut params = Map::new();
    match route.ref_kind.as_str() {
        "flf" => {
            reject_unexpected_refs(input, &["start_frame_url", "end_frame_url"], "flf")?;
            let start = normalize_image_ref(input.get("start_frame_url"), "start_frame_url")?;
            params.insert("image_url".into(), json!(start));
            let mut flf = json!({"start_frame_url": start});
            if (route.min_refs >= 2 || route.end_frame == "required")
                && !present(input.get("end_frame_url"))
            {
                return Err(bad("invalid_input", format!("end_frame_url is required for {} (rule 1: storyboard start AND end frames)", route.model)));
            }
            if present(input.get("end_frame_url")) {
                let end = normalize_image_ref(input.get("end_frame_url"), "end_frame_url")?;
                params.insert("end_image_url".into(), json!(end));
                flf["end_frame_url"] = json!(end);
            }
            Ok((json!({"ref_kind": "flf", "flf": flf}), params))
        }
        "motion_control" => {
            reject_unexpected_refs(input, &["start_frame_url"], "motion_control")?;
            let start = normalize_image_ref(input.get("start_frame_url"), "start_frame_url")?;
            params.insert("image_url".into(), json!(start));
            Ok((json!({"ref_kind": "motion_control", "motion_control": {"start_frame_url": start}}), params))
        }
        "ingredient" => {
            reject_unexpected_refs(input, &["ingredient_refs"], "ingredient")?;
            let n = |k: u64| format!("{k} reference URL{}", if k == 1 { "" } else { "s" });
            let refs = input.get("ingredient_refs").and_then(Value::as_array).cloned().unwrap_or_default();
            if (refs.len() as u64) < route.min_refs {
                return Err(bad("invalid_input", format!("ingredient_refs must contain at least {} for {}", n(route.min_refs), route.model)));
            }
            if (refs.len() as u64) > route.max_refs {
                return Err(bad("invalid_input", format!("ingredient_refs may contain at most {} for {}", n(route.max_refs), route.model)));
            }
            let urls = refs
                .iter()
                .enumerate()
                .map(|(i, r)| normalize_image_ref(Some(r), &format!("ingredient_refs[{i}]")))
                .collect::<RecipeResult<Vec<_>>>()?;
            params.insert("reference_image_urls".into(), json!(urls));
            Ok((json!({"ref_kind": "ingredient", "ingredient": {"ingredient_refs": urls}}), params))
        }
        _ => Err(bad(
            "invalid_input",
            format!(
                "{} takes no start or end frame. Rule 1 requires storyboard start AND end frames before any video run; pick an image-to-video or first/last-frame model.",
                route.model
            ),
        )),
    }
}

#[cfg(feature = "commands")]
fn resolve_contract(ctx: &Context, input: &Value) -> RecipeResult<(Value, Option<String>)> {
    let template = nonblank(input.get("shot_contract_template"));
    let inline = field(input, "shot_contract_inline");
    let overrides = field(input, "shot_contract_overrides");
    if template.is_some() && inline.is_some() {
        return Err(bad(
            "invalid_input",
            "Use either shot_contract_template or shot_contract_inline, not both",
        ));
    }
    if template.is_none() && overrides.is_some() {
        return Err(bad(
            "invalid_input",
            "shot_contract_overrides requires shot_contract_template",
        ));
    }
    if let Some(inline) = inline {
        return Ok((merge_shot_contract(&json!({}), inline)?, None));
    }
    let Some(slug) = template else {
        return Err(bad(
            "invalid_input",
            "Either shot_contract_template or shot_contract_inline is required",
        ));
    };
    let venture = normalize_venture(
        &require_string(input, "venture").or_else(|_| require_string(input, "brand"))?,
    )?;
    let t = read_template(&content_root(ctx, input), &venture, &slug)?;
    let contract = match overrides {
        Some(o) => merge_shot_contract(&t.contract, o)?,
        None => t.contract,
    };
    Ok((contract, Some(t.slug)))
}

#[cfg(feature = "commands")]
/// The prompt and meta composed from the request fields and the contract.
fn compose_for_request(request: &Value, contract: &Value) -> RecipeResult<(String, Value)> {
    let brand = request.get("brand").and_then(Value::as_str).unwrap_or("");
    let mut defaults_used = Map::new();
    let mut pick = |key: &str, lib: &Lib, fallback: &str| -> String {
        if let Some(k) = nonblank(request.get(key)) {
            return k;
        }
        let spec = brand_default(lib, brand);
        let k = spec
            .as_ref()
            .and_then(|s| s.get("item_key"))
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string();
        defaults_used.insert(
            key.into(),
            json!({"key": k, "source": if spec.is_some() { "brand_default" } else { "fallback" }}),
        );
        k
    };
    let camera = pick("camera_move", camera_moves_lib(), "eye_level_static");
    let aesthetic = pick("aesthetic", aesthetics_lib(), "documentary");
    let prompt_text = nonblank(request.get("prompt")).unwrap_or_default();
    let contract_str = |k: &str| nonblank(contract.get(k));

    let mut p = request.clone();
    let set = |p: &mut Value, k: &str, v: Value| p[k] = v;
    set(&mut p, "camera_move", json!(camera));
    set(&mut p, "aesthetic", json!(aesthetic));
    if nonblank(request.get("subject")).is_none() {
        set(
            &mut p,
            "subject",
            json!(contract_str("subject_focus").unwrap_or_else(|| prompt_text.clone())),
        );
    }
    if nonblank(request.get("setting")).is_none() {
        set(
            &mut p,
            "setting",
            json!(contract_str("source_frame_read")
                .unwrap_or_else(|| "the approved reference frame".into())),
        );
    }
    if nonblank(request.get("action")).is_none() {
        set(
            &mut p,
            "action",
            json!(contract_str("primary_action").unwrap_or_else(|| prompt_text.clone())),
        );
    }
    if nonblank(request.get("product_spec")).is_none() {
        if let Some(m) = contract_str("motion_budget") {
            set(&mut p, "product_spec", json!(m));
        }
    }
    if nonblank(request.get("lighting")).is_none() {
        if let Some(l) = contract_str("lighting_lock") {
            set(&mut p, "lighting", json!(l));
        }
    }
    if strings(request.get("negatives")).is_empty() {
        set(
            &mut p,
            "negatives",
            json!(strings(contract.get("negative_prompt"))),
        );
    }
    let (prompt, mut meta) = compose_prompt(&p).map_err(|m| bad("invalid_input", m))?;
    meta["source"] = json!("video_preflight");
    meta["defaults_used"] = Value::Object(defaults_used);
    Ok((prompt, meta))
}

#[cfg(feature = "commands")]
fn normalize_request(input: &Value) -> RecipeResult<Value> {
    let prompt = require_string(input, "prompt")?;
    let brand = require_string(input, "brand")?.to_uppercase();
    if brand == crate::brands::NO_BRAND || !crate::brands::is_known(&brand) {
        return Err(bad(
            "invalid_input",
            format!(
                "brand must be one of: {}",
                crate::brands::codes().join(", ")
            ),
        ));
    }
    let project_slug = slugify(&require_string(input, "project_slug")?)?;
    let provider = require_string(input, "provider")?;
    let model = require_string(input, "model")?;
    let aspect = require_string(input, "aspect")?;
    let aspects: Vec<String> = routes_lib()
        .json
        .get("aspects")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !aspects.contains(&aspect) {
        return Err(bad(
            "invalid_input",
            format!("aspect must be one of: {}", aspects.join(", ")),
        ));
    }
    let duration = input
        .get("duration")
        .and_then(Value::as_f64)
        .filter(|d| d.is_finite() && *d > 0.0);
    let Some(duration) = duration else {
        return Err(bad(
            "invalid_input",
            "duration must be a positive number of seconds",
        ));
    };
    let mut r = json!({
        "brand": brand,
        "venture": normalize_venture(&brand.to_lowercase())?,
        "project_slug": project_slug,
        "provider": provider,
        "model": model,
        "prompt": prompt,
        "aspect": aspect,
        "duration": if duration.fract() == 0.0 { json!(duration as i64) } else { json!(duration) },
    });
    if let Some(res) = nonblank(input.get("resolution")) {
        r["resolution"] = json!(res);
    }
    if let Some(a) = input.get("generate_audio").and_then(Value::as_bool) {
        r["generate_audio"] = json!(a);
    }
    for f in [
        "camera_move",
        "marketing_template",
        "aesthetic",
        "subject",
        "setting",
        "action",
        "mood",
        "shot_type",
        "product_spec",
        "psych_effect",
        "lighting",
        "subject_motion",
        "scene_motion",
        "transition",
        "text_overlay",
        "audio_recipe",
        "qa_rubric",
        "shot_contract_template",
    ] {
        if let Some(s) = nonblank(input.get(f)) {
            r[f] = json!(s);
        }
    }
    for f in ["colors", "negatives", "continuity_locks"] {
        if let Some(arr) = input.get(f).and_then(Value::as_array) {
            let out = strings(input.get(f));
            if out.len() != arr.len() {
                return Err(bad(
                    "invalid_input",
                    format!("{f} must contain non-empty strings"),
                ));
            }
            r[f] = json!(out);
        }
    }
    Ok(r)
}

#[cfg(feature = "commands")]
fn drafts_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("video_drafts")
}

#[cfg(feature = "commands")]
static DRAFT_SEQ: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "commands")]
fn new_draft_id(project_slug: &str) -> String {
    let stamp: String = iso_now()
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    let nonce = fnv64(&format!(
        "{}-{}-{}",
        iso_now(),
        std::process::id(),
        DRAFT_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    format!("video-{stamp}-{project_slug}-{:08x}", nonce & 0xffff_ffff)
}

#[cfg(feature = "commands")]
fn valid_draft_id(id: &str) -> bool {
    let rest = id.strip_prefix("video-");
    rest.is_some_and(|r| {
        r.len() > 23
            && r.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && r.rsplit('-')
                .next()
                .is_some_and(|h| h.len() == 8 && h.chars().all(|c| c.is_ascii_hexdigit()))
    })
}

#[cfg(feature = "commands")]
fn draft_path(ctx: &Context, id: &str) -> RecipeResult<PathBuf> {
    if !valid_draft_id(id) {
        return Err(bad(
            "invalid_input",
            format!("invalid video draft id: {id}"),
        ));
    }
    Ok(drafts_dir(ctx).join(format!("{id}.json")))
}

#[cfg(feature = "commands")]
fn read_draft(ctx: &Context, id: &str) -> RecipeResult<Value> {
    let path = draft_path(ctx, id)?;
    let s = std::fs::read_to_string(&path)
        .map_err(|_| bad("not_found", format!("video draft not found: {id}")))?;
    serde_json::from_str(&s).map_err(|e| bad("io", format!("video draft {id} is unreadable: {e}")))
}

#[cfg(feature = "commands")]
fn write_draft(ctx: &Context, draft: &Value) -> RecipeResult<()> {
    let id = draft
        .get("draft_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = draft_path(ctx, id)?;
    rightkit_fs::ensure_private_dir(&drafts_dir(ctx)).map_err(|e| bad("io", e.to_string()))?;
    rightkit_fs::atomic_write(
        &path,
        serde_json::to_string_pretty(draft)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| bad("io", e.to_string()))
}

#[cfg(feature = "commands")]
fn engine_fingerprint(engine_request: &Value) -> String {
    format!("{:016x}", fnv64(&stable_stringify(engine_request)))
}

#[cfg(feature = "commands")]
/// The guarded preflight. Validates and composes only; nothing is submitted.
pub fn preflight(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let mut request = normalize_request(input)?;
    let model = request["model"].as_str().unwrap_or_default().to_string();
    let route = resolve_route(&model)?;
    let (refs, mut params) = resolve_refs(input, &route)?;
    request["refs"] = refs;

    let mut input_for_contract = input.clone();
    input_for_contract["venture"] = request["venture"].clone();
    let (contract, template_slug) = resolve_contract(ctx, &input_for_contract)?;
    if let Some(slug) = &template_slug {
        request["shot_contract_template"] = json!(slug);
    }
    let project = request["project_slug"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut warnings =
        validate_shot_contract(&contract, &project).map_err(|m| bad("invalid_contract", m))?;
    if route.end_frame == "unsupported" {
        warnings.push(format!(
            "{model} takes no end frame; single-frame image-to-video drifts (rule 1). Prefer a first/last-frame model for anchored shots."
        ));
    }

    let (composed_prompt, composed_meta) = compose_for_request(&request, &contract)?;
    let rendered = render_shot_contract(
        &contract,
        &model,
        input.get("cinema_studio").and_then(Value::as_bool) == Some(true),
        &warnings,
    )?;
    request["composer_meta"] = composed_meta.clone();

    params.insert("aspect".into(), request["aspect"].clone());
    params.insert("duration".into(), request["duration"].clone());
    if let Some(r) = request.get("resolution") {
        params.insert("resolution".into(), r.clone());
    }
    if let Some(a) = request.get("generate_audio") {
        params.insert("generate_audio".into(), a.clone());
    }
    let engine_request = json!({
        "kind": "video",
        "provider": request["provider"],
        "model": model,
        "prompt": rendered.prompt,
        "params": Value::Object(params),
    });
    // The engine's own dry run: validates duration/resolution/refs for the model
    // and prices the run without storing anything or touching a key.
    let estimate = rightkit_media_gen::api::generate(ctx, &engine_request, true)?;
    let est = estimate.get("est_usd").cloned().unwrap_or(Value::Null);

    let created_at = iso_now();
    let draft_id = new_draft_id(&project);
    let mut draft = json!({
        "schema_version": "1.0.0",
        "draft_id": draft_id,
        "status": "draft",
        "execution_mode": "preflight_only",
        "created_at": created_at,
        "request": request,
        "route": route.to_json(),
        "resolved_shot_contract": contract,
        "composed_prompt": composed_prompt,
        "composed_meta": composed_meta,
        "contract_prompt": rendered.prompt,
        "contract_meta": rendered.meta,
        "warnings": warnings,
        "estimated_cost_usd": est,
        "engine_request": engine_request,
        "fingerprint": engine_fingerprint(&engine_request),
        "approval": {"required": true, "approved": false, "approved_at": null, "approved_by": null},
        "pipeline": {
            "type": "guarded_video_preflight",
            "execution_note": "This draft validates the shot contract and frames and estimates spend only. A run needs operator approval of this exact draft.",
        },
    });
    draft["manifest_path"] = json!(to_slash(&draft_path(ctx, &draft_id)?));
    write_draft(ctx, &draft)?;
    // The single-use approval challenge travels with the response, not inside the stored draft.
    let (inputs, legacy) = approval_inputs_of(&draft);
    draft["approval_challenge"] =
        rightkit_media_gen_approvals::issue(ctx, "video_templates", &draft_id, &inputs, &legacy)?;
    Ok(draft)
}

#[cfg(feature = "commands")]
/// The draft fields an approval binds (the SHA-256 in `approvals` covers all of them), plus the
/// draft's existing audit hash for the UI to compare.
fn approval_inputs_of(draft: &Value) -> (Value, String) {
    let inputs = json!({
        "request": draft["request"],
        "route": draft["route"],
        "resolved_shot_contract": draft["resolved_shot_contract"],
        "composed_prompt": draft["composed_prompt"],
        "contract_prompt": draft["contract_prompt"],
        "engine_request": draft["engine_request"],
        "fingerprint": draft["fingerprint"],
    });
    (
        inputs,
        draft["fingerprint"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

#[cfg(feature = "commands")]
/// Record the user's in-app approval of a draft. Needs an approval token only the app shell can
/// mint (`approvals`); `operator_approved` alone is refused. Refuses a draft whose engine request
/// no longer matches the fingerprint taken at preflight, and one whose approval inputs changed
/// since the challenge was issued.
pub fn approve(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let gate = rightkit_media_gen_approvals::gate(input)?;
    let id = require_string(input, "draft_id")?;
    let mut draft = read_draft(ctx, &id)?;
    let current = engine_fingerprint(&draft["engine_request"]);
    if draft.get("fingerprint").and_then(Value::as_str) != Some(current.as_str()) {
        return Err(bad(
            "draft_modified",
            "The draft's engine request changed after preflight; run the preflight again.",
        ));
    }
    if draft.get("execution_mode").and_then(Value::as_str) != Some("preflight_only") {
        return Err(bad(
            "invalid_input",
            "Video run can only execute a preflight draft manifest",
        ));
    }
    let (inputs, legacy) = approval_inputs_of(&draft);
    let record = rightkit_media_gen_approvals::consume(
        ctx,
        &gate,
        "video_templates",
        &id,
        &inputs,
        &legacy,
    )?;
    let already = draft["approval"]["approved"].as_bool() == Some(true);
    if !already {
        draft["status"] = json!("approved");
        let mut approval = rightkit_media_gen_approvals::approval_fields(&record);
        approval.insert("required".into(), json!(true));
        approval.insert("approved".into(), json!(true));
        draft["approval"] = Value::Object(approval);
        write_draft(ctx, &draft)?;
    }
    Ok(draft)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[cfg(feature = "commands")]
fn handle_templates(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let venture = normalize_venture(
        &require_string(input, "venture")
            .map_err(|_| bad("invalid_input", "Invalid video venture \"\""))?,
    )?;
    let root = content_root(ctx, input);
    let (templates, skipped) = list_templates(&root, &venture)?;
    Ok(json!({
        "venture": venture,
        "content_root": to_slash(&root),
        "templates": templates.iter().map(|t| t.to_json(false)).collect::<Vec<_>>(),
        "skipped": skipped,
    }))
}

#[cfg(feature = "commands")]
fn handle_template(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let venture = normalize_venture(&nonblank(input.get("venture")).unwrap_or_default())?;
    let slug = nonblank(input.get("slug")).unwrap_or_default();
    Ok(read_template(&content_root(ctx, input), &venture, &slug)?.to_json(true))
}

#[cfg(feature = "commands")]
fn handle_library_list(id: &str, key: &str) -> RecipeResult<Value> {
    let lib = library(id).ok_or_else(|| {
        bad(
            "invalid_input",
            format!("Unsupported video recipe library \"{id}\""),
        )
    })?;
    let mut v = library_view(id, lib);
    v[key] = v["items"].clone();
    Ok(v)
}

#[cfg(feature = "commands")]
fn handle_recipes(input: &Value) -> RecipeResult<Value> {
    match nonblank(input.get("library")) {
        None => Ok(json!({
            "libraries": LIBRARIES.iter().map(|(id, label, f)| json!({"library": id, "label": label, "count": flatten_library(f()).len()})).collect::<Vec<_>>(),
        })),
        Some(id) => handle_library_list(&id, "recipes"),
    }
}

#[cfg(feature = "commands")]
fn handle_validate(input: &Value) -> RecipeResult<Value> {
    let contract = input.get("contract").cloned().unwrap_or(Value::Null);
    let shot = nonblank(input.get("shot_id")).unwrap_or_else(|| "<unknown>".into());
    Ok(match validate_shot_contract(&contract, &shot) {
        Ok(warnings) => json!({"ok": true, "warnings": warnings}),
        Err(error) => json!({"ok": false, "error": error, "warnings": []}),
    })
}

#[cfg(feature = "commands")]
fn handle_render(ctx: &Context, input: &Value) -> RecipeResult<Value> {
    let model = require_string(input, "model")?;
    let contract = match field(input, "contract") {
        Some(c) => c.clone(),
        None => resolve_contract(ctx, input)?.0,
    };
    let shot = nonblank(input.get("shot_id")).unwrap_or_else(|| "<unknown>".into());
    let warnings =
        validate_shot_contract(&contract, &shot).map_err(|m| bad("invalid_contract", m))?;
    let rendered = render_shot_contract(
        &contract,
        &model,
        input.get("cinema_studio").and_then(Value::as_bool) == Some(true),
        &warnings,
    )?;
    let mut out = json!({
        "prompt": rendered.prompt,
        "family": rendered.family,
        "meta": rendered.meta,
        "resolved_shot_contract": contract,
    });
    if nonblank(input.get("prompt")).is_some() {
        let mut request = input.clone();
        request["brand"] = json!(nonblank(input.get("brand"))
            .unwrap_or_default()
            .to_uppercase());
        let (composed, meta) = compose_for_request(&request, &contract)?;
        out["composed_prompt"] = json!(composed);
        out["composed_meta"] = meta;
    }
    Ok(out)
}

/// Per-model native camera parameter overlay (#279). Pure: the prompt text is never removed, so
/// an adapter that ignores the overlay still gets the prompt-text camera move. `mode` is
/// "native" when the model has a documented/unverified native field and the move maps to it,
/// else "prompt_only". Adapters apply `params` only when `status` is "documented" (or
/// "unverified" with explicit opt-in).
pub fn camera_param_overlay(model: &str, camera_key: &str) -> RecipeResult<Value> {
    let route = resolve_route(model)?;
    let lib = camera_params_lib();
    if find_in_categories(camera_moves_lib(), camera_key).is_none() {
        return Err(bad(
            "invalid_input",
            format!("Unknown camera move \"{camera_key}\""),
        ));
    }
    let mut out = json!({
        "model": route.model, "family": route.family, "camera_move": camera_key,
        "mode": "prompt_only", "params": {}, "prompt_fallback": true,
    });
    let key = route.model.trim().to_lowercase();
    let Some(entry) = lib.json.get("models").and_then(|m| m.get(&key)) else {
        return Ok(out);
    };
    let param = entry.get("param").and_then(Value::as_str).unwrap_or("");
    let field = entry
        .get("request_field")
        .and_then(Value::as_str)
        .unwrap_or("");
    let value = lib
        .json
        .get("moves")
        .and_then(|m| m.get(camera_key))
        .and_then(|m| m.get(param));
    if let Some(v) = value.filter(|v| !v.is_null()) {
        out["mode"] = json!("native");
        out["params"] = json!({ field: v });
        out["status"] = entry.get("status").cloned().unwrap_or(json!("unverified"));
    }
    Ok(out)
}

#[cfg(feature = "commands")]
fn handle_camera_params(input: &Value) -> RecipeResult<Value> {
    let model = require_string(input, "model")?;
    let key = require_string(input, "camera_move")?;
    camera_param_overlay(&model, &key)
}

#[cfg(feature = "commands")]
pub fn handle(ctx: &Context, name: &str, input: &Value) -> RecipeResult<Value> {
    match name {
        "studio.video_templates.templates" => handle_templates(ctx, input),
        "studio.video_templates.template" => handle_template(ctx, input),
        "studio.video_templates.camera_moves" => handle_library_list("camera_moves", "moves"),
        "studio.video_templates.ad_patterns" => handle_library_list("ad_patterns", "patterns"),
        "studio.video_templates.camera_params" => handle_camera_params(input),
        "studio.video_templates.recipes" => handle_recipes(input),
        "studio.video_templates.validate" => handle_validate(input),
        "studio.video_templates.render" => handle_render(ctx, input),
        "studio.video_templates.preflight" => preflight(ctx, input),
        "studio.video_templates.approve" => approve(ctx, input),
        "studio.video_templates.draft" => read_draft(ctx, &require_string(input, "draft_id")?),
        _ => Err(bad("unknown_command", format!("unknown command: {name}"))),
    }
}
