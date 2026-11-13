//! studio.voices: the voice library ported from VoiceRight (494ab38) and
//! extended with provider voices.
//!
//! Commands (all `studio.voices.<verb>`):
//! - `list` `{provider?, offline?}`: ElevenLabs voices (live `GET /v1/voices`,
//!   free), WaveSpeed defaults, and local voice records
//!   (`<data>/voices/local/*.json`, written by the QTS lane or by import).
//! - `set_default` `{provider, voice_id, model?}` / `delete` `{id, source?}`.
//! - `import_voiceright` `{source_dir?}`: copy VoiceRight's `<id>.pb` voice
//!   prompts into `<data>/voices/local/` as `verified:false` records.
//! - `verify` `{id}`: the synth-check (§4.7); runs one short synthesis through
//!   the local-qts provider (or a registered [`register_synth_check`] hook);
//!   `verify_pending` only while the model files are not installed.
//! - `batch` `{lines|text, provider, model, voice_id?, output_dir?, dry_run?}`
//!   and `batch_status` `{batch_id, resume?}`: one generated file per
//!   non-empty line, each a `voice` job with its own `request_id`.
//! - `tier` and `clone_elevenlabs`: account tier and ElevenLabs instant voice
//!   cloning (consent required, plan invariant 7).
//! - `export` `{path|batch_id, format?, normalize?, zip?}`: wav/flac/m4a (AAC)
//!   conversion via ffmpeg, optional EBU R128 loudness normalisation, batch
//!   folder or zip with manifest.
//!
//! Local QTS x-vector cloning and synthesis belong to the QTS lane; it plugs
//! in through [`register_synth_check`] and [`register_local_clone`].

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::providers::qts::qts_models as models;
use crate::providers::{self, qts, GenRequest, Kind, Output, Provider, SubmitOutcome};
use crate::{api, credentials, jobs, library, store};

use crate::{Context, GenError, GenResult};

/// How the voice library reaches the host's local model installer (the QTS weights and ONNX
/// Runtime). Register once with [`set_install_hooks`]; without them the install view reads idle
/// and `install` answers `not_configured`.
#[derive(Clone, Copy)]
pub struct InstallHooks {
    /// Live progress of a running install of a model id, if any.
    pub live_progress: fn(&str) -> Option<Value>,
    /// The persisted install record of a model id (`{state, events, updated_at}`).
    pub install_record: fn(&Context, &str) -> Value,
    /// Whether this platform can run the voice engine's ONNX Runtime.
    pub ort_supported: fn() -> bool,
    /// Start the background voice-model install.
    pub start_install: fn(&Context) -> GenResult<Value>,
}

impl std::fmt::Debug for InstallHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallHooks").finish_non_exhaustive()
    }
}

fn no_progress(_: &str) -> Option<Value> {
    None
}

fn no_record(_: &Context, _: &str) -> Value {
    Value::Null
}

fn no_ort() -> bool {
    false
}

fn no_install(_: &Context) -> GenResult<Value> {
    Err(GenError::new(
        "not_configured",
        "no local model installer is registered",
    ))
}

const NO_HOOKS: InstallHooks = InstallHooks {
    live_progress: no_progress,
    install_record: no_record,
    ort_supported: no_ort,
    start_install: no_install,
};

static INSTALL_HOOKS: std::sync::OnceLock<InstallHooks> = std::sync::OnceLock::new();

/// Register the host's installer hooks once. Returns the rejected hooks when already set.
pub fn set_install_hooks(hooks: InstallHooks) -> Result<(), InstallHooks> {
    INSTALL_HOOKS.set(hooks)
}

fn install_hooks() -> &'static InstallHooks {
    INSTALL_HOOKS.get().unwrap_or(&NO_HOOKS)
}
use crate::host::http::{Http, HttpError, Multipart, Request, Response};

pub const COMMANDS: &[&str] = &[
    "studio.voices.list",
    "studio.voices.set_default",
    "studio.voices.delete",
    "studio.voices.rename",
    "studio.voices.import_voiceright",
    "studio.voices.verify",
    "studio.voices.preview",
    "studio.voices.presets",
    "studio.voices.design",
    "studio.voices.batch",
    "studio.voices.batch_status",
    "studio.voices.clone_elevenlabs",
    "studio.voices.clone_local",
    "studio.voices.tier",
    "studio.voices.models_status",
    "studio.voices.install",
    "studio.voices.install_status",
    "studio.voices.export",
];

const ELEVENLABS: &str = "elevenlabs";
const BUILTIN: &str = include_str!("../data/voices/builtin.json");
const MAX_BATCH_LINES: usize = 500;
const MAX_CLONE_BYTES: u64 = 10 * 1024 * 1024;
const CLONE_EXTS: &[&str] = &["wav", "mp3", "m4a", "flac", "ogg"];

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    let base_owned =
        providers::base_url_override("RIGHTKIT_ELEVENLABS_BASE", providers::elevenlabs::BASE);
    let base = base_owned.as_str();
    match name {
        "studio.voices.list" => list_with(ctx, base, input),
        "studio.voices.set_default" => set_default(ctx, input),
        "studio.voices.delete" => delete_with(ctx, base, input),
        "studio.voices.rename" => rename(ctx, input),
        "studio.voices.import_voiceright" => import_voiceright(ctx, input),
        "studio.voices.verify" => verify(ctx, input),
        "studio.voices.preview" => preview(ctx, input),
        "studio.voices.presets" => presets(),
        "studio.voices.design" => design(ctx, input),
        "studio.voices.batch" => {
            batch_with(ctx, &providers::registry(), input, RunMode::Background)
        }
        "studio.voices.batch_status" => batch_status(ctx, input),
        "studio.voices.clone_elevenlabs" => clone_with(ctx, base, input),
        "studio.voices.clone_local" => clone_local(ctx, input),
        "studio.voices.tier" => tier_with(ctx, base),
        "studio.voices.models_status" => models_status(ctx),
        "studio.voices.install" => install(ctx),
        "studio.voices.install_status" => install_status(ctx),
        "studio.voices.export" => export(ctx, input),
        other => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {other}"),
        )),
    }
}

// ---------------------------------------------------------------- hooks

/// Synth check for one local voice record (§4.7: "each imported voice
/// synthesizes"). The QTS lane registers it; `Ok` marks the voice verified.
pub type SynthCheck = fn(&Context, &Value) -> GenResult<()>;
/// Local QTS x-vector clone: `(ctx, name, reference audio)` -> new voice record.
pub type LocalClone = fn(&Context, &str, &Path) -> GenResult<Value>;

static SYNTH_CHECK: OnceLock<SynthCheck> = OnceLock::new();
static LOCAL_CLONE: OnceLock<LocalClone> = OnceLock::new();

pub fn register_synth_check(f: SynthCheck) -> bool {
    SYNTH_CHECK.set(f).is_ok()
}

pub fn register_local_clone(f: LocalClone) -> bool {
    LOCAL_CLONE.set(f).is_ok()
}

/// The registered local clone hook, for the QTS lane's own command.
pub fn local_clone_hook() -> Option<LocalClone> {
    LOCAL_CLONE.get().copied()
}

// ---------------------------------------------------------------- paths and files

fn voices_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("voices")
}
fn local_dir(ctx: &Context) -> PathBuf {
    voices_dir(ctx).join("local")
}
fn batches_dir(ctx: &Context) -> PathBuf {
    voices_dir(ctx).join("batches")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Lowercase an arbitrary file stem into a `valid_id` slug; empty if nothing
/// usable remains.
fn slug_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut dash = true; // trim leading separators too
    for c in raw.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.truncate(128);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn bad(msg: impl Into<String>) -> GenError {
    GenError::new("invalid_input", msg)
}

fn io(e: std::io::Error) -> GenError {
    store::io_err(e)
}

/// A random id from the owner (`rightkit-logs` `identity::random_uuid`, OS random source), hyphens dropped.
fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        rightkit_logs::identity::random_uuid().replace('-', "")
    )
}

fn write_json(path: &Path, v: &Value) -> GenResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    rightkit_fs::atomic_write(path, &serde_json::to_vec_pretty(v).unwrap_or_default())
        .map_err(|e| GenError::new("io", e.to_string()))
}

/// Bytes published atomically through the filesystem owner (no half-written voice file after a crash).
fn atomic_bytes(path: &Path, bytes: &[u8]) -> GenResult<()> {
    rightkit_fs::atomic_write(path, bytes).map_err(|e| GenError::new("io", e.to_string()))
}

