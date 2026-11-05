//! `brief run`: the generation engine's side of the RightKit content-brief / delivery-envelope contracts.
//!
//! ```text
//! <engine> brief run --project <dir> --input <content-brief.json> [--timeout-secs <n>] [--dry-run]
//! ```
//!
//! Same CLI and JSON contract as every suite engine's `brief run` (a dispatcher calls them all the same
//! way): one JSON object on stdout and exit 0, `{"error":{code,message}}`
//! and exit 1 on failure, a `bad_usage` error and exit 2 on a usage error; progress as JSON lines
//! on stderr. The brief and the envelope are the `rightkit-contracts` types; the envelope is
//! validated against the embedded delivery-envelope v1 schema before it is written.
//!
//! What this engine produces: the `stills` and `carousel` deliverables (delivery-envelope payloads
//! `stills` and `carousel`), generated with the keyless local `local-image` route. `video` and
//! `text` are not this engine's envelopes and are listed under `skipped`.
//!
//! Library registration: every run has a caller (`--caller <name>`, else `<PREFIX>_CALLER`, else
//! `cli`). An INSTALLED engine (release build, no `<PREFIX>_QA`) also registers each delivered image
//! in the user's own library under that caller, so the Gallery lists it ("From `<caller>`")
//! beside the project copy under `<project>/exports/`. QA-marked engines never touch
//! the user's library (see `registration_for`). Registration problems are reported on the
//! deliverable row (`library`) and never fail a delivery whose project files are already placed.
//!
//! Review: `review.mode` decides whether a person must have approved the files before they are
//! placed under `<project>/exports/`. `manual` and `auto_when_clean` refuse the deliverable with
//! `eyes_approval_required` (details name each unapproved asset and its gate state) unless every
//! file is approved in the app and unchanged since; `none` does not require it, and each delivered
//! row and manifest carries `review: {mode, eyes_gate}` (`not_required`, or `approved` with the
//! approved hashes). The generated files stay in the library with a pending review request, and a
//! re-run reuses them (same request ids), so approving then re-running delivers.
//!
//! Money: content-brief v1 carries no spend authorisation, so this command never starts a paid
//! provider call. Every request is pinned to the keyless `local-image` provider and must estimate
//! at 0 USD in the existing dry-run path before it is submitted. A deliverable that would need a
//! paid route is refused with `spend_authorisation_required`. Caps, held (`outcome_unknown`) jobs
//! and the ledger are untouched: a held job is reported, never resubmitted.
//!
//! Model lifetime: the run's local-image jobs execute IN THIS PROCESS through one warm
//! `LocalImage` provider (`run_providers`), so the ~10 GB model server loads once for the whole
//! run, not once per image. Each image is still its own job record (own id, result, `job cancel
//! <id>`; a failed image does not lose the others). The server belongs to this process: it ends
//! when the provider is dropped at the end of `execute` (success, failure or timeout), at once
//! when another process waits for the machine-wide lock (the idle holder yields between images),
//! after any job that failed or was cancelled, by the 300 s idle backstop, and with this process
//! if it is killed (macOS watchdog, Windows Job Object, Linux parent-death signal).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_contracts::types::content_brief::{ContentBrief, Deliverable, ReviewMode, StageId};
use rightkit_contracts::types::delivery_envelope::{
    DeliveryEnvelope, Engine, Payload, Producer, Slide, Still,
};
use rightkit_contracts::Contract;
use rightkit_media_gen::jobs::RunOptions;
use rightkit_media_gen::providers::local_image::LocalImage;
use rightkit_media_gen::providers::Provider;
use rightkit_media_gen::{api, library, Context, GenError, GenResult};
use serde_json::{json, Value};

/// Whether the host QA marker (`<PREFIX>_QA`) is set (non-empty, not `0`).
pub fn qa_marked() -> bool {
    rightkit_media_gen::host::env_var("QA").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The per-project state folder name: `.` plus the host settings namespace.
fn project_state_dir_name() -> String {
    format!(".{}", rightkit_media_gen::host::config().settings_namespace)
}

pub const USAGE: &str = "\
usage: <engine> brief run --project <dir> --input <content-brief.json> [--timeout-secs <n>] [--dry-run] [--use-user-library] [--caller <name>] [--budget-project <id> --budget-remaining <usd>]";

/// The only provider `brief run` may use: keyless, local, 0 USD.
const LOCAL_PROVIDER: &str = "local-image";
/// Delivery-envelope v1 bounds.
const MAX_ALT: usize = 1500;
const MAX_SLIDE_COPY: usize = 300;
const MAX_IMAGES: usize = 20;
const MIN_SLIDES: usize = 2;

// ------------------------------------------------------------------ CLI

struct Args {
    project: PathBuf,
    input: PathBuf,
    timeout_secs: Option<u64>,
    dry_run: bool,
    /// Keep job state in the user's own library instead of `<project>/.<namespace>/`.
    /// For the user's own app only; refused for QA-marked engines.
    use_user_library: bool,
    /// `--caller`; resolved against `<PREFIX>_CALLER` and the `cli` default in `run_cli`.
    caller: Option<String>,
    /// `--budget-project` / `--budget-remaining`: a budget Pulse issued (resolved against
    /// `RIGHTKIT_BUDGET_PROJECT` / `RIGHTKIT_BUDGET_REMAINING` in `run_cli`).
    budget_project: Option<String>,
    budget_remaining: Option<String>,
}

/// `Ok(None)` means help was asked for.
fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("run") => {}
        Some("--help" | "-h") => return Ok(None),
        Some(other) => return Err(format!("unknown brief subcommand {other:?}")),
        None => return Err("missing brief subcommand (run)".into()),
    }
    let mut project: Option<PathBuf> = None;
    let mut input: Option<PathBuf> = None;
    let mut timeout_secs: Option<u64> = None;
    let mut dry_run = false;
    let mut use_user_library = false;
    let mut caller: Option<String> = None;
    let mut budget_project: Option<String> = None;
    let mut budget_remaining: Option<String> = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--project" => {
                project = Some(PathBuf::from(it.next().ok_or("--project needs a value")?))
            }
            "--input" => input = Some(PathBuf::from(it.next().ok_or("--input needs a value")?)),
            "--timeout-secs" => {
                let raw = it.next().ok_or("--timeout-secs needs a value")?;
                match raw.parse::<u64>() {
                    Ok(n) if n >= 1 => timeout_secs = Some(n),
                    _ => {
                        return Err(format!(
                        "--timeout-secs takes a whole number of seconds, at least 1, got {raw:?}"
                    ))
                    }
                }
            }
            "--dry-run" => dry_run = true,
            "--use-user-library" => use_user_library = true,
            "--caller" => caller = Some(it.next().ok_or("--caller needs a value")?.clone()),
            "--budget-project" => {
                budget_project = Some(it.next().ok_or("--budget-project needs a value")?.clone())
            }
            "--budget-remaining" => {
                budget_remaining =
                    Some(it.next().ok_or("--budget-remaining needs a value")?.clone())
            }
            "--help" | "-h" => return Ok(None),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(Some(Args {
        project: project.ok_or("missing --project")?,
        input: input.ok_or("missing --input")?,
        timeout_secs,
        dry_run,
        use_user_library,
        caller,
        budget_project,
        budget_remaining,
    }))
}

