//! studio.kdp: the KDP Color Thumbnail workflow, ported from the Media Console
//! baseline (a7271cf `server/kdp-workflows.mjs`, `src/components/KdpStudio.tsx`).
//!
//! One line-art page and one coloring style go to NB2 (provider `nanobanana`,
//! model `nano-banana-2`) at 2K and 1:1 with a locked prompt. Everything but the
//! style is fixed by `data/kdp/workflow.json`.
//!
//! Commands:
//! - `studio.kdp.styles`: styles, locks, prompt label, cost estimate.
//! - `studio.kdp.settings` / `studio.kdp.settings_set`: source roots (`{roots:[{path,kind}]}`; `null` resets).
//! - `studio.kdp.sources`: recent KDP images found under the roots.
//! - `studio.kdp.preflight`: builds the locked request and a dry-run estimate; sends nothing. The page is a local
//!   file and NanoBanana reads only URLs, so the draft carries `requires_fal_upload_confirmation: true`.
//!   (`studio.kdp.run` uploads it to the user's own fal storage, and only with `confirm_fal_upload: true`
//!   on that call; without it the run fails with `upload_confirmation_required` before any request.)
//! - `studio.kdp.run`: submits that request through `rightkit_media_gen::api::generate`, writes the request
//!   manifest (`<data>/studio/requests/image/<draft_id>.json`, the baseline draft/run record) and returns the job.
//!   The engine starts the detached job runner for it (`commands.rs`).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_media_gen::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "studio.kdp.styles",
    "studio.kdp.settings",
    "studio.kdp.settings_set",
    "studio.kdp.sources",
    "studio.kdp.preflight",
    "studio.kdp.run",
];

const STYLES_JSON: &str = include_str!("../data/kdp/styles.json");
const WORKFLOW_JSON: &str = include_str!("../data/kdp/workflow.json");

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Style {
    pub id: String,
    pub label: String,
    pub prompt_fragment: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Root {
    pub path: String,
    /// `kdp` (folder per project) or `codex` (only `book-*` folders count).
    pub kind: String,
}

fn styles() -> &'static [Style] {
    static S: OnceLock<Vec<Style>> = OnceLock::new();
    S.get_or_init(|| serde_json::from_str(STYLES_JSON).expect("kdp styles.json"))
}

fn workflow() -> &'static Value {
    static W: OnceLock<Value> = OnceLock::new();
    W.get_or_init(|| serde_json::from_str(WORKFLOW_JSON).expect("kdp workflow.json"))
}

fn wf_str(pointer: &str) -> &'static str {
    workflow()
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn style_by_id(id: &str) -> GenResult<&'static Style> {
    styles().iter().find(|s| s.id == id).ok_or_else(|| {
        let ids: Vec<&str> = styles().iter().map(|s| s.id.as_str()).collect();
        GenError::new(
            "invalid_input",
            format!(
                "Unsupported KDP thumbnail style \"{id}\". Pick one of: {}",
                ids.join(", ")
            ),
        )
    })
}

