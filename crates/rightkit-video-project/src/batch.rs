//! Durable batch delivery: `batch.create`, `batch.run`, `batch.status`, `batch.list`
//! and `batch.cover`. A batch is a group of independently qualified outputs: each is one
//! (timeline or saved variant, render preset) pair that is rendered, passed through QA and
//! given a cover frame on its own, so one failing output never holds back the others and an
//! interruption costs at most the output in flight.
//!
//! The state file `<dir>/exports/batches/<id>.json` (host-selected schema) is the only
//! memory. It is rewritten atomically after every state change, and `batch.run` decides
//! what to do from it plus the files it names, never from what a previous call intended:
//! * `qualified` and the render still hashes to the recorded BLAKE3: skipped, never re-rendered;
//! * `rendered` and the render still matches: QA only (a passing receipt for that exact file
//!   stands and is not rewritten);
//! * anything else (`pending`, `failed`, or a file that is missing or changed): rendered again.
//!
//! Looks are pinned: `batch.create` records a `finishing` snapshot (the settings that decide
//! the delivered picture and sound) and its BLAKE3. `batch.run` renders only while the
//! project's current settings still hash the same, so every output of a batch is delivered
//! with one look. A changed look is refused, never rendered; `"finishing": "current"` is the
//! deliberate choice to re-pin, which makes every output pending again.
//!
//! Renders are named `renders/batch-<id>-<key>.mp4`; covers live in
//! `exports/batches/<id>/covers/<key>.<ext>`. Nothing here edits a timeline, a render
//! record or a QA receipt of another output.
//!
//! `batch.run {dir, id, job?}` registers the run as job `job` (default `batch-<id>`) and each
//! render as `<job>-stage`, so `recipe.cancel {job}` stops the run and the render in flight.
//! Progress events are `{event: "batch.output", batch, key, stage: render|qa|cover|output,
//! state: running|done|failed|skipped|qualified, fraction, error?}`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::render::parse_preset;
use crate::{Access, Project, ProjectError};

/// An error with the existing engine command code and message.
#[derive(Debug)]
pub struct BatchError {
    pub code: String,
    pub message: String,
}

impl BatchError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn bad(message: impl Into<String>) -> Self {
        Self::new("bad_input", message)
    }
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BatchError {}

impl From<ProjectError> for BatchError {
    fn from(error: ProjectError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

/// An output job admitted by the batch, bound to the run's cancellation flag.
pub struct BatchRender<'a> {
    pub dir: &'a Path,
    pub variant: Option<&'a str>,
    pub preset: &'a str,
    pub out_name: &'a str,
    pub stage_job: &'a str,
    pub timeout_secs: Option<u64>,
    pub cancel: Arc<AtomicBool>,
}

/// A QA report and the immutable render digest and preset it qualified.
pub struct BatchQa {
    pub passed: bool,
    pub status: String,
    pub receipt: String,
    pub blocking: Vec<String>,
    pub render_blake3: String,
    pub preset: String,
}

/// Effective settings supplied by the host after applying its defaults.
pub struct BatchFinishing {
    pub settings: Map<String, Value>,
    pub keys: &'static [&'static str],
    pub caption_style: Option<String>,
    pub burn_captions: bool,
}

/// Integrations which use the engine's existing settings, render and review services.
pub trait BatchHost {
    type ActivityGuard;

    fn register_job(&self, job: &str)
        -> Result<(Self::ActivityGuard, Arc<AtomicBool>), BatchError>;
    fn recipe_ref(&self, dir: &Path) -> Result<Value, BatchError>;
    fn finishing(&self, dir: &Path) -> Result<BatchFinishing, BatchError>;
    fn saved_variants(&self, dir: &Path) -> Result<Vec<String>, BatchError>;
    fn validate_variant_name(&self, name: &str) -> Result<(), BatchError>;
    fn render(
        &self,
        request: BatchRender<'_>,
        progress: &mut dyn FnMut(&Value),
    ) -> Result<PathBuf, BatchError>;
    /// Read and verify persisted evidence before returning it.
    fn cached_qa(&self, dir: &Path, hash: &str) -> Option<BatchQa>;
    fn run_qa(
        &self,
        dir: &Path,
        variant: Option<&str>,
        output: &Path,
        preset: &str,
        progress: &mut dyn FnMut(&Value),
    ) -> Result<BatchQa, BatchError>;
    fn thumbnail(
        &self,
        dir: &Path,
        render_file: &str,
        at_ms: i64,
        width: u32,
        progress: &mut dyn FnMut(&Value),
    ) -> Result<PathBuf, BatchError>;
}

/// Serve method names handled here.
pub const METHODS: &[&str] = &[
    "batch.create",
    "batch.run",
    "batch.status",
    "batch.list",
    "batch.cover",
];

const COVER_WIDTH: u32 = 1280;
/// A render name is `batch-<id>-<variant>__<preset>` and `sanitize_out_name` allows 100
/// characters: 6 + id + 1 + 48 (longest variant name) + 2 + 6 (longest preset) leaves 37.
const MAX_ID_LEN: usize = 32;

