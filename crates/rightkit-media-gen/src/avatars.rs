//! studio.avatars: HeyGen avatar creation (digital twin from video, photo avatar, prompt
//! avatar). Ported from the Media Console baseline (a7271cf `server/studio-heygen-avatar.mjs`,
//! `server/studio-heygen-avatar-run.mjs`, `tools/backends/heygen.js`); the avatar-video
//! generation adapter lives in `crate::providers::heygen`.
//!
//! Endpoints (HeyGen API v3, auth header `X-Api-Key`; shapes read from the live reference on
//! 2026-10-06 at developers.heygen.com):
//! - `POST /v3/assets/direct-uploads` `{filename, content_type, size_bytes}` ->
//!   `{data:{asset_id, upload_url, upload_headers}}`; `PUT upload_url` with the bytes and the
//!   returned headers verbatim; `POST /v3/assets/{id}/complete`. Limit 200 MiB for non-audio.
//! - `POST /v3/avatars`: `{type:"digital_twin"|"photo", name, file, avatar_group_id?}` or
//!   `{type:"prompt", name, prompt (<=1000), reference_images? (<=3), avatar_group_id?,
//!   avatar_id?, aspect_ratio?}` -> `{data:{avatar_item, avatar_group}}`. `file` and each
//!   reference image is an asset input `{type:"url",url}` or `{type:"asset_id",asset_id}`.
//! - `POST /v3/avatars/{group_id}/consent` `{reroute_url?, consent_text?}` ->
//!   `{data:{avatar_group, url}}`; the person opens `url` to give provider consent.
//! - `GET /v3/avatars/{group_id}` -> group with `status` (processing | pending_consent | failed
//!   | completed) and `consent_status`; `GET /v3/avatars?ownership=private&limit&token` lists.
//!
//! Consent invariant (plan: clones require consent): `create_twin` and `create_photo` copy a
//! real person's likeness, so they refuse to run without `consent:true` in the input, which
//! the caller sets only after the person depicted has agreed. HeyGen's own consent step
//! follows (`consent_url`). `create_prompt` depicts no real person and needs no flag.
//!
//! Creation cost is account/plan gated: HeyGen publishes no API price. Results carry
//! `est_usd:null` plus [`PRICE_NOTICE`] (`cost_notice`, `price_published:false`), never an
//! invented amount and never a reservation. Because the app cannot cap an unpriced spend, a real
//! (non-dry-run) create needs the explicit per-run `confirm_unpublished_price:true`
//! ([`CONFIRM_FIELD`]; same pattern as `confirm_fal_upload`; never defaulted, stored or inferred),
//! else it is refused with `price_confirmation_required` before any key lookup or network call.
//! `dry_run:true` validates the input and returns the exact request body with no network call.
//!
//! Commands:
//! - `studio.avatars.create_twin`: `{name, consent, video_path|video_url|video_asset_id, avatar_group_id?, consent_link?, consent_redirect_url?, consent_text?, request_id?, dry_run?}`
//! - `studio.avatars.create_photo`: `{name, consent, image_path|image_url|image_asset_id, avatar_group_id?, consent_link?, ..., dry_run?}`
//! - `studio.avatars.create_prompt`: `{name, prompt, reference_images?:[{path|url|asset_id}], aspect_ratio?, avatar_group_id?, avatar_id?, dry_run?}`
//! - `studio.avatars.status`: `{avatar_group_id}`
//! - `studio.avatars.list`: `{ownership?:"private"|"public", limit?, token?}`
//!
//! Generation routes (avatar video, lip-sync) are shared with the media lane:
//! `studio.avatars.routes` reports every local/API/rented route for the avatar
//! and lip-sync capabilities with its readiness and quality state, and
//! `studio.avatars.animate` / `studio.avatars.lip_sync` validate a request and
//! return the exact `media.generate`/`generate` body (plus `submit_via`) — they
//! never submit or spend themselves. `studio.avatars.review` records the
//! operator's quality verdict for a created avatar or a rendered output so
//! unqualified results stay visibly distinct from accepted ones.

use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::Duration;

use crate::{Context, GenError, GenResult};

/// The host's model-management command handler (`studio.models.catalog`, `studio.models.workers`),
/// which the avatar route listing reads for local and rented avatar engines.
pub type ModelsHandler = fn(&Context, &str, &Value) -> GenResult<Value>;

static MODELS_HANDLER: std::sync::OnceLock<ModelsHandler> = std::sync::OnceLock::new();

/// Register the host's model-management handler once. Without one, the avatar routes list only
/// the API providers. Returns the rejected handler when one is already set.
pub fn set_models_handler(handler: ModelsHandler) -> Result<(), ModelsHandler> {
    MODELS_HANDLER.set(handler)
}

fn models_handler(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match MODELS_HANDLER.get() {
        Some(handler) => handler(ctx, name, input),
        None => Err(GenError::new(
            "not_configured",
            "no model-management handler is registered",
        )),
    }
}
use crate::host::http::{Http, HttpError, Request, Response};
use crate::{ledger, store};

pub const COMMANDS: &[&str] = &[
    "studio.avatars.create_twin",
    "studio.avatars.create_photo",
    "studio.avatars.create_prompt",
    "studio.avatars.status",
    "studio.avatars.list",
    "studio.avatars.routes",
    "studio.avatars.animate",
    "studio.avatars.lip_sync",
    "studio.avatars.review",
];

/// Per-run consent for an unpriced spend. Only the JSON boolean `true` counts.
pub const CONFIRM_FIELD: &str = "confirm_unpublished_price";
/// Shown before the user confirms and returned in every create result.
pub const PRICE_NOTICE: &str = "Price not published: HeyGen does not publish an API price for avatar creation, so the app cannot estimate or reserve it. It may use your HeyGen plan credits or bill your account, and it is not counted against the app's daily cap.";

const MAX_UPLOAD_BYTES: u64 = 200 * 1024 * 1024;
const MAX_PROMPT_CHARS: usize = 1000;
const MAX_REFERENCES: usize = 3;
const MAX_NAME_CHARS: usize = 120;
const ASPECTS: [&str; 6] = ["auto", "16:9", "9:16", "1:1", "4:5", "5:4"];
const REGISTRY_FILE: &str = "avatars/created.json";

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    let base =
        crate::providers::base_url_override("RIGHTKIT_HEYGEN_BASE", crate::providers::heygen::BASE);
    handle_at(ctx, name, input, &base)
}