/// CLI entry (`<engine> brief …`, words after `brief`): prints the JSON result or error and
/// returns the exit code (0, 1 command error, 2 usage error).
pub fn run_cli(args: &[String]) -> i32 {
    let args = match parse_args(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            eprintln!("{USAGE}");
            return 0;
        }
        Err(message) => {
            eprintln!("{USAGE}");
            println!(
                "{}",
                json!({"error": {"code": "bad_usage", "message": message}})
            );
            return 2;
        }
    };
    let caller = match rightkit_media_gen::host::resolve_caller(args.caller.as_deref()) {
        Ok(caller) => caller,
        Err(message) => {
            eprintln!("{USAGE}");
            println!(
                "{}",
                json!({"error": {"code": "bad_usage", "message": message}})
            );
            return 2;
        }
    };
    // A budget Pulse issued: validated before anything is created. This command never reserves paid
    // spend (every request is pinned to the 0 USD local route), so the budget can only ever be
    // reported against, never exceeded; a bad one (NaN, negative, half a pair) is still refused.
    let budget = match rightkit_media_gen::ledger::CallerBudget::from_flags_and_env(
        args.budget_project.as_deref(),
        args.budget_remaining.as_deref(),
    ) {
        Ok(budget) => budget,
        Err(e) => {
            eprintln!("{USAGE}");
            println!(
                "{}",
                json!({"error": {"code": e.code, "message": e.message}})
            );
            return 2;
        }
    };
    let decided = match decide_data_root(&args) {
        Ok(decided) => decided,
        Err(message) => {
            println!(
                "{}",
                json!({"error": {"code": "data_folder_required", "message": message}})
            );
            return 2;
        }
    };
    if let Err(message) = apply_data_root(&decided, args.dry_run) {
        println!(
            "{}",
            json!({"error": {"code": "data_folder_required", "message": message}})
        );
        return 2;
    }
    let ctx = Context::production();
    if !args.dry_run {
        rightkit_media_gen::ledger::set_process_budget(budget.clone());
    }
    let library = LibraryTarget {
        mode: registration_for(
            &decided,
            &rightkit_media_gen::host::user_data_root(),
            qa_marked(),
            false,
        ),
        caller,
    };
    match execute(
        &ctx,
        &args.project,
        &args.input,
        args.timeout_secs,
        args.dry_run,
        &library,
    ) {
        Ok(mut value) => {
            if let Some(b) = &budget {
                if args.dry_run {
                    value["would"]["budget"] = b.to_value();
                }
            }
            if args.dry_run {
                // Reported, never created: where a real run would keep its state.
                value["would"]["data_root"] =
                    json!(decided.path().map(|p| p.to_string_lossy().into_owned()));
            }
            println!("{value}");
            0
        }
        Err(error) => {
            let mut body = json!({"code": error.code, "message": error.message});
            if let Some(details) = &error.details {
                body["details"] = details.clone();
            }
            println!("{}", json!({"error": body}));
            1
        }
    }
}

/// Where a run keeps its state, decided before anything is created or moved.
#[derive(Clone)]
pub enum DataRoot {
    /// The caller named the folder (`<PREFIX>_APP_DATA` or `--data-dir`).
    Explicit(PathBuf),
    /// `--use-user-library`: the per-user default folder.
    UserLibrary,
    /// The dispatched default: `<project>/.<namespace>`, absolute, not yet created.
    Project(PathBuf),
    /// `--project` is not a directory: `execute` reports it, and nothing is decided.
    Unresolved,
}

impl DataRoot {
    /// The folder the run uses, for reporting. `None` when `execute` reports a missing project.
    fn path(&self) -> Option<PathBuf> {
        match self {
            DataRoot::Explicit(path) | DataRoot::Project(path) => Some(path.clone()),
            DataRoot::UserLibrary => Some(rightkit_media_gen::host::user_data_root()),
            DataRoot::Unresolved => None,
        }
    }
}

/// Where a dispatched `brief run` keeps its state (job store, library, local-image store, logs,
/// leases): never the user's real library by accident.
///
/// 1. A folder the caller chose (`<PREFIX>_APP_DATA` / `--data-dir`) wins; QA harnesses and
///    isolated dispatchers (a dispatcher under QA) set it.
/// 2. A QA-marked engine (`<PREFIX>_QA`) with no folder is refused: `data_folder_required`.
/// 3. `--use-user-library` is for the user's own app: it keeps the per-user default.
/// 4. Otherwise (the cross-app dispatch default) state lives in `<project>/.<namespace>/`: the
///    project is where the caller reads the envelope and images, deleting the project deletes the
///    state, and the user's library is not touched. Installed models are still read from the
///    user's model folder (read-only) so the run can find them.
///
/// Validates the refusals and decides the folder. Writes nothing and changes no environment, so a
/// dry run can call it freely.
fn decide_data_root(args: &Args) -> Result<DataRoot, String> {
    use rightkit_media_gen::host as protocol;
    if let Some(root) = protocol::env_var("APP_DATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        return Ok(DataRoot::Explicit(root));
    }
    if qa_marked() {
        return Err(format!(
            "the QA marker is set and no data folder was given; a test must never use the user's real data folder ({}). Set the APP_DATA override or pass --data-dir <dir>",
            protocol::user_data_root().display()
        ));
    }
    if args.use_user_library {
        return Ok(DataRoot::UserLibrary);
    }
    if !args.project.is_dir() {
        // execute() reports the missing project; nothing may be created for it.
        return Ok(DataRoot::Unresolved);
    }
    let dir = args.project.join(project_state_dir_name());
    let root = std::path::absolute(&dir).map_err(|e| format!("resolve {}: {e}", dir.display()))?;
    Ok(DataRoot::Project(root))
}