/// Serialises state-file read-modify-write cycles inside this process, so `batch.cover`
/// during a `batch.run` of the same batch cannot lose either write.
static STATE_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum State {
    Pending,
    Rendered,
    Qualified,
    Failed,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Rendered => "rendered",
            State::Qualified => "qualified",
            State::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QaRef {
    status: String,
    /// Project-relative path of the QA receipt.
    receipt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cover {
    at_ms: i64,
    /// Project-relative path of the cover image.
    path: String,
    blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    /// `<variant or main>__<preset>`: stable, and the file stem of the render and cover.
    key: String,
    /// A saved variant name; `None` renders the main timeline.
    variant: Option<String>,
    preset: String,
    qa_preset: String,
    state: State,
    /// Project-relative path of the render.
    output: Option<String>,
    output_blake3: Option<String>,
    qa: Option<QaRef>,
    cover: Option<Cover>,
    error: Option<String>,
    attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    schema: String,
    id: String,
    created_at: String,
    timeline_cursor: u64,
    recipe_ref: Value,
    /// The finishing settings every output of this batch is rendered with (see
    /// [`finishing_snapshot`]). `None` on batches made before pinning existed.
    #[serde(default)]
    finishing: Option<Value>,
    /// Lowercase hex BLAKE3 of `finishing` as canonical JSON.
    #[serde(default)]
    finishing_blake3: Option<String>,
    outputs: Vec<Output>,
}

impl Batch {
    fn output(&self, key: &str) -> Result<&Output, BatchError> {
        self.outputs
            .iter()
            .find(|o| o.key == key)
            .ok_or_else(|| BatchError::bad(format!("batch {} has no output {key:?}", self.id)))
    }
}

/// What `batch.create` takes for one output.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputSpec {
    variant: Option<String>,
    preset: String,
}

// ------------------------------------------------------------------ helpers

fn internal(message: impl Into<String>) -> BatchError {
    BatchError::new("internal", message)
}

fn io(context: impl Into<String>, e: std::io::Error) -> BatchError {
    BatchError::new("io_error", format!("{}: {e}", context.into()))
}

/// Project-relative path with `/` separators; absolute when `path` is outside `dir`.
fn rel(dir: &Path, path: &Path) -> String {
    match path.strip_prefix(dir) {
        Ok(r) => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// True when `path` is a file whose BLAKE3 is `expected`.
fn file_matches(path: &Path, expected: &str) -> Result<bool, BatchError> {
    Ok(path.is_file()
        && rightkit_fs::digest::blake3_file(path)
            .map_err(|e| io(format!("hashing {}", path.display()), e))?
            == expected)
}

/// File extension of the image the bytes start as.
fn image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn valid_id(id: &str) -> Result<(), BatchError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.chars().enumerate().all(|(i, c)| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && (c == '-' || c == '_'))
        });
    if ok {
        Ok(())
    } else {
        Err(BatchError::bad(format!(
            "invalid batch id {id:?}: use 1 to {MAX_ID_LEN} lowercase letters, digits, '-' or '_', starting with a letter or digit"
        )))
    }
}

fn batches_dir(dir: &Path) -> PathBuf {
    dir.join("exports").join("batches")
}

fn state_path(dir: &Path, id: &str) -> PathBuf {
    batches_dir(dir).join(format!("{id}.json"))
}

fn covers_dir(dir: &Path, id: &str) -> PathBuf {
    batches_dir(dir).join(id).join("covers")
}

fn corrupt(path: &Path, detail: impl std::fmt::Display) -> BatchError {
    BatchError::new(
        "project_corrupt",
        format!("{} is not a valid batch: {detail}", path.display()),
    )
}

fn read_batch(schema: &str, path: &Path, id: &str) -> Result<Batch, BatchError> {
    let bytes = std::fs::read(path).map_err(|e| io(format!("reading {}", path.display()), e))?;
    let batch: Batch = serde_json::from_slice(&bytes).map_err(|e| corrupt(path, e))?;
    if batch.schema != schema {
        return Err(corrupt(path, format!("schema {:?}", batch.schema)));
    }
    if batch.id != id {
        return Err(corrupt(path, format!("it names id {:?}", batch.id)));
    }
    Ok(batch)
}

fn load(schema: &str, dir: &Path, id: &str) -> Result<Batch, BatchError> {
    valid_id(id)?;
    let path = state_path(dir, id);
    if !path.is_file() {
        return Err(BatchError::bad(format!("no batch {id:?} in this project")));
    }
    read_batch(schema, &path, id)
}

fn save(dir: &Path, batch: &Batch) -> Result<(), BatchError> {
    let path = state_path(dir, &batch.id);
    let mut bytes = serde_json::to_vec_pretty(batch)
        .map_err(|e| internal(format!("serialising batch {}: {e}", batch.id)))?;
    bytes.push(b'\n');
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(&bytes),
    )
    .map_err(|e| ProjectError::io(format!("writing {}", path.display()), e.into()))?;
    Ok(())
}

