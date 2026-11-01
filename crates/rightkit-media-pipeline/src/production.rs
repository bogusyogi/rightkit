//! studio.production: the Production stack (campaign factory, canvas, director
//! shot plan, post plan, brand-kit check, save pack, hand-offs), ported from
//! the Media Console baseline a7271cf (`ProductionStackStudio.tsx`,
//! `production-stack-builders.ts`, `server/studio-production-stack.mjs`).
//!
//! Everything here is deterministic planning: no command calls a provider.
//! Hand-offs are requests the Image, Video and Voice views run when the
//! operator confirms. Copy and layout data lives in
//! `data/production/templates.json`.
//!
//! Commands (`studio.production.<verb>`):
//! - `templates`: channels, brands, pickers, defaults.
//! - `build`: campaign values -> campaign, canvas, director, post, hand-offs, pack.
//! - `brand_check`: lock ledger for a brand kit, with a file-exists check.
//! - `preflight`: validate a pack and report hand-off readiness (writes nothing).
//! - `save_pack`: validate and write a pack folder under `<data_dir>/packs/`.
//! - `packs`: list saved packs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "studio.production.templates",
    "studio.production.build",
    "studio.production.brand_check",
    "studio.production.preflight",
    "studio.production.save_pack",
    "studio.production.packs",
];

const TEMPLATES_JSON: &str = include_str!("../data/production/templates.json");
const SCHEMA_VERSION: &str = "1.0.0";
const PACK_KIND: &str = "campaign_production_pack";

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.production.templates" => Ok(templates().clone()),
        "studio.production.build" => build(input),
        "studio.production.brand_check" => brand_check(input),
        "studio.production.preflight" => preflight(input),
        "studio.production.save_pack" => save_pack(ctx, input),
        "studio.production.packs" => list_packs(ctx),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

fn invalid(message: impl Into<String>) -> GenError {
    GenError::new("invalid_input", message)
}

fn templates() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| {
        serde_json::from_str(TEMPLATES_JSON).expect("production templates.json is valid")
    })
}

// ---------------------------------------------------------------------------
// helpers

fn clean(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn field(input: &Value, key: &str) -> String {
    clean(input.get(key))
}

fn tstr(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Lowercase, collapse non-alphanumerics to '-', trim '-', cut to 80 chars,
/// like the baseline `slugify` (the cut happens after the trim).
pub fn slugify(value: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut pending = false;
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if pending && !slug.is_empty() {
                slug.push('-');
            }
            pending = false;
            slug.push(c);
        } else {
            pending = true;
        }
    }
    slug.truncate(80);
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn num(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e12 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

fn clamp_number(value: Option<&Value>, fallback: f64, min: f64, max: f64) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => n.round().clamp(min, max),
        _ => fallback,
    }
}

fn short_id() -> String {
    rightkit_logs::identity::random_uuid()
        .chars()
        .filter(|c| *c != '-')
        .take(8)
        .collect()
}

