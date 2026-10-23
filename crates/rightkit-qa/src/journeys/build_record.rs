//! The build record: what the build lane hands to the runner lane (ADR-0001 item 1, Sage C6/C9).
//!
//! `journeys run` never compiles. It spawns `rustright cargo ... --no-run --message-format=json`
//! (the broker is the only compile path), and this module turns what came back into a
//! `build-record.json` the runner can trust at launch time:
//!
//! * **Attestation.** The broker child exited 0, cargo said `build-finished` with `success: true`,
//!   and stderr names the broker request (`{prefix}: request {id}`, `rustright-cli/src/cli.rs:350`).
//!   That line is a text line, not a contract: the first match is taken, a second different id or a
//!   `(exit N)` suffix with `N != 0` refuses the build. A structured id is an open ADR-0002 item.
//! * **Artifacts.** Only executables named by `compiler-artifact` records of THIS build are accepted
//!   (never a path found beside the source, never a dev target dir). Each is copied into the run
//!   store (`<run>/build/bin/<role>/<file>`) while being hashed, the copy is re-hashed, and the
//!   QA marker is measured. The runner's own sha256 is what the record holds.
//! * **Source identity.** Git `head`, `tree`, `dirty` and, when dirty, a digest of the tracked diff
//!   plus every untracked file, taken before and after the build. A change in between refuses the
//!   build (`source_mismatch`).
//! * **Inputs.** Lockfile hashes, the feature set, the build command, the manifest hash and the
//!   toolchain pin file. [`BuildRecord::stale_against`] lists every input that moved, and
//!   [`BuildRecord::verify_launch`] refuses a stale record, then re-hashes the binary
//!   ([`crate::binary::check_launch`]).
//!
//! Nothing here runs a compiler or tests. The process spawns are `git` (read-only) and the broker.
use super::receipt::{self, BuildArtifact, ProducedBy, Source};
use super::{code, is_hex_lower, is_safe_relative, Problem};
use crate::binary::{self, LaunchCheck, LaunchRefusal, MarkerScanner};
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{canonical_plain, err, tail, Error, Result};
use rightkit_fs::digest::Sha256Hasher;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const BUILD_RECORD_FILE: &str = "build-record.json";
pub const BUILD_RECORD_SCHEMA: &str = "rightkit.journeys.build-record/1";
/// Where the copied binaries live, relative to the run directory.
pub const STORE_DIR: &str = "build/bin";
/// The only text this release reads the broker request id from.
pub const REQUEST_ID_SOURCE: &str = "stderr_request_line";
/// Hard deadline for the broker build (the broker has its own queue and caps).
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// The program whose `cargo` subcommand is the broker. Anything else is a compile outside the broker.
const BROKER_PROGRAM: &str = "rustright";
/// The RustRight client's retired name (rustright#91): refused by name, with the replacement.
const RETIRED_BROKER: &str = "rightkit";
/// Lockfiles recorded when found at or above the build directory (inside the repository).
pub const LOCKFILE_NAMES: [&str; 4] = [
    "Cargo.lock",
    "pnpm-lock.yaml",
    "package-lock.json",
    "yarn.lock",
];
/// rustup toolchain pin files, nearest first.
const TOOLCHAIN_PIN_NAMES: [&str; 2] = ["rust-toolchain.toml", "rust-toolchain"];
/// Roles that name a binary which is NOT an app with the control server: the adapter's own test
/// executable and the engine. Every other role must carry the QA control marker. A role in this
/// list is a decision about the binary, not a guess: the manifest has no marker field yet, so the
/// table is the single place to change (open question for the manifest owner).
pub const MARKER_NOT_APPLICABLE_ROLES: [&str; 2] = ["adapter", "engine"];

fn coded(c: &str, detail: impl std::fmt::Display) -> Error {
    Error(format!("{c}: {detail}"))
}

// ------------------------------------------------------------------------------------ types

/// Whether a recorded binary must carry the QA control marker at launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerPolicy {
    Required,
    NotApplicable,
}

impl MarkerPolicy {
    /// [`MARKER_NOT_APPLICABLE_ROLES`] are exempt; every other role (and every unknown one) is
    /// required to carry the marker.
    pub fn for_role(role: &str) -> Self {
        if MARKER_NOT_APPLICABLE_ROLES.contains(&role) {
            MarkerPolicy::NotApplicable
        } else {
            MarkerPolicy::Required
        }
    }

    pub fn required(self) -> bool {
        self == MarkerPolicy::Required
    }
}

/// A path (relative to the repository root, `/` separated) and the SHA-256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHash {
    pub path: String,
    pub sha256: String,
}

/// The cargo feature selection a build command asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSet {
    pub requested: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
}

/// What the runner observed about the broker build. `attested` is true only when every field
/// below holds; a record that is not attested is never written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    pub request_id: String,
    pub request_id_source: String,
    pub exit_code: i32,
    pub build_finished: bool,
    pub attested: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    /// `rustc -vV` text cargo cached beside the artifacts (`.rustc_info.json`); best effort, recorded
    /// not re-derived, so it is information and never compared.
    pub rustc: Option<String>,
    /// The nearest `rust-toolchain(.toml)`; compared.
    pub pin_file: Option<FileHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedArtifact {
    pub role: String,
    pub target: String,
    pub package: String,
    pub kind: Vec<String>,
    pub test_profile: bool,
    pub features: Vec<String>,
    /// Where cargo said the executable was (the broker's target dir; information only).
    pub source_path: String,
    /// The copy the runner launches, relative to the run directory.
    pub store_path: String,
    pub size: u64,
    pub sha256: String,
    /// Measured on the store copy at build time.
    pub qa_marker: bool,
    pub marker: MarkerPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecord {
    pub schema: String,
    pub run_id: String,
    pub app: String,
    pub host_os: String,
    pub host_arch: String,
    pub manifest: FileHash,
    /// The final broker argv (a missing `--message-format` is added, nothing else is).
    pub command: Vec<String>,
    /// The broker program path the runner actually spawned (`command[0]` resolved on PATH, or the
    /// absolute path it named). Recorded, not compared: a moved broker shows here, not as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_path: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    /// Source identity before the build, and again after it. They are equal in a written record.
    pub source: Source,
    pub source_after: Source,
    pub broker: Attestation,
    pub lockfiles: Vec<FileHash>,
    pub features: FeatureSet,
    pub toolchain: Toolchain,
    pub artifacts: Vec<RecordedArtifact>,
}

/// One declared binary: its role and the cargo package or target that produces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSpec {
    pub role: String,
    pub cargo_target: Option<String>,
    pub cargo_package: Option<String>,
    pub marker: MarkerPolicy,
}

/// A validated, compile-only broker build.
#[derive(Debug, Clone)]
pub struct BuildPlan {
    pub name: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub artifacts: Vec<ArtifactSpec>,
}

impl BuildPlan {
    pub fn new(
        name: &str,
        command: &[String],
        cwd: PathBuf,
        artifacts: Vec<ArtifactSpec>,
    ) -> Result<BuildPlan> {
        let command = validate_build_command(command)?;
        if artifacts.is_empty() {
            return err("a build plan declares no artifact");
        }
        let mut roles = BTreeSet::new();
        for a in &artifacts {
            if !role_ok(&a.role) {
                return err(format!(
                    "artifact role {:?} must be letters, digits, '.', '_' or '-' (at most 64) and not start with '.'",
                    a.role
                ));
            }
            if !roles.insert(a.role.as_str()) {
                return err(format!("artifact role {:?} is declared twice", a.role));
            }
        }
        Ok(BuildPlan {
            name: name.to_string(),
            command,
            cwd,
            artifacts,
        })
    }
}

fn role_ok(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 64
        && !role.starts_with('.')
        && role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// What the broker child produced. `code` is its exit code (`None` when it was killed).
#[derive(Debug, Clone)]
pub struct RawBuild {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// The broker program path actually spawned (set by [`spawn_broker`]); `None` for any other spawn.
    /// The build record stores it (`broker_path`), so the record names the binary that built it.
    pub broker: Option<String>,
}

// ---------------------------------------------------------------------------- build command

/// The words before a `--` separator (everything after it belongs to a test binary).
fn before_separator(command: &[String]) -> &[String] {
    match command.iter().position(|a| a == "--") {
        Some(i) => &command[..i],
        None => command,
    }
}

/// Check that `command` is a compile-only broker build and return it with `--message-format=json`
/// added when absent. Allowed: `rustright cargo build ...` and
/// `rustright cargo test ... --no-run ...`. Refused: any other program (the retired `rightkit` by name) or subcommand, a
/// leading flag or `+toolchain` before the subcommand, `test` without `--no-run` (tests never run
/// in the build lane), a `--message-format` that is not JSON, and `--target-dir`, `--out-dir` or
/// `--artifact-dir` (a target outside the broker's is a violation, not something to cap).
pub fn validate_build_command(command: &[String]) -> Result<Vec<String>> {
    let Some(program) = command.first() else {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            "the build command is empty",
        ));
    };
    let stem = Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if stem == RETIRED_BROKER {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            "`rightkit` is the retired name of the RustRight client (rustright#91); the build command is `rustright cargo ...`",
        ));
    }
    if stem != BROKER_PROGRAM || command.get(1).map(String::as_str) != Some("cargo") {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            format!(
                "build command must be `rustright cargo ...` (the broker is the only compile path), got {:?}",
                &command[..command.len().min(2)]
            ),
        ));
    }
    let Some(sub) = command.get(2) else {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            "build command names no cargo subcommand",
        ));
    };
    if sub.starts_with('-') || sub.starts_with('+') {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            format!("build command must name the cargo subcommand first, got {sub:?}"),
        ));
    }
    let words = before_separator(command);
    match sub.as_str() {
        "build" => {}
        "test" => {
            if !words.iter().skip(3).any(|a| a == "--no-run") {
                return Err(coded(
                    code::BUILD_NOT_ATTESTED,
                    "`cargo test` in the build lane needs --no-run: tests never run in the broker",
                ));
            }
        }
        other => {
            return Err(coded(
                code::BUILD_NOT_ATTESTED,
                format!("the build lane allows only `build` and `test --no-run`, not {other:?}"),
            ));
        }
    }
    let mut format_value: Option<String> = None;
    let mut i = 3;
    while i < words.len() {
        let a = words[i].as_str();
        for banned in ["--target-dir", "--out-dir", "--artifact-dir"] {
            if a == banned || a.starts_with(&format!("{banned}=")) {
                return Err(coded(
                    code::BUILD_NOT_ATTESTED,
                    format!("{banned} is refused: the broker owns the target directory"),
                ));
            }
        }
        if a == "--message-format" {
            i += 1;
            format_value = words.get(i).cloned();
        } else if let Some(v) = a.strip_prefix("--message-format=") {
            format_value = Some(v.to_string());
        }
        i += 1;
    }
    let mut out = command.to_vec();
    match format_value.as_deref() {
        Some(v) if v.split(',').any(|p| p.starts_with("json")) => {}
        Some(v) => {
            return Err(coded(
                code::BUILD_NOT_ATTESTED,
                format!("--message-format {v:?} is not JSON: artifacts are read from compiler-artifact records"),
            ));
        }
        None => {
            let at = words.len();
            out.insert(at, "--message-format=json".to_string());
        }
    }
    Ok(out)
}

