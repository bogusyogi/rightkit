//! The one upload to the user's own fal storage, shared by every flow that must
//! hand a local image to a provider that reads only URLs.
//!
//! Callers: character reference lock (the host character module, flag
//! `upload`), character LoRA training (dataset zip), and NanoBanana
//! reference images (KDP Colour Thumbnail, Image Studio image-to-image and the
//! Production hero frame all reach it through [`crate::api::generate_with`]).
//!
//! The destination is always the user's fal account, under the user's fal key
//! (`credentials` provider `fal`): `POST {rest}/storage/upload/initiate`, then a
//! `PUT` of the bytes to the returned `upload_url`. This crate never uses any
//! other host. Uploads are cached by content hash for seven days in
//! `<data>/characters/_uploads.json`.
//!
//! Consent for NanoBanana references is per run: see [`require_confirmation`].
//! The `confirm_fal_upload` request field is never defaulted, stored or inferred.

use crate::host::http::{Http, HttpError, Request, Response};
use crate::library::sha256_hex;
use crate::{credentials, store, Context, GenError, GenResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a cached upload URL is reused (fal's CDN files outlive this).
pub const UPLOAD_TTL_S: u64 = 7 * 24 * 3600;

/// Request field that authorises one upload run. Must be the JSON boolean `true`.
pub const CONFIRM_FIELD: &str = "confirm_fal_upload";

/// Error code returned when a local reference needs consent that was not given.
pub const CONFIRMATION_REQUIRED: &str = "upload_confirmation_required";

/// Where to add the key, as the UI words it (Settings > Providers > API keys).
pub const ADD_KEY_HINT: &str = "Settings > Providers > API keys > fal";

const IMAGE_EXT: [&str; 6] = ["png", "jpg", "jpeg", "webp", "avif", "gif"];

fn err(code: &str, message: impl Into<String>) -> GenError {
    GenError::new(code, message)
}

/// fal REST base. The override variable is honoured in debug builds only
/// (`providers::base_url_override`), so mock tests never loosen a release build.
pub fn rest_base() -> String {
    crate::providers::base_url_override("RIGHTKIT_FAL_REST_BASE", "https://rest.alpha.fal.ai")
}

/// The shared HTTP client for fal storage and queue calls.
pub fn http() -> Http {
    Http::new(Duration::from_secs(10), Duration::from_secs(600))
}

/// The user's fal key, or a typed refusal that says where to add one.
pub fn fal_key(ctx: &Context) -> GenResult<String> {
    credentials::get(ctx, "fal")?.ok_or_else(|| {
        err(
            "no_key",
            format!("No fal API key is configured. Add one in {ADD_KEY_HINT}; the image is uploaded to your own fal storage, so a fal key is required."),
        )
    })
}

pub fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

pub fn finish(r: Result<Response, HttpError>, key: &str) -> GenResult<(u16, String)> {
    let resp = r.map_err(|e| {
        err(
            "transport",
            redact(&format!("fal transport error: {e}"), key),
        )
    })?;
    let status = resp.status;
    // Invalid UTF-8 in the body reports as "fal read error"; a failed body read is a transport error above.
    // The body is capped at rightkit's default, 32 MiB.
    let body = String::from_utf8(resp.body)
        .map_err(|e| err("transport", redact(&format!("fal read error: {e}"), key)))?;
    Ok((status, body))
}

pub fn http_error(status: u16, body: &str, key: &str) -> GenError {
    let msg: String = redact(body, key).chars().take(300).collect();
    let code = match status {
        401 | 403 => "auth",
        402 => "billing",
        408 | 429 => "rate_limited",
        500..=599 => "unavailable",
        _ => "invalid_request",
    };
    err(code, format!("fal {status}: {msg}"))
}

pub fn fal_post(url: &str, key: &str, body: &Value) -> GenResult<Value> {
    let req = Request::post(url)
        .secret_header("Authorization", format!("Key {key}"))
        .json(body)
        .map_err(|e| {
            err(
                "transport",
                redact(&format!("fal transport error: {e}"), key),
            )
        })?;
    let r = http().send(req);
    let (status, text) = finish(r, key)?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &text, key));
    }
    serde_json::from_str(&text)
        .map_err(|e| err("bad_response", format!("fal returned invalid JSON: {e}")))
}