fn nearest_aspect(aspect: &str, allowed: &[String]) -> String {
    if allowed.iter().any(|a| a == aspect) {
        return aspect.to_string();
    }
    let ratio = |s: &str| -> Option<f64> {
        let (w, h) = s.split_once(':')?;
        let (w, h) = (w.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?);
        (w > 0.0 && h > 0.0).then(|| (w / h).ln())
    };
    let Some(target) = ratio(aspect) else {
        return allowed
            .first()
            .cloned()
            .unwrap_or_else(|| aspect.to_string());
    };
    let mut best: Option<(&String, f64)> = None;
    for a in allowed {
        if let Some(r) = ratio(a) {
            let d = (r - target).abs();
            if best.is_none_or(|(_, bd)| d < bd - 1e-9) {
                best = Some((a, d));
            }
        }
    }
    best.map(|(a, _)| a.clone())
        .unwrap_or_else(|| aspect.to_string())
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// brand kit

struct Kit {
    brand: String,
    locks: Value,
    missing: Vec<Value>,
}

fn lock(id: &str, value: &str, product: &str, offer: &str) -> Value {
    let spec = &templates()["locks"][id];
    let required = spec["required"].as_bool().unwrap_or(false);
    let status = if !value.is_empty() {
        "ready"
    } else if required {
        "missing"
    } else {
        "optional"
    };
    let mut note = tstr(spec, "note");
    match id {
        "product_ref" => note = format!("{note}: {product}"),
        "logo" => note = format!("{note}: {offer}"),
        _ => {}
    }
    json!({
        "id": id,
        "label": tstr(spec, "label"),
        "status": status,
        "value": if value.is_empty() { Value::Null } else { json!(value) },
        "note": note,
    })
}

fn resolve_brand(input: &Value) -> GenResult<String> {
    let t = templates();
    let brand = field(input, "brand");
    if brand.is_empty() {
        return Ok(tstr(t, "default_brand"));
    }
    if strings(t, "brands").contains(&brand) {
        Ok(brand)
    } else {
        Err(invalid(format!("unknown brand: {brand}")))
    }
}

fn summarize_kit(input: &Value, brand: &str, product: &str, offer: &str) -> Kit {
    let product_ref = lock(
        "product_ref",
        &field(input, "product_reference_path"),
        product,
        offer,
    );
    let logo = lock("logo", &field(input, "logo_path"), product, offer);
    let character = lock("character", &field(input, "character_id"), product, offer);
    let voice = lock("voice", &field(input, "voice_id"), product, offer);
    let missing: Vec<Value> = [&product_ref, &logo]
        .into_iter()
        .filter(|l| l["status"] == "missing")
        .cloned()
        .map(|mut l| {
            // Baseline `missing` entries keep the bare lock note; only `locks` carry the ": <product|offer>" suffix.
            let id = tstr(&l, "id");
            l["note"] = json!(tstr(&templates()["locks"][id.as_str()], "note"));
            l
        })
        .collect();
    Kit {
        brand: brand.to_string(),
        locks: json!({ "product_ref": product_ref, "logo": logo, "character": character, "voice": voice }),
        missing,
    }
}

fn kit_json(kit: &Kit) -> Value {
    json!({
        "brand": kit.brand,
        "readiness": if kit.missing.is_empty() { "ready" } else { "needs_refs" },
        "locks": kit.locks,
        "missing": kit.missing,
    })
}

fn path_exists(value: &str) -> Option<bool> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return None;
    }
    let path = value.strip_prefix("file://").unwrap_or(value);
    Some(Path::new(path).is_file())
}

