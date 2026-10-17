//! Committed-tree source acquisition. Port of `committed-source.mjs`: HEAD's tree and
//! the blobs a scan needs are read through `git`, hash-verified, and bounded.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::error::{Res, ScanError};
use crate::gitio::{self, dev_null, native_git, protected_env};
use crate::json::locale_compare;

const MAX_TREE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_BLOB_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;
const BLOB_CHUNK_BYTES: u64 = 32 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const BLOB_TIMEOUT: Duration = Duration::from_secs(30);

pub type Bytes = Arc<Vec<u8>>;

/// One tree entry. `bytes` is present only for blobs the scan reads.
#[derive(Debug, Clone)]
pub struct Entry {
    pub rel: String,
    pub mode: String,
    pub kind: String,
    pub oid: String,
    /// `None` for blobs the scan does not read, as Node's `undefined`.
    pub size: Option<u64>,
    pub bytes: Option<Bytes>,
}

/// Coverage entry in report shape.
pub type Coverage = (String, String);

#[derive(Debug)]
pub struct CommittedSource {
    pub repository_root: PathBuf,
    pub origin: Option<String>,
    pub commit: String,
    pub tree: String,
    pub entries: Vec<Entry>,
    pub coverage: Vec<Coverage>,
}

impl CommittedSource {
    pub fn entry(&self, rel: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.rel == rel)
    }
}

/// `git -C root --no-replace-objects -c core.fsmonitor=false -c core.hooksPath=<null>
/// -c safe.directory=<safe> args`.
pub(crate) fn git_args(root: &Path, safe_directory: &Path, args: &[&str]) -> Vec<String> {
    let mut full = vec![
        "-C".to_string(),
        root.to_string_lossy().into_owned(),
        "--no-replace-objects".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        format!("core.hooksPath={}", dev_null()),
        "-c".into(),
        format!("safe.directory={}", safe_directory.to_string_lossy()),
    ];
    full.extend(args.iter().map(|arg| (*arg).to_string()));
    full
}

fn run_git(
    root: &Path,
    safe_directory: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    timeout: Duration,
    max: usize,
) -> Res<Vec<u8>> {
    let full = git_args(root, safe_directory, args);
    let env = protected_env();
    let output = gitio::run(native_git(), &full, Some(&env), input, timeout, max);
    match output {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(_) => Err(ScanError::new(format!(
            "committed ownership: git {} failed",
            args.first().copied().unwrap_or("command")
        ))),
        Err(error) => Err(ScanError::new(format!(
            "committed ownership: git {} failed ({})",
            args.first().copied().unwrap_or("command"),
            error.code
        ))),
    }
}