/// Verified atomic copy through the filesystem owner; returns the byte count.
fn copy_export(src: &Path, dest: &Path) -> GenResult<u64> {
    rightkit_fs::copy_verified(src, dest, rightkit_fs::AtomicWriteOptions::default(), None)
        .map(|copied| copied.bytes)
        .map_err(|e| GenError::new("io", e.to_string()))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn builtin() -> Value {
    serde_json::from_str(BUILTIN).unwrap_or(Value::Null)
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- defaults

fn defaults_path(ctx: &Context) -> PathBuf {
    voices_dir(ctx).join("defaults.json")
}

fn default_key(provider: &str, model: Option<&str>) -> String {
    match model {
        Some(m) if provider == "wavespeed" => format!("{provider}/{m}"),
        _ => provider.to_string(),
    }
}

fn stored_defaults(ctx: &Context) -> Map<String, Value> {
    read_json(&defaults_path(ctx))
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// `set_default` `{provider, voice_id, model?}`; `provider` is `elevenlabs`,
/// `wavespeed` (with `model`) or `local`.
fn set_default(ctx: &Context, input: &Value) -> GenResult<Value> {
    let provider = str_of(input, "provider").ok_or_else(|| bad("missing field: provider"))?;
    let voice = str_of(input, "voice_id").ok_or_else(|| bad("missing field: voice_id"))?;
    if !["elevenlabs", "wavespeed", "local"].contains(&provider) {
        return Err(bad(format!("unknown provider: {provider}")));
    }
    if !valid_id(voice) {
        return Err(bad("voice_id has invalid characters"));
    }
    let model = str_of(input, "model");
    if provider == "wavespeed" && model.is_none() {
        return Err(bad(
            "wavespeed defaults are per model: missing field: model",
        ));
    }
    if provider == "local" && !local_dir(ctx).join(format!("{voice}.json")).is_file() {
        return Err(GenError::new(
            "voice_not_found",
            format!("no local voice {voice}"),
        ));
    }
    let mut d = stored_defaults(ctx);
    let key = default_key(provider, model);
    d.insert(key.clone(), json!(voice));
    write_json(&defaults_path(ctx), &Value::Object(d))?;
    Ok(json!({ "provider": provider, "model": model, "voice_id": voice, "key": key }))
}

// ---------------------------------------------------------------- local voices

fn read_local(ctx: &Context) -> (Vec<Value>, Vec<Value>) {
    let mut voices = Vec::new();
    let mut warnings = Vec::new();
    let Ok(rd) = std::fs::read_dir(local_dir(ctx)) else {
        return (voices, warnings);
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for p in paths {
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let Some(rec) = read_json(&p).filter(Value::is_object) else {
            warnings.push(json!({ "code": "bad_local_record", "message": format!("{stem}.json is not a voice record; skipped") }));
            continue;
        };
        let id = str_of(&rec, "id").map(String::from).unwrap_or(stem);
        if !valid_id(&id) {
            warnings.push(json!({ "code": "bad_local_record", "message": format!("{id}: invalid id; skipped") }));
            continue;
        }
        let kind = str_of(&rec, "kind").unwrap_or("clone").to_string();
        let prompt_present = str_of(&rec, "prompt_file")
            .map(|f| !f.contains(['/', '\\']) && local_dir(ctx).join(f).is_file())
            .unwrap_or(false);
        voices.push(json!({
            "id": id,
            "name": str_of(&rec, "name").unwrap_or(&id),
            "provider": "local",
            "source": "local",
            "category": kind,
            "kind": kind,
            "deletable": kind != "base",
            "verified": rec.get("verified").and_then(Value::as_bool).unwrap_or(false),
            "prompt_present": prompt_present,
            "provenance": rec.get("provenance").cloned().unwrap_or(Value::Null),
            "created_at": rec.get("created_at").cloned().unwrap_or(Value::Null),
        }));
    }
    (voices, warnings)
}

// ---------------------------------------------------------------- ElevenLabs client

struct El {
    base: String,
    http: Http,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl El {
    fn new(base: &str) -> Self {
        let http = Http::new(Duration::from_secs(10), Duration::from_secs(120));
        Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// Send once with the 32 MiB reply cap; non-2xx replies are returned, not raised.
    fn exchange(&self, req: Request, key: &str) -> GenResult<Resp> {
        Self::finish(self.http.send(req.max_response_bytes(32 << 20)), key)
    }

    fn finish(r: Result<Response, HttpError>, key: &str) -> GenResult<Resp> {
        // The body is read inside `send`, so a failed body read is a transport error with no status.
        let resp = r.map_err(|e| {
            GenError::new(
                "transport",
                redact(&format!("elevenlabs transport error: {e}"), key),
            )
        })?;
        Ok(Resp {
            status: resp.status,
            body: resp.body,
        })
    }

    fn get(&self, path: &str, key: &str) -> GenResult<Resp> {
        self.exchange(
            Request::get(format!("{}{path}", self.base)).secret_header("xi-api-key", key),
            key,
        )
    }

    fn delete(&self, path: &str, key: &str) -> GenResult<Resp> {
        self.exchange(
            Request::delete(format!("{}{path}", self.base)).secret_header("xi-api-key", key),
            key,
        )
    }

    fn post_multipart(&self, path: &str, key: &str, form: Multipart) -> GenResult<Resp> {
        // A boundary that occurs inside a part is refused here as invalid input, before any request is sent.
        let req = Request::post(format!("{}{path}", self.base))
            .secret_header("xi-api-key", key)
            .multipart(form)
            .map_err(|e| bad(format!("elevenlabs upload framing: {e}")))?;
        self.exchange(req, key)
    }
}

fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn detail(body: &[u8], key: &str) -> String {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.get("detail"))
        .map(|d| match d {
            Value::String(s) => s.clone(),
            Value::Object(o) => o
                .get("message")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| d.to_string()),
            other => other.to_string(),
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    redact(&msg.chars().take(300).collect::<String>(), key)
}

fn status_error(r: &Resp, key: &str) -> GenError {
    let text = detail(&r.body, key);
    match r.status {
        401 | 403 => GenError::new(
            "auth",
            format!("elevenlabs rejected the request ({}): {text}", r.status),
        ),
        408 | 429 => GenError::new("rate_limited", format!("elevenlabs {}: {text}", r.status)),
        500..=599 => GenError::new("unavailable", format!("elevenlabs {}: {text}", r.status)),
        s => GenError::new("invalid_request", format!("elevenlabs {s}: {text}")),
    }
}

/// The stored ElevenLabs key. Tests ignore the process environment so a
/// developer's real key never leaks into them.
fn get_key(ctx: &Context) -> GenResult<Option<String>> {
    {
        credentials::get(ctx, ELEVENLABS)
    }
}

fn el_key(ctx: &Context) -> GenResult<String> {
    get_key(ctx)?.ok_or_else(|| GenError::new("no_credentials", "no API key stored for elevenlabs"))
}

// ---------------------------------------------------------------- list

fn effective_default(
    stored: &Map<String, Value>,
    key: &str,
    fallback: Option<&str>,
) -> Option<String> {
    stored
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| fallback.map(String::from))
}

fn list_with(ctx: &Context, el_base: &str, input: &Value) -> GenResult<Value> {
    let filter = str_of(input, "provider");
    let offline = input
        .get("offline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let want = |p: &str| filter.is_none_or(|f| f == p);
    let stored = stored_defaults(ctx);
    let data = builtin();
    let mut voices: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let mut defaults = Map::new();

    if want(ELEVENLABS) {
        let mut live: Option<Vec<Value>> = None;
        if !offline {
            match get_key(ctx) {
                Ok(Some(key)) => match El::new(el_base).get("/v1/voices", &key) {
                    Ok(r) if (200..300).contains(&r.status) => {
                        let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                        live = Some(
                            v.get("voices")
                                .and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(el_voice).collect())
                                .unwrap_or_default(),
                        );
                    }
                    Ok(r) => {
                        let e = status_error(&r, &key);
                        warnings.push(json!({ "code": e.code, "message": e.message, "provider": ELEVENLABS }));
                    }
                    Err(e) => warnings.push(json!({ "code": e.code, "message": e.message, "provider": ELEVENLABS })),
                },
                Ok(None) => warnings.push(json!({
                    "code": "no_credentials",
                    "message": "Add an ElevenLabs key in Settings to list your voices; showing the built-in premade voice.",
                    "provider": ELEVENLABS
                })),
                Err(e) => warnings.push(json!({ "code": e.code, "message": e.message, "provider": ELEVENLABS })),
            }
        }
        let items = live.unwrap_or_else(|| {
            data[ELEVENLABS]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|b| {
                            json!({
                                "id": b["id"], "name": b["name"], "provider": ELEVENLABS, "source": "builtin",
                                "category": b["category"], "description": b["description"],
                                "deletable": false, "free_plan_usable": true,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        let first = items
            .first()
            .and_then(|v| v["id"].as_str())
            .map(String::from);
        let builtin_default = items
            .iter()
            .find(|v| v["id"] == "JBFqnCBsd6RMkjVDRZzb")
            .and_then(|v| v["id"].as_str())
            .map(String::from)
            .or(first);
        let eff = effective_default(&stored, ELEVENLABS, builtin_default.as_deref())
            .filter(|d| items.iter().any(|v| v["id"] == d.as_str()));
        if let Some(d) = &eff {
            defaults.insert(ELEVENLABS.into(), json!(d));
        }
        for mut v in items {
            v["default"] = json!(eff.as_deref().is_some_and(|d| v["id"] == d));
            voices.push(v);
        }
    }

    if want("wavespeed") {
        let items = data["wavespeed"].as_array().cloned().unwrap_or_default();
        let mut models: Vec<String> = Vec::new();
        for b in &items {
            if let Some(m) = b["model"].as_str() {
                if !models.iter().any(|x| x == m) {
                    models.push(m.to_string());
                }
            }
        }
        for m in models {
            let in_model: Vec<&Value> = items.iter().filter(|b| b["model"] == m.as_str()).collect();
            let fallback = in_model.first().and_then(|b| b["id"].as_str());
            let key = default_key("wavespeed", Some(&m));
            let eff = effective_default(&stored, &key, fallback)
                .filter(|d| in_model.iter().any(|b| b["id"] == d.as_str()));
            if let Some(d) = &eff {
                defaults.insert(key, json!(d));
            }
            for b in in_model {
                voices.push(json!({
                    "id": b["id"], "name": b["name"], "provider": "wavespeed", "model": m, "source": "builtin",
                    "category": "default", "description": b["description"], "deletable": false,
                    "default": eff.as_deref().is_some_and(|d| b["id"] == d),
                }));
            }
        }
    }

    if want("local") {
        let (local, w) = read_local(ctx);
        warnings.extend(w);
        let eff = stored
            .get("local")
            .and_then(Value::as_str)
            .filter(|d| local.iter().any(|v| v["id"] == *d))
            .map(String::from);
        if let Some(d) = &eff {
            defaults.insert("local".into(), json!(d));
        }
        for mut v in local {
            v["default"] = json!(eff.as_deref().is_some_and(|d| v["id"] == d));
            voices.push(v);
        }
    }

    Ok(json!({ "voices": voices, "defaults": defaults, "warnings": warnings }))
}

fn el_voice(v: &Value) -> Option<Value> {
    let id = str_of(v, "voice_id")?;
    let category = str_of(v, "category").unwrap_or("unknown");
    Some(json!({
        "id": id,
        "name": str_of(v, "name").unwrap_or(id),
        "provider": ELEVENLABS,
        "source": "api",
        "category": category,
        "description": v.get("description").cloned().unwrap_or(Value::Null),
        "labels": v.get("labels").cloned().unwrap_or(Value::Null),
        "preview_url": v.get("preview_url").cloned().unwrap_or(Value::Null),
        "deletable": category == "cloned",
        // Free-plan keys cannot use library voices through the API (adapter note).
        "free_plan_usable": matches!(category, "premade" | "cloned" | "generated"),
    }))
}

// ---------------------------------------------------------------- delete

/// `delete` `{id, source?}`: a local clone/imported voice (record + prompt) or an
/// ElevenLabs cloned voice (`source:"elevenlabs"`, refused for premade/library).
fn delete_with(ctx: &Context, el_base: &str, input: &Value) -> GenResult<Value> {
    let id = str_of(input, "id").ok_or_else(|| bad("missing field: id"))?;
    if !valid_id(id) {
        return Err(bad("id has invalid characters"));
    }
    let source = str_of(input, "source").unwrap_or("local");
    match source {
        "local" => {
            let rec_path = local_dir(ctx).join(format!("{id}.json"));
            let rec = read_json(&rec_path)
                .ok_or_else(|| GenError::new("voice_not_found", format!("no local voice {id}")))?;
            if str_of(&rec, "kind") == Some("base") {
                return Err(bad("the base voice cannot be deleted"));
            }
            if let Some(f) = str_of(&rec, "prompt_file").filter(|f| !f.contains(['/', '\\'])) {
                match std::fs::remove_file(local_dir(ctx).join(f)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(e)),
                }
            }
            std::fs::remove_file(&rec_path).map_err(io)?;
            clear_default(ctx, "local", id)?;
            Ok(json!({ "deleted": id, "source": "local" }))
        }
        "elevenlabs" => {
            let key = el_key(ctx)?;
            let el = El::new(el_base);
            let info = el.get(&format!("/v1/voices/{id}"), &key)?;
            if !(200..300).contains(&info.status) {
                return Err(status_error(&info, &key));
            }
            let v: Value = serde_json::from_slice(&info.body).unwrap_or(Value::Null);
            if str_of(&v, "category") != Some("cloned") {
                return Err(GenError::new(
                    "not_a_clone",
                    "only cloned voices can be deleted; premade and library voices stay",
                ));
            }
            let r = el.delete(&format!("/v1/voices/{id}"), &key)?;
            if !(200..300).contains(&r.status) {
                return Err(status_error(&r, &key));
            }
            clear_default(ctx, ELEVENLABS, id)?;
            Ok(json!({ "deleted": id, "source": "elevenlabs" }))
        }
        other => Err(bad(format!("unknown source: {other}"))),
    }
}

fn clear_default(ctx: &Context, provider: &str, id: &str) -> GenResult<()> {
    let mut d = stored_defaults(ctx);
    if d.get(provider).and_then(Value::as_str) == Some(id) {
        d.remove(provider);
        write_json(&defaults_path(ctx), &Value::Object(d))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- VoiceRight import

/// Where VoiceRight (`com.rightsuite.voiceright`) keeps cloned voices:
/// `<app-data>/engine/voices/<id>.pb` + `<id>.name` (its docs/05, engine/mod.rs).
/// App data is `~/Library/Application Support` on macOS and `%APPDATA%` on
/// Windows (Tauri `app_data_dir`).
fn voiceright_default_dirs() -> Vec<PathBuf> {
    let rel = Path::new("com.rightsuite.voiceright")
        .join("engine")
        .join("voices");
    // Flat `<app-data base>/<identifier>` as Tauri's `app_data_dir` lays it out; the base is the
    // parent of the resolved folder, so a debug build's `Dev` namespace never changes the search.
    let app = rightkit_fs::AppId::new("com.rightsuite.voiceright").without_org();
    rightkit_fs::AppDirs::resolve(app)
        .ok()
        .and_then(|dirs| dirs.data.parent().map(|base| base.join(&rel)))
        .into_iter()
        .collect()
}

fn has_prompts(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pb"))
        })
        .unwrap_or(false)
}

fn resolve_source(dir: &Path) -> Option<PathBuf> {
    [
        dir.to_path_buf(),
        dir.join("engine").join("voices"),
        dir.join("voices"),
    ]
    .into_iter()
    .find(|d| d.is_dir() && has_prompts(d))
}

fn import_voiceright(ctx: &Context, input: &Value) -> GenResult<Value> {
    import_from(ctx, input, &voiceright_default_dirs())
}

fn import_from(ctx: &Context, input: &Value, defaults: &[PathBuf]) -> GenResult<Value> {
    let (searched, source): (Vec<PathBuf>, Option<PathBuf>) = match str_of(input, "source_dir") {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_absolute() {
                return Err(bad("source_dir must be an absolute path"));
            }
            (vec![p.clone()], resolve_source(&p))
        }
        None => (
            defaults.to_vec(),
            defaults.iter().find_map(|d| resolve_source(d)),
        ),
    };
    let searched_json: Vec<String> = searched.iter().map(|p| p.display().to_string()).collect();
    let Some(src) = source else {
        return Ok(json!({
            "source_found": false, "searched": searched_json, "found": 0,
            "imported": [], "skipped": [], "errors": [],
            "verification": "pending",
        }));
    };
    let mut prompts: Vec<PathBuf> = std::fs::read_dir(&src)
        .map_err(io)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("pb"))
        .collect();
    prompts.sort();
    let (mut imported, mut skipped, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    for p in &prompts {
        let raw_id = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        // VoiceRight file names may carry dotted suffixes (e.g.
        // `sample1.xvector.voice-clone-prompt.pb`); slugify instead of skipping.
        let src_id = slug_id(&raw_id);
        if src_id.is_empty() {
            skipped.push(json!({ "source_id": raw_id, "reason": "invalid_id" }));
            continue;
        }
        let meta = match std::fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(e) => {
                errors.push(json!({ "source_id": src_id, "message": e.to_string() }));
                continue;
            }
        };
        if !meta.is_file() {
            skipped.push(json!({ "source_id": src_id, "reason": "not_a_regular_file" }));
            continue;
        }
        let bytes = match std::fs::read(p) {
            Ok(b) => b,
            Err(e) => {
                errors.push(json!({ "source_id": src_id, "message": e.to_string() }));
                continue;
            }
        };
        if bytes.is_empty() {
            skipped.push(json!({ "source_id": src_id, "reason": "empty_prompt" }));
            continue;
        }
        let sha = library::sha256_hex(&bytes);
        let id = format!("vr-{src_id}");
        let rec_path = local_dir(ctx).join(format!("{id}.json"));
        if let Some(existing) = read_json(&rec_path) {
            if existing["provenance"]["source_sha256"] == sha.as_str() {
                skipped
                    .push(json!({ "source_id": src_id, "id": id, "reason": "already_imported" }));
                continue;
            }
        }
        let name = std::fs::read_to_string(p.with_extension("name"))
            .ok()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| src_id.clone());
        let prompt_file = format!("{id}.pb");
        let rec = json!({
            "id": id,
            "name": name,
            "kind": "imported",
            "engine": "qts",
            "prompt_file": prompt_file,
            "verified": false,
            "verification": "pending",
            // The prompt is supplied material from the VoiceRight folder; no consent is stated
            // there, so rights and evidence stay `unknown`.
            "facts": crate::provenance::supplied_unknown(Some(p.display().to_string()), Vec::new()).to_value(),
            "provenance": {
                "origin": "voiceright",
                "source_id": raw_id,
                "source_slug": src_id,
                "source_path": p.display().to_string(),
                "source_sha256": sha,
                "bytes": bytes.len(),
                "imported_at": store::now(),
            },
            "created_at": store::now(),
        });
        let write = std::fs::create_dir_all(local_dir(ctx))
            .map_err(io)
            .and_then(|_| atomic_bytes(&local_dir(ctx).join(&prompt_file), &bytes))
            .and_then(|_| write_json(&rec_path, &rec));
        match write {
            Ok(()) => imported.push(json!({ "id": id, "name": name, "source_id": src_id, "bytes": bytes.len(), "verified": false })),
            Err(e) => errors.push(json!({ "source_id": src_id, "message": e.message })),
        }
    }
    Ok(json!({
        "source_found": true, "source_dir": src.display().to_string(), "searched": searched_json,
        "found": prompts.len(), "imported": imported, "skipped": skipped, "errors": errors,
        "verification": "pending",
        "note": "Imported voices stay verified:false until the QTS lane's synth check runs (studio.voices.verify).",
    }))
}

/// `verify` `{id}`: run the registered synth check for one local voice.
fn verify(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = str_of(input, "id").ok_or_else(|| bad("missing field: id"))?;
    if !valid_id(id) {
        return Err(bad("id has invalid characters"));
    }
    let path = local_dir(ctx).join(format!("{id}.json"));
    let mut rec = read_json(&path)
        .ok_or_else(|| GenError::new("voice_not_found", format!("no local voice {id}")))?;
    let outcome = match SYNTH_CHECK.get() {
        Some(check) => check(ctx, &rec),
        None => default_synth_check(ctx, &rec),
    };
    rec["verified"] = json!(outcome.is_ok());
    rec["verification"] = json!(if outcome.is_ok() { "passed" } else { "failed" });
    rec["verified_at"] = json!(store::now());
    write_json(&path, &rec)?;
    outcome?;
    Ok(json!({ "id": id, "verified": true }))
}

/// Built-in synth check (§4.7): one short line through the in-process QTS
/// provider with the record's x-vector prompt. Missing model files surface as
/// `verify_pending` so the record stays `verified:false` until install.
fn default_synth_check(ctx: &Context, rec: &Value) -> GenResult<()> {
    let prompt = str_of(rec, "prompt_file")
        .filter(|f| !f.contains(['/', '\\']) && local_dir(ctx).join(f).is_file())
        .ok_or_else(|| {
            GenError::new(
                "voice_prompt_missing",
                "the voice record has no prompt file",
            )
        })?;
    let provs = providers::registry();
    let provider = provs.iter().find(|p| p.id() == qts::ID).ok_or_else(|| {
        GenError::new(
            "provider_unavailable",
            "the local-qts provider is not registered",
        )
    })?;
    let req = GenRequest {
        kind: Kind::Voice,
        provider: qts::ID.into(),
        model: qts::MODEL.into(),
        prompt: "Synthesizing a short check to verify this voice.".into(),
        params: json!({ "voice_prompt": prompt }),
    };
    provider.submit(&req, "").map(|_| ()).map_err(|e| {
        if e.code == "not_installed" {
            GenError::new("verify_pending", e.message)
        } else {
            GenError::new(e.code, e.message)
        }
    })
}

// ---------------------------------------------------------------- tier and cloning

fn tier_with(ctx: &Context, el_base: &str) -> GenResult<Value> {
    let key = el_key(ctx)?;
    let r = El::new(el_base).get("/v1/user", &key)?;
    if !(200..300).contains(&r.status) {
        let mut e = status_error(&r, &key);
        if r.status == 401 {
            e.message
                .push_str(" (a restricted key needs the user_read scope to report its tier)");
        }
        return Err(e);
    }
    let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let s = &v["subscription"];
    Ok(json!({
        "provider": ELEVENLABS,
        "tier": s.get("tier").cloned().unwrap_or(Value::Null),
        "status": s.get("status").cloned().unwrap_or(Value::Null),
        "can_use_instant_voice_cloning": s.get("can_use_instant_voice_cloning").and_then(Value::as_bool).unwrap_or(false),
        "can_use_professional_voice_cloning": s.get("can_use_professional_voice_cloning").and_then(Value::as_bool).unwrap_or(false),
        "character_count": s.get("character_count").cloned().unwrap_or(Value::Null),
        "character_limit": s.get("character_limit").cloned().unwrap_or(Value::Null),
        "voice_limit": s.get("voice_limit").cloned().unwrap_or(Value::Null),
    }))
}

fn multipart(fields: &[(&str, &str)], file: (&str, &str, &str, &[u8])) -> Multipart {
    let mut form = Multipart::new();
    for (k, v) in fields {
        form = form.text(k, v);
    }
    let (field, name, mime, data) = file;
    form.file_bytes(field, name, mime, data.to_vec())
}

fn audio_mime(ext: &str) -> &'static str {
    match ext {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => "audio/ogg",
    }
}

/// What an ElevenLabs instant clone costs, shown before the user confirms and returned in the
/// command result. There is no per-call charge to estimate or reserve: the clone takes one of the
/// plan's voice slots. (Researched 2026-10-11; no price is invented here.)
pub const CLONE_COST_NOTICE: &str = "No per-call charge; uses a plan voice slot.";

/// `clone_elevenlabs` `{name, audio_path, consent:true, description?, dry_run?}`.
/// Order: consent, input, `dry_run` (returns the cost notice, no key, no network), key, tier
/// (`clone_not_allowed`), upload.
fn clone_with(ctx: &Context, el_base: &str, input: &Value) -> GenResult<Value> {
    if input.get("consent").and_then(Value::as_bool) != Some(true) {
        return Err(GenError::new(
            "consent_required",
            "Confirm you have the speaker's consent to clone this voice before continuing.",
        ));
    }
    let name = str_of(input, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| bad("missing field: name"))?;
    if name.chars().count() > 100 {
        return Err(bad("name is longer than 100 characters"));
    }
    let audio =
        PathBuf::from(str_of(input, "audio_path").ok_or_else(|| bad("missing field: audio_path"))?);
    let ext = audio
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !CLONE_EXTS.contains(&ext.as_str()) {
        return Err(bad(format!(
            "reference audio must be one of: {}",
            CLONE_EXTS.join(", ")
        )));
    }
    let meta = std::fs::metadata(&audio).map_err(|_| bad("reference audio file not found"))?;
    if !meta.is_file() {
        return Err(bad("reference audio is not a file"));
    }
    if meta.len() == 0 || meta.len() > MAX_CLONE_BYTES {
        return Err(bad("reference audio must be between 1 byte and 10 MB"));
    }
    if input
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(json!({
            "dry_run": true, "provider": ELEVENLABS, "name": name,
            "est_usd": null, "cost_notice": CLONE_COST_NOTICE,
        }));
    }
    let key = el_key(ctx)?;
    let el = El::new(el_base);

    let u = el.get("/v1/user", &key)?;
    let allowed = if (200..300).contains(&u.status) {
        let v: Value = serde_json::from_slice(&u.body).unwrap_or(Value::Null);
        if v["subscription"]["can_use_instant_voice_cloning"].as_bool() == Some(true) {
            Ok(())
        } else {
            let tier = v["subscription"]["tier"].as_str().unwrap_or("this plan");
            Err(format!(
                "ElevenLabs instant voice cloning is not available on {tier}."
            ))
        }
    } else if u.status == 401 {
        Err("Could not confirm the account tier (the key lacks the user_read scope), so cloning was not attempted.".to_string())
    } else {
        return Err(status_error(&u, &key));
    };
    if let Err(msg) = allowed {
        return Err(GenError::new("clone_not_allowed", msg));
    }

    let data = std::fs::read(&audio).map_err(io)?;
    let file_name = audio
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("reference");
    let mut fields = vec![("name", name)];
    if let Some(d) = str_of(input, "description") {
        fields.push(("description", d));
    }
    let form = multipart(&fields, ("files", file_name, audio_mime(&ext), &data));
    let r = el.post_multipart("/v1/voices/add", &key, form)?;
    if !(200..300).contains(&r.status) {
        let e = status_error(&r, &key);
        if r.status == 402
            || (r.status == 400 && e.message.to_ascii_lowercase().contains("subscription"))
        {
            return Err(GenError::new("clone_not_allowed", e.message));
        }
        return Err(e);
    }
    let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let voice_id = str_of(&v, "voice_id")
        .ok_or_else(|| GenError::new("bad_response", "elevenlabs returned no voice_id"))?;
    // Consent log: who/what was cloned and when, kept beside the library.
    let log_path = voices_dir(ctx).join("clone-consent.json");
    let mut log = read_json(&log_path)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    log.push(json!({
        "provider": ELEVENLABS, "voice_id": voice_id, "name": name, "consent": true,
        "reference_file": file_name, "reference_sha256": library::sha256_hex(&data), "at": store::now(),
        "facts": crate::provenance::supplied_unknown(Some(audio.display().to_string()), vec![format!("clone-consent:{ELEVENLABS}:{voice_id}")]).to_value(),
    }));
    write_json(&log_path, &Value::Array(log))?;
    Ok(json!({
        "provider": ELEVENLABS, "voice_id": voice_id, "name": name,
        "requires_verification": v.get("requires_verification").and_then(Value::as_bool).unwrap_or(false),
        "est_usd": null, "cost_notice": CLONE_COST_NOTICE,
    }))
}

// ---------------------------------------------------------------- batch

#[derive(Clone, Copy, PartialEq)]
pub enum RunMode {
    /// Drive every job to completion before returning (tests, CLI `wait`).
    Inline,
    /// Return after submitting; a thread in this process drives the jobs.
    Background,
    /// Submit only.
    None,
}

static MANIFEST_LOCK: Mutex<()> = Mutex::new(());

fn manifest_path(ctx: &Context, batch_id: &str) -> PathBuf {
    batches_dir(ctx).join(format!("{batch_id}.json"))
}

fn batch_lines(input: &Value) -> GenResult<Vec<String>> {
    let raw: Vec<String> = match input.get("lines").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        None => str_of(input, "text")
            .map(|t| t.lines().map(String::from).collect())
            .unwrap_or_default(),
    };
    let lines: Vec<String> = raw
        .into_iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Err(bad("the batch has no non-empty lines"));
    }
    if lines.len() > MAX_BATCH_LINES {
        return Err(GenError::new(
            "too_many_lines",
            format!(
                "a batch is limited to {MAX_BATCH_LINES} lines; got {}",
                lines.len()
            ),
        ));
    }
    Ok(lines)
}

fn request_for(
    provider: &str,
    model: &str,
    voice: Option<&str>,
    extra: &Value,
    text: &str,
    request_id: Option<&str>,
) -> Value {
    let mut params = extra.as_object().cloned().unwrap_or_default();
    if let Some(v) = voice {
        params.insert("voice_id".into(), json!(v));
    }
    let mut req = json!({ "kind": "voice", "provider": provider, "model": model, "prompt": text, "params": params });
    if let Some(r) = request_id {
        req["request_id"] = json!(r);
    }
    req
}

pub fn batch_with(
    ctx: &Context,
    provs: &[Box<dyn Provider>],
    input: &Value,
    mode: RunMode,
) -> GenResult<Value> {
    let mut provider = str_of(input, "provider").unwrap_or(ELEVENLABS).to_string();
    let mut model = str_of(input, "model")
        .unwrap_or(if provider == ELEVENLABS {
            "eleven_v4_turbo"
        } else {
            "elevenlabs-turbo-v2.5"
        })
        .to_string();
    let preset =
        str_of(input, "preset").or_else(|| input.get("params").and_then(|p| str_of(p, "preset")));
    let voice = str_of(input, "voice_id").map(String::from);
    if preset.is_some() && voice.is_some() {
        return Err(bad("preset is an alternative to voice_id"));
    }
    if let Some(preset) = preset {
        validate_preset(preset)?;
    }
    if let Some(v) = voice.as_deref() {
        if !valid_id(v) {
            return Err(bad("voice_id has invalid characters"));
        }
    }
    let mut extra = input.get("params").cloned().unwrap_or(Value::Null);
    if let Some(preset) = preset {
        if provider != "local" && provider != qts::ID {
            return Err(bad("presets require provider local or local-qts"));
        }
        provider = "local".into();
        let mut params = extra.as_object().cloned().unwrap_or_default();
        if params.contains_key("voice_prompt") || params.contains_key("voice_id") {
            return Err(bad("preset is an alternative to voice_id/voice_prompt"));
        }
        params.insert("preset".into(), json!(preset.to_ascii_lowercase()));
        if let Some(instruct) = str_of(input, "instruct") {
            params.insert("instruct".into(), json!(instruct));
        }
        extra = Value::Object(params);
    } else if str_of(input, "instruct").is_some() {
        return Err(bad("instruct requires preset"));
    }
    // `provider:"local"` voices synthesize through the in-process QTS lane:
    // the record's x-vector prompt file becomes the `voice_prompt` param.
    let is_local = provider == "local";
    if is_local {
        provider = qts::ID.to_string();
        model = qts::MODEL.to_string();
        if let Some(v) = &voice {
            let rec = read_json(&local_dir(ctx).join(format!("{v}.json")))
                .ok_or_else(|| GenError::new("voice_not_found", format!("no local voice {v}")))?;
            let prompt = str_of(&rec, "prompt_file")
                .filter(|f| !f.contains(['/', '\\']) && local_dir(ctx).join(f).is_file())
                .ok_or_else(|| {
                    GenError::new(
                        "voice_prompt_missing",
                        format!("local voice {v} has no prompt file"),
                    )
                })?;
            let mut o = extra.as_object().cloned().unwrap_or_default();
            o.insert("voice_prompt".into(), json!(prompt));
            extra = Value::Object(o);
        }
    }
    let voice_param = if is_local { None } else { voice.as_deref() };
    let provider = provider.as_str();
    let model = model.as_str();
    let voice = voice_param;
    let lines = batch_lines(input)?;

    if input
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mut est = 0.0;
        for t in &lines {
            let r = api::generate_with(
                ctx,
                provs,
                &request_for(provider, model, voice, &extra, t, None),
                true,
            )?;
            est += r["est_usd"].as_f64().unwrap_or(0.0);
        }
        return Ok(
            json!({ "dry_run": true, "lines": lines.len(), "est_usd": est, "provider": provider, "model": model }),
        );
    }

    let batch_id = match str_of(input, "batch_id") {
        Some(b) if valid_id(b) => b.to_string(),
        Some(_) => return Err(bad("batch_id has invalid characters")),
        None => new_id("b"),
    };
    let out_dir = match str_of(input, "output_dir") {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_absolute()
                || p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(bad("output_dir must be an absolute path without .."));
            }
            p
        }
        None => batches_dir(ctx).join(&batch_id),
    };
    let prefix = str_of(input, "name_prefix")
        .filter(|p| valid_id(p))
        .unwrap_or("line");
    let mut entries: Vec<Value> = Vec::new();
    for (i, text) in lines.iter().enumerate() {
        let n = i + 1;
        let rid = format!("voices-batch-{batch_id}-{n:04}");
        match api::generate_with(ctx, provs, &request_for(provider, model, voice, &extra, text, Some(&rid)), false) {
            Ok(r) => entries.push(json!({
                "n": n, "text": text, "request_id": rid, "job_id": r["job_id"], "est_usd": r["est_usd"],
                "status": "queued", "file": null, "error": null,
            })),
            // Nothing submitted yet: a request-level failure (key, model, cap) aborts cleanly.
            Err(e) if i == 0 => return Err(e),
            Err(e) => entries.push(json!({
                "n": n, "text": text, "request_id": rid, "job_id": null, "est_usd": 0.0,
                "status": "rejected", "file": null, "error": { "code": e.code, "message": e.message },
            })),
        }
    }
    std::fs::create_dir_all(&out_dir).map_err(io)?;
    let manifest = json!({
        "batch_id": batch_id, "created_at": store::now(), "provider": provider, "model": model, "voice_id": voice,
        "output_dir": out_dir.display().to_string(), "prefix": prefix, "lines": entries,
    });
    {
        let _g = MANIFEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_json(&manifest_path(ctx, &batch_id), &manifest)?;
    }
    match mode {
        RunMode::Inline => run_batch(ctx, provs, &batch_id),
        RunMode::Background => spawn_batch(ctx, &batch_id),
        RunMode::None => {}
    }
    batch_status_of(ctx, &batch_id)
}