/// `brand_check`: the lock ledger, plus whether each reference file exists.
/// Readiness follows the baseline rule (a lock is ready once a value is set);
/// a file that cannot be found is a warning, since the file may live on
/// another machine or behind a URL.
fn brand_check(input: &Value) -> GenResult<Value> {
    let brand = resolve_brand(input)?;
    let product = Some(field(input, "product_name"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Product".into());
    let offer = Some(field(input, "offer"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Offer".into());
    let kit = summarize_kit(input, &brand, &product, &offer);
    let mut out = kit_json(&kit);
    let mut warnings = Vec::new();
    for id in ["product_ref", "logo"] {
        if let Some(value) = kit.locks[id]["value"].as_str() {
            let exists = path_exists(value);
            out["locks"][id]["exists"] = exists.map_or(Value::Null, Value::Bool);
            if exists == Some(false) {
                warnings.push(format!(
                    "{} not found on this machine: {value}",
                    tstr(&kit.locks[id], "label")
                ));
            }
        }
    }
    out["warnings"] = json!(warnings);
    Ok(out)
}

// ---------------------------------------------------------------------------
// campaign factory

fn campaign_plan(input: &Value) -> GenResult<(Value, Kit)> {
    let t = templates();
    let fb = &t["fallbacks"];
    let brand = resolve_brand(input)?;
    let or = |key: &str, fallback_key: &str| {
        Some(field(input, key))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| tstr(fb, fallback_key))
    };
    let product = or("product_name", "product_name");
    let offer = or("offer", "offer");
    let audience = or("audience", "audience");
    let promise = Some(field(input, "promise"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("make {audience} care quickly"));
    let slug_source = Some(field(input, "project_slug"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{product}-{offer}"));
    let project_slug = slugify(&slug_source, &tstr(fb, "project_slug"));
    let kit = summarize_kit(input, &brand, &product, &offer);
    let channels = normalize_channels(input);
    let vars: Vec<(&str, &str)> = vec![
        ("product", &product),
        ("offer", &offer),
        ("audience", &audience),
        ("promise", &promise),
    ];

    let hooks: Vec<Value> = t["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| json!({ "id": h["id"], "label": h["label"], "angle": h["angle"], "line": fill(&tstr(h, "line"), &vars) }))
        .collect();
    let hook0 = hooks[0]["line"].as_str().unwrap_or_default().to_string();

    let scenes: Vec<Value> = t["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let required: Vec<Value> = match s["needs_ref"].as_str() {
                Some(id) if kit.locks[id]["status"] == "missing" => vec![json!(id)],
                _ => vec![],
            };
            json!({
                "id": s["id"], "label": s["label"], "duration_s": s["duration_s"], "visual_role": s["visual_role"],
                "prompt": fill(&tstr(s, "prompt"), &vars), "voiceover": fill(&tstr(s, "voiceover"), &vars),
                "required_refs": required,
            })
        })
        .collect();

    let variants: Vec<Value> = channels
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let spec = t["channels"].as_array().unwrap().iter().find(|c| c["id"] == *id).unwrap();
            json!({
                "id": format!("variant-{id}"), "channel": id, "label": spec["label"], "aspect": spec["aspect"],
                "duration_s": spec["duration_s"], "caption_style": spec["caption_style"], "cta": spec["cta"],
                "priority": if i == 0 { "primary" } else { "draft" },
            })
        })
        .collect();

    let h = &t["handoff"];
    let mut hv = vars.clone();
    hv.push(("hook0", &hook0));
    let plan = json!({
        "schema_version": SCHEMA_VERSION,
        "brand": brand,
        "project_slug": project_slug,
        "product_name": product,
        "offer": offer,
        "audience": audience,
        "promise": promise,
        "identity_locks": kit.locks,
        "required_assets": kit.missing,
        "hooks": hooks,
        "scenes": scenes,
        "variants": variants,
        "handoff": {
            "recommended_view": "video",
            "suggested_model": h["video"]["model"],
            "image_prompt": fill(&tstr(h, "image_prompt"), &hv),
            "video_prompt": fill(&tstr(h, "video_prompt"), &hv),
            "tts_script": fill(&tstr(h, "tts_script"), &hv),
        },
    });
    Ok((plan, kit))
}

fn normalize_channels(input: &Value) -> Vec<String> {
    let t = templates();
    let known: HashSet<String> = t["channels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str().map(String::from))
        .collect();
    let mut seen = HashSet::new();
    let picked: Vec<String> = input
        .get("channels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|c| known.contains(c))
        .filter(|c| seen.insert(c.clone()))
        .collect();
    if picked.is_empty() {
        strings(t, "default_channels")
    } else {
        picked
    }
}

// ---------------------------------------------------------------------------
// canvas

fn canvas(campaign: &Value) -> Value {
    fn node(id: &str, ty: &str, label: &str, status: &str, x: i64, y: i64, summary: &str) -> Value {
        json!({ "id": id, "type": ty, "label": label, "status": status, "x": x, "y": y, "summary": summary })
    }
    fn edge(from: &str, to: &str, label: &str) -> Value {
        json!({ "from": from, "to": to, "label": label })
    }
    let lock_status = |id: &str| {
        let s = campaign["identity_locks"][id]["status"]
            .as_str()
            .unwrap_or("missing");
        if s == "ready" || s == "optional" {
            "ready"
        } else {
            "needs_input"
        }
    };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let h = &campaign["handoff"];
    let scenes = campaign["scenes"].as_array().cloned().unwrap_or_default();
    let variants = campaign["variants"].as_array().cloned().unwrap_or_default();
    let missing = !campaign["required_assets"]
        .as_array()
        .is_none_or(Vec::is_empty);

    let mut nodes = vec![
        node(
            "brief",
            "brief",
            "Brief",
            "ready",
            0,
            0,
            &format!(
                "{} for {}",
                s(&campaign["product_name"]),
                s(&campaign["audience"])
            ),
        ),
        node(
            "brand-kit",
            "brand_kit",
            "Brand Kit",
            if missing { "needs_input" } else { "ready" },
            220,
            0,
            &format!("{} locks and missing refs", s(&campaign["brand"])),
        ),
        node(
            "identity",
            "identity",
            "Identity Lock",
            lock_status("product_ref"),
            440,
            0,
            "Product, logo, character, and voice state",
        ),
        node(
            "image-refs",
            "reference",
            "Reference Frames",
            lock_status("product_ref"),
            660,
            0,
            &s(&h["image_prompt"]),
        ),
        node(
            "voice",
            "voice",
            "Voice Script",
            if campaign["identity_locks"]["voice"]["status"] == "missing" {
                "planned"
            } else {
                "ready"
            },
            660,
            170,
            &s(&h["tts_script"]),
        ),
        node(
            "video-draft",
            "video",
            "Video Draft",
            "planned",
            880,
            80,
            &s(&h["video_prompt"]),
        ),
        node(
            "post",
            "post",
            "Post Pass",
            "planned",
            1100,
            80,
            "Captions, crop, music, watermark, export naming",
        ),
    ];
    for (i, sc) in scenes.iter().enumerate() {
        let needs = !sc["required_refs"].as_array().is_none_or(Vec::is_empty);
        nodes.push(node(
            &format!("scene-{}", i + 1),
            "scene",
            &s(&sc["label"]),
            if needs { "needs_input" } else { "planned" },
            660,
            300 + i as i64 * 130,
            &s(&sc["prompt"]),
        ));
    }
    for (i, v) in variants.iter().enumerate() {
        nodes.push(node(
            &format!("export-{}", s(&v["channel"])),
            "export",
            &s(&v["label"]),
            "planned",
            1320,
            i as i64 * 120,
            &format!(
                "{}, {}s, {}",
                s(&v["aspect"]),
                v["duration_s"],
                s(&v["caption_style"])
            ),
        ));
    }

    let mut edges = vec![
        edge("brief", "brand-kit", "locks"),
        edge("brand-kit", "identity", "identity"),
        edge("identity", "image-refs", "refs"),
        edge("image-refs", "video-draft", "frames"),
        edge("voice", "video-draft", "audio/script"),
        edge("video-draft", "post", "render"),
    ];
    for (i, sc) in scenes.iter().enumerate() {
        edges.push(edge(
            "image-refs",
            &format!("scene-{}", i + 1),
            &s(&sc["visual_role"]),
        ));
    }
    for i in 0..scenes.len() {
        edges.push(edge(&format!("scene-{}", i + 1), "video-draft", "scene"));
    }
    for v in &variants {
        edges.push(edge(
            "post",
            &format!("export-{}", s(&v["channel"])),
            &s(&v["channel"]),
        ));
    }
    json!({ "schema_version": SCHEMA_VERSION, "project_slug": campaign["project_slug"], "nodes": nodes, "edges": edges })
}

// ---------------------------------------------------------------------------
// director shot plan

fn director_plan(subject_fallback: &str, input: &Value, style_notes: &str) -> Value {
    let t = templates();
    let fb = &t["fallbacks"];
    let d = input.get("director").cloned().unwrap_or_else(|| json!({}));
    let or = |key: &str, fallback_key: &str| {
        Some(field(&d, key))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| tstr(fb, fallback_key))
    };
    let subject = Some(subject_fallback.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| tstr(fb, "director_subject"));
    let duration = clamp_number(
        d.get("duration"),
        fb["duration"].as_f64().unwrap_or(6.0),
        fb["duration_min"].as_f64().unwrap_or(1.0),
        fb["duration_max"].as_f64().unwrap_or(12.0),
    );
    let aspect = or("aspect", "aspect");
    let profile = or("camera_profile", "camera_profile");
    let lens = or("lens", "lens");
    let movement = or("movement", "movement");
    let mut locks: Vec<String> = style_notes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(6)
        .collect();
    if locks.is_empty() {
        locks = strings(fb, "continuity_locks");
    }
    let audio_mode = Some(field(&d, "audio_mode"))
        .filter(|m| strings(t, "audio_modes").contains(m))
        .unwrap_or_else(|| "silent".into());

    let td = &t["director"];
    let vars: Vec<(&str, &str)> = vec![
        ("subject", &subject),
        ("movement", &movement),
        ("profile", &profile),
        ("lens", &lens),
    ];
    let segments: Vec<Value> = td["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let from = if s["from"].as_f64() == Some(0.0) { 0.0 } else { round1(duration * s["from"].as_f64().unwrap_or(0.0)) };
            let to = if s["to"].as_f64() == Some(1.0) { duration } else { round1(duration * s["to"].as_f64().unwrap_or(1.0)) };
            json!({
                "start_s": num(from), "end_s": num(to),
                "camera": fill(&tstr(s, "camera"), &vars), "action": fill(&tstr(s, "action"), &vars),
                "forbidden_zone": s["forbidden_zone"],
            })
        })
        .collect();
    let joined = locks.join(", ");
    let addendum_vars: Vec<(&str, &str)> = vec![
        ("profile", &profile),
        ("lens", &lens),
        ("movement", &movement),
        ("locks", &joined),
    ];
    json!({
        "schema_version": SCHEMA_VERSION,
        "subject": subject,
        "duration": num(duration),
        "aspect": aspect,
        "prompt_addendum": fill(&tstr(td, "prompt_addendum"), &addendum_vars),
        "control_stack": [
            { "label": "Profile", "value": profile },
            { "label": "Lens", "value": lens },
            { "label": "Movement", "value": movement },
            { "label": "Aspect", "value": aspect },
            { "label": "Audio", "value": audio_mode },
        ],
        "checklist": td["checklist"],
        "shot_contract_inline": {
            "primary_action": fill(&tstr(td, "primary_action"), &vars),
            "subject_focus": subject,
            "camera_profile": profile,
            "lens_lock": lens,
            "continuity_locks": locks,
            "motion_budget": td["motion_budget"],
            "lighting_lock": td["lighting_lock"],
            "audio_lock": { "mode": audio_mode },
            "time_segments": segments,
            "negative_prompt": td["negative_prompt"],
        },
    })
}

// ---------------------------------------------------------------------------
// post plan

fn post_plan(campaign: &Value, input: &Value) -> Value {
    let p = input.get("post").cloned().unwrap_or_else(|| json!({}));
    let flag = |key: &str, default: bool| p.get(key).and_then(Value::as_bool).unwrap_or(default);
    let captions = flag("captions", true);
    let music = flag("music", false);
    let watermark = flag("watermark", true);
    let slug = tstr(campaign, "project_slug");
    let steps_tpl = templates()["post"]["steps"].as_array().unwrap();
    let tasks: Vec<Value> = campaign["variants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let aspect = tstr(v, "aspect");
            let duration = v["duration_s"].to_string();
            let steps: Vec<String> = steps_tpl
                .iter()
                .filter(|s| match s["when"].as_str() {
                    Some("captions") => captions,
                    Some("music") => music,
                    Some("watermark") => watermark,
                    _ => true,
                })
                .map(|s| fill(&tstr(s, "text"), &[("aspect", &aspect), ("duration_s", &duration)]))
                .collect();
            let channel = tstr(v, "channel");
            json!({
                "id": format!("post-{channel}"), "channel": channel, "label": v["label"], "aspect": aspect,
                "duration_s": v["duration_s"],
                "output_name": format!("{slug}-{channel}-{}.mp4", aspect.replacen(':', "x", 1)),
                "steps": steps,
            })
        })
        .collect();
    json!({ "schema_version": SCHEMA_VERSION, "project_slug": slug, "tasks": tasks })
}

// ---------------------------------------------------------------------------
// hand-offs

/// Requests for the Image, Video and Voice views. Each carries `view`, `kind`,
/// a prompt and engine `params` the view can pass to `generate` after the
/// operator confirms; nothing here is submitted.
fn handoff(campaign: &Value, director: &Value) -> Value {
    let t = templates();
    let h = &t["handoff"];
    let reasons = &h["reasons"];
    let product_ref = campaign["identity_locks"]["product_ref"]["value"]
        .as_str()
        .map(String::from);
    let primary = campaign["variants"]
        .as_array()
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or(Value::Null);
    let primary_aspect = Some(tstr(&primary, "aspect"))
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| tstr(director, "aspect"));
    let primary_duration = primary["duration_s"]
        .as_f64()
        .unwrap_or_else(|| director["duration"].as_f64().unwrap_or(5.0));
    let slug = tstr(campaign, "project_slug");
    let brand = tstr(campaign, "brand");

    let mut shot_contract = director["shot_contract_inline"].clone();
    shot_contract["source_frame_read"] = json!(if product_ref.is_some() {
        &h["source_frame_read_ok"]
    } else {
        &h["source_frame_read_missing"]
    });
    shot_contract["campaign_scenes"] = json!(campaign["scenes"].as_array().unwrap().iter().map(|s| json!({
        "id": s["id"], "label": s["label"], "visual_role": s["visual_role"], "duration_s": s["duration_s"], "prompt": s["prompt"]
    })).collect::<Vec<_>>());
    shot_contract["campaign_variants"] = json!(campaign["variants"].as_array().unwrap().iter().map(|v| json!({
        "channel": v["channel"], "aspect": v["aspect"], "duration_s": v["duration_s"], "caption_style": v["caption_style"]
    })).collect::<Vec<_>>());

    // Image: a hero/start frame.
    let ih = &h["image"];
    let image_aspect = nearest_aspect(&primary_aspect, &strings(ih, "aspects"));
    let mut image_params = json!({ "aspect": image_aspect, "resolution": ih["resolution"] });
    if let Some(r) = &product_ref {
        image_params["image_urls"] = json!([r]);
    }
    let image_reason = match (&product_ref, image_aspect != primary_aspect) {
        (None, _) => Some(tstr(reasons, "image_no_ref")),
        (_, true) => Some(fill(
            &tstr(reasons, "image_aspect"),
            &[("aspect", &primary_aspect), ("sent", &image_aspect)],
        )),
        _ => None,
    };
    // A local product reference must be uploaded to the user's own fal storage before NanoBanana can
    // read it; `generate` does that only when the run carries `confirm_fal_upload: true`. The hand-off
    // never sets that flag itself: the person confirms each run.
    let ref_is_local = product_ref
        .as_deref()
        .is_some_and(|r| rightkit_media_gen::fal_storage::local_path(r).is_some());
    let image = json!({
        "status": if image_reason.is_some() { "ready_with_warnings" } else { "ready" },
        "reason": image_reason,
        "requires_fal_upload_confirmation": ref_is_local,
        "request": {
            "view": "image", "kind": "image", "provider": ih["provider"], "model": ih["model"],
            "prompt": campaign["handoff"]["image_prompt"], "params": image_params,
            "label": format!("{slug}-hero-frame"), "notes": ih["notes"],
            "workflow": { "kind": "campaign_hero_frame", "brand": brand, "project_slug": format!("{slug}-hero-frame"),
                "product_name": campaign["product_name"], "offer": campaign["offer"], "audience": campaign["audience"], "required_for_video": true },
        },
    });

    // Video: needs a start frame.
    let vh = &h["video"];
    let video_aspect = nearest_aspect(&primary_aspect, &strings(vh, "aspects"));
    let lo = vh["min_duration_s"].as_f64().unwrap_or(4.0);
    let hi = vh["max_duration_s"].as_f64().unwrap_or(5.0);
    let video_duration = primary_duration.min(hi).max(lo).round();
    let mut video_params = json!({ "aspect": video_aspect, "resolution": vh["resolution"], "duration": num(video_duration) });
    if let Some(r) = &product_ref {
        video_params["image_url"] = json!(r);
    }
    let video_prompt = format!(
        "{} {}",
        tstr(&campaign["handoff"], "video_prompt"),
        tstr(director, "prompt_addendum")
    );
    let (video_status, video_reason) = if product_ref.is_none() {
        ("blocked", Some(tstr(reasons, "video_no_ref")))
    } else if video_aspect != primary_aspect {
        (
            "ready_with_warnings",
            Some(fill(
                &tstr(reasons, "video_aspect"),
                &[("aspect", &primary_aspect), ("sent", &video_aspect)],
            )),
        )
    } else {
        ("ready", None)
    };
    let video = json!({
        "status": video_status,
        "reason": video_reason,
        "request": {
            "view": "video", "kind": "video", "provider": vh["provider"], "model": vh["model"],
            "prompt": video_prompt.trim(), "params": video_params,
            "label": format!("{slug}-video"), "shot_contract": shot_contract,
            "workflow": { "kind": "campaign_video", "source": "genright_campaign_stack", "brand": brand, "campaign_project_slug": slug,
                "subject": campaign["product_name"], "action": campaign["promise"], "mood": vh["mood"],
                "text_overlay": primary["caption_style"], "audio_recipe": director["shot_contract_inline"]["audio_lock"]["mode"],
                "continuity_locks": director["shot_contract_inline"]["continuity_locks"],
                "scenes": campaign["scenes"], "variants": campaign["variants"] },
        },
    });

    // Voice: the campaign script.
    let voh = &h["voice"];
    let script = tstr(&campaign["handoff"], "tts_script");
    let mut voice_params = json!({});
    if let Some(v) = campaign["identity_locks"]["voice"]["value"].as_str() {
        voice_params["voice_id"] = json!(v);
    }
    let mut tags = strings(voh, "emotion_tags");
    tags.push(brand.to_lowercase());
    let voice = json!({
        "status": if script.is_empty() { "blocked" } else { "ready" },
        "reason": if script.is_empty() { json!(tstr(reasons, "voice_no_script")) } else { Value::Null },
        "request": {
            "view": "voice", "kind": "voice", "provider": voh["provider"], "model": voh["model"],
            "prompt": script, "params": voice_params, "label": format!("{slug}-voice"),
            "workflow": { "kind": "campaign_voice", "brand": brand, "emotion_tags": tags, "campaign_project_slug": slug },
        },
    });
    json!({ "image": image, "video": video, "voice": voice })
}

// ---------------------------------------------------------------------------
// project document

const PROJECT_KIND: &str = "production_project";
const PROJECT_SCHEMA: &str = "1.0.0";

/// The canonical, versioned project document (`project.json`). Campaign,
/// canvas, shot plan and post plan are views derived from it. `beats` is the
/// ordered shot plan; `gates` holds the human approvals (both optional until a
/// later revision drives them: beat map approval, then the chosen look).
fn project_doc(campaign: &Value, director: &Value) -> Value {
    let beats: Vec<Value> = campaign["scenes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| {
            json!({
                "id": s["id"],
                "purpose": s["visual_role"],
                "label": s["label"],
                "script_text": s["voiceover"],
                "shot_contract": { "prompt": s["prompt"], "duration_s": s["duration_s"], "required_refs": s["required_refs"] },
                "voice": Value::Null,
                "music_cue": Value::Null,
                "caption": Value::Null,
                "status": "planned",
            })
        })
        .collect();
    json!({
        "schema_version": PROJECT_SCHEMA,
        "kind": PROJECT_KIND,
        "project_slug": campaign["project_slug"],
        "brand": campaign["brand"],
        "brief": { "product_name": campaign["product_name"], "offer": campaign["offer"], "audience": campaign["audience"], "promise": campaign["promise"] },
        "beats": beats,
        "gates": { "beat_map": { "approved_at": Value::Null }, "look": { "chosen": Value::Null } },
        "director": director,
    })
}

