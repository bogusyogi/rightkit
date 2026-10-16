//! Build-time provisioning of a verified ONNX Runtime bundle (`provision` feature).
//!
//! Run-once, never continuous: a build script or `xtask` calls [`provision_runtime`], which
//! downloads the catalog archive for a target, checks its SHA-256 before opening it, extracts only
//! the runtime files, checks each extracted file against the catalog digest, and writes the
//! `manifest.json` that [`crate::verify_runtime_manifest`] accepts. Nothing here loads a library
//! or creates a session, and nothing runs at app run time.
//!
//! Catalog rows come from the suite native-library catalog in `rightkit-ffmpeg`. Only rows whose
//! status is `approved` or `pinned` are used; `planned` and stale rows are refused. Per target:
//!
//! | target | archive | files written to the bundle |
//! |---|---|---|
//! | `windows-x64` | NuGet `Microsoft.ML.OnnxRuntime.DirectML` and `Microsoft.AI.DirectML` (zip) | `onnxruntime.dll`, `DirectML.dll` |
//! | `macos-arm64` | `onnxruntime-osx-arm64-<version>.tgz` | `libonnxruntime.dylib` |
//! | `android-arm64` | `onnxruntime-android-<version>.aar` (zip), member `jni/arm64-v8a/libonnxruntime.so` | `libonnxruntime.so` |
//!
//! `android-arm64` needs an approved or pinned catalog row (`os` `android`, `arch` `aarch64`,
//! `artifactFileName` `libonnxruntime.so`, the `.aar` as the archive). No such row ships today, so
//! provisioning that target returns a `Row` error until the catalog gains one; the AAR member path
//! is unverified against a real AAR.
//!
//! The bundle directory is owned by provisioning: any other content in it is removed when the
//! bundle is replaced, because `verify_runtime_manifest` rejects unlisted files.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use rightkit_ffmpeg::native::{
    NativeCatalog, NativeCatalogError, NativeLibrary, NativeRecord, NativeStatus,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{verify_runtime_manifest, RuntimeError, VerifiedRuntime};

/// Platforms with a catalog-pinned runtime archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProvisionTarget {
    /// ONNX Runtime with the DirectML execution provider, plus `DirectML.dll`.
    WindowsX64,
    /// ONNX Runtime for Apple silicon (CPU and CoreML in one dylib).
    MacosArm64,
    /// ONNX Runtime for Android arm64-v8a, taken from the Maven AAR (UNVERIFIED: no catalog row
    /// and no real AAR checked yet).
    AndroidArm64,
}

impl ProvisionTarget {
    /// Parse `windows-x64`, `macos-arm64` or `android-arm64`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "windows-x64" => Some(Self::WindowsX64),
            "macos-arm64" => Some(Self::MacosArm64),
            "android-arm64" => Some(Self::AndroidArm64),
            _ => None,
        }
    }

    /// The target Cargo is compiling for, from `CARGO_CFG_TARGET_OS` and `CARGO_CFG_TARGET_ARCH`
    /// (set for build scripts). `None` outside a build script or for an unsupported target.
    pub fn from_cargo_env() -> Option<Self> {
        let os = std::env::var("CARGO_CFG_TARGET_OS").ok()?;
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok()?;
        match (os.as_str(), arch.as_str()) {
            ("windows", "x86_64") => Some(Self::WindowsX64),
            ("macos", "aarch64") => Some(Self::MacosArm64),
            ("android", "aarch64") => Some(Self::AndroidArm64),
            _ => None,
        }
    }

    /// The machine this code runs on.
    pub fn host() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => Some(Self::WindowsX64),
            ("macos", "aarch64") => Some(Self::MacosArm64),
            ("android", "aarch64") => Some(Self::AndroidArm64),
            _ => None,
        }
    }

    /// Manifest `platform` value.
    pub fn platform(self) -> &'static str {
        match self {
            Self::WindowsX64 => "windows-x64",
            Self::MacosArm64 => "macos-arm64",
            Self::AndroidArm64 => "android-arm64",
        }
    }

    fn os_arch(self) -> (&'static str, &'static str) {
        match self {
            Self::WindowsX64 => ("windows", "x86_64"),
            Self::MacosArm64 => ("macos", "aarch64"),
            Self::AndroidArm64 => ("android", "aarch64"),
        }
    }

    /// Manifest `onnxruntime.variant`.
    fn variant(self) -> &'static str {
        match self {
            Self::WindowsX64 => "directml",
            Self::MacosArm64 => "coreml",
            // Claims no accelerator: which providers the AAR build contains is checked at run time.
            Self::AndroidArm64 => "cpu",
        }
    }
}

