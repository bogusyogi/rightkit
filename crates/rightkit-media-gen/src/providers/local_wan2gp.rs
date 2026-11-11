//! Discovery of a user-installed Wan2GP checkout (issue #79).
//!
//! Wan2GP is Python and is installed by the user outside the app, so the shipped binary
//! contains no Python. This module only finds the checkout, checks the interpreter it will run
//! under, and records the git revision when the checkout is a git clone. Every probe is a child
//! process started through `rightkit-child-process`, bounded by a timeout and stopped on expiry.
//!
//! Documented Wan2GP surface (github.com/deepbeepmeep/Wan2GP): the web UI is `python wgp.py`,
//! a saved queue runs headless with `python wgp.py --process <queue.zip>`, and the install route
//! is a git clone with Python 3.11.x on RTX 20XX to 50XX or 3.10.x on GTX 10XX. The docs name
//! no version flag, so the version is the git revision, not a Wan2GP-reported string.
//!
//! Generation (the `Provider` impl below) writes one Wan2GP settings `.json` per job and runs
//! `python wgp.py --process <settings.json> --output-dir <dir>` (docs/CLI.md in the Wan2GP
//! repository: `--process` takes a `.zip` queue or a `.json` settings file, `--output-dir`
//! overrides the output folder, exit code 0 means every task completed, 1 means failure, 130
//! means interrupted). The child is owned by a monitor thread through `rightkit-child-process`;
//! progress is the last percentage Wan2GP printed to its log, cancel terminates the process tree,
//! and the newest `.mp4` in the job's output folder is handed to the library as `Output::Path`.
//! UNVERIFIED against a real install: the settings keys (`model_type`, `prompt`, `resolution`,
//! `video_length`, `seed`, `num_inference_steps`, `image_prompt_type`, `image_start`), the
//! 16 fps frame math, and the progress text format. The docs name no settings schema.

use super::{
    Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError,
    SubmitOutcome,
};
use rightkit_child_process::{OwnedCommand, RunOptions};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

pub const ID: &str = "local-wan2gp";
/// Web UI and headless entry script in a Wan2GP checkout.
pub const ENTRY: &str = "wgp.py";
/// Environment variable naming the Wan2GP checkout directory.
pub const DIR_ENV: &str = "RIGHTKIT_WAN2GP_DIR";
/// Environment variable naming the Python interpreter to run Wan2GP with.
pub const PYTHON_ENV: &str = "RIGHTKIT_WAN2GP_PYTHON";
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// A discovered, probed Wan2GP installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wan2GpInstall {
    pub root: PathBuf,
    pub entry: PathBuf,
    pub python: PathBuf,
    /// `major.minor.patch` from `python --version`.
    pub python_version: String,
    /// Short git revision of the checkout, when it is a git clone and git answers.
    pub revision: Option<String>,
}

/// Candidate checkout directories for the platform, in preference order.
fn candidate_roots(tools_root: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(root) = tools_root else {
        return Vec::new();
    };
    vec![
        root.join("shared").join("tools").join("wan2gp"),
        root.join("tools").join("wan2gp"),
    ]
}

/// The checkout to use: `RIGHTKIT_WAN2GP_DIR` when set and non-empty (passed through, so a bad
/// path fails with its own error), otherwise the first default that contains `wgp.py`.
pub fn resolve_root(env: Option<OsString>, tools_root: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(value) = env.filter(|v| !v.to_string_lossy().trim().is_empty()) {
        return Some(PathBuf::from(value));
    }
    candidate_roots(tools_root)
        .into_iter()
        .find(|root| root.join(ENTRY).is_file())
}

