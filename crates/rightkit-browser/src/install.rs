//! Installer-time and updater-time delivery of Chrome for Testing to ONE fixed path
//! (cargo feature `install`).
//!
//! [`ensure_chrome_for_testing`] puts a pinned, digest-checked Chrome for Testing at
//! `<managed root>/current`, keeps exactly one rollback copy at `current.previous`, and
//! extracts into `current.staging` first, so `current` is never half written. No per-version
//! folder and no new id: every app resolves the same path ([`chrome_for_testing_path`]
//! prefers `current`).
//!
//! * Download: `rightkit-models`' vetted, resumable, hash-verified downloader (rightkit-http
//!   address policy and redirect vetting). This module has no HTTP client of its own.
//! * Digest: each pinned archive's SHA-256 is required ([`Sha256Pin`]). A pin entry whose digest
//!   is still [`Sha256Pin::Todo`] is refused with [`InstallError::DigestNotPinned`]; nothing is
//!   downloaded for it. The digest is checked again on the file on disk before it is opened.
//! * Extraction: zip only, hardened: every member must sit under the expected top-level
//!   folder, no absolute or `..` paths, no duplicate members, nothing written through a symlink,
//!   symlinks only on Unix and only with a relative target that stays inside the folder, bounded
//!   member count and total size, no set-id bits.
//! * Windows: Chrome's sandbox read-and-execute grant is applied to the staged folder before the
//!   swap, so `current` never appears without it.
//! * Idempotent: when `current/install.json` already records the pinned version, platform and
//!   digests and the executable exists, nothing is downloaded.
//!
//! [`chrome_for_testing_path`]: crate::chrome_for_testing_path

use crate::config::{executable_in, platform_key, ManagedRoot};
use rightkit_fs::digest::sha256_file;
use rightkit_fs::{FileLock, LockOptions};
use rightkit_models::download::{download_resumable, part_path_for, DownloadRequest};
use rightkit_models::http::{http_client, ModelHttp};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// How long an install waits for another install of the same managed root.
const LOCK_WAIT: Duration = Duration::from_secs(300);
/// Most members one archive may hold (Chrome for Testing has a few thousand).
const MAX_MEMBERS: usize = 50_000;
/// Most bytes one archive may expand to (Chrome for Testing is about 0.5 GiB).
const MAX_EXPANDED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Marker written into `current` once an install is complete.
const MARKER: &str = "install.json";

/// Which Chrome for Testing archive a [`PinnedDownload`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadKind {
    /// `chrome-<platform>.zip`: the browser (required on every platform).
    Chrome,
    /// `chrome-headless-shell-<platform>.zip`: the Windows fixture's browser (see
    /// `chrome-headless-shell` in the crate changelog); optional elsewhere.
    HeadlessShell,
}

impl DownloadKind {
    /// The single top-level folder every member of this archive must sit under.
    fn top_level(self, platform: &str) -> String {
        match self {
            Self::Chrome => format!("chrome-{platform}"),
            Self::HeadlessShell => format!("chrome-headless-shell-{platform}"),
        }
    }
}

/// A pinned archive digest. **Required:** the installer refuses [`Sha256Pin::Todo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sha256Pin {
    /// Lowercase hex SHA-256 of the archive (64 characters).
    Pinned(&'static str),
    /// TODO(#267): the published digest has not been recorded yet. Never guess one: compute it
    /// from the archive Google serves at the pinned URL, record it here, and review the change.
    Todo,
}

/// One archive of a pinned Chrome for Testing build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedDownload {
    /// Google's platform key: `mac-arm64`, `mac-x64`, `win64` or `linux64`.
    pub platform: &'static str,
    pub kind: DownloadKind,
    /// `https` URL of the archive. Immutable: it names the version.
    pub url: &'static str,
    pub sha256: Sha256Pin,
}