/// Reloads the state file, applies `change` to one output and writes it back, so a write
/// from another call (a cover chosen during a run) is never overwritten with stale data.
fn mutate(
    schema: &str,
    dir: &Path,
    id: &str,
    key: &str,
    change: impl FnOnce(&mut Output),
) -> Result<(), BatchError> {
    let _guard = lock();
    let mut batch = load(schema, dir, id)?;
    let output = batch
        .outputs
        .iter_mut()
        .find(|o| o.key == key)
        .ok_or_else(|| internal(format!("batch {id} lost output {key}")))?;
    change(output);
    save(dir, &batch)
}

fn emit(
    progress: &mut dyn FnMut(&Value),
    id: &str,
    key: &str,
    stage: &str,
    state: &str,
    fraction: f64,
    error: Option<&str>,
) {
    let mut event = json!({
        "event": "batch.output",
        "batch": id,
        "key": key,
        "stage": stage,
        "state": state,
        "fraction": fraction,
    });
    if let Some(error) = error {
        event["error"] = json!(error);
    }
    progress(&event);
}

fn record_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".render.json");
    PathBuf::from(name)
}

/// `duration_ms` from the render record next to a render.
fn render_duration_ms(output: &Path) -> Result<i64, BatchError> {
    let path = record_path(output);
    let bytes = std::fs::read(&path).map_err(|e| io(format!("reading {}", path.display()), e))?;
    let record: Value = serde_json::from_slice(&bytes)
        .map_err(|e| internal(format!("{} is not JSON: {e}", path.display())))?;
    record["duration_ms"]
        .as_i64()
        .ok_or_else(|| internal(format!("{} has no duration_ms", path.display())))
}

// ------------------------------------------------------------------ finishing pin

/// Canonical JSON: object keys sorted at every level, no whitespace, so the hash does not
/// depend on how a map happens to be ordered.
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                canonical_json(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn finishing_hash(snapshot: &Value) -> String {
    let mut text = String::new();
    canonical_json(snapshot, &mut text);
    rightkit_fs::digest::blake3_bytes(text.as_bytes())
}

fn finishing_snapshot(input: BatchFinishing) -> Value {
    let recipe = input.settings.get("recipe");
    let recipe_part = |key: &str| {
        recipe
            .and_then(|recipe| recipe.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut snapshot = Map::new();
    for &key in input.keys {
        snapshot.insert(
            key.to_string(),
            input.settings.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    snapshot.insert("recipe_finish".into(), recipe_part("finish"));
    snapshot.insert("recipe_captions".into(), recipe_part("captions"));
    snapshot.insert("caption_style_in_force".into(), json!(input.caption_style));
    snapshot.insert("burn_in_force".into(), json!(input.burn_captions));
    let lut = input
        .settings
        .get("lut_path")
        .and_then(Value::as_str)
        .or_else(|| {
            recipe
                .and_then(|recipe| recipe.get("finish"))
                .and_then(|finish| finish.get("lut_path"))
                .and_then(Value::as_str)
        })
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .filter(|path| path.is_file());
    snapshot.insert(
        "lut_blake3".into(),
        match lut {
            Some(path) => rightkit_fs::digest::blake3_file(path).map_or(Value::Null, Value::String),
            None => Value::Null,
        },
    );
    Value::Object(snapshot)
}

/// The current snapshot and its hash.
fn finishing_now(dir: &Path, host: &impl BatchHost) -> Result<(Value, String), BatchError> {
    let snapshot = finishing_snapshot(host.finishing(dir)?);
    let hash = finishing_hash(&snapshot);
    Ok((snapshot, hash))
}

/// Snapshot keys whose values differ between two snapshots (either may be absent).
fn differing_keys(pinned: Option<&Value>, current: &Value) -> Vec<String> {
    let empty = Map::new();
    let a = pinned.and_then(Value::as_object).unwrap_or(&empty);
    let b = current.as_object().unwrap_or(&empty);
    let mut keys: Vec<String> = a.keys().chain(b.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys.retain(|k| a.get(k).unwrap_or(&Value::Null) != b.get(k).unwrap_or(&Value::Null));
    keys
}

/// What `batch.run` does about the batch's pinned look.
enum Pin {
    /// Pinned and unchanged.
    Match,
    /// Older batch with no snapshot: pin the current settings, keep the outputs as they are.
    First,
    /// Changed, and the caller chose `"finishing": "current"`: pin again, redo every output.
    Repin(Vec<String>),
    /// Changed and not chosen: render nothing.
    Refuse(Vec<String>),
}

fn refusal(id: &str, differs: &[String]) -> BatchError {
    BatchError::bad(format!(
        "the project's finishing settings changed since batch {id:?} was created (differs: {}); nothing was rendered. Run batch.run with \"finishing\": \"current\" to pin the batch to the current settings (every output not yet delivered under them, including ones already qualified, is rendered again), or restore the settings and run again",
        differs.join(", ")
    ))
}

// ------------------------------------------------------------------ create

fn output_key(variant: Option<&str>, preset: &str) -> String {
    format!("{}__{preset}", variant.unwrap_or("main"))
}

fn create(
    schema: &str,
    dir: &Path,
    specs: Vec<OutputSpec>,
    id: Option<String>,
    host: &impl BatchHost,
    dry_run: bool,
) -> Result<Value, BatchError> {
    let project = Project::open(dir, Access::Read)?;
    if specs.is_empty() {
        return Err(BatchError::bad("a batch needs at least one output"));
    }
    let saved_variants = if specs.iter().any(|s| s.variant.is_some()) {
        host.saved_variants(dir)?
    } else {
        Vec::new()
    };
    let mut outputs: Vec<Output> = Vec::with_capacity(specs.len());
    for spec in specs {
        // The QA preset has the render preset's name: youtube, reels, tiktok and square.
        let preset = parse_preset(&spec.preset)?;
        if let Some(name) = &spec.variant {
            host.validate_variant_name(name)?;
            if !saved_variants.contains(name) {
                return Err(BatchError::bad(format!(
                    "no saved variant {name:?}; variants.list names the saved ones"
                )));
            }
        }
        let key = output_key(spec.variant.as_deref(), preset.name());
        if outputs.iter().any(|o| o.key == key) {
            return Err(BatchError::bad(format!(
                "output {key:?} is listed more than once"
            )));
        }
        outputs.push(Output {
            key,
            variant: spec.variant,
            preset: preset.name().to_string(),
            qa_preset: preset.name().to_string(),
            state: State::Pending,
            output: None,
            output_blake3: None,
            qa: None,
            cover: None,
            error: None,
            attempts: 0,
        });
    }
    let mut keys: Vec<&str> = outputs.iter().map(|o| o.key.as_str()).collect();
    keys.sort_unstable();
    // Without an id the batch is named by its outputs, so a retried create finds itself.
    let id = id.unwrap_or_else(|| {
        let hash = rightkit_fs::digest::blake3_bytes(keys.join("\n").as_bytes());
        format!("b-{}", &hash[..12])
    });
    valid_id(&id)?;

    let _guard = lock();
    let path = state_path(dir, &id);
    if path.is_file() {
        let existing = read_batch(schema, &path, &id)?;
        let mut existing_keys: Vec<&str> =
            existing.outputs.iter().map(|o| o.key.as_str()).collect();
        existing_keys.sort_unstable();
        if existing_keys != keys {
            return Err(BatchError::bad(format!(
                "conflict: batch {id:?} already exists with different outputs ({})",
                existing_keys.join(", ")
            )));
        }
        return Ok(json!({"status": "exists", "created": false, "id": id, "batch": existing}));
    }
    let recipe_ref = host.recipe_ref(dir)?;
    let (finishing, finishing_blake3) = finishing_now(dir, host)?;
    let batch = Batch {
        schema: schema.to_string(),
        id: id.clone(),
        created_at: now(),
        timeline_cursor: project.cursor() as u64,
        recipe_ref,
        finishing: Some(finishing),
        finishing_blake3: Some(finishing_blake3),
        outputs,
    };
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "batch.create",
                "id": id,
                "outputs": batch.outputs.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            },
        }));
    }
    save(dir, &batch)?;
    Ok(json!({"status": "created", "created": true, "id": id, "batch": batch}))
}

// ------------------------------------------------------------------ run

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Nothing to do: qualified, with its render and cover intact.
    Skip,
    /// Qualified render whose cover is missing or changed: make the cover only.
    Cover,
    /// Render intact: QA, then the cover.
    Qa,
    /// Render (again), then QA and the cover.
    Render,
}

