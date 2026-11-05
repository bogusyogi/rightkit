//! Template swap (#269): saved base media + fixed prompt + default model + named input slots,
//! stored under `<data_dir>/templates/<id>/`. `templates.swap_preflight` turns a template plus slot
//! media into an ordinary template draft (`recipes::single_stage_draft`), so the estimate,
//! `studio.recipes.template_approve` and the spend gates apply unchanged. Nothing here calls a
//! provider; every estimate is the engine's own dry run.
//!
//! Routes (a candidate that fails its dry run is listed under `considered`, never guessed):
//! - image: `nanobanana/nano-banana-2`, `evolink/gpt-image-2`, `fal/bytedance/seedream/v5/lite/edit`
//!   (base image first, then the slot images, as `image_urls`).
//! - video: the catalogue's `motion_control` rows (Kling 3.0 Motion Control, Wan Animate): base
//!   video in the video input, first slot image in the reference input.
//! - TODO(#269): `pixverse/swap` and a Seedream 4 edit row are not in the catalogue and their
//!   provider schemas are not documented in-repo; they are reported under `unserved_routes`.
//!   Catalogue/provider rows belong to the catalogue lane.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use rightkit_media_gen::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "templates.list",
    "templates.save",
    "templates.delete",
    "templates.import",
    "templates.swap_preflight",
];

const IMAGE_ROUTES: &[(&str, &str)] = &[
    ("nanobanana", "nano-banana-2"),
    ("evolink", "gpt-image-2"),
    ("fal", "bytedance/seedream/v5/lite/edit"),
];
const UNSERVED: &[(&str, &str, &str)] = &[
    (
        "fal",
        "pixverse/swap",
        "video: no catalogue row and no provider schema documented in the repository",
    ),
    (
        "fal",
        "bytedance/seedream/v4/edit",
        "image: not in the catalogue; seedream/v5/lite/edit is served instead",
    ),
];

fn bad(m: impl Into<String>) -> GenError {
    GenError::new("invalid_input", m)
}

fn root(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("templates")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(48).collect()
}

fn media_type(value: &str) -> Option<&'static str> {
    let path = value.split(['?', '#']).next().unwrap_or("");
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "avif" | "bmp" => Some("image"),
        "mp4" | "mov" | "webm" | "m4v" => Some("video"),
        _ => None,
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn write(path: &Path, v: &Value) -> GenResult<()> {
    let body = serde_json::to_string_pretty(v)
        .map_err(|e| GenError::new("template_write", e.to_string()))?;
    rightkit_fs::atomic_write(path, body.as_bytes())
        .map_err(|e| GenError::new("template_write", format!("{}: {e}", path.display())))
}

fn now() -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    time::OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_default()
}

fn load(ctx: &Context, id: &str) -> GenResult<Value> {
    if !valid_id(id) {
        return Err(bad("template id is not valid"));
    }
    let body = fs::read_to_string(root(ctx).join(id).join("template.json"))
        .map_err(|_| GenError::new("template_not_found", format!("No template \"{id}\"")))?;
    serde_json::from_str(&body).map_err(|e| GenError::new("template_invalid", format!("{id}: {e}")))
}

fn list(ctx: &Context) -> Value {
    let mut out: Vec<Value> = fs::read_dir(root(ctx))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| fs::read_to_string(e.path().join("template.json")).ok())
                .filter_map(|b| serde_json::from_str(&b).ok())
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| {
        a["created_at"]
            .as_str()
            .cmp(&b["created_at"].as_str())
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    json!({"ok": true, "templates": out, "unserved_routes": unserved()})
}

fn unserved() -> Value {
    Value::Array(
        UNSERVED
            .iter()
            .map(|(p, m, why)| json!({"provider": p, "model": m, "reason": why}))
            .collect(),
    )
}