/// As [`fal_post`] for a paid submit, and says whether a failure left the outcome open:
/// the `bool` is true when the request may have reached fal (a timeout or dropped
/// connection after the connection opened, a 408/429/5xx reply, or a 2xx reply that could
/// not be read), false when it provably sent nothing or fal definitely rejected it (a
/// connect-phase failure, any other 4xx). Same rule as the provider adapters.
pub fn fal_post_checked(url: &str, key: &str, body: &Value) -> Result<Value, (GenError, bool)> {
    let req = Request::post(url)
        .secret_header("Authorization", format!("Key {key}"))
        .json(body)
        .map_err(|e| {
            (
                err(
                    "transport",
                    redact(&format!("fal transport error: {e}"), key),
                ),
                false,
            )
        })?;
    let sent = http().send(req);
    let delivered = crate::providers::request_may_have_reached(&sent);
    let replied = sent.is_ok();
    let (status, text) = finish(sent, key).map_err(|e| (e, delivered || replied))?;
    if !(200..300).contains(&status) {
        return Err((
            http_error(status, &text, key),
            crate::providers::status_may_have_run(status),
        ));
    }
    serde_json::from_str(&text).map_err(|e| {
        (
            err("bad_response", format!("fal returned invalid JSON: {e}")),
            true,
        )
    })
}

/// GET; returns the status and parsed body without failing on non-2xx.
pub fn fal_get(url: &str, key: &str) -> GenResult<(u16, Value, String)> {
    let r = http().send(Request::get(url).secret_header("Authorization", format!("Key {key}")));
    let (status, text) = finish(r, key)?;
    Ok((
        status,
        serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    ))
}

/// Initiate + PUT to the user's fal storage; returns the file URL fal serves it from.
pub fn upload_bytes(
    key: &str,
    bytes: &[u8],
    file_name: &str,
    content_type: &str,
) -> GenResult<String> {
    let init = fal_post(
        &format!(
            "{}/storage/upload/initiate?storage_type=fal-cdn-v3",
            rest_base()
        ),
        key,
        &json!({"content_type": content_type, "file_name": file_name}),
    )?;
    let upload_url = init["upload_url"]
        .as_str()
        .ok_or_else(|| err("bad_response", "fal upload initiate returned no upload_url"))?;
    let file_url = init["file_url"]
        .as_str()
        .ok_or_else(|| err("bad_response", "fal upload initiate returned no file_url"))?;
    let r = http().send(
        Request::put(upload_url)
            .header("Content-Type", content_type)
            .body(bytes.to_vec()),
    );
    let (status, text) = finish(r, key)?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &text, key));
    }
    Ok(file_url.to_string())
}

pub fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn mime_for(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("gif") => "image/gif",
        _ => "image/png",
    }
}

fn epoch_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("characters").join("_uploads.json")
}