fn spawn_batch(ctx: &Context, batch_id: &str) {
    let (ctx, id) = (ctx.clone(), batch_id.to_string());
    std::thread::spawn(move || run_batch(&ctx, &providers::registry(), &id));
}

/// Drive every non-terminal job one after another (providers rate-limit),
/// collecting each finished file into the output folder as it lands.
fn run_batch(ctx: &Context, provs: &[Box<dyn Provider>], batch_id: &str) {
    let Some(m) = read_json(&manifest_path(ctx, batch_id)) else {
        return;
    };
    let opts = jobs::RunOptions {
        stderr_events: false,
        ..jobs::RunOptions::default()
    };
    for line in m["lines"].as_array().cloned().unwrap_or_default() {
        if let Some(job_id) = line["job_id"].as_str() {
            let _ = jobs::run_job_with(ctx, provs, job_id, &opts);
            let _ = collect(ctx, batch_id);
        }
    }
}

fn is_final(s: &str) -> bool {
    matches!(s, "complete" | "failed" | "cancelled" | "rejected")
}

/// Refresh every line from its job and copy finished audio into the output
/// folder (idempotent), then persist the manifest.
fn collect(ctx: &Context, batch_id: &str) -> GenResult<Value> {
    let _g = MANIFEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = manifest_path(ctx, batch_id);
    let mut m = read_json(&path)
        .ok_or_else(|| GenError::new("batch_not_found", format!("no batch {batch_id}")))?;
    let out_dir = PathBuf::from(m["output_dir"].as_str().unwrap_or_default());
    let prefix = m["prefix"].as_str().unwrap_or("line").to_string();
    let conn = store::open(ctx)?;
    let mut changed = false;
    if let Some(lines) = m["lines"].as_array_mut() {
        for line in lines.iter_mut() {
            let cur = line["status"].as_str().unwrap_or("queued").to_string();
            let Some(rid) = line["request_id"].as_str().map(String::from) else {
                continue;
            };
            if cur == "rejected"
                || (cur == "complete" && !line["file"].is_null())
                || line["job_id"].is_null()
            {
                continue;
            }
            let Ok(job) = store::get_job_by_request(&conn, &rid) else {
                continue;
            };
            let status = job.status.clone();
            if status != cur {
                line["status"] = json!(status);
                changed = true;
            }
            // A held line (`outcome_unknown`) keeps the job's error so the view can tell a
            // possibly-delivered submit from a saved-but-unstored result (`result_recovered`).
            let err = if matches!(
                status.as_str(),
                "failed" | "cancelled" | store::OUTCOME_UNKNOWN
            ) {
                job.error_json
                    .as_deref()
                    .and_then(|e| serde_json::from_str::<Value>(e).ok())
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            if line["error"] != err {
                line["error"] = err;
                changed = true;
            }
            if status == "complete" && line["file"].is_null() {
                let res = api::job_result(ctx, &json!({ "job_id": job.job_id }))?;
                if let Some(src) = res["assets"][0]["path"].as_str() {
                    let ext = Path::new(src)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("mp3");
                    let n = line["n"].as_u64().unwrap_or(0);
                    let dest = out_dir.join(format!("{prefix}-{n:03}.{ext}"));
                    std::fs::create_dir_all(&out_dir).map_err(io)?;
                    copy_export(Path::new(src), &dest)?;
                    line["file"] = json!(dest.display().to_string());
                    changed = true;
                }
            }
        }
    }
    if changed {
        write_json(&path, &m)?;
    }
    Ok(m)
}

fn batch_status_of(ctx: &Context, batch_id: &str) -> GenResult<Value> {
    let m = collect(ctx, batch_id)?;
    let conn = store::open(ctx)?;
    let mut lines = Vec::new();
    let (mut done, mut failed, mut running, mut held, mut est) = (0, 0, 0, 0, 0.0);
    for l in m["lines"].as_array().cloned().unwrap_or_default() {
        let status = l["status"].as_str().unwrap_or("queued");
        est += l["est_usd"].as_f64().unwrap_or(0.0);
        match status {
            "complete" => done += 1,
            "failed" | "cancelled" | "rejected" => failed += 1,
            // Held for the user: nothing drives it, so it neither runs nor finishes the batch.
            store::OUTCOME_UNKNOWN => held += 1,
            _ => running += 1,
        }
        let progress = l["request_id"]
            .as_str()
            .filter(|_| !is_final(status))
            .and_then(|rid| store::get_job_by_request(&conn, rid).ok())
            .and_then(|j| store::latest_event(&conn, &j.job_id).ok())
            .unwrap_or(Value::Null);
        let mut l = l;
        l["progress"] = progress;
        lines.push(l);
    }
    let total = lines.len();
    Ok(json!({
        "batch_id": m["batch_id"], "provider": m["provider"], "model": m["model"], "voice_id": m["voice_id"],
        "output_dir": m["output_dir"], "created_at": m["created_at"], "total": total,
        "completed": done, "failed": failed, "running": running, "held": held, "est_usd": est,
        "finished": running == 0, "lines": lines,
    }))
}

/// `batch_status` `{batch_id, resume?}`; `resume:true` restarts a background
/// run for jobs that are still unfinished (a job lock keeps this safe).
fn batch_status(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = str_of(input, "batch_id").ok_or_else(|| bad("missing field: batch_id"))?;
    if !valid_id(id) {
        return Err(bad("batch_id has invalid characters"));
    }
    let st = batch_status_of(ctx, id)?;
    if input
        .get("resume")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && st["running"].as_u64().unwrap_or(0) > 0
    {
        spawn_batch(ctx, id);
    }
    Ok(st)
}

// ---------------------------------------------------------------- local engine: rename, clone, preview

/// `rename` `{id, name}`: local voice records only.
fn rename(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = str_of(input, "id").ok_or_else(|| bad("missing field: id"))?;
    if !valid_id(id) {
        return Err(bad("id has invalid characters"));
    }
    let name = str_of(input, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| bad("missing field: name"))?;
    if name.chars().count() > 100 {
        return Err(bad("name is longer than 100 characters"));
    }
    let path = local_dir(ctx).join(format!("{id}.json"));
    let mut rec = read_json(&path)
        .ok_or_else(|| GenError::new("voice_not_found", format!("no local voice {id}")))?;
    rec["name"] = json!(name);
    write_json(&path, &rec)?;
    Ok(json!({ "id": id, "name": name }))
}

fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        "voice".to_string()
    } else {
        s.chars().take(64).collect()
    }
}