/// Member of the ONNX Runtime Android AAR (a zip) holding the arm64-v8a runtime. UNVERIFIED
/// against a real AAR: confirm before relying on it.
const ANDROID_ARM64_AAR_MEMBER: &str = "jni/arm64-v8a/libonnxruntime.so";

/// Why provisioning failed. Nothing is left in the destination when an error returns, except that
/// a previous bundle is kept as it was.
#[derive(Debug)]
pub enum ProvisionError {
    /// Catalog could not be read or parsed.
    Catalog(String),
    /// No single approved or pinned row for the library on the target, or the row cannot be used.
    Row {
        library: &'static str,
        reason: String,
    },
    /// The archive could not be fetched.
    Fetch { url: String, message: String },
    /// A downloaded archive or an extracted file does not match its catalog digest.
    Digest {
        what: String,
        expected: String,
        actual: String,
    },
    /// A member could not be extracted from the archive.
    Extract {
        archive: PathBuf,
        member: String,
        message: String,
    },
    /// Filesystem failure while staging or replacing the bundle.
    Io { path: PathBuf, message: String },
    /// The staged or final bundle failed `verify_runtime_manifest`.
    Verify(RuntimeError),
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(m) => write!(f, "native catalog: {m}"),
            Self::Row { library, reason } => write!(f, "{library} catalog row: {reason}"),
            Self::Fetch { url, message } => write!(f, "fetch {url}: {message}"),
            Self::Digest {
                what,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "{what}: sha256 {actual} does not match catalog {expected}"
                )
            }
            Self::Extract {
                archive,
                member,
                message,
            } => {
                write!(f, "extract {member} from {}: {message}", archive.display())
            }
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Verify(e) => write!(f, "bundle verification: {e}"),
        }
    }
}

impl std::error::Error for ProvisionError {}

impl From<RuntimeError> for ProvisionError {
    fn from(error: RuntimeError) -> Self {
        Self::Verify(error)
    }
}

/// Result of [`provision_runtime`] / [`provision_runtime_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionOutcome {
    /// The bundle as verified by [`verify_runtime_manifest`].
    pub runtime: VerifiedRuntime,
    /// `true` when the destination already held a matching verified bundle and nothing was
    /// downloaded or written.
    pub reused: bool,
}

/// Fetches `url` into `dest` (a file path). The caller verifies the SHA-256 afterwards; `sha256`
/// is given so a fetcher can verify or resume against it.
pub type Fetch<'a> = dyn FnMut(&str, &str, &Path) -> Result<(), String> + 'a;

/// One file to take from an archive into the bundle.
struct Planned {
    record: NativeRecord,
    archive_url: String,
    archive_sha256: String,
    archive_name: String,
    member: String,
    out_name: &'static str,
}

#[derive(Serialize)]
struct ManifestOut<'a> {
    schema: u32,
    platform: &'a str,
    onnxruntime: IdentityOut<'a>,
    files: Vec<FileOut>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityOut<'a> {
    version: &'a str,
    variant: &'a str,
    catalog_row: &'a str,
}

#[derive(Serialize)]
struct FileOut {
    path: String,
    sha256: String,
    bytes: u64,
}

/// Provision with the embedded suite catalog and the vetted HTTP downloader.
///
/// `dest_dir` becomes the bundle directory (it holds `manifest.json`); `cache_dir` keeps the
/// verified archives between runs (a build script uses a directory under `OUT_DIR`).
pub fn provision_runtime(
    target: ProvisionTarget,
    dest_dir: &Path,
    cache_dir: &Path,
) -> Result<ProvisionOutcome, ProvisionError> {
    let catalog = NativeCatalog::embedded().map_err(catalog_error)?;
    provision_runtime_with(&catalog, target, dest_dir, cache_dir, &mut http_fetch)
}

/// Load a native-library catalog JSON from disk (schema 1, the `native-libraries.json` shape).
pub fn load_catalog(path: &Path) -> Result<NativeCatalog, ProvisionError> {
    let text = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    let catalog = NativeCatalog::parse_native_only(&text).map_err(catalog_error)?;
    catalog.validate().map_err(catalog_error)?;
    Ok(catalog)
}

