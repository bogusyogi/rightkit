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
pub(super) const BROKER_PROGRAM: &str = "rustright";
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

pub(super) fn valid_request_id(id: &str) -> bool {
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