fn handle_at(ctx: &Context, name: &str, input: &Value, base: &str) -> GenResult<Value> {
    match name {
        "studio.avatars.create_twin" => create(ctx, input, base, Kind::Twin),
        "studio.avatars.create_photo" => create(ctx, input, base, Kind::Photo),
        "studio.avatars.create_prompt" => create(ctx, input, base, Kind::Prompt),
        "studio.avatars.status" => status(ctx, input, base),
        "studio.avatars.list" => list(ctx, input, base),
        "studio.avatars.routes" => routes(ctx, input),
        "studio.avatars.animate" => animate(ctx, input),
        "studio.avatars.lip_sync" => lip_sync(ctx, input),
        "studio.avatars.review" => review(ctx, input),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Twin,
    Photo,
    Prompt,
}

impl Kind {
    fn api_type(self) -> &'static str {
        match self {
            Kind::Twin => "digital_twin",
            Kind::Photo => "photo",
            Kind::Prompt => "prompt",
        }
    }
    fn needs_consent(self) -> bool {
        self != Kind::Prompt
    }
}

fn bad(msg: impl Into<String>) -> GenError {
    GenError::new("invalid_input", msg)
}

fn text<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ---- HTTP ----

struct Client {
    base: String,
    key: String,
    http: Http,
}

impl Client {
    fn new(base: &str, key: String) -> Self {
        let http = Http::new(Duration::from_secs(10), Duration::from_secs(600));
        Self {
            base: base.trim_end_matches('/').to_string(),
            key,
            http,
        }
    }

    fn redact(&self, s: &str) -> String {
        rightkit_logs::KnownSecrets::from_values([self.key.as_str()]).redact(s)
    }

    /// Send once with the 16 MiB reply cap; non-2xx replies are returned, not raised.
    fn exchange(&self, req: Request) -> GenResult<(u16, Vec<u8>)> {
        self.finish(self.http.send(req.max_response_bytes(16 << 20)))
    }

    fn finish(&self, r: Result<Response, HttpError>) -> GenResult<(u16, Vec<u8>)> {
        // The body is read inside `send`, so a failed body read is a transport error with no status.
        let resp = r.map_err(|e| {
            GenError::new(
                "transport",
                self.redact(&format!("heygen transport error: {e}")),
            )
        })?;
        Ok((resp.status, resp.body))
    }

    fn error_for(&self, status: u16, body: &[u8]) -> GenError {
        let v: Option<Value> = serde_json::from_slice(body).ok();
        let msg = v
            .as_ref()
            .and_then(|v| {
                v.pointer("/error/message")
                    .or_else(|| v.get("message"))
                    .or_else(|| v.get("error"))
            })
            .map(|d| {
                d.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| d.to_string())
            })
            .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
        let msg = self.redact(&msg.chars().take(300).collect::<String>());
        let lower = msg.to_ascii_lowercase();
        let money = ["insufficient", "quota", "balance", "credit", "plan"]
            .iter()
            .any(|w| lower.contains(w));
        if money && matches!(status, 400 | 402 | 403) {
            return GenError::new("billing", format!("heygen billing/plan: {msg}"));
        }
        match status {
            401 | 403 => GenError::new(
                "auth",
                format!("heygen rejected the API key ({status}): {msg}"),
            ),
            402 => GenError::new("billing", format!("heygen billing/plan: {msg}")),
            408 | 429 => GenError::new("rate_limited", format!("heygen {status}: {msg}")),
            500..=599 => GenError::new("unavailable", format!("heygen {status}: {msg}")),
            s => GenError::new("invalid_request", format!("heygen {s}: {msg}")),
        }
    }

    fn json_reply(&self, status: u16, body: &[u8]) -> GenResult<Value> {
        if !(200..300).contains(&status) {
            return Err(self.error_for(status, body));
        }
        serde_json::from_slice(body)
            .map_err(|_| GenError::new("bad_response", "heygen reply was not JSON"))
    }

    fn get(&self, path_and_query: &str) -> GenResult<Value> {
        let req = Request::get(format!("{}{}", self.base, path_and_query))
            .secret_header("X-Api-Key", &self.key);
        let (status, body) = self.exchange(req)?;
        self.json_reply(status, &body)
    }

    fn post(&self, path: &str, body: &Value, idempotency: Option<&str>) -> GenResult<Value> {
        let mut req =
            Request::post(format!("{}{}", self.base, path)).secret_header("X-Api-Key", &self.key);
        if let Some(k) = idempotency {
            req = req.header("Idempotency-Key", k);
        }
        let req = req.json(body).map_err(|e| {
            GenError::new(
                "transport",
                self.redact(&format!("heygen transport error: {e}")),
            )
        })?;
        let (status, reply) = self.exchange(req)?;
        self.json_reply(status, &reply)
    }

    /// init -> PUT bytes -> complete. Returns the reusable `asset_id`.
    fn upload(
        &self,
        path: &Path,
        content_type: &str,
        idem_root: Option<&str>,
    ) -> GenResult<String> {
        let bytes =
            std::fs::read(path).map_err(|e| bad(format!("cannot read {}: {e}", path.display())))?;
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload");
        let init_key = idem_root.map(|r| format!("{r}:init"));
        let created = self.post(
            "/v3/assets/direct-uploads",
            &json!({"filename": filename, "content_type": content_type, "size_bytes": bytes.len()}),
            init_key.as_deref(),
        )?;
        let d = created.get("data").unwrap_or(&created);
        let asset_id = d
            .get("asset_id")
            .and_then(Value::as_str)
            .filter(|s| valid_id(s))
            .ok_or_else(|| GenError::new("bad_response", "heygen upload reply had no asset_id"))?
            .to_string();
        let url = d
            .get("upload_url")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            .ok_or_else(|| {
                GenError::new("bad_response", "heygen upload reply had no upload_url")
            })?;
        let mut put = Request::put(url);
        let mut has_ct = false;
        if let Some(h) = d.get("upload_headers").and_then(Value::as_object) {
            for (k, v) in h {
                if let Some(v) = v.as_str() {
                    has_ct |= k.eq_ignore_ascii_case("content-type");
                    put = put.header(k.as_str(), v);
                }
            }
        }
        if !has_ct {
            put = put.set_header("Content-Type", content_type);
        }
        let (status, reply) = self.exchange(put.body(bytes))?;
        if !(200..300).contains(&status) {
            return Err(GenError::new(
                "upload_failed",
                self.redact(&format!(
                    "heygen direct upload failed ({status}): {}",
                    String::from_utf8_lossy(&reply)
                        .chars()
                        .take(200)
                        .collect::<String>()
                )),
            ));
        }
        let complete_key = idem_root.map(|r| format!("{r}:complete"));
        self.post(
            &format!("/v3/assets/{asset_id}/complete"),
            &json!({}),
            complete_key.as_deref(),
        )
        .map_err(|mut e| {
            e.message = format!("{} (orphaned asset {asset_id})", e.message);
            e
        })?;
        Ok(asset_id)
    }
}