/// The `--features` / `-F` selection of a build command (words before `--` only), sorted and
/// de-duplicated, plus the two flags that change the default set.
pub fn features_of(command: &[String]) -> FeatureSet {
    let args = before_separator(command);
    let mut set = BTreeSet::new();
    let mut all = false;
    let mut no_default = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let value: Option<&str> = if a == "--features" || a == "-F" {
            i += 1;
            args.get(i).map(String::as_str)
        } else if let Some(v) = a.strip_prefix("--features=") {
            Some(v)
        } else if a.starts_with("-F") && a.len() > 2 && !a.starts_with("--") {
            Some(&a[2..])
        } else {
            if a == "--all-features" {
                all = true;
            }
            if a == "--no-default-features" {
                no_default = true;
            }
            None
        };
        if let Some(v) = value {
            for f in v.split([',', ' ']) {
                let f = f.trim();
                if !f.is_empty() {
                    set.insert(f.to_string());
                }
            }
        }
        i += 1;
    }
    FeatureSet {
        requested: set.into_iter().collect(),
        all_features: all,
        no_default_features: no_default,
    }
}

fn manifest_path_arg(command: &[String]) -> Option<String> {
    let args = before_separator(command);
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--manifest-path" {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = args[i].strip_prefix("--manifest-path=") {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
}

// ------------------------------------------------------------------------ broker build output

fn valid_request_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}

/// The broker request id from the stderr line `{prefix}: request {id}` (prefix `rustright` or
/// `rustright managed-agent`, the client's only prefixes). The first matching line wins. Lines of other shapes
/// are ignored, so compiler text cannot supply an id; a second, different id (a re-run after a
/// reconnect) or an `... (exit N)` suffix with `N != 0` refuses the build, because then the text
/// no longer describes one successful request.
pub fn parse_request_id(stderr: &str) -> Result<String> {
    let mut ids: Vec<String> = Vec::new();
    for line in stderr.lines() {
        let line = line.trim();
        let Some((prefix, rest)) = line.split_once(": request ") else {
            continue;
        };
        let prefix_ok = prefix == "rustright" || prefix.starts_with("rustright ");
        if !prefix_ok || prefix.len() > 40 {
            continue;
        }
        let mut words = rest.split_whitespace();
        let Some(id) = words.next().filter(|id| valid_request_id(id)) else {
            continue;
        };
        if let Some(tail_text) = rest.split_once('(').map(|(_, t)| t) {
            if let Some(n) = tail_text
                .strip_prefix("exit ")
                .and_then(|t| t.strip_suffix(')'))
                .and_then(|t| t.trim().parse::<i64>().ok())
            {
                if n != 0 {
                    return Err(coded(
                        code::BUILD_NOT_ATTESTED,
                        format!("stderr reports request {id} ended with exit {n}"),
                    ));
                }
            }
        }
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }
    match ids.len() {
        0 => Err(coded(
            code::BUILD_NOT_ATTESTED,
            "stderr carries no broker request line (`rustright: request <id>`), so the build cannot be tied to a broker job",
        )),
        1 => Ok(ids.remove(0)),
        n => Err(coded(
            code::BUILD_NOT_ATTESTED,
            format!("stderr names {n} different broker requests; refusing to pick one"),
        )),
    }
}

/// One `compiler-artifact` record, reduced to the fields the runner uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerArtifact {
    pub package: String,
    pub target: String,
    pub kind: Vec<String>,
    pub test_profile: bool,
    pub features: Vec<String>,
    pub executable: Option<PathBuf>,
    pub fresh: bool,
}

/// What the JSON lines of a `--message-format=json` build said.
#[derive(Debug, Clone, Default)]
pub struct ParsedBuild {
    pub artifacts: Vec<CompilerArtifact>,
    /// `Some(true)` when every `build-finished` record said success, `Some(false)` when one said
    /// otherwise, `None` when there was none.
    pub build_finished: Option<bool>,
}