/// `clone_local` `{name, audio_path, consent:true}`: instant x-vector clone on
/// the on-device engine (no upload, no spend). Same consent gate as hosted
/// cloning (plan invariant 7).
fn clone_local(ctx: &Context, input: &Value) -> GenResult<Value> {
    if input.get("consent").and_then(Value::as_bool) != Some(true) {
        return Err(GenError::new(
            "consent_required",
            "Confirm you have the speaker's consent to clone this voice before continuing.",
        ));
    }
    let name = str_of(input, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| bad("missing field: name"))?;
    if name.chars().count() > 100 {
        return Err(bad("name is longer than 100 characters"));
    }
    let audio =
        PathBuf::from(str_of(input, "audio_path").ok_or_else(|| bad("missing field: audio_path"))?);
    let ext = audio
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !CLONE_EXTS.contains(&ext.as_str()) {
        return Err(bad(format!(
            "reference audio must be one of: {}",
            CLONE_EXTS.join(", ")
        )));
    }
    let meta = std::fs::metadata(&audio).map_err(|_| bad("reference audio file not found"))?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_CLONE_BYTES {
        return Err(bad(
            "reference audio must be a file between 1 byte and 10 MB",
        ));
    }
    let dir = models::model_dir();
    if !models::files_present(&dir) {
        return Err(GenError::new(
            "not_installed",
            "the on-device voice model is not installed; install it from the voice library",
        ));
    }
    let base = slug(name);
    let mut id = base.clone();
    for i in 2..1000 {
        if !local_dir(ctx).join(format!("{id}.json")).exists() {
            break;
        }
        id = format!("{base}-{i}");
    }
    let prompt_file = format!("{id}.xvector.pb");
    let out = local_dir(ctx).join(&prompt_file);
    qts::qts_client::clone_from_wav(&audio, &out).map_err(|e| {
        GenError::new(
            if e.code == "worker_missing"
                || e.code == "models_missing"
                || e.code == "models_invalid"
            {
                "not_installed"
            } else {
                e.code
            },
            e.message,
        )
    })?;
    let ref_bytes = std::fs::read(&audio).unwrap_or_default();
    let rec = json!({
        "id": id, "name": name, "kind": "clone", "engine": "qts",
        "prompt_file": prompt_file, "verified": false, "verification": "pending",
        // The reference audio is supplied material; the clone gate required `consent: true`, which
        // is logged in clone-consent.json. That statement is carried as a consent reference only:
        // rights stay `unknown` (consent is not a licence) and evidence `unknown`.
        "facts": crate::provenance::supplied_unknown(Some(audio.display().to_string()), vec![format!("clone-consent:local:{id}")]).to_value(),
        "provenance": {
            "origin": "local_clone",
            "reference_file": audio.file_name().and_then(|n| n.to_str()).unwrap_or("reference"),
            "reference_sha256": library::sha256_hex(&ref_bytes),
            "created_by": "studio.voices.clone_local",
        },
        "created_at": store::now(),
    });
    write_json(&local_dir(ctx).join(format!("{id}.json")), &rec)?;
    let log_path = voices_dir(ctx).join("clone-consent.json");
    let mut log = read_json(&log_path)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    log.push(json!({ "provider": "local", "voice_id": id, "name": name, "consent": true,
        "reference_file": audio.display().to_string(), "reference_sha256": library::sha256_hex(&ref_bytes), "at": store::now() }));
    write_json(&log_path, &Value::Array(log))?;
    Ok(json!({ "id": id, "name": name, "provider": "local", "verified": false }))
}