/// Where a run's delivered images are registered besides the project folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// Copy each delivered image into the user's own library under the caller's tag.
    IntoUserLibrary,
    /// The run already writes to the user's library: tag its files in place.
    TagInPlace,
    /// Nothing is registered; the reason is reported.
    Skip(&'static str),
}

/// The caller and where its outputs are registered.
struct LibraryTarget {
    caller: rightkit_media_gen::host::Caller,
    mode: Registration,
}

impl LibraryTarget {
    /// The sidecar entry (`brief.run` command): the app owns its own library and gallery.
    fn none() -> Self {
        Self {
            caller: rightkit_media_gen::host::Caller::cli(),
            mode: Registration::Skip("sidecar_run"),
        }
    }
}

/// The one rule for touching the user's real library: only an installed engine, never a QA-marked
/// one, and never a run the caller isolated in its own folder.
///
/// - QA-marked (`<PREFIX>_QA`): never (row 225's guard; a test must not write real data).
/// - the dispatched default (`<project>/.<namespace>`): copy into the user's library.
/// - `--use-user-library`, or an explicit folder that IS the user's folder: the files are already in
///   the library; tag them.
/// - any other explicit folder (an isolated caller): leave it isolated.
pub fn registration_for(
    decided: &DataRoot,
    user_root: &Path,
    qa: bool,
    debug: bool,
) -> Registration {
    if qa {
        return Registration::Skip("qa_marked");
    }
    if debug {
        return Registration::Skip("debug_build");
    }
    match decided {
        DataRoot::Project(_) => Registration::IntoUserLibrary,
        DataRoot::UserLibrary => Registration::TagInPlace,
        DataRoot::Explicit(path) if same_folder(path, user_root) => Registration::TagInPlace,
        DataRoot::Explicit(_) => Registration::Skip("explicit_data_folder"),
        DataRoot::Unresolved => Registration::Skip("no_project"),
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Register one delivered image per the run's [`Registration`]. Returns the `library` entry for the
/// row: `registered` / `tagged` (with `path`, and `asset_id` when registered), `skipped` (with
/// `reason`) or `failed` (with `error`). A failure never undoes the project copy.
fn register_image(target: &LibraryTarget, p: &Produced, bytes: &[u8], aspect: &str) -> Value {
    let skipped = |reason: &str| json!({"status": "skipped", "reason": reason});
    let failed =
        |e: GenError| json!({"status": "failed", "error": {"code": e.code, "message": e.message}});
    match &target.mode {
        Registration::Skip(reason) => skipped(reason),
        Registration::TagInPlace => match library::tag_in_place(&p.src, &target.caller) {
            Ok(path) => json!({"status": "tagged", "path": path.to_string_lossy()}),
            Err(e) => failed(e),
        },
        Registration::IntoUserLibrary => {
            // Defence in depth: re-check right where the real folder would be written.
            if qa_marked() {
                return skipped("qa_or_debug");
            }
            let ctx = Context::new(rightkit_media_gen::host::user_data_root());
            let text = |key: &str| p.provenance[key].as_str().unwrap_or("").to_string();
            let (provider, model, job_id) = (text("provider"), text("model"), text("job_id"));
            let asset = library::CallerAsset {
                caller: &target.caller,
                provider: &provider,
                model: &model,
                job_id: &job_id,
                prompt: &p.prompt,
                aspect,
                cost_usd: 0.0,
            };
            match library::register_caller_asset(&ctx, bytes, &p.mime, &asset) {
                Ok(stored) => {
                    json!({"status": "registered", "asset_id": stored.asset_id, "path": stored.path.to_string_lossy()})
                }
                Err(e) => failed(e),
            }
        }
    }
}

/// What a run would do with the library, for `--dry-run`.
fn library_summary(library: &LibraryTarget) -> Value {
    let (mode, reason) = match &library.mode {
        Registration::IntoUserLibrary => ("register", None),
        Registration::TagInPlace => ("tag", None),
        Registration::Skip(reason) => ("skip", Some(*reason)),
    };
    json!({"caller": library.caller.slug, "label": library.caller.label, "mode": mode, "reason": reason})
}

/// Makes the decided folder this process's data folder. A real run creates `<project>/.<namespace>`,
/// announces the root. A dry run sets the same environment in
/// this process, so it plans against the same installed models, and touches nothing on disk.
fn apply_data_root(decided: &DataRoot, dry_run: bool) -> Result<(), String> {
    let root = match decided {
        DataRoot::Explicit(_) | DataRoot::Unresolved | DataRoot::UserLibrary => return Ok(()),
        DataRoot::Project(root) => root,
    };
    if !dry_run {
        std::fs::create_dir_all(root).map_err(|e| format!("create {}: {e}", root.display()))?;
    }
    if std::env::var_os(rightkit_media_gen::host::env_name("MODEL_ROOT"))
        .filter(|v| !v.is_empty())
        .is_none()
    {
        std::env::set_var(
            rightkit_media_gen::host::env_name("MODEL_ROOT"),
            rightkit_media_gen::providers::local_image::user_model_root(),
        );
    }
    if std::env::var_os(rightkit_media_gen::host::env_name("CONTENT_ROOT"))
        .filter(|v| !v.is_empty())
        .is_none()
    {
        std::env::set_var(
            rightkit_media_gen::host::env_name("CONTENT_ROOT"),
            root.join("content"),
        );
    }
    // Every child (job runner, watchdog, model server) inherits this explicitly.
    std::env::set_var(rightkit_media_gen::host::env_name("APP_DATA"), root);
    if !dry_run {
        eprintln!(
            "{}",
            json!({"event": "brief.data_root", "data_root": root.to_string_lossy(), "source": "project"})
        );
    }
    Ok(())
}

/// Sidecar entry (`brief.run` command): `{project, input, timeout_secs?, dry_run?}`.
pub fn run_input(ctx: &Context, input: &Value, dry_run: bool) -> GenResult<Value> {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| GenError::new("bad_input", format!("missing field: {key}")))
    };
    let project = text("project")?;
    let brief = text("input")?;
    let timeout_secs = match input.get("timeout_secs").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => Some(n),
            _ => {
                return Err(GenError::new(
                    "bad_input",
                    "timeout_secs must be a whole number of seconds, at least 1",
                ))
            }
        },
    };
    let dry_run = dry_run
        || input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    execute(
        ctx,
        &project,
        &brief,
        timeout_secs,
        dry_run,
        &LibraryTarget::none(),
    )
}