/// A Chrome for Testing version with a per-platform URL and SHA-256 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedChrome {
    /// Dotted numeric version, for example `155.0.8059.39`.
    pub version: &'static str,
    pub downloads: &'static [PinnedDownload],
}

macro_rules! cft_url {
    ($version:literal, $platform:literal, $name:literal) => {
        concat!(
            "https://storage.googleapis.com/chrome-for-testing-public/",
            $version,
            "/",
            $platform,
            "/",
            $name,
            "-",
            $platform,
            ".zip"
        )
    };
}

macro_rules! cft_chrome {
    ($version:literal, $platform:literal) => {
        PinnedDownload {
            platform: $platform,
            kind: DownloadKind::Chrome,
            url: cft_url!($version, $platform, "chrome"),
            sha256: Sha256Pin::Todo,
        }
    };
    // Digest recorded by `scripts/chrome-pins.mjs --apply` from the real download (#267).
    ($version:literal, $platform:literal, $sha:literal) => {
        PinnedDownload {
            platform: $platform,
            kind: DownloadKind::Chrome,
            url: cft_url!($version, $platform, "chrome"),
            sha256: Sha256Pin::Pinned($sha),
        }
    };
}

/// The Chrome for Testing build this crate version installs.
///
/// Every archive below carries a recorded SHA-256 (computed once from the real archive by
/// `scripts/chrome-pins.mjs --apply`, because Google publishes no digests for Chrome for Testing).
/// A new version is pinned only together with its digests; an entry left as [`Sha256Pin::Todo`]
/// makes [`ensure_chrome_for_testing`] return [`InstallError::DigestNotPinned`] before any download.
pub const PINNED_CHROME_FOR_TESTING: PinnedChrome = PinnedChrome {
    version: "155.0.8059.39",
    downloads: &[
        cft_chrome!(
            "155.0.8059.39",
            "mac-arm64",
            "529a71bd61aaa2ef266a4d4bd300ae9572ba6a3468a8d55c3023ffeffb5b6b4e"
        ),
        cft_chrome!(
            "155.0.8059.39",
            "mac-x64",
            "9e4da7961d4a426078f02e89ecfa569a7926ada2dc88aa53c0bd9ed0d46c3bb7"
        ),
        cft_chrome!(
            "155.0.8059.39",
            "linux64",
            "55672d1f392fd3e7b7a08621b6e804e6bcb39d40cf155504abb74b3a021ea8ea"
        ),
        cft_chrome!(
            "155.0.8059.39",
            "win64",
            "59ab2a6e99bde9c0bc180414988394f9f5355a0d3764c704151ee8ecea235c7e"
        ),
        PinnedDownload {
            platform: "win64",
            kind: DownloadKind::HeadlessShell,
            url: cft_url!("155.0.8059.39", "win64", "chrome-headless-shell"),
            sha256: Sha256Pin::Pinned(
                "20798f7c51a22def0b3d02e526a8f52e7ee2c7eddcde9d81a4c23f618e8b5436",
            ),
        },
    ],
};

/// Result of [`ensure_chrome_for_testing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeInstall {
    pub version: String,
    /// The Chrome executable inside `current`.
    pub executable: PathBuf,
    /// False when the pinned version was already installed and nothing was downloaded.
    pub changed: bool,
}

/// Why an install did not happen. The managed `current` is unchanged on every error except an
/// [`InstallError::Swap`] that reports a failed rollback.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("the pin has no Chrome for Testing download for platform {0}")]
    UnsupportedPlatform(String),
    #[error("pinned {kind:?} digest for {platform} is not recorded (Sha256Pin::Todo); refusing to download without one")]
    DigestNotPinned {
        platform: String,
        kind: DownloadKind,
    },
    #[error("invalid pin: {0}")]
    InvalidPin(String),
    #[error("another install holds {0}: {1}")]
    Busy(PathBuf, String),
    #[error("download of {url} failed: {message}")]
    Download { url: String, message: String },
    #[error("{url}: archive digest {actual} does not match the pinned {expected}")]
    DigestMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    #[error("unsafe or invalid archive: {0}")]
    Archive(String),
    #[error("Windows sandbox grant failed: {0}")]
    SandboxGrant(String),
    #[error("could not swap the new install into place: {0}")]
    Swap(String),
    #[error("io on {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct MarkerFile {
    kind: DownloadKind,
    url: String,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Marker {
    version: String,
    platform: String,
    files: Vec<MarkerFile>,
}

