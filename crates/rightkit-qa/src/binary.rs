//! Find the dev build of the app under test, wherever the build put it.
//!
//! `right-release dev` and `rustright cargo build` run through the RightKit build broker, which builds
//! into its own target root (`<managed root>/rightkit-build-control/ws/<key>/g/<gen>/<leaf>/<triple>/<profile>/`)
//! and does not forward app env such as `MAILRIGHT_APP_BINARY` into a later `rustright cargo test`. A
//! journey run there could not find the executable, and every app hand-rolled discovery.
//!
//! [`resolve`] tries, in order, and reports which source won ([`BinarySource`]):
//!
//! 1. **Env override**: `RIGHTKIT_QA_APP_BINARY`, then `RIGHTKIT_QA_UI_BINARY`, then the app's own
//!    variable named by `ui.binary_env`. A set variable that fails validation is an error, never a fall-through.
//! 2. **Pointer file**: a gitignored file under the app root (`.rightkit-qa-app-binary`, or `ui.pointer_file`)
//!    whose first non-empty line is the executable path (relative paths resolve against the app root).
//!    Same strictness as the env override.
//! 3. **Broker target root**: the newest dev-profile executable named `<bin>` under the broker workspace
//!    whose `identity.json` `canonicalRoot` is the app's cargo workspace root. See "Broker format" below.
//! 4. **App-root relative paths**: the existing `ui.binaries` entry.
//!
//! Every candidate must also be a QA build: it carries `QA_CONTROL_MARKER` (Latin-1 or UTF-16LE), the same
//! bytes `right-release` scans for. A broker or app-root candidate without it is skipped with a reason; an env or
//! pointer value without it is an error. With a single dev profile a plain dev build and the control build both
//! write `target/<triple>/debug/<bin>`, so the marker is what tells them apart. `--build-ui` keeps a stable copy
//! ([`stash_qa_build`]) and points `.rightkit-qa-app-binary` at it. Both that pointer file and the
//! `<target>/rightkit-qa/` copy are build output: git-ignore them.
//!
//! # One fixed launch path
//!
//! macOS ties privacy grants to the path an app runs from, and the broker's target folders change per
//! generation (`ws/<hash>/g/<gen>/...`). [`resolve_stashed`] (what `QaConfig::resolve_ui_binary` and the suite
//! use) therefore ALWAYS returns the fixed stash copy under the app root, copied in place with no
//! retained copy of the replaced build (D01), whichever source found the build. A QA launch never
//! runs from a broker generation path.
//!
//! Every candidate must exist, be a regular file, be named `<bin>` (or `<bin>.exe`) and not be an installed app.
//! A dev build is the `release` Cargo profile plus `qa-native` (GATE-CONTRACT GC-A07), so a `release` profile
//! directory is accepted. What keeps a shipping artifact out is the control marker every candidate must carry:
//! `right-release` refuses to seal or upload a build that has it, so a release-tier build has none.
//!
//! # Broker format
//!
//! The broker's per-job receipts (`<broker root>/receipts/<id>.json`) carry only freshness counts
//! (`compilerArtifacts`), no executable path, so they cannot name the last built artifact. What is stable
//! is the workspace state: `ws/<key>/identity.json` (`{"schemaVersion":1,"key","canonicalRoot"}`) and the
//! target tree `ws/<key>/g/<generation>/<leaf>/...` where `<leaf>` is `t` or `target`. Under the leaf
//! sits `[<triple>/]<profile>/<bin>[.exe]`. Only the profiles `release` (the dev build), `release-iterate`,
//! `debug` and `dev` are read; the newest by modification time wins.
use crate::util::{err, Error, Result};
use std::path::{Path, PathBuf};