fn parse_slots(kind: &str, input: &Value) -> GenResult<Vec<Value>> {
    let raw: Vec<Value> = match input.get("slots") {
        Some(Value::Array(a)) if !a.is_empty() => a.clone(),
        None | Some(Value::Null) => {
            vec![json!({"name": "subject", "label": "Your image", "type": "image"})]
        }
        _ => return Err(bad("slots must be a non-empty array")),
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for s in raw {
        let name = text(&s, "name")
            .map(|n| slug(&n))
            .filter(|n| !n.is_empty())
            .ok_or_else(|| bad("each slot needs a name"))?;
        if seen.contains(&name) {
            return Err(bad(format!("duplicate slot name \"{name}\"")));
        }
        let ty = text(&s, "type").unwrap_or_else(|| "image".into());
        if ty != "image" && ty != "video" {
            return Err(bad(format!("slot {name}: type must be image or video")));
        }
        if kind == "image" && ty != "image" {
            return Err(bad(format!(
                "slot {name}: an image template takes image slots only"
            )));
        }
        seen.push(name.clone());
        let label = text(&s, "label").unwrap_or_else(|| name.clone());
        out.push(json!({"name": name, "label": label, "type": ty, "required": s.get("required").and_then(Value::as_bool).unwrap_or(true)}));
    }
    Ok(out)
}

/// `templates.save`: create or replace a template. A local base file is copied into the store.
fn save(ctx: &Context, input: &Value) -> GenResult<Value> {
    let name = text(input, "name").ok_or_else(|| bad("name is required"))?;
    let id = text(input, "id").unwrap_or_else(|| slug(&name));
    if !valid_id(&id) {
        return Err(bad("template id is not valid"));
    }
    let base = input
        .get("base")
        .ok_or_else(|| bad("base ({path} or {url}) is required"))?;
    let prompt = text(input, "prompt").ok_or_else(|| bad("prompt is required"))?;
    let (base_ref, base_type) = if let Some(url) = text(base, "url") {
        if !is_url(&url) {
            return Err(bad("base.url must be an http(s) URL"));
        }
        let t = media_type(&url)
            .ok_or_else(|| bad("base.url must end in an image or video extension"))?;
        (json!({"url": url}), t)
    } else if let Some(path) = text(base, "path") {
        let src = PathBuf::from(&path);
        if !src.is_file() {
            return Err(GenError::new(
                "file_not_found",
                format!("base file not found: {path}"),
            ));
        }
        let t = media_type(&path).ok_or_else(|| {
            bad("base file must be a png/jpg/webp/gif/avif/bmp image or mp4/mov/webm/m4v video")
        })?;
        let ext = path
            .rsplit_once('.')
            .map(|x| x.1.to_ascii_lowercase())
            .unwrap_or_default();
        let dir = root(ctx).join(&id);
        rightkit_fs::ensure_private_dir(&dir)
            .map_err(|e| GenError::new("template_write", format!("{}: {e}", dir.display())))?;
        let dest = dir.join(format!("base.{ext}"));
        // The base file is user-chosen and may be a symlink: resolve it with the owner's `canonicalize_plain`, then copy
        // it verified and atomically (rightkit-fs `copy_verified`) so a half-copied base never sits in the template.
        let canonical = rightkit_fs::canonicalize_plain(&src)
            .map_err(|e| GenError::new("template_write", format!("{}: {e}", src.display())))?;
        rightkit_fs::copy_verified(
            &canonical,
            &dest,
            rightkit_fs::AtomicWriteOptions::default(),
            None,
        )
        .map_err(|e| GenError::new("template_write", format!("{}: {e}", dest.display())))?;
        (json!({"path": dest.to_string_lossy()}), t)
    } else {
        return Err(bad("base needs a path or a url"));
    };
    let kind = base_type;
    if let Some(k) = text(input, "kind") {
        if k != kind {
            return Err(bad(format!(
                "kind {k} does not match the base media ({kind})"
            )));
        }
    }
    let slots = parse_slots(kind, input)?;
    let default_model = match input.get("default_model") {
        None | Some(Value::Null) => Value::Null,
        Some(m) => match (text(m, "provider"), text(m, "model")) {
            (Some(p), Some(mo)) => json!({"provider": p, "model": mo}),
            _ => return Err(bad("default_model needs provider and model")),
        },
    };
    let created = load(ctx, &id)
        .ok()
        .and_then(|t| t["created_at"].as_str().map(String::from))
        .unwrap_or_else(now);
    let doc = json!({
        "schema_version": "1.0.0", "id": id, "name": name, "kind": kind, "base": base_ref, "prompt": prompt,
        "default_model": default_model, "slots": slots, "source": input.get("source").cloned().unwrap_or(Value::Null),
        "created_at": created, "updated_at": now(),
    });
    rightkit_fs::ensure_private_dir(&root(ctx).join(&id))
        .map_err(|e| GenError::new("template_write", e.to_string()))?;
    write(&root(ctx).join(&id).join("template.json"), &doc)?;
    Ok(json!({"ok": true, "template": doc}))
}

fn delete(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = text(input, "id").ok_or_else(|| bad("id is required"))?;
    if !valid_id(&id) {
        return Err(bad("template id is not valid"));
    }
    let dir = root(ctx).join(&id);
    let existed = dir.join("template.json").is_file();
    if existed {
        fs::remove_dir_all(&dir)
            .map_err(|e| GenError::new("template_delete", format!("{}: {e}", dir.display())))?;
    }
    Ok(json!({"ok": true, "id": id, "deleted": existed}))
}

/// URL import needs a downloader the host provides; this crate ships none, so a URL import
/// is a typed refusal rather than a guessed invocation. Callers inject their own downloader
/// through `import_with`.
fn url_download(_url: &str) -> Result<PathBuf, GenError> {
    Err(GenError::new(
        "downloader_contract_unavailable",
        "URL import needs a downloader this host does not provide. Import a local file instead.",
    ))
}

/// `templates.import`: from a local file now; from a URL through `download` (injectable for tests).
fn import_with(
    ctx: &Context,
    input: &Value,
    download: &dyn Fn(&str) -> Result<PathBuf, GenError>,
) -> GenResult<Value> {
    let mut input = input.clone();
    if let Some(url) = text(&input, "url") {
        let file = download(&url)?;
        input["base"] = json!({"path": file.to_string_lossy()});
        input["source"] = json!({"url": url});
    } else if let Some(path) = text(&input, "path") {
        input["base"] = json!({"path": path});
        input["source"] = json!({"path": path});
    } else {
        return Err(bad("import needs a path or a url"));
    }
    if text(&input, "name").is_none() {
        let from = text(&input, "path")
            .or_else(|| text(&input, "url"))
            .unwrap_or_default();
        let stem = from
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .rsplit_once('.')
            .map(|x| x.0.to_string())
            .unwrap_or_default();
        input["name"] = json!(if stem.is_empty() {
            "Imported template".to_string()
        } else {
            stem
        });
    }
    if text(&input, "prompt").is_none() {
        return Err(bad("prompt is required"));
    }
    save(ctx, &input)
}

fn kind_of(e: &rightkit_media_gen::catalogue::Entry) -> String {
    serde_json::to_value(e.kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// One candidate: its request, or the reason it cannot be built.
struct Candidate {
    provider: String,
    model: String,
    request: Result<Value, String>,
}

fn base_value(t: &Value) -> String {
    t["base"]["url"]
        .as_str()
        .or_else(|| t["base"]["path"].as_str())
        .unwrap_or("")
        .to_string()
}

fn swap_prompt(t: &Value, slots: &[(Value, String)], image: bool) -> String {
    let mut p = t["prompt"].as_str().unwrap_or("").trim().to_string();
    if image {
        p.push_str(" Image 1 is the template scene to keep.");
        for (i, (d, _)) in slots.iter().enumerate() {
            p.push_str(&format!(
                " Image {} is {}.",
                i + 2,
                d["label"].as_str().unwrap_or("the subject")
            ));
        }
    }
    p
}

fn candidates(
    t: &Value,
    slots: &[(Value, String)],
    params: &Map<String, Value>,
    prompt: &str,
    wanted: Option<(String, String)>,
) -> Vec<Candidate> {
    let kind = t["kind"].as_str().unwrap_or("image");
    let base = base_value(t);
    let mut routes: Vec<(String, String)> = Vec::new();
    if kind == "image" {
        routes.extend(
            IMAGE_ROUTES
                .iter()
                .map(|(p, m)| (p.to_string(), m.to_string())),
        );
    } else {
        routes.extend(
            rightkit_media_gen::catalogue::Catalogue::get()
                .entries()
                .iter()
                .filter(|e| kind_of(e) == "motion_control")
                .map(|e| (e.provider.clone(), e.model.clone())),
        );
    }
    if let Some(w) = &wanted {
        routes.retain(|r| r == w);
        if routes.is_empty() {
            routes.push(w.clone());
        }
    }
    routes
        .into_iter()
        .map(|(provider, model)| {
            let request = if kind == "image" {
                let mut urls = vec![json!(base)];
                urls.extend(slots.iter().map(|(_, v)| json!(v)));
                let mut p = params.clone();
                p.insert("image_urls".into(), Value::Array(urls));
                Ok(json!({"kind": "image", "provider": provider, "model": model, "prompt": prompt, "params": Value::Object(p)}))
            } else {
                video_request(&provider, &model, &base, slots, params, prompt)
            };
            Candidate { provider, model, request }
        })
        .collect()
}

fn video_request(
    provider: &str,
    model: &str,
    base: &str,
    slots: &[(Value, String)],
    params: &Map<String, Value>,
    prompt: &str,
) -> Result<Value, String> {
    use rightkit_media_gen::catalogue::{Catalogue, InputType, Role};
    let entry = Catalogue::get()
        .find(provider, model)
        .ok_or_else(|| "not in the catalogue".to_string())?;
    let pick = |role: Role| {
        entry
            .inputs
            .iter()
            .find(|i| i.send && i.role == Some(role) && i.ty == InputType::Url)
            .map(|i| i.param.clone())
    };
    let video_param = pick(Role::Video).ok_or("route has no video input")?;
    let ref_param = pick(Role::Reference).ok_or("route has no single reference image input")?;
    let image = slots
        .iter()
        .find(|(d, _)| d["type"] == "image")
        .ok_or("no image slot filled")?;
    let mut p = params.clone();
    p.insert(video_param, json!(base));
    p.insert(ref_param, json!(image.1));
    Ok(
        json!({"kind": "motion_control", "provider": provider, "model": model, "prompt": prompt, "params": Value::Object(p)}),
    )
}

/// `templates.swap_preflight`: template + slot media -> a priced, unapproved template draft.
/// Route: explicit `provider`+`model`, else the template's default model, else the cheapest
/// candidate by the engine's dry-run estimate.
fn swap_preflight(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = text(input, "template_id")
        .or_else(|| text(input, "id"))
        .ok_or_else(|| bad("template_id is required"))?;
    let t = load(ctx, &id)?;
    let filled = input
        .get("slots")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("slots must be an object of slot name -> image/video path or URL"))?;
    let decls = t["slots"].as_array().cloned().unwrap_or_default();
    if let Some(k) = filled
        .keys()
        .find(|k| !decls.iter().any(|d| d["name"] == k.as_str()))
    {
        return Err(GenError::new(
            "unknown_slot",
            format!("template \"{id}\" has no slot \"{k}\""),
        ));
    }
    let mut slots: Vec<(Value, String)> = Vec::new();
    for d in &decls {
        let name = d["name"].as_str().unwrap_or("");
        match filled
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(v) => {
                if media_type(v) != d["type"].as_str() {
                    return Err(GenError::new(
                        "slot_type",
                        format!(
                            "slot \"{name}\" takes {} media; got {v}",
                            d["type"].as_str().unwrap_or("?")
                        ),
                    ));
                }
                if !is_url(v) && !Path::new(v).is_file() {
                    return Err(GenError::new(
                        "file_not_found",
                        format!("slot \"{name}\": file not found: {v}"),
                    ));
                }
                slots.push((d.clone(), v.to_string()));
            }
            None if d["required"] == false => {}
            None => {
                return Err(GenError::new(
                    "slot_missing",
                    format!("slot \"{name}\" is required"),
                ))
            }
        }
    }
    let params = input
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let prompt = swap_prompt(&t, &slots, t["kind"] == "image");
    let wanted = match (text(input, "provider"), text(input, "model")) {
        (Some(p), Some(m)) => Some((p, m)),
        (None, None) => t["default_model"].as_object().and_then(|m| {
            Some((
                m.get("provider")?.as_str()?.to_string(),
                m.get("model")?.as_str()?.to_string(),
            ))
        }),
        _ => return Err(bad("provider and model go together")),
    };
    let mut considered: Vec<Value> = Vec::new();
    let mut priced: Vec<(f64, Value)> = Vec::new();
    for c in candidates(&t, &slots, &params, &prompt, wanted.clone()) {
        let label = format!("{}/{}", c.provider, c.model);
        match c.request.and_then(|r| {
            rightkit_media_gen::api::generate(ctx, &r, true)
                .map(|e| (r, e))
                .map_err(|e| e.message)
        }) {
            Ok((r, e)) => {
                let usd = e["est_usd"].as_f64().unwrap_or(f64::MAX);
                considered.push(json!({"route": label, "est_usd": e["est_usd"], "usable": true}));
                priced.push((usd, r));
            }
            Err(why) => considered.push(json!({"route": label, "usable": false, "reason": why})),
        }
    }
    // Stable: the first of equal estimates wins, so a template's declared order breaks ties.
    let best = priced
        .iter()
        .enumerate()
        .min_by(|a, b| {
            a.1 .0
                .partial_cmp(&b.1 .0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        })
        .map(|x| x.1);
    let Some((est, request)) = best else {
        return Ok(
            json!({"ok": false, "code": "no_route", "message": "No route can serve this template with these slots.", "considered": considered, "unserved_routes": unserved()}),
        );
    };
    let route = json!({"provider": request["provider"], "model": request["model"]});
    let rows: Vec<Value> = slots
        .iter()
        .map(|(d, v)| json!({"name": d["name"], "type": d["type"], "value": v}))
        .collect();
    let mut locals: Vec<String> = slots
        .iter()
        .map(|(_, v)| v.clone())
        .chain(std::iter::once(base_value(&t)))
        .filter(|v| !is_url(v))
        .collect();
    locals.dedup();
    let draft = crate::recipes::single_stage_draft(
        ctx,
        &format!("swap-{id}"),
        request,
        json!(rows),
        route.clone(),
        json!([]),
    )?;
    Ok(json!({
        "ok": true, "template_id": id, "route": route, "est_usd": est, "considered": considered,
        "unserved_routes": unserved(), "draft": draft,
        "upload_confirmation": {"required": !locals.is_empty(), "paths": locals, "confirm_field": "confirm_fal_upload", "note": "Local media is uploaded to fal storage by generate only with this per-run confirmation."},
        "next": "Approve with studio.recipes.template_approve, then run its `generate` request through the engine generate command."
    }))
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "templates.list" => Ok(list(ctx)),
        "templates.save" => save(ctx, input),
        "templates.delete" => delete(ctx, input),
        "templates.import" => import_with(ctx, input, &url_download),
        "templates.swap_preflight" => swap_preflight(ctx, input),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