/// `preview` `{id, text?}`: synthesize a short line with a local voice and
/// return the WAV path for the shared MediaPlayer.
fn preview(ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut params = Map::new();
    let id = if let Some(preset) = str_of(input, "preset") {
        if str_of(input, "id").is_some() || str_of(input, "voice_id").is_some() {
            return Err(bad("preset is an alternative to voice id"));
        }
        validate_preset(preset)?;
        params.insert("preset".into(), json!(preset.to_ascii_lowercase()));
        if let Some(instruct) = str_of(input, "instruct") {
            params.insert("instruct".into(), json!(instruct));
        }
        preset.to_ascii_lowercase()
    } else {
        if str_of(input, "instruct").is_some() {
            return Err(bad("instruct requires preset"));
        }
        let id = str_of(input, "id")
            .or_else(|| str_of(input, "voice_id"))
            .ok_or_else(|| bad("missing field: id or preset"))?;
        if !valid_id(id) {
            return Err(bad("id has invalid characters"));
        }
        let path = local_dir(ctx).join(format!("{id}.json"));
        let rec = read_json(&path)
            .ok_or_else(|| GenError::new("voice_not_found", format!("no local voice {id}")))?;
        if let Some(f) = str_of(&rec, "prompt_file")
            .filter(|f| !f.contains(['/', '\\']) && local_dir(ctx).join(f).is_file())
        {
            params.insert("voice_prompt".into(), json!(f));
        }
        id.to_string()
    };
    if let Some(language) = str_of(input, "language") {
        params.insert("language".into(), json!(language));
    }
    let text = str_of(input, "text").unwrap_or("This is a preview of my voice.");
    let provs = providers::registry();
    let provider = provs.iter().find(|p| p.id() == qts::ID).ok_or_else(|| {
        GenError::new(
            "provider_unavailable",
            "the local-qts provider is not registered",
        )
    })?;
    let req = GenRequest {
        kind: Kind::Voice,
        provider: qts::ID.into(),
        model: qts::MODEL.into(),
        prompt: text.into(),
        params: Value::Object(params),
    };
    let out = provider
        .submit(&req, "")
        .map_err(|e| GenError::new(e.code, e.message))?;
    let SubmitOutcome::Completed { outputs } = out else {
        return Err(GenError::new(
            "provider_unavailable",
            "local-qts returned no audio",
        ));
    };
    let Some(Output::Bytes { bytes, .. }) = outputs.into_iter().next() else {
        return Err(GenError::new(
            "provider_unavailable",
            "local-qts returned no audio",
        ));
    };
    write_preview(ctx, &format!("{id}-preview"), &bytes).map(|mut result| {
        result["id"] = json!(id);
        result
    })
}