/// Env var: the dev executable, for one app.
pub const APP_BINARY_ENV: &str = "RIGHTKIT_QA_APP_BINARY";
/// Older env var read by the suite runner; honored second.
pub const UI_BINARY_ENV: &str = "RIGHTKIT_QA_UI_BINARY";
/// The embedded control server's marker; keep identical to `embedded::QA_CONTROL_MARKER` and
/// `packages/release/qa-control-scan.mjs`. Not feature-gated here: this module only scans files.
pub const QA_CONTROL_MARKER: &str = "RIGHTKIT-QA-CONTROL-SERVER-PRESENT";
/// Directory (under the cargo target dir) that holds the stable QA copy.
pub const STASH_DIR: &str = ".rightkit-qa-bin";
/// Default pointer file name, relative to the app root.
pub const POINTER_FILE: &str = ".rightkit-qa-app-binary";
/// Profile directories the broker lookup reads. The dev build is the `release` profile plus `qa-native`
/// (GATE-CONTRACT GC-A07); the older dev profiles stay readable. The control marker, not the directory,
/// tells a QA build from a plain one.
pub const DEV_PROFILES: [&str; 4] = ["release", "release-iterate", "debug", "dev"];

/// Which source produced the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinarySource {
    /// The named env var.
    Env(String),
    /// The pointer file at this path.
    PointerFile(PathBuf),
    /// The broker target root workspace directory (`ws/<key>`).
    BrokerTarget(PathBuf),
    /// The configured app-root relative path.
    AppRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinary {
    pub path: PathBuf,
    pub source: BinarySource,
}

/// Inputs to [`resolve`]. `env` is injectable so tests never read the process environment.
pub struct BinaryQuery<'a> {
    /// Cargo target name (executable stem), e.g. `mailright`.
    pub bin: &'a str,
    pub app_root: &'a Path,
    /// Cargo workspace roots to match against the broker's `canonicalRoot`, first match wins per entry.
    pub workspace_roots: Vec<PathBuf>,
    /// `<managed root>/rightkit-build-control`; `None` skips the broker step.
    pub broker_root: Option<PathBuf>,
    /// The app-specific env var name (`ui.binary_env`).
    pub extra_env: Option<&'a str>,
    /// Pointer file path, relative to the app root unless absolute.
    pub pointer_file: Option<&'a str>,
    /// The configured `ui.binaries` path for this platform, relative to the app root.
    pub app_relative: Option<&'a str>,
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

/// The broker root under a managed root.
pub fn broker_root_of(managed_root: &Path) -> PathBuf {
    managed_root.join("rightkit-build-control")
}

pub fn resolve(q: &BinaryQuery) -> Result<ResolvedBinary> {
    if q.bin.trim().is_empty() {
        return err("binary name is empty");
    }
    // 1. Env override.
    let mut names = vec![APP_BINARY_ENV.to_string(), UI_BINARY_ENV.to_string()];
    if let Some(n) = q.extra_env.filter(|n| !n.trim().is_empty()) {
        names.push(n.to_string());
    }
    for name in names {
        if let Some(v) = (q.env)(&name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            let path = absolute(q.app_root, &v);
            validate(&path, q.bin).map_err(|e| Error(format!("{name}: {}", e.0)))?;
            require_qa(&path).map_err(|e| Error(format!("{name}: {}", e.0)))?;
            return Ok(ResolvedBinary {
                path,
                source: BinarySource::Env(name),
            });
        }
    }
    // 2. Pointer file.
    let pointer = absolute(q.app_root, q.pointer_file.unwrap_or(POINTER_FILE));
    if let Ok(text) = std::fs::read_to_string(&pointer) {
        if let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
            let path = absolute(q.app_root, line);
            validate(&path, q.bin)
                .map_err(|e| Error(format!("pointer file {}: {}", pointer.display(), e.0)))?;
            require_qa(&path)
                .map_err(|e| Error(format!("pointer file {}: {}", pointer.display(), e.0)))?;
            return Ok(ResolvedBinary {
                path,
                source: BinarySource::PointerFile(pointer),
            });
        }
    }
    // 3. Broker target root.
    let mut notes = Vec::new();
    let mut not_qa: Option<PathBuf> = None;
    if let Some(broker) = &q.broker_root {
        let (found, skipped) = from_broker(broker, &q.workspace_roots, q.bin);
        if let Some((path, ws)) = found {
            return Ok(ResolvedBinary {
                path,
                source: BinarySource::BrokerTarget(ws),
            });
        }
        if let Some(first) = skipped.first() {
            not_qa = Some(first.clone());
        }
        notes.push(format!(
            "no QA dev build of {} under {} (skipped without the control marker: {})",
            q.bin,
            broker.display(),
            skipped.len()
        ));
    }
    // 4. App-root relative.
    if let Some(rel) = q.app_relative {
        let path = absolute(q.app_root, rel);
        match validate(&path, q.bin) {
            Ok(()) => match has_qa_marker(&path) {
                Ok(true) => {
                    return Ok(ResolvedBinary {
                        path,
                        source: BinarySource::AppRoot,
                    })
                }
                _ => {
                    notes.push(format!("{} lacks the control marker", path.display()));
                    not_qa.get_or_insert(path);
                }
            },
            Err(e) => notes.push(e.0),
        }
    }
    if let Some(found) = not_qa {
        return err(format!("{}; {}", not_qa_message(&found), notes.join("; ")));
    }
    err(format!(
        "no dev binary for {}: set {APP_BINARY_ENV}, write a path into {}, or build it ({})",
        q.bin,
        pointer.display(),
        notes.join("; ")
    ))
}