fn text(root: &Path, args: &[&str]) -> Res<String> {
    let bytes = run_git(root, root, args, None, COMMAND_TIMEOUT, 4 * 1024 * 1024)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn valid_path(value: &str) -> bool {
    if value.is_empty() || value.contains('\\') || value.starts_with('/') {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    value
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Git object id of `kind bytes`, for hash verification.
pub fn hash_git_object(kind: &str, bytes: &[u8], sha256: bool) -> String {
    let header = format!("{kind} {}\0", bytes.len());
    if sha256 {
        let mut hasher = Sha256::new();
        hasher.update(header.as_bytes());
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    } else {
        let mut hasher = Sha1::new();
        hasher.update(header.as_bytes());
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }
}

fn is_oid(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

struct Row {
    rel: String,
    mode: String,
    kind: String,
    oid: String,
}

fn parse_tree(bytes: &[u8], sha256: bool) -> Res<Vec<Row>> {
    if bytes.len() > MAX_TREE_BYTES {
        return Err(ScanError::new(
            "committed ownership: tree listing exceeds 32MiB",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ScanError::new("The encoded data was not valid for encoding utf-8"))?;
    let digest_length = if sha256 { 64 } else { 40 };
    let mut rows = Vec::new();
    for record in text.split('\0') {
        if record.is_empty() {
            continue;
        }
        let Some(tab) = record.find('\t') else {
            return Err(ScanError::new("committed ownership: malformed tree entry"));
        };
        let mut head = record[..tab].split(' ');
        let (mode, kind, oid) = (
            head.next().unwrap_or(""),
            head.next().unwrap_or(""),
            head.next().unwrap_or(""),
        );
        let rel = &record[tab + 1..];
        if !valid_path(rel) || !is_oid(oid, digest_length) {
            return Err(ScanError::new(
                "committed ownership: invalid tree path or object id",
            ));
        }
        if kind != "blob" && kind != "commit" {
            return Err(ScanError::new(format!(
                "committed ownership: unsupported tree object type at {rel}"
            )));
        }
        if !matches!(mode, "100644" | "100755" | "120000" | "160000") {
            return Err(ScanError::new(format!(
                "committed ownership: unsupported tree mode at {rel}"
            )));
        }
        if (kind == "commit") != (mode == "160000") {
            return Err(ScanError::new(format!(
                "committed ownership: mismatched tree mode and object type at {rel}"
            )));
        }
        rows.push(Row {
            rel: rel.to_string(),
            mode: mode.to_string(),
            kind: kind.to_string(),
            oid: oid.to_string(),
        });
    }
    rows.sort_by(|a, b| locale_compare(&a.rel, &b.rel));
    Ok(rows)
}

/// Sizes for `oids` through `cat-file --batch-check`. Used by the committed and working
/// tree readers; `error_prefix` selects the message family.
pub(crate) fn blob_sizes(
    root: &Path,
    safe_directory: &Path,
    oids: &[String],
    timeout: Duration,
    size_cap: u64,
) -> Res<Vec<u64>> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let input = format!("{}\n", oids.join("\n"));
    if input.len() > MAX_TREE_BYTES {
        return Err(ScanError::new(
            "committed ownership: blob size query exceeds 32MiB",
        ));
    }
    let out = run_git(
        root,
        safe_directory,
        &[
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
        Some(input.as_bytes()),
        timeout,
        MAX_TREE_BYTES.min(oids.len() * 128 + 1),
    )?;
    let text = String::from_utf8_lossy(&out).trim_end().to_string();
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() != oids.len() {
        return Err(ScanError::new(
            "committed ownership: malformed blob size response",
        ));
    }
    lines
        .iter()
        .zip(oids)
        .map(|(line, expected)| {
            let mut parts = line.trim().split(' ');
            let (oid, kind, size) = (
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
            );
            match size.parse::<u64>() {
                Ok(size) if oid == expected && kind == "blob" && size <= size_cap => Ok(size),
                _ => Err(ScanError::new(
                    "committed ownership: invalid blob size response",
                )),
            }
        })
        .collect()
}

/// Reads `oids` through `cat-file --batch`, chunked by size so one spawn never buffers
/// the whole repository, hash-verifying every blob.
pub(crate) fn read_blobs(
    root: &Path,
    safe_directory: &Path,
    oids: &[String],
    sizes: &[u64],
    max_blob: u64,
) -> Res<HashMap<String, Bytes>> {
    let mut result = HashMap::new();
    let mut chunk: Vec<&String> = Vec::new();
    let mut chunk_bytes = 0u64;
    let mut flush = |chunk: &mut Vec<&String>, chunk_bytes: &mut u64| -> Res<()> {
        if !chunk.is_empty() {
            result.extend(read_chunk(
                root,
                safe_directory,
                chunk,
                *chunk_bytes,
                max_blob,
            )?);
        }
        chunk.clear();
        *chunk_bytes = 0;
        Ok(())
    };
    for (oid, size) in oids.iter().zip(sizes) {
        let cost = size + 128;
        if !chunk.is_empty() && chunk_bytes + cost > BLOB_CHUNK_BYTES {
            flush(&mut chunk, &mut chunk_bytes)?;
        }
        chunk.push(oid);
        chunk_bytes += cost;
    }
    flush(&mut chunk, &mut chunk_bytes)?;
    Ok(result)
}

fn read_chunk(
    root: &Path,
    safe_directory: &Path,
    oids: &[&String],
    expected: u64,
    max_blob: u64,
) -> Res<HashMap<String, Bytes>> {
    let input = format!(
        "{}\n",
        oids.iter()
            .map(|o| o.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let limit = usize::try_from(expected)
        .unwrap_or(usize::MAX)
        .saturating_add(1024 * 1024);
    let output = run_git(
        root,
        safe_directory,
        &["cat-file", "--batch"],
        Some(input.as_bytes()),
        BLOB_TIMEOUT,
        limit,
    )?;
    let mut result = HashMap::new();
    let mut offset = 0usize;
    for expected in oids {
        let Some(header_end) = output[offset..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| p + offset)
        else {
            return Err(ScanError::new("committed ownership: malformed blob header"));
        };
        let header = String::from_utf8_lossy(&output[offset..header_end]).into_owned();
        let mut parts = header.split(' ');
        let (oid, kind, size) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        let size: u64 = size
            .parse()
            .map_err(|_| ScanError::new("committed ownership: invalid blob object"))?;
        if oid != expected.as_str() || kind != "blob" || size > max_blob {
            return Err(ScanError::new("committed ownership: invalid blob object"));
        }
        let start = header_end + 1;
        let end = start + size as usize;
        if end >= output.len() || output[end] != b'\n' {
            return Err(ScanError::new("committed ownership: truncated blob object"));
        }
        let bytes = &output[start..end];
        if hash_git_object("blob", bytes, expected.len() == 64) != **expected {
            return Err(ScanError::new(format!(
                "committed ownership: blob hash mismatch {expected}"
            )));
        }
        result.insert((*expected).clone(), Arc::new(bytes.to_vec()));
        offset = end + 1;
    }
    if offset != output.len() {
        return Err(ScanError::new(
            "committed ownership: unexpected blob output",
        ));
    }
    Ok(result)
}

const MANIFEST_NAMES: [&str; 10] = [
    "Cargo.toml",
    "package.json",
    "Package.swift",
    "ownership-exceptions.json",
    "pnpm-workspace.yaml",
    "Cargo.lock",
    "pnpm-lock.yaml",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
];
const CODE_EXTENSIONS: [&str; 19] = [
    "rs", "ts", "tsx", "js", "mjs", "cjs", "jsx", "swift", "m", "mm", "c", "cc", "cpp", "h", "py",
    "go", "kt", "java", "cs",
];

fn has_code_extension(rel: &str) -> bool {
    rel.rsplit_once('.').is_some_and(|(_, ext)| {
        CODE_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

pub fn acquire(
    root: &Path,
    expected_commit: Option<&str>,
    expected_tree: Option<&str>,
    required_paths: &[String],
) -> Res<CommittedSource> {
    let repository_root =
        crate::paths::resolve(Path::new(&text(root, &["rev-parse", "--show-toplevel"])?));
    let origin = run_git(
        &repository_root,
        &repository_root,
        &["config", "--get", "remote.origin.url"],
        None,
        COMMAND_TIMEOUT,
        4 * 1024 * 1024,
    )
    .ok()
    .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string());
    let object_format = text(root, &["rev-parse", "--show-object-format"])?;
    if object_format != "sha1" && object_format != "sha256" {
        return Err(ScanError::new(
            "committed ownership: unsupported Git object format",
        ));
    }
    let sha256 = object_format == "sha256";
    let digest_length = if sha256 { 64 } else { 40 };
    let commit = text(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if !is_oid(&commit, digest_length) || expected_commit.is_some_and(|expected| expected != commit)
    {
        return Err(ScanError::new(
            "committed ownership: expected commit is not current HEAD",
        ));
    }
    let tree = text(
        root,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )?;
    if !is_oid(&tree, digest_length) || expected_tree.is_some_and(|expected| expected != tree) {
        return Err(ScanError::new(
            "committed ownership: expected tree does not match commit",
        ));
    }
    let listing = run_git(
        root,
        root,
        &["ls-tree", "-r", "-z", "--full-tree", &tree],
        None,
        COMMAND_TIMEOUT,
        MAX_TREE_BYTES,
    )?;
    let rows = parse_tree(&listing, sha256)?;
    let wanted: HashSet<&str> = required_paths
        .iter()
        .map(String::as_str)
        .filter(|p| valid_path(p))
        .collect();
    let should_read = |row: &Row| {
        row.kind == "blob"
            && row.mode != "120000"
            && (wanted.contains(row.rel.as_str())
                || MANIFEST_NAMES.contains(&row.rel.rsplit('/').next().unwrap_or(""))
                || has_code_extension(&row.rel))
    };
    let candidates: Vec<&Row> = rows.iter().filter(|row| should_read(row)).collect();
    let mut unique: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for row in &candidates {
        if seen.insert(row.oid.clone()) {
            unique.push(row.oid.clone());
        }
    }
    let sizes = blob_sizes(root, root, &unique, COMMAND_TIMEOUT, u64::MAX >> 1)?;
    let size_by_oid: HashMap<&str, u64> = unique
        .iter()
        .map(String::as_str)
        .zip(sizes.iter().copied())
        .collect();
    let mut blob_oids: Vec<String> = Vec::new();
    let mut blob_sizes_list: Vec<u64> = Vec::new();
    let mut coverage = Vec::new();
    let mut total = 0u64;
    let mut taken: HashSet<&str> = HashSet::new();
    for row in &candidates {
        let size = size_by_oid[row.oid.as_str()];
        if size > MAX_BLOB_BYTES {
            coverage.push((row.rel.clone(), "committed-blob-over-64MiB".to_string()));
            continue;
        }
        if !taken.contains(row.oid.as_str()) && total + size > MAX_TOTAL_BLOB_BYTES {
            coverage.push((row.rel.clone(), "committed-blobs-over-128MiB".to_string()));
            continue;
        }
        if taken.insert(row.oid.as_str()) {
            total += size;
            blob_oids.push(row.oid.clone());
            blob_sizes_list.push(size);
        }
    }
    let blobs = read_blobs(root, root, &blob_oids, &blob_sizes_list, MAX_BLOB_BYTES)?;
    let entries = rows
        .into_iter()
        .map(|row| {
            let size = if row.kind == "blob" {
                size_by_oid.get(row.oid.as_str()).copied()
            } else {
                Some(0)
            };
            let bytes = blobs.get(&row.oid).cloned();
            Entry {
                rel: row.rel,
                mode: row.mode,
                kind: row.kind,
                oid: row.oid,
                size,
                bytes,
            }
        })
        .collect();
    Ok(CommittedSource {
        repository_root,
        origin,
        commit,
        tree,
        entries,
        coverage,
    })
}

/// Requires a clean checkout; with expectations, also that HEAD did not move.
pub fn assert_checkout_clean(
    root: &Path,
    expected_commit: Option<&str>,
    expected_tree: Option<&str>,
) -> Res<()> {
    if expected_commit.is_some() || expected_tree.is_some() {
        let commit = text(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        let tree = text(
            root,
            &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
        )?;
        if expected_commit.is_some_and(|e| e != commit) || expected_tree.is_some_and(|e| e != tree)
        {
            return Err(ScanError::new(
                "committed ownership: HEAD changed during protected source acquisition",
            ));
        }
    }
    let output = run_git(
        root,
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        None,
        COMMAND_TIMEOUT,
        MAX_TREE_BYTES,
    )?;
    if !output.is_empty() {
        return Err(ScanError::new(
            "committed ownership: protected release checkout has tracked or untracked changes",
        ));
    }
    Ok(())
}

/// Convenience for tests: entries by `rel`.
pub fn entry_map(source: &CommittedSource) -> BTreeMap<&str, &Entry> {
    source
        .entries
        .iter()
        .map(|entry| (entry.rel.as_str(), entry))
        .collect()
}