fn write_preview(ctx: &Context, stem: &str, bytes: &[u8]) -> GenResult<Value> {
    let dir = voices_dir(ctx).join("previews");
    std::fs::create_dir_all(&dir).map_err(io)?;
    let file = dir.join(format!("{stem}.wav"));
    atomic_bytes(&file, bytes)?;
    Ok(json!({ "path": file.display().to_string(), "bytes": bytes.len() }))
}

fn validate_preset(preset: &str) -> GenResult<()> {
    if !qts::qts_client::PRESET_SPEAKERS.contains(&preset.to_ascii_lowercase().as_str()) {
        return Err(bad("unknown preset speaker"));
    }
    let mode = qts::qts_client::Mode::CustomVoice;
    if !models::mode_files_present(&models::model_dir_for_mode(mode), mode) {
        return Err(GenError::new(
            "not_installed",
            "model_missing: custom_voice",
        ));
    }
    Ok(())
}

fn presets() -> GenResult<Value> {
    let mode = qts::qts_client::Mode::CustomVoice;
    Ok(json!({ "speakers": qts::qts_client::PRESET_SPEAKERS,
        "installed": models::mode_files_present(&models::model_dir_for_mode(mode), mode) }))
}

fn design(ctx: &Context, input: &Value) -> GenResult<Value> {
    let instruct = str_of(input, "instruct")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("missing field: instruct"))?;
    let text = str_of(input, "sample_text")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("missing field: sample_text"))?;
    let name = str_of(input, "name")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Designed voice");
    if name.chars().count() > 100 {
        return Err(bad("name is longer than 100 characters"));
    }
    let save = input.get("save").and_then(Value::as_bool).unwrap_or(false);
    let mode = qts::qts_client::Mode::VoiceDesign;
    if !models::mode_files_present(&models::model_dir_for_mode(mode), mode) {
        return Err(GenError::new(
            "not_installed",
            "model_missing: voice_design",
        ));
    }
    if save && !models::files_present(&models::model_dir()) {
        return Err(GenError::new(
            "not_installed",
            "Base model is needed to save a designed voice",
        ));
    }
    let language = str_of(input, "language").unwrap_or("en");
    let language_id =
        qts::qts_client::language_id(language).ok_or_else(|| bad("unsupported language"))?;
    let opts = qts::qts_client::SynthOptions {
        mode,
        instruct: Some(instruct.into()),
        language_id,
        ..Default::default()
    };
    let audio = qts::qts_client::synthesize(text, &opts).map_err(|e| {
        GenError::new(
            if matches!(
                e.code,
                "models_missing" | "worker_missing" | "models_invalid"
            ) {
                "not_installed"
            } else {
                e.code
            },
            e.message,
        )
    })?;
    let mut result = write_preview(ctx, &new_id("design"), &audio.wav)?;
    result["voice_id"] = Value::Null;
    if save {
        let cloned = clone_local(
            ctx,
            &json!({ "name": name, "audio_path": result["path"], "consent": true }),
        )?;
        let id = cloned["id"]
            .as_str()
            .ok_or_else(|| GenError::new("clone_failed", "clone returned no voice id"))?;
        let path = local_dir(ctx).join(format!("{id}.json"));
        let mut rec = read_json(&path)
            .ok_or_else(|| GenError::new("voice_not_found", "saved voice record missing"))?;
        rec["provenance"]["origin"] = json!("designed");
        rec["provenance"]["instruct"] = json!(instruct);
        rec["provenance"]["created_by"] = json!("studio.voices.design");
        write_json(&path, &rec)?;
        result["voice_id"] = json!(id);
    }
    Ok(result)
}