/// Key lookup honoring the configured source plus the real credential store.
fn cred_get(ctx: &Context, provider: &str) -> GenResult<Option<String>> {
    crate::credentials::get(ctx, provider)
}

fn cred_status(ctx: &Context, provider: &str) -> Option<&'static str> {
    crate::credentials::status_source(ctx, provider)
}

fn key(ctx: &Context) -> GenResult<String> {
    cred_get(ctx, "heygen")?
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            GenError::new(
                "no_key",
                "no HeyGen API key: add one under Models & Keys (or set HEYGEN_API_KEY)",
            )
        })
}

// ---- Asset inputs ----

/// A source asset as the caller gave it, before any upload.
#[derive(Debug, PartialEq)]
enum Source {
    Path {
        path: std::path::PathBuf,
        content_type: &'static str,
    },
    Url(String),
    AssetId(String),
}

fn content_type(path: &Path, want_video: bool) -> GenResult<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match (want_video, ext.as_str()) {
        (true, "mp4") => Ok("video/mp4"),
        (true, "mov") => Ok("video/quicktime"),
        (true, "webm") => Ok("video/webm"),
        (false, "png") => Ok("image/png"),
        (false, "jpg") | (false, "jpeg") => Ok("image/jpeg"),
        (false, "webp") => Ok("image/webp"),
        _ => Err(bad(format!(
            "{} must be a {} file ({})",
            path.display(),
            if want_video { "video" } else { "image" },
            if want_video {
                "mp4, mov, webm"
            } else {
                "png, jpg, webp"
            }
        ))),
    }
}

/// Exactly one of `<prefix>_path|_url|_asset_id` (or the keys in `keys`).
fn source_from(v: &Value, keys: [&str; 3], label: &str, want_video: bool) -> GenResult<Source> {
    let given: Vec<(usize, &str)> = keys
        .iter()
        .enumerate()
        .filter_map(|(i, k)| text(v, k).map(|t| (i, t)))
        .collect();
    if given.len() != 1 {
        return Err(bad(format!(
            "{label} must be given as exactly one of {}, {}, or {}",
            keys[0], keys[1], keys[2]
        )));
    }
    let (which, value) = given[0];
    match which {
        0 => {
            let path = std::path::PathBuf::from(value);
            let ct = content_type(&path, want_video)?;
            let meta = std::fs::metadata(&path)
                .map_err(|_| bad(format!("{} does not exist", path.display())))?;
            if !meta.is_file() {
                return Err(bad(format!("{} is not a file", path.display())));
            }
            if meta.len() > MAX_UPLOAD_BYTES {
                return Err(bad(format!(
                    "{} is larger than HeyGen's 200 MiB upload limit",
                    path.display()
                )));
            }
            Ok(Source::Path {
                path,
                content_type: ct,
            })
        }
        1 => {
            if !value.to_ascii_lowercase().starts_with("https://") {
                return Err(bad(format!("{} must be an https URL", keys[1])));
            }
            Ok(Source::Url(value.to_string()))
        }
        _ => {
            if !valid_id(value) {
                return Err(bad(format!("{} is not a valid asset id", keys[2])));
            }
            Ok(Source::AssetId(value.to_string()))
        }
    }
}

/// Reference images: `[{path|url|asset_id}]`, at most three.
fn references_from(input: &Value) -> GenResult<Vec<Source>> {
    let Some(list) = input.get("reference_images") else {
        return Ok(vec![]);
    };
    let list = list
        .as_array()
        .ok_or_else(|| bad("reference_images must be an array"))?;
    if list.len() > MAX_REFERENCES {
        return Err(bad(format!(
            "HeyGen prompt avatars take at most {MAX_REFERENCES} reference images"
        )));
    }
    list.iter()
        .enumerate()
        .map(|(i, r)| {
            source_from(
                r,
                ["path", "url", "asset_id"],
                &format!("reference_images[{i}]"),
                false,
            )
        })
        .collect()
}

/// Turns a source into the API asset input, uploading local files unless `upload` is false
/// (dry run), in which case a placeholder shows what would happen.
fn asset_input(
    client: Option<&Client>,
    src: &Source,
    idem: Option<&str>,
    uploaded: &mut Vec<String>,
) -> GenResult<Value> {
    match src {
        Source::Url(u) => Ok(json!({"type": "url", "url": u})),
        Source::AssetId(id) => Ok(json!({"type": "asset_id", "asset_id": id})),
        Source::Path { path, content_type } => match client {
            Some(c) => {
                let id = c.upload(path, content_type, idem)?;
                uploaded.push(id.clone());
                Ok(json!({"type": "asset_id", "asset_id": id}))
            }
            None => Ok(
                json!({"type": "asset_id", "asset_id": format!("<upload of {}>", path.display())}),
            ),
        },
    }
}

// ---- Create ----