// ------------------------------------------------------------------ planning

/// One image to generate: its prompt, and for a carousel the slide copy.
struct Image {
    prompt: String,
    slide_copy: Option<String>,
}

fn label(deliverable: Deliverable) -> &'static str {
    match deliverable {
        Deliverable::Video => "video",
        Deliverable::Stills => "stills",
        Deliverable::Carousel => "carousel",
        Deliverable::Text => "text",
    }
}

/// Blank-line separated paragraphs, each on one line.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(line.trim());
        }
    }
    if !current.is_empty() {
        out.push(current.join(" "));
    }
    out
}

/// `stills`: one image. The brief has no image-prompt field, so the prompt is the hook, else the
/// script's first paragraph.
fn stills_images(brief: &ContentBrief) -> GenResult<Vec<Image>> {
    let hook = brief
        .hook
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(String::from);
    let prompt = hook.or_else(|| {
        brief
            .script
            .as_deref()
            .map(paragraphs)
            .and_then(|p| p.into_iter().next())
    });
    match prompt {
        Some(prompt) => Ok(vec![Image {
            prompt,
            slide_copy: None,
        }]),
        None => Err(GenError::new(
            "brief_insufficient",
            "stills need a hook or a script to describe the image; the brief has neither",
        )),
    }
}

/// `carousel`: one slide per script paragraph (2 to 20).
fn carousel_images(brief: &ContentBrief) -> GenResult<Vec<Image>> {
    let paras = brief.script.as_deref().map(paragraphs).unwrap_or_default();
    if paras.len() < MIN_SLIDES {
        return Err(GenError::new(
            "brief_insufficient",
            format!("a carousel needs a script with at least {MIN_SLIDES} blank-line separated paragraphs, one per slide; found {}", paras.len()),
        ));
    }
    if paras.len() > MAX_IMAGES {
        return Err(GenError::new(
            "brief_insufficient",
            format!(
                "a carousel holds at most {MAX_IMAGES} slides; the script has {} paragraphs",
                paras.len()
            ),
        ));
    }
    Ok(paras
        .into_iter()
        .map(|p| {
            let slide_copy = (p.chars().count() <= MAX_SLIDE_COPY).then(|| p.clone());
            Image {
                prompt: p,
                slide_copy,
            }
        })
        .collect())
}

fn platform_aspect(platform: &str) -> &'static str {
    match platform {
        "youtube" => "16:9",
        "youtube_shorts" | "shorts" | "instagram_reels" | "reels" | "tiktok" => "9:16",
        _ => "1:1",
    }
}

/// The platforms' shared aspect, else square.
fn aspect_of(platforms: &[String]) -> &'static str {
    let first = platforms
        .first()
        .map(|p| platform_aspect(p))
        .unwrap_or("1:1");
    if platforms.iter().all(|p| platform_aspect(p) == first) {
        first
    } else {
        "1:1"
    }
}

/// Instagram feed platforms: a carousel built only for these is portrait 4:5.
fn is_instagram_feed(platform: &str) -> bool {
    matches!(
        platform,
        "instagram" | "instagram_carousel" | "instagram_feed"
    )
}

/// Carousels are 4:5 when every platform is an Instagram feed variant; otherwise the shared rule.
fn carousel_aspect(platforms: &[String]) -> &'static str {
    if !platforms.is_empty() && platforms.iter().all(|p| is_instagram_feed(p)) {
        "4:5"
    } else {
        aspect_of(platforms)
    }
}

/// The aspect of one deliverable. Stills keep the shared rule; only carousels take 4:5.
fn aspect_for(deliverable: Deliverable, platforms: &[String]) -> &'static str {
    if deliverable == Deliverable::Carousel {
        carousel_aspect(platforms)
    } else {
        aspect_of(platforms)
    }
}

/// The local-image request params for an aspect. The local-image provider accepts only 1:1, 16:9
/// and 9:16 as aspects, so 4:5 is sent as explicit dimensions: 1024x1280 is exactly 4:5 and a
/// multiple of 256, so it satisfies every profile's `dimension_multiple`.
fn image_params(aspect: &str) -> Value {
    if aspect == "4:5" {
        json!({"width": 1024, "height": 1280})
    } else {
        json!({"aspect": aspect})
    }
}

/// Stable per image: the brief's own stage request id and input hash when the stage exists (a
/// corrected brief changes the input hash, so stale jobs are never reused), plus a digest of the
/// request itself because a stored job is returned by request id whatever its body.
fn request_id(
    brief: &ContentBrief,
    deliverable: Deliverable,
    model: &str,
    aspect: &str,
    image: &Image,
    index: usize,
) -> String {
    let stage = if deliverable == Deliverable::Stills {
        StageId::Generate
    } else {
        StageId::Carousel
    };
    let base = brief
        .stages
        .iter()
        .find(|s| s.id == stage)
        .map(|s| format!("{}:{}", s.request_id, s.input_hash))
        .unwrap_or_else(|| {
            format!(
                "brief:{}:r{}:{}",
                brief.brief_id,
                brief.revision,
                label(deliverable)
            )
        });
    let digest = library::sha256_hex(
        format!("{LOCAL_PROVIDER}|{model}|{aspect}|{}", image.prompt).as_bytes(),
    );
    format!("{base}:{}:{}", index + 1, &digest[..16])
}