/// The package name of a cargo `package_id`: `name 0.1.0 (path+file:///x)` (older cargo),
/// `path+file:///x/crates/name#name@0.1.0` and `...#0.1.0` (the name is then the directory).
pub fn package_name_of(package_id: &str) -> Option<String> {
    let id = package_id.trim();
    if id.contains(' ') {
        // The older form never has the `url#name@version` shape; a `#` inside its url is not one.
        return id.split_whitespace().next().map(str::to_string);
    }
    if let Some((url, frag)) = id.split_once('#') {
        if let Some((name, _version)) = frag.rsplit_once('@') {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        let seg = url.trim_end_matches('/').rsplit('/').next()?;
        return if seg.is_empty() {
            None
        } else {
            Some(seg.to_string())
        };
    }
    id.split_whitespace().next().map(str::to_string)
}

/// Parse the JSON lines of a cargo build. Lines that are not JSON objects are ignored (a broker
/// banner or libtest noise), and so are reasons this module does not use.
pub fn parse_cargo_json(stdout: &str) -> ParsedBuild {
    let mut out = ParsedBuild::default();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["reason"].as_str() {
            Some("build-finished") => {
                let ok = v["success"].as_bool().unwrap_or(false);
                out.build_finished = Some(out.build_finished.unwrap_or(true) && ok);
            }
            Some("compiler-artifact") => {
                let strings = |x: &Value| -> Vec<String> {
                    x.as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                out.artifacts.push(CompilerArtifact {
                    package: v["package_id"]
                        .as_str()
                        .and_then(package_name_of)
                        .unwrap_or_default(),
                    target: v["target"]["name"].as_str().unwrap_or("").to_string(),
                    kind: strings(&v["target"]["kind"]),
                    test_profile: v["profile"]["test"].as_bool().unwrap_or(false),
                    features: strings(&v["features"]),
                    executable: v["executable"].as_str().map(PathBuf::from),
                    fresh: v["fresh"].as_bool().unwrap_or(false),
                });
            }
            _ => {}
        }
    }
    out
}

/// The one `compiler-artifact` executable that `spec` names, or an error saying why not.
/// A spec with neither a cargo target nor a package is refused (a prebuilt path is not produced
/// by this build). Duplicate records of one executable path count once. A package without a
/// target matches its non-test binaries only; a target that matches both a test-profile and a
/// normal binary takes the normal one. More than one remaining executable is ambiguous and refused.
pub fn select_artifact<'a>(
    spec: &ArtifactSpec,
    all: &'a [CompilerArtifact],
) -> Result<&'a CompilerArtifact> {
    if spec.cargo_target.is_none() && spec.cargo_package.is_none() {
        return Err(coded(
            code::ARTIFACT_NOT_FROM_BUILD,
            format!(
                "role {:?} names no cargo_target or cargo_package: only artifacts produced by this build are accepted",
                spec.role
            ),
        ));
    }
    let mut seen: BTreeSet<&Path> = BTreeSet::new();
    let mut matching: Vec<&CompilerArtifact> = all
        .iter()
        .filter(|a| a.executable.is_some())
        .filter(|a| spec.cargo_target.as_deref().is_none_or(|t| a.target == t))
        .filter(|a| spec.cargo_package.as_deref().is_none_or(|p| a.package == p))
        .filter(|a| seen.insert(a.executable.as_deref().unwrap()))
        .collect();
    let is_normal_bin =
        |a: &&CompilerArtifact| !a.test_profile && a.kind.iter().any(|k| k == "bin");
    if spec.cargo_target.is_none() || matching.len() > 1 {
        let normal: Vec<&CompilerArtifact> =
            matching.iter().copied().filter(is_normal_bin).collect();
        if spec.cargo_target.is_none() || !normal.is_empty() {
            matching = normal;
        }
    }
    match matching.len() {
        1 => Ok(matching[0]),
        0 => Err(coded(
            code::ARTIFACT_NOT_FROM_BUILD,
            format!(
                "this build produced no executable for role {:?} (cargo_target {:?}, cargo_package {:?})",
                spec.role, spec.cargo_target, spec.cargo_package
            ),
        )),
        n => Err(coded(
            code::ARTIFACT_NOT_FROM_BUILD,
            format!(
                "role {:?} is ambiguous: {n} executables match ({}); name a cargo_target",
                spec.role,
                matching
                    .iter()
                    .map(|a| a.executable.as_ref().unwrap().display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// `rustc -vV` text from `.rustc_info.json`, which cargo keeps in the target directory above its
/// artifacts. Best effort: `None` when the file is absent or has another shape.
fn rustc_from_target_dir(exe: &Path) -> Option<String> {
    for dir in exe.ancestors().skip(1).take(8) {
        let info = dir.join(".rustc_info.json");
        if !info.is_file() {
            continue;
        }
        let v: Value = serde_json::from_slice(&std::fs::read(&info).ok()?).ok()?;
        for entry in v["outputs"].as_object()?.values() {
            let Some(text) = entry["stdout"].as_str() else {
                continue;
            };
            if text.contains("\nrelease:") && text.contains("\nhost:") {
                return Some(text.trim().to_string());
            }
        }
        return None;
    }
    None
}

// ------------------------------------------------------------------------- the build process

/// Spawn `program args` in `cwd` with a hard deadline, capturing both streams. Used for the broker
/// build; the process tree is owned and killed on timeout (`process::run_owned`).
pub fn spawn_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    tracker: &Tracker,
    timeout: Duration,
) -> Result<RawBuild> {
    let out = run_owned(
        program,
        args,
        &RunOptions {
            cwd: Some(cwd.to_path_buf()),
            env: env.to_vec(),
            timeout: Some(timeout),
            label: "journeys build".into(),
            ..Default::default()
        },
        tracker,
    )?;
    Ok(RawBuild {
        code: out.code,
        stdout: out.stdout,
        stderr: out.stderr,
        timed_out: out.timed_out,
        broker: None,
    })
}

/// Run the plan's broker command. The runner spawns it; nothing in this crate compiles. The program
/// is resolved on PATH first ([`resolve_broker`]), so a Windows `.cmd` shim is spawned by full path.
pub fn spawn_broker(plan: &BuildPlan) -> Result<RawBuild> {
    let Some((program, args)) = plan.command.split_first() else {
        return err("build command is empty");
    };
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let resolved = resolve_broker(program, &path_var, cfg!(windows))?;
    let spawned = resolved.to_string_lossy().into_owned();
    let mut raw = spawn_command(
        &spawned,
        args,
        &plan.cwd,
        &[],
        &Tracker::new(),
        BUILD_TIMEOUT,
    )?;
    raw.broker = Some(spawned);
    Ok(raw)
}

/// The broker name the runner accepts. The retired `rightkit` is refused, never resolved in its place.
pub const BROKER_NAMES: [&str; 1] = [BROKER_PROGRAM];

/// Resolve the broker program against `path_var` (the PATH to walk, in order). The command's own
/// name is tried first, then the other alias. On Windows each directory is tried with the
/// extensions `.exe`, `.cmd` and `.bat`, in that order. The first existing file wins; nothing is
/// copied and no fallback is taken. Refuses with `broker_not_found` when neither name resolves.
pub fn resolve_broker(program: &str, path_var: &std::ffi::OsStr, windows: bool) -> Result<PathBuf> {
    // An absolute `command[0]` is the broker itself: it is spawned as named, never swapped for a
    // PATH copy, and a missing file is refused rather than replaced.
    if Path::new(program).is_absolute() {
        if Path::new(program).is_file() {
            return Ok(PathBuf::from(program));
        }
        return Err(coded(
            "broker_not_found",
            format!("the broker program {program} is not a file"),
        ));
    }
    let stem = Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if stem == RETIRED_BROKER {
        return Err(coded(
            "broker_not_found",
            "`rightkit` is the retired name of the RustRight client (rustright#91); name `rustright`",
        ));
    }
    let mut names: Vec<&str> = BROKER_NAMES.to_vec();
    if let Some(i) = names.iter().position(|n| *n == stem) {
        names.swap(0, i);
    }
    for name in &names {
        if let Some(hit) = find_on_path(name, path_var, windows) {
            return Ok(hit);
        }
    }
    Err(coded(
        "broker_not_found",
        "`rustright` does not resolve on PATH; put the broker on PATH (the Windows shim rustright.cmd counts)",
    ))
}

fn find_on_path(name: &str, path_var: &std::ffi::OsStr, windows: bool) -> Option<PathBuf> {
    let exts: &[&str] = if windows {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(path_var) {
        // An empty PATH entry means the current directory to the OS loader; the broker is never
        // looked up there (N-B5 of the Oracle recheck).
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in exts {
            let candidate = if ext.is_empty() {
                dir.join(name)
            } else {
                dir.join(format!("{name}.{ext}"))
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------- source and inputs

fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd
        .output()
        .map_err(|e| Error(format!("cannot run git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        return err(format!(
            "git {} failed: {}",
            args.join(" "),
            tail(String::from_utf8_lossy(&out.stderr).trim(), 400)
        ));
    }
    Ok(out.stdout)
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_bytes(dir, args)?)
        .trim()
        .to_string())
}

/// The repository root that contains `dir`.
pub fn git_top(dir: &Path) -> Result<PathBuf> {
    let top = git_text(dir, &["rev-parse", "--show-toplevel"])?;
    if top.is_empty() {
        return err(format!("{} is not inside a git work tree", dir.display()));
    }
    Ok(PathBuf::from(top))
}

/// Whether `file` (inside the work tree `top`) exists in the committed tree at `HEAD`. A scenario
/// the runner reads must be bound to a committed source: an ignored, untracked or staged-only file
/// is not, and is refused. A git failure answers `false` (refused), never `true`.
pub fn committed_at_head(file: &Path) -> Result<bool> {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
        return Ok(false);
    };
    // `HEAD:./<name>` is resolved against the directory git runs in, so no path-prefix matching
    // between the file and the work tree root is needed (the forms differ on Windows).
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["cat-file", "-e"])
        .arg(format!("HEAD:./{}", name.to_string_lossy()))
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, as `git_bytes` does
    }
    let out = cmd
        .output()
        .map_err(|e| Error(format!("cannot run git cat-file: {e}")))?;
    Ok(out.status.success())
}

fn diff_digest(top: &Path) -> Result<String> {
    let tracked = git_bytes(
        top,
        &[
            "diff",
            "HEAD",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
        ],
    )?;
    let untracked = git_bytes(top, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut h = Sha256Hasher::new();
    h.update(b"rightkit-source-diff/1\0");
    h.update(&(tracked.len() as u64).to_le_bytes());
    h.update(&tracked);
    let mut names: Vec<&[u8]> = untracked
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .collect();
    names.sort();
    for name in names {
        h.update(&(name.len() as u64).to_le_bytes());
        h.update(name);
        let path = top.join(String::from_utf8_lossy(name).into_owned());
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| Error(format!("cannot stat untracked {}: {e}", path.display())))?;
        if meta.file_type().is_symlink() {
            h.update(b"L");
            let target = std::fs::read_link(&path)
                .map_err(|e| Error(format!("cannot read link {}: {e}", path.display())))?;
            h.update(target.to_string_lossy().as_bytes());
        } else if meta.is_dir() {
            h.update(b"D");
        } else {
            h.update(b"F");
            let sha = rightkit_fs::digest::sha256_file(&path)
                .map_err(|e| Error(format!("cannot hash untracked {}: {e}", path.display())))?;
            h.update(sha.as_bytes());
        }
    }
    Ok(h.finalize())
}

/// Git identity of the work tree at `top`: `head`, the tree of `head`, whether anything differs
/// from it (tracked changes or untracked files that are not ignored) and, when dirty, a digest of
/// all of that. Two identities are equal only when the checkout is byte-identical in everything
/// git sees, so a file edited during a build changes it even though `head` and `tree` stay put.
pub fn source_identity(top: &Path) -> Result<Source> {
    let head = git_text(top, &["rev-parse", "HEAD"])?;
    let tree = git_text(top, &["rev-parse", "HEAD^{tree}"])?;
    if !is_hex_lower(&head, 40) || !is_hex_lower(&tree, 40) {
        return err(format!(
            "git head {head:?} / tree {tree:?} are not 40-hex ids (only SHA-1 repositories are supported)"
        ));
    }
    let status = git_bytes(
        top,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let dirty = !status.is_empty();
    let diff_sha256 = if dirty { Some(diff_digest(top)?) } else { None };
    Ok(Source {
        head,
        tree,
        dirty,
        diff_sha256,
    })
}

fn rel_slash(top: &Path, p: &Path) -> String {
    p.strip_prefix(top)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Lockfiles at or above each of `start_dirs`, up to the repository root `top`. Cargo's lockfile
/// is required (a build without one has no pinned dependencies). Sorted by path.
pub fn find_lockfiles(top: &Path, start_dirs: &[PathBuf]) -> Result<Vec<FileHash>> {
    let top_c = canonical_plain(top)
        .map_err(|e| Error(format!("cannot resolve {}: {e}", top.display())))?;
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for start in start_dirs {
        let start_c = canonical_plain(start)
            .map_err(|e| Error(format!("cannot resolve {}: {e}", start.display())))?;
        if !start_c.starts_with(&top_c) {
            return err(format!(
                "{} is outside the repository {}",
                start_c.display(),
                top_c.display()
            ));
        }
        for dir in start_c.ancestors() {
            if !dir.starts_with(&top_c) {
                break;
            }
            for name in LOCKFILE_NAMES {
                let p = dir.join(name);
                if !p.is_file() {
                    continue;
                }
                let rel = rel_slash(&top_c, &p);
                if found.contains_key(&rel) {
                    continue;
                }
                let sha = rightkit_fs::digest::sha256_file(&p)
                    .map_err(|e| Error(format!("cannot hash {}: {e}", p.display())))?;
                found.insert(rel, sha);
            }
        }
    }
    if !found
        .keys()
        .any(|k| k == "Cargo.lock" || k.ends_with("/Cargo.lock"))
    {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            "no Cargo.lock at or above the build directory: a build without a lockfile has no pinned dependencies",
        ));
    }
    Ok(found
        .into_iter()
        .map(|(path, sha256)| FileHash { path, sha256 })
        .collect())
}

fn find_toolchain_pin(top: &Path, cwd: &Path) -> Result<Option<FileHash>> {
    let top_c = canonical_plain(top)
        .map_err(|e| Error(format!("cannot resolve {}: {e}", top.display())))?;
    let cwd_c = canonical_plain(cwd)
        .map_err(|e| Error(format!("cannot resolve {}: {e}", cwd.display())))?;
    for dir in cwd_c.ancestors() {
        if !dir.starts_with(&top_c) {
            break;
        }
        for name in TOOLCHAIN_PIN_NAMES {
            let p = dir.join(name);
            if p.is_file() {
                let sha256 = rightkit_fs::digest::sha256_file(&p)
                    .map_err(|e| Error(format!("cannot hash {}: {e}", p.display())))?;
                return Ok(Some(FileHash {
                    path: rel_slash(&top_c, &p),
                    sha256,
                }));
            }
        }
    }
    Ok(None)
}

/// Everything a build record is bound to, measured now. Gathered before the build, after it, and
/// again at launch; [`BuildRecord::stale_against`] compares a record with one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentInputs {
    pub manifest_sha256: String,
    pub source: Source,
    pub lockfiles: Vec<FileHash>,
    pub features: FeatureSet,
    pub command: Vec<String>,
    pub toolchain_pin: Option<FileHash>,
}

impl CurrentInputs {
    pub fn gather(app_root: &Path, plan: &BuildPlan, manifest_sha256: &str) -> Result<Self> {
        let top = git_top(app_root)?;
        let source = source_identity(&top)?;
        let mut dirs = vec![plan.cwd.clone()];
        if let Some(mp) = manifest_path_arg(&plan.command) {
            let p = plan.cwd.join(mp);
            dirs.push(
                p.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| plan.cwd.clone()),
            );
        }
        Ok(CurrentInputs {
            manifest_sha256: manifest_sha256.to_string(),
            source,
            lockfiles: find_lockfiles(&top, &dirs)?,
            features: features_of(&plan.command),
            command: plan.command.clone(),
            toolchain_pin: find_toolchain_pin(&top, &plan.cwd)?,
        })
    }

    /// Problems when `after` (measured once the build ended) is not what was measured before it.
    pub fn stability_problems(&self, after: &CurrentInputs) -> Vec<Problem> {
        let mut p = Vec::new();
        if self.source != after.source {
            p.push(Problem::new(
                code::SOURCE_MISMATCH,
                None,
                format!(
                    "the source changed during the build: before {}, after {}",
                    describe_source(&self.source),
                    describe_source(&after.source)
                ),
            ));
        }
        if self.lockfiles != after.lockfiles {
            p.push(Problem::new(
                code::BUILD_RECORD_STALE,
                None,
                format!(
                    "a lockfile changed during the build: {}",
                    describe_lock_change(&self.lockfiles, &after.lockfiles)
                ),
            ));
        }
        if self.toolchain_pin != after.toolchain_pin {
            p.push(Problem::new(
                code::BUILD_RECORD_STALE,
                None,
                "the toolchain pin changed during the build",
            ));
        }
        p
    }
}

fn describe_source(s: &Source) -> String {
    format!(
        "tree {} dirty {} diff {}",
        s.tree,
        s.dirty,
        s.diff_sha256.as_deref().unwrap_or("-")
    )
}

fn describe_lock_change(old: &[FileHash], new: &[FileHash]) -> String {
    let o: BTreeMap<&str, &str> = old
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let n: BTreeMap<&str, &str> = new
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let mut parts = Vec::new();
    for (path, sha) in &n {
        match o.get(path) {
            None => parts.push(format!("{path} added")),
            Some(prev) if prev != sha => parts.push(format!("{path} changed")),
            _ => {}
        }
    }
    for path in o.keys() {
        if !n.contains_key(path) {
            parts.push(format!("{path} removed"));
        }
    }
    parts.join(", ")
}

// ------------------------------------------------------------------------------- recording

/// Copy `src` to a new file `dst` while hashing it and scanning it for the QA marker, then re-hash
/// the copy and require the same digest. Returns `(sha256, size, marker found)`. `dst` must not
/// exist. The permission bits of `src` are kept (an executable stays executable).
pub fn copy_hashed(src: &Path, dst: &Path) -> Result<(String, u64, bool)> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error(format!("cannot create {}: {e}", parent.display())))?;
    }
    let mut from = std::fs::File::open(src)
        .map_err(|e| Error(format!("cannot open {}: {e}", src.display())))?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|e| Error(format!("cannot create {}: {e}", dst.display())))?;
    let mut hasher = Sha256Hasher::new();
    let mut scanner = MarkerScanner::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = from
            .read(&mut buf)
            .map_err(|e| Error(format!("cannot read {}: {e}", src.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        scanner.feed(&buf[..n]);
        to.write_all(&buf[..n])
            .map_err(|e| Error(format!("cannot write {}: {e}", dst.display())))?;
        size += n as u64;
    }
    to.sync_all()
        .map_err(|e| Error(format!("cannot flush {}: {e}", dst.display())))?;
    drop(to);
    if let Ok(meta) = std::fs::metadata(src) {
        let _ = std::fs::set_permissions(dst, meta.permissions());
    }
    let sha256 = hasher.finalize();
    let again = rightkit_fs::digest::sha256_file(dst)
        .map_err(|e| Error(format!("cannot re-hash {}: {e}", dst.display())))?;
    if again != sha256 {
        return Err(coded(
            code::ARTIFACT_MISMATCH,
            format!(
                "the copy {} does not match what was read from {}",
                dst.display(),
                src.display()
            ),
        ));
    }
    Ok((sha256, size, scanner.found()))
}

/// Make `dir` (and parents) and keep it to the current user where the platform has modes.
fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Error(format!("cannot create {}: {e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Everything [`record_build`] needs.
pub struct RecordRequest<'a> {
    pub run_id: &'a str,
    pub app: &'a str,
    pub manifest_path: &'a str,
    pub plan: &'a BuildPlan,
    /// Inputs measured before the build.
    pub before: &'a CurrentInputs,
    /// The same inputs measured after it.
    pub after: &'a CurrentInputs,
    pub started_at: String,
    pub ended_at: String,
    pub raw: &'a RawBuild,
    pub run_dir: &'a Path,
}

/// Turn a finished broker build into a [`BuildRecord`], copying the binaries into
/// `<run_dir>/build/bin`. Refuses (with a message that starts with the problem code) when the build
/// is not attested, the source moved, a declared artifact is missing or ambiguous, or an app-class
/// binary lacks the QA marker. Nothing is written for a refused build except copies already made;
/// the caller owns the run directory and cleans it.
pub fn record_build(req: &RecordRequest) -> Result<BuildRecord> {
    let raw = req.raw;
    if raw.timed_out {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            "the broker build timed out",
        ));
    }
    if raw.code != Some(0) {
        return Err(coded(
            code::BUILD_NOT_ATTESTED,
            format!(
                "the broker build exited with {:?}: {}",
                raw.code,
                tail(raw.stderr.trim(), 2000)
            ),
        ));
    }
    let request_id = parse_request_id(&raw.stderr)?;
    let parsed = parse_cargo_json(&raw.stdout);
    match parsed.build_finished {
        Some(true) => {}
        Some(false) => {
            return Err(coded(
                code::BUILD_NOT_ATTESTED,
                "cargo reported build-finished with success: false",
            ))
        }
        None => {
            return Err(coded(
                code::BUILD_NOT_ATTESTED,
                "the build output has no build-finished record; cannot attest the build",
            ))
        }
    }
    if let Some(p) = req.before.stability_problems(req.after).first() {
        return Err(coded(&p.code, &p.detail));
    }
    let store = req.run_dir.join(STORE_DIR);
    create_private_dir(&store)?;
    let mut artifacts = Vec::new();
    let mut rustc: Option<String> = None;
    for spec in &req.plan.artifacts {
        let found = select_artifact(spec, &parsed.artifacts)?;
        let exe = found
            .executable
            .clone()
            .ok_or_else(|| Error(format!("role {:?} has no executable", spec.role)))?;
        let file_name = exe
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error(format!("{} has no file name", exe.display())))?
            .to_string();
        let meta = std::fs::symlink_metadata(&exe).map_err(|e| {
            coded(
                code::ARTIFACT_NOT_FROM_BUILD,
                format!("{} named by cargo does not exist: {e}", exe.display()),
            )
        })?;
        if !meta.is_file() {
            return Err(coded(
                code::ARTIFACT_NOT_FROM_BUILD,
                format!("{} named by cargo is not a regular file", exe.display()),
            ));
        }
        let dest = store.join(&spec.role).join(&file_name);
        let (sha256, size, marker) = copy_hashed(&exe, &dest)?;
        if spec.marker.required() && !marker {
            let _ = std::fs::remove_file(&dest);
            return Err(coded(
                code::QA_MARKER_MISSING,
                format!(
                    "{} (role {:?}) does not carry the QA control marker; it is not a test-mode/control build",
                    exe.display(),
                    spec.role
                ),
            ));
        }
        if rustc.is_none() {
            rustc = rustc_from_target_dir(&exe);
        }
        artifacts.push(RecordedArtifact {
            role: spec.role.clone(),
            target: found.target.clone(),
            package: found.package.clone(),
            kind: found.kind.clone(),
            test_profile: found.test_profile,
            features: found.features.clone(),
            source_path: exe.to_string_lossy().into_owned(),
            store_path: format!("{STORE_DIR}/{}/{file_name}", spec.role),
            size,
            sha256,
            qa_marker: marker,
            marker: spec.marker,
        });
    }
    let record = BuildRecord {
        schema: BUILD_RECORD_SCHEMA.to_string(),
        run_id: req.run_id.to_string(),
        app: req.app.to_string(),
        host_os: std::env::consts::OS.to_string(),
        host_arch: std::env::consts::ARCH.to_string(),
        manifest: FileHash {
            path: req.manifest_path.to_string(),
            sha256: req.before.manifest_sha256.clone(),
        },
        command: req.plan.command.clone(),
        broker_path: raw.broker.clone(),
        started_at: req.started_at.clone(),
        ended_at: req.ended_at.clone(),
        source: req.before.source.clone(),
        source_after: req.after.source.clone(),
        broker: Attestation {
            request_id,
            request_id_source: REQUEST_ID_SOURCE.to_string(),
            exit_code: 0,
            build_finished: true,
            attested: true,
        },
        lockfiles: req.before.lockfiles.clone(),
        features: req.before.features.clone(),
        toolchain: Toolchain {
            rustc,
            pin_file: req.before.toolchain_pin.clone(),
        },
        artifacts,
    };
    record.validate()?;
    Ok(record)
}