/// Provision from an explicit catalog and fetcher (tests inject a local fetcher).
///
/// Steps: pick one `approved` or `pinned` row per needed library, return early when `dest_dir`
/// already holds a matching verified bundle, fetch and SHA-256-check each archive into
/// `cache_dir`, extract only the runtime files into a sibling staging directory, check each
/// against the catalog file digest, write `manifest.json`, verify the staged bundle, then swap it
/// into `dest_dir`.
pub fn provision_runtime_with(
    catalog: &NativeCatalog,
    target: ProvisionTarget,
    dest_dir: &Path,
    cache_dir: &Path,
    fetch: &mut Fetch<'_>,
) -> Result<ProvisionOutcome, ProvisionError> {
    let dest = std::path::absolute(dest_dir).map_err(|e| io_error(dest_dir, e))?;
    let cache = std::path::absolute(cache_dir).map_err(|e| io_error(cache_dir, e))?;
    let plan = plan(catalog, target)?;

    if let Some(runtime) = existing_matches(&dest, target, &plan) {
        return Ok(ProvisionOutcome {
            runtime,
            reused: true,
        });
    }

    fs::create_dir_all(&cache).map_err(|e| io_error(&cache, e))?;
    let parent = dest
        .parent()
        .ok_or_else(|| io_error(&dest, "destination has no parent directory"))?;
    fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io_error(&dest, "destination has no UTF-8 directory name"))?
        .to_owned();
    let stage = parent.join(format!(".{name}.provision-{}", std::process::id()));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).map_err(|e| io_error(&stage, e))?;

    let built = build_stage(&plan, target, &cache, &stage, fetch);
    if let Err(error) = built {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    let swapped = swap_into_place(&stage, &dest, parent, &name);
    if swapped.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    swapped?;
    let runtime = verify_runtime_manifest(&dest)?;
    Ok(ProvisionOutcome {
        runtime,
        reused: false,
    })
}

fn build_stage(
    plan: &[Planned],
    target: ProvisionTarget,
    cache: &Path,
    stage: &Path,
    fetch: &mut Fetch<'_>,
) -> Result<(), ProvisionError> {
    let mut files = Vec::with_capacity(plan.len());
    for item in plan {
        let archive = fetch_verified(item, cache, fetch)?;
        let out = stage.join(item.out_name);
        rightkit_models::archive::extract_member(&archive, &item.member, &out).map_err(|e| {
            ProvisionError::Extract {
                archive: archive.clone(),
                member: item.member.clone(),
                message: e.message,
            }
        })?;
        item.record.verify_file(&out).map_err(|e| match e {
            NativeCatalogError::DigestMismatch {
                expected, actual, ..
            } => ProvisionError::Digest {
                what: format!("{} ({})", item.out_name, item.record.id),
                expected,
                actual,
            },
            other => catalog_error(other),
        })?;
        let (bytes, sha256) = hash_file(&out)?;
        files.push(FileOut {
            path: item.out_name.to_owned(),
            sha256,
            bytes,
        });
    }

    let onnx = &plan[0].record;
    let manifest = ManifestOut {
        schema: 1,
        platform: target.platform(),
        onnxruntime: IdentityOut {
            version: &onnx.version,
            variant: target.variant(),
            catalog_row: &onnx.id,
        },
        files,
    };
    let text = serde_json::to_vec_pretty(&manifest).map_err(|e| io_error(stage, e))?;
    let manifest_path = stage.join("manifest.json");
    fs::write(&manifest_path, text).map_err(|e| io_error(&manifest_path, e))?;
    verify_runtime_manifest(stage)?;
    Ok(())
}