/// A validated plan: what to fetch for this platform, digests normalized to lowercase.
struct Plan<'a> {
    platform: &'a str,
    items: Vec<(&'a PinnedDownload, String)>,
}

fn plan<'a>(pin: &'a PinnedChrome, platform: &'a str) -> Result<Plan<'a>, InstallError> {
    let version_ok = !pin.version.is_empty()
        && pin
            .version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !version_ok {
        return Err(InstallError::InvalidPin(format!(
            "version {:?} is not a dotted numeric version",
            pin.version
        )));
    }
    let mut items = Vec::new();
    for d in pin.downloads.iter().filter(|d| d.platform == platform) {
        if !d.url.starts_with("https://") {
            return Err(InstallError::InvalidPin(format!(
                "{} is not an https URL",
                d.url
            )));
        }
        if items
            .iter()
            .any(|(o, _): &(&PinnedDownload, String)| o.kind == d.kind)
        {
            return Err(InstallError::InvalidPin(format!(
                "two {:?} downloads for {platform}",
                d.kind
            )));
        }
        let digest = match d.sha256 {
            Sha256Pin::Pinned(hex) => {
                if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(InstallError::InvalidPin(format!(
                        "digest for {} is not 64 hex characters",
                        d.url
                    )));
                }
                hex.to_ascii_lowercase()
            }
            Sha256Pin::Todo => {
                return Err(InstallError::DigestNotPinned {
                    platform: platform.to_string(),
                    kind: d.kind,
                })
            }
        };
        items.push((d, digest));
    }
    if !items.iter().any(|(d, _)| d.kind == DownloadKind::Chrome) {
        return Err(InstallError::UnsupportedPlatform(platform.to_string()));
    }
    Ok(Plan { platform, items })
}

fn marker_for(pin: &PinnedChrome, plan: &Plan<'_>) -> Marker {
    Marker {
        version: pin.version.to_string(),
        platform: plan.platform.to_string(),
        files: plan
            .items
            .iter()
            .map(|(d, sha)| MarkerFile {
                kind: d.kind,
                url: d.url.to_string(),
                sha256: sha.clone(),
            })
            .collect(),
    }
}

/// Put the pinned Chrome for Testing at `root/current` (see the module docs). Blocking: call it
/// from an installer or updater step, or from `spawn_blocking`.
pub fn ensure_chrome_for_testing(
    root: &ManagedRoot,
    pin: &PinnedChrome,
) -> Result<ChromeInstall, InstallError> {
    let http = http_client();
    ensure_with(root, pin, platform_key(), &|d, sha, dest| {
        fetch(&http, d, sha, dest)
    })
}

