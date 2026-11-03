//! The media library: owned copies of produced media (§4.4). Files are
//! written fully to a temp file, fsynced, then renamed into place.

use std::path::{Path, PathBuf};

pub use rightkit_fs::digest::sha256_bytes as sha256_hex;
pub(crate) use rightkit_fs::digest::sha256_file_with_len;
use rightkit_fs::digest::Sha256Hasher;

use crate::store::io_err;
use crate::{Context, GenResult};

#[derive(Debug)]
pub struct Stored {
    pub asset_id: String,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

pub fn ext_for_mime(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/mp4" | "audio/aac" => "m4a",
        "application/json" => "json",
        _ => "bin",
    }
}

/// Adopt a finalized worker-owned file at a deterministic, immutable asset
/// location. A receipt is durable before same-volume rename; cross-volume
/// adoption uses a streamed atomic copy.
pub fn adopt_path_asset(
    ctx: &Context,
    job_id: &str,
    output_index: usize,
    source: &Path,
    mime: &str,
    expected_sha256: &str,
    expected_bytes: u64,
) -> GenResult<Stored> {
    if job_id.is_empty()
        || !job_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(crate::GenError::new(
            "invalid_output",
            "invalid worker job id",
        ));
    }
    let mut identity = Sha256Hasher::new();
    identity.update(job_id.as_bytes());
    identity.update(&[0]);
    identity.update(&(output_index as u64).to_le_bytes());
    identity.update(&[0]);
    identity.update(expected_sha256.as_bytes());
    let raw_id = identity.finalize();
    let asset_id = format!(
        "{}-{}-{}-{}-{}",
        &raw_id[0..8],
        &raw_id[8..12],
        &raw_id[12..16],
        &raw_id[16..20],
        &raw_id[20..32]
    );
    let dir = ctx.library_dir().join(&asset_id);
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    let final_path = dir.join(format!("{asset_id}.{}", ext_for_mime(mime)));
    let receipt_path = dir.join("adoption.json");

    if final_path.exists() {
        let receipt: serde_json::Value =
            std::fs::read(&receipt_path)
                .map_err(io_err)
                .and_then(|data| {
                    serde_json::from_slice(&data).map_err(|e| {
                        crate::GenError::new(
                            "invalid_output",
                            format!("invalid adoption receipt: {e}"),
                        )
                    })
                })?;
        if receipt["job_id"] != job_id
            || receipt["output_index"] != output_index
            || receipt["sha256"] != expected_sha256
            || receipt["bytes"] != expected_bytes
            || receipt["mime"] != mime
        {
            return Err(crate::GenError::new(
                "invalid_output",
                "adoption receipt does not match worker artifact",
            ));
        }
        let (sha256, bytes) = sha256_file_with_len(&final_path).map_err(io_err)?;
        if sha256 != expected_sha256 || bytes != expected_bytes {
            return Err(crate::GenError::new(
                "invalid_output",
                "existing adopted artifact does not match worker manifest",
            ));
        }
        return Ok(Stored {
            asset_id,
            path: final_path,
            sha256,
            bytes,
        });
    }

    // A worker's artifact must sit in its own job folder: the Blender worker's under the data dir,
    // or Wan2GP's under its jobs root (`RIGHTKIT_WAN2GP_JOBS`, else the organisation root).
    let mut owned_roots = vec![ctx.data_dir.join("blender-jobs").join(job_id)];
    if let Some(jobs) = crate::providers::local_wan2gp::jobs_dir() {
        owned_roots.push(jobs.join(job_id));
    }
    let canonical_roots: Vec<PathBuf> = owned_roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .collect();
    if canonical_roots.is_empty() {
        return Err(io_err(owned_roots[0].canonicalize().unwrap_err()));
    }
    let meta = std::fs::symlink_metadata(source).map_err(io_err)?;
    if !meta.file_type().is_file() {
        return Err(crate::GenError::new(
            "invalid_output",
            "worker artifact must be a regular file",
        ));
    }
    let canonical_source = source.canonicalize().map_err(io_err)?;
    if !canonical_roots
        .iter()
        .any(|root| canonical_source.starts_with(root))
    {
        return Err(crate::GenError::new(
            "invalid_output",
            "worker artifact is outside its owned job directory",
        ));
    }
    let (source_sha256, source_bytes) = sha256_file_with_len(&canonical_source).map_err(io_err)?;
    if source_sha256 != expected_sha256 || source_bytes != expected_bytes {
        return Err(crate::GenError::new(
            "invalid_output",
            "worker artifact hash or size does not match result manifest",
        ));
    }
    let receipt = serde_json::to_vec(&serde_json::json!({
        "job_id": job_id,
        "output_index": output_index,
        "source": canonical_source.to_string_lossy(),
        "destination": final_path.to_string_lossy(),
        "mime": mime,
        "sha256": expected_sha256,
        "bytes": expected_bytes,
    }))
    .map_err(|e| {
        crate::GenError::new(
            "invalid_output",
            format!("could not encode adoption receipt: {e}"),
        )
    })?;
    rightkit_fs::atomic_write(&receipt_path, &receipt)
        .map_err(|e| io_err(std::io::Error::other(e)))?;

    // Same-volume adoption avoids copying multi-GB movie outputs. Receipt is
    // durable first, so a restart can find the destination if the rename won.
    let no_replace = rightkit_fs::AtomicWriteOptions {
        replace: rightkit_fs::ReplaceMode::NoReplace,
        ..Default::default()
    };
    match rightkit_fs::publish_file_with(&canonical_source, &final_path, no_replace) {
        Ok(()) => {
            return Ok(Stored {
                asset_id,
                path: final_path,
                sha256: source_sha256,
                bytes: source_bytes,
            });
        }
        Err(rightkit_fs::FsError::AlreadyExists(_)) => {
            let (sha256, bytes) = sha256_file_with_len(&final_path).map_err(io_err)?;
            if sha256 == expected_sha256 && bytes == expected_bytes {
                return Ok(Stored {
                    asset_id,
                    path: final_path,
                    sha256,
                    bytes,
                });
            }
            return Err(io_err(std::io::Error::from(
                std::io::ErrorKind::AlreadyExists,
            )));
        }
        Err(rightkit_fs::FsError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(e) => return Err(io_err(std::io::Error::other(e))),
    }

    let copied = rightkit_fs::copy_verified(
        &canonical_source,
        &final_path,
        rightkit_fs::atomic::AtomicWriteOptions::default(),
        Some(expected_sha256),
    )
    .map_err(|e| match e {
        rightkit_fs::FsError::DigestMismatch { .. } => crate::GenError::new(
            "invalid_output",
            "worker artifact hash or size does not match result manifest",
        ),
        other => io_err(std::io::Error::other(other)),
    })?;
    if copied.bytes != expected_bytes {
        let _ = std::fs::remove_file(&final_path);
        return Err(crate::GenError::new(
            "invalid_output",
            "worker artifact hash or size does not match result manifest",
        ));
    }
    Ok(Stored {
        asset_id,
        path: final_path,
        sha256: copied.sha256,
        bytes: copied.bytes,
    })
}