/// Return the cached archive for `item`, fetching it when absent or when the cached copy fails
/// its digest. The archive digest is checked here, before any extraction.
fn fetch_verified(
    item: &Planned,
    cache: &Path,
    fetch: &mut Fetch<'_>,
) -> Result<PathBuf, ProvisionError> {
    let path = cache.join(&item.archive_name);
    if path.is_file() {
        match hash_file(&path) {
            Ok((_, actual)) if actual.eq_ignore_ascii_case(&item.archive_sha256) => {
                return Ok(path)
            }
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    fetch(&item.archive_url, &item.archive_sha256, &path).map_err(|message| {
        ProvisionError::Fetch {
            url: item.archive_url.clone(),
            message,
        }
    })?;
    let (_, actual) = hash_file(&path)?;
    if !actual.eq_ignore_ascii_case(&item.archive_sha256) {
        let _ = fs::remove_file(&path);
        return Err(ProvisionError::Digest {
            what: format!("archive {}", item.archive_url),
            expected: item.archive_sha256.clone(),
            actual,
        });
    }
    Ok(path)
}

/// Replace `dest` with `stage`. The previous bundle moves aside first and returns if the second
/// rename fails, so a failure leaves the previous bundle in place.
fn swap_into_place(
    stage: &Path,
    dest: &Path,
    parent: &Path,
    name: &str,
) -> Result<(), ProvisionError> {
    let backup = parent.join(format!(".{name}.previous-{}", std::process::id()));
    let _ = fs::remove_dir_all(&backup);
    let had_previous = dest.exists();
    if had_previous {
        fs::rename(dest, &backup).map_err(|e| io_error(dest, e))?;
    }
    if let Err(error) = fs::rename(stage, dest) {
        if had_previous {
            let _ = fs::rename(&backup, dest);
        }
        return Err(io_error(dest, error));
    }
    if had_previous {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// A verified bundle already in `dest` that matches the plan exactly (same platform, catalog row,
/// version, variant, listed files and file digests), or `None`.
fn existing_matches(
    dest: &Path,
    target: ProvisionTarget,
    plan: &[Planned],
) -> Option<VerifiedRuntime> {
    if !dest.join("manifest.json").is_file() {
        return None;
    }
    let runtime = verify_runtime_manifest(dest).ok()?;
    let onnx = &plan[0].record;
    let identity = &runtime.onnxruntime;
    let same_identity = runtime.platform == target.platform()
        && identity.catalog_row == onnx.id
        && identity.version == onnx.version
        && identity.variant == target.variant();
    let same_files = runtime.files.len() == plan.len()
        && plan.iter().all(|item| {
            runtime.files.iter().any(|file| {
                file.path == item.out_name
                    && item.record.sha256.as_deref().is_some_and(|expected| {
                        item.record.content_sha256.is_some()
                            || expected.eq_ignore_ascii_case(&file.sha256)
                    })
            })
        });
    (same_identity && same_files).then_some(runtime)
}

fn plan(catalog: &NativeCatalog, target: ProvisionTarget) -> Result<Vec<Planned>, ProvisionError> {
    catalog.validate().map_err(catalog_error)?;
    let onnx = select(catalog, NativeLibrary::Onnxruntime, "onnxruntime", target)?;
    let onnx_url = archive_of(&onnx, "onnxruntime")?;
    let mut plan = Vec::new();
    match target {
        ProvisionTarget::WindowsX64 => {
            expect_file_name(&onnx, "onnxruntime", "onnxruntime.dll")?;
            plan.push(planned(
                onnx,
                onnx_url,
                "runtimes/win-x64/native/onnxruntime.dll".to_owned(),
                "onnxruntime.dll",
            )?);
            let directml = select(catalog, NativeLibrary::Directml, "directml", target)?;
            let url = archive_of(&directml, "directml")?;
            expect_file_name(&directml, "directml", "DirectML.dll")?;
            plan.push(planned(
                directml,
                url,
                "bin/x64-win/DirectML.dll".to_owned(),
                "DirectML.dll",
            )?);
        }
        ProvisionTarget::MacosArm64 => {
            let file = onnx.artifact_file_name.clone().unwrap_or_default();
            if !(file.starts_with("libonnxruntime") && file.ends_with(".dylib")) {
                return Err(row(
                    "onnxruntime",
                    format!("artifactFileName {file:?} is not a libonnxruntime dylib"),
                ));
            }
            // The upstream tarball unpacks under a directory named after the archive file.
            let archive_name = file_name_of(&onnx_url.0);
            let wrapper = archive_name
                .strip_suffix(".tgz")
                .or_else(|| archive_name.strip_suffix(".tar.gz"))
                .ok_or_else(|| {
                    row(
                        "onnxruntime",
                        format!("archive {archive_name:?} is not a .tgz"),
                    )
                })?
                .to_owned();
            let member = format!("{wrapper}/lib/{file}");
            plan.push(planned(onnx, onnx_url, member, "libonnxruntime.dylib")?);
        }
        ProvisionTarget::AndroidArm64 => {
            expect_file_name(&onnx, "onnxruntime", "libonnxruntime.so")?;
            let archive_name = file_name_of(&onnx_url.0);
            if !archive_name.ends_with(".aar") {
                return Err(row(
                    "onnxruntime",
                    format!("archive {archive_name:?} is not an .aar"),
                ));
            }
            plan.push(planned(
                onnx,
                onnx_url,
                ANDROID_ARM64_AAR_MEMBER.to_owned(),
                "libonnxruntime.so",
            )?);
        }
    }
    Ok(plan)
}

fn planned(
    record: NativeRecord,
    archive: (String, String),
    member: String,
    out_name: &'static str,
) -> Result<Planned, ProvisionError> {
    let archive_name = file_name_of(&archive.0);
    Ok(Planned {
        record,
        archive_url: archive.0,
        archive_sha256: archive.1,
        archive_name,
        member,
        out_name,
    })
}

/// Exactly one `approved` or `pinned` record for the library on the target.
fn select(
    catalog: &NativeCatalog,
    library: NativeLibrary,
    label: &'static str,
    target: ProvisionTarget,
) -> Result<NativeRecord, ProvisionError> {
    let (os, arch) = target.os_arch();
    let mut hits = catalog.records(library).filter(|r| {
        matches!(r.status, NativeStatus::Approved | NativeStatus::Pinned)
            && r.os.eq_ignore_ascii_case(os)
            && r.arch.eq_ignore_ascii_case(arch)
    });
    let first = hits.next().ok_or_else(|| {
        row(
            label,
            format!("no approved or pinned row for {}", target.platform()),
        )
    })?;
    if hits.next().is_some() {
        return Err(row(
            label,
            format!(
                "more than one approved or pinned row for {}",
                target.platform()
            ),
        ));
    }
    Ok(first.clone())
}

fn archive_of(
    record: &NativeRecord,
    label: &'static str,
) -> Result<(String, String), ProvisionError> {
    if record.sha256.is_none() {
        return Err(row(label, format!("{} has no file sha256", record.id)));
    }
    let archive = record.archive.as_ref().ok_or_else(|| {
        row(
            label,
            format!("{} has no archive url and sha256", record.id),
        )
    })?;
    if !archive.url.starts_with("https://") {
        return Err(row(
            label,
            format!("{} archive url is not https", record.id),
        ));
    }
    Ok((archive.url.clone(), archive.sha256.to_ascii_lowercase()))
}

fn expect_file_name(
    record: &NativeRecord,
    label: &'static str,
    expected: &str,
) -> Result<(), ProvisionError> {
    match record.artifact_file_name.as_deref() {
        Some(name) if name == expected => Ok(()),
        other => Err(row(
            label,
            format!(
                "{} artifactFileName {other:?}, expected {expected:?}",
                record.id
            ),
        )),
    }
}

fn file_name_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

fn row(library: &'static str, reason: String) -> ProvisionError {
    ProvisionError::Row { library, reason }
}

fn catalog_error(error: NativeCatalogError) -> ProvisionError {
    ProvisionError::Catalog(error.to_string())
}

fn io_error(path: &Path, error: impl fmt::Display) -> ProvisionError {
    ProvisionError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn hash_file(path: &Path) -> Result<(u64, String), ProvisionError> {
    let mut file = fs::File::open(path).map_err(|e| io_error(path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|e| io_error(path, e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((total, digest))
}

/// Default fetcher: the vetted `rightkit-models` resumable HTTPS download, pinned to `sha256`.
pub fn http_fetch(url: &str, sha256: &str, dest: &Path) -> Result<(), String> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let client = rightkit_models::http::http_client();
    rightkit_models::download::download_resumable(
        &client,
        rightkit_models::download::DownloadRequest {
            url,
            part_path: &part,
            dest_path: dest,
            expected_sha256: Some(sha256),
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod android_tests {
    use super::*;

    #[test]
    fn android_arm64_target_parses_and_names_its_platform() {
        let target = ProvisionTarget::parse("android-arm64").unwrap();
        assert_eq!(target, ProvisionTarget::AndroidArm64);
        assert_eq!(target.platform(), "android-arm64");
        assert_eq!(target.os_arch(), ("android", "aarch64"));
        assert_eq!(target.variant(), "cpu");
        assert_eq!(ProvisionTarget::parse("android-x64"), None);
        // The manifest platform string is accepted by the bundle verifier's platform rule.
        assert!(crate::runtime_bundle::valid_platform(target.platform()));
    }
}
