//! Find the dev build of the app under test, wherever the build put it.
//!
//! `right-release dev` and `rightkit cargo build` run through the RightKit build broker, which builds
//! into its own target root (`<managed root>/rightkit-build-control/ws/<key>/g/<gen>/<leaf>/<triple>/<profile>/`)
//! and does not forward app env such as `MAILRIGHT_APP_BINARY` into a later `rightkit cargo test`. A
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
//! Every candidate must also be a QA build: it carries [`QA_CONTROL_MARKER`] (Latin-1 or UTF-16LE), the same
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
//! use) therefore ALWAYS returns the fixed stash copy under the app root, copied in place with a kept
//! `<bin>.previous`, whichever source found the build. A QA launch never runs from a broker generation path
//! or from `.previous`.
//!
//! Every candidate must exist, be a regular file, be named `<bin>` (or `<bin>.exe`) and must not sit in a
//! `release` profile directory. A shipping or release-tier artifact is never picked.
//!
//! # Broker format
//!
//! The broker's per-job receipts (`<broker root>/receipts/<id>.json`) carry only freshness counts
//! (`compilerArtifacts`), no executable path, so they cannot name the last built artifact. What is stable
//! is the workspace state: `ws/<key>/identity.json` (`{"schemaVersion":1,"key","canonicalRoot"}`) and the
//! target tree `ws/<key>/g/<generation>/<leaf>/...` where `<leaf>` is `t` or `target`. Under the leaf
//! sits `[<triple>/]<profile>/<bin>[.exe]`. Only the dev profiles `release-iterate`, `debug` and `dev` are
//! read; the newest by modification time wins.
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
/// Profile directories the broker lookup reads. `release` is never in this list.
pub const DEV_PROFILES: [&str; 3] = ["release-iterate", "debug", "dev"];

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