fn create(ctx: &Context, input: &Value, base: &str, kind: Kind) -> GenResult<Value> {
    let name = text(input, "name").ok_or_else(|| bad("name is required"))?;
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(bad(format!(
            "name must be {MAX_NAME_CHARS} characters or fewer"
        )));
    }
    if kind.needs_consent() && input.get("consent").and_then(Value::as_bool) != Some(true) {
        return Err(GenError::new(
            "consent_required",
            "this copies a real person's likeness: set consent:true only after the person shown has agreed (HeyGen then asks them to confirm)",
        ));
    }
    let group = text(input, "avatar_group_id");
    if let Some(g) = group {
        if !valid_id(g) {
            return Err(bad("avatar_group_id is not a valid id"));
        }
    }
    let dry_run = input
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_id = text(input, "request_id");
    if let Some(r) = request_id {
        if r.len() > 100 {
            return Err(bad("request_id must be 100 characters or fewer"));
        }
    }
    let idem_root =
        request_id.map(|r| format!("{}-avatar:{r}", crate::host::config().settings_namespace));

    // Validate everything and resolve sources before any network call.
    enum Plan {
        File(Source),
        Prompt { prompt: String, refs: Vec<Source> },
    }
    let plan = match kind {
        Kind::Twin => Plan::File(source_from(
            input,
            ["video_path", "video_url", "video_asset_id"],
            "training video",
            true,
        )?),
        Kind::Photo => Plan::File(source_from(
            input,
            ["image_path", "image_url", "image_asset_id"],
            "source image",
            false,
        )?),
        Kind::Prompt => {
            let prompt = text(input, "prompt").ok_or_else(|| bad("prompt is required"))?;
            if prompt.chars().count() > MAX_PROMPT_CHARS {
                return Err(bad(format!(
                    "prompt must be {MAX_PROMPT_CHARS} characters or fewer"
                )));
            }
            Plan::Prompt {
                prompt: prompt.to_string(),
                refs: references_from(input)?,
            }
        }
    };
    let aspect = match text(input, "aspect_ratio") {
        Some(a) if kind == Kind::Prompt => {
            if !ASPECTS.contains(&a) {
                return Err(bad(format!(
                    "aspect_ratio must be one of {}",
                    ASPECTS.join(", ")
                )));
            }
            Some(a)
        }
        Some(_) => return Err(bad("aspect_ratio applies to prompt avatars only")),
        None => None,
    };
    let want_consent_link = input
        .get("consent_link")
        .and_then(Value::as_bool)
        .unwrap_or(kind.needs_consent());

    if !dry_run && input.get(CONFIRM_FIELD).and_then(Value::as_bool) != Some(true) {
        return Err(GenError::new(
            "price_confirmation_required",
            format!("{PRICE_NOTICE} Send {CONFIRM_FIELD}: true to create this avatar for this run only."),
        )
        .with_details(json!({"confirm_field": CONFIRM_FIELD, "price_published": false, "cost_notice": PRICE_NOTICE})));
    }

    let client = if dry_run {
        None
    } else {
        Some(Client::new(base, key(ctx)?))
    };
    let mut uploaded = Vec::new();
    let mut body = Map::new();
    body.insert("type".into(), json!(kind.api_type()));
    body.insert("name".into(), json!(name));
    match &plan {
        Plan::File(src) => {
            body.insert(
                "file".into(),
                asset_input(client.as_ref(), src, idem_root.as_deref(), &mut uploaded)?,
            );
        }
        Plan::Prompt { prompt, refs } => {
            body.insert("prompt".into(), json!(prompt));
            if !refs.is_empty() {
                let mut items = Vec::new();
                for (i, r) in refs.iter().enumerate() {
                    let idem = idem_root.as_ref().map(|k| format!("{k}:ref{i}"));
                    items.push(asset_input(
                        client.as_ref(),
                        r,
                        idem.as_deref(),
                        &mut uploaded,
                    )?);
                }
                body.insert("reference_images".into(), Value::Array(items));
            }
            if let Some(a) = aspect {
                body.insert("aspect_ratio".into(), json!(a));
            }
            if let Some(v) = text(input, "avatar_id") {
                if !valid_id(v) {
                    return Err(bad("avatar_id is not a valid id"));
                }
                body.insert("avatar_id".into(), json!(v));
            }
        }
    }
    if let Some(g) = group {
        body.insert("avatar_group_id".into(), json!(g));
    }
    let body = Value::Object(body);

    let Some(client) = client else {
        return Ok(json!({
            "dry_run": true,
            "avatar_type": kind.api_type(),
            "request_body": body,
            "consent_link": want_consent_link,
            "est_usd": null,
            "price_published": false,
            "cost_notice": PRICE_NOTICE,
            "confirm_field": CONFIRM_FIELD,
            "note": "Nothing was sent. HeyGen avatar creation cost is account/plan gated.",
        }));
    };

    // The unpriced spend commits with no reservation, so the per-run confirmation is the only record
    // of it: leave an audit row before the POST (never the key). Written after the key lookup, so a
    // refusal that sends nothing leaves no row.
    ledger::audit_setting(
        &store::open(ctx)?,
        "avatar.confirm_unpublished_price",
        &json!({
            "confirm_field": CONFIRM_FIELD,
            "confirmed": true,
            "avatar_type": kind.api_type(),
            "name": name,
            "avatar_group_id": group,
            "price_published": false,
        }),
    )?;
    let created = client.post(
        "/v3/avatars",
        &body,
        idem_root.as_ref().map(|r| format!("{r}:create")).as_deref(),
    )?;
    let data = created.get("data").unwrap_or(&created);
    let item = data.get("avatar_item").filter(|v| !v.is_null());
    let grp = data.get("avatar_group").filter(|v| !v.is_null());
    let group_id = grp
        .and_then(|g| g.get("id"))
        .or_else(|| item.and_then(|i| i.get("group_id")))
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| group.map(String::from));
    let item_id = item
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
        .map(String::from);

    let mut consent_url: Option<String> = None;
    let mut consent_error: Option<String> = None;
    if want_consent_link {
        match group_id.as_deref() {
            Some(g) if valid_id(g) => {
                let mut cbody = Map::new();
                if let Some(u) = text(input, "consent_redirect_url") {
                    if !u.to_ascii_lowercase().starts_with("https://") {
                        consent_error = Some(
                            "consent_redirect_url must be https; consent link not requested".into(),
                        );
                    } else {
                        cbody.insert("reroute_url".into(), json!(u));
                    }
                }
                if let Some(t) = text(input, "consent_text") {
                    cbody.insert(
                        "consent_text".into(),
                        json!(t.chars().take(1000).collect::<String>()),
                    );
                }
                if consent_error.is_none() {
                    let key = idem_root.as_ref().map(|r| format!("{r}:consent"));
                    match client.post(
                        &format!("/v3/avatars/{g}/consent"),
                        &Value::Object(cbody),
                        key.as_deref(),
                    ) {
                        Ok(v) => {
                            let d = v.get("data").unwrap_or(&v);
                            consent_url = d.get("url").and_then(Value::as_str).map(String::from);
                        }
                        // The avatar exists; report the failure instead of hiding the result.
                        Err(e) => consent_error = Some(e.message),
                    }
                }
            }
            _ => {
                consent_error = Some(
                    "HeyGen returned no avatar group id, so no consent link was created".into(),
                )
            }
        }
    }

    let status = grp
        .and_then(|g| g.get("status"))
        .or_else(|| item.and_then(|i| i.get("status")))
        .and_then(Value::as_str)
        .unwrap_or("submitted")
        .to_string();
    let consent_status = grp
        .and_then(|g| g.get("consent_status"))
        .and_then(Value::as_str)
        .map(String::from);
    if let Some(g) = &group_id {
        remember(
            ctx,
            g,
            name,
            kind.api_type(),
            item_id.as_deref(),
            consent_url.as_deref(),
        );
    }
    let next_action = match (&consent_url, kind.needs_consent()) {
        (Some(_), _) => "Open consent_url, finish HeyGen's consent step, then check status until it is completed.",
        (None, true) => "Complete HeyGen consent for this avatar group, then check status until it is completed.",
        (None, false) => "Check status until it is completed, then render with the avatar id.",
    };
    Ok(json!({
        "avatar_type": kind.api_type(),
        "avatar_item_id": item_id,
        "avatar_group_id": group_id,
        "status": status,
        "consent_status": consent_status,
        "consent_url": consent_url,
        "consent_error": consent_error,
        "uploaded_asset_ids": uploaded,
        "est_usd": null,
        "price_published": false,
        "cost_notice": PRICE_NOTICE,
        "next_action": next_action,
    }))
}