fn fetch(
    http: &ModelHttp,
    d: &PinnedDownload,
    sha256: &str,
    dest: &Path,
) -> Result<(), InstallError> {
    let part = part_path_for(dest);
    download_resumable(
        http,
        DownloadRequest {
            url: d.url,
            part_path: &part,
            dest_path: dest,
            expected_sha256: Some(sha256),
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .map(|_| ())
    .map_err(|e| InstallError::Download {
        url: d.url.to_string(),
        message: e.to_string(),
    })
}

type Fetch<'a> = &'a dyn Fn(&PinnedDownload, &str, &Path) -> Result<(), InstallError>;

fn ensure_with(
    root: &ManagedRoot,
    pin: &PinnedChrome,
    platform: &str,
    fetch: Fetch<'_>,
) -> Result<ChromeInstall, InstallError> {
    let plan = plan(pin, platform)?;
    let marker = marker_for(pin, &plan);
    let base = root.path();
    fs::create_dir_all(base).map_err(io_err(base))?;
    let lock_path = base.join("current.lock");
    let _lock = FileLock::acquire(
        &lock_path,
        &LockOptions::wait(LOCK_WAIT).labeled("rightkit-browser chrome-for-testing install"),
    )
    .map_err(|e| InstallError::Busy(lock_path.clone(), e.to_string()))?;

    let current = root.current();
    let executable = executable_in(&current);
    if installed_marker(&current).as_ref() == Some(&marker) && executable.is_file() {
        #[cfg(windows)]
        grant_sandbox_access(&current, &executable)?;
        return Ok(ChromeInstall {
            version: pin.version.to_string(),
            executable,
            changed: false,
        });
    }

    let staging = root.staging();
    let downloads = base.join("current.download");
    remove_dir_if_exists(&staging)?;
    fs::create_dir_all(&downloads).map_err(io_err(&downloads))?;
    fs::create_dir(&staging).map_err(io_err(&staging))?;

    let built = (|| {
        for (d, sha) in &plan.items {
            let archive = downloads.join(format!("{}.zip", d.kind.top_level(plan.platform)));
            fetch(d, sha, &archive)?;
            // Never open an archive whose digest was not checked here, whatever fetched it.
            let actual = sha256_file(&archive).map_err(io_err(&archive))?;
            if &actual != sha {
                let _ = fs::remove_file(&archive);
                return Err(InstallError::DigestMismatch {
                    url: d.url.to_string(),
                    expected: sha.clone(),
                    actual,
                });
            }
            extract_zip(&archive, &staging, &d.kind.top_level(plan.platform))?;
        }
        if !executable_in(&staging).is_file() {
            return Err(InstallError::Archive(format!(
                "the archive has no Chrome executable at {}",
                executable_in(&staging).display()
            )));
        }
        let json = serde_json::to_vec_pretty(&marker)
            .map_err(|e| InstallError::Archive(format!("marker: {e}")))?;
        let marker_path = staging.join(MARKER);
        fs::write(&marker_path, json).map_err(io_err(&marker_path))?;
        #[cfg(windows)]
        grant_sandbox_access(&staging, &executable_in(&staging))?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&downloads);
    if let Err(e) = built {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    swap_into_place(root).inspect_err(|_| {
        let _ = fs::remove_dir_all(&staging);
    })?;
    Ok(ChromeInstall {
        version: pin.version.to_string(),
        executable,
        changed: true,
    })
}

fn installed_marker(current: &Path) -> Option<Marker> {
    let bytes = fs::read(current.join(MARKER)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn remove_dir_if_exists(path: &Path) -> Result<(), InstallError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path)(e)),
    }
}

/// `current` -> `current.previous` (the one rollback copy), `current.staging` -> `current`.
/// A failure after the first step puts the old `current` back.
fn swap_into_place(root: &ManagedRoot) -> Result<(), InstallError> {
    let (current, previous, staging) = (root.current(), root.previous(), root.staging());
    let had_current = current.exists();
    if had_current {
        remove_dir_if_exists(&previous)?;
        fs::rename(&current, &previous).map_err(|e| {
            InstallError::Swap(format!(
                "cannot move {} aside (is Chrome for Testing running?): {e}",
                current.display()
            ))
        })?;
    }
    if let Err(e) = fs::rename(&staging, &current) {
        let restored = !had_current || fs::rename(&previous, &current).is_ok();
        return Err(InstallError::Swap(format!(
            "cannot move {} into place: {e}{}",
            staging.display(),
            if restored {
                ""
            } else {
                "; the previous install could not be put back and is at current.previous"
            }
        )));
    }
    Ok(())
}

/// A member's path under `staging`, or `None` when it is absolute, escapes, or is not made of
/// plain components. `top` must be its first component.
fn safe_member_path(name: &str, top: &str) -> Option<PathBuf> {
    if name.contains('\\') || name.contains('\0') {
        return None;
    }
    let mut out = PathBuf::new();
    let mut first = true;
    for part in Path::new(name).components() {
        match part {
            Component::Normal(p) => {
                let p = p.to_str()?;
                if p.contains(':') {
                    return None;
                }
                if first && p != top {
                    return None;
                }
                first = false;
                out.push(p);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!first).then_some(out)
}

/// True when `target`, a symlink target placed at `link` (both relative to the staging root),
/// is relative and stays under the first component of `link`.
#[cfg(any(unix, test))]
fn symlink_target_stays_inside(link: &Path, target: &str) -> bool {
    if target.is_empty() || target.contains('\\') || target.contains('\0') {
        return false;
    }
    let t = Path::new(target);
    if t.is_absolute() {
        return false;
    }
    // Depth of the link's folder below the top-level folder; `..` may not climb above it.
    let mut depth = link.components().count().saturating_sub(2);
    for part in t.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ => return false,
        }
    }
    true
}

fn archive_err(message: impl std::fmt::Display) -> InstallError {
    InstallError::Archive(message.to_string())
}

/// Refuse to create anything below a symlink or reparse point already in `staging`.
fn ensure_no_link_ancestors(staging: &Path, relative: &Path) -> Result<(), InstallError> {
    let mut at = staging.to_path_buf();
    let count = relative.components().count();
    for (i, part) in relative.components().enumerate() {
        at.push(part);
        if i + 1 == count {
            break;
        }
        match fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(archive_err(format!(
                    "member path passes through a symlink: {}",
                    relative.display()
                )))
            }
            Ok(m) if !m.is_dir() => {
                return Err(archive_err(format!(
                    "member path passes through a file: {}",
                    relative.display()
                )))
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&at)(e)),
        }
    }
    Ok(())
}