fn not_qa_message(path: &Path) -> String {
    format!(
        "found {} but it isn't a test-mode/control build; run the suite with --build-ui or build with the app's QA feature",
        path.display()
    )
}

fn require_qa(path: &Path) -> Result<()> {
    match has_qa_marker(path) {
        Ok(true) => Ok(()),
        Ok(false) => err(not_qa_message(path)),
        Err(e) => err(format!("cannot read {}: {e}", path.display())),
    }
}

/// True when the file contains [`QA_CONTROL_MARKER`] as Latin-1/UTF-8 bytes or as UTF-16LE (Windows
/// wide string). Streamed in chunks, so a large executable is never held in memory.
pub fn has_qa_marker(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let narrow = QA_CONTROL_MARKER.as_bytes().to_vec();
    let wide: Vec<u8> = QA_CONTROL_MARKER.bytes().flat_map(|b| [b, 0]).collect();
    let keep = wide.len() - 1;
    let find = |hay: &[u8], pat: &[u8]| hay.windows(pat.len()).any(|w| w == pat);
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(false);
        }
        carry.extend_from_slice(&buf[..n]);
        if find(&carry, &narrow) || find(&carry, &wide) {
            return Ok(true);
        }
        if carry.len() > keep {
            carry.drain(..carry.len() - keep);
        }
    }
}

/// Exists, a regular file, named `<bin>` or `<bin>.exe`, and not an installed app.
pub fn validate(path: &Path, bin: &str) -> Result<()> {
    crate::installed::refuse_installed(path, "app binary")?;
    let meta =
        std::fs::metadata(path).map_err(|_| Error(format!("{} does not exist", path.display())))?;
    if !meta.is_file() {
        return err(format!("{} is not a file", path.display()));
    }
    if !name_matches(path, bin) {
        return err(format!("{} is not named {bin}", path.display()));
    }
    Ok(())
}

fn name_matches(path: &Path, bin: &str) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let exe = format!("{bin}.exe");
    name == bin || name == exe || (cfg!(windows) && name.eq_ignore_ascii_case(&exe))
}