// ------------------------------------------------------------------------ reading and checks

/// Why a launch from a build record was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordRefusal {
    /// The record no longer matches its inputs.
    Stale(Vec<Problem>),
    /// The record holds no artifact with this role.
    UnknownRole(String),
    /// The recorded store path is unsafe or the binary failed [`binary::check_launch`].
    Binary {
        role: String,
        refusal: LaunchRefusal,
    },
}

impl RecordRefusal {
    pub fn problems(&self) -> Vec<Problem> {
        match self {
            RecordRefusal::Stale(p) => p.clone(),
            RecordRefusal::UnknownRole(role) => vec![Problem::new(
                code::ARTIFACT_NOT_FROM_BUILD,
                Some(role.as_str()),
                format!("the build record has no artifact with role {role:?}"),
            )],
            RecordRefusal::Binary { role, refusal } => vec![Problem::new(
                refusal.code(),
                Some(role.as_str()),
                refusal.to_string(),
            )],
        }
    }
}

impl std::fmt::Display for RecordRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .problems()
            .iter()
            .map(|p| format!("{}: {}", p.code, p.detail))
            .collect();
        f.write_str(&parts.join("; "))
    }
}

impl std::error::Error for RecordRefusal {}

impl From<RecordRefusal> for Error {
    fn from(r: RecordRefusal) -> Self {
        Error(r.to_string())
    }
}