// ------------------------------------------------------------------ outcomes

/// A deliverable that did not finish. `status` is the row status: `refused` (nothing was
/// started), `failed`, or `timed_out`.
struct Fail(Box<FailData>);

struct FailData {
    status: &'static str,
    error: GenError,
}

impl std::ops::Deref for Fail {
    type Target = FailData;
    fn deref(&self) -> &FailData {
        &self.0
    }
}

impl Fail {
    fn new(status: &'static str, error: GenError) -> Self {
        Fail(Box::new(FailData { status, error }))
    }
}

fn fail(status: &'static str, code: impl Into<String>, message: impl Into<String>) -> Fail {
    Fail::new(status, GenError::new(code, message))
}

fn io_fail(context: String, e: impl std::fmt::Display) -> Fail {
    fail("failed", "io_error", format!("{context}: {e}"))
}

fn failure_row(deliverable: Deliverable, status: &str, code: &str, message: &str) -> Value {
    json!({
        "deliverable": label(deliverable),
        "status": status,
        "envelope": null,
        "manifest": null,
        "code": code,
        "message": message,
    })
}

fn spend_refusal(why: &str) -> Fail {
    fail(
        "refused",
        "spend_authorisation_required",
        format!(
            "{why}; content-brief v1 carries no spend authorisation, so no paid provider was contacted and nothing was reserved"
        ),
    )
}

fn step(brief_id: &str, deliverable: &str, step: &str, status: &str) {
    eprintln!(
        "{}",
        json!({"event": "brief.step", "brief_id": brief_id, "deliverable": deliverable, "step": step, "status": status})
    );
}

fn remaining(deadline: Option<Instant>) -> Option<Duration> {
    deadline.map(|t| t.saturating_duration_since(Instant::now()))
}

// ------------------------------------------------------------------ run

fn execute(
    ctx: &Context,
    project: &Path,
    input: &Path,
    timeout_secs: Option<u64>,
    dry_run: bool,
    library: &LibraryTarget,
) -> GenResult<Value> {
    if !project.is_dir() {
        return Err(GenError::new(
            "bad_input",
            format!(
                "--project {} is not an existing directory",
                project.display()
            ),
        ));
    }
    let text = std::fs::read_to_string(input)
        .map_err(|e| GenError::new("io_error", format!("reading {}: {e}", input.display())))?;
    let brief: ContentBrief =
        rightkit_contracts::parse(Contract::ContentBrief, &text).map_err(|e| {
            GenError::new(
                "bad_input",
                format!("invalid content brief {}: {e}", input.display()),
            )
        })?;
    let policy_revision = brief
        .review
        .as_ref()
        .and_then(|r| r.policy_revision)
        .ok_or_else(|| {
            GenError::new(
                "bad_input",
                "brief has no review.policyRevision; the delivery envelope requires one",
            )
        })?;

    let mut items: Vec<(Deliverable, GenResult<Vec<Image>>)> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for &deliverable in &brief.deliverables {
        match deliverable {
            Deliverable::Stills => items.push((deliverable, stills_images(&brief))),
            Deliverable::Carousel => items.push((deliverable, carousel_images(&brief))),
            Deliverable::Video | Deliverable::Text => skipped.push(json!({
                "item": format!("deliverable:{}", label(deliverable)),
                "reason": "not produced by this engine (its envelope payloads are stills and carousel)",
            })),
        }
    }
    if items.is_empty() {
        return Err(GenError::new(
            "bad_input",
            "brief requests no deliverable this engine produces (stills, carousel)",
        ));
    }

    let model = LocalImage::new().first_installed_model();
    let deadline = timeout_secs.map(|s| Instant::now() + Duration::from_secs(s));

    if dry_run {
        return Ok(plan(
            ctx,
            &brief,
            &items,
            model.as_deref(),
            &skipped,
            library,
        ));
    }

    // Dropped before the result is built (see `release`): the server is gone when the run ends.
    let providers = run_providers();
    let mut rows: Vec<Value> = Vec::new();
    let mut delivered = 0usize;
    let mut first_hard_failure: Option<GenError> = None;
    for (deliverable, images) in &items {
        let deliverable = *deliverable;
        let outcome = match (images, model.as_deref()) {
            (Err(e), _) => Err(Fail::new("refused", e.clone())),
            (Ok(_), None) => Err(spend_refusal(
                "no keyless local image model is installed (the only 0 USD image route), and every other image route is paid",
            )),
            (Ok(images), Some(model)) => {
                let aspect = aspect_for(deliverable, &brief.platforms);
                deliver(ctx, &providers, project, &brief, policy_revision, deliverable, images, model, aspect, deadline, library)
            }
        };
        match outcome {
            Ok(row) => {
                delivered += 1;
                rows.push(row);
            }
            Err(f) => {
                if f.status != "refused" && first_hard_failure.is_none() {
                    first_hard_failure = Some(f.error.clone());
                }
                let mut row = failure_row(deliverable, f.status, &f.error.code, &f.error.message);
                if let Some(details) = &f.error.details {
                    row["details"] = details.clone();
                }
                rows.push(row);
            }
        }
    }
    release(providers);
    if delivered == 0 {
        if let Some(error) = first_hard_failure {
            return Err(error);
        }
    }
    let done = delivered == items.len() && skipped.is_empty();
    Ok(json!({
        "status": if done { "done" } else { "partial" },
        "brief_id": brief.brief_id,
        "revision": brief.revision,
        "deliverables": rows,
        "skipped": skipped,
        "spend": spend_summary(ctx),
    }))
}

/// The run's `spend`: the fixed 0 USD local-route facts, plus the caller budget's project and the
/// cumulative `reserved_usd` / `actual_usd` of every job this run reserved for when a budget was
/// given (issue #37).
fn spend_summary(ctx: &Context) -> Value {
    let mut spend = json!({"authorised": false, "route": LOCAL_PROVIDER, "max_est_usd": 0.0});
    let report = rightkit_media_gen::store::open(ctx)
        .and_then(|db| rightkit_media_gen::ledger::process_spend(&db));
    match report {
        Ok(Some(Value::Object(fields))) => {
            for (k, v) in fields {
                spend[k] = v;
            }
        }
        Ok(_) => {}
        Err(e) => spend["report_error"] = json!({"code": e.code, "message": e.message}),
    }
    spend
}