// ---------------------------------------------------------------- model install

/// Install progress in the `models_status`/`install_status` shape, read from
/// the shared model install (`rightkit-models`) record and live session.
fn install_view(ctx: &Context) -> Value {
    let id = models::MODEL_ID;
    if let Some(p) = (install_hooks().live_progress)(id) {
        return json!({ "state": "running", "file": p["file"], "downloaded_bytes": p["bytes_done"], "total_bytes": p["bytes_total"], "at": store::now() });
    }
    let rec = (install_hooks().install_record)(ctx, id);
    match rec["state"].as_str() {
        Some("installed") => json!({ "state": "done", "at": rec["updated_at"] }),
        Some("failed") | Some("cancelled") => {
            let last = rec["events"]
                .as_array()
                .and_then(|e| e.last())
                .map(|e| e["detail"].clone())
                .unwrap_or(Value::Null);
            json!({ "state": "error", "error": { "code": last["code"].as_str().unwrap_or("cancelled"), "message": last["message"].as_str().unwrap_or("install cancelled") }, "at": rec["updated_at"] })
        }
        _ => json!({ "state": "idle" }),
    }
}

/// `models_status`: what the on-device engine still needs — model weights,
/// the ONNX Runtime library, and the worker binary.
fn models_status(ctx: &Context) -> GenResult<Value> {
    let dir = models::model_dir();
    let files: Vec<Value> = models::MANIFEST
        .iter()
        .map(|f| {
            let p = dir.join(f.name);
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            json!({ "name": f.name, "expected_bytes": f.bytes, "bytes": bytes, "present": bytes == f.bytes })
        })
        .collect();
    let models_ok = models::files_present(&dir);
    let ort = models::find_onnxruntime();
    // macOS loads ONNX Runtime only from the app bundle; report where and whether it is there.
    #[cfg(target_os = "macos")]
    let bundled = models::bundled_onnxruntime_path();
    #[cfg(not(target_os = "macos"))]
    let bundled: Option<std::path::PathBuf> = None;
    let worker = qts::qts_client::worker_exe().ok();
    let state = install_view(ctx);
    let modes: Map<String, Value> = [qts::qts_client::Mode::Base, qts::qts_client::Mode::CustomVoice, qts::qts_client::Mode::VoiceDesign]
        .into_iter().map(|mode| {
            let dir = models::model_dir_for_mode(mode);
            let installed = models::mode_files_present(&dir, mode);
            (mode.as_str().into(), json!({ "installed": installed, "ready": installed && ort.is_some() && worker.is_some(),
                "model_dir": dir.display().to_string(), "files": models::mode_files(&dir, mode).iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "local_only": mode != qts::qts_client::Mode::Base }))
        }).collect();
    Ok(json!({
        "engine": "qts",
        "model_dir": dir.display().to_string(),
        "files": files,
        "models_installed": models_ok,
        "modes": modes,
        "onnxruntime": { "present": ort.is_some(), "path": ort.as_ref().map(|p| p.display().to_string()), "bundled_path": bundled.as_ref().map(|p| p.display().to_string()), "bundled_exists": bundled.as_ref().map(|p| p.is_file()), "name": models::runtime_lib_name(), "supported_platform": (install_hooks().ort_supported)() },
        "worker": { "present": worker.is_some(), "path": worker.as_ref().map(|p| p.display().to_string()) },
        "ready": models_ok && ort.is_some() && worker.is_some(),
        "download_bytes": models::DOWNLOAD_BYTES,
        "install": state,
    }))
}

/// `install`: download the QTS weights and the official ONNX Runtime release
/// library in the background through the shared model install
/// (`rightkit-models`: manifest-pinned, resumable, atomically activated);
/// poll with `install_status`.
fn install(ctx: &Context) -> GenResult<Value> {
    (install_hooks().start_install)(ctx)?;
    install_status(ctx)
}