// ---- Status and list ----

fn status(ctx: &Context, input: &Value, base: &str) -> GenResult<Value> {
    let id = text(input, "avatar_group_id").ok_or_else(|| bad("avatar_group_id is required"))?;
    if !valid_id(id) {
        return Err(bad("avatar_group_id is not a valid id"));
    }
    let client = Client::new(base, key(ctx)?);
    let v = client.get(&format!("/v3/avatars/{id}"))?;
    let mut group = v.get("data").cloned().unwrap_or(v);
    let st = group
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let consent_url = recorded(ctx, id).and_then(|r| {
        r.get("consent_url")
            .and_then(Value::as_str)
            .map(String::from)
    });
    let (ready, next) = match st.as_str() {
        "completed" => (
            true,
            "Ready: use the avatar's look id as avatar_id when generating avatar video.",
        ),
        "pending_consent" => (
            false,
            "Waiting for provider consent: open consent_url and complete it.",
        ),
        "failed" => (false, "HeyGen failed to build this avatar; see error."),
        _ => (false, "Still processing; check again shortly."),
    };
    if let Some(o) = group.as_object_mut() {
        o.insert("ready".into(), json!(ready));
        o.insert("consent_url".into(), json!(consent_url));
        o.insert("next_action".into(), json!(next));
        let reviews = read_reviews(ctx);
        o.insert(
            "quality".into(),
            json!(quality_of(review_for(&reviews, "avatar_group_id", id))),
        );
    }
    Ok(group)
}

fn list(ctx: &Context, input: &Value, base: &str) -> GenResult<Value> {
    let ownership = text(input, "ownership").unwrap_or("private");
    if ownership != "private" && ownership != "public" {
        return Err(bad("ownership must be private or public"));
    }
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 50);
    let mut q = format!("/v3/avatars?ownership={ownership}&limit={limit}");
    if let Some(t) = text(input, "token") {
        if !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_=.".contains(c))
            || t.len() > 512
        {
            return Err(bad("token is not a valid pagination token"));
        }
        q.push_str(&format!("&token={t}"));
    }
    let client = Client::new(base, key(ctx)?);
    let v = client.get(&q)?;
    let mut avatars = v
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let registry = read_registry(ctx);
    let reviews = read_reviews(ctx);
    for a in &mut avatars {
        let rec = a
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| registry.iter().find(|r| r["avatar_group_id"] == id));
        let q = a
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| review_for(&reviews, "avatar_group_id", id));
        if let Some(o) = a.as_object_mut() {
            if let Some(rec) = rec {
                o.insert("consent_url".into(), rec["consent_url"].clone());
                o.insert("avatar_type".into(), rec["avatar_type"].clone());
            }
            o.insert("quality".into(), json!(quality_of(q)));
        }
    }
    Ok(json!({
        "avatars": avatars,
        "has_more": v.get("has_more").and_then(Value::as_bool).unwrap_or(false),
        "next_token": v.get("next_token").cloned().unwrap_or(Value::Null),
    }))
}

// ---- Local record of avatars created here (the consent URL is only returned once) ----

fn registry_path(ctx: &Context) -> std::path::PathBuf {
    ctx.data_dir.join(REGISTRY_FILE)
}

fn read_registry(ctx: &Context) -> Vec<Value> {
    std::fs::read(registry_path(ctx))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
        .unwrap_or_default()
}

fn recorded(ctx: &Context, group_id: &str) -> Option<Value> {
    read_registry(ctx)
        .into_iter()
        .find(|r| r["avatar_group_id"] == group_id)
}