/// Write `data` as a new asset: `<library>/<asset_id>/<asset_id>.<ext>`.
pub fn store_asset(ctx: &Context, data: &[u8], mime: &str) -> GenResult<Stored> {
    let asset_id = uuid::Uuid::new_v4().to_string();
    let dir = ctx.library_dir().join(&asset_id);
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    let final_path = dir.join(format!("{asset_id}.{}", ext_for_mime(mime)));
    rightkit_fs::atomic_write(&final_path, data).map_err(|e| io_err(std::io::Error::other(e)))?;
    Ok(Stored {
        asset_id,
        path: final_path,
        sha256: sha256_hex(data),
        bytes: data.len() as u64,
    })
}

/// What a caller (another app, an agent session) produced, as the library records it.
pub struct CallerAsset<'a> {
    pub caller: &'a crate::host::Caller,
    pub provider: &'a str,
    pub model: &'a str,
    pub job_id: &'a str,
    pub prompt: &'a str,
    pub aspect: &'a str,
    pub cost_usd: f64,
}

/// Deterministic id of a caller-registered asset: the same bytes registered by the same caller map to
/// the same folder (a re-run adds nothing), another caller's identical bytes get their own entry.
pub fn caller_asset_id(caller_slug: &str, sha256: &str) -> String {
    let mut identity = Sha256Hasher::new();
    identity.update(b"caller-registered");
    identity.update(&[0]);
    identity.update(caller_slug.as_bytes());
    identity.update(&[0]);
    identity.update(sha256.as_bytes());
    let raw = identity.finalize();
    format!(
        "{}-{}-{}-{}-{}",
        &raw[0..8],
        &raw[8..12],
        &raw[12..16],
        &raw[16..20],
        &raw[20..32]
    )
}

/// Register `data` (already hash-verified by the caller as `sha256`) in the library of `ctx` under
/// `asset.caller`: `<library>/<id>/<id>.<ext>` plus `<file>.caller.json`, `<file>.sidecar.json`
/// (generation metadata the Gallery reads) and the folder `facts.json`. Sidecars are written first and
/// the media last, so a listed file is never untagged; an existing identical entry is kept as is.
/// Never removes or overwrites a differing file: a folder holding other bytes is `invalid_output`.
pub fn register_caller_asset(
    ctx: &Context,
    data: &[u8],
    mime: &str,
    asset: &CallerAsset,
) -> GenResult<Stored> {
    let sha256 = sha256_hex(data);
    let asset_id = caller_asset_id(&asset.caller.slug, &sha256);
    let dir = ctx.library_dir().join(&asset_id);
    let file_name = format!("{asset_id}.{}", ext_for_mime(mime));
    let final_path = dir.join(&file_name);
    if final_path.exists() {
        let (existing, _) = sha256_file_with_len(&final_path).map_err(io_err)?;
        if existing != sha256 {
            return Err(crate::GenError::new(
                "invalid_output",
                "an existing library entry for this caller holds different bytes",
            ));
        }
    }
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    let write = |path: PathBuf, body: Vec<u8>| {
        if path.exists() {
            return Ok(());
        }
        rightkit_fs::atomic_write(&path, &body).map_err(|e| io_err(std::io::Error::other(e)))
    };
    write(
        dir.join(format!("{file_name}.caller.json")),
        serde_json::to_vec(&serde_json::json!({
            "caller": asset.caller.slug,
            "label": asset.caller.label,
            "registered_at": crate::store::now(),
        }))
        .unwrap_or_default(),
    )?;
    write(
        dir.join(format!("{file_name}.sidecar.json")),
        serde_json::to_vec(&serde_json::json!({
            "provider": asset.provider,
            "model": asset.model,
            "prompt": asset.prompt,
            "aspect": asset.aspect,
            "cost_usd": asset.cost_usd,
        }))
        .unwrap_or_default(),
    )?;
    if !dir.join("facts.json").exists() {
        let facts = crate::host::asset_facts::AssetFacts::generated(
            asset.provider,
            asset.model,
            asset.job_id,
            Vec::new(),
            &[],
            crate::host::asset_facts::EvidenceKind::Illustrative,
        );
        crate::provenance::write_facts_file(&dir, &facts, &serde_json::Value::Null, None)?;
    }
    write(final_path.clone(), data.to_vec())?;
    Ok(Stored {
        asset_id,
        path: final_path,
        sha256,
        bytes: data.len() as u64,
    })
}