fn absolute(root: &Path, text: &str) -> PathBuf {
    let p = Path::new(text);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

fn norm(text: &str) -> String {
    let t = text.strip_prefix(r"\\?\").unwrap_or(text);
    let t = t.replace('\\', "/");
    let t = t.trim_end_matches('/').to_string();
    if cfg!(windows) {
        t.to_lowercase()
    } else {
        t
    }
}

fn norm_path(p: &Path) -> String {
    let real = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    norm(&real.to_string_lossy())
}

/// Newest QA dev-profile `<bin>` in any broker workspace whose canonical root is one of `roots`.
/// Returns the executable and its `ws/<key>` directory, plus the valid candidates skipped for
/// lacking the control marker (newest first).
fn from_broker(
    broker: &Path,
    roots: &[PathBuf],
    bin: &str,
) -> (Option<(PathBuf, PathBuf)>, Vec<PathBuf>) {
    let wanted: Vec<String> = roots.iter().map(|r| norm_path(r)).collect();
    let mut all: Vec<(std::time::SystemTime, PathBuf, PathBuf)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(broker.join("ws")) else {
        return (None, vec![]);
    };
    for entry in rd.flatten() {
        let ws = entry.path();
        let Ok(text) = std::fs::read_to_string(ws.join("identity.json")) else {
            continue;
        };
        let Ok(id) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(root) = id.get("canonicalRoot").and_then(|v| v.as_str()) else {
            continue;
        };
        if !wanted.contains(&norm_path(Path::new(root))) {
            continue;
        }
        for file in broker_candidates(&ws, bin) {
            if validate(&file, bin).is_err() {
                continue;
            }
            let Ok(t) = std::fs::metadata(&file).and_then(|m| m.modified()) else {
                continue;
            };
            all.push((t, file, ws.clone()));
        }
    }
    all.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut skipped = Vec::new();
    for (_, file, ws) in all {
        if has_qa_marker(&file).unwrap_or(false) {
            return (Some((file, ws)), skipped);
        }
        skipped.push(file);
    }
    (None, skipped)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn is_dev_profile(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| DEV_PROFILES.contains(&n))
}

/// `g/<gen>/<leaf>/[<triple>/]<dev profile>/<bin>[.exe]` under one workspace.
fn broker_candidates(ws: &Path, bin: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for generation in subdirs(&ws.join("g")) {
        for leaf in subdirs(&generation) {
            if !matches!(
                leaf.file_name().and_then(|n| n.to_str()),
                Some("t" | "target")
            ) {
                continue;
            }
            for child in subdirs(&leaf) {
                let profiles = if is_dev_profile(&child) {
                    vec![child]
                } else {
                    subdirs(&child)
                };
                for profile in profiles.into_iter().filter(|p| is_dev_profile(p)) {
                    for name in [bin.to_string(), format!("{bin}.exe")] {
                        let f = profile.join(name);
                        if f.is_file() {
                            out.push(f);
                        }
                    }
                }
            }
        }
    }
    out
}

/// True when `path` already sits under the fixed stash `<app_root>/<STASH_DIR>`.
pub fn is_stashed(path: &Path, app_root: &Path) -> bool {
    let stash = app_root.join(STASH_DIR);
    path.starts_with(&stash)
        || match (path.canonicalize(), stash.canonicalize()) {
            (Ok(p), Ok(s)) => p.starts_with(s),
            _ => false,
        }
}

/// [`resolve`], then ALWAYS the fixed stash: whichever source wins (env, pointer, broker target or an
/// app-root path), the returned path is the stash copy `<app_root>/.rightkit-qa-bin/<triple>/<bin>`, copied
/// in place with no retained copy of the replaced build ([`stash_qa_build`], D01). A QA launch
/// therefore never runs from a broker generation path (`ws/<hash>/g/<gen>/...`, a new folder on every
/// generation) or from `target/debug`, so macOS privacy grants keep one path. `source` still names
/// where the build was found. The pointer file is not rewritten (a fresh build is picked up on the
/// next run).
pub fn resolve_stashed(q: &BinaryQuery) -> Result<ResolvedBinary> {
    let found = resolve(q)?;
    if is_stashed(&found.path, q.app_root) {
        return Ok(found);
    }
    let path = stash_binary(&found.path, q.app_root, None, true)?;
    Ok(ResolvedBinary {
        path,
        source: found.source,
    })
}

/// After a successful `--build-ui`: copy the built QA executable (and a sibling `.pdb`) to
/// `<app_root>/<STASH_DIR>/<triple-or-host>/<name>` in place (staging copy, then rename, retaining
/// no copy of the replaced build), and point `<app_root>/.rightkit-qa-app-binary` (or
/// `pointer_file`) at the copy.
/// A later plain dev build that overwrites `debug/<bin>` then cannot displace it. The built file must
/// carry the control marker.
pub fn stash_qa_build(exe: &Path, app_root: &Path, pointer_file: Option<&str>) -> Result<PathBuf> {
    let dest = stash_binary(exe, app_root, Some(pointer_file), true)?;
    Ok(dest)
}

/// The stash copy itself. `pointer` is `Some(<pointer file override>)` to write the pointer file, `None` to leave
/// it alone; `require_marker` keeps the QA control-marker check (an explicitly named UI binary skips it, as
/// before, and passes `false`). The caller has already refused installed apps.
pub fn stash_binary(
    exe: &Path,
    app_root: &Path,
    pointer: Option<Option<&str>>,
    require_marker: bool,
) -> Result<PathBuf> {
    if require_marker {
        require_qa(exe)?;
    }
    let name = exe
        .file_name()
        .ok_or_else(|| Error(format!("{} has no file name", exe.display())))?;
    let profile = exe
        .parent()
        .ok_or_else(|| Error("artifact has no profile dir".into()))?;
    // `<target>/[<triple>/]<profile>/<name>`: a parent dir name with `-` is the target triple.
    let up_name = profile
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let triple = if up_name.contains('-') {
        up_name
    } else {
        "host"
    };
    // Under the app root, not the cargo target dir: the broker's target tree is garbage-collected and
    // its generation folder changes, so nothing may run from it.
    let dir = app_root.join(STASH_DIR).join(triple);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error(format!("cannot create {}: {e}", dir.display())))?;
    let dest = dir.join(name);
    crate::inplace::replace_file_in_place(exe, &dest)?;
    let stem = exe.file_stem().and_then(|n| n.to_str()).unwrap_or("");
    for pdb in [stem.to_string(), stem.replace('-', "_")] {
        let from = profile.join(format!("{pdb}.pdb"));
        if from.is_file() {
            crate::inplace::replace_file_in_place(&from, &dir.join(format!("{pdb}.pdb")))?;
        }
    }
    if let Some(pointer_file) = pointer {
        let pointer = absolute(app_root, pointer_file.unwrap_or(POINTER_FILE));
        let tmp = pointer.with_extension(format!("tmp-{}", crate::util::new_id()));
        std::fs::write(&tmp, format!("{}\n", dest.display()))
            .and_then(|_| std::fs::rename(&tmp, &pointer))
            .map_err(|e| Error(format!("cannot write {}: {e}", pointer.display())))?;
    }
    Ok(dest)
}

// ---------------------------------------------------------------------------------- launch checks
//
// ADR-0001 item 1: the runner lane never trusts a path. A binary recorded in a build record
// (`journeys::build_record`) is re-hashed immediately before it is launched and refused when the
// bytes differ from the recorded digest, or when it is an app-class binary without the QA control
// marker. Hash and marker come from ONE read of the file, so the two answers describe the same
// bytes. The check cannot close the window between the read and the exec; the store the copies sit
// in is a run-private directory (see `build_record::STORE_DIR`) so nothing else is expected to
// write there.

/// Incremental form of [`has_qa_marker`]: feed the chunks of one file in order, then ask
/// [`MarkerScanner::found`]. Recognizes the same Latin-1/UTF-8 and UTF-16LE spellings, including
/// a marker that straddles a chunk boundary.
#[derive(Debug, Clone)]
pub struct MarkerScanner {
    narrow: Vec<u8>,
    wide: Vec<u8>,
    carry: Vec<u8>,
    found: bool,
}

impl Default for MarkerScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkerScanner {
    pub fn new() -> Self {
        MarkerScanner {
            narrow: QA_CONTROL_MARKER.as_bytes().to_vec(),
            wide: QA_CONTROL_MARKER.bytes().flat_map(|b| [b, 0]).collect(),
            carry: Vec::new(),
            found: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if self.found {
            return;
        }
        self.carry.extend_from_slice(chunk);
        let find = |hay: &[u8], pat: &[u8]| hay.windows(pat.len()).any(|w| w == pat);
        if find(&self.carry, &self.narrow) || find(&self.carry, &self.wide) {
            self.found = true;
            self.carry = Vec::new();
            return;
        }
        let keep = self.wide.len() - 1;
        if self.carry.len() > keep {
            let cut = self.carry.len() - keep;
            self.carry.drain(..cut);
        }
    }

    pub fn found(&self) -> bool {
        self.found
    }
}

/// What one pass over a file measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileScan {
    pub sha256: String,
    pub size: u64,
    pub qa_marker: bool,
}

/// SHA-256, size and QA-marker presence of `path`, from a single streamed read.
pub fn scan_file(path: &Path) -> std::io::Result<FileScan> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let mut scanner = MarkerScanner::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        scanner.feed(&buf[..n]);
        size += n as u64;
    }
    Ok(FileScan {
        sha256: hasher.finalize(),
        size,
        qa_marker: scanner.found(),
    })
}

