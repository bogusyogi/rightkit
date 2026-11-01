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
    #[cfg(test)]
    {
        credentials::get_with_env(ctx, ELEVENLABS, &|_| None)
    }
    #[cfg(not(test))]
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
    let want = |p: &str| filter.map_or(true, |f| f == p);
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
            let n = if src == &dest {
                src.metadata().map(|m| m.len()).unwrap_or(0)
            } else {
                copy_export(src, &dest)?
            };
            n
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
                "no verified ffmpeg (bundled; RIGHTKIT_FFMPEG or PATH in development builds): {e}"
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

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// The Pro gate refuses when no licence check is installed; these unit tests drive cloud providers
    /// through mocks, so they install an allow-all gate (test builds only).
    fn allow_pro() {
        struct AllowAll;
        impl crate::entitlement::Gate for AllowAll {
            fn require(&self, _feature: &str) -> crate::GenResult<()> {
                Ok(())
            }
        }
        crate::entitlement::install(Box::new(AllowAll));
    }
    use crate::credentials::{CredentialSource, MemoryCredentials};
    use crate::providers::{
        Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, ProviderError, SubmitOutcome,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("rightkit-voices-test-{}", new_id("")));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const KEY: &str = "xi-secret-KEY-789";

    fn ctx_with(dir: &Dir, key: Option<&str>) -> Context {
        let creds = Arc::new(MemoryCredentials::new());
        if let Some(k) = key {
            creds.set(ELEVENLABS, k).unwrap();
            creds.set("fake", "k").unwrap();
        }
        Context::new(dir.0.join("data")).with_credentials(creds)
    }

    type Seen = Arc<Mutex<Vec<(String, String, Vec<u8>, String)>>>;

    /// Mock ElevenLabs: handler gets (method, path, body) and returns (status, body).
    fn mock<F>(handler: F) -> (String, Seen)
    where
        F: Fn(&str, &str, &[u8]) -> (u16, Vec<u8>) + Send + 'static,
    {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut st) = stream else { continue };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let hend = loop {
                    let n = st.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(p + 4);
                    }
                };
                let Some(hend) = hend else { continue };
                let head = String::from_utf8_lossy(&buf[..hend]).to_string();
                let cl = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < hend + cl {
                    let n = st.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body = buf[hend..].to_vec();
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                s2.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone(), head));
                let (code, out) = handler(&method, &path, &body);
                let h = format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = st.write_all(h.as_bytes());
                let _ = st.write_all(&out);
            }
        });
        (base, seen)
    }

    fn j(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    fn write_local(ctx: &Context, rec: Value) {
        let id = rec["id"].as_str().unwrap().to_string();
        write_json(&local_dir(ctx).join(format!("{id}.json")), &rec).unwrap();
    }

    // ---- list

    #[test]
    fn list_without_key_falls_back_to_builtin_and_tolerates_empty_local() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        let r = list_with(&ctx, "http://127.0.0.1:1", &json!({})).unwrap();
        let v = r["voices"].as_array().unwrap();
        assert!(v.iter().any(|x| x["provider"] == "elevenlabs"
            && x["id"] == "JBFqnCBsd6RMkjVDRZzb"
            && x["default"] == true));
        assert!(v
            .iter()
            .any(|x| x["provider"] == "wavespeed" && x["id"] == "Alicia" && x["default"] == true));
        assert!(v.iter().all(|x| x["provider"] != "local"));
        assert_eq!(r["warnings"][0]["code"], "no_credentials");
        assert_eq!(
            r["defaults"]["wavespeed/minimax-speech-02-hd"],
            "Wise_Woman"
        );
    }

    #[test]
    fn list_reads_live_elevenlabs_voices_and_local_records() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, seen) = mock(|m, p, _| {
            assert_eq!((m, p), ("GET", "/v1/voices"));
            (
                200,
                j(json!({"voices": [
                    {"voice_id": "v1", "name": "Rachel", "category": "premade", "labels": {"accent": "american"}, "preview_url": "https://x/p.mp3"},
                    {"voice_id": "v2", "name": "Mine", "category": "cloned"},
                    {"voice_id": "v3", "name": "Shared", "category": "professional"}
                ]})),
            )
        });
        write_local(
            &ctx,
            json!({"id": "base", "name": "Base", "kind": "base", "verified": true}),
        );
        write_local(
            &ctx,
            json!({"id": "vr-a", "name": "Adrian", "kind": "imported", "prompt_file": "vr-a.pb", "verified": false, "provenance": {"origin": "voiceright"}}),
        );
        std::fs::write(local_dir(&ctx).join("broken.json"), "not json").unwrap();
        std::fs::write(local_dir(&ctx).join("vr-a.pb"), b"p").unwrap();
        let r = list_with(&ctx, &base, &json!({})).unwrap();
        let v = r["voices"].as_array().unwrap();
        let by = |id: &str| v.iter().find(|x| x["id"] == id).unwrap().clone();
        assert_eq!(by("v1")["source"], "api");
        assert_eq!(by("v1")["free_plan_usable"], true);
        assert_eq!(by("v2")["deletable"], true);
        assert_eq!(by("v3")["free_plan_usable"], false);
        assert_eq!(by("base")["deletable"], false);
        assert_eq!(by("vr-a")["verified"], false);
        assert_eq!(by("vr-a")["prompt_present"], true);
        assert!(r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "bad_local_record"));
        // Default falls to the first usable voice when George is not in the list.
        assert_eq!(r["defaults"]["elevenlabs"], "v1");
        let head = seen.lock().unwrap()[0].3.to_ascii_lowercase();
        assert!(head.contains(&format!("xi-api-key: {KEY}").to_ascii_lowercase()));
        // Provider filter.
        let only = list_with(&ctx, &base, &json!({"provider": "local"})).unwrap();
        assert!(only["voices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["provider"] == "local"));
    }

    #[test]
    fn list_api_failure_falls_back_with_warning_and_never_leaks_key() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, _) =
            mock(|_, _, _| (401, j(json!({"detail": {"message": format!("bad {KEY}")}}))));
        let r = list_with(&ctx, &base, &json!({"provider": "elevenlabs"})).unwrap();
        assert_eq!(r["voices"][0]["source"], "builtin");
        assert_eq!(r["warnings"][0]["code"], "auth");
        assert!(!r.to_string().contains(KEY));
    }

    // ---- defaults and delete

    #[test]
    fn set_default_validates_persists_and_follows_into_list() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        assert_eq!(
            set_default(&ctx, &json!({"provider": "x", "voice_id": "a"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            set_default(
                &ctx,
                &json!({"provider": "wavespeed", "voice_id": "Alicia"})
            )
            .unwrap_err()
            .code,
            "invalid_input"
        );
        assert_eq!(
            set_default(&ctx, &json!({"provider": "local", "voice_id": "nope"}))
                .unwrap_err()
                .code,
            "voice_not_found"
        );
        assert_eq!(
            set_default(&ctx, &json!({"provider": "elevenlabs", "voice_id": "../x"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        set_default(&ctx, &json!({"provider": "wavespeed", "model": "minimax-speech-02-hd", "voice_id": "Deep_Voice_Man"})).unwrap();
        write_local(&ctx, json!({"id": "vr-a", "name": "A", "kind": "imported"}));
        set_default(&ctx, &json!({"provider": "local", "voice_id": "vr-a"})).unwrap();
        let r = list_with(&ctx, "http://127.0.0.1:1", &json!({"offline": true})).unwrap();
        assert_eq!(
            r["defaults"]["wavespeed/minimax-speech-02-hd"],
            "Deep_Voice_Man"
        );
        assert_eq!(r["defaults"]["wavespeed/elevenlabs-turbo-v2.5"], "Alicia");
        assert_eq!(r["defaults"]["local"], "vr-a");
        assert!(
            r["warnings"].as_array().unwrap().is_empty(),
            "offline skips the network"
        );
    }

    #[test]
    fn delete_local_removes_files_clears_default_and_protects_base() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        write_local(&ctx, json!({"id": "base", "name": "Base", "kind": "base"}));
        write_local(
            &ctx,
            json!({"id": "vr-a", "name": "A", "kind": "imported", "prompt_file": "vr-a.pb"}),
        );
        std::fs::write(local_dir(&ctx).join("vr-a.pb"), b"p").unwrap();
        set_default(&ctx, &json!({"provider": "local", "voice_id": "vr-a"})).unwrap();
        assert_eq!(
            delete_with(&ctx, "http://x", &json!({"id": "base"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            delete_with(&ctx, "http://x", &json!({"id": "../vr-a"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        delete_with(&ctx, "http://x", &json!({"id": "vr-a"})).unwrap();
        assert!(
            !local_dir(&ctx).join("vr-a.pb").exists()
                && !local_dir(&ctx).join("vr-a.json").exists()
        );
        assert!(stored_defaults(&ctx).get("local").is_none());
        assert_eq!(
            delete_with(&ctx, "http://x", &json!({"id": "vr-a"}))
                .unwrap_err()
                .code,
            "voice_not_found"
        );
    }

    #[test]
    fn delete_elevenlabs_only_deletes_clones() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, seen) = mock(|m, p, _| match (m, p) {
            ("GET", "/v1/voices/cl1") => (200, j(json!({"voice_id": "cl1", "category": "cloned"}))),
            ("GET", "/v1/voices/pm1") => {
                (200, j(json!({"voice_id": "pm1", "category": "premade"})))
            }
            ("DELETE", "/v1/voices/cl1") => (200, j(json!({"status": "ok"}))),
            _ => (404, j(json!({}))),
        });
        let e =
            delete_with(&ctx, &base, &json!({"id": "pm1", "source": "elevenlabs"})).unwrap_err();
        assert_eq!(e.code, "not_a_clone");
        delete_with(&ctx, &base, &json!({"id": "cl1", "source": "elevenlabs"})).unwrap();
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| format!("{} {}", s.0, s.1))
            .collect();
        assert_eq!(
            methods,
            [
                "GET /v1/voices/pm1",
                "GET /v1/voices/cl1",
                "DELETE /v1/voices/cl1"
            ]
        );
    }

    // ---- VoiceRight import

    fn voiceright_fixture(root: &Path) -> PathBuf {
        let dir = root
            .join("com.rightsuite.voiceright")
            .join("engine")
            .join("voices");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("voice-1.pb"), b"prompt-bytes-1").unwrap();
        std::fs::write(dir.join("voice-1.name"), "  Adrian \n").unwrap();
        std::fs::write(dir.join("voice-2.pb"), b"prompt-bytes-2").unwrap(); // no name file
        std::fs::write(dir.join("empty.pb"), b"").unwrap();
        std::fs::write(dir.join("bad.id.pb"), b"prompt-bytes-3").unwrap(); // slugs to bad-id
        std::fs::write(dir.join("!.pb"), b"x").unwrap();
        dir
    }

    #[test]
    fn import_copies_prompts_records_provenance_and_marks_unverified() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        let vr = voiceright_fixture(&d.0.join("appdata"));
        let r = import_from(&ctx, &json!({}), &[vr.clone()]).unwrap();
        assert_eq!(r["source_found"], true);
        assert_eq!(r["found"], 5);
        let imported = r["imported"].as_array().unwrap();
        assert_eq!(imported.len(), 3);
        assert!(imported.iter().all(|v| v["verified"] == false));
        let skipped: Vec<&str> = r["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["reason"].as_str().unwrap())
            .collect();
        assert!(skipped.contains(&"empty_prompt") && skipped.contains(&"invalid_id"));
        assert_eq!(r["verification"], "pending");

        // Dotted source names import under a slugged id with the raw stem in provenance.
        let slugged = read_json(&local_dir(&ctx).join("vr-bad-id.json")).unwrap();
        assert_eq!(slugged["provenance"]["source_id"], "bad.id");
        assert_eq!(
            std::fs::read(local_dir(&ctx).join("vr-voice-1.pb")).unwrap(),
            b"prompt-bytes-1"
        );
        let rec = read_json(&local_dir(&ctx).join("vr-voice-1.json")).unwrap();
        assert_eq!(rec["name"], "Adrian");
        assert_eq!(rec["kind"], "imported");
        assert_eq!(rec["verified"], false);
        assert_eq!(rec["provenance"]["origin"], "voiceright");
        assert_eq!(
            rec["provenance"]["source_sha256"],
            library::sha256_hex(b"prompt-bytes-1")
        );
        assert_eq!(
            read_json(&local_dir(&ctx).join("vr-voice-2.json")).unwrap()["name"],
            "voice-2"
        );
        // Originals untouched.
        assert!(vr.join("voice-1.pb").is_file());

        // Re-import is idempotent; a changed prompt re-imports.
        let again = import_from(&ctx, &json!({}), &[vr.clone()]).unwrap();
        assert_eq!(again["imported"].as_array().unwrap().len(), 0);
        assert!(again["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"] == "already_imported"));
        std::fs::write(vr.join("voice-1.pb"), b"changed").unwrap();
        let third = import_from(&ctx, &json!({}), &[vr]).unwrap();
        assert_eq!(third["imported"].as_array().unwrap().len(), 1);

        // They appear in the library, unverified.
        let l = list_with(&ctx, "http://x", &json!({"provider": "local"})).unwrap();
        assert_eq!(l["voices"].as_array().unwrap().len(), 3);
        assert!(l["voices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["verified"] == false && v["prompt_present"] == true));
    }

    #[test]
    fn import_accepts_app_data_root_and_reports_missing_source() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        let none = import_from(&ctx, &json!({}), &[d.0.join("nowhere")]).unwrap();
        assert_eq!(none["source_found"], false);
        assert_eq!(none["found"], 0);
        voiceright_fixture(&d.0.join("appdata"));
        let root = d.0.join("appdata").join("com.rightsuite.voiceright");
        let r = import_from(
            &ctx,
            &json!({"source_dir": root.display().to_string()}),
            &[],
        )
        .unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 3);
        assert_eq!(
            import_from(&ctx, &json!({"source_dir": "relative/x"}), &[])
                .unwrap_err()
                .code,
            "invalid_input"
        );
    }

    #[test]
    fn verify_reports_missing_prompt_and_pending_without_models() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        write_local(
            &ctx,
            json!({"id": "vr-a", "name": "A", "kind": "imported", "verified": false}),
        );
        assert_eq!(
            verify(&ctx, &json!({"id": "vr-a"})).unwrap_err().code,
            "voice_prompt_missing"
        );
        // A record with a prompt file reaches the real QTS synth check; the
        // outcome depends on this machine's install, but the record always
        // reflects it: verified only when the check passed.
        std::fs::write(local_dir(&ctx).join("vr-a.pb"), b"prompt").unwrap();
        write_local(
            &ctx,
            json!({"id": "vr-b", "name": "B", "kind": "imported", "verified": false, "prompt_file": "vr-a.pb"}),
        );
        let r = verify(&ctx, &json!({"id": "vr-b"}));
        let rec = read_json(&local_dir(&ctx).join("vr-b.json")).unwrap();
        assert_eq!(rec["verified"].as_bool().unwrap(), r.is_ok());
        assert_eq!(
            verify(&ctx, &json!({"id": "zzz"})).unwrap_err().code,
            "voice_not_found"
        );
        assert_eq!(
            read_json(&local_dir(&ctx).join("vr-a.json")).unwrap()["verified"],
            false
        );
    }

    // ---- tier and clone

    fn tier_body(can: bool) -> Vec<u8> {
        j(
            json!({"subscription": {"tier": "starter", "status": "active", "can_use_instant_voice_cloning": can,
            "character_count": 10, "character_limit": 1000, "voice_limit": 10}}),
        )
    }

    #[test]
    fn tier_reports_cloning_ability() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, _) = mock(|_, p, _| {
            assert_eq!(p, "/v1/user");
            (200, tier_body(true))
        });
        let t = tier_with(&ctx, &base).unwrap();
        assert_eq!(
            (
                t["tier"].as_str(),
                t["can_use_instant_voice_cloning"].as_bool()
            ),
            (Some("starter"), Some(true))
        );
        let (base, _) = mock(|_, _, _| (200, j(json!({}))));
        assert_eq!(
            tier_with(&ctx, &base).unwrap()["can_use_instant_voice_cloning"],
            false
        );
        let (base, _) = mock(|_, _, _| (401, j(json!({"detail": "missing user_read"}))));
        let e = tier_with(&ctx, &base).unwrap_err();
        assert_eq!(e.code, "auth");
        assert!(e.message.contains("user_read"));
        assert_eq!(
            tier_with(&ctx_with(&d, None), &base).unwrap_err().code,
            "no_credentials"
        );
    }

    fn wav_file(dir: &Dir) -> String {
        let p = dir.0.join("ref.wav");
        std::fs::write(&p, b"RIFF....WAVEfmt fake-audio").unwrap();
        p.display().to_string()
    }

    #[test]
    fn clone_requires_consent_before_anything_else() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, seen) = mock(|_, _, _| (500, j(json!({}))));
        for consent in [
            json!({}),
            json!({"consent": false}),
            json!({"consent": "yes"}),
        ] {
            let mut input = json!({"name": "Me", "audio_path": wav_file(&d)});
            input
                .as_object_mut()
                .unwrap()
                .extend(consent.as_object().unwrap().clone());
            assert_eq!(
                clone_with(&ctx, &base, &input).unwrap_err().code,
                "consent_required"
            );
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "no network call without consent"
        );
    }

    #[test]
    fn clone_is_refused_when_tier_disallows() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, seen) = mock(|_, p, _| match p {
            "/v1/user" => (200, tier_body(false)),
            _ => (500, j(json!({}))),
        });
        let e = clone_with(
            &ctx,
            &base,
            &json!({"name": "Me", "audio_path": wav_file(&d), "consent": true}),
        )
        .unwrap_err();
        assert_eq!(e.code, "clone_not_allowed");
        assert!(e.message.contains("starter"));
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "only the tier check ran; nothing uploaded"
        );
        // Tier cannot be confirmed (no user_read): also refused.
        let (base, _) = mock(|_, _, _| (401, j(json!({}))));
        let e = clone_with(
            &ctx,
            &base,
            &json!({"name": "Me", "audio_path": wav_file(&d), "consent": true}),
        )
        .unwrap_err();
        assert_eq!(e.code, "clone_not_allowed");
    }

    #[test]
    fn clone_uploads_multipart_and_logs_consent() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (base, seen) = mock(|m, p, _| match (m, p) {
            ("GET", "/v1/user") => (200, tier_body(true)),
            ("POST", "/v1/voices/add") => (
                200,
                j(json!({"voice_id": "newvoice1", "requires_verification": false})),
            ),
            _ => (404, j(json!({}))),
        });
        let r = clone_with(&ctx, &base, &json!({"name": "Adrian", "audio_path": wav_file(&d), "consent": true, "description": "mine"})).unwrap();
        assert_eq!(
            (r["voice_id"].as_str(), r["name"].as_str()),
            (Some("newvoice1"), Some("Adrian"))
        );
        let calls = seen.lock().unwrap().clone();
        let (_, path, body, head) = &calls[1];
        assert_eq!(path, "/v1/voices/add");
        let h = head.to_ascii_lowercase();
        assert!(h.contains("multipart/form-data; boundary=") && h.contains("xi-api-key"));
        let text = String::from_utf8_lossy(body);
        assert!(
            text.contains("name=\"name\"\r\n\r\nAdrian")
                && text.contains("name=\"description\"\r\n\r\nmine")
        );
        assert!(
            text.contains("name=\"files\"; filename=\"ref.wav\"")
                && text.contains("Content-Type: audio/wav")
                && text.contains("fake-audio")
        );
        let log = read_json(&voices_dir(&ctx).join("clone-consent.json")).unwrap();
        assert_eq!(log[0]["voice_id"], "newvoice1");
        assert_eq!(log[0]["consent"], true);
        assert!(!log.to_string().contains(KEY));
    }

    #[test]
    fn clone_reports_the_cost_notice_instead_of_a_silent_zero() {
        let d = Dir::new();
        // Dry run: validated, notice returned, no key lookup and no network (the base is unreachable).
        let input =
            json!({"name": "Adrian", "audio_path": wav_file(&d), "consent": true, "dry_run": true});
        let r = clone_with(&ctx_with(&d, None), "http://x", &input).unwrap();
        assert_eq!(
            (r["dry_run"].as_bool(), r["cost_notice"].as_str()),
            (Some(true), Some(CLONE_COST_NOTICE))
        );
        assert!(r["est_usd"].is_null(), "no invented amount");
        assert_eq!(
            clone_with(
                &ctx_with(&d, None),
                "http://x",
                &json!({"name": "x", "dry_run": true})
            )
            .unwrap_err()
            .code,
            "consent_required"
        );
        // The real result carries the same notice and still needs no confirm flag.
        let ctx = ctx_with(&d, Some(KEY));
        let (base, _) = mock(|m, p, _| match (m, p) {
            ("GET", "/v1/user") => (200, tier_body(true)),
            ("POST", "/v1/voices/add") => (200, j(json!({"voice_id": "newvoice2"}))),
            _ => (404, j(json!({}))),
        });
        let r = clone_with(
            &ctx,
            &base,
            &json!({"name": "Adrian", "audio_path": wav_file(&d), "consent": true}),
        )
        .unwrap();
        assert_eq!(r["cost_notice"].as_str(), Some(CLONE_COST_NOTICE));
        assert!(r["est_usd"].is_null());
    }

    #[test]
    fn clone_validates_input_and_maps_payment_errors() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let ok = |extra: Value| {
            let mut v = json!({"name": "Me", "audio_path": wav_file(&d), "consent": true});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            v
        };
        assert_eq!(
            clone_with(&ctx, "http://x", &ok(json!({"name": "  "})))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            clone_with(&ctx, "http://x", &ok(json!({"audio_path": "/nope/a.wav"})))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            clone_with(&ctx, "http://x", &ok(json!({"audio_path": "/etc/hosts"})))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            clone_with(&ctx_with(&d, None), "http://x", &ok(json!({})))
                .unwrap_err()
                .code,
            "no_credentials"
        );
        let (base, _) = mock(|_, p, _| match p {
            "/v1/user" => (200, tier_body(true)),
            _ => (
                402,
                j(json!({"detail": {"message": format!("upgrade your subscription {KEY}")}})),
            ),
        });
        let e = clone_with(&ctx, &base, &ok(json!({}))).unwrap_err();
        assert_eq!(e.code, "clone_not_allowed");
        assert!(!e.message.contains(KEY));
    }

    // ---- batch

    static MODELS: [ModelInfo; 1] = [ModelInfo {
        provider: "fake",
        model: "fake-voice",
        kind: Kind::Voice,
        label: "fake",
        est_usd: 0.01,
        durations: Durations::NONE,
    }];

    struct FakeVoice(Arc<Mutex<Vec<(String, Value)>>>);
    impl Provider for FakeVoice {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn models(&self) -> &'static [ModelInfo] {
            &MODELS
        }
        fn estimate_usd(&self, r: &GenRequest) -> Result<f64, ProviderError> {
            Ok(r.prompt.chars().count() as f64 / 1000.0)
        }
        fn submit(&self, r: &GenRequest, key: &str) -> Result<SubmitOutcome, ProviderError> {
            assert_eq!(key, "k");
            self.0
                .lock()
                .unwrap()
                .push((r.prompt.clone(), r.params.clone()));
            if r.prompt.contains("FAIL") {
                return Err(ProviderError::new("rejected", "no thanks", false));
            }
            let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
            bytes.extend_from_slice(r.prompt.as_bytes());
            Ok(SubmitOutcome::Completed {
                outputs: vec![Output::Bytes {
                    bytes,
                    mime: "audio/mpeg".into(),
                }],
            })
        }
        fn poll(&self, _: &GenRequest, _: &str, _: &str) -> Result<PollOutcome, ProviderError> {
            Err(ProviderError::new("unsupported", "sync", false))
        }
        fn test_key(&self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn fetch(&self, _: &Output) -> Result<(Vec<u8>, String), ProviderError> {
            Err(ProviderError::new("nope", "no network", false))
        }
    }

    static QTS_MODELS: [ModelInfo; 1] = [ModelInfo {
        provider: "local-qts",
        model: "qwen3-tts-0.6b-q8_0",
        kind: Kind::Voice,
        label: "fake local voice",
        est_usd: 0.0,
        durations: Durations::NONE,
    }];

    /// Stand-in for the local-qts lane: keyless, records voice_prompt params.
    struct FakeQts(Arc<Mutex<Vec<(String, Value)>>>);
    impl Provider for FakeQts {
        fn id(&self) -> &'static str {
            "local-qts"
        }
        fn models(&self) -> &'static [ModelInfo] {
            &QTS_MODELS
        }
        fn needs_key(&self) -> bool {
            false
        }
        fn estimate_usd(&self, _r: &GenRequest) -> Result<f64, ProviderError> {
            Ok(0.0)
        }
        fn submit(&self, r: &GenRequest, key: &str) -> Result<SubmitOutcome, ProviderError> {
            assert_eq!(key, "");
            self.0
                .lock()
                .unwrap()
                .push((r.prompt.clone(), r.params.clone()));
            let mut bytes = b"RIFF".to_vec();
            bytes.extend_from_slice(r.prompt.as_bytes());
            Ok(SubmitOutcome::Completed {
                outputs: vec![Output::Bytes {
                    bytes,
                    mime: "audio/wav".into(),
                }],
            })
        }
        fn poll(&self, _: &GenRequest, _: &str, _: &str) -> Result<PollOutcome, ProviderError> {
            Err(ProviderError::new("unsupported", "sync", false))
        }
        fn test_key(&self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn fetch(&self, _: &Output) -> Result<(Vec<u8>, String), ProviderError> {
            Err(ProviderError::new("nope", "no network", false))
        }
    }

    fn fake() -> (
        Vec<Box<dyn Provider>>,
        Arc<Mutex<Vec<(String, Value)>>>,
        Arc<Mutex<Vec<(String, Value)>>>,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let qts_seen = Arc::new(Mutex::new(Vec::new()));
        (
            vec![
                Box::new(FakeVoice(seen.clone())),
                Box::new(FakeQts(qts_seen.clone())),
            ],
            seen,
            qts_seen,
        )
    }

    fn bin(extra: Value) -> Value {
        let mut v = json!({"provider": "fake", "model": "fake-voice", "voice_id": "voiceABC"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn batch_makes_one_file_per_non_empty_line() {
        allow_pro();
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (provs, seen, _qts_seen) = fake();
        let out = d.0.join("out");
        let input = bin(
            json!({"text": "first line\n\n   \nsecond line\r\nthird line\n", "output_dir": out.display().to_string(), "batch_id": "t1"}),
        );
        let st = batch_with(&ctx, &provs, &input, RunMode::Inline).unwrap();
        assert_eq!(
            (
                st["total"].as_u64(),
                st["completed"].as_u64(),
                st["failed"].as_u64()
            ),
            (Some(3), Some(3), Some(0))
        );
        assert_eq!(st["finished"], true);
        let mut files: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, ["line-001.mp3", "line-002.mp3", "line-003.mp3"]);
        assert!(std::fs::read(out.join("line-002.mp3"))
            .unwrap()
            .ends_with(b"second line"));
        // voice_id reached the provider; one request per line with its own request_id.
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|(_, p)| p["voice_id"] == "voiceABC"));
        let ids: Vec<String> = st["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["request_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "voices-batch-t1-0001",
                "voices-batch-t1-0002",
                "voices-batch-t1-0003"
            ]
        );
        // Status is repeatable and re-running does not duplicate work.
        let again = batch_status(&ctx, &json!({"batch_id": "t1"})).unwrap();
        assert_eq!(again["completed"], 3);
        let re = batch_with(&ctx, &provs, &input, RunMode::Inline).unwrap();
        assert_eq!(re["completed"], 3);
        assert_eq!(
            seen.lock().unwrap().len(),
            3,
            "same request_ids dedupe to the same jobs"
        );
    }

    #[test]
    fn batch_reports_failures_per_line_and_progress_while_queued() {
        allow_pro();
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (provs, _, _) = fake();
        let st = batch_with(&ctx, &provs, &bin(json!({"lines": ["ok one", "FAIL here", "ok two"], "output_dir": d.0.join("o").display().to_string()})), RunMode::Inline).unwrap();
        assert_eq!(
            (st["completed"].as_u64(), st["failed"].as_u64()),
            (Some(2), Some(1))
        );
        let bad = &st["lines"][1];
        assert_eq!(bad["status"], "failed");
        assert_eq!(bad["error"]["code"], "rejected");
        assert!(bad["file"].is_null());
        // Submit-only: lines are queued with progress available, nothing copied yet.
        let q = batch_with(
            &ctx,
            &provs,
            &bin(json!({"lines": ["later"], "output_dir": d.0.join("q").display().to_string()})),
            RunMode::None,
        )
        .unwrap();
        assert_eq!(
            (q["running"].as_u64(), q["finished"].clone()),
            (Some(1), json!(false))
        );
        assert!(!d.0.join("q").join("line-001.mp3").exists());
        let id = q["batch_id"].as_str().unwrap().to_string();
        run_batch(&ctx, &provs, &id);
        assert_eq!(
            batch_status(&ctx, &json!({"batch_id": id})).unwrap()["completed"],
            1
        );
    }

    #[test]
    fn batch_validation_dry_run_and_defaults() {
        allow_pro();
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (provs, seen, _qts_seen) = fake();
        let e = |v: Value| {
            batch_with(&ctx, &provs, &v, RunMode::None)
                .unwrap_err()
                .code
        };
        assert_eq!(e(bin(json!({"text": " \n \n"}))), "invalid_input");
        assert_eq!(e(bin(json!({"lines": vec!["x"; 501]}))), "too_many_lines");
        assert_eq!(
            e(bin(json!({"text": "hi", "voice_id": "a b"}))),
            "invalid_input"
        );
        assert_eq!(
            e(bin(json!({"text": "hi", "output_dir": "relative"}))),
            "invalid_input"
        );
        assert_eq!(
            e(bin(json!({"text": "hi", "output_dir": "/tmp/../etc"}))),
            "invalid_input"
        );
        assert_eq!(
            e(bin(
                json!({"text": "hi", "provider": "local", "voice_id": "ghost"})
            )),
            "voice_not_found"
        );
        assert_eq!(
            e(bin(json!({"text": "hi", "model": "nope"}))),
            "unknown_model"
        );
        assert_eq!(
            e(json!({"text": "hi", "provider": "elevenlabs", "model": "eleven_v4"})),
            "unknown_provider"
        );
        let dry = batch_with(
            &ctx,
            &provs,
            &bin(json!({"lines": ["a".repeat(1000), "b".repeat(500)], "dry_run": true})),
            RunMode::Inline,
        )
        .unwrap();
        assert_eq!(dry["lines"], 2);
        assert!((dry["est_usd"].as_f64().unwrap() - 1.5).abs() < 1e-9);
        assert!(seen.lock().unwrap().is_empty() && !batches_dir(&ctx).exists());
        // No credentials: aborts before any job exists.
        let nokey = ctx_with(&d, None);
        let err =
            batch_with(&nokey, &provs, &bin(json!({"text": "hi"})), RunMode::None).unwrap_err();
        assert_eq!(err.code, "no_credentials");
        assert!(!batches_dir(&nokey).exists());
        assert_eq!(
            batch_status(&ctx, &json!({"batch_id": "ghost"}))
                .unwrap_err()
                .code,
            "batch_not_found"
        );
    }

    #[test]
    fn batch_local_voices_run_through_local_qts_with_voice_prompt() {
        let d = Dir::new();
        let ctx = ctx_with(&d, Some(KEY));
        let (provs, _seen, qts_seen) = fake();
        write_local(
            &ctx,
            json!({"id": "vr-1", "name": "Imported", "kind": "imported", "prompt_file": "vr-1.pb"}),
        );
        std::fs::write(local_dir(&ctx).join("vr-1.pb"), b"prompt").unwrap();
        let out = d.0.join("local-out");
        let st = batch_with(
            &ctx,
            &provs,
            &json!({"lines": ["hello there"], "provider": "local", "voice_id": "vr-1", "output_dir": out.display().to_string(), "batch_id": "lq1"}),
            RunMode::Inline,
        )
        .unwrap();
        assert_eq!(
            (
                st["completed"].as_u64(),
                st["provider"].as_str(),
                st["model"].as_str()
            ),
            (Some(1), Some("local-qts"), Some("qwen3-tts-0.6b-q8_0"))
        );
        assert!(out.join("line-001.wav").is_file());
        let calls = qts_seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["voice_prompt"], "vr-1.pb");
        assert!(
            calls[0].1.get("voice_id").is_none(),
            "local voices pass a prompt file, not a voice_id"
        );
        // Missing prompt file is a clear error before anything submits.
        write_local(
            &ctx,
            json!({"id": "vr-2", "name": "Broken", "kind": "imported"}),
        );
        let e = batch_with(
            &ctx,
            &provs,
            &json!({"lines": ["x"], "provider": "local", "voice_id": "vr-2"}),
            RunMode::None,
        )
        .unwrap_err();
        assert_eq!(e.code, "voice_prompt_missing");
    }

    // ---- local engine lifecycle (no models installed: gates only) ----

    #[test]
    fn rename_updates_local_voice_records() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        write_local(&ctx, json!({"id": "vr-a", "name": "A", "kind": "imported"}));
        assert_eq!(
            rename(&ctx, &json!({"id": "ghost", "name": "x"}))
                .unwrap_err()
                .code,
            "voice_not_found"
        );
        assert_eq!(
            rename(&ctx, &json!({"id": "vr-a", "name": "  "}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            rename(&ctx, &json!({"id": "../x", "name": "x"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        rename(&ctx, &json!({"id": "vr-a", "name": "Adrian V2"})).unwrap();
        assert_eq!(
            read_json(&local_dir(&ctx).join("vr-a.json")).unwrap()["name"],
            "Adrian V2"
        );
    }

    #[test]
    fn clone_local_gates_on_consent_input_and_models() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        let wav = d.0.join("ref.wav");
        std::fs::write(&wav, b"RIFFfake").unwrap();
        let e = |v: Value| clone_local(&ctx, &v).unwrap_err().code;
        assert_eq!(
            e(json!({"name": "n", "audio_path": wav})),
            "consent_required"
        );
        assert_eq!(
            e(json!({"name": " ", "audio_path": wav, "consent": true})),
            "invalid_input"
        );
        assert_eq!(
            e(json!({"name": "n", "audio_path": d.0.join("ref.txt"), "consent": true})),
            "invalid_input"
        );
        assert_eq!(
            e(json!({"name": "n", "audio_path": "/no/such.wav", "consent": true})),
            "invalid_input"
        );
        // Valid request stops at the model check (no weights in the test env).
        let code = e(json!({"name": "n", "audio_path": wav, "consent": true}));
        assert!(
            code == "not_installed" || code == "engine_load" || code == "worker_missing",
            "unexpected: {code}"
        );
    }

    #[test]
    fn preview_and_status_and_install_status_are_safe_offline() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        assert_eq!(
            preview(&ctx, &json!({"id": "ghost"})).unwrap_err().code,
            "voice_not_found"
        );
        let st = models_status(&ctx).unwrap();
        assert!(
            st["files"].as_array().unwrap().len() == 2
                && st["onnxruntime"].is_object()
                && st["worker"].is_object()
        );
        let ist = install_status(&ctx).unwrap();
        assert!(ist["state"].is_string());
    }

    #[test]
    fn import_accepts_qts_testdata_voice_prompts() {
        // The QTS fixtures carry real x-vector prompt files — the
        // same format VoiceRight wrote — so import treats them as voices.
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/e2e/fixtures/qts");
        let r = import_from(&ctx, &json!({"source_dir": src.display().to_string()}), &[]).unwrap();
        assert_eq!(r["source_found"], true);
        assert_eq!(r["imported"].as_array().unwrap().len(), 2);
        let rec =
            read_json(&local_dir(&ctx).join("vr-sample1-xvector-voice-clone-prompt.json")).unwrap();
        assert_eq!(rec["kind"], "imported");
        assert_eq!(rec["verified"], false);
        assert_eq!(
            rec["provenance"]["source_id"],
            "sample1.xvector.voice-clone-prompt"
        );
        assert!(local_dir(&ctx)
            .join("vr-sample1-xvector-voice-clone-prompt.pb")
            .is_file());
    }

    fn write_wav(path: &Path) {
        let n = 2400usize;
        let mut data = Vec::with_capacity(44 + n * 2);
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&((36 + n * 2) as u32).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&[1, 0, 1, 0]);
        data.extend_from_slice(&24000u32.to_le_bytes());
        data.extend_from_slice(&48000u32.to_le_bytes());
        data.extend_from_slice(&[2, 0, 16, 0]);
        data.extend_from_slice(b"data");
        data.extend_from_slice(&((n * 2) as u32).to_le_bytes());
        for i in 0..n {
            let s = (f32::sin(i as f32 * 440.0 / 24000.0 * std::f32::consts::TAU) * 8000.0) as i16;
            data.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }

    fn fake_batch(ctx: &Context, id: &str) -> PathBuf {
        let out = batches_dir(ctx).join(id);
        std::fs::create_dir_all(&out).unwrap();
        write_wav(&out.join("line-001.wav"));
        write_wav(&out.join("line-002.wav"));
        let manifest = json!({
            "batch_id": id, "created_at": store::now(), "provider": "local-qts",
            "model": "qwen3-tts-0.6b-q8_0", "output_dir": out.display().to_string(), "prefix": "line",
            "lines": [
                {"n":1,"text":"a","request_id":"r1","job_id":"j1","status":"complete","file":out.join("line-001.wav").display().to_string(),"error":null},
                {"n":2,"text":"b","request_id":"r2","job_id":"j2","status":"complete","file":out.join("line-002.wav").display().to_string(),"error":null},
                {"n":3,"text":"c","request_id":"r3","job_id":"j3","status":"failed","file":null,"error":{"code":"x","message":"y"}},
            ],
        });
        write_json(&manifest_path(ctx, id), &manifest).unwrap();
        out
    }

    #[test]
    fn export_batch_folder_and_zip_with_manifest() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        let out = fake_batch(&ctx, "bexp1");

        // Plain wav folder export needs no ffmpeg: files are copied + manifest.
        let r = export(&ctx, &json!({"batch_id": "bexp1"})).unwrap();
        assert_eq!(r["count"], 2);
        let dir = out.join("export-wav");
        assert!(dir.join("line-001.wav").is_file() && dir.join("line-002.wav").is_file());
        let m = read_json(&dir.join("manifest.json")).unwrap();
        assert_eq!(m["export"]["format"], "wav");
        // The live manifest records the last export.
        assert_eq!(
            read_json(&manifest_path(&ctx, "bexp1")).unwrap()["last_export"]["files"],
            2
        );

        // Zip export: PK signature, 2 wav + manifest.json members.
        let r = export(&ctx, &json!({"batch_id": "bexp1", "zip": true})).unwrap();
        let zp = PathBuf::from(r["zip"].as_str().unwrap());
        let z = std::fs::read(&zp).unwrap();
        assert_eq!(&z[..4], b"PK\x03\x04");
        assert_eq!(&z[z.len() - 22..z.len() - 18], b"PK\x05\x06");
        let contains = |needle: &[u8]| z.windows(needle.len()).any(|w| w == needle);
        assert!(
            contains(b"line-001.wav") && contains(b"line-002.wav") && contains(b"manifest.json")
        );
    }

    #[test]
    fn export_validates_input() {
        let d = Dir::new();
        let ctx = ctx_with(&d, None);
        assert_eq!(export(&ctx, &json!({})).unwrap_err().code, "invalid_input");
        assert_eq!(
            export(&ctx, &json!({"path": "/x", "format": "ogg"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            export(&ctx, &json!({"batch_id": "nope"})).unwrap_err().code,
            "batch_not_found"
        );
        assert_eq!(
            export(&ctx, &json!({"path": "relative/x.wav"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            export(
                &ctx,
                &json!({"path": d.0.join("missing.wav").display().to_string()})
            )
            .unwrap_err()
            .code,
            "file_not_found"
        );

        // Single-file wav copy without ffmpeg.
        let src = d.0.join("clip.wav");
        write_wav(&src);
        let r = export(&ctx, &json!({"path": src.display().to_string()})).unwrap();
        assert_eq!(r["count"], 1);
        let dest = PathBuf::from(r["files"][0]["path"].as_str().unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
    }
}