/// Tag a file that already lives in the library or gallery with its caller: `<file>.caller.json`
/// beside it. An existing tag by the same caller is kept; one by another caller is `invalid_output`
/// (a file has one origin), never rewritten.
pub fn tag_in_place(file: &Path, caller: &crate::host::Caller) -> GenResult<PathBuf> {
    let mut name = file
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(crate::host::CALLER_SIDECAR_SUFFIX);
    let path = file.with_file_name(name);
    if let Ok(bytes) = std::fs::read(&path) {
        let existing: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        return if existing["caller"] == caller.slug.as_str() {
            Ok(path)
        } else {
            Err(crate::GenError::new(
                "invalid_output",
                format!("{} is already tagged for another caller", file.display()),
            ))
        };
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "caller": caller.slug,
        "label": caller.label,
        "registered_at": crate::store::now(),
    }))
    .unwrap_or_default();
    rightkit_fs::atomic_write(&path, &body).map_err(|e| io_err(std::io::Error::other(e)))?;
    Ok(path)
}

/// How a finalized local file was duplicated into the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// Copy-on-write clone (macOS APFS `clonefile`): own inode, shared blocks.
    Clone,
    /// Hard link: same inode, same volume only.
    HardLink,
    /// Full byte copy: different volumes, or clone and link both failed.
    Copy,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::HardLink => "hardlink",
            Self::Copy => "copy",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "clone" => Some(Self::Clone),
            "hardlink" => Some(Self::HardLink),
            "copy" => Some(Self::Copy),
            _ => None,
        }
    }
}

/// Marker file in a library asset folder tying it to its finalized gallery file.
const PAIR_FILE: &str = "pair.json";

/// Content root of local-image outputs (`<content>/<content subdir>/local-images`): the only
/// place a paired gallery file may live and be deleted from.
pub fn local_images_root() -> PathBuf {
    crate::providers::local_image::engine_paths().output_root
}

fn plain_asset_id(asset_id: &str) -> bool {
    !asset_id.is_empty() && !asset_id.contains(['/', '\\', '.'])
}

/// `<root>/<conversation>/<job>/<file>` split into its three plain components.
fn gallery_parts(
    canonical_root: &Path,
    canonical_parent: &Path,
    file_name: &std::ffi::OsStr,
) -> Option<[String; 3]> {
    let rel = canonical_parent.strip_prefix(canonical_root).ok()?;
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    parts.push(file_name.to_string_lossy().into_owned());
    let [conversation, job, file]: [String; 3] = parts.try_into().ok()?;
    if [&conversation, &job, &file]
        .iter()
        .any(|p| p.is_empty() || p.starts_with('.'))
    {
        return None;
    }
    Some([conversation, job, file])
}