fn classify(dir: &Path, o: &Output) -> Result<Step, BatchError> {
    let intact = match (&o.output, &o.output_blake3) {
        (Some(path), Some(hash)) => file_matches(&dir.join(path), hash)?,
        _ => false,
    };
    Ok(match (o.state, intact) {
        (State::Qualified, true) => match &o.cover {
            Some(c) if file_matches(&dir.join(&c.path), &c.blake3)? => Step::Skip,
            _ => Step::Cover,
        },
        (State::Rendered, true) => Step::Qa,
        _ => Step::Render,
    })
}

struct Ctx<'a, H: BatchHost> {
    schema: &'a str,
    dir: &'a Path,
    id: &'a str,
    host: &'a H,
    timeout_secs: Option<u64>,
    /// Job id of the render in flight; `recipe.cancel {job}` also cancels it.
    stage_job: String,
    cancel: Arc<AtomicBool>,
}

#[allow(clippy::too_many_arguments)]
fn run(
    schema: &str,
    dir: &Path,
    id: &str,
    job: Option<&str>,
    timeout_secs: Option<u64>,
    finishing: Option<&str>,
    host: &impl BatchHost,
    dry_run: bool,
    progress: &mut dyn FnMut(&Value),
) -> Result<Value, BatchError> {
    let repin = match finishing {
        None => false,
        Some("current") => true,
        Some(other) => {
            return Err(BatchError::bad(format!(
                "finishing must be \"current\" (or omitted), got {other:?}"
            )))
        }
    };
    let batch = load(schema, dir, id)?;
    // One look per batch: compare the settings in force with the ones the batch was
    // created under before anything renders.
    let (now_snapshot, now_hash) = finishing_now(dir, host)?;
    let pin = match (&batch.finishing_blake3, repin) {
        (None, _) => Pin::First,
        (Some(h), _) if *h == now_hash => Pin::Match,
        (Some(_), true) => Pin::Repin(differing_keys(batch.finishing.as_ref(), &now_snapshot)),
        (Some(_), false) => Pin::Refuse(differing_keys(batch.finishing.as_ref(), &now_snapshot)),
    };
    let pin_reply = |action: &str, differs: &[String]| {
        json!({
            "action": action,
            "finishing_blake3": now_hash,
            "previous_finishing_blake3": batch.finishing_blake3,
            "differs": differs,
        })
    };
    if let Pin::Refuse(differs) = &pin {
        if !dry_run {
            return Err(refusal(id, differs));
        }
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "batch.run",
                "id": id,
                "finishing": pin_reply("refuse", differs),
                "render": [],
                "qa": [],
                "cover": [],
                "skip": [],
            },
        }));
    }
    let steps = if matches!(pin, Pin::Repin(_)) {
        vec![Step::Render; batch.outputs.len()]
    } else {
        batch
            .outputs
            .iter()
            .map(|o| classify(dir, o))
            .collect::<Result<Vec<_>, _>>()?
    };
    // A main-timeline output renders the timeline as it is now. If the edit moved since
    // the batch was made, the batch would mix two edits; the caller makes a new batch.
    let cursor = Project::open(dir, Access::Read)?.cursor() as u64;
    let moved = batch
        .outputs
        .iter()
        .zip(&steps)
        .any(|(o, s)| o.variant.is_none() && *s == Step::Render);
    if moved && cursor != batch.timeline_cursor {
        return Err(BatchError::bad(format!(
            "the timeline moved from revision {} to {cursor} since batch {id:?} was created; create a new batch for the current edit",
            batch.timeline_cursor
        )));
    }
    let keys_of = |wanted: &[Step]| -> Vec<&str> {
        batch
            .outputs
            .iter()
            .zip(&steps)
            .filter(|(_, s)| wanted.contains(s))
            .map(|(o, _)| o.key.as_str())
            .collect()
    };
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "batch.run",
                "id": id,
                "finishing": match &pin {
                    Pin::Match => pin_reply("match", &[]),
                    Pin::First => pin_reply("pin", &[]),
                    Pin::Repin(d) => pin_reply("repin", d),
                    Pin::Refuse(_) => unreachable!("refusal returned above"),
                },
                "render": keys_of(&[Step::Render]),
                "qa": keys_of(&[Step::Qa]),
                "cover": keys_of(&[Step::Cover]),
                "skip": keys_of(&[Step::Skip]),
            },
        }));
    }

    let run_job = job.map_or_else(|| format!("batch-{id}"), String::from);
    let (_guard, cancel) = host.register_job(&run_job)?;
    // Record the pin before any render, so an interruption resumes under the same look.
    if matches!(pin, Pin::First | Pin::Repin(_)) {
        let _guard = lock();
        let mut fresh = load(schema, dir, id)?;
        fresh.finishing = Some(now_snapshot.clone());
        fresh.finishing_blake3 = Some(now_hash.clone());
        if matches!(pin, Pin::Repin(_)) {
            for o in &mut fresh.outputs {
                o.state = State::Pending;
                o.output = None;
                o.output_blake3 = None;
                o.qa = None;
                o.cover = None;
                o.error = None;
            }
        }
        save(dir, &fresh)?;
    }
    let ctx = Ctx {
        schema,
        dir,
        id,
        host,
        timeout_secs,
        stage_job: format!("{run_job}-stage"),
        cancel: Arc::clone(&cancel),
    };
    let total = batch.outputs.len().max(1) as f64;
    let mut ran: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut covered: Vec<String> = Vec::new();
    for (index, (o, step)) in batch.outputs.iter().zip(&steps).enumerate() {
        let key = o.key.as_str();
        let start = index as f64 / total;
        if cancel.load(Ordering::SeqCst) {
            emit(
                progress,
                id,
                key,
                "output",
                "failed",
                start,
                Some("cancelled"),
            );
            return Err(ProjectError::RenderCancelled.into());
        }
        if *step == Step::Skip {
            skipped.push(o.key.clone());
            emit(progress, id, key, "output", "skipped", start, None);
            continue;
        }
        if *step == Step::Render {
            // The settings may have been edited while this run was going; never mix looks.
            let (latest, latest_hash) = finishing_now(dir, host)?;
            if latest_hash != now_hash {
                let differs = differing_keys(Some(&now_snapshot), &latest);
                return Err(BatchError::bad(format!(
                    "the project's finishing settings changed while batch {id:?} was running (differs: {}); outputs already delivered are kept and {key:?} was not rendered. Restore the settings and run again, or run with \"finishing\": \"current\" to re-pin",
                    differs.join(", ")
                )));
            }
        }
        match process(&ctx, o, *step, start, progress) {
            Ok(rendered) => {
                if rendered {
                    ran.push(o.key.clone());
                } else if *step == Step::Cover {
                    covered.push(o.key.clone());
                }
            }
            Err(e) => {
                if cancel.load(Ordering::SeqCst) {
                    emit(
                        progress,
                        id,
                        key,
                        "output",
                        "failed",
                        start,
                        Some("cancelled"),
                    );
                    return Err(ProjectError::RenderCancelled.into());
                }
                let message = e.to_string();
                // `rendered` keeps its verified file and is retried from QA; an output with
                // nothing to show is `failed`; a `qualified` one that lost its cover is back
                // to `rendered` so qualified always means a cover exists.
                mutate(schema, dir, id, key, |x| {
                    x.error = Some(message.clone());
                    x.state = match x.state {
                        State::Pending => State::Failed,
                        State::Qualified => State::Rendered,
                        other => other,
                    };
                })?;
                emit(progress, id, key, "output", "failed", start, Some(&message));
            }
        }
    }

    if cancel.load(Ordering::SeqCst) {
        return Err(ProjectError::RenderCancelled.into());
    }
    let after = load(schema, dir, id)?;
    let all = after.outputs.iter().all(|o| o.state == State::Qualified);
    let finishing_reply = match &pin {
        Pin::Match => pin_reply("match", &[]),
        Pin::First => {
            let mut r = pin_reply("pinned", &[]);
            r["note"] = json!("this batch had no recorded finishing settings; the current ones are now pinned and outputs delivered earlier were not re-checked");
            r
        }
        Pin::Repin(d) => pin_reply("repinned", d),
        Pin::Refuse(_) => unreachable!("refusal returned above"),
    };
    Ok(json!({
        "status": if all { "done" } else { "partial" },
        "id": id,
        "finishing": finishing_reply,
        "outputs": after.outputs.iter().map(|o| json!({
            "key": o.key,
            "state": o.state.as_str(),
            "output": o.output,
            "cover": o.cover,
            "error": o.error,
        })).collect::<Vec<_>>(),
        "ran": ran,
        "skipped": skipped,
        "covered": covered,
    }))
}