// ---------------------------------------------------------------------------
// build / pack

fn build(input: &Value) -> GenResult<Value> {
    if !input.is_object() {
        return Err(invalid("production values must be an object"));
    }
    let (campaign, kit) = campaign_plan(input)?;
    let canvas = canvas(&campaign);
    let director = director_plan(
        &tstr(&campaign, "product_name"),
        input,
        &field(input, "style_notes"),
    );
    let post = post_plan(&campaign, input);
    let handoff = handoff(&campaign, &director);
    let project = project_doc(&campaign, &director);
    let pack = json!({
        "schema_version": SCHEMA_VERSION,
        "kind": PACK_KIND,
        "project_slug": campaign["project_slug"],
        "created_source": "genright_campaign_stack",
        "campaign": campaign,
        "canvas": canvas,
        "director": director,
        "post": post,
        "handoff": handoff,
        "project": project,
    });
    Ok(json!({
        "project": pack["project"], "campaign": pack["campaign"], "brand_kit": kit_json(&kit), "canvas": pack["canvas"],
        "director": pack["director"], "post": pack["post"], "handoff": pack["handoff"], "pack": pack,
    }))
}

fn require_object<'a>(value: Option<&'a Value>, name: &str) -> GenResult<&'a Map<String, Value>> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(format!("{name} must be an object")))
}