/// `path` with its deepest existing ancestor canonicalized and the not-yet-existing tail appended as written. `None`
/// for a tail that holds `.`/`..` or anything that is not a plain name, or any failure other than "not found".
fn canonical_lenient(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut head = path.to_path_buf();
    loop {
        match head.canonicalize() {
            Ok(real) => {
                let mut out = real;
                out.extend(tail.iter().rev());
                return Some(out);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tail.push(head.file_name()?.to_os_string());
                if !head.pop() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

/// True when `file` is a gallery output of `root` (`<root>/<conversation>/<job>/<file>`), judged the way
/// [`remove_gallery_item`] judges it (the parent folder canonicalized) but also when that folder is already gone, so a
/// repeat after a crash still finds its rows. A recorded path outside the root is never one: its history rows are not dropped.
fn is_gallery_output(root: &Path, file: &Path) -> bool {
    let (Ok(canonical_root), Some(parent), Some(name)) =
        (root.canonicalize(), file.parent(), file.file_name())
    else {
        return false;
    };
    canonical_lenient(parent)
        .is_some_and(|parent| gallery_parts(&canonical_root, &parent, name).is_some())
}

/// An absolute path as lexical components: on Windows separators normalized to `/`, a verbatim `\\?\` prefix dropped
/// (`\\?\UNC\` becomes `//`) and case folded; on every platform empty components (doubled or trailing separators)
/// collapsed. Off Windows a backslash is an ordinary name character. `None` for a
/// relative path or one holding a `.` or `..` component, which is never trusted lexically.
fn lexical_parts(path: &Path) -> Option<Vec<String>> {
    let mut text = path.to_string_lossy().into_owned();
    // Windows rules only on Windows: there a backslash separates, a verbatim prefix is dropped and case is ignored.
    // Elsewhere a backslash or a verbatim-looking prefix is part of a file name, so rewriting it would let one odd
    // name outside the root read as a nested path under it.
    if cfg!(windows) {
        text = text.replace('\\', "/");
        if let Some(rest) = text.strip_prefix("//?/UNC/") {
            text = format!("//{rest}");
        } else if let Some(rest) = text.strip_prefix("//?/") {
            text = rest.to_string();
        }
        text = text.to_lowercase();
    }
    let mut parts: Vec<String> = Vec::new();
    for (i, component) in text.split('/').enumerate() {
        match component {
            "." | ".." => return None,
            "" if i == 0 => parts.push(String::new()),
            "" => {}
            other => parts.push(other.to_string()),
        }
    }
    // Absolute only: a leading `/` (or `//` for a share) or a drive such as `c:`.
    let absolute = parts
        .first()
        .is_some_and(|first| first.is_empty() || (first.len() == 2 && first.ends_with(':')));
    (absolute && parts.len() >= 2).then_some(parts)
}

/// The fallback for a gallery root that cannot be resolved: `file` is `<root>/<conversation>/<job>/<file>` judged only
/// by its text against the root as written (on Windows separators normalized and case ignored; no `..` anywhere), with the same
/// shape rules as [`gallery_parts`].
fn lexically_gallery_output(root: &Path, file: &Path) -> bool {
    let (Some(root), Some(file)) = (lexical_parts(root), lexical_parts(file)) else {
        return false;
    };
    if file.len() != root.len() + 3 || !file.starts_with(&root) {
        return false;
    }
    file[root.len()..]
        .iter()
        .all(|part| !part.is_empty() && !part.starts_with('.'))
}

/// Whether the history rows of `file` may be dropped with it: it is a gallery output of `root` by the canonical judgment
/// of [`is_gallery_output`], or the gallery root as written no longer exists (moved away, renamed or wiped, so nothing can
/// be canonicalized) and the recorded path is lexically under that root. An existing root never takes the lexical path:
/// a recorded file outside it, or reached through a link out of it, keeps its rows.
fn may_drop_rows(root: &Path, file: &Path) -> bool {
    if is_gallery_output(root, file) {
        return true;
    }
    let root_missing =
        matches!(root.canonicalize(), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    root_missing && lexically_gallery_output(root, file)
}

/// Deterministic library id for a gallery file, so either side can find the other.
fn finalized_asset_id(parts: &[String; 3]) -> String {
    let mut identity = Sha256Hasher::new();
    identity.update(b"local-image-link");
    for part in parts {
        identity.update(&[0]);
        identity.update(part.as_bytes());
    }
    let raw = identity.finalize();
    format!(
        "{}-{}-{}-{}-{}",
        &raw[0..8],
        &raw[8..12],
        &raw[12..16],
        &raw[16..20],
        &raw[20..32]
    )
}

/// The pair marker of a library asset folder. Link-safe: the folder is classified through its parent's `list_entries`
/// (a symlink or junction, dangling or not, reads as `Symlink` and has NO marker here, so a delete never trusts a link
/// target's marker), and only a genuine directory is then opened through the owner (`StateDir::open_shared`, which
/// refuses a link again at open time) before the marker is read.
///
/// `Ok(None)` only when there is genuinely no marker to trust: the parent or the folder or the marker file does not exist,
/// or the folder is a link. Any other list, open or read failure (and a marker that is not JSON) is an `Err`, so a
/// destructive caller stops instead of deleting an asset whose paired gallery file it could not identify.
fn read_pair(dir: &Path) -> GenResult<Option<serde_json::Value>> {
    let failed = |what: &str, e: &dyn std::fmt::Display| {
        crate::GenError::new("io_error", format!("{what} {}: {e}", dir.display()))
    };
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
    else {
        return Err(failed(
            "library folder has no parent or a non-UTF-8 name:",
            &"",
        ));
    };
    let parent_dir = match rightkit_fs::StateDir::open_shared(parent) {
        Ok(d) => d,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(failed("cannot open the library folder of", &e)),
    };
    let entries = match parent_dir.list_entries(usize::MAX) {
        Ok(v) => v,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(failed("cannot list the library folder holding", &e)),
    };
    let kind = entries.into_iter().find(|e| e.name == name).map(|e| e.kind);
    if kind != Some(rightkit_fs::EntryKind::Dir) {
        return Ok(None);
    }
    let folder = match rightkit_fs::StateDir::open_shared(dir) {
        Ok(d) => d,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(failed("cannot open", &e)),
    };
    let bytes = match folder.read_opt(PAIR_FILE, 1 << 20) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(None),
        Err(e) => return Err(failed("cannot read the pair marker of", &e)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| failed("the pair marker is not valid JSON in", &e))
}

/// Store the library copy of a file a local provider already finalized under `root`
/// (`<root>/<conversation>/<job>/<file>`). The file stays in place; the library gets a
/// clone, hard link or (last resort) copy of it plus a pair marker used by deletion.
/// The library bytes are re-hashed after linking and must equal `expected_sha256`.
pub fn store_finalized_asset(
    ctx: &Context,
    root: &Path,
    source: &Path,
    mime: &str,
    expected_sha256: &str,
    expected_bytes: u64,
) -> GenResult<(Stored, LinkKind)> {
    let bad = |m: &str| crate::GenError::new("invalid_output", m);
    let canonical_root = root.canonicalize().map_err(io_err)?;
    let meta = std::fs::symlink_metadata(source).map_err(io_err)?;
    if !meta.file_type().is_file() {
        return Err(bad("finalized artifact must be a regular file"));
    }
    let canonical_source = source.canonicalize().map_err(io_err)?;
    let (Some(parent), Some(name)) = (canonical_source.parent(), canonical_source.file_name())
    else {
        return Err(bad("finalized artifact has no parent folder"));
    };
    let Some(parts) = gallery_parts(&canonical_root, parent, name) else {
        return Err(bad(
            "finalized artifact is not at <conversation>/<job>/<file> inside its content root",
        ));
    };
    let asset_id = finalized_asset_id(&parts);
    let dir = ctx.library_dir().join(&asset_id);
    let final_path = dir.join(format!("{asset_id}.{}", ext_for_mime(mime)));
    let source_str = canonical_source.to_string_lossy().into_owned();

    if final_path.exists() {
        // Retried build after a crash: accept only the identical pair.
        let pair =
            read_pair(&dir)?.ok_or_else(|| bad("existing library copy has no pair marker"))?;
        if pair["source"] != source_str.as_str() {
            return Err(bad(
                "existing library copy belongs to a different gallery file",
            ));
        }
        let (sha256, bytes) = sha256_file_with_len(&final_path).map_err(io_err)?;
        if sha256 != expected_sha256 || bytes != expected_bytes {
            return Err(bad(
                "existing library copy does not match the finalized artifact",
            ));
        }
        let kind = pair["link"]
            .as_str()
            .and_then(LinkKind::parse)
            .unwrap_or(LinkKind::Copy);
        return Ok((
            Stored {
                asset_id,
                path: final_path,
                sha256,
                bytes,
            },
            kind,
        ));
    }

    std::fs::create_dir_all(&dir).map_err(io_err)?;
    let write_pair = |link: Option<LinkKind>| {
        let body = serde_json::json!({
            "type": "local-image-pair",
            "source": source_str,
            "relative": parts.join("/"),
            "sha256": expected_sha256,
            "bytes": expected_bytes,
            "link": link.map(LinkKind::as_str),
        });
        rightkit_fs::atomic_write(
            &dir.join(PAIR_FILE),
            &serde_json::to_vec(&body).unwrap_or_default(),
        )
        .map_err(|e| io_err(std::io::Error::other(e)))
    };
    // Marker first: if we die after linking, deletion still finds the counterpart.
    write_pair(None)?;
    // RightKit's ladder: clone, then hard link, then atomic copy. It refuses symlinks at either end,
    // never overwrites `final_path`, and leaves nothing at `final_path` on failure.
    let kind = match rightkit_fs::clone_or_link_or_copy_nofollow(&canonical_source, &final_path) {
        Ok(rightkit_fs::CloneOutcome::Cloned) => LinkKind::Clone,
        Ok(rightkit_fs::CloneOutcome::HardLinked) => LinkKind::HardLink,
        Ok(rightkit_fs::CloneOutcome::Copied) => LinkKind::Copy,
        Err(e) => {
            let _ = remove_tree(&dir);
            return Err(io_err(std::io::Error::other(e)));
        }
    };
    let verified = sha256_file_with_len(&final_path);
    match verified {
        Ok((sha256, bytes)) if sha256 == expected_sha256 && bytes == expected_bytes => {
            write_pair(Some(kind))?;
            Ok((
                Stored {
                    asset_id,
                    path: final_path,
                    sha256,
                    bytes,
                },
                kind,
            ))
        }
        other => {
            // Never leave a link to a file that changed under us, and never delete the source.
            let _ = remove_tree(&dir);
            match other {
                Err(e) => Err(io_err(e)),
                Ok(_) => Err(bad(
                    "finalized artifact hash or size does not match the result",
                )),
            }
        }
    }
}

/// What a delete actually removed (both false when everything was already gone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub library: bool,
    pub gallery: bool,
}

fn io_failure(what: &str, path: &Path, e: &dyn std::fmt::Display) -> crate::GenError {
    crate::GenError::new("io_error", format!("{what} {}: {e}", path.display()))
}

/// One name removed through the owner. Test builds can inject a failure for a name (see `test_inject`).
fn remove_entry(dir: &rightkit_fs::StateDir, name: &str) -> rightkit_fs::Result<bool> {
    dir.remove(name)
}

/// Permanent delete of one file through the owner's pinned `StateDir` (rightkit-fs 0.4.6): the name is removed relative
/// to its pinned parent, no link is followed. The delete stays permanent (no trash). `Ok(false)` only when the file or
/// its folder is already gone; any other open or remove failure is an `Err` so a caller never treats it as "gone".
fn remove_if_exists(path: &Path) -> GenResult<bool> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Err(io_failure(
            "path has no parent or a non-UTF-8 name:",
            path,
            &"",
        ));
    };
    let dir = match rightkit_fs::StateDir::open_shared(parent) {
        Ok(d) => d,
        Err(e) if e.is_not_found() => return Ok(false),
        Err(e) => return Err(io_failure("cannot open the folder of", path, &e)),
    };
    match remove_entry(&dir, name) {
        Ok(removed) => Ok(removed),
        Err(e) if e.is_not_found() => Ok(false),
        Err(e) => Err(io_failure("cannot remove", path, &e)),
    }
}

/// Permanent delete of a whole folder through `StateDir` (entries first, the folder last). A link is removed itself
/// (`remove_link`), never followed, including when `dir` itself is a link. Every child handle is dropped BEFORE its
/// parent entry is removed (Windows default sharing has no `FILE_SHARE_DELETE`). `Ok(true)` only when the entry is gone
/// by this call, `Ok(false)` when it was missing or is not a folder; any other failure is an `Err`.
/// The pair marker of the top folder is removed LAST (after every other entry, before the folder itself), so a failure
/// part way leaves the marker and a repeat call still finds the paired gallery file.
/// Owner gap (no recursive remove under `StateDir`): tracked upstream.
fn remove_tree(dir: &Path) -> GenResult<bool> {
    fn empty(d: &rightkit_fs::StateDir, last: Option<&str>) -> rightkit_fs::Result<()> {
        let mut held_back = false;
        for entry in d.list_entries(usize::MAX)? {
            let is_folder_or_link = matches!(
                entry.kind,
                rightkit_fs::EntryKind::Dir | rightkit_fs::EntryKind::Symlink
            );
            if last == Some(entry.name.as_str()) && !is_folder_or_link {
                held_back = true;
                continue;
            }
            match entry.kind {
                rightkit_fs::EntryKind::Dir => {
                    {
                        let child = d.child(&entry.name)?;
                        empty(&child, None)?;
                    } // child handle closed here, before the directory entry is removed
                    d.remove_dir(&entry.name)?;
                }
                rightkit_fs::EntryKind::Symlink => {
                    d.remove_link(&entry.name)?;
                }
                _ => {
                    remove_entry(d, &entry.name)?;
                }
            }
        }
        if let (Some(name), true) = (last, held_back) {
            remove_entry(d, name)?;
        }
        Ok(())
    }
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
    else {
        return Err(io_failure(
            "folder has no parent or a non-UTF-8 name:",
            dir,
            &"",
        ));
    };
    let parent_dir = match rightkit_fs::StateDir::open_shared(parent) {
        Ok(d) => d,
        Err(e) if e.is_not_found() => return Ok(false),
        Err(e) => return Err(io_failure("cannot open the parent of", dir, &e)),
    };
    // Classify the top entry through the parent's `list_entries`, which reports a symlink or junction (a reparse point
    // on Windows, dangling or not) as `EntryKind::Symlink`; `StateDir::stat` opens without reparse traversal and ERRORS
    // on a link on Windows, so it cannot be used for this. The link is never opened or followed.
    let kind = match parent_dir.list_entries(usize::MAX) {
        Ok(entries) => entries.into_iter().find(|e| e.name == name).map(|e| e.kind),
        Err(e) if e.is_not_found() => return Ok(false),
        Err(e) => return Err(io_failure("cannot list the parent of", dir, &e)),
    };
    match kind {
        Some(rightkit_fs::EntryKind::Symlink) => parent_dir
            .remove_link(name)
            .map_err(|e| io_failure("cannot remove the link", dir, &e)),
        Some(rightkit_fs::EntryKind::Dir) => {
            {
                let child = match parent_dir.child(name) {
                    Ok(c) => c,
                    Err(e) if e.is_not_found() => return Ok(false),
                    Err(e) => return Err(io_failure("cannot open", dir, &e)),
                };
                empty(&child, Some(PAIR_FILE)).map_err(|e| io_failure("cannot empty", dir, &e))?;
            } // child handle closed before the top folder is removed
            parent_dir
                .remove_dir(name)
                .map_err(|e| io_failure("cannot remove the folder", dir, &e))
        }
        // Missing, or not a folder (a plain file is not an asset folder): nothing to remove here.
        _ => Ok(false),
    }
}

/// Whether the output folder still holds a media file (a file that is not `.json`). A folder that is already gone holds
/// none; any other read failure, of the folder or of one entry (or of an entry's type), is an `Err`, so an unreadable
/// entry is never taken as "no media left" (which would delete the whole folder).
fn media_left(folder: &Path) -> GenResult<bool> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io_failure("cannot read the gallery folder", folder, &e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| io_failure("cannot read the gallery folder", folder, &e))?;
        let kind = entry
            .file_type()
            .map_err(|e| io_failure("cannot read the type of", &entry.path(), &e))?;
        if kind.is_file() && !entry.file_name().to_string_lossy().ends_with(".json") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove one gallery output: the image, its `<file>.*.json` sidecars (`.sidecar`, `.eyes`, `.ai`,
/// `.chat-image`), and `jury/<file>.verdict.json`. When no other media file remains in the job
/// folder the whole folder (including `run-manifest.json`) goes too. Refuses anything that is not
/// `<root>/<conversation>/<job>/<file>` after canonicalising the parent folder, so a recorded path
/// outside `root` is never followed. A missing folder or file is not an error.
fn remove_gallery_item(root: &Path, file: &Path) -> GenResult<bool> {
    // A path that is already gone is not an error; any other failure is, so the caller stops.
    let resolve = |p: &Path, what: &str| match p.canonicalize() {
        Ok(c) => Ok(Some(c)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_failure(what, p, &e)),
    };
    let Some(canonical_root) = resolve(root, "cannot resolve the gallery root")? else {
        return Ok(false);
    };
    let (Some(parent), Some(name)) = (file.parent(), file.file_name()) else {
        return Ok(false);
    };
    let Some(canonical_parent) = resolve(parent, "cannot resolve the gallery folder")? else {
        return Ok(false);
    };
    let Some(parts) = gallery_parts(&canonical_root, &canonical_parent, name) else {
        return Ok(false);
    };
    let file_name = parts[2].clone();
    // Sidecars and the jury verdict first, the image LAST: a failure part way leaves the image, so a history card
    // never points at a missing file; a repeat finishes the rest.
    let mut removed = false;
    let prefix = format!("{file_name}.");
    match std::fs::read_dir(&canonical_parent) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| {
                    io_failure("cannot read the gallery folder", &canonical_parent, &e)
                })?;
                let n = entry.file_name().to_string_lossy().into_owned();
                if n.starts_with(&prefix)
                    && n.ends_with(".json")
                    && entry.file_type().is_ok_and(|t| t.is_file())
                {
                    removed |= remove_if_exists(&entry.path())?;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(io_failure(
                "cannot read the gallery folder",
                &canonical_parent,
                &e,
            ))
        }
    }
    removed |= remove_if_exists(
        &canonical_parent
            .join("jury")
            .join(format!("{file_name}.verdict.json")),
    )?;
    removed |= remove_if_exists(&canonical_parent.join(&file_name))?;
    if !media_left(&canonical_parent)? {
        removed |= remove_tree(&canonical_parent)?;
        // Drop the conversation folder only if it is now empty (remove_dir refuses otherwise).
        if let Some(conversation) = canonical_parent.parent() {
            if conversation != canonical_root {
                if let (Some(grand), Some(name)) = (
                    conversation.parent(),
                    conversation.file_name().and_then(|n| n.to_str()),
                ) {
                    let _ =
                        rightkit_fs::StateDir::open_shared(grand).and_then(|d| d.remove_dir(name));
                }
            }
        }
    }
    Ok(removed)
}

/// Drop the local-image history rows for gallery outputs a delete is about to remove, so a
/// conversation card cannot point at a missing file. Matches the exact recorded path (local-image
/// writes forward slashes). No local-image database means no rows to drop.
fn forget_local_image_outputs(paths: &[String]) -> GenResult<()> {
    let database = crate::providers::local_image::engine_paths().database_path;
    if paths.is_empty() || !database.is_file() {
        return Ok(());
    }
    let store = rightkit_local_gen::store::Store::open(&database)
        .map_err(|e| crate::GenError::new("store_failed", e))?;
    for path in paths {
        store
            .delete_artifacts_by_path(&path.replace('\\', "/"))
            .map_err(|e| crate::GenError::new("store_failed", e))?;
    }
    Ok(())
}

/// Remove an asset's folder, and with it the paired gallery file when the folder carries a pair
/// marker. Gallery side first: the marker (in the library folder) is the tie, so it goes last and a
/// crash in between is finished by a repeat call. Crate-private for `cleanup_acked`; the public
/// entry is [`delete_asset_command`].
pub(crate) fn remove_asset(ctx: &Context, asset_id: &str) -> GenResult<Removed> {
    // Reject anything that is not a plain id to avoid path traversal.
    if !plain_asset_id(asset_id) {
        return Ok(Removed::default());
    }
    // A real read failure stops here, before anything is removed.
    let pair = read_pair(&ctx.library_dir().join(asset_id))?;
    remove_asset_with_pair(
        ctx,
        asset_id,
        pair.as_ref(),
        &local_images_root(),
        &forget_local_image_outputs,
    )
}

/// The gallery side of a delete: the paired gallery output named by the marker, if any. `Ok(false)` when it is already
/// gone; any other failure is an `Err` and the caller stops before touching the history rows or the library folder.
fn remove_gallery_side(pair: Option<&serde_json::Value>, images_root: &Path) -> GenResult<bool> {
    match pair.and_then(|p| p["source"].as_str()) {
        Some(source) => remove_gallery_item(images_root, Path::new(source)),
        None => Ok(false),
    }
}

/// The library side of a delete: the asset folder, with the pair marker removed last inside it. No `exists()` guard: it
/// follows links and would skip a dangling link or junction; `remove_tree` answers false for a missing entry.
fn remove_library_side(ctx: &Context, asset_id: &str) -> GenResult<bool> {
    remove_tree(&ctx.library_dir().join(asset_id))
}

/// [`remove_asset`] with the pair marker already read (it is read once per delete and passed through), the gallery
/// root given, and the row-drop hook injected: the same gallery, rows, library (marker last) sequence as the
/// path delete, in the one function [`remove_then_forget`], so `cleanup_acked` can never leave a card pointing at a
/// removed file.
fn remove_asset_with_pair(
    ctx: &Context,
    asset_id: &str,
    pair: Option<&serde_json::Value>,
    images_root: &Path,
    forget: &dyn Fn(&[String]) -> GenResult<()>,
) -> GenResult<Removed> {
    remove_then_forget(ctx, asset_id, pair, None, images_root, forget)
}

/// One delete in the order that keeps the pair marker the LAST thing to go: the gallery files, then (only when that
/// succeeded) the local-image history rows of the outputs removed, then the library folder with its marker. A failed
/// file removal leaves the rows (a card never points at a file that is still there); a failed row drop, or a crash
/// anywhere before the library folder is gone, leaves the marker, so a repeat call finds the paired path again and
/// finishes (a gallery file that is already gone is not an error). If `forget` itself fails after the files went, the
/// cards name a missing file until the caller repeats the delete (the marker is intact, so the repeat finishes);
/// dropping rows before files would trade that for orphan files.
fn remove_then_forget(
    ctx: &Context,
    asset_id: &str,
    pair: Option<&serde_json::Value>,
    gallery_only: Option<&Path>,
    images_root: &Path,
    forget: &dyn Fn(&[String]) -> GenResult<()>,
) -> GenResult<Removed> {
    if !plain_asset_id(asset_id) {
        return Ok(Removed::default());
    }
    // Rows are dropped only for outputs that really are gallery files of `images_root` (or, when that root is gone,
    // lexically under it as written): a marker whose source lies outside it (a hand-edited or foreign marker) removes
    // nothing there, so it must not drop any rows either.
    let mut gallery_outputs: Vec<String> = pair
        .and_then(|p| p["source"].as_str())
        .filter(|source| may_drop_rows(images_root, Path::new(source)))
        .map(str::to_owned)
        .into_iter()
        .collect();
    let mut gallery = remove_gallery_side(pair, images_root)?;
    if let Some(path) = gallery_only {
        // No library copy (or its marker was already gone): still remove the gallery output, before the rows and the marker.
        if may_drop_rows(images_root, path) {
            gallery_outputs.push(path.to_string_lossy().into_owned());
        }
        gallery |= remove_gallery_item(images_root, path)?;
    }
    forget(&gallery_outputs)?;
    let library = remove_library_side(ctx, asset_id)?;
    Ok(Removed { library, gallery })
}

/// `asset.delete` `{asset_id}` or `{path}`: delete one produced asset through either side.
/// `path` may be the library file or the gallery file of a paired local-image output (or a
/// library file of any other asset). Removes the library folder and the paired gallery output
/// together, and drops the asset from its job result. Succeeds when the other side is already gone.
pub fn delete_asset_command(
    ctx: &Context,
    input: &serde_json::Value,
) -> GenResult<serde_json::Value> {
    let invalid = |m: &str| crate::GenError::new("invalid_request", m);
    let (asset_id, gallery_only): (String, Option<PathBuf>) = if let Some(id) =
        input["asset_id"].as_str()
    {
        (id.to_string(), None)
    } else if let Some(p) = input["path"].as_str() {
        let path = PathBuf::from(p);
        if !path.is_absolute() {
            return Err(invalid("path must be absolute"));
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(invalid("path has no parent folder"));
        };
        let canonical_parent = parent.canonicalize().ok();
        let library_root = ctx.library_dir().canonicalize().ok();
        let images_root = local_images_root().canonicalize().ok();
        match (&canonical_parent, &library_root, &images_root) {
            (Some(par), Some(lib), _) if par.parent() == Some(lib.as_path()) => {
                let id = par
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (id, None)
            }
            (Some(par), _, Some(img)) => match gallery_parts(img, par, name) {
                Some(parts) => (finalized_asset_id(&parts), Some(path)),
                None => return Err(invalid("path is not a library or local-image gallery file")),
            },
            // Parent folder already gone (a repeat delete): resolve lexically, remove nothing new.
            (None, _, _) => {
                // A library path `<library>/<asset_id>/<file>` whose folder a late crash already removed.
                let lib_raw = ctx.library_dir();
                let lib_canonical = lib_raw.canonicalize().ok();
                let lib_rel = path.strip_prefix(&lib_raw).ok().or_else(|| {
                    lib_canonical
                        .as_deref()
                        .and_then(|c| path.strip_prefix(c).ok())
                });
                let lib_parts: Option<Vec<String>> = lib_rel.and_then(|r| {
                    r.components()
                        .map(|c| match c {
                            std::path::Component::Normal(n) => {
                                Some(n.to_string_lossy().into_owned())
                            }
                            _ => None,
                        })
                        .collect()
                });
                if let Some([id, _file]) = lib_parts.as_deref() {
                    if plain_asset_id(id) {
                        let id = id.clone();
                        return finish_delete(ctx, id, None);
                    }
                }
                let raw = local_images_root();
                let canonical = raw.canonicalize().ok();
                let rel = path
                    .strip_prefix(&raw)
                    .ok()
                    .or_else(|| canonical.as_deref().and_then(|c| path.strip_prefix(c).ok()));
                let parts: Option<Vec<String>> = rel.and_then(|r| {
                    r.components()
                        .map(|c| match c {
                            std::path::Component::Normal(n) => {
                                Some(n.to_string_lossy().into_owned())
                            }
                            _ => None,
                        })
                        .collect()
                });
                let parts: Option<[String; 3]> = parts.and_then(|p| p.try_into().ok());
                match parts.filter(|p| p.iter().all(|x| !x.is_empty() && !x.starts_with('.'))) {
                    Some(parts) => (finalized_asset_id(&parts), None),
                    None => {
                        return Err(invalid("path is not a library or local-image gallery file"))
                    }
                }
            }
            _ => {
                return Err(invalid(
                    "path is outside the library and the local-image content root",
                ))
            }
        }
    } else {
        return Err(invalid("asset.delete needs asset_id or path"));
    };
    if !plain_asset_id(&asset_id) {
        return Err(invalid("invalid asset id"));
    }
    finish_delete(ctx, asset_id, gallery_only)
}

/// The delete itself, once the target is resolved to an asset id (and, for a gallery path, its file). Idempotent: a
/// repeat after a late crash (folder gone, asset row or job-result entry left) completes the bookkeeping.
fn finish_delete(
    ctx: &Context,
    asset_id: String,
    gallery_only: Option<PathBuf>,
) -> GenResult<serde_json::Value> {
    let dir = ctx.library_dir().join(&asset_id);
    // The pair is read once (a read error stops here, before anything is removed) and passed through. The history rows
    // of the removed gallery outputs go only after every file removal succeeded.
    let pair = read_pair(&dir)?;
    let removed = remove_then_forget(
        ctx,
        &asset_id,
        pair.as_ref(),
        gallery_only.as_deref(),
        &local_images_root(),
        &forget_local_image_outputs,
    )?;
    // Drop the asset from its job result so job listings stop showing it.
    let db = crate::store::open(ctx)?;
    if let Some(row) = crate::store::get_asset(&db, &asset_id)? {
        let current: Option<String> = db
            .read(|c| {
                let mut st = c.prepare("SELECT result_json FROM jobs WHERE job_id=?1")?;
                let mut rows = st.query_map([&row.job_id], |r| r.get::<_, Option<String>>(0))?;
                let first = rows.next().transpose()?.flatten();
                Ok(first)
            })
            .map_err(crate::store::je)?;
        if let Some(mut result) =
            current.and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
        {
            if let Some(assets) = result["assets"].as_array_mut() {
                assets.retain(|a| a["asset_id"].as_str() != Some(asset_id.as_str()));
            }
            db.write(|c| {
                c.execute(
                    "UPDATE jobs SET result_json=?2 WHERE job_id=?1",
                    rightkit_db::rusqlite::params![row.job_id, result.to_string()],
                )?;
                Ok(())
            })
            .map_err(crate::store::je)?;
        }
    }
    // The asset row goes last, once no library copy remains (a failed library removal keeps it so
    // `asset.resolve` still reports the file). Idempotent: a repeat delete matches no row.
    if !dir.exists() {
        db.write(|c| {
            c.execute(
                "DELETE FROM assets WHERE asset_id=?1",
                rightkit_db::rusqlite::params![asset_id],
            )?;
            c.execute(
                "DELETE FROM asset_facts WHERE asset_id=?1",
                rightkit_db::rusqlite::params![asset_id],
            )?;
            crate::search::drop_asset(c, &asset_id)?;
            Ok(())
        })
        .map_err(crate::store::je)?;
    }
    Ok(serde_json::json!({
        "asset_id": asset_id,
        "removed": {"library": removed.library, "gallery": removed.gallery},
    }))
}

/// Asset id of a library entry whose `facts.json` records `sha256` as its stored-bytes hash
/// (`origin.content_sha256`). Dedup for acquired assets: the same bytes from any provider map to one
/// entry. A missing library folder or an unreadable `facts.json` is skipped, never an error.
pub fn find_by_sha256(ctx: &Context, sha256: &str) -> GenResult<Option<String>> {
    let root = ctx.library_dir();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(e)),
    };
    for entry in entries.flatten() {
        let Ok(facts_bytes) = std::fs::read(entry.path().join("facts.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&facts_bytes) else {
            continue;
        };
        if value["facts"]["origin"]["content_sha256"].as_str() == Some(sha256) {
            return Ok(Some(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}