/// A binary that passed [`check_launch`]: the digest and size measured just now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCheck {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub qa_marker: bool,
}

/// Why a binary was refused at launch. [`LaunchRefusal::code`] is the journeys problem code the
/// refusal maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRefusal {
    /// Missing, a directory, or a link: only a regular file recorded by the build is launched.
    NotARegularFile { path: PathBuf },
    /// The file exists but could not be read.
    Unreadable { path: PathBuf, error: String },
    /// An installed app (Program Files, /Applications, ...): never a QA launch target.
    Installed { path: PathBuf, detail: String },
    /// The bytes differ from the digest the build record holds.
    HashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// An app-class binary without the control marker: not a QA build.
    MarkerMissing { path: PathBuf },
}

impl LaunchRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            LaunchRefusal::NotARegularFile { .. }
            | LaunchRefusal::Unreadable { .. }
            | LaunchRefusal::HashMismatch { .. } => crate::journeys::code::ARTIFACT_MISMATCH,
            LaunchRefusal::Installed { .. } => crate::journeys::code::ARTIFACT_NOT_FROM_BUILD,
            LaunchRefusal::MarkerMissing { .. } => crate::journeys::code::QA_MARKER_MISSING,
        }
    }
}

impl std::fmt::Display for LaunchRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchRefusal::NotARegularFile { path } => write!(
                f,
                "{} is missing or is not a regular file; refusing to launch it",
                path.display()
            ),
            LaunchRefusal::Unreadable { path, error } => {
                write!(f, "cannot read {} to re-hash it: {error}", path.display())
            }
            LaunchRefusal::Installed { path, detail } => {
                write!(f, "{} is an installed app: {detail}", path.display())
            }
            LaunchRefusal::HashMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "{} changed since the build: sha256 {actual} is not the recorded {expected}",
                path.display()
            ),
            LaunchRefusal::MarkerMissing { path } => write!(
                f,
                "{} does not carry the QA control marker; it is not a test-mode/control build",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LaunchRefusal {}

impl From<LaunchRefusal> for Error {
    fn from(r: LaunchRefusal) -> Self {
        Error(format!("{}: {r}", r.code()))
    }
}

/// Re-hash `path` and refuse it unless it is the file the build recorded: a regular file (not a
/// link), not an installed app, with SHA-256 `expected_sha256`, and, when `require_marker`,
/// carrying [`QA_CONTROL_MARKER`]. The digest is checked before the marker, so a swapped file is
/// reported as a mismatch whatever it contains.
pub fn check_launch(
    path: &Path,
    expected_sha256: &str,
    require_marker: bool,
) -> std::result::Result<LaunchCheck, LaunchRefusal> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| LaunchRefusal::NotARegularFile {
        path: path.to_path_buf(),
    })?;
    if !meta.is_file() {
        return Err(LaunchRefusal::NotARegularFile {
            path: path.to_path_buf(),
        });
    }
    crate::installed::refuse_installed(path, "launch binary").map_err(|e| {
        LaunchRefusal::Installed {
            path: path.to_path_buf(),
            detail: e.0,
        }
    })?;
    let scan = scan_file(path).map_err(|e| LaunchRefusal::Unreadable {
        path: path.to_path_buf(),
        error: e.to_string(),
    })?;
    if scan.sha256 != expected_sha256 {
        return Err(LaunchRefusal::HashMismatch {
            path: path.to_path_buf(),
            expected: expected_sha256.to_string(),
            actual: scan.sha256,
        });
    }
    if require_marker && !scan.qa_marker {
        return Err(LaunchRefusal::MarkerMissing {
            path: path.to_path_buf(),
        });
    }
    Ok(LaunchCheck {
        path: path.to_path_buf(),
        sha256: scan.sha256,
        size: scan.size,
        qa_marker: scan.qa_marker,
    })
}