/// `--dry-run`: what would be generated; nothing is stored, submitted or written.
fn plan(
    ctx: &Context,
    brief: &ContentBrief,
    items: &[(Deliverable, GenResult<Vec<Image>>)],
    model: Option<&str>,
    skipped: &[Value],
    library: &LibraryTarget,
) -> Value {
    let mut rows = Vec::new();
    for (deliverable, images) in items {
        let deliverable = *deliverable;
        let aspect = aspect_for(deliverable, &brief.platforms);
        let row = match (images, model) {
            (Err(e), _) => failure_row(deliverable, "refused", &e.code, &e.message),
            (Ok(_), None) => {
                let f = spend_refusal("no keyless local image model is installed");
                failure_row(deliverable, f.status, &f.error.code, &f.error.message)
            }
            (Ok(images), Some(model)) => {
                let mut estimate: Option<Fail> = None;
                for (index, image) in images.iter().enumerate() {
                    let request = image_request(brief, deliverable, model, aspect, image, index);
                    if let Err(f) = guard_free(ctx, &request) {
                        estimate = Some(f);
                        break;
                    }
                }
                match estimate {
                    Some(f) => failure_row(deliverable, f.status, &f.error.code, &f.error.message),
                    None => json!({
                        "deliverable": label(deliverable),
                        "status": "would_generate",
                        "images": images.len(),
                        "model": model,
                        "aspect": aspect,
                        "est_usd": 0.0,
                        "review": json!({"mode": review_mode(brief), "eyes_gate": if review_mode(brief) == "none" { "not_required" } else { "required" }}),
                    }),
                }
            }
        };
        rows.push(row);
    }
    json!({
        "dry_run": true,
        "would": {
            "command": "brief.run",
            "deliverables": rows,
            "skipped": skipped,
            "library": library_summary(library),
        },
    })
}

fn image_request(
    brief: &ContentBrief,
    deliverable: Deliverable,
    model: &str,
    aspect: &str,
    image: &Image,
    index: usize,
) -> Value {
    json!({
        "kind": "image",
        "provider": LOCAL_PROVIDER,
        "model": model,
        "prompt": image.prompt,
        "params": image_params(aspect),
        "request_id": request_id(brief, deliverable, model, aspect, image, index),
    })
}

/// The money gate: the existing dry-run estimate must be exactly 0 USD for the pinned keyless
/// provider before anything is submitted.
fn guard_free(ctx: &Context, request: &Value) -> Result<(), Fail> {
    let estimate = api::generate(ctx, request, true).map_err(|e| Fail::new("failed", e))?;
    match estimate["est_usd"].as_f64() {
        Some(usd) if usd <= 0.0 => Ok(()),
        other => Err(spend_refusal(&format!(
            "the {LOCAL_PROVIDER} route estimated {other:?} USD"
        ))),
    }
}

/// A generated image, verified and ready to place.
struct Produced {
    src: PathBuf,
    sha256: String,
    mime: String,
    media: Value,
    prompt: String,
    slide_copy: Option<String>,
    provenance: Value,
}