/// Parse `Python 3.11.14` (or a bare `3.11.14`) into its three numbers.
pub fn parse_python_version(output: &str) -> Option<(u32, u32, u32)> {
    let first = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let version = first.strip_prefix("Python").unwrap_or(first).trim();
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Wan2GP's documented interpreters: 3.10.x (GTX 10XX route) and 3.11.x (RTX 20XX to 50XX route).
pub fn python_version_supported(version: (u32, u32, u32)) -> bool {
    matches!((version.0, version.1), (3, 10) | (3, 11))
}

/// Argv for a headless run of a saved Wan2GP queue, as documented.
pub fn headless_args(queue: &Path) -> Vec<OsString> {
    vec![
        OsString::from(ENTRY),
        OsString::from("--process"),
        queue.as_os_str().to_owned(),
    ]
}

/// Run a short probe to completion or until the timeout, returning its stdout.
fn probe(program: &Path, args: &[OsString]) -> Result<String, ProviderError> {
    let out = rightkit_child_process::run(program, args, &RunOptions::new(PROBE_TIMEOUT))
        .map_err(|e| unavailable(format!("could not run {}: {e}", program.display())))?;
    if out.timed_out {
        return Err(unavailable(format!(
            "{} did not answer within {}s",
            program.display(),
            PROBE_TIMEOUT.as_secs()
        )));
    }
    if !out.status.success() {
        return Err(unavailable(format!(
            "{} exited with {}",
            program.display(),
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout.bytes).into_owned())
}

/// Discover and probe the installation at `root` using `python`. Pure checks run first, so a
/// missing entry script never starts a child process.
pub fn discover(root: &Path, python: &Path) -> Result<Wan2GpInstall, ProviderError> {
    let root = root.canonicalize().map_err(|e| {
        unavailable(format!(
            "Wan2GP directory is unavailable at {}: {e}",
            root.display()
        ))
    })?;
    let entry = root.join(ENTRY);
    if !entry.is_file() {
        return Err(unavailable(format!(
            "{ENTRY} is missing from {}",
            root.display()
        )));
    }
    let output = probe(python, &[OsString::from("--version")])?;
    let version = parse_python_version(&output)
        .ok_or_else(|| unavailable("Python version probe returned unexpected output"))?;
    if !python_version_supported(version) {
        return Err(unavailable(format!(
            "Wan2GP documents Python 3.10 or 3.11; {} is {}.{}.{}",
            python.display(),
            version.0,
            version.1,
            version.2
        )));
    }
    let revision = if root.join(".git").exists() {
        probe(
            Path::new("git"),
            &[
                OsString::from("-C"),
                root.as_os_str().to_owned(),
                OsString::from("rev-parse"),
                OsString::from("--short"),
                OsString::from("HEAD"),
            ],
        )
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|rev| !rev.is_empty())
    } else {
        None
    };
    Ok(Wan2GpInstall {
        root,
        entry,
        python: python.to_path_buf(),
        python_version: format!("{}.{}.{}", version.0, version.1, version.2),
        revision,
    })
}

/// Discover from the environment and platform defaults. `None` when no checkout is found.
pub fn discover_from_env(
    tools_root: Option<PathBuf>,
) -> Option<Result<Wan2GpInstall, ProviderError>> {
    let root = resolve_root(std::env::var_os(DIR_ENV), tools_root)?;
    let python = std::env::var_os(PYTHON_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python"));
    Some(discover(&root, &python))
}

fn unavailable(message: impl Into<String>) -> ProviderError {
    ProviderError::new("not_installed", message, true)
}

const MODELS_LIST: [ModelInfo; 2] = [
    ModelInfo {
        provider: ID,
        model: "wan2gp-t2v",
        kind: Kind::Video,
        label: "Wan2GP text-to-video (your install, free)",
        est_usd: 0.0,
        durations: Durations::range(1, 10, Some(5)),
    },
    ModelInfo {
        provider: ID,
        model: "wan2gp-i2v",
        kind: Kind::Video,
        label: "Wan2GP image-to-video (your install, free)",
        est_usd: 0.0,
        durations: Durations::range(1, 10, Some(5)),
    },
];
const KNOWN_PARAMS: [&str; 6] = [
    "duration",
    "resolution",
    "seed",
    "steps",
    "model_type",
    "image_path",
];
/// Frames per second assumed for the duration to `video_length` conversion (Wan 2.1 family; unverified for other model types).
const FRAMES_PER_SECOND: u32 = 16;
const RUN_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const MONITOR_TICK: Duration = Duration::from_millis(100);
/// Environment variable overriding where per-job folders live (tests); default is under the organisation root.
pub const JOBS_ENV: &str = "RIGHTKIT_WAN2GP_JOBS";

/// Where per-job folders live: `RIGHTKIT_WAN2GP_JOBS`, else `<organisation root>/wan2gp-jobs`.
pub fn jobs_dir() -> Option<PathBuf> {
    std::env::var_os(JOBS_ENV)
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| super::tools_root().map(|r| r.join("wan2gp-jobs")))
}

pub struct LocalWan2Gp {
    jobs: Option<PathBuf>,
    owner_token: String,
}

impl LocalWan2Gp {
    pub fn new() -> Self {
        let jobs = jobs_dir();
        Self {
            jobs,
            owner_token: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn jobs_root(&self) -> Result<&Path, ProviderError> {
        self.jobs
            .as_deref()
            .ok_or_else(|| unavailable("no folder is available for Wan2GP job files"))
    }

    fn install(&self) -> Result<Wan2GpInstall, ProviderError> {
        discover_from_env(super::tools_root()).unwrap_or_else(|| {
            Err(unavailable(format!(
                "Wan2GP is not installed; set {DIR_ENV} to your Wan2GP checkout"
            )))
        })
    }

    fn checked(&self, req: &GenRequest) -> Result<(), ProviderError> {
        if req.kind != Kind::Video {
            return Err(invalid("local-wan2gp only produces video"));
        }
        if !MODELS_LIST.iter().any(|m| m.model == req.model) {
            return Err(invalid(format!("unknown local-wan2gp model {}", req.model)));
        }
        if req.prompt.trim().is_empty() {
            return Err(invalid("local-wan2gp needs a prompt"));
        }
        if let Some(map) = req.params.as_object() {
            if let Some(key) = map.keys().find(|k| !KNOWN_PARAMS.contains(&k.as_str())) {
                return Err(invalid(format!("local-wan2gp does not take params.{key}")));
            }
        }
        if let Some(d) = req.params.get("duration") {
            match d.as_u64() {
                Some(1..=10) => {}
                _ => {
                    return Err(invalid(
                        "params.duration must be a whole number of seconds from 1 to 10",
                    ))
                }
            }
        }
        if let Some(r) = req.params.get("resolution") {
            let ok = r.as_str().is_some_and(|r| {
                r.split_once('x')
                    .is_some_and(|(w, h)| w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok())
            });
            if !ok {
                return Err(invalid("params.resolution must look like 832x480"));
            }
        }
        for key in ["seed", "steps"] {
            if req
                .params
                .get(key)
                .is_some_and(|v| !v.is_i64() && !v.is_u64())
            {
                return Err(invalid(format!("params.{key} must be a whole number")));
            }
        }
        if let Some(m) = req.params.get("model_type") {
            if !m.as_str().is_some_and(|m| {
                !m.is_empty()
                    && m.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
            }) {
                return Err(invalid(
                    "params.model_type must be a Wan2GP model type name",
                ));
            }
        }
        let image = req
            .params
            .get("image_path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        match (req.model.as_str(), image) {
            ("wan2gp-i2v", None) => {
                return Err(invalid(
                    "wan2gp-i2v needs params.image_path, a local image file",
                ))
            }
            ("wan2gp-t2v", Some(_)) => {
                return Err(invalid("wan2gp-t2v takes no image; use wan2gp-i2v"))
            }
            (_, Some(path)) if !Path::new(path).is_absolute() || !Path::new(path).is_file() => {
                return Err(invalid(
                    "params.image_path must be an absolute path to an existing file",
                ))
            }
            _ => {}
        }
        Ok(())
    }

    fn start(
        &self,
        req: &GenRequest,
        job_id: &str,
        attempt: i64,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.checked(req)?;
        if !safe_id(job_id) {
            return Err(invalid("job id is not safe for a Wan2GP job folder"));
        }
        let install = self.install()?;
        let job_dir = self.jobs_root()?.join(job_id);
        let dir = job_dir.join(format!("a{attempt}"));
        let out = dir.join("out");
        std::fs::create_dir_all(&out)
            .map_err(|e| unavailable(format!("could not create the Wan2GP job folder: {e}")))?;
        let settings_path = dir.join("settings.json");
        rightkit_fs::atomic_write(
            &settings_path,
            &serde_json::to_vec_pretty(&settings_json(req)).unwrap_or_default(),
        )
        .map_err(|e| unavailable(format!("could not write {}: {e}", settings_path.display())))?;
        let log = rightkit_fs::open_private_append(&dir.join("run.log"))
            .map_err(|e| unavailable(format!("could not create the Wan2GP log: {e}")))?;
        let log_err = log
            .try_clone()
            .map_err(|e| unavailable(format!("could not share the Wan2GP log: {e}")))?;
        let mut args = headless_args(&settings_path);
        args.push(OsString::from("--output-dir"));
        args.push(out.as_os_str().to_owned());
        let mut command = OwnedCommand::new(install.python.clone());
        command
            .command_mut()
            .args(&args)
            .current_dir(&install.root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        let child = command
            .spawn()
            .map_err(|e| unavailable(format!("could not start Wan2GP: {e}")))?;
        let state_path = dir.join("state.json");
        write_state(
            &state_path,
            &json!({"state": "running", "owner_pid": std::process::id(), "owner_token": self.owner_token}),
        )?;
        let (cancel_path, token) = (job_dir.join("cancel.json"), self.owner_token.clone());
        std::thread::spawn(move || monitor(child, state_path, cancel_path, token));
        Ok(SubmitOutcome::Accepted {
            provider_job_id: format!("{attempt}:{job_id}"),
        })
    }

    fn attempt_dir(&self, provider_id: &str) -> Option<PathBuf> {
        let (attempt, job_id) = provider_id.split_once(':')?;
        if !safe_id(job_id) || attempt.parse::<u32>().is_err() {
            return None;
        }
        Some(self.jobs.as_ref()?.join(job_id).join(format!("a{attempt}")))
    }
}

impl Default for LocalWan2Gp {
    fn default() -> Self {
        Self::new()
    }
}

/// The Wan2GP settings document for one request. UNVERIFIED key names (see module docs).
fn settings_json(req: &GenRequest) -> Value {
    let p = &req.params;
    let image = p.get("image_path").and_then(Value::as_str);
    let default_type = if image.is_some() { "i2v" } else { "t2v" };
    let seconds = p.get("duration").and_then(Value::as_u64).unwrap_or(5) as u32;
    let mut v = json!({
        "model_type": p.get("model_type").and_then(Value::as_str).unwrap_or(default_type),
        "prompt": req.prompt,
        "resolution": p.get("resolution").and_then(Value::as_str).unwrap_or("832x480"),
        "video_length": seconds * FRAMES_PER_SECOND + 1,
        "seed": p.get("seed").and_then(Value::as_i64).unwrap_or(-1),
    });
    if let Some(steps) = p.get("steps").and_then(Value::as_i64) {
        v["num_inference_steps"] = json!(steps);
    }
    if let Some(path) = image {
        v["image_prompt_type"] = json!("S");
        v["image_start"] = json!(path);
    }
    v
}

fn monitor(
    mut child: rightkit_child_process::OwnedChild,
    state_path: PathBuf,
    cancel_path: PathBuf,
    token: String,
) {
    let started = Instant::now();
    let base = |state: &str| json!({"state": state, "owner_pid": std::process::id(), "owner_token": token});
    let finish = |v: Value| {
        let _ = write_state(&state_path, &v);
    };
    loop {
        if cancel_path.exists() {
            let _ = child.terminate_tree_bounded(Duration::from_secs(5));
            return finish(base("cancelled"));
        }
        // Owner wait: blocks up to one tick and returns the moment the leader exits.
        match child.wait_timeout(MONITOR_TICK) {
            Ok(Some(status)) => {
                let mut v = base("exited");
                v["success"] = json!(status.success());
                v["exit_code"] = json!(status.code());
                return finish(v);
            }
            Ok(None) if started.elapsed() < RUN_TIMEOUT => {}
            Ok(None) => {
                let _ = child.terminate_tree_bounded(Duration::from_secs(5));
                return finish(base("timeout"));
            }
            Err(e) => {
                let _ = child.terminate_tree_bounded(Duration::from_secs(5));
                let mut v = base("failed");
                v["message"] = json!(format!("could not observe Wan2GP: {e}"));
                return finish(v);
            }
        }
    }
}

/// Last `NN%` Wan2GP (tqdm) printed to its log, as a progress line.
pub fn last_percent(log_tail: &str) -> Option<String> {
    let mut found = None;
    for chunk in log_tail.split(['\r', '\n']) {
        if let Some(i) = chunk.find('%') {
            let rev: String = chunk[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let digits: String = rev.chars().rev().collect();
            if let Ok(n) = digits.parse::<u32>() {
                if n <= 100 {
                    found = Some(format!("Wan2GP generating: {n}%"));
                }
            }
        }
    }
    found
}

fn log_tail(path: &Path) -> String {
    rightkit_logs::tail_bytes(path, 4096)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Newest `.mp4` under `out` (Wan2GP names files itself; the folder is private to the job).
fn newest_video(out: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![out.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
            {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| modified >= *t) {
                    best = Some((modified, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

fn adopt(path: &Path) -> Result<Output, ProviderError> {
    let (sha256, total) = rightkit_fs::digest::sha256_file_with_len(path)
        .map_err(|e| unavailable(format!("could not read the Wan2GP output: {e}")))?;
    Ok(Output::Path {
        path: path.to_path_buf(),
        mime: "video/mp4".into(),
        sha256,
        bytes: total,
    })
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn write_state(path: &Path, v: &Value) -> Result<(), ProviderError> {
    rightkit_fs::atomic_write(path, v.to_string().as_bytes())
        .map_err(|e| unavailable(format!("could not write {}: {e}", path.display())))
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", message, false)
}

impl Provider for LocalWan2Gp {
    fn id(&self) -> &'static str {
        ID
    }
    fn models(&self) -> &'static [ModelInfo] {
        &MODELS_LIST
    }
    fn capabilities(&self, model: &str) -> crate::capabilities::Capabilities {
        use crate::capabilities::{self as c, Need};
        let caps = c::Capabilities::default()
            .mode("prompt")
            .setting(
                "duration",
                json!({"param": "duration", "unit": "s", "min": 1, "max": 10, "integer": true}),
            )
            .setting(
                "resolution",
                c::resolution(
                    "resolution",
                    &["832x480", "480x832", "1280x720", "720x1280"],
                    false,
                    Some("832x480"),
                ),
            )
            .setting("seed", c::seed());
        if model == "wan2gp-i2v" {
            caps.frames(Need::Required, "image_path", Need::None, "")
        } else {
            caps.frames(Need::None, "", Need::None, "")
        }
    }
    fn needs_key(&self) -> bool {
        false
    }
    /// A local engine: the same attempt re-run costs nothing and writes to its own folder.
    fn submit_is_idempotent(&self) -> bool {
        true
    }
    fn runtime_is_external(&self) -> bool {
        true
    }
    fn can_cancel(&self) -> bool {
        true
    }
    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.checked(req)?;
        Ok(0.0)
    }
    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        Some(self.install().map(|_| ()))
    }
    fn test_key(&self, _api_key: &str) -> Result<(), ProviderError> {
        self.install().map(|_| ())
    }
    fn submit(&self, req: &GenRequest, _api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.start(req, &format!("manual-{}", uuid::Uuid::new_v4()), 1)
    }
    fn submit_job(
        &self,
        req: &GenRequest,
        _api_key: &str,
        job_id: &str,
        attempt: i64,
        _idempotency_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.start(req, job_id, attempt)
    }
    fn poll(
        &self,
        _req: &GenRequest,
        provider_job_id: &str,
        _api_key: &str,
    ) -> Result<PollOutcome, ProviderError> {
        let dir = self
            .attempt_dir(provider_job_id)
            .ok_or_else(|| invalid("invalid local Wan2GP provider job id"))?;
        let state: Value = match std::fs::read(dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(v) => v,
            None => {
                return Ok(PollOutcome::Failed {
                    message: "Wan2GP job state is missing; the job will be resubmitted".into(),
                })
            }
        };
        match state["state"].as_str().unwrap_or("") {
            "running" => {
                if state["owner_pid"].as_u64() == Some(std::process::id() as u64)
                    && state["owner_token"].as_str() == Some(self.owner_token.as_str())
                {
                    Ok(PollOutcome::Running)
                } else {
                    Ok(PollOutcome::Failed {
                        message: "Wan2GP runner was lost; the job will be resubmitted".into(),
                    })
                }
            }
            "cancelled" => Err(ProviderError::new(
                "cancelled",
                "Wan2GP generation was cancelled",
                false,
            )),
            "timeout" => Ok(PollOutcome::Failed {
                message: "Wan2GP exceeded its two-hour limit".into(),
            }),
            "failed" => Ok(PollOutcome::Failed {
                message: state["message"]
                    .as_str()
                    .unwrap_or("Wan2GP failed")
                    .to_owned(),
            }),
            "exited" if state["success"] == Value::Bool(true) => {
                match newest_video(&dir.join("out")) {
                    Some(path) => Ok(PollOutcome::Completed {
                        outputs: vec![adopt(&path)?],
                    }),
                    None => Ok(PollOutcome::Failed {
                        message: "Wan2GP exited cleanly but wrote no .mp4 file".into(),
                    }),
                }
            }
            "exited" => {
                let code = state["exit_code"]
                    .as_i64()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".into());
                let log = log_tail(&dir.join("run.log"));
                let chars: Vec<char> = log.trim().chars().collect();
                let tail: String = chars[chars.len().saturating_sub(400)..].iter().collect();
                Ok(PollOutcome::Failed {
                    message: format!("Wan2GP exited with code {code}: {tail}"),
                })
            }
            _ => Ok(PollOutcome::Failed {
                message: "Wan2GP job state is unrecognised".into(),
            }),
        }
    }
    fn cancel_job(
        &self,
        _req: &GenRequest,
        _provider_job_id: Option<&str>,
        job_id: &str,
    ) -> Result<(), ProviderError> {
        if !safe_id(job_id) {
            return Err(invalid("job id is not safe for cancellation"));
        }
        let dir = self.jobs_root()?.join(job_id);
        if dir.is_dir() {
            rightkit_fs::atomic_write(&dir.join("cancel.json"), br#"{"cancel":true}"#)
                .map_err(|e| unavailable(format!("could not write cancel.json: {e}")))?;
        }
        Ok(())
    }
    fn progress_message(&self, _req: &GenRequest, provider_job_id: &str) -> Option<String> {
        let dir = self.attempt_dir(provider_job_id)?;
        Some(
            last_percent(&log_tail(&dir.join("run.log")))
                .unwrap_or_else(|| "Wan2GP is starting".into()),
        )
    }
    fn fetch(&self, _output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        Err(invalid(
            "local Wan2GP path outputs are adopted directly by the library",
        ))
    }
}
