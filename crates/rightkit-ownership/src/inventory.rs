//! Working-tree inventory constrained to Git's tracked and non-ignored path set. Port of
//! `ownership-git-inventory.mjs` plus `contentInventory` and `listedCoverage` from
//! `ownership.mjs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::committed::{blob_sizes, git_args, read_blobs, Bytes};
use crate::gitio::{self, native_git, protected_env};
use crate::json::{locale_compare, sha256_hex, sha256_json};
use crate::paths;
use crate::types::Cov;

const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;
const MAX_BLOB_BATCH_BYTES: u64 = 256 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_millis(2500);
pub const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const OPAQUE_DIRS: [&str; 4] = ["target", ".build", "DerivedData", "Pods"];

#[derive(Debug, Clone)]
pub struct File {
    pub rel: String,
    pub abs: PathBuf,
    pub size: Option<u64>,
    pub bytes: Option<Bytes>,
    pub oid: Option<String>,
    pub digest: Option<String>,
    pub cache_key: Option<String>,
    pub in_node_modules: bool,
}

#[derive(Debug, Default)]
pub struct Inventory {
    pub files: Vec<File>,
    pub coverage: Vec<Cov>,
}

struct Staged {
    mode: String,
    oid: String,
    stage: u32,
}

#[derive(Default)]
struct ListEntry {
    stages: Vec<Staged>,
    relevant: bool,
    over_limit: bool,
    max_file_bytes: u64,
    stat: Option<std::fs::Metadata>,
    stat_error: bool,
    symlink: bool,
    non_file: bool,
    clean_blob_size: Option<u64>,
}

fn run_listing(
    root: &Path,
    repository_root: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let full = git_args(root, repository_root, args);
    let env = protected_env();
    match gitio::run(
        native_git(),
        &full,
        Some(&env),
        input,
        GIT_TIMEOUT,
        MAX_LIST_BYTES,
    ) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(_) => Err(format!("ownership inventory: git {} failed", args[0])),
        Err(error) => Err(format!(
            "ownership inventory: git {} failed ({})",
            args[0], error.code
        )),
    }
}

fn inventory_message(message: String) -> String {
    message.replacen("committed ownership: ", "ownership inventory: ", 1)
}

fn is_stage_header(candidate: &[u8]) -> bool {
    // ^(?:100644|100755|120000|160000) [0-9a-f]{40,64} \d$
    let text = match std::str::from_utf8(candidate) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let parts: Vec<&str> = text.split(' ').collect();
    parts.len() == 3
        && matches!(parts[0], "100644" | "100755" | "120000" | "160000")
        && (40..=64).contains(&parts[1].len())
        && parts[1]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && parts[2].len() == 1
        && parts[2].as_bytes()[0].is_ascii_digit()
}