/// The locked colorize prompt for a style (baseline `buildKdpColorThumbnailPrompt`).
pub fn build_prompt(style_id: &str) -> GenResult<String> {
    let style = style_by_id(style_id)?;
    let lines = workflow()
        .pointer("/prompt/lines")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(lines
        .into_iter()
        .map(|l| {
            if l == "{style}" {
                style.prompt_fragment.as_str()
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join(" "))
}

fn slugify(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let slug: String = out.trim_matches('-').chars().take(60).collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

// ---- settings --------------------------------------------------------------

fn settings_path(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("studio").join("kdp.json")
}

fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        // The user's home folder as the owner resolves it (`HOME` on macOS and Linux, the Profile known folder on
        // Windows); no environment fallback of our own.
        let home = rightkit_fs::AppDirs::resolve(rightkit_fs::AppId::new("RightKit"))
            .ok()
            .map(|dirs| dirs.home.into_os_string());
        if let Some(home) = home {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

/// Baseline roots: the KDP folder and the codex generated-images folder
/// (`CODEX_GENERATED_IMAGES_ROOT` when set).
fn default_roots() -> Vec<Root> {
    let mut roots: Vec<Root> = workflow()
        .pointer("/sources/default_roots")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if let Ok(codex) = std::env::var(wf_str("/sources/codex_root_env")) {
        if !codex.trim().is_empty() {
            roots.retain(|r| r.kind != "codex");
            roots.push(Root {
                path: codex,
                kind: "codex".into(),
            });
        }
    }
    for r in &mut roots {
        r.path = expand_home(&r.path);
    }
    roots
}

fn saved_roots(ctx: &Context) -> Option<Vec<Root>> {
    let text = std::fs::read_to_string(settings_path(ctx)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(v.get("roots")?.clone()).ok()
}

fn effective_roots(ctx: &Context) -> Vec<Root> {
    saved_roots(ctx).unwrap_or_else(default_roots)
}

fn settings_view(ctx: &Context) -> Value {
    let saved = saved_roots(ctx);
    let roots: Vec<Value> = saved
        .clone()
        .unwrap_or_else(default_roots)
        .iter()
        .map(|r| json!({"path": r.path, "kind": r.kind, "exists": Path::new(&r.path).is_dir()}))
        .collect();
    json!({"roots": roots, "customized": saved.is_some(), "defaults": default_roots()})
}

fn settings_set(ctx: &Context, input: &Value) -> GenResult<Value> {
    let path = settings_path(ctx);
    match input.get("roots") {
        None | Some(Value::Null) => {
            let _ = std::fs::remove_file(&path);
        }
        Some(v) => {
            let roots: Vec<Root> = serde_json::from_value(v.clone()).map_err(|e| {
                GenError::new(
                    "invalid_input",
                    format!("roots must be [{{path, kind}}]: {e}"),
                )
            })?;
            for r in &roots {
                if r.path.trim().is_empty() || !["kdp", "codex"].contains(&r.kind.as_str()) {
                    return Err(GenError::new(
                        "invalid_input",
                        "each root needs a path and kind \"kdp\" or \"codex\"",
                    ));
                }
            }
            if let Some(dir) = path.parent() {
                rightkit_fs::ensure_private_dir(dir)
                    .map_err(|e| GenError::new("io", e.to_string()))?;
            }
            rightkit_fs::atomic_write(
                &path,
                &serde_json::to_vec_pretty(&json!({"roots": roots})).unwrap_or_default(),
            )
            .map_err(|e| GenError::new("io", e.to_string()))?;
        }
    }
    Ok(settings_view(ctx))
}

// ---- sources ---------------------------------------------------------------

fn is_image(p: &Path) -> bool {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    workflow()
        .pointer("/sources/image_extensions")
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|e| e.as_str() == Some(ext.as_str())))
        .unwrap_or(false)
}

/// Walk `dir` in name order, collecting image files until `max` are found.
fn discover_images(dir: &Path, max: usize, out: &mut Vec<PathBuf>) {
    if out.len() >= max {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= max {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            discover_images(&p, max, out);
        } else if ft.is_file() && is_image(&p) {
            out.push(p);
        }
    }
}

/// Project slug for a file under a root, or `None` when it is not KDP material
/// (baseline `inferProject`: the first folder under the KDP root; under the
/// codex root only `book-*` folders are KDP).
fn project_for(root: &Root, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(&root.path).ok()?;
    let mut parts = rel.components();
    let first = parts.next()?.as_os_str().to_string_lossy().into_owned();
    let nested = parts.next().is_some();
    match root.kind.as_str() {
        "codex" => (nested && first.starts_with("book-")).then_some(first),
        _ => Some(if nested { first } else { "kdp".into() }),
    }
}

fn sources(ctx: &Context, input: &Value) -> GenResult<Value> {
    let cap = workflow()
        .pointer("/sources/reference_cap")
        .and_then(Value::as_u64)
        .unwrap_or(200) as usize;
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(cap);
    let scan = limit
        * workflow()
            .pointer("/sources/scan_factor")
            .and_then(Value::as_u64)
            .unwrap_or(3) as usize;
    let mut references: Vec<Value> = Vec::new();
    let mut diagnostics: Vec<Value> = Vec::new();
    let roots = effective_roots(ctx);
    for root in &roots {
        if references.len() >= limit {
            break;
        }
        let dir = Path::new(&root.path);
        if !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        discover_images(dir, scan, &mut files);
        files.sort();
        for file in files {
            if references.len() >= limit {
                break;
            }
            let Some(project) = project_for(root, &file) else {
                continue;
            };
            let meta = std::fs::metadata(&file).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified_ms = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let base = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            references.push(json!({
                "path": file.to_string_lossy(),
                "basename": base,
                "label": base,
                "source": if root.kind == "codex" { "codex_generated" } else { "kdp" },
                "project_slug": project,
                "brand": "KDP",
                "kind": "image",
                "size_bytes": size,
                "modified_ms": modified_ms,
            }));
        }
    }
    // Newest first: the picker doubles as the "recent KDP images" strip.
    references.sort_by(|a, b| b["modified_ms"].as_u64().cmp(&a["modified_ms"].as_u64()));
    if references.len() >= limit {
        diagnostics.push(json!({
            "severity": "warning",
            "message": format!("KDP thumbnail references capped at {limit}; narrow the source set if the image is missing."),
        }));
    }
    Ok(json!({
        "references": references,
        "reference_cap": cap,
        "roots": roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
        "diagnostics": diagnostics,
    }))
}

// ---- preflight / run -------------------------------------------------------

fn styles_view() -> Value {
    json!({
        "styles": styles(),
        "default_style": wf_str("/default_style"),
        "title": wf_str("/title"),
        "locks": workflow()["locks"],
        "prompt_label": wf_str("/prompt/label"),
        "estimate": workflow()["estimate"],
    })
}

/// The source must be an existing image inside a configured root or the library.
fn checked_source(ctx: &Context, input: &Value) -> GenResult<PathBuf> {
    let raw = input
        .get("source_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if raw.is_empty() {
        return Err(GenError::new(
            "invalid_input",
            "KDP color thumbnail source_path is required",
        ));
    }
    let path = PathBuf::from(raw);
    if !is_image(&path) {
        return Err(GenError::new(
            "invalid_input",
            "KDP color thumbnail source must be an image",
        ));
    }
    let canon = path.canonicalize().map_err(|_| {
        GenError::new(
            "invalid_input",
            format!("KDP color thumbnail source does not exist: {raw}"),
        )
    })?;
    let mut allowed: Vec<PathBuf> = effective_roots(ctx)
        .iter()
        .filter_map(|r| Path::new(&r.path).canonicalize().ok())
        .collect();
    if let Ok(lib) = ctx.library_dir().canonicalize() {
        allowed.push(lib);
    }
    if !allowed.iter().any(|a| canon.starts_with(a)) {
        return Err(GenError::new(
            "path_not_allowed",
            "KDP color thumbnail source must be inside a configured KDP source folder",
        ));
    }
    Ok(canon)
}

/// Build the locked engine `generate` request plus the workflow record.
fn build_draft(ctx: &Context, input: &Value) -> GenResult<Value> {
    let style_id = input
        .get("style")
        .and_then(Value::as_str)
        .unwrap_or(wf_str("/default_style"));
    let style = style_by_id(style_id)?;
    let source = checked_source(ctx, input)?;
    let slug = slugify(
        input
            .get("project_slug")
            .and_then(Value::as_str)
            .unwrap_or(""),
        wf_str("/default_project_slug"),
    );
    // canonicalize() yields verbatim \\?\ paths on Windows; downstream validators
    // accept plain drive-letter paths, so simplify the prefix away.
    let source_str = source
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or_else(|| source.to_string_lossy().into_owned());
    let locks = &workflow()["locks"];
    let mut request = json!({
        "kind": "image",
        "provider": locks["provider"],
        "model": locks["model"],
        "prompt": build_prompt(&style.id)?,
        "params": {
            "aspect": locks["aspect"],
            "resolution": locks["resolution"],
            "image_urls": [source_str],
        },
    });
    if let Some(rid) = input
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        request["request_id"] = json!(rid);
    }
    Ok(json!({
        "status": "draft",
        "request": request,
        "workflow": {
            "kind": wf_str("/kind"),
            "style": style.id,
            "style_label": style.label,
            "locked_model": locks["model_label"],
            "locked_aspect": locks["aspect"],
            "locked_resolution": locks["resolution"],
            "project_slug": slug,
            "brand": locks["brand"],
            "source_path": source_str,
            "notes": format!("{} · {}", wf_str("/notes_prefix"), style.label),
        },
    }))
}

fn preflight(ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut draft = build_draft(ctx, input)?;
    let est = rightkit_media_gen::api::generate(ctx, &draft["request"], true)?;
    draft["est_usd"] = est["est_usd"].clone();
    // The page is a local file, and NanoBanana reads only URLs: the run uploads it to the
    // user's own fal storage, which needs `confirm_fal_upload: true` on `studio.kdp.run`.
    if est["requires_fal_upload_confirmation"] == true {
        draft["requires_fal_upload_confirmation"] = json!(true);
        draft["local_references"] = est["local_references"].clone();
    }
    Ok(draft)
}

/// The baseline image-studio request record for a KDP draft
/// (`createKdpColorThumbnailDraft` -> `normalizeImageStudioRequest`): the shape
/// the Media Console wrote into `cache/studio-requests/image/<draft_id>.json`.
fn legacy_request(draft: &Value) -> Value {
    let w = &draft["workflow"];
    let locks = &workflow()["locks"];
    json!({
        "mode": "image_to_image",
        "brand": w["brand"],
        "project_slug": w["project_slug"],
        "intent": "premium",
        "aspect": locks["aspect"],
        "prompt": draft["request"]["prompt"],
        "references": [w["source_path"]],
        "resolution": locks["resolution"],
        "quality": Value::Null,
        "negativePrompt": Value::Null,
        "style": Value::Null,
        "size": Value::Null,
        "image_size": Value::Null,
        "notes": w["notes"],
        "generation_params": {
            "resolution": locks["resolution"],
            "aspect": locks["aspect"],
            "references": [w["source_path"]],
        },
        "workflow": {
            "kind": w["kind"],
            "style": w["style"],
            "style_label": w["style_label"],
            "locked_model": w["locked_model"],
            "locked_aspect": w["locked_aspect"],
            "locked_resolution": w["locked_resolution"],
            "source_path": w["source_path"],
        },
    })
}

/// Persist the request manifest for a submitted run, like the baseline draft +
/// run manifests, so the image can be traced to its style, source and project
/// after a restart: `<data_dir>/studio/requests/image/<draft_id>.json`.
fn persist_manifest(ctx: &Context, draft: &Value) -> GenResult<PathBuf> {
    let job_id = draft["job"]["job_id"].as_str().unwrap_or_default();
    let created_at = rightkit_media_gen::store::now();
    let stamp: String = created_at
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    let slug = draft["workflow"]["project_slug"]
        .as_str()
        .unwrap_or_default();
    let draft_id = format!(
        "image-{stamp}-{slug}-{}",
        job_id.chars().take(8).collect::<String>()
    );
    let dir = ctx.data_dir.join("studio").join("requests").join("image");
    rightkit_fs::ensure_private_dir(&dir).map_err(|e| GenError::new("io", e.to_string()))?;
    let path = dir.join(format!("{draft_id}.json"));
    let manifest = json!({
        "schema_version": "1.0.0",
        "draft_id": draft_id,
        "status": "submitted",
        "execution_mode": "operator_approved_router_run",
        "created_at": created_at,
        "job_id": job_id,
        "request": legacy_request(draft),
        "engine_request": draft["request"],
        "route": {
            "intent": "premium",
            "provider": draft["request"]["provider"],
            "model": draft["request"]["model"],
            "label": "Premium",
        },
        "manifest_path": path.to_string_lossy(),
    });
    rightkit_fs::atomic_write(
        &path,
        &serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| GenError::new("io", e.to_string()))?;
    Ok(path)
}

fn run(ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut draft = build_draft(ctx, input)?;
    // Consent is per run: only the JSON boolean `true` on this call is forwarded. It is
    // added to the engine request here, after the draft, so the manifest never records it.
    let mut send = draft["request"].clone();
    if rightkit_media_gen::fal_storage::confirmed(input) {
        send[rightkit_media_gen::fal_storage::CONFIRM_FIELD] = json!(true);
    }
    let job = rightkit_media_gen::api::generate(ctx, &send, false)?;
    draft["status"] = json!("submitted");
    draft["job"] = job;
    let manifest = persist_manifest(ctx, &draft)?;
    draft["manifest_path"] = json!(manifest.to_string_lossy());
    Ok(draft)
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.kdp.styles" => Ok(styles_view()),
        "studio.kdp.settings" => Ok(settings_view(ctx)),
        "studio.kdp.settings_set" => settings_set(ctx, input),
        "studio.kdp.sources" => sources(ctx, input),
        "studio.kdp.preflight" => preflight(ctx, input),
        "studio.kdp.run" => run(ctx, input),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