/// One output through the steps `step` asks for, persisting each state change. `Ok(true)`
/// when it rendered. A QA verdict of fail is recorded here as `failed` and is not an error.
fn process(
    ctx: &Ctx<'_, impl BatchHost>,
    o: &Output,
    step: Step,
    fraction: f64,
    progress: &mut dyn FnMut(&Value),
) -> Result<bool, BatchError> {
    let (schema, dir, id, key) = (ctx.schema, ctx.dir, ctx.id, o.key.as_str());
    let mut rendered = false;
    if step == Step::Render {
        emit(progress, id, key, "render", "running", fraction, None);
        mutate(schema, dir, id, key, |x| {
            x.state = State::Pending;
            x.output = None;
            x.output_blake3 = None;
            x.qa = None;
            x.attempts += 1;
        })?;
        let out_name = format!("batch-{id}-{key}");
        let output = ctx.host.render(
            BatchRender {
                dir,
                variant: o.variant.as_deref(),
                preset: &o.preset,
                out_name: &out_name,
                stage_job: &ctx.stage_job,
                timeout_secs: ctx.timeout_secs,
                cancel: Arc::clone(&ctx.cancel),
            },
            progress,
        )?;
        let hash = rightkit_fs::digest::blake3_file(&output)
            .map_err(|e| io(format!("hashing {}", output.display()), e))?;
        let record_file = record_path(&output);
        let record: Value = serde_json::from_slice(
            &std::fs::read(&record_file)
                .map_err(|e| io(format!("reading {}", record_file.display()), e))?,
        )
        .map_err(|e| internal(format!("{} is not JSON: {e}", record_file.display())))?;
        if record["output_blake3"].as_str() != Some(hash.as_str()) {
            return Err(internal(format!(
                "the render record disagrees with the BLAKE3 of {}",
                output.display()
            )));
        }
        let output_rel = rel(dir, &output);
        mutate(schema, dir, id, key, |x| {
            x.state = State::Rendered;
            x.output = Some(output_rel);
            x.output_blake3 = Some(hash);
            x.error = None;
        })?;
        rendered = true;
        emit(progress, id, key, "render", "done", fraction, None);
    }

    if ctx.cancel.load(Ordering::SeqCst) {
        return Err(ProjectError::RenderCancelled.into());
    }
    let current = load(schema, dir, id)?.output(key)?.clone();
    let (output_rel, hash) = match (&current.output, &current.output_blake3) {
        (Some(path), Some(hash)) => (path.clone(), hash.clone()),
        _ => {
            return Err(internal(format!(
                "output {key} has no render to continue from"
            )))
        }
    };
    let output = dir.join(output_rel);

    if step != Step::Cover {
        emit(progress, id, key, "qa", "running", fraction, None);
        let verdict = qualify(ctx, &current, &output, &hash, progress)?;
        let qa = QaRef {
            status: verdict.status.clone(),
            receipt: verdict.receipt.clone(),
        };
        mutate(schema, dir, id, key, |x| x.qa = Some(qa))?;
        if !verdict.passed {
            let message = format!("QA failed: {}", verdict.blocking.join(", "));
            mutate(schema, dir, id, key, |x| {
                x.state = State::Failed;
                x.error = Some(message.clone());
            })?;
            emit(progress, id, key, "qa", "failed", fraction, Some(&message));
            emit(
                progress,
                id,
                key,
                "output",
                "failed",
                fraction,
                Some(&message),
            );
            return Ok(rendered);
        }
        emit(progress, id, key, "qa", "done", fraction, None);
    }

    if ctx.cancel.load(Ordering::SeqCst) {
        return Err(ProjectError::RenderCancelled.into());
    }
    emit(progress, id, key, "cover", "running", fraction, None);
    let cover = match &current.cover {
        Some(c) if file_matches(&dir.join(&c.path), &c.blake3)? => c.clone(),
        // A cover that was chosen keeps its frame time when its image has to be remade.
        Some(c) => make_cover(ctx, &current, &output, c.at_ms, progress)?,
        None => {
            let at_ms = render_duration_ms(&output)? / 5;
            make_cover(ctx, &current, &output, at_ms, progress)?
        }
    };
    mutate(schema, dir, id, key, |x| {
        x.cover = Some(cover);
        x.state = State::Qualified;
        x.error = None;
    })?;
    emit(progress, id, key, "cover", "done", fraction, None);
    emit(progress, id, key, "output", "qualified", fraction, None);
    Ok(rendered)
}