/// Port of `normalizeProductionPack`: reject anything that is not a complete
/// campaign production pack and return the pack with a clean slug.
pub fn normalize_pack(input: &Value) -> GenResult<Value> {
    let pack = input
        .as_object()
        .ok_or_else(|| invalid("Campaign production pack payload is required"))?;
    if pack.get("kind").and_then(Value::as_str) != Some(PACK_KIND) {
        return Err(invalid("Payload must be a campaign production pack"));
    }
    if pack.get("schema_version").and_then(Value::as_str) != Some(SCHEMA_VERSION) {
        return Err(invalid(
            "Campaign production pack schema_version must be 1.0.0",
        ));
    }
    let campaign = require_object(pack.get("campaign"), "campaign")?;
    let canvas = require_object(pack.get("canvas"), "canvas")?;
    require_object(pack.get("director"), "director")?;
    let post = require_object(pack.get("post"), "post")?;
    let handoff = require_object(pack.get("handoff"), "handoff")?;
    if !canvas.get("nodes").is_some_and(Value::is_array)
        || !canvas.get("edges").is_some_and(Value::is_array)
    {
        return Err(invalid(
            "Campaign production pack canvas must include nodes and edges arrays",
        ));
    }
    if !post.get("tasks").is_some_and(Value::is_array) {
        return Err(invalid(
            "Campaign production pack post plan must include tasks",
        ));
    }
    for key in ["image", "video", "voice"] {
        let entry = require_object(handoff.get(key), &format!("handoff.{key}"))?;
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !["ready", "ready_with_warnings", "blocked"].contains(&status) {
            return Err(invalid(format!("handoff.{key}.status is invalid")));
        }
        require_object(entry.get("request"), &format!("handoff.{key}.request"))?;
    }
    let slug_source = pack
        .get("project_slug")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| campaign.get("project_slug").and_then(Value::as_str))
        .unwrap_or("campaign-production-pack");
    let slug = slugify(slug_source, "");
    if slug.is_empty() {
        return Err(invalid("project_slug is required"));
    }
    let mut normalized = input.clone();
    normalized["project_slug"] = json!(slug);
    Ok(normalized)
}