pub fn git_listed_inventory(
    app_root: &Path,
    relevant: &dyn Fn(&str) -> bool,
    protected_paths: &[String],
    max_file_bytes: &dyn Fn(&str) -> u64,
    repository_root: &Path,
) -> Inventory {
    let root = paths::resolve(app_root);
    let one = |coverage: Cov| Inventory {
        files: Vec::new(),
        coverage: vec![coverage],
    };
    let safe_root = match paths::real(repository_root) {
        Ok(path) => path,
        Err(error) => {
            return one(Cov::new(
                ".",
                format!("repository-root-unavailable:{}", paths::io_code(&error)),
            ))
        }
    };
    let listed = run_listing(
        &root,
        &safe_root,
        &[
            "ls-files",
            "--stage",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        None,
    )
    .and_then(|listing| {
        run_listing(
            &root,
            &safe_root,
            &[
                "diff-files",
                "--name-only",
                "-z",
                "--no-ext-diff",
                "--no-textconv",
                "--relative",
            ],
            None,
        )
        .map(|dirty| (listing, dirty))
    });
    let (listing, dirty_bytes) = match listed {
        Ok(pair) => pair,
        Err(message) => return one(Cov::new(".", format!("git-inventory-failed:{message}"))),
    };
    let dirty: HashSet<String> = if dirty_bytes.is_empty() {
        HashSet::new()
    } else {
        let end = dirty_bytes.len() - usize::from(dirty_bytes.last() == Some(&0));
        String::from_utf8_lossy(&dirty_bytes[..end])
            .split('\0')
            .map(str::to_string)
            .collect()
    };
    let mut order: Vec<String> = Vec::new();
    let mut entries: HashMap<String, ListEntry> = HashMap::new();
    let parsed: Result<(), String> = (|| {
        let mut offset = 0;
        while offset < listing.len() {
            let Some(end) = listing[offset..]
                .iter()
                .position(|b| *b == 0)
                .map(|p| p + offset)
            else {
                return Err("malformed NUL listing".into());
            };
            let record = &listing[offset..end];
            offset = end + 1;
            let tab = record.iter().position(|b| *b == 9);
            let decode = |bytes: &[u8]| {
                std::str::from_utf8(bytes)
                    .map(str::to_string)
                    .map_err(|_| "The encoded data was not valid for encoding utf-8".to_string())
            };
            match tab {
                Some(tab) if is_stage_header(&record[..tab]) => {
                    let header = String::from_utf8_lossy(&record[..tab]).into_owned();
                    let parts: Vec<&str> = header.split(' ').collect();
                    let rel = decode(&record[tab + 1..])?;
                    let entry = entries.entry(rel.clone()).or_insert_with(|| {
                        order.push(rel.clone());
                        ListEntry::default()
                    });
                    entry.stages.push(Staged {
                        mode: parts[0].into(),
                        oid: parts[1].into(),
                        stage: parts[2].parse().unwrap_or(0),
                    });
                }
                _ => {
                    let rel = decode(record)?;
                    if !entries.contains_key(&rel) {
                        order.push(rel.clone());
                        entries.insert(rel.clone(), ListEntry::default());
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(message) = parsed {
        return one(Cov::new(".", format!("git-listing-invalid:{message}")));
    }
    let protected: HashSet<&str> = protected_paths.iter().map(String::as_str).collect();
    let mut coverage: Vec<Cov> = Vec::new();
    let mut relevant_rels: Vec<String> = Vec::new();
    for rel in &order {
        let entry = entries.get_mut(rel).expect("listed entry");
        if entry.stages.iter().any(|s| s.stage != 0) || entry.stages.len() > 1 {
            continue;
        }
        let selected = protected.contains(rel.as_str()) || relevant(rel);
        entry.relevant = selected;
        if !selected {
            continue;
        }
        entry.max_file_bytes = max_file_bytes(rel);
        let abs = abs_path(&root, rel);
        match std::fs::symlink_metadata(&abs) {
            Ok(stat) => entry.stat = Some(stat),
            Err(error) => {
                entry.stat_error = true;
                if error.kind() == std::io::ErrorKind::NotFound {
                    coverage.push(Cov::new(rel.clone(), "deleted"));
                } else {
                    coverage.push(Cov::new(
                        rel.clone(),
                        format!("stat-failed:{}", paths::io_code(&error)),
                    ));
                }
            }
        }
        if let Some(stat) = &entry.stat {
            if stat.file_type().is_symlink() {
                entry.symlink = true;
                coverage.push(Cov::new(rel.clone(), "symlink"));
            } else if !stat.is_file() {
                entry.non_file = true;
                coverage.push(Cov::new(rel.clone(), "not-regular-file"));
            }
            if stat.is_file() && dirty.contains(rel) && stat.len() > entry.max_file_bytes {
                entry.over_limit = true;
                coverage.push(Cov::new(rel.clone(), "file-over-size-limit"));
            }
        }
        let mode = entry.stages.first().map(|s| s.mode.as_str());
        if !entry.stat_error
            && !entry.symlink
            && !entry.non_file
            && !entry.over_limit
            && mode != Some("120000")
            && mode != Some("160000")
        {
            relevant_rels.push(rel.clone());
        }
    }
    let mut blob_oids: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for rel in &relevant_rels {
        let entry = &entries[rel];
        if let Some(stage) = entry.stages.first() {
            if !entry.over_limit && !dirty.contains(rel) && seen.insert(stage.oid.clone()) {
                blob_oids.push(stage.oid.clone());
            }
        }
    }
    let fetched = blob_sizes(
        &root,
        &safe_root,
        &blob_oids,
        GIT_TIMEOUT,
        MAX_BLOB_BATCH_BYTES,
    )
    .and_then(|sizes| read_blobs(&root, &safe_root, &blob_oids, &sizes, MAX_BLOB_BATCH_BYTES));
    let fetched = match fetched {
        Ok(map) => map,
        Err(error) => {
            return one(Cov::new(
                ".",
                format!("git-blob-read-failed:{}", inventory_message(error.message)),
            ))
        }
    };
    for rel in &relevant_rels {
        let entry = entries.get_mut(rel).expect("relevant entry");
        let Some(oid) = entry.stages.first().map(|s| s.oid.clone()) else {
            continue;
        };
        if dirty.contains(rel) || entry.over_limit {
            continue;
        }
        let Some(bytes) = fetched.get(&oid) else {
            continue;
        };
        entry.clean_blob_size = Some(bytes.len() as u64);
        if bytes.len() as u64 > entry.max_file_bytes {
            entry.over_limit = true;
            coverage.push(Cov::new(rel.clone(), "file-over-size-limit"));
        }
    }
    let mut files = Vec::new();
    for rel in &order {
        let entry = entries.get_mut(rel).expect("listed entry");
        let abs = abs_path(&root, rel);
        if entry.stages.iter().any(|s| s.stage != 0) || entry.stages.len() > 1 {
            coverage.push(Cov::new(rel.clone(), "git-index-conflict"));
            continue;
        }
        let indexed = entry.stages.first();
        let mode = indexed.map(|s| s.mode.as_str());
        if mode == Some("160000") {
            coverage.push(Cov::new(rel.clone(), "gitlink"));
            continue;
        }
        if mode == Some("120000") {
            coverage.push(Cov::new(rel.clone(), "symlink"));
            continue;
        }
        let oid = indexed.map(|s| s.oid.clone());
        let is_relevant = entry.relevant;
        let is_dirty = indexed.is_none() || dirty.contains(rel);
        let clean_irrelevant_regular =
            !is_relevant && !is_dirty && oid.is_some() && matches!(mode, Some("100644" | "100755"));
        let mut stat = entry.stat.take();
        if !clean_irrelevant_regular {
            if stat.is_none() {
                match std::fs::symlink_metadata(&abs) {
                    Ok(found) => stat = Some(found),
                    Err(error) => {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            coverage.push(Cov::new(rel.clone(), "deleted"));
                        } else {
                            coverage.push(Cov::new(
                                rel.clone(),
                                format!("stat-failed:{}", paths::io_code(&error)),
                            ));
                        }
                        continue;
                    }
                }
            }
            let found = stat.as_ref().expect("stat");
            if found.file_type().is_symlink() {
                coverage.push(Cov::new(rel.clone(), "symlink"));
                continue;
            }
            if !found.is_file() {
                coverage.push(Cov::new(rel.clone(), "not-regular-file"));
                continue;
            }
        }
        let in_node_modules = rel.split('/').any(|part| part == "node_modules");
        let mut bytes: Option<Bytes> = None;
        let mut digest = None;
        let mut cache_key = None;
        if is_relevant && entry.over_limit {
            // Size-limit coverage was already recorded before any blob or file read.
        } else if is_relevant {
            if !is_dirty && oid.as_ref().is_some_and(|o| fetched.contains_key(o)) {
                bytes = oid.as_ref().and_then(|o| fetched.get(o)).cloned();
            } else if is_dirty {
                let too_big = stat
                    .as_ref()
                    .is_some_and(|s| s.len() > entry.max_file_bytes);
                if too_big {
                    coverage.push(Cov::new(rel.clone(), "file-over-size-limit"));
                } else {
                    match std::fs::read(&abs) {
                        Ok(read) => bytes = Some(std::sync::Arc::new(read)),
                        Err(error) => {
                            coverage.push(Cov::new(
                                rel.clone(),
                                format!("file-read-failed:{}", paths::io_code(&error)),
                            ));
                            continue;
                        }
                    }
                }
            } else {
                coverage.push(Cov::new(rel.clone(), "git-blob-cache-miss"));
            }
            if bytes
                .as_ref()
                .is_some_and(|b| b.len() as u64 > entry.max_file_bytes)
            {
                coverage.push(Cov::new(rel.clone(), "file-over-size-limit"));
                bytes = None;
            }
            if let Some(read) = &bytes {
                if is_dirty {
                    let sum = sha256_hex(read);
                    cache_key = Some(format!("sha256:{sum}"));
                    digest = Some(sum);
                } else if let Some(oid) = &oid {
                    digest = Some(format!("git:{oid}"));
                    cache_key = Some(format!("gitblob:{oid}"));
                }
            }
        } else if !is_dirty {
            if let Some(oid) = &oid {
                digest = Some(format!("git:{oid}"));
                cache_key = Some(format!("gitblob:{oid}"));
            }
        }
        let size = bytes
            .as_ref()
            .map(|b| b.len() as u64)
            .or(entry.clean_blob_size)
            .or_else(|| stat.as_ref().map(|s| s.len()));
        files.push(File {
            rel: rel.clone(),
            abs,
            size,
            bytes,
            oid,
            digest,
            cache_key,
            in_node_modules,
        });
    }
    Inventory { files, coverage }
}

fn abs_path(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
    }
    path
}

/// Coverage for installed and build-cache directories (`listedCoverage`).
pub fn listed_coverage(inventory: &mut Inventory) {
    let mut reported: HashSet<String> = HashSet::new();
    let mut additions = Vec::new();
    for file in &inventory.files {
        let parts: Vec<&str> = file.rel.split('/').collect();
        let node_modules = parts.iter().position(|p| *p == "node_modules");
        let opaque = parts.iter().position(|p| OPAQUE_DIRS.contains(p));
        for (at, reason) in [
            (node_modules, "node-modules-provenance-not-verified"),
            (opaque, "installed-or-build-cache"),
        ] {
            let Some(at) = at else { continue };
            let dir = parts[..=at].join("/");
            if reported.insert(format!("{dir}:{reason}")) {
                additions.push(Cov::new(dir, reason));
            }
        }
    }
    inventory.coverage.extend(additions);
}

/// Result of `contentInventory`.
pub struct Content {
    pub digest: String,
    pub coverage: Vec<Cov>,
    pub complete: bool,
}

/// Git-listed content inventory; clean inputs use their blob IDs.
pub fn content_inventory(app_root: &Path) -> Content {
    let mut inventory =
        git_listed_inventory(app_root, &|_| true, &[], &|_| 64 * 1024 * 1024, app_root);
    listed_coverage(&mut inventory);
    let mut files: Vec<(&str, Option<u64>, &str)> = inventory
        .files
        .iter()
        .filter_map(|f| f.digest.as_deref().map(|d| (f.rel.as_str(), f.size, d)))
        .collect();
    files.sort_by(|a, b| locale_compare(a.0, b.0));
    let list: Vec<Value> = files
        .iter()
        .map(|(path, size, digest)| match size {
            Some(size) => json!({"path": path, "size": size, "digest": digest}),
            None => json!({"path": path, "digest": digest}),
        })
        .collect();
    let complete = inventory.coverage.is_empty();
    Content {
        digest: sha256_json(&Value::Array(list)),
        coverage: inventory.coverage,
        complete,
    }
}