fn list(v: &[String]) -> String {
    format!("[{}]", v.join(", "))
}

impl BuildRecord {
    /// Structural checks: schema, shapes of every hash and path, an attested broker build, at
    /// least one lockfile and artifact, unique roles, and store paths that stay in the store.
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| -> Result<()> { err(format!("build record: malformed {what}")) };
        if self.schema != BUILD_RECORD_SCHEMA {
            return err(format!(
                "build record schema is {:?}, expected {BUILD_RECORD_SCHEMA:?}",
                self.schema
            ));
        }
        if self.run_id.trim().is_empty() || self.app.trim().is_empty() || self.command.is_empty() {
            return bad("run_id, app or command");
        }
        for s in [&self.source, &self.source_after] {
            if !is_hex_lower(&s.head, 40) || !is_hex_lower(&s.tree, 40) {
                return bad("source head or tree (40 hex)");
            }
            if s.dirty != s.diff_sha256.is_some()
                || s.diff_sha256
                    .as_deref()
                    .is_some_and(|d| !is_hex_lower(d, 64))
            {
                return bad("source dirty / diff_sha256");
            }
        }
        if !is_hex_lower(&self.manifest.sha256, 64) || self.manifest.path.trim().is_empty() {
            return bad("manifest hash");
        }
        let a = &self.broker;
        if !a.attested
            || a.exit_code != 0
            || !a.build_finished
            || !valid_request_id(&a.request_id)
            || a.request_id_source.trim().is_empty()
        {
            return Err(coded(
                code::BUILD_NOT_ATTESTED,
                "the record does not attest an exit-0, build-finished broker build with a request id",
            ));
        }
        if self.lockfiles.is_empty()
            || self
                .lockfiles
                .iter()
                .chain(self.toolchain.pin_file.iter())
                .any(|f| !is_hex_lower(&f.sha256, 64) || !is_safe_relative(&f.path))
        {
            return bad("lockfiles");
        }
        if self.artifacts.is_empty() {
            return bad("artifacts (none)");
        }
        let mut roles = BTreeSet::new();
        for art in &self.artifacts {
            let prefix = format!("{STORE_DIR}/{}/", art.role);
            if !role_ok(&art.role)
                || !roles.insert(art.role.as_str())
                || !is_hex_lower(&art.sha256, 64)
                || !is_safe_relative(&art.store_path)
                || !art.store_path.starts_with(&prefix)
                || art.store_path.len() == prefix.len()
            {
                return bad(&format!("artifact {:?}", art.role));
            }
        }
        Ok(())
    }

    pub fn artifact(&self, role: &str) -> Option<&RecordedArtifact> {
        self.artifacts.iter().find(|a| a.role == role)
    }

    /// Read and validate `build-record.json`.
    pub fn read(path: &Path) -> Result<BuildRecord> {
        let bytes = std::fs::read(path)
            .map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        let rec: BuildRecord = serde_json::from_slice(&bytes)
            .map_err(|e| Error(format!("{}: {e}", path.display())))?;
        rec.validate()
            .map_err(|e| Error(format!("{}: {e}", path.display())))?;
        Ok(rec)
    }

    /// Write the record next to `path` and rename it into place, so a reader never sees half a file.
    pub fn write(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let tmp = path.with_extension(format!("tmp-{}", crate::util::new_id()));
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        std::fs::write(&tmp, bytes)
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                Error(format!("cannot write {}: {e}", path.display()))
            })
    }

    /// Every input that moved since this record was built. Empty means the record still describes
    /// the checkout: same manifest, command, features, lockfiles, toolchain pin and source.
    pub fn stale_against(&self, cur: &CurrentInputs) -> Vec<Problem> {
        let mut p = Vec::new();
        let mut stale =
            |detail: String| p.push(Problem::new(code::BUILD_RECORD_STALE, None, detail));
        if self.manifest.sha256 != cur.manifest_sha256 {
            stale(format!(
                "{} changed since the build (sha256 {} was {})",
                self.manifest.path, cur.manifest_sha256, self.manifest.sha256
            ));
        }
        if self.command != cur.command {
            stale(format!(
                "the build command changed: built with {:?}, now {:?}",
                self.command, cur.command
            ));
        }
        if self.features != cur.features {
            stale(format!(
                "the feature set changed: built with {} (all {}, no-default {}), now {} (all {}, no-default {})",
                list(&self.features.requested),
                self.features.all_features,
                self.features.no_default_features,
                list(&cur.features.requested),
                cur.features.all_features,
                cur.features.no_default_features
            ));
        }
        if self.lockfiles != cur.lockfiles {
            stale(format!(
                "a lockfile changed since the build: {}",
                describe_lock_change(&self.lockfiles, &cur.lockfiles)
            ));
        }
        if self.toolchain.pin_file != cur.toolchain_pin {
            stale("the toolchain pin file changed since the build".to_string());
        }
        for (label, rec) in [("before", &self.source), ("after", &self.source_after)] {
            if *rec != cur.source {
                p.push(Problem::new(
                    code::SOURCE_MISMATCH,
                    None,
                    format!(
                        "the source is not what was built ({label} the build: {}; now {})",
                        describe_source(rec),
                        describe_source(&cur.source)
                    ),
                ));
                break;
            }
        }
        p
    }

    /// The launch gate: refuse when the record is stale, then re-hash the binary of `role` (and
    /// require the QA marker when its policy says so) just before it is launched. The returned
    /// [`LaunchCheck`] holds the digest measured now, the one the receipt's `launched` records.
    pub fn verify_launch(
        &self,
        run_dir: &Path,
        role: &str,
        cur: &CurrentInputs,
    ) -> std::result::Result<LaunchCheck, RecordRefusal> {
        let stale = self.stale_against(cur);
        if !stale.is_empty() {
            return Err(RecordRefusal::Stale(stale));
        }
        let art = self
            .artifact(role)
            .ok_or_else(|| RecordRefusal::UnknownRole(role.to_string()))?;
        let prefix = format!("{STORE_DIR}/");
        if !is_safe_relative(&art.store_path) || !art.store_path.starts_with(&prefix) {
            return Err(RecordRefusal::Binary {
                role: role.to_string(),
                refusal: LaunchRefusal::NotARegularFile {
                    path: PathBuf::from(&art.store_path),
                },
            });
        }
        binary::check_launch(
            &run_dir.join(&art.store_path),
            &art.sha256,
            art.marker.required(),
        )
        .map_err(|refusal| RecordRefusal::Binary {
            role: role.to_string(),
            refusal,
        })
    }

    /// The receipt's `build` section for this record. `broker_receipt` stays `None`: the broker
    /// request id is recorded here, but no broker receipt file with tree and digests exists to
    /// bind (D5), so the artifacts count as `build_output`.
    pub fn to_receipt_build(&self, run_dir: &Path) -> receipt::Build {
        receipt::Build {
            command: self.command.clone(),
            source_tree: self.source.tree.clone(),
            tree_after_build: self.source_after.tree.clone(),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            broker_receipt: None,
            artifacts: self
                .artifacts
                .iter()
                .map(|a| BuildArtifact {
                    role: a.role.clone(),
                    path: run_dir.join(&a.store_path).to_string_lossy().into_owned(),
                    size: a.size,
                    sha256: a.sha256.clone(),
                    produced_by: ProducedBy::BuildOutput,
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::QA_CONTROL_MARKER;

    const REQ: &str = "096d6843-aaaa-4bbb-8ccc-0123456789ab";

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-brec-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn rep(c: char, n: usize) -> String {
        c.to_string().repeat(n)
    }

    fn source(tree: char) -> Source {
        Source {
            head: rep('a', 40),
            tree: rep(tree, 40),
            dirty: false,
            diff_sha256: None,
        }
    }

    fn artifact_line(
        pkg_id: &str,
        target: &str,
        kind: &str,
        test: bool,
        exe: Option<&Path>,
        features: &[&str],
    ) -> String {
        serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": pkg_id,
            "target": {"name": target, "kind": [kind]},
            "profile": {"test": test},
            "features": features,
            "executable": exe.map(|p| p.to_string_lossy().into_owned()),
            "fresh": false,
        })
        .to_string()
    }

    fn write_exe(path: &Path, marker: bool, filler: u8) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = vec![filler; 2048];
        if marker {
            bytes.extend_from_slice(QA_CONTROL_MARKER.as_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// A finished fake build: an adapter test binary and a marked app binary, both in `target`.
    #[allow(dead_code)]
    struct Fx {
        dir: PathBuf,
        adapter: PathBuf,
        app: PathBuf,
        stdout: String,
        stderr: String,
        plan: BuildPlan,
    }

    fn fixture(tag: &str, app_marker: bool) -> Fx {
        let dir = tmp(tag);
        let adapter = dir
            .join("target/debug/deps")
            .join("journeys_adapter-1234.exe");
        let app = dir.join("target/debug").join("fxapp.exe");
        write_exe(&adapter, false, 1);
        write_exe(&app, app_marker, 2);
        let stdout = [
            artifact_line(
                "path+file:///x/crates/fxlib#0.1.0",
                "fxlib",
                "lib",
                false,
                None,
                &[],
            ),
            artifact_line(
                "path+file:///x/fx#fx@0.1.0",
                "journeys_adapter",
                "test",
                true,
                Some(&adapter),
                &["qa"],
            ),
            artifact_line(
                "path+file:///x/crates/fxapp#0.1.0",
                "fxapp",
                "bin",
                false,
                Some(&app),
                &["qa"],
            ),
            r#"{"reason":"build-finished","success":true}"#.to_string(),
        ]
        .join("\n");
        let stderr = format!(
            "rustright: request {REQ}\n[rustright] target warm at start (4 entries, generation 3)\n   Compiling fx v0.1.0\n    Finished `test` profile in 3.0s\n"
        );
        let plan = BuildPlan::new(
            "fx",
            &argv("rustright cargo test -p fx --no-run --features qa --message-format=json"),
            dir.clone(),
            vec![
                ArtifactSpec {
                    role: "adapter".into(),
                    cargo_target: Some("journeys_adapter".into()),
                    cargo_package: None,
                    marker: MarkerPolicy::for_role("adapter"),
                },
                ArtifactSpec {
                    role: "app".into(),
                    cargo_target: None,
                    cargo_package: Some("fxapp".into()),
                    marker: MarkerPolicy::for_role("app"),
                },
            ],
        )
        .unwrap();
        Fx {
            dir,
            adapter,
            app,
            stdout,
            stderr,
            plan,
        }
    }

    fn inputs(plan: &BuildPlan, tree: char) -> CurrentInputs {
        CurrentInputs {
            manifest_sha256: rep('b', 64),
            source: source(tree),
            lockfiles: vec![FileHash {
                path: "Cargo.lock".into(),
                sha256: rep('c', 64),
            }],
            features: features_of(&plan.command),
            command: plan.command.clone(),
            toolchain_pin: None,
        }
    }

    /// The broker path the fixture's raw build claims to have spawned.
    const FIXTURE_BROKER: &str = "/fixture/bin/rustright";

    fn raw(fx: &Fx) -> RawBuild {
        RawBuild {
            code: Some(0),
            stdout: fx.stdout.clone(),
            stderr: fx.stderr.clone(),
            timed_out: false,
            broker: Some(FIXTURE_BROKER.to_string()),
        }
    }

    fn record(
        fx: &Fx,
        raw: &RawBuild,
        before: &CurrentInputs,
        after: &CurrentInputs,
    ) -> Result<BuildRecord> {
        record_build(&RecordRequest {
            run_id: "run-1",
            app: "fxapp",
            manifest_path: "journeys.toml",
            plan: &fx.plan,
            before,
            after,
            started_at: "2026-10-23T10:00:00Z".into(),
            ended_at: "2026-10-23T10:00:05Z".into(),
            raw,
            run_dir: &fx.dir.join("run"),
        })
    }

    #[test]
    fn the_record_stores_the_broker_path_that_was_spawned() {
        // The record names the broker binary the runner actually spawned, copied from the raw build.
        let (_fx, rec, _cur) = built("broker-recorded");
        assert_eq!(rec.broker_path.as_deref(), Some(FIXTURE_BROKER));
    }

    fn built(tag: &str) -> (Fx, BuildRecord, CurrentInputs) {
        let fx = fixture(tag, true);
        let cur = inputs(&fx.plan, 'd');
        let rec = record(&fx, &raw(&fx), &cur, &cur).unwrap();
        (fx, rec, cur)
    }

    #[test]
    fn parses_compiler_artifact_records() {
        let fx = fixture("parse", true);
        let parsed = parse_cargo_json(&format!("running 1 test\n{}\nnot json {{\n", fx.stdout));
        assert_eq!(parsed.build_finished, Some(true));
        assert_eq!(parsed.artifacts.len(), 3);
        let lib = &parsed.artifacts[0];
        assert_eq!(
            (lib.package.as_str(), lib.target.as_str()),
            ("fxlib", "fxlib")
        );
        assert!(lib.executable.is_none());
        let adapter = &parsed.artifacts[1];
        assert_eq!(adapter.package, "fx");
        assert!(adapter.test_profile);
        assert_eq!(adapter.kind, vec!["test"]);
        assert_eq!(adapter.features, vec!["qa"]);
        assert_eq!(adapter.executable.as_deref(), Some(fx.adapter.as_path()));
        assert_eq!(parsed.artifacts[2].package, "fxapp");
        // A failed build-finished anywhere wins; none at all is `None`.
        let failed = parse_cargo_json(
            "{\"reason\":\"build-finished\",\"success\":true}\n{\"reason\":\"build-finished\",\"success\":false}\n",
        );
        assert_eq!(failed.build_finished, Some(false));
        assert_eq!(parse_cargo_json("").build_finished, None);
    }

    #[test]
    fn package_names_come_from_every_package_id_form() {
        assert_eq!(
            package_name_of("postright-engine 0.1.0 (path+file:///D:/Claude/postright)").as_deref(),
            Some("postright-engine")
        );
        assert_eq!(
            package_name_of("path+file:///D:/Claude/rightkit/crates/rightkit-qa#0.8.0").as_deref(),
            Some("rightkit-qa")
        );
        assert_eq!(
            package_name_of("path+file:///x/crates/engine#postright-engine@0.1.0").as_deref(),
            Some("postright-engine")
        );
        assert_eq!(
            package_name_of("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0")
                .as_deref(),
            Some("serde")
        );
        assert_eq!(package_name_of("").as_deref(), None);
    }

    #[test]
    fn broker_id_and_attested_exit_are_recorded() {
        let (fx, rec, _cur) = built("attest");
        assert_eq!(rec.broker.request_id, REQ);
        assert_eq!(rec.broker.request_id_source, REQUEST_ID_SOURCE);
        assert_eq!(rec.broker.exit_code, 0);
        assert!(rec.broker.build_finished && rec.broker.attested);
        assert_eq!(rec.command, fx.plan.command);
        // Both binaries were copied into the store and hashed by the runner.
        let run = fx.dir.join("run");
        for art in &rec.artifacts {
            let copy = run.join(&art.store_path);
            assert!(copy.is_file(), "{}", copy.display());
            assert_eq!(rightkit_fs::digest::sha256_file(&copy).unwrap(), art.sha256);
        }
        let app = rec.artifact("app").unwrap();
        assert!(app.qa_marker && app.marker == MarkerPolicy::Required);
        assert_eq!(app.package, "fxapp");
        assert_eq!(app.features, vec!["qa"]);
        assert!(!rec.artifact("adapter").unwrap().qa_marker);
        assert_eq!(rec.features.requested, vec!["qa"]);
        // The record round-trips through its file.
        let path = run.join(BUILD_RECORD_FILE);
        rec.write(&path).unwrap();
        assert_eq!(BuildRecord::read(&path).unwrap(), rec);
        // The receipt `build` section is derived from it.
        let rb = rec.to_receipt_build(&run);
        assert_eq!(rb.source_tree, rb.tree_after_build);
        assert!(rb.broker_receipt.is_none());
        assert_eq!(rb.artifacts.len(), 2);
    }

    #[test]
    fn request_id_rules() {
        // The first matching line wins; compiler text with another shape supplies nothing.
        let ok = format!(
            "noise\nerror: request deadbeef12345678\nrustright managed-agent: request {REQ}\n"
        );
        assert_eq!(parse_request_id(&ok).unwrap(), REQ);
        // The reconnect line repeats the same id: still one request.
        let again =
            format!("rustright: request {REQ}\nrustright: request {REQ} COMPLETED (exit 0)\n");
        assert_eq!(parse_request_id(&again).unwrap(), REQ);
        // A non-zero exit in that line, or a second request, refuses the build.
        let failed =
            format!("rustright: request {REQ}\nrustright: request {REQ} FAILED (exit 101)\n");
        assert!(parse_request_id(&failed)
            .unwrap_err()
            .0
            .starts_with("build_not_attested"));
        let two = format!(
            "rustright: request {REQ}\nrustright: request 11111111-2222-3333-4444-555555555555\n"
        );
        assert!(parse_request_id(&two)
            .unwrap_err()
            .0
            .contains("2 different"));
        // No line, or an id of the wrong shape, is refused.
        assert!(parse_request_id("Compiling x\n").is_err());
        assert!(parse_request_id("rustright: request NOT-AN-ID!\n").is_err());
        assert!(parse_request_id("rustright: request abc\n").is_err());
    }

    #[test]
    fn an_unattested_build_is_refused() {
        let fx = fixture("unattested", true);
        let cur = inputs(&fx.plan, 'd');
        let good = raw(&fx);
        let mut r = good.clone();
        r.code = Some(1);
        assert!(record(&fx, &r, &cur, &cur)
            .unwrap_err()
            .0
            .starts_with("build_not_attested"));
        let mut r = good.clone();
        r.code = None;
        assert!(record(&fx, &r, &cur, &cur).is_err());
        let mut r = good.clone();
        r.timed_out = true;
        assert!(record(&fx, &r, &cur, &cur)
            .unwrap_err()
            .0
            .contains("timed out"));
        let mut r = good.clone();
        r.stdout = r.stdout.replace("\"success\":true", "\"success\":false");
        assert!(record(&fx, &r, &cur, &cur)
            .unwrap_err()
            .0
            .contains("success: false"));
        let mut r = good.clone();
        r.stdout = r.stdout.replace("build-finished", "something-else");
        assert!(record(&fx, &r, &cur, &cur)
            .unwrap_err()
            .0
            .contains("no build-finished"));
        let mut r = good.clone();
        r.stderr = "Compiling x\n".into();
        assert!(record(&fx, &r, &cur, &cur)
            .unwrap_err()
            .0
            .contains("no broker request line"));
    }

    #[test]
    fn a_tree_change_after_the_build_is_source_mismatch() {
        let fx = fixture("treechange", true);
        let before = inputs(&fx.plan, 'd');
        let mut after = before.clone();
        after.source = source('e');
        let e = record(&fx, &raw(&fx), &before, &after).unwrap_err();
        assert!(e.0.starts_with("source_mismatch"), "{}", e.0);
        // A dirty-state change with the same tree is a change too (edit during the build).
        let mut dirty = before.clone();
        dirty.source.dirty = true;
        dirty.source.diff_sha256 = Some(rep('f', 64));
        assert!(record(&fx, &raw(&fx), &before, &dirty)
            .unwrap_err()
            .0
            .starts_with("source_mismatch"));
        // A lockfile that moved during the build is stale, not silently recorded.
        let mut lock = before.clone();
        lock.lockfiles[0].sha256 = rep('9', 64);
        assert!(record(&fx, &raw(&fx), &before, &lock)
            .unwrap_err()
            .0
            .starts_with("build_record_stale"));
    }

    #[test]
    fn at_launch_a_changed_tree_is_source_mismatch_and_nothing_launches() {
        let (fx, rec, cur) = built("treelaunch");
        let run = fx.dir.join("run");
        assert!(rec.verify_launch(&run, "app", &cur).is_ok());
        let mut moved = cur.clone();
        moved.source = source('e');
        let refusal = rec.verify_launch(&run, "app", &moved).unwrap_err();
        let problems = refusal.problems();
        assert!(
            problems.iter().any(|p| p.code == code::SOURCE_MISMATCH),
            "{refusal}"
        );
        assert!(matches!(refusal, RecordRefusal::Stale(_)));
    }

    #[test]
    fn the_record_goes_stale_on_a_feature_change() {
        let (fx, rec, cur) = built("feature");
        assert!(rec.stale_against(&cur).is_empty());
        let mut other = cur.clone();
        other.features = features_of(&argv(
            "rustright cargo test -p fx --no-run --features qa,extra",
        ));
        other.command =
            argv("rustright cargo test -p fx --no-run --features qa,extra --message-format=json");
        let problems = rec.stale_against(&other);
        assert!(problems.iter().all(|p| p.code == code::BUILD_RECORD_STALE));
        assert!(
            problems
                .iter()
                .any(|p| p.detail.contains("feature set changed")),
            "{problems:?}"
        );
        assert!(problems
            .iter()
            .any(|p| p.detail.contains("command changed")));
        // Dropping default features, or asking for all, is a different build too.
        let mut nd = cur.clone();
        nd.features.no_default_features = true;
        assert!(rec
            .stale_against(&nd)
            .iter()
            .any(|p| p.detail.contains("feature set changed")));
        let refusal = rec
            .verify_launch(&fx.dir.join("run"), "app", &other)
            .unwrap_err();
        assert!(matches!(refusal, RecordRefusal::Stale(_)));
    }

    #[test]
    fn the_record_goes_stale_on_a_lockfile_change() {
        let (_fx, rec, cur) = built("lock");
        let mut changed = cur.clone();
        changed.lockfiles[0].sha256 = rep('9', 64);
        let p = rec.stale_against(&changed);
        assert_eq!(p.len(), 1);
        assert!(
            p[0].detail.contains("Cargo.lock changed"),
            "{}",
            p[0].detail
        );
        let mut added = cur.clone();
        added.lockfiles.push(FileHash {
            path: "pnpm-lock.yaml".into(),
            sha256: rep('8', 64),
        });
        assert!(rec.stale_against(&added)[0]
            .detail
            .contains("pnpm-lock.yaml added"));
        let mut removed = cur.clone();
        removed.lockfiles.clear();
        assert!(rec.stale_against(&removed)[0]
            .detail
            .contains("Cargo.lock removed"));
        // The manifest and toolchain pin are bound too.
        let mut manifest = cur.clone();
        manifest.manifest_sha256 = rep('1', 64);
        assert!(rec.stale_against(&manifest)[0]
            .detail
            .contains("journeys.toml changed"));
        let mut pin = cur.clone();
        pin.toolchain_pin = Some(FileHash {
            path: "rust-toolchain.toml".into(),
            sha256: rep('2', 64),
        });
        assert!(rec.stale_against(&pin)[0].detail.contains("toolchain pin"));
    }

    #[test]
    fn a_binary_tampered_with_after_the_build_is_refused_at_launch() {
        let (fx, rec, cur) = built("tamper");
        let run = fx.dir.join("run");
        let ok = rec.verify_launch(&run, "app", &cur).unwrap();
        assert_eq!(ok.sha256, rec.artifact("app").unwrap().sha256);
        // Overwrite the stored copy with a different but still marked binary.
        let stored = run.join(&rec.artifact("app").unwrap().store_path);
        write_exe(&stored, true, 99);
        let e = rec.verify_launch(&run, "app", &cur).unwrap_err();
        match &e {
            RecordRefusal::Binary { role, refusal } => {
                assert_eq!(role, "app");
                assert!(
                    matches!(refusal, LaunchRefusal::HashMismatch { .. }),
                    "{refusal}"
                );
            }
            other => panic!("{other}"),
        }
        assert!(e
            .problems()
            .iter()
            .any(|p| p.code == code::ARTIFACT_MISMATCH));
        // The untouched adapter binary still launches (roles are checked one by one).
        assert!(rec.verify_launch(&run, "adapter", &cur).is_ok());
        // A role the record does not hold is refused, never defaulted.
        assert!(matches!(
            rec.verify_launch(&run, "engine", &cur).unwrap_err(),
            RecordRefusal::UnknownRole(_)
        ));
    }

    #[test]
    fn a_binary_without_the_qa_marker_is_refused() {
        // At build time: an app-class role without the marker never makes a record.
        let fx = fixture("nomarker", false);
        let cur = inputs(&fx.plan, 'd');
        let e = record(&fx, &raw(&fx), &cur, &cur).unwrap_err();
        assert!(e.0.starts_with("qa_marker_missing"), "{}", e.0);
        assert!(!fx
            .dir
            .join("run")
            .join(STORE_DIR)
            .join("app")
            .join("fxapp.exe")
            .exists());
        // At launch: a record whose app policy is `required` and whose stored file lost the marker
        // (same digest recorded as measured) is refused as MarkerMissing.
        let (fx2, mut rec, cur2) = built("nomarker2");
        let run = fx2.dir.join("run");
        let stored = run.join(&rec.artifact("app").unwrap().store_path);
        write_exe(&stored, false, 2);
        let new_sha = rightkit_fs::digest::sha256_file(&stored).unwrap();
        rec.artifacts
            .iter_mut()
            .find(|a| a.role == "app")
            .unwrap()
            .sha256 = new_sha;
        let e = rec.verify_launch(&run, "app", &cur2).unwrap_err();
        match e {
            RecordRefusal::Binary { refusal, .. } => {
                assert!(matches!(refusal, LaunchRefusal::MarkerMissing { .. }));
                assert_eq!(refusal.code(), code::QA_MARKER_MISSING);
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn marker_policy_exempts_only_adapter_and_engine() {
        assert_eq!(
            MarkerPolicy::for_role("adapter"),
            MarkerPolicy::NotApplicable
        );
        assert_eq!(
            MarkerPolicy::for_role("engine"),
            MarkerPolicy::NotApplicable
        );
        for role in ["app", "ui", "native-ui", "anything-else"] {
            assert!(MarkerPolicy::for_role(role).required(), "{role}");
        }
    }

    #[test]
    fn build_commands_are_compile_only_and_through_the_broker() {
        let retired = validate_build_command(&argv(&format!("{RETIRED_BROKER} cargo build -p fx")))
            .unwrap_err();
        assert!(
            retired.0.contains("retired name of the RustRight client"),
            "{}",
            retired.0
        );
        let ok =
            validate_build_command(&argv("rustright cargo build -p fx --features qa")).unwrap();
        assert_eq!(ok.last().unwrap(), "--message-format=json");
        let ok = validate_build_command(&argv(
            "rustright.exe cargo test -p fx --no-run -- --nocapture",
        ))
        .unwrap();
        // The format flag goes before the `--` separator.
        assert_eq!(ok.last().unwrap(), "--nocapture");
        let pos_sep = ok.iter().position(|a| a == "--").unwrap();
        assert_eq!(ok[pos_sep - 1], "--message-format=json");
        let keep = validate_build_command(&argv(
            "rustright cargo build --message-format=json-render-diagnostics",
        ))
        .unwrap();
        assert_eq!(
            keep.iter()
                .filter(|a| a.starts_with("--message-format"))
                .count(),
            1
        );
        for bad in [
            "cargo build -p fx",
            "rustright npm build",
            "rustright cargo test -p fx",
            "rustright cargo run -p fx",
            "rustright cargo check",
            "rustright cargo +nightly build",
            "rustright cargo --offline build",
            "rustright cargo build --target-dir D:/t",
            "rustright cargo build --target-dir=D:/t",
            "rustright cargo build --out-dir D:/o",
            "rustright cargo build --message-format short",
            "rustright cargo build --message-format=human",
            "rustright cargo test --no-run --artifact-dir x",
        ] {
            let e = validate_build_command(&argv(bad));
            assert!(e.is_err(), "{bad} should be refused");
            assert!(e.unwrap_err().0.starts_with("build_not_attested"));
        }
        // `--no-run` after `--` belongs to the test binary, not to cargo.
        assert!(validate_build_command(&argv("rustright cargo test -p fx -- --no-run")).is_err());
        assert!(validate_build_command(&[]).is_err());
    }

    #[test]
    fn features_are_read_from_every_spelling() {
        let f = features_of(&argv(
            "rustright cargo test --features a,b -F c --features=d -Fe --no-default-features",
        ));
        assert_eq!(f.requested, vec!["a", "b", "c", "d", "e"]);
        assert!(f.no_default_features && !f.all_features);
        let f = features_of(&argv(
            "rustright cargo build --all-features -- --features ignored",
        ));
        assert!(f.all_features && f.requested.is_empty());
        // Order and duplicates do not matter.
        assert_eq!(
            features_of(&argv("x cargo build -F b,a -F a")),
            features_of(&argv("x cargo build --features a,b"))
        );
    }

    #[test]
    fn artifact_selection_is_exact_and_refuses_ambiguity() {
        let exe = |n: &str| Some(PathBuf::from(format!("/t/{n}")));
        let rec = |pkg: &str, target: &str, kind: &str, test: bool, e: Option<PathBuf>| {
            CompilerArtifact {
                package: pkg.into(),
                target: target.into(),
                kind: vec![kind.into()],
                test_profile: test,
                features: vec![],
                executable: e,
                fresh: false,
            }
        };
        let all = vec![
            rec("eng", "eng", "lib", false, None),
            rec("eng", "eng", "bin", true, exe("eng-test")),
            rec("eng", "eng", "bin", false, exe("eng")),
            rec("eng", "eng", "bin", false, exe("eng")), // a repeated record of the same file
            rec("two", "a", "bin", false, exe("a")),
            rec("two", "b", "bin", false, exe("b")),
            rec("eng", "it", "test", true, exe("it-1")),
        ];
        let spec = |t: Option<&str>, p: Option<&str>| ArtifactSpec {
            role: "r".into(),
            cargo_target: t.map(str::to_string),
            cargo_package: p.map(str::to_string),
            marker: MarkerPolicy::Required,
        };
        // A package takes its non-test binary; a target with both flavors takes the normal one.
        assert_eq!(
            select_artifact(&spec(None, Some("eng")), &all)
                .unwrap()
                .executable,
            exe("eng")
        );
        assert_eq!(
            select_artifact(&spec(Some("eng"), None), &all)
                .unwrap()
                .executable,
            exe("eng")
        );
        // A test target is taken as it is.
        assert_eq!(
            select_artifact(&spec(Some("it"), None), &all)
                .unwrap()
                .executable,
            exe("it-1")
        );
        // A package with two binaries is ambiguous until a target names one.
        assert!(select_artifact(&spec(None, Some("two")), &all)
            .unwrap_err()
            .0
            .contains("ambiguous"));
        assert_eq!(
            select_artifact(&spec(Some("b"), Some("two")), &all)
                .unwrap()
                .executable,
            exe("b")
        );
        // Nothing matches, or nothing is declared: refused with the not-from-build code.
        assert!(select_artifact(&spec(Some("nope"), None), &all)
            .unwrap_err()
            .0
            .starts_with("artifact_not_from_build"));
        assert!(select_artifact(&spec(None, None), &all)
            .unwrap_err()
            .0
            .contains("only artifacts produced by this build"));
        // A library has no executable even when it matches.
        assert!(select_artifact(&spec(Some("eng"), Some("nobody")), &all).is_err());
    }

    #[test]
    fn lockfiles_are_found_at_and_above_the_build_directory() {
        let top = tmp("locks");
        let sub = top.join("src-tauri");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Cargo.lock"), b"lock").unwrap();
        std::fs::write(top.join("pnpm-lock.yaml"), b"pnpm").unwrap();
        let found = find_lockfiles(&top, std::slice::from_ref(&sub)).unwrap();
        let paths: Vec<&str> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["pnpm-lock.yaml", "src-tauri/Cargo.lock"]);
        assert_eq!(found[1].sha256, crate::util::sha256_hex(b"lock"));
        // A build with no Cargo.lock anywhere is refused.
        let bare = tmp("nolock");
        assert!(find_lockfiles(&bare, std::slice::from_ref(&bare))
            .unwrap_err()
            .0
            .contains("no Cargo.lock"));
        // A start directory outside the repository is refused.
        assert!(find_lockfiles(&sub, std::slice::from_ref(&top))
            .unwrap_err()
            .0
            .contains("outside the repository"));
    }

    #[test]
    fn a_malformed_or_unattested_record_does_not_load() {
        let (fx, rec, _cur) = built("validate");
        let run = fx.dir.join("run");
        let path = run.join(BUILD_RECORD_FILE);
        rec.write(&path).unwrap();
        let mut bad = rec.clone();
        bad.broker.attested = false;
        assert!(bad
            .validate()
            .unwrap_err()
            .0
            .starts_with("build_not_attested"));
        let mut bad = rec.clone();
        bad.broker.request_id = "x".into();
        assert!(bad.validate().is_err());
        let mut bad = rec.clone();
        bad.artifacts[0].store_path = "../escape/app.exe".into();
        assert!(bad.validate().is_err());
        let mut bad = rec.clone();
        bad.artifacts[1].role = bad.artifacts[0].role.clone();
        assert!(bad.validate().is_err());
        let mut bad = rec.clone();
        bad.source.dirty = true; // dirty without a diff digest
        assert!(bad.validate().is_err());
        let mut bad = rec.clone();
        bad.lockfiles.clear();
        assert!(bad.validate().is_err());
        // The file form is refused too.
        std::fs::write(&path, br#"{"schema":"other"}"#).unwrap();
        assert!(BuildRecord::read(&path).is_err());
    }

    #[test]
    fn a_scenario_file_not_committed_at_head_is_refused() {
        let dir = tmp("head-bound");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .expect("git is required for this fixture")
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("kept.scenario.toml"), "name = \"kept\"\n").unwrap();
        git(&["add", "kept.scenario.toml"]);
        git(&[
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa@example.invalid",
            "commit",
            "-q",
            "-m",
            "fixture",
        ]);
        // Ignored-style (untracked) and staged-only files are not committed at HEAD.
        std::fs::write(dir.join("ghost.scenario.toml"), "name = \"ghost\"\n").unwrap();
        std::fs::write(dir.join("staged.scenario.toml"), "name = \"staged\"\n").unwrap();
        git(&["add", "staged.scenario.toml"]);
        assert!(committed_at_head(&dir.join("kept.scenario.toml")).unwrap());
        assert!(!committed_at_head(&dir.join("ghost.scenario.toml")).unwrap());
        assert!(!committed_at_head(&dir.join("staged.scenario.toml")).unwrap());
    }

    #[test]
    fn the_broker_is_resolved_on_path_by_full_path_with_an_alias_and_a_typed_refusal() {
        let dir = tmp("broker-path");
        std::fs::write(dir.join("rustright.cmd"), "@echo off\r\n").unwrap();
        let path_var = std::env::join_paths([dir.as_path()]).unwrap();
        // A Windows shim is found by its extension and returned as a full path.
        let hit = resolve_broker("rustright", &path_var, true).unwrap();
        assert_eq!(hit, dir.join("rustright.cmd"));
        // The retired name is refused by name, never resolved to rustright.cmd in its place.
        let retired = resolve_broker(RETIRED_BROKER, &path_var, true).unwrap_err();
        assert!(
            retired.0.contains("retired") && retired.0.contains("rustright"),
            "{}",
            retired.0
        );
        // `rustright` on the command also resolves, and nothing is copied into the directory.
        assert_eq!(
            resolve_broker("rustright", &path_var, true).unwrap(),
            dir.join("rustright.cmd")
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        // Neither name on PATH is a typed refusal that names both.
        let empty = tmp("broker-empty");
        let none = std::env::join_paths([empty.as_path()]).unwrap();
        let e = resolve_broker("rustright", &none, true).unwrap_err();
        assert!(e.0.starts_with("broker_not_found"), "{}", e.0);
        assert!(e.0.contains("rustright"), "{}", e.0);
        // Off Windows the shim extension does not count.
        assert!(resolve_broker("rustright", &path_var, false).is_err());
    }

    #[test]
    fn an_empty_path_entry_never_resolves_the_broker_in_the_working_directory() {
        // A PATH of only empty entries means the working directory to the OS, and is never searched.
        let sep = if cfg!(windows) { ";" } else { ":" };
        let empty_entries = std::ffi::OsString::from(sep);
        assert!(resolve_broker("rustright", &empty_entries, cfg!(windows)).is_err());
        // An empty entry before a real directory still finds the broker in that directory.
        let dir = tmp("broker-after-empty");
        let name = if cfg!(windows) {
            "rustright.cmd"
        } else {
            "rustright"
        };
        std::fs::write(dir.join(name), "").unwrap();
        let mixed = std::ffi::OsString::from(format!("{sep}{}", dir.display()));
        assert_eq!(
            resolve_broker("rustright", &mixed, cfg!(windows)).unwrap(),
            dir.join(name)
        );
    }

    #[test]
    fn an_absolute_broker_program_is_spawned_as_named_and_a_missing_one_is_refused() {
        // The absolute path is the broker itself: no PATH copy replaces it.
        let dir = tmp("broker-absolute");
        let name = if cfg!(windows) {
            "pinned.exe"
        } else {
            "pinned"
        };
        let pinned = dir.join(name);
        std::fs::write(&pinned, "").unwrap();
        let other = tmp("broker-absolute-other");
        std::fs::write(other.join("rustright.cmd"), "").unwrap();
        let path_var = std::env::join_paths([other.as_path()]).unwrap();
        let resolved = resolve_broker(&pinned.to_string_lossy(), &path_var, cfg!(windows)).unwrap();
        assert_eq!(resolved, pinned);
        let missing = dir.join("absent-broker");
        let e = resolve_broker(&missing.to_string_lossy(), &path_var, cfg!(windows)).unwrap_err();
        assert!(e.0.starts_with("broker_not_found"), "{}", e.0);
    }

    /// Helper process for [`a_real_child_process_passes_stderr_and_json_through`].
    #[test]
    fn fake_broker_child() {
        let Ok(stdout) = std::env::var("RKQA_FAKE_BROKER_STDOUT") else {
            return;
        };
        println!("{stdout}");
        eprintln!(
            "rustright: request {}",
            std::env::var("RKQA_FAKE_BROKER_ID").unwrap()
        );
        eprintln!("[rustright] target warm at start (1 entries, generation 1)");
    }

    #[test]
    fn a_real_child_process_passes_stderr_and_json_through() {
        let fx = fixture("child", true);
        let exe = std::env::current_exe().unwrap();
        let raw = spawn_command(
            exe.to_str().unwrap(),
            &argv("--exact journeys::build_record::tests::fake_broker_child --nocapture"),
            &fx.dir,
            &[
                ("RKQA_FAKE_BROKER_STDOUT".into(), fx.stdout.clone()),
                ("RKQA_FAKE_BROKER_ID".into(), REQ.into()),
            ],
            &Tracker::new(),
            Duration::from_secs(120),
        )
        .unwrap();
        assert_eq!(raw.code, Some(0), "{}", raw.stderr);
        assert_eq!(parse_request_id(&raw.stderr).unwrap(), REQ);
        let parsed = parse_cargo_json(&raw.stdout);
        assert_eq!(parsed.build_finished, Some(true));
        assert!(parsed
            .artifacts
            .iter()
            .any(|a| a.target == "fxapp" && a.executable.is_some()));
        let cur = inputs(&fx.plan, 'd');
        let rec = record(&fx, &raw, &cur, &cur).unwrap();
        assert_eq!(rec.broker.request_id, REQ);
    }
}