fn pack_arg(input: &Value) -> GenResult<&Value> {
    // Accept `{pack}` or the pack itself.
    Ok(input.get("pack").unwrap_or(input))
}

fn preflight(input: &Value) -> GenResult<Value> {
    let pack = normalize_pack(pack_arg(input)?)?;
    let mut warnings: Vec<String> = Vec::new();
    let mut statuses = Map::new();
    for key in ["image", "video", "voice"] {
        let entry = &pack["handoff"][key];
        statuses.insert(key.to_string(), entry["status"].clone());
        if let Some(reason) = entry["reason"].as_str() {
            warnings.push(format!("{key}: {reason}"));
        }
    }
    for id in ["product_ref", "logo"] {
        if let Some(value) = pack["campaign"]["identity_locks"][id]["value"].as_str() {
            if path_exists(value) == Some(false) {
                warnings.push(format!("{id}: file not found on this machine: {value}"));
            }
        }
    }
    let blocked: Vec<&str> = statuses
        .iter()
        .filter(|(_, v)| *v == "blocked")
        .map(|(k, _)| k.as_str())
        .collect();
    Ok(json!({
        "ok": true,
        "project_slug": pack["project_slug"],
        "handoff": statuses,
        "blocked": blocked,
        "exports": pack["post"]["tasks"].as_array().map_or(0, Vec::len),
        "warnings": warnings,
    }))
}