fn extract_zip(archive: &Path, staging: &Path, top: &str) -> Result<(), InstallError> {
    let file = fs::File::open(archive).map_err(io_err(archive))?;
    let mut zip = zip::ZipArchive::new(file).map_err(archive_err)?;
    if zip.is_empty() || zip.len() > MAX_MEMBERS {
        return Err(archive_err(format!("{} members", zip.len())));
    }
    let mut budget = MAX_EXPANDED_BYTES;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(archive_err)?;
        let name = entry.name().map_err(archive_err)?.to_string();
        let relative = safe_member_path(&name, top)
            .ok_or_else(|| archive_err(format!("unsafe member path {name:?}")))?;
        ensure_no_link_ancestors(staging, &relative)?;
        let dest = staging.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&dest).map_err(io_err(&dest))?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        if entry.is_symlink() {
            #[cfg(unix)]
            {
                let mut target = String::new();
                entry
                    .by_ref()
                    .take(4097)
                    .read_to_string(&mut target)
                    .map_err(archive_err)?;
                if !symlink_target_stays_inside(&relative, &target) {
                    return Err(archive_err(format!(
                        "symlink {name:?} points outside its folder: {target:?}"
                    )));
                }
                std::os::unix::fs::symlink(&target, &dest).map_err(io_err(&dest))?;
                continue;
            }
            #[cfg(not(unix))]
            return Err(archive_err(format!("symlink member {name:?} is refused")));
        }
        if !entry.is_file() {
            return Err(archive_err(format!(
                "member {name:?} is not a regular file"
            )));
        }
        budget = budget
            .checked_sub(entry.size())
            .ok_or_else(|| archive_err("archive expands beyond the size limit"))?;
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    archive_err(format!("duplicate member {name:?}"))
                } else {
                    io_err(&dest)(e)
                }
            })?;
        let declared = entry.size();
        let written =
            io::copy(&mut entry.by_ref().take(declared + 1), &mut out).map_err(io_err(&dest))?;
        if written != declared {
            return Err(archive_err(format!(
                "member {name:?} is not its declared size"
            )));
        }
        drop(out);
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // Keep the execute bits; never set-id, sticky or group/other write.
            fs::set_permissions(&dest, fs::Permissions::from_mode((mode & 0o755) | 0o600))
                .map_err(io_err(&dest))?;
        }
    }
    Ok(())
}