/// Best effort: a failed write must not turn a created avatar into an error.
fn remember(
    ctx: &Context,
    group_id: &str,
    name: &str,
    avatar_type: &str,
    item_id: Option<&str>,
    consent_url: Option<&str>,
) {
    let mut all = read_registry(ctx);
    all.retain(|r| r["avatar_group_id"] != group_id);
    all.push(json!({
        "avatar_group_id": group_id,
        "avatar_item_id": item_id,
        "name": name,
        "avatar_type": avatar_type,
        "consent_url": consent_url,
        "created_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    }));
    let path = registry_path(ctx);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = rightkit_fs::atomic_write(&path, &serde_json::to_vec_pretty(&all).unwrap_or_default());
}

// ---- Generation routes: local / API / rented --------------------------------

use crate::providers::{self, Kind as GenKind};

/// Operator quality verdicts persisted per created avatar or rendered output.
/// `accepted` is a human decision; anything else stays visibly below it.
const REVIEW_FILE: &str = "avatars/reviewed.json";
const MODEL_ROUTES_KEY: &str = "models.routes";
const SOULX_MODEL_ID: &str = "soulx-flashhead";

fn quality_of(verdict: Option<&Value>) -> &'static str {
    match verdict.and_then(|v| v["verdict"].as_str()) {
        Some("accepted") => "accepted",
        Some("unqualified") => "unqualified",
        _ => "unreviewed",
    }
}

fn read_reviews(ctx: &Context) -> Vec<Value> {
    std::fs::read(ctx.data_dir.join(REVIEW_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
        .unwrap_or_default()
}

fn review_for<'a>(reviews: &'a [Value], key: &str, id: &str) -> Option<&'a Value> {
    reviews
        .iter()
        .find(|r| r["subject"].as_str() == Some(id) || r[key].as_str() == Some(id))
}

/// The persisted `models.routes` selection for one capability (`avatar`).
fn selected_route(ctx: &Context, capability: &str) -> GenResult<Value> {
    let routes = store::setting_json(ctx, MODEL_ROUTES_KEY, json!({}))?;
    Ok(routes.get(capability).cloned().unwrap_or(Value::Null))
}

/// `studio.avatars.routes` `{capability?: "avatar"|"lip_sync"}` → every route
/// that can produce the capability: API provider models from the provider
/// registry, local/rented model-manifest entries (consumed read-only from
/// `studio.models.catalog`), and staged rented workers (`studio.models.workers`).
/// Each route carries `ready` and a `quality` state so callers can keep
/// unqualified output visually distinct.
fn routes(ctx: &Context, input: &Value) -> GenResult<Value> {
    let capability = text(input, "capability").unwrap_or("avatar");
    let (gen_kind, label): (GenKind, &str) = match capability {
        "avatar" | "talking_head" => (GenKind::Avatar, "avatar"),
        "lip_sync" | "lipsync" => (GenKind::Lipsync, "lip_sync"),
        other => {
            return Err(bad(format!(
                "capability must be avatar or lip_sync, got {other}"
            )))
        }
    };
    let mut api = Vec::new();
    let mut local = Vec::new();
    for p in providers::registry() {
        let models: Vec<&providers::ModelInfo> =
            p.models().iter().filter(|m| m.kind == gen_kind).collect();
        if models.is_empty() {
            continue;
        }
        if !p.needs_key() {
            // Bundled local engine (e.g. local-wav2lip): keyless, free; ready
            // comes from the runtime probe (model files, ORT, ffmpeg).
            let ready = matches!(p.runtime_ready(), Some(Ok(())));
            for m in models {
                local.push(json!({
                    "route": "local",
                    "id": m.model,
                    "provider": p.id(),
                    "model": m.model,
                    "label": m.label,
                    "engine": "local",
                    "est_usd": m.est_usd,
                    "ready": ready,
                    "quality": "unreviewed",
                }));
            }
            continue;
        }
        let configured = cred_status(ctx, p.id()).is_some();
        for m in models {
            api.push(json!({
                "route": "api",
                "provider": p.id(),
                "model": m.model,
                "label": m.label,
                "est_usd": m.est_usd,
                "credential": if configured { "configured" } else { "unconfigured" },
                "ready": configured,
                "quality": "unreviewed",
            }));
        }
    }
    let mut rented = Vec::new();
    if let Ok(catalog) = models_handler(ctx, "studio.models.catalog", &json!({})) {
        for e in catalog
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if e["modality"].as_str() != Some("avatar") {
                continue;
            }
            let engine = e["engine"].as_str().unwrap_or("");
            let unqualified = e["measured"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .any(|x| x["notes"].as_str().unwrap_or("").contains("not qualified"))
                })
                .unwrap_or(false);
            let item = json!({
                "id": e["id"],
                "label": e["label"],
                "engine": engine,
                "engine_kind": e["engine_kind"],
                "capabilities": e["capabilities"],
                "requirements": e["requirements"],
                "fit": e["fit"],
                "readiness": e["readiness"],
                "measured": e["measured"],
                "quality": if unqualified { "unqualified" } else { "unreviewed" },
            });
            match engine {
                "rented" => rented.push(item),
                _ => local.push(item),
            }
        }
    }
    let workers = models_handler(ctx, "studio.models.workers", &json!({}))
        .ok()
        .and_then(|w| w.get("workers").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|w| {
            let hay =
                format!("{} {}", w["worker_id"].as_str().unwrap_or(""), w["spec"]).to_lowercase();
            hay.contains("soulx") || hay.contains("flashhead") || hay.contains("avatar")
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "capability": label,
        "selected": selected_route(ctx, "avatar")?,
        "valid_routes": ["auto", "local", "api", "rented"],
        "routes": { "api": api, "local": local, "rented": rented },
        "rented_workers": workers,
        "worker_contract": "studio.models.worker.action {action: register|stage|reconnect|begin|retrieve|teardown}",
    }))
}

/// Resolve `provider`/`model` (either may be absent) against avatar/lip-sync
/// models in the provider registry.
fn resolve_model(input: &Value, kind: GenKind) -> GenResult<(String, String)> {
    let want_provider = text(input, "provider");
    let want_model = text(input, "model");
    for p in providers::registry() {
        if let Some(w) = want_provider {
            if p.id() != w {
                continue;
            }
        }
        for m in p.models() {
            if m.kind != kind {
                continue;
            }
            if want_model.is_none_or(|w| w == m.model) {
                return Ok((p.id().to_string(), m.model.to_string()));
            }
        }
    }
    let word = match kind {
        GenKind::Avatar => "avatar",
        _ => "lip_sync",
    };
    match (want_provider, want_model) {
        (Some(pv), Some(mo)) => Err(bad(format!("{pv} has no {word} model {mo}"))),
        (Some(pv), None) => Err(bad(format!("{pv} has no {word} models"))),
        (None, Some(mo)) => Err(bad(format!("no provider has a {word} model {mo}"))),
        _ => Err(bad(format!("no {word} model selected"))),
    }
}