/// Reuse only passing QA evidence bound to the exact bytes and preset.
fn qualify(
    ctx: &Ctx<'_, impl BatchHost>,
    output: &Output,
    path: &Path,
    hash: &str,
    progress: &mut dyn FnMut(&Value),
) -> Result<BatchQa, BatchError> {
    if let Some(verified) = ctx.host.cached_qa(ctx.dir, hash).filter(|report| {
        report.passed && report.preset == output.qa_preset && report.render_blake3 == hash
    }) {
        return Ok(verified);
    }
    let report = ctx.host.run_qa(
        ctx.dir,
        output.variant.as_deref(),
        path,
        &output.qa_preset,
        progress,
    )?;
    if report.render_blake3 != hash {
        return Err(internal("the QA report is bound to a different file"));
    }
    if report.preset != output.qa_preset {
        return Err(internal("the QA report is bound to a different preset"));
    }
    Ok(report)
}

/// `media.thumbnail` of the render at `at_ms`, copied to this output's cover file so the
/// thumbnail cache may evict the original.
fn make_cover(
    ctx: &Ctx<'_, impl BatchHost>,
    o: &Output,
    output: &Path,
    at_ms: i64,
    progress: &mut dyn FnMut(&Value),
) -> Result<Cover, BatchError> {
    let file_name = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| internal("render output has no file name"))?;
    let cached = ctx
        .host
        .thumbnail(ctx.dir, &file_name, at_ms, COVER_WIDTH, progress)?;
    let bytes =
        std::fs::read(&cached).map_err(|e| io(format!("reading {}", cached.display()), e))?;
    let ext = image_ext(&bytes).ok_or_else(|| {
        internal(format!(
            "{} is not a PNG, JPEG or WebP image",
            cached.display()
        ))
    })?;
    let path = covers_dir(ctx.dir, ctx.id).join(format!("{}.{ext}", o.key));
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(&bytes),
    )
    .map_err(|e| ProjectError::io(format!("writing {}", path.display()), e.into()))?;
    Ok(Cover {
        at_ms,
        path: rel(ctx.dir, &path),
        blake3: rightkit_fs::digest::blake3_bytes(&bytes),
    })
}