fn packs_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("packs")
}

fn write_json(path: &Path, value: &Value) -> GenResult<()> {
    let text =
        serde_json::to_string_pretty(value).map_err(|e| GenError::new("io", e.to_string()))?;
    rightkit_fs::atomic_write(path, (text + "\n").as_bytes())
        .map_err(|e| GenError::new("io", format!("cannot write {}: {e}", path.display())))
}

/// Write the pack folder: `pack.json` (the whole pack), `manifest.json` (the
/// baseline draft record) and one file per part for downstream tools.
fn save_pack(ctx: &Context, input: &Value) -> GenResult<Value> {
    let pack = normalize_pack(pack_arg(input)?)?;
    let created_at = rightkit_media_gen::store::now();
    let stamp: String = created_at.chars().filter(char::is_ascii_digit).collect();
    let slug = tstr(&pack, "project_slug");
    let draft_id = format!("production-{stamp}-{slug}-{}", short_id());
    let dir = packs_dir(ctx).join(&draft_id);
    rightkit_fs::ensure_private_dir(&dir)
        .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", dir.display())))?;

    let project = if pack["project"].is_object() {
        pack["project"].clone()
    } else {
        project_doc(&pack["campaign"], &pack["director"])
    };
    let files: Vec<(&str, Value)> = vec![
        ("project.json", project),
        ("pack.json", pack.clone()),
        ("campaign.json", pack["campaign"].clone()),
        ("canvas.json", pack["canvas"].clone()),
        ("shot_contract.json", pack["director"].clone()),
        ("post_plan.json", pack["post"].clone()),
        ("handoff_image.json", pack["handoff"]["image"].clone()),
        ("handoff_video.json", pack["handoff"]["video"].clone()),
        ("handoff_voice.json", pack["handoff"]["voice"].clone()),
    ];
    for (name, value) in &files {
        write_json(&dir.join(name), value)?;
    }
    let manifest_path = dir.join("manifest.json");
    let draft = json!({
        "schema_version": SCHEMA_VERSION,
        "draft_id": draft_id,
        "status": "draft",
        "execution_mode": "production_pack_preflight",
        "created_at": created_at,
        "production_pack": pack,
        "pack_dir": dir.to_string_lossy(),
        "manifest_path": manifest_path.to_string_lossy(),
        "files": files.iter().map(|(n, _)| *n).chain(["manifest.json"]).collect::<Vec<_>>(),
    });
    write_json(&manifest_path, &draft)?;
    Ok(draft)
}

fn list_packs(ctx: &Context) -> GenResult<Value> {
    let mut packs = Vec::new();
    if let Ok(read) = std::fs::read_dir(packs_dir(ctx)) {
        for entry in read.flatten() {
            let manifest = entry.path().join("manifest.json");
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            packs.push(json!({
                "draft_id": v["draft_id"], "project_slug": v["production_pack"]["project_slug"], "created_at": v["created_at"],
                "pack_dir": entry.path().to_string_lossy(), "manifest_path": manifest.to_string_lossy(),
            }));
        }
    }
    packs.sort_by(|a, b| {
        b["created_at"]
            .as_str()
            .cmp(&a["created_at"].as_str())
            .then(b["draft_id"].as_str().cmp(&a["draft_id"].as_str()))
    });
    Ok(json!({ "packs": packs }))
}