fn install_status(ctx: &Context) -> GenResult<Value> {
    let mut st = install_view(ctx);
    st["running"] = json!((install_hooks().live_progress)(models::MODEL_ID).is_some());
    st["ready"] =
        json!(models::files_present(&models::model_dir()) && models::find_onnxruntime().is_some());
    Ok(st)
}

// ---------------------------------------------------------------- export

/// `export` `{path?|batch_id?, format?=wav|flac|m4a, normalize?, out_dir?, zip?}`:
/// convert finished voice audio to wav/flac/m4a (AAC ~192k) with the resolved
/// ffmpeg, optionally EBU R128 loudness-normalised (`loudnorm` I=-16,
/// TP=-1.5, LRA=11). For a `batch_id`, exports every completed line into a
/// `export-<fmt>` folder beside the batch output with a copied manifest, or
/// into `<data>/voices/exports/<batch_id>-<fmt>.zip` when `zip:true`.
fn export(ctx: &Context, input: &Value) -> GenResult<Value> {
    let format = str_of(input, "format").unwrap_or("wav").to_string();
    if format == "mp3" {
        return Err(bad(
            "mp3 export was replaced by m4a (AAC); use format wav, flac or m4a",
        ));
    }
    if !matches!(format.as_str(), "wav" | "flac" | "m4a") {
        return Err(bad("format must be wav, flac or m4a"));
    }
    let normalize = input
        .get("normalize")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let zip = input.get("zip").and_then(Value::as_bool).unwrap_or(false);

    // Sources: one explicit file, or every completed line of a batch.
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    let mut batch_manifest: Option<Value> = None;
    if let Some(id) = str_of(input, "batch_id") {
        if !valid_id(id) {
            return Err(bad("batch_id has invalid characters"));
        }
        let m = read_json(&manifest_path(ctx, id))
            .ok_or_else(|| GenError::new("batch_not_found", format!("no batch {id}")))?;
        for line in m["lines"].as_array().cloned().unwrap_or_default() {
            if line["status"].as_str() == Some("complete") {
                if let Some(f) = line["file"].as_str() {
                    let p = PathBuf::from(f);
                    if p.is_file() {
                        items.push((format!("line-{:03}", line["n"].as_u64().unwrap_or(0)), p));
                    }
                }
            }
        }
        if items.is_empty() {
            return Err(GenError::new(
                "nothing_to_export",
                format!("batch {id} has no completed files"),
            ));
        }
        batch_manifest = Some(m);
    } else if let Some(src) = str_of(input, "path") {
        let p = PathBuf::from(src);
        if !p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(bad("path must be absolute without .."));
        }
        if !p.is_file() {
            return Err(GenError::new(
                "file_not_found",
                format!("no such file: {src}"),
            ));
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("voice");
        items.push((slug_id(stem).replace('_', "-"), p));
    } else {
        return Err(bad("missing field: path or batch_id"));
    }

    let needs_ffmpeg = normalize || format != "wav";
    let ffmpeg = if needs_ffmpeg {
        Some(ffmpeg_bin()?)
    } else {
        None
    };
    let out_dir = match str_of(input, "output_dir") {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_absolute()
                || p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(bad("output_dir must be an absolute path without .."));
            }
            p
        }
        None => {
            let base = match str_of(input, "batch_id") {
                Some(id) => manifest_path(ctx, id)
                    .parent()
                    .map(|p| p.join(id))
                    .unwrap_or_else(|| voices_dir(ctx)),
                None => voices_dir(ctx).join("exports"),
            };
            if zip {
                voices_dir(ctx).join("exports")
            } else {
                base.join(format!("export-{format}"))
            }
        }
    };
    std::fs::create_dir_all(&out_dir).map_err(io)?;

    let mut files: Vec<Value> = Vec::new();
    let mut zip_parts: Vec<(String, Vec<u8>)> = Vec::new();
    for (stem, src) in &items {
        let name = format!("{stem}.{format}");
        let dest = out_dir.join(&name);
        let bytes = if let Some(ff) = &ffmpeg {
            transcode(ff, src, &dest, &format, normalize)?
        } else {
            if src == &dest {
                src.metadata().map(|m| m.len()).unwrap_or(0)
            } else {
                copy_export(src, &dest)?
            }
        };
        let data = std::fs::read(&dest).map_err(io)?;
        let entry = json!({
            "name": name, "path": dest.display().to_string(), "bytes": bytes,
            "source": src.display().to_string(), "sha256": library::sha256_hex(&data),
        });
        if zip {
            zip_parts.push((name, data));
        }
        files.push(entry);
    }

    let mut zip_path = Value::Null;
    if zip {
        // The zip carries its own manifest: one row per exported file.
        let zmanifest = json!({
            "batch_id": str_of(input, "batch_id"), "format": format, "normalized": normalize,
            "created_at": store::now(),
            "files": files.iter().map(|f| json!({"name": f["name"], "bytes": f["bytes"], "sha256": f["sha256"], "source": f["source"]})).collect::<Vec<_>>(),
        });
        zip_parts.push((
            "manifest.json".into(),
            serde_json::to_vec_pretty(&zmanifest).unwrap_or_default(),
        ));
        let name = format!(
            "{}-{format}.zip",
            str_of(input, "batch_id").unwrap_or("voice-export")
        );
        let zp = out_dir.join(&name);
        atomic_bytes(&zp, &zip_stored(&zip_parts).map_err(io)?)?;
        zip_path = json!(zp.display().to_string());
    } else if let Some(m) = &batch_manifest {
        // Folder export keeps a manifest copy next to the audio.
        let mut m = m.clone();
        m["export"] = json!({ "format": format, "normalized": normalize, "dir": out_dir.display().to_string(), "exported_at": store::now() });
        write_json(&out_dir.join("manifest.json"), &m)?;
    }
    if let Some(m) = &batch_manifest {
        if let Some(id) = m["batch_id"].as_str() {
            let path = manifest_path(ctx, id);
            if let Some(mut live) = read_json(&path) {
                live["last_export"] = json!({
                    "format": format, "normalized": normalize, "zip": zip,
                    "at": store::now(),
                    "target": if zip { zip_path.clone() } else { json!(out_dir.display().to_string()) },
                    "files": files.len(),
                });
                let _ = write_json(&path, &live);
            }
        }
    }

    Ok(json!({
        "format": format, "normalized": normalize, "count": files.len(),
        "dir": out_dir.display().to_string(), "files": files, "zip": zip_path,
        "note": if normalize { "EBU R128 loudness normalised (I=-16, TP=-1.5, LRA=11)" } else { "as-synthesised loudness" },
    }))
}

/// Resolve ffmpeg through `crate::ffmpeg` (rightkit-ffmpeg discovery).
fn ffmpeg_bin() -> GenResult<crate::host::ffmpeg::FfmpegHandle> {
    crate::host::ffmpeg::resolve_ffmpeg().map_err(|e| {
        GenError::new(
            "ffmpeg_missing",
            format!(
                "no verified ffmpeg (bundled; {} or PATH in development builds): {e}",
                crate::host::env_name("FFMPEG")
            ),
        )
    })
}

const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Final-encode arguments per export format (`m4a` = built-in AAC-LC 192k).
fn export_encoding(format: &str) -> rightkit_ffmpeg::loudnorm::ExportEncoding {
    rightkit_ffmpeg::loudnorm::ExportEncoding::for_format(format)
}

/// One ffmpeg transcode; returns the output byte count. `normalize` converges the
/// integrated loudness on -16 LUFS (+-0.5 LU) via `rightkit_ffmpeg::loudnorm::normalize_file`.
fn transcode(
    ffmpeg: &crate::host::ffmpeg::FfmpegHandle,
    src: &Path,
    dest: &Path,
    format: &str,
    normalize: bool,
) -> GenResult<u64> {
    use rightkit_ffmpeg::{loudnorm, run_checked, CancellationToken, FfmpegCommand};
    let cancel = CancellationToken::new();
    let encoding = export_encoding(format);
    if normalize {
        loudnorm::normalize_file(
            ffmpeg,
            src,
            dest,
            &loudnorm::LoudnessTarget::SPEECH_EXPORT,
            &encoding,
            &loudnorm::ConvergeOptions::default(),
            EXPORT_TIMEOUT,
            &cancel,
            None,
        )
        .map_err(|e| {
            GenError::new(
                "export_failed",
                e.to_string().chars().take(300).collect::<String>(),
            )
        })?;
    } else {
        let mut cmd = FfmpegCommand::base("error");
        cmd.input(src)
            .texts(encoding.args.iter().cloned())
            .path(dest);
        run_checked(ffmpeg, &cmd, EXPORT_TIMEOUT, &cancel).map_err(|e| {
            GenError::new(
                "export_failed",
                e.to_string().chars().take(300).collect::<String>(),
            )
        })?;
    }
    dest.metadata().map(|m| m.len()).map_err(io)
}

/// A stored (uncompressed) zip archive, written by RightKit's deterministic stored-zip writer.
fn zip_stored(files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<u8>> {
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    rightkit_fs::stored_zip_bytes(&entries)
}