/// Windows: give Chrome's sandbox read and execute on `directory` (the `icacls` grant described
/// by `BrowserError::ChromeSandboxAccess`) and check it took. An unpacked Chrome is unreadable
/// to its own AppContainer until this runs.
#[cfg(windows)]
fn grant_sandbox_access(directory: &Path, executable: &Path) -> Result<(), InstallError> {
    use crate::sandbox_access::{
        missing_access, ALL_APPLICATION_PACKAGES, ALL_RESTRICTED_APPLICATION_PACKAGES,
    };
    if missing_access(executable).is_none() {
        return Ok(());
    }
    let icacls = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("icacls.exe"))
        .unwrap_or_else(|| PathBuf::from("icacls.exe"));
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: icacls is a console tool; never flash a window from a GUI app.
    let status = std::process::Command::new(icacls)
        .creation_flags(0x0800_0000)
        .arg(directory)
        .arg("/grant")
        .arg(format!("*{ALL_APPLICATION_PACKAGES}:(OI)(CI)(RX)"))
        .arg(format!(
            "*{ALL_RESTRICTED_APPLICATION_PACKAGES}:(OI)(CI)(RX)"
        ))
        .args(["/T", "/C", "/Q"])
        .status()
        .map_err(|e| InstallError::SandboxGrant(e.to_string()))?;
    if !status.success() {
        return Err(InstallError::SandboxGrant(format!(
            "icacls exited {status}"
        )));
    }
    match missing_access(executable) {
        None => Ok(()),
        Some(m) => Err(InstallError::SandboxGrant(format!(
            "{} still lacks access for {}",
            m.directory.display(),
            m.principals.join(" and ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn pinned(url: &'static str, sha: Sha256Pin) -> PinnedDownload {
        PinnedDownload {
            platform: "linux64",
            kind: DownloadKind::Chrome,
            url,
            sha256: sha,
        }
    }

    /// A zip with the given (name, bytes) members, written to `path`; returns its digest.
    fn write_zip(path: &Path, members: &[(&str, &[u8])]) -> String {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        for (name, data) in members {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        sha256_file(path).unwrap()
    }

    fn exe_member() -> String {
        let exe = executable_in(Path::new("."));
        let exe = exe.to_string_lossy().replace('\\', "/");
        exe.trim_start_matches("./").to_string()
    }

    /// Leaked pin for tests: `Sha256Pin::Pinned` needs `'static`.
    fn leak_pin(version: &'static str, platform: &'static str, sha: &str) -> &'static PinnedChrome {
        let sha: &'static str = Box::leak(sha.to_string().into_boxed_str());
        let downloads: &'static [PinnedDownload] = Box::leak(Box::new([PinnedDownload {
            platform,
            kind: DownloadKind::Chrome,
            url: "https://example.invalid/chrome.zip",
            sha256: Sha256Pin::Pinned(sha),
        }]));
        Box::leak(Box::new(PinnedChrome { version, downloads }))
    }

    #[test]
    fn the_shipped_pin_table_is_a_typed_todo_until_real_digests_are_recorded() {
        for d in PINNED_CHROME_FOR_TESTING.downloads {
            assert!(d
                .url
                .starts_with("https://storage.googleapis.com/chrome-for-testing-public/"));
            assert!(d.url.contains(PINNED_CHROME_FOR_TESTING.version));
            if d.sha256 == Sha256Pin::Todo {
                let err = plan(&PINNED_CHROME_FOR_TESTING, d.platform).err().unwrap();
                assert!(matches!(err, InstallError::DigestNotPinned { .. }));
            }
        }
    }

    #[test]
    fn a_missing_digest_is_refused_before_anything_is_fetched() {
        let temp = tempfile::tempdir().unwrap();
        let root = ManagedRoot::at(temp.path().join("managed"));
        let downloads: &'static [PinnedDownload] = Box::leak(Box::new([pinned(
            "https://example.invalid/c.zip",
            Sha256Pin::Todo,
        )]));
        let pin = PinnedChrome {
            version: "1.2.3.4",
            downloads,
        };
        let err = ensure_with(&root, &pin, "linux64", &|_, _, _| panic!("fetched")).unwrap_err();
        assert!(matches!(err, InstallError::DigestNotPinned { .. }));
        assert!(!root.current().exists() && !root.staging().exists());
    }

    #[test]
    fn plan_rejects_bad_pins_and_unknown_platforms() {
        let bad_digest = leak_pin("1.2.3.4", "linux64", "not-hex");
        assert!(matches!(
            plan(bad_digest, "linux64"),
            Err(InstallError::InvalidPin(_))
        ));
        let bad_version = leak_pin("1.2.x", "linux64", SHA_A);
        assert!(matches!(
            plan(bad_version, "linux64"),
            Err(InstallError::InvalidPin(_))
        ));
        let ok = leak_pin("1.2.3.4", "linux64", SHA_A);
        assert!(matches!(
            plan(ok, "win64"),
            Err(InstallError::UnsupportedPlatform(_))
        ));
        let http_url: &'static [PinnedDownload] = Box::leak(Box::new([pinned(
            "http://example.invalid/c.zip",
            Sha256Pin::Pinned(SHA_A),
        )]));
        let pin = PinnedChrome {
            version: "1.2.3.4",
            downloads: http_url,
        };
        assert!(matches!(
            plan(&pin, "linux64"),
            Err(InstallError::InvalidPin(_))
        ));
    }

    #[test]
    fn member_paths_must_stay_under_the_expected_top_level_folder() {
        let top = "chrome-linux64";
        assert_eq!(
            safe_member_path("chrome-linux64/chrome", top).unwrap(),
            Path::new("chrome-linux64").join("chrome")
        );
        for bad in [
            "../chrome-linux64/chrome",
            "/chrome-linux64/chrome",
            "chrome-linux64/../../x",
            "other/chrome",
            "chrome-linux64\\chrome",
            "chrome-linux64/a:b",
            "C:/x",
            "",
        ] {
            assert!(safe_member_path(bad, top).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn symlink_targets_may_not_leave_their_top_level_folder() {
        let link = Path::new("chrome-mac-arm64/App/Frameworks/Current");
        assert!(symlink_target_stays_inside(link, "A"));
        assert!(symlink_target_stays_inside(link, "../Resources/x"));
        assert!(!symlink_target_stays_inside(link, "/etc/passwd"));
        assert!(!symlink_target_stays_inside(link, "../../../../.."));
        assert!(!symlink_target_stays_inside(link, ""));
    }

    #[test]
    fn extraction_refuses_traversal_duplicates_and_foreign_top_levels() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("a.zip");
        let top = format!("chrome-{}", platform_key());
        for members in [
            vec![("../evil", &b"x"[..])],
            vec![("elsewhere/chrome", &b"x"[..])],
            vec![
                (format!("{top}/a").leak() as &str, &b"1"[..]),
                (format!("{top}/a").leak() as &str, &b"2"[..]),
            ],
        ] {
            write_zip(&archive, &members);
            let staging = temp.path().join("staging");
            let _ = fs::remove_dir_all(&staging);
            fs::create_dir(&staging).unwrap();
            let err = extract_zip(&archive, &staging, &top).unwrap_err();
            assert!(matches!(err, InstallError::Archive(_)), "{err}");
            assert!(!temp.path().join("evil").exists());
        }
    }

    fn pin_for(archive: &Path) -> &'static PinnedChrome {
        let sha = sha256_file(archive).unwrap();
        leak_pin("155.0.1.1", platform_key(), &sha)
    }

    fn fake_fetch(
        source: PathBuf,
    ) -> impl Fn(&PinnedDownload, &str, &Path) -> Result<(), InstallError> {
        move |_, _, dest| fs::copy(&source, dest).map(|_| ()).map_err(io_err(dest))
    }

    #[test]
    fn install_extracts_swaps_into_current_and_keeps_one_previous() {
        let temp = tempfile::tempdir().unwrap();
        let root = ManagedRoot::at(temp.path().join("managed"));
        let exe = exe_member();
        let first = temp.path().join("first.zip");
        write_zip(&first, &[(&exe, b"one")]);
        let pin1 = pin_for(&first);
        let out = ensure_with(&root, pin1, platform_key(), &fake_fetch(first)).unwrap();
        assert!(out.changed);
        assert_eq!(out.executable, root.executable());
        assert_eq!(fs::read(&out.executable).unwrap(), b"one");
        assert!(root.current().join(MARKER).is_file());
        assert!(!root.staging().exists() && !root.previous().exists());

        // Same pin again: idempotent, no fetch.
        let again = ensure_with(&root, pin1, platform_key(), &|_, _, _| panic!("fetched")).unwrap();
        assert!(!again.changed);

        // A new pin: current becomes previous; a third install drops the oldest.
        let second = temp.path().join("second.zip");
        write_zip(&second, &[(&exe, b"two")]);
        let pin2 = pin_for(&second);
        assert!(
            ensure_with(&root, pin2, platform_key(), &fake_fetch(second))
                .unwrap()
                .changed
        );
        assert_eq!(fs::read(root.executable()).unwrap(), b"two");
        assert_eq!(fs::read(executable_in(&root.previous())).unwrap(), b"one");
        let third = temp.path().join("third.zip");
        write_zip(&third, &[(&exe, b"three")]);
        let pin3 = pin_for(&third);
        ensure_with(&root, pin3, platform_key(), &fake_fetch(third)).unwrap();
        assert_eq!(fs::read(executable_in(&root.previous())).unwrap(), b"two");
        let entries: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with("current.lock"))
            .collect();
        let mut sorted = entries.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["current", "current.previous"]);
    }

    #[test]
    fn a_digest_mismatch_leaves_current_untouched_and_extracts_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = ManagedRoot::at(temp.path().join("managed"));
        let exe = exe_member();
        let good = temp.path().join("good.zip");
        write_zip(&good, &[(&exe, b"good")]);
        let pin = pin_for(&good);
        ensure_with(&root, pin, platform_key(), &fake_fetch(good)).unwrap();

        // The pin says SHA_A, the "download" is something else.
        let other = temp.path().join("other.zip");
        write_zip(&other, &[(&exe, b"tampered")]);
        let bad_pin = leak_pin("156.0.0.0", platform_key(), SHA_A);
        let err = ensure_with(&root, bad_pin, platform_key(), &fake_fetch(other)).unwrap_err();
        assert!(matches!(err, InstallError::DigestMismatch { .. }), "{err}");
        assert_eq!(fs::read(root.executable()).unwrap(), b"good");
        assert!(!root.staging().exists() && !root.previous().exists());
    }

    #[test]
    fn an_archive_without_the_executable_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = ManagedRoot::at(temp.path().join("managed"));
        let top = format!("chrome-{}", platform_key());
        let archive = temp.path().join("empty.zip");
        write_zip(&archive, &[(&format!("{top}/readme.txt"), b"x")]);
        let err = ensure_with(
            &root,
            pin_for(&archive),
            platform_key(),
            &fake_fetch(archive),
        )
        .unwrap_err();
        assert!(matches!(err, InstallError::Archive(_)), "{err}");
        assert!(!root.current().exists() && !root.staging().exists());
    }
}