/// Generate every image of one deliverable, place the files in the project, validate and write
/// the envelope and the manifest. Returns the `deliverables` row.
#[allow(clippy::too_many_arguments)]
fn deliver(
    ctx: &Context,
    providers: &[Box<dyn Provider>],
    project: &Path,
    brief: &ContentBrief,
    policy_revision: u64,
    deliverable: Deliverable,
    images: &[Image],
    model: &str,
    aspect: &str,
    deadline: Option<Instant>,
    library: &LibraryTarget,
) -> Result<Value, Fail> {
    let name = label(deliverable);
    let mut produced: Vec<Produced> = Vec::with_capacity(images.len());
    for (index, image) in images.iter().enumerate() {
        step(
            &brief.brief_id,
            name,
            "generate",
            &format!("{}/{}", index + 1, images.len()),
        );
        let request = image_request(brief, deliverable, model, aspect, image, index);
        guard_free(ctx, &request)?;
        let mut live = request.clone();
        live["wait"] = json!(true);
        if let Some(left) = remaining(deadline) {
            if left.is_zero() {
                return Err(timeout_fail());
            }
            live["wait_timeout_s"] = json!(left.as_secs_f64());
        }
        // In this process, through the run's warm provider: the model stays loaded between images.
        let record = api::generate_wait_with(ctx, providers, &live, &run_options())
            .map_err(|e| Fail::new("failed", e))?;
        // A brief that stops waiting (timeout) leaves nothing running: a local job holds a
        // ~10 GB model server, and nobody will collect the result.
        cancel_if_unfinished(ctx, providers, &record);
        produced.push(collect(&record, &request, image)?);
    }

    // The human-eyes gate: what the brief's review mode requires of these files, checked before a
    // single one is placed under exports/. Refused deliveries leave the generated files in the library.
    step(&brief.brief_id, name, "review", "start");
    let review = review_gate(brief, &produced)?;
    step(
        &brief.brief_id,
        name,
        "review",
        review["eyes_gate"].as_str().unwrap_or("unknown"),
    );

    step(&brief.brief_id, name, "place", "start");
    let mut files: Vec<Value> = Vec::new();
    let mut stills: Vec<Still> = Vec::new();
    let mut hashes: Vec<String> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    let mut provenance: Vec<Value> = Vec::new();
    let mut library_rows: Vec<Value> = Vec::new();
    for p in &produced {
        let bytes = std::fs::read(&p.src)
            .map_err(|e| io_fail(format!("reading {}", p.src.display()), e))?;
        if library::sha256_hex(&bytes) != p.sha256 {
            return Err(fail(
                "failed",
                "asset_corrupt",
                format!("{} does not match its recorded sha256", p.src.display()),
            ));
        }
        let (width, height) = dimensions(&bytes, &p.mime, &p.media).ok_or_else(|| {
            fail(
                "failed",
                "invalid_output",
                format!("cannot read the dimensions of {}", p.src.display()),
            )
        })?;
        let ext = match p.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            other => {
                return Err(fail(
                    "failed",
                    "invalid_output",
                    format!("{other} is not an image type the delivery envelope allows"),
                ))
            }
        };
        let blake3_hex = rightkit_fs::digest::blake3_bytes(&bytes);
        let dst = project
            .join("exports")
            .join("images")
            .join(format!("{blake3_hex}.{ext}"));
        place(&p.src, &dst, &blake3_hex)?;
        // The project copy is placed and verified; the library registration reports its own outcome.
        library_rows.push(register_image(library, p, &bytes, aspect));
        let path = rel(project, &dst);
        files.push(json!({"role": "image", "path": path, "blake3": format!("blake3:{blake3_hex}"), "bytes": bytes.len()}));
        outputs.push(path);
        stills.push(Still {
            hash: format!("blake3:{blake3_hex}"),
            mime_type: p.mime.clone(),
            width,
            height,
            alt: alt_text(&p.prompt),
        });
        hashes.push(blake3_hex);
        provenance.push(p.provenance.clone());
    }

    let payload = match deliverable {
        Deliverable::Stills => Payload::Stills { stills },
        Deliverable::Carousel => Payload::Carousel {
            slides: stills
                .into_iter()
                .zip(&produced)
                .enumerate()
                .map(|(i, (still, p))| Slide {
                    index: (i + 1) as u32,
                    still,
                    slide_copy: p.slide_copy.clone(),
                    step_ids: Vec::new(),
                })
                .collect(),
        },
        Deliverable::Video | Deliverable::Text => {
            return Err(fail(
                "failed",
                "internal",
                format!("{name} is not a deliverable of this engine"),
            ))
        }
    };

    let id_hash = rightkit_fs::digest::blake3_bytes(hashes.join("").as_bytes());
    let suffix = format!("-r{}-{name}-{}", brief.revision, &id_hash[..12]);
    let keep = 128usize.saturating_sub(suffix.len());
    let envelope_id = format!(
        "{}{suffix}",
        brief.brief_id.chars().take(keep).collect::<String>()
    );
    let stem = envelope_id.replace(':', "_");

    let envelope = DeliveryEnvelope {
        contract: Contract::DeliveryEnvelope.name().to_string(),
        contract_version: Contract::DeliveryEnvelope.supported_version().to_string(),
        envelope_id: envelope_id.clone(),
        brief_id: brief.brief_id.clone(),
        brief_revision: brief.revision,
        // The assets are generated from this brief revision; no other content revision exists.
        content_revision: brief.revision,
        policy_revision,
        rule_revision: brief.rule_revision,
        rule_revision_ref: brief.rule_revision_ref.clone(),
        producer: Producer {
            engine: Engine::Genright,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        platforms: brief.platforms.clone(),
        copy: None,
        payload,
    };
    let document = serde_json::to_value(&envelope).map_err(|e| {
        fail(
            "failed",
            "internal",
            format!("serialising the delivery envelope: {e}"),
        )
    })?;
    rightkit_contracts::validate(Contract::DeliveryEnvelope, &document).map_err(|e| {
        fail(
            "failed",
            "envelope_invalid",
            format!("delivery envelope {envelope_id} failed contract validation: {e}"),
        )
    })?;

    let manifests = project.join("exports").join("manifests");
    let envelope_path = manifests.join(format!("{stem}.envelope.json"));
    write_json(&envelope_path, &document)?;
    let envelope_bytes = std::fs::read(&envelope_path)
        .map_err(|e| io_fail(format!("reading {}", envelope_path.display()), e))?;
    files.push(json!({
        "role": "envelope",
        "path": rel(project, &envelope_path),
        "blake3": format!("blake3:{}", rightkit_fs::digest::blake3_bytes(&envelope_bytes)),
        "bytes": envelope_bytes.len(),
    }));
    let manifest = json!({
        "schema": "genright.export-manifest.v1",
        "brief": {
            "id": brief.brief_id,
            "revision": brief.revision,
            "type": serde_json::to_value(brief.brief_type).unwrap_or(Value::Null),
            "hook": brief.hook,
        },
        "deliverable": name,
        "platforms": brief.platforms,
        "aspect": aspect,
        "files": files,
        "provenance": provenance,
        "spend": {"authorised": false, "route": LOCAL_PROVIDER, "est_usd": 0.0},
        "review": review,
        "envelope": rel(project, &envelope_path),
        "status": "delivered",
    });
    let manifest_path = manifests.join(format!("{stem}.manifest.json"));
    write_json(&manifest_path, &manifest)?;
    step(&brief.brief_id, name, "manifest", "delivered");

    Ok(json!({
        "deliverable": name,
        "platforms": brief.platforms,
        "status": "delivered",
        "review": review,
        "outputs": outputs,
        "library": {"caller": library.caller.slug, "label": library.caller.label, "images": library_rows},
        "envelope": rel(project, &envelope_path),
        "manifest": rel(project, &manifest_path),
    }))
}

/// The brief's `review.mode` as the eyes gate names it. A brief without `review` cannot get here
/// (`execute` requires `review.policyRevision`); if it ever did, it would be gated like `manual`.
fn review_mode(brief: &ContentBrief) -> &'static str {
    match brief.review.as_ref().map(|r| r.mode) {
        Some(ReviewMode::None) => "none",
        Some(ReviewMode::AutoWhenClean) => "auto_when_clean",
        Some(ReviewMode::Manual) | Option::None => "manual",
    }
}

/// `manual` and `auto_when_clean` deliver only assets a person approved in the app and have not
/// changed since (`eyes_approval_required` otherwise, naming each asset and its state); `none`
/// does not require it and the result says so.
fn review_gate(brief: &ContentBrief, produced: &[Produced]) -> Result<Value, Fail> {
    let files: Vec<PathBuf> = produced.iter().map(|p| p.src.clone()).collect();
    rightkit_approvals::eyes::review_gate(review_mode(brief), &files, "brief delivery")
        .map_err(|error| Fail::new("refused", error))
}