// ------------------------------------------------------------------ cover, status, list

/// Replaces an output's cover with the frame of its render at `at_ms`. The output's state,
/// render, record and QA receipt are not touched.
#[allow(clippy::too_many_arguments)]
fn cover(
    schema: &str,
    dir: &Path,
    id: &str,
    key: &str,
    at_ms: i64,
    host: &impl BatchHost,
    dry_run: bool,
    progress: &mut dyn FnMut(&Value),
) -> Result<Value, BatchError> {
    let batch = load(schema, dir, id)?;
    let o = batch.output(key)?.clone();
    let (Some(output_rel), Some(hash)) = (&o.output, &o.output_blake3) else {
        return Err(BatchError::bad(format!(
            "output {key:?} has no render yet; run batch.run first"
        )));
    };
    let output = dir.join(output_rel);
    if !file_matches(&output, hash)? {
        return Err(BatchError::bad(format!(
            "the render of {key:?} is missing or changed; run batch.run to render it again"
        )));
    }
    let duration_ms = render_duration_ms(&output)?;
    if at_ms < 0 || at_ms >= duration_ms {
        return Err(BatchError::bad(format!(
            "at_ms must be from 0 to {} (the render is {duration_ms} ms long)",
            duration_ms - 1
        )));
    }
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {"command": "batch.cover", "id": id, "key": key, "at_ms": at_ms},
        }));
    }
    let ctx = Ctx {
        schema,
        dir,
        id,
        host,
        timeout_secs: None,
        stage_job: String::new(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let chosen = make_cover(&ctx, &o, &output, at_ms, progress)?;
    let record = chosen.clone();
    mutate(schema, dir, id, key, |x| x.cover = Some(record))?;
    // The old image is only a leftover when the new one has a different extension.
    if let Some(old) = o.cover.as_ref().filter(|c| c.path != chosen.path) {
        let _ = std::fs::remove_file(dir.join(&old.path));
    }
    Ok(json!({"status": "saved", "id": id, "key": key, "cover": chosen}))
}

fn status(schema: &str, dir: &Path, id: &str, host: &impl BatchHost) -> Result<Value, BatchError> {
    let batch = load(schema, dir, id)?;
    let mut doc = serde_json::to_value(&batch)
        .map_err(|e| internal(format!("serialising batch {id}: {e}")))?;
    // `finishing_matches`: true or false against the project's settings now; null when the
    // batch is unpinned (made before pinning) or the settings cannot be read.
    let (matches, differs, current) = match finishing_now(dir, host) {
        Ok((snapshot, hash)) => match &batch.finishing_blake3 {
            Some(pinned) => (
                json!(*pinned == hash),
                differing_keys(batch.finishing.as_ref(), &snapshot),
                json!(hash),
            ),
            None => (Value::Null, Vec::new(), json!(hash)),
        },
        Err(e) => {
            doc["finishing_error"] = json!(e.message);
            (Value::Null, Vec::new(), Value::Null)
        }
    };
    doc["finishing_matches"] = matches;
    doc["finishing_differs"] = json!(differs);
    doc["current_finishing_blake3"] = current;
    Ok(doc)
}

fn list(schema: &str, dir: &Path) -> Result<Value, BatchError> {
    Project::open(dir, Access::Read)?;
    let root = batches_dir(dir);
    let mut batches: Vec<Batch> = Vec::new();
    let mut invalid: Vec<Value> = Vec::new();
    if root.is_dir() {
        let entries =
            std::fs::read_dir(&root).map_err(|e| io(format!("reading {}", root.display()), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match read_batch(schema, &path, id) {
                Ok(batch) => batches.push(batch),
                Err(e) => invalid.push(json!({"id": id, "error": e.message})),
            }
        }
    }
    batches.sort_by_key(|batch| batch.id.clone());
    let rows: Vec<Value> = batches
        .iter()
        .map(|b| {
            let mut counts = serde_json::Map::new();
            for state in [
                State::Pending,
                State::Rendered,
                State::Qualified,
                State::Failed,
            ] {
                let n = b.outputs.iter().filter(|o| o.state == state).count();
                counts.insert(state.as_str().to_string(), json!(n));
            }
            json!({
                "id": b.id,
                "created_at": b.created_at,
                "outputs": b.outputs.len(),
                "counts": counts,
            })
        })
        .collect();
    Ok(json!({"batches": rows, "invalid": invalid}))
}

// ------------------------------------------------------------------ dispatch

fn dispatch(
    schema: &str,
    method: &str,
    params: &Value,
    host: &impl BatchHost,
    dry_run: bool,
    progress: &mut dyn FnMut(&Value),
) -> Result<Value, BatchError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        dir: Option<PathBuf>,
        id: Option<String>,
        outputs: Option<Vec<OutputSpec>>,
        key: Option<String>,
        at_ms: Option<i64>,
        job: Option<String>,
        timeout_secs: Option<u64>,
        finishing: Option<String>,
        dry_run: Option<bool>,
    }
    let p: P = serde_json::from_value(if params.is_null() {
        json!({})
    } else {
        params.clone()
    })
    .map_err(|e| BatchError::bad(format!("invalid params: {e}")))?;
    let dry_run = dry_run || p.dry_run.unwrap_or(false);
    let dir = p.dir.ok_or_else(|| BatchError::bad("missing param: dir"))?;
    let need_id = |id: Option<String>| id.ok_or_else(|| BatchError::bad("missing param: id"));
    match method {
        "batch.create" => {
            let outputs = p
                .outputs
                .ok_or_else(|| BatchError::bad("missing param: outputs"))?;
            create(schema, &dir, outputs, p.id, host, dry_run)
        }
        "batch.run" => run(
            schema,
            &dir,
            &need_id(p.id)?,
            p.job.as_deref(),
            p.timeout_secs,
            p.finishing.as_deref(),
            host,
            dry_run,
            progress,
        ),
        "batch.status" => status(schema, &dir, &need_id(p.id)?, host),
        "batch.list" => list(schema, &dir),
        "batch.cover" => {
            let key = p.key.ok_or_else(|| BatchError::bad("missing param: key"))?;
            let at_ms = p
                .at_ms
                .ok_or_else(|| BatchError::bad("missing param: at_ms"))?;
            cover(
                schema,
                &dir,
                &need_id(p.id)?,
                &key,
                at_ms,
                host,
                dry_run,
                progress,
            )
        }
        other => Err(BatchError::bad(format!("unknown method {other}"))),
    }
}

/// Serve entry: `Some(result)` when `method` belongs to this module. Per-output events
/// (`{"event":"batch.output",..}`) and the renders' `render.progress` go to `progress`.
pub fn handle_method(
    schema: &str,
    method: &str,
    params: &Value,
    host: &impl BatchHost,
    dry_run: bool,
    progress: &mut dyn FnMut(&Value),
) -> Option<Result<Value, BatchError>> {
    METHODS
        .contains(&method)
        .then(|| dispatch(schema, method, params, host, dry_run, progress))
}