/// Exists, a regular file, named `<bin>` or `<bin>.exe`, and not in a `release` profile directory.
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
    if path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "release")
    {
        return err(format!("{} is a release-tier artifact", path.display()));
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
/// in place with a kept `<bin>.previous` ([`stash_qa_build`]). A QA launch therefore never runs from a
/// broker generation path (`ws/<hash>/g/<gen>/...`, a new folder on every generation) or from
/// `target/debug`, so macOS privacy grants keep one path. `source` still names where the build was
/// found. The pointer file is not rewritten (a fresh build is picked up on the next run); the stash
/// copy of the same bytes does not rotate `.previous`.
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
/// `<app_root>/<STASH_DIR>/<triple-or-host>/<name>` in place (staging copy, then rename, keeping
/// `<name>.previous`), and point `<app_root>/.rightkit-qa-app-binary` (or `pointer_file`) at the copy.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-binary-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fixture executable that carries the control marker.
    fn touch(p: &Path, age_s: u64) {
        write_exe(p, age_s, true);
    }

    fn write_exe(p: &Path, age_s: u64, marker: bool) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut bytes = b"MZ....padding".to_vec();
        if marker {
            bytes.extend_from_slice(QA_CONTROL_MARKER.as_bytes());
        }
        std::fs::write(p, bytes).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_s);
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn exe(name: &str) -> String {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        }
    }

    /// A fixture broker with one workspace for `root`.
    fn broker(base: &Path, root: &Path) -> PathBuf {
        let ws = base.join("rightkit-build-control/ws/abc123");
        std::fs::create_dir_all(&ws).unwrap();
        let id = serde_json::json!({"schemaVersion":1,"key":"abc123","canonicalRoot":root.to_string_lossy()});
        std::fs::write(ws.join("identity.json"), id.to_string()).unwrap();
        ws
    }

    fn query<'a>(
        bin: &'a str,
        app: &'a Path,
        broker: Option<PathBuf>,
        env: &'a dyn Fn(&str) -> Option<String>,
    ) -> BinaryQuery<'a> {
        BinaryQuery {
            bin,
            app_root: app,
            workspace_roots: vec![app.to_path_buf()],
            broker_root: broker,
            extra_env: Some("MYAPP_APP_BINARY"),
            pointer_file: None,
            app_relative: Some("target/debug/myapp"),
            env,
        }
    }

    #[test]
    fn broker_picks_newest_dev_profile_and_never_release() {
        let base = tmp("broker");
        let app = base.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let ws = broker(&base, &app);
        let old = ws
            .join("g/001/t/x86_64-pc-windows-msvc/release-iterate")
            .join(exe("myapp"));
        let new = ws
            .join("g/002/t/x86_64-pc-windows-msvc/debug")
            .join(exe("myapp"));
        let rel = ws
            .join("g/002/t/x86_64-pc-windows-msvc/release")
            .join(exe("myapp"));
        touch(&old, 500);
        touch(&new, 100);
        touch(&rel, 1); // newest, but release tier
        let env = |_: &str| None;
        let got = resolve(&query(
            "myapp",
            &app,
            Some(base.join("rightkit-build-control")),
            &env,
        ))
        .unwrap();
        assert_eq!(got.path, new);
        assert_eq!(got.source, BinarySource::BrokerTarget(ws));
    }

    #[test]
    fn broker_ignores_other_workspaces_and_other_bins() {
        let base = tmp("other");
        let app = base.join("app");
        let other = base.join("other");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let ws = broker(&base, &other);
        touch(&ws.join("g/001/target/debug").join(exe("myapp")), 10);
        let ws2 = base.join("rightkit-build-control/ws/def456");
        std::fs::create_dir_all(&ws2).unwrap();
        let id = serde_json::json!({"canonicalRoot": app.to_string_lossy()});
        std::fs::write(ws2.join("identity.json"), id.to_string()).unwrap();
        touch(&ws2.join("g/001/target/debug").join(exe("helper")), 10);
        let env = |_: &str| None;
        let e = resolve(&query(
            "myapp",
            &app,
            Some(base.join("rightkit-build-control")),
            &env,
        ))
        .unwrap_err();
        assert!(e.0.contains("no dev binary for myapp"), "{}", e.0);
    }

    #[test]
    fn env_beats_pointer_beats_broker_beats_app_root() {
        let base = tmp("order");
        let app = base.join("app");
        let ws = broker(&base, &app);
        let from_broker = ws.join("g/001/t/debug").join(exe("myapp"));
        touch(&from_broker, 10);
        let rel = app.join("target/debug/myapp");
        touch(&rel, 10);
        touch(&app.join("target/debug/myapp.exe"), 10);
        let b = Some(base.join("rightkit-build-control"));

        let none = |_: &str| None;
        let got = resolve(&query("myapp", &app, b.clone(), &none)).unwrap();
        assert!(matches!(got.source, BinarySource::BrokerTarget(_)));

        let pointed = base.join("pointed").join(exe("myapp"));
        touch(&pointed, 10);
        std::fs::write(app.join(POINTER_FILE), format!("\n{}\n", pointed.display())).unwrap();
        let got = resolve(&query("myapp", &app, b.clone(), &none)).unwrap();
        assert_eq!(got.path, pointed);
        assert!(matches!(got.source, BinarySource::PointerFile(_)));

        let envd = base.join("envd").join(exe("myapp"));
        touch(&envd, 10);
        let p = envd.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "MYAPP_APP_BINARY").then(|| p.clone());
        let got = resolve(&query("myapp", &app, b.clone(), &env)).unwrap();
        assert_eq!(got.path, envd);
        assert_eq!(got.source, BinarySource::Env("MYAPP_APP_BINARY".into()));

        // With no broker and no pointer, the app-root path wins.
        std::fs::remove_file(app.join(POINTER_FILE)).unwrap();
        let got = resolve(&query("myapp", &app, None, &none)).unwrap();
        assert_eq!(got.source, BinarySource::AppRoot);
    }

    #[test]
    fn invalid_override_is_an_error_not_a_fall_through() {
        let base = tmp("bad");
        let app = base.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let wrong = base.join("wrong").join(exe("helper"));
        touch(&wrong, 1);
        let p = wrong.to_string_lossy().into_owned();
        let env = move |k: &str| (k == APP_BINARY_ENV).then(|| p.clone());
        let e = resolve(&query("myapp", &app, None, &env)).unwrap_err();
        assert!(e.0.contains("is not named myapp"), "{}", e.0);
        let missing = |k: &str| (k == APP_BINARY_ENV).then(|| "nope/myapp".to_string());
        assert!(resolve(&query("myapp", &app, None, &missing))
            .unwrap_err()
            .0
            .contains("does not exist"));
    }

    #[test]
    fn validate_rejects_directories_and_release_dirs() {
        let base = tmp("val");
        let dir = base.join("myapp");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(validate(&dir, "myapp")
            .unwrap_err()
            .0
            .contains("not a file"));
        let rel = base.join("target/release").join(exe("myapp"));
        touch(&rel, 1);
        assert!(validate(&rel, "myapp")
            .unwrap_err()
            .0
            .contains("release-tier"));
        let ok = base.join("target/release-iterate").join(exe("myapp"));
        touch(&ok, 1);
        assert!(validate(&ok, "myapp").is_ok());
    }

    #[test]
    fn marker_scan_finds_narrow_wide_and_chunk_straddling_bytes() {
        let base = tmp("marker");
        let narrow = base.join("n.exe");
        write_exe(&narrow, 1, true);
        assert!(has_qa_marker(&narrow).unwrap());
        let none = base.join("none.exe");
        write_exe(&none, 1, false);
        assert!(!has_qa_marker(&none).unwrap());
        let wide = base.join("w.exe");
        let mut b = vec![7u8; 100];
        b.extend(QA_CONTROL_MARKER.bytes().flat_map(|c| [c, 0]));
        std::fs::write(&wide, b).unwrap();
        assert!(has_qa_marker(&wide).unwrap());
        // The marker straddles the 1 MiB read boundary.
        let big = base.join("big.exe");
        let mut b = vec![1u8; (1 << 20) - 10];
        b.extend_from_slice(QA_CONTROL_MARKER.as_bytes());
        std::fs::write(&big, b).unwrap();
        assert!(has_qa_marker(&big).unwrap());
        // A truncated marker is not a match.
        let part = base.join("part.exe");
        std::fs::write(&part, &QA_CONTROL_MARKER.as_bytes()[..20]).unwrap();
        assert!(!has_qa_marker(&part).unwrap());
    }

    #[test]
    fn broker_skips_non_qa_newest_and_takes_the_qa_one() {
        let base = tmp("skip");
        let app = base.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let ws = broker(&base, &app);
        let qa = ws.join("g/001/t/debug").join(exe("myapp"));
        let plain = ws.join("g/002/t/debug").join(exe("myapp"));
        write_exe(&qa, 500, true);
        write_exe(&plain, 5, false); // newest, but a plain dev build
        let env = |_: &str| None;
        let b = Some(base.join("rightkit-build-control"));
        let got = resolve(&query("myapp", &app, b.clone(), &env)).unwrap();
        assert_eq!(got.path, qa);
        // Only the plain build exists: fail closed with the instructive message.
        std::fs::remove_file(&qa).unwrap();
        let e = resolve(&query("myapp", &app, b, &env)).unwrap_err();
        assert!(
            e.0.contains("but it isn't a test-mode/control build"),
            "{}",
            e.0
        );
        assert!(e.0.contains("--build-ui"), "{}", e.0);
    }

    #[test]
    fn env_and_pointer_without_marker_are_errors() {
        let base = tmp("nomarker");
        let app = base.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let plain = base.join("p").join(exe("myapp"));
        write_exe(&plain, 1, false);
        let p = plain.to_string_lossy().into_owned();
        let env = move |k: &str| (k == APP_BINARY_ENV).then(|| p.clone());
        assert!(resolve(&query("myapp", &app, None, &env))
            .unwrap_err()
            .0
            .contains("isn't a test-mode/control build"));
        std::fs::write(app.join(POINTER_FILE), plain.to_string_lossy().as_bytes()).unwrap();
        let none = |_: &str| None;
        let e = resolve(&query("myapp", &app, None, &none)).unwrap_err();
        assert!(
            e.0.contains("pointer file") && e.0.contains("isn't a test-mode/control build"),
            "{}",
            e.0
        );
    }

    #[test]
    fn app_root_candidate_without_marker_is_refused() {
        let base = tmp("approot");
        let app = base.join("app");
        write_exe(&app.join("target/debug").join(exe("myapp")), 1, false);
        write_exe(&app.join("target/debug/myapp"), 1, false);
        let none = |_: &str| None;
        let e = resolve(&query("myapp", &app, None, &none)).unwrap_err();
        assert!(e.0.contains("isn't a test-mode/control build"), "{}", e.0);
    }

    #[test]
    fn resolve_stashed_never_returns_a_broker_generation_path() {
        let base = tmp("stashed");
        let app = base.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let ws = broker(&base, &app);
        let gen1 = ws.join("g/001/t/debug").join(exe("myapp"));
        touch(&gen1, 10);
        let env = |_: &str| None;
        let b = Some(base.join("rightkit-build-control"));
        let q = query("myapp", &app, b.clone(), &env);
        assert!(matches!(
            resolve(&q).unwrap().source,
            BinarySource::BrokerTarget(_)
        ));
        let got = resolve_stashed(&q).unwrap();
        assert!(matches!(got.source, BinarySource::BrokerTarget(_)));
        assert!(is_stashed(&got.path, &app));
        assert!(got.path.starts_with(app.join(STASH_DIR)));
        assert!(!got.path.starts_with(&ws));
        // A new generation lands in a new folder; the launch path stays the same, `.previous` keeps the old copy.
        let gen2 = ws.join("g/002/t/debug").join(exe("myapp"));
        std::fs::create_dir_all(gen2.parent().unwrap()).unwrap();
        std::fs::write(
            &gen2,
            [b"MZ-gen2".as_slice(), QA_CONTROL_MARKER.as_bytes()].concat(),
        )
        .unwrap();
        let later = resolve_stashed(&query("myapp", &app, b, &env)).unwrap();
        assert_eq!(later.path, got.path);
        assert!(crate::inplace::previous_path(&got.path).is_file());
        // The app-root source is stashed too, and an env override is no exception.
        let rel = app.join("target/debug/myapp");
        touch(&rel, 5);
        let envd = base.join("envd").join(exe("myapp"));
        touch(&envd, 5);
        let p = envd.to_string_lossy().into_owned();
        let env2 = move |k: &str| (k == "MYAPP_APP_BINARY").then(|| p.clone());
        let from_env = resolve_stashed(&query("myapp", &app, None, &env2)).unwrap();
        assert!(from_env.path.starts_with(app.join(STASH_DIR)));
        assert_eq!(
            from_env.source,
            BinarySource::Env("MYAPP_APP_BINARY".into())
        );
        // The installed-app refusal from 0.6.3 still applies to the resolved source.
        assert!(is_stashed(&from_env.path, &app) && !is_stashed(&envd, &app));
    }

    #[test]
    fn stash_copies_atomically_and_writes_the_pointer() {
        let base = tmp("stash");
        let app = base.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let built = base
            .join("target/x86_64-pc-windows-msvc/debug")
            .join(exe("myapp"));
        write_exe(&built, 5, true);
        std::fs::write(built.with_extension("pdb"), b"pdb").unwrap();
        let got = stash_qa_build(&built, &app, None).unwrap();
        assert_eq!(
            got,
            app.join(".rightkit-qa-bin/x86_64-pc-windows-msvc")
                .join(exe("myapp"))
        );
        assert!(has_qa_marker(&got).unwrap());
        assert!(got.with_extension("pdb").is_file());
        // Stashing a different build replaces it in place and keeps exactly one `.previous`.
        write_exe(&built, 3, true);
        std::fs::write(
            &built,
            [b"MZ-second".as_slice(), QA_CONTROL_MARKER.as_bytes()].concat(),
        )
        .unwrap();
        let again = stash_qa_build(&built, &app, None).unwrap();
        assert_eq!(again, got);
        let prev = crate::inplace::previous_path(&got);
        assert!(prev.is_file(), "the earlier stash is kept as .previous");
        assert!(!has_qa_marker(&prev).is_err());
        // A later plain dev build overwrites the original; the stashed copy and pointer still win.
        write_exe(&built, 1, false);
        let none = |_: &str| None;
        let r = resolve(&query("myapp", &app, None, &none)).unwrap();
        assert_eq!(r.path, got);
        assert!(matches!(r.source, BinarySource::PointerFile(_)));
        // No triple directory: stored under `host`.
        let plain_layout = base.join("t2/debug").join(exe("myapp"));
        write_exe(&plain_layout, 1, true);
        assert!(stash_qa_build(&plain_layout, &app, None)
            .unwrap()
            .starts_with(app.join(".rightkit-qa-bin/host")));
        // A build without the marker is refused.
        let bad = base.join("t3/debug").join(exe("myapp"));
        write_exe(&bad, 1, false);
        assert!(stash_qa_build(&bad, &app, None).is_err());
    }
}