/// Look up a HeyGen look id from a locally recorded avatar group.
fn look_for_group(ctx: &Context, group_id: &str) -> Option<String> {
    recorded(ctx, group_id)
        .and_then(|r| {
            r.get("avatar_item_id")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .filter(|s| valid_id(s))
}

/// Rented (SoulX) answer for a prepared run: never submits, names the worker
/// contract and any staged workers, and marks the output unqualified.
fn rented_answer(ctx: &Context, mode: &str) -> GenResult<Value> {
    let routes = routes(ctx, &json!({"capability": "avatar"}))?;
    let workers = routes["rented_workers"].clone();
    let ready_worker = workers
        .as_array()
        .and_then(|w| {
            w.iter().find(|w| {
                matches!(
                    w["state"].as_str(),
                    Some("staged") | Some("ready") | Some("running")
                )
            })
        })
        .cloned();
    Ok(json!({
        "mode": mode,
        "route": "rented",
        "model": SOULX_MODEL_ID,
        "engine_kind": "experimental",
        "quality": "unqualified",
        "worker_required": true,
        "worker": ready_worker,
        "submit_via": "studio.models.worker.action",
        "est_usd": null,
        "note": "SoulX-FlashHead is a rented/remote-worker experiment whose prior output was rejected (portrait crop, mouth/head motion); it is not quality-accepted. Stage or reconnect a worker via studio.models.worker.action, then run there — paid execution stays gated on an authorized target and budget.",
    }))
}

fn https_url<'a>(input: &'a Value, key: &str) -> GenResult<Option<&'a str>> {
    match text(input, key) {
        Some(u) if u.to_ascii_lowercase().starts_with("https://") => Ok(Some(u)),
        Some(_) => Err(bad(format!("{key} must be an https URL"))),
        None => Ok(None),
    }
}

/// Build the `params`/`prompt` pair for one avatar-family request and estimate
/// it with the provider adapter (free; no network).
fn prepared(ctx: &Context, input: &Value, kind: GenKind, mode: &str) -> GenResult<Value> {
    let (provider, model) = if text(input, "provider").is_none() && text(input, "model").is_none() {
        // No explicit pick: honor the persisted route selection, else the
        // first configured API model of this kind, else the first registered.
        let sel = selected_route(ctx, "avatar")?;
        let sel_model = sel["model"].as_str();
        if sel["route"].as_str() == Some("rented") || sel_model == Some(SOULX_MODEL_ID) {
            return rented_answer(ctx, mode);
        }
        if let Some(m) = sel_model {
            let mut with = input.clone();
            with["model"] = json!(m);
            resolve_model(&with, kind)?
        } else {
            let first = providers::registry()
                .iter()
                .flat_map(|p| p.models().iter().map(move |m| (p.id(), m)))
                .find(|(pid, m)| m.kind == kind && cred_status(ctx, pid).is_some())
                .or_else(|| {
                    providers::registry()
                        .iter()
                        .flat_map(|p| p.models().iter().map(move |m| (p.id(), m)))
                        .find(|(_, m)| m.kind == kind)
                });
            match first {
                Some((pid, m)) => (pid.to_string(), m.model.to_string()),
                None => return Err(bad(format!("no registered {mode} model"))),
            }
        }
    } else {
        resolve_model(input, kind)?
    };
    if model == SOULX_MODEL_ID {
        return rented_answer(ctx, mode);
    }
    // Keyless bundled engine (local-wav2lip): local files in, MP4 out. No
    // URL inputs; readiness comes from the runtime probe, not credentials.
    if let Some(p) = providers::registry()
        .into_iter()
        .find(|p| p.id() == provider && !p.needs_key())
    {
        let image = text(input, "image_path")
            .ok_or_else(|| bad(format!("{model} needs a local image_path")))?;
        let audio = text(input, "audio_path")
            .ok_or_else(|| bad(format!("{model} needs a local audio_path")))?;
        for (k, v) in [("image_path", image), ("audio_path", audio)] {
            if !Path::new(v).is_file() {
                return Err(bad(format!("{k} not found: {v}")));
            }
        }
        let mut params = Map::new();
        params.insert("image_path".into(), json!(image));
        params.insert("audio_path".into(), json!(audio));
        for k in ["face_size", "face_cx", "face_cy", "fps"] {
            if let Some(v) = input.get(k) {
                params.insert(k.into(), v.clone());
            }
        }
        let ready = matches!(p.runtime_ready(), Some(Ok(())));
        let mut warnings = Vec::new();
        if !ready {
            warnings.push("local engine not installed: Wav2Lip model, ONNX Runtime, and ffmpeg must all be present".to_string());
        }
        let params = Value::Object(params);
        let est = p
            .estimate_usd(&providers::GenRequest {
                kind,
                provider: provider.clone(),
                model: model.clone(),
                prompt: String::new(),
                params: params.clone(),
            })
            .ok();
        return Ok(json!({
            "mode": mode,
            "route": "local",
            "request": {
                "kind": if kind == GenKind::Avatar { "avatar" } else { "lipsync" },
                "provider": provider,
                "model": model,
                "prompt": "",
                "params": params,
            },
            "est_usd": est,
            "ready": ready,
            "quality": "unreviewed",
            "warnings": warnings,
            "submit_via": "media.generate",
            "note": "Prepared only — nothing was sent. Local Wav2Lip runs fully in-process (no network) when submitted.",
        }));
    }

    let mut params = Map::new();
    let mut warnings = Vec::new();
    let prompt = text(input, "script")
        .or_else(|| text(input, "prompt"))
        .unwrap_or("")
        .to_string();
    let audio_url = https_url(input, "audio_url")?;
    match kind {
        GenKind::Avatar => {
            if provider == "heygen" {
                let avatar = text(input, "avatar_id")
                    .map(String::from)
                    .or_else(|| text(input, "avatar_group_id").and_then(|g| look_for_group(ctx, g)))
                    .ok_or_else(|| {
                        bad("avatar_id (HeyGen look id) or avatar_group_id is required")
                    })?;
                if !valid_id(&avatar) {
                    return Err(bad("avatar_id is not a valid id"));
                }
                params.insert("avatar_id".into(), json!(avatar));
                if audio_url.is_some() && !prompt.is_empty() {
                    return Err(bad("use a script or audio_url, not both"));
                }
                if audio_url.is_none() && prompt.is_empty() {
                    return Err(bad("a script or audio_url is required"));
                }
                if let Some(u) = audio_url {
                    params.insert("audio_url".into(), json!(u));
                } else if let Some(v) = text(input, "voice_id") {
                    if !valid_id(v) {
                        return Err(bad("voice_id is not a valid id"));
                    }
                    params.insert("voice_id".into(), json!(v));
                }
                if let Some(a) = text(input, "aspect_ratio") {
                    if !ASPECTS.contains(&a) {
                        return Err(bad(format!(
                            "aspect_ratio must be one of {}",
                            ASPECTS.join(", ")
                        )));
                    }
                    params.insert("aspect_ratio".into(), json!(a));
                }
                if let Some(r) = text(input, "resolution") {
                    params.insert("resolution".into(), json!(r));
                }
                if input.get("remove_background").and_then(Value::as_bool) == Some(true)
                    || text(input, "background").is_some_and(|b| {
                        b.eq_ignore_ascii_case("transparent") || b.eq_ignore_ascii_case("remove")
                    })
                {
                    params.insert("background".into(), json!("transparent"));
                }
            } else {
                // Still-image talking head (WaveSpeed InfiniteTalk and kin).
                let image = https_url(input, "image_url")?
                    .ok_or_else(|| bad(format!("{model} needs a portrait image_url (https)")))?;
                let audio =
                    audio_url.ok_or_else(|| bad(format!("{model} needs an audio_url (https)")))?;
                params.insert("image_url".into(), json!(image));
                params.insert("audio_url".into(), json!(audio));
                if let Some(r) = text(input, "resolution") {
                    if !["480p", "720p"].contains(&r) {
                        return Err(bad("resolution must be 480p or 720p"));
                    }
                    params.insert("resolution".into(), json!(r));
                }
                if let Some(m) = https_url(input, "mask_image")? {
                    params.insert("mask_image".into(), json!(m));
                }
                if let Some(s) = input.get("seed").and_then(Value::as_i64) {
                    params.insert("seed".into(), json!(s));
                }
            }
        }
        GenKind::Lipsync => {
            let video = https_url(input, "video_url")?
                .ok_or_else(|| bad("video lip-sync needs a video_url (https)"))?;
            let audio =
                audio_url.ok_or_else(|| bad("video lip-sync needs an audio_url (https)"))?;
            params.insert("video_url".into(), json!(video));
            params.insert("audio_url".into(), json!(audio));
            if let Some(s) = text(input, "sync_mode") {
                params.insert("sync_mode".into(), json!(s));
            }
        }
        _ => return Err(bad("unsupported avatar mode")),
    }
    if text(input, "duration_secs")
        .and_then(|d| d.parse::<f64>().ok())
        .is_some()
    {
        params.insert("duration_secs".into(), json!(text(input, "duration_secs")));
    }

    let params = Value::Object(params);
    let est = providers::registry()
        .into_iter()
        .find(|p| p.id() == provider)
        .and_then(|p| {
            p.estimate_usd(&providers::GenRequest {
                kind,
                provider: provider.clone(),
                model: model.clone(),
                prompt: prompt.clone(),
                params: params.clone(),
            })
            .ok()
        });
    let configured = cred_status(ctx, &provider).is_some();
    if !configured {
        warnings.push(format!(
            "no {provider} API key configured; add one under Models & Keys before submitting"
        ));
    }
    Ok(json!({
        "mode": mode,
        "route": "api",
        "request": {
            "kind": if kind == GenKind::Avatar { "avatar" } else { "lipsync" },
            "provider": provider,
            "model": model,
            "prompt": prompt,
            "params": params,
        },
        "est_usd": est,
        "ready": configured,
        "quality": "unreviewed",
        "warnings": warnings,
        "submit_via": "media.generate",
        "note": "Prepared only — nothing was sent. Submit `request` via media.generate to spend.",
    }))
}