/// The run's providers: the production registry with `local-image` replaced by a provider that
/// keeps the model server loaded between this run's consecutive jobs.
fn run_providers() -> Vec<Box<dyn Provider>> {
    let mut providers = rightkit_media_gen::providers::registry();
    providers.retain(|p| p.id() != rightkit_media_gen::providers::local_image::ID);
    providers.push(Box::new(LocalImage::warm_for_run()));
    providers
}

/// End of the run: dropping the warm provider stops the model server and releases the
/// machine-wide lock now, with no idle grace.
fn release(providers: Vec<Box<dyn Provider>>) {
    drop(providers);
}

/// In-process run options: a short poll so an image is noticed within a second of finishing.
fn run_options() -> RunOptions {
    RunOptions {
        poll_initial: Duration::from_millis(500),
        poll_max: Duration::from_secs(2),
        ..RunOptions::default()
    }
}

/// Cancel a job `brief run` is abandoning (its wait timed out while the job was still going).
fn cancel_if_unfinished(ctx: &Context, providers: &[Box<dyn Provider>], record: &Value) {
    let unfinished = record["timed_out"] == true
        || matches!(record["status"].as_str(), Some("submitted" | "running"));
    if let (true, Some(job_id)) = (unfinished, record["job_id"].as_str()) {
        let _ = api::job_cancel_with(ctx, providers, &json!({"job_id": job_id}));
    }
}

fn timeout_fail() -> Fail {
    fail(
        "timed_out",
        "timeout",
        "the --timeout-secs budget ran out; unfinished jobs keep their normal recoverable state",
    )
}

/// Turn a finished (or not) `generate --wait` job record into one verified image.
fn collect(record: &Value, request: &Value, image: &Image) -> Result<Produced, Fail> {
    let job_id = record["job_id"].as_str().unwrap_or("");
    if record["timed_out"] == true {
        return Err(timeout_fail());
    }
    match record["status"].as_str() {
        Some("complete") => {}
        Some("outcome_unknown") => {
            return Err(fail(
                "failed",
                "outcome_unknown",
                format!("job {job_id} is held with an unknown outcome; brief run never resubmits it (resolve it with `job resolve`)"),
            ))
        }
        Some("submitted" | "running") => {
            return Err(fail("timed_out", "job_unfinished", format!("job {job_id} is not finished and keeps its recoverable state")))
        }
        _ => {
            let code = record["error"]["code"].as_str().unwrap_or("job_failed");
            let message = record["error"]["message"].as_str().unwrap_or("the generation job did not complete");
            return Err(fail("failed", code, format!("job {job_id}: {message}")));
        }
    }
    let assets: Vec<&Value> = record["result"]["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|a| a["mime"].as_str().is_some_and(|m| m.starts_with("image/")))
                .collect()
        })
        .unwrap_or_default();
    let [asset] = assets.as_slice() else {
        return Err(fail(
            "failed",
            "invalid_output",
            format!(
                "job {job_id} returned {} image assets, expected 1",
                assets.len()
            ),
        ));
    };
    let text = |key: &str| {
        asset[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let (Some(path), Some(sha256), Some(mime)) = (text("path"), text("sha256"), text("mime"))
    else {
        return Err(fail(
            "failed",
            "invalid_output",
            format!("job {job_id} returned an asset without path, sha256 or mime"),
        ));
    };
    Ok(Produced {
        src: PathBuf::from(path),
        sha256: sha256.clone(),
        mime,
        media: asset["media"].clone(),
        prompt: image.prompt.clone(),
        slide_copy: image.slide_copy.clone(),
        provenance: json!({
            "asset_id": asset["asset_id"],
            "job_id": record["job_id"],
            "request_id": request["request_id"],
            "provider": asset["provider"],
            "model": asset["model"],
            "sha256": sha256,
            "prompt": image.prompt,
            "est_usd": record["est_usd"],
            "actual_usd": record["actual_usd"],
        }),
    })
}

/// PNG header first (exact), else the dimensions the library probed.
fn dimensions(bytes: &[u8], mime: &str, media: &Value) -> Option<(u32, u32)> {
    if mime == "image/png"
        && bytes.len() >= 24
        && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && &bytes[12..16] == b"IHDR"
    {
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return (width > 0 && height > 0).then_some((width, height));
    }
    let dim = |key: &str| {
        media[key]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
    };
    Some((dim("width")?, dim("height")?))
}

fn alt_text(prompt: &str) -> String {
    format!("Generated image: {prompt}")
        .chars()
        .take(MAX_ALT)
        .collect()
}

/// Put the library file in the project under its content address. The library stays the system
/// of record: clone or copy (never a hard link), verified by hash. A file already there with the
/// same hash is a rerun and is kept.
fn place(src: &Path, dst: &Path, blake3_hex: &str) -> Result<(), Fail> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_fail(format!("creating {}", parent.display()), e))?;
    }
    let same = |path: &Path| -> Result<bool, Fail> {
        let digest = rightkit_fs::digest::blake3_file(path)
            .map_err(|e| io_fail(format!("reading {}", path.display()), e))?;
        Ok(digest == blake3_hex)
    };
    if let Ok(meta) = std::fs::symlink_metadata(dst) {
        if meta.file_type().is_file() && same(dst)? {
            return Ok(());
        }
        return Err(fail(
            "failed",
            "asset_conflict",
            format!("{} already exists with different content", dst.display()),
        ));
    }
    rightkit_fs::clone::clone_or_link_or_copy_nofollow_with(
        src,
        dst,
        &rightkit_fs::CloneOptions::default().without_hard_link(),
    )
    .map_err(|e| io_fail(format!("placing {}", dst.display()), e))?;
    if !same(dst)? {
        return Err(fail(
            "failed",
            "asset_corrupt",
            format!(
                "{} differs from the generated image after placement",
                dst.display()
            ),
        ));
    }
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<(), Fail> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_fail(format!("creating {}", parent.display()), e))?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| {
        fail(
            "failed",
            "internal",
            format!("serialising {}: {e}", path.display()),
        )
    })?;
    bytes.push(b'\n');
    rightkit_fs::atomic_write(path, &bytes)
        .map_err(|e| io_fail(format!("writing {}", path.display()), e))
}

/// Project-relative path with `/` separators.
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