fn read_cache(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Upload a local image to the user's fal storage (free), cached by content hash for 7 days.
///
/// The fal key is checked first, so a missing key refuses before any file is read
/// or any request is made, whether or not the image is cached.
pub fn upload_file(ctx: &Context, path: &Path) -> GenResult<String> {
    let key = fal_key(ctx)?;
    if !path.is_file() || !is_image(path) {
        return Err(err(
            "invalid_input",
            format!("reference is not an image file: {}", path.display()),
        ));
    }
    let bytes = std::fs::read(path).map_err(store::io_err)?;
    let sha = sha256_hex(&bytes);
    let cache_file = cache_path(ctx);
    let mut cache = read_cache(&cache_file);
    if let Some(e) = cache.get(&sha) {
        if let (Some(url), Some(at)) = (e["url"].as_str(), e["at"].as_u64()) {
            if epoch_s().saturating_sub(at) < UPLOAD_TTL_S {
                return Ok(url.to_string());
            }
        }
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("reference.png");
    let url = upload_bytes(&key, &bytes, name, mime_for(path))?;
    cache[&sha] = json!({"url": url, "at": epoch_s()});
    if let Some(dir) = cache_file.parent() {
        std::fs::create_dir_all(dir).map_err(store::io_err)?;
    }
    rightkit_fs::atomic_write(
        &cache_file,
        &serde_json::to_vec_pretty(&cache).unwrap_or_default(),
    )
    .map_err(|e| store::io_err(std::io::Error::other(e)))?;
    Ok(url)
}

// ---------------------------------------------------------------------------
// NanoBanana references: local file -> user's fal storage, with per-run consent
// ---------------------------------------------------------------------------

/// A local reference: `file://` URL or an absolute path (POSIX, drive letter, or a Windows
/// UNC or verbatim path such as `\\?\D:\x.png`).
/// `http://` and `https://` values are never local.
pub fn local_path(u: &str) -> Option<PathBuf> {
    if u.starts_with("http://") || u.starts_with("https://") {
        return None;
    }
    let p = u.strip_prefix("file://").unwrap_or(u);
    let abs = p.starts_with('/')
        || p.starts_with("\\\\")
        || (p.len() > 2
            && p.as_bytes()[1] == b':'
            && (p.as_bytes()[2] == b'/' || p.as_bytes()[2] == b'\\'));
    abs.then(|| PathBuf::from(p))
}

/// One entry of `params.image_urls` that is a local file.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRef {
    pub index: usize,
    pub path: PathBuf,
}

/// The local-file entries of `params.image_urls` (NanoBanana's reference list).
/// Remote https/http URLs and malformed entries are left for the provider to judge.
pub fn local_image_refs(params: &Value) -> Vec<LocalRef> {
    params
        .get("image_urls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .filter_map(|(index, v)| {
                    v.as_str()
                        .and_then(local_path)
                        .map(|path| LocalRef { index, path })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// True only for the JSON boolean `true` in the request. No default, no inference.
pub fn confirmed(input: &Value) -> bool {
    input.get(CONFIRM_FIELD).and_then(Value::as_bool) == Some(true)
}

/// Refuse, with `upload_confirmation_required`, unless this run is confirmed.
/// Pure: no network, no file read, no ledger access.
pub fn require_confirmation(refs: &[LocalRef], confirmed: bool) -> GenResult<()> {
    if refs.is_empty() || confirmed {
        return Ok(());
    }
    let names: Vec<String> = refs
        .iter()
        .map(|r| {
            r.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image")
                .to_string()
        })
        .collect();
    let paths: Vec<String> = refs
        .iter()
        .map(|r| r.path.to_string_lossy().into_owned())
        .collect();
    Err(err(
        CONFIRMATION_REQUIRED,
        format!(
            "This app uploads this image to your fal storage so NanoBanana can read it ({} local image{}: {}). Send {CONFIRM_FIELD}: true to allow this for this run only.",
            refs.len(),
            if refs.len() == 1 { "" } else { "s" },
            names.join(", ")
        ),
    )
    .with_details(json!({
        "destination": "your fal storage",
        "provider": "nanobanana",
        "count": refs.len(),
        "paths": paths,
        "confirm_field": CONFIRM_FIELD,
    })))
}

/// Upload every local reference to the user's fal storage and rewrite
/// `params.image_urls` in place. Call only after [`require_confirmation`] and
/// [`fal_key`] have passed.
pub fn upload_refs(ctx: &Context, params: &mut Value, refs: &[LocalRef]) -> GenResult<()> {
    for r in refs {
        let url = upload_file(ctx, &r.path)?;
        if let Some(slot) = params
            .get_mut("image_urls")
            .and_then(Value::as_array_mut)
            .and_then(|a| a.get_mut(r.index))
        {
            *slot = Value::String(url);
        }
    }
    Ok(())
}

/// Stand-in URL used only to price a request whose local references have not
/// been uploaded yet (dry runs and the pre-flight estimate). Never sent anywhere.
pub const PRICING_PLACEHOLDER: &str = "https://pending-upload.invalid/reference.png";

/// A copy of `params` with each local reference replaced by [`PRICING_PLACEHOLDER`].
pub fn priced_params(params: &Value, refs: &[LocalRef]) -> Value {
    let mut out = params.clone();
    if let Some(a) = out.get_mut("image_urls").and_then(Value::as_array_mut) {
        for r in refs {
            if let Some(slot) = a.get_mut(r.index) {
                *slot = Value::String(PRICING_PLACEHOLDER.into());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Declared catalogue rows (inpaint, relight, face swap, template swap): any url-type input
// may hold a local file. fal rows upload it to the user's fal storage; WaveSpeed has no
// upload path wired here, so a WaveSpeed row uses fal storage only when a fal key exists.
// ---------------------------------------------------------------------------

/// A local file held by a url-type param (`index` is Some for a `url_list` item).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamRef {
    pub param: String,
    pub index: Option<usize>,
    pub path: PathBuf,
}

/// Local files in the named params (string or array of strings).
pub fn local_param_refs(params: &Value, names: &[String]) -> Vec<ParamRef> {
    let mut out = Vec::new();
    for n in names {
        match params.get(n) {
            Some(Value::String(s)) => {
                if let Some(path) = local_path(s) {
                    out.push(ParamRef {
                        param: n.clone(),
                        index: None,
                        path,
                    });
                }
            }
            Some(Value::Array(a)) => {
                for (i, v) in a.iter().enumerate() {
                    if let Some(path) = v.as_str().and_then(local_path) {
                        out.push(ParamRef {
                            param: n.clone(),
                            index: Some(i),
                            path,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn set_param(params: &mut Value, r: &ParamRef, url: &str) {
    match r.index {
        None => {
            if let Some(slot) = params.get_mut(&r.param) {
                *slot = Value::String(url.into());
            }
        }
        Some(i) => {
            if let Some(slot) = params
                .get_mut(&r.param)
                .and_then(Value::as_array_mut)
                .and_then(|a| a.get_mut(i))
            {
                *slot = Value::String(url.into());
            }
        }
    }
}

/// Refuse with `upload_confirmation_required` (listing the files) unless this run is confirmed.
/// `provider` names the row's provider in the message. Pure: no network, no file read.
pub fn require_param_confirmation(
    refs: &[ParamRef],
    confirmed: bool,
    provider: &str,
) -> GenResult<()> {
    if refs.is_empty() || confirmed {
        return Ok(());
    }
    let names: Vec<String> = refs
        .iter()
        .map(|r| {
            r.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image")
                .to_string()
        })
        .collect();
    let paths: Vec<String> = refs
        .iter()
        .map(|r| r.path.to_string_lossy().into_owned())
        .collect();
    Err(err(
        CONFIRMATION_REQUIRED,
        format!(
            "This app uploads local files to your fal storage so {provider} can read them ({} local file{}: {}). Send {CONFIRM_FIELD}: true to allow this for this run only.",
            refs.len(),
            if refs.len() == 1 { "" } else { "s" },
            names.join(", ")
        ),
    )
    .with_details(json!({
        "destination": "your fal storage",
        "provider": provider,
        "count": refs.len(),
        "paths": paths,
        "confirm_field": CONFIRM_FIELD,
    })))
}

/// Upload every local param file to the user's fal storage and rewrite the params in place.
/// Call only after [`require_param_confirmation`] and [`fal_key`] have passed.
pub fn upload_param_refs(ctx: &Context, params: &mut Value, refs: &[ParamRef]) -> GenResult<()> {
    for r in refs {
        let url = upload_file(ctx, &r.path)?;
        set_param(params, r, &url);
    }
    Ok(())
}

/// A copy of `params` with each local file replaced by [`PRICING_PLACEHOLDER`] (pricing only).
pub fn priced_param_refs(params: &Value, refs: &[ParamRef]) -> Value {
    let mut out = params.clone();
    for r in refs {
        set_param(&mut out, r, PRICING_PLACEHOLDER);
    }
    out
}

#[cfg(test)]
mod param_ref_tests {
    use super::*;

    #[test]
    fn finds_local_files_in_strings_and_lists_only() {
        let p = json!({"image_url": "/tmp/a.png", "mask_url": "https://x/m.png", "image_urls": ["https://x/1.png", "C:\\pics\\b.png"], "prompt": "/not/a/param"});
        let names = vec![
            "image_url".to_string(),
            "mask_url".into(),
            "image_urls".into(),
        ];
        let refs = local_param_refs(&p, &names);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].param, "image_url");
        assert_eq!(refs[1].index, Some(1));
    }

    #[test]
    fn refuses_without_confirmation_listing_files_and_prices_with_placeholder() {
        let p = json!({"image_url": "/tmp/a.png"});
        let refs = local_param_refs(&p, &["image_url".to_string()]);
        let e = require_param_confirmation(&refs, false, "wavespeed").unwrap_err();
        assert_eq!(e.code, CONFIRMATION_REQUIRED);
        assert!(require_param_confirmation(&refs, true, "wavespeed").is_ok());
        assert_eq!(
            priced_param_refs(&p, &refs)["image_url"],
            PRICING_PLACEHOLDER
        );
        let mut q = p.clone();
        set_param(&mut q, &refs[0], "https://cdn/x.png");
        assert_eq!(q["image_url"], "https://cdn/x.png");
    }
}