/// `studio.avatars.animate` — talking-avatar video from a script or an audio
/// track (avatar_id for HeyGen, image_url + audio_url for still-image models).
fn animate(ctx: &Context, input: &Value) -> GenResult<Value> {
    prepared(ctx, input, GenKind::Avatar, "animate")
}

/// `studio.avatars.lip_sync` — video+audio re-times the lips of an existing
/// video (`video_url`, lip-sync models); a portrait+audio request is an
/// `animate` talking-head run instead (`image_url` + `audio_url`).
fn lip_sync(ctx: &Context, input: &Value) -> GenResult<Value> {
    if text(input, "video_url").is_some() {
        prepared(ctx, input, GenKind::Lipsync, "lip_sync")
    } else {
        let mut out = prepared(ctx, input, GenKind::Avatar, "lip_sync")?;
        if out["route"].as_str() == Some("api")
            && out["request"]["params"].get("audio_url").is_none()
        {
            return Err(bad(
                "lip_sync needs an audio_url (https); for video input also pass video_url",
            ));
        }
        out["mode"] = json!("lip_sync");
        Ok(out)
    }
}

/// `studio.avatars.review` `{avatar_group_id?|avatar_item_id?|path?|asset_id?,
/// verdict: "accept"|"reject"|"unqualified"|"unreviewed", note?}` — persist the
/// operator's quality verdict. `list`/`status` merge it back as `quality`.
fn review(ctx: &Context, input: &Value) -> GenResult<Value> {
    let verdict = match text(input, "verdict") {
        Some("accept") | Some("accepted") => "accepted",
        Some("reject") | Some("unqualified") => "unqualified",
        Some("unreviewed") => "unreviewed",
        _ => {
            return Err(bad(
                "verdict must be accept, reject, unqualified, or unreviewed",
            ))
        }
    };
    let (subject_key, subject) = ["avatar_group_id", "avatar_item_id", "asset_id", "path"]
        .iter()
        .find_map(|k| text(input, k).map(|v| (*k, v.to_string())))
        .ok_or_else(|| {
            bad("a subject (avatar_group_id, avatar_item_id, asset_id, or path) is required")
        })?;
    let mut all = read_reviews(ctx);
    all.retain(|r| r["subject"].as_str() != Some(subject.as_str()));
    all.push(json!({
        "subject": subject,
        "subject_kind": subject_key,
        "verdict": verdict,
        "note": text(input, "note").map(|n| n.chars().take(500).collect::<String>()),
        "at": store::now(),
    }));
    let path = ctx.data_dir.join(REVIEW_FILE);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rightkit_fs::atomic_write(&path, &serde_json::to_vec_pretty(&all).unwrap_or_default())
        .map_err(|e| GenError::new("io_error", e.to_string()))?;
    Ok(json!({"subject": subject, "quality": verdict, "reviews": all.len()}))
}
