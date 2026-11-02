use serde_json::Value;
use std::{env, fs, path::PathBuf};

// The identity must reach the compiler as SOURCE, not as a process env var.
// `cargo:rustc-env` + `option_env!` looks equivalent, but sccache does not hash
// arbitrary process env, so a cached object compiled before the identity
// existed is reused verbatim and the binary ships "sha256:unknown" while the
// build log shows the real digest. A generated file lands in the dep-info
// sccache does hash, so a changed identity forces a real recompile.
fn emit(commit: &str, tree: &str, kind: &str) {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("out directory"))
        .join("release_identity_generated.rs");
    let generated = format!(
        "pub const SOURCE_COMMIT: Option<&str> = {};
pub const SOURCE_TREE_SHA256: Option<&str> = {};
pub const SOURCE_KIND: &str = {kind:?};
",
        match commit {
            "" => "None".to_string(),
            value => format!("Some({value:?})"),
        },
        match tree {
            "" => "None".to_string(),
            value => format!("Some({value:?})"),
        },
    );
    let unchanged = fs::read(&out)
        .map(|bytes| bytes == generated.as_bytes())
        .unwrap_or(false);
    if !unchanged {
        fs::write(&out, generated).expect("write generated release identity");
    }
}

fn lowercase_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

// Build scripts run before rightkit-child-process is available, so std Command is the only option here.
#[allow(clippy::disallowed_methods)]
fn git(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_identity(engine: &std::path::Path) -> Option<(String, String)> {
    let commit = git(&engine, &["rev-parse", "HEAD"])?;
    let tree_id = git(&engine, &["rev-parse", "HEAD:./"])?;
    if let Some(head) = git(&engine, &["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={}", engine.join(head).display());
    }
    let tree = rightkit_fs::digest::sha256_bytes(tree_id.as_bytes());
    lowercase_hex(&commit, 40).then_some((commit, tree))
}

// A broker checked-tree snapshot has no `.git` (rustright#176). The broker writes the commit and
// the git tree ids it exported next to the snapshot root; the tree id of `membrane/engine` gives the
// same `sourceTreeSha256` git gives in the primary checkout, so one commit has one identity on
// both build paths. The file is looked for upward from this crate so the engine may sit at any
// depth; the first one found wins.
fn snapshot_identity(engine: &std::path::Path) -> Option<(String, String)> {
    let engine = engine.canonicalize().ok()?;
    let names: Vec<std::path::Component> = engine.components().collect();
    // Deepest directory first: the engine itself, then each directory holding it.
    for depth in (1..=names.len()).rev() {
        let dir: PathBuf = names[..depth].iter().collect();
        // Path of the engine relative to the directory holding the file, as the broker keys it.
        let components: Vec<String> = names[depth..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let candidate = dir.join(".rightkit-source-identity.json");
        if let Ok(bytes) = fs::read(&candidate) {
            println!("cargo:rerun-if-changed={}", candidate.display());
            let value: Value = serde_json::from_slice(&bytes).ok()?;
            let commit = value.get("commit").and_then(Value::as_str)?;
            let key = components.join("/");
            let tree_id = value
                .get("trees")
                .and_then(|trees| trees.get(key.as_str()))
                .and_then(Value::as_str)?;
            if !lowercase_hex(commit, 40) || !lowercase_hex(tree_id, 40) {
                return None;
            }
            let tree = rightkit_fs::digest::sha256_bytes(tree_id.as_bytes());
            return Some((commit.to_string(), tree));
        }
    }
    None
}

// Last resort when neither git, the broker snapshot file nor the environment supplies an identity
// (#551): a digest over the engine's own sources, so a git-less build still carries a real
// `sha256:<64 hex>` generation and never ships "unknown". It is stable for identical source bytes
// but is not the git tree digest, so it differs from a primary-checkout build of the same commit;
// the build says so (cargo warning) and the binary reports `release_identity_source: content`.
fn content_identity(engine: &std::path::Path) -> Option<String> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if matches!(name.as_str(), "target" | ".git" | "node_modules" | "dist") {
                    continue;
                }
                walk(&path, root, out);
            } else if kind.is_file() {
                let Ok(bytes) = fs::read(&path) else { continue };
                let rel = path
                    .strip_prefix(root)
                    .map(|value| value.to_string_lossy().replace('\\', "/"))
                    .unwrap_or(name);
                out.push((rel, rightkit_fs::digest::sha256_bytes(&bytes)));
            }
        }
    }
    let engine = engine.canonicalize().ok()?;
    let mut files = Vec::new();
    for top in ["Cargo.toml", "Cargo.lock"] {
        let path = engine.join(top);
        if let Ok(bytes) = fs::read(&path) {
            println!("cargo:rerun-if-changed={}", path.display());
            files.push((top.to_string(), rightkit_fs::digest::sha256_bytes(&bytes)));
        }
    }
    // An engine checkout hashes its crates; this crate alone (a registry build) hashes itself.
    let crates = engine.join("crates");
    let sources = if crates.is_dir() {
        crates
    } else {
        engine.clone()
    };
    println!("cargo:rerun-if-changed={}", sources.display());
    walk(&sources, &engine, &mut files);
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut listing = String::new();
    for (path, digest) in &files {
        listing.push_str(path);
        listing.push('\0');
        listing.push_str(digest);
        listing.push('\n');
    }
    Some(rightkit_fs::digest::sha256_bytes(listing.as_bytes()))
}

// The gate's own environment may carry the identity (#551 option b): both values or neither.
fn env_identity() -> Option<(String, String)> {
    println!("cargo:rerun-if-env-changed=RIGHTKIT_CONTEXT_SOURCE_COMMIT");
    println!("cargo:rerun-if-env-changed=RIGHTKIT_CONTEXT_SOURCE_TREE_SHA256");
    let commit = env::var("RIGHTKIT_CONTEXT_SOURCE_COMMIT").ok()?;
    let tree = env::var("RIGHTKIT_CONTEXT_SOURCE_TREE_SHA256").ok()?;
    (lowercase_hex(&commit, 40) && lowercase_hex(&tree, 64)).then_some((commit, tree))
}

// The engine root: RIGHTKIT_CONTEXT_ENGINE_ROOT when the host names it, else the directory holding
// `crates/` when this crate sits in an engine checkout or snapshot (`<engine>/crates/<this crate>`).
// A registry build has neither and identifies itself from its own sources.
fn engine_root(manifest: &std::path::Path) -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed=RIGHTKIT_CONTEXT_ENGINE_ROOT");
    if let Some(root) = env::var_os("RIGHTKIT_CONTEXT_ENGINE_ROOT").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(root));
    }
    let names: Vec<std::path::Component> = manifest.components().collect();
    let holder = names.len().checked_sub(2)?;
    (names[holder].as_os_str() == "crates").then(|| names[..holder].iter().collect())
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let engine = engine_root(&manifest);
    let identity = engine.as_ref().map(|root| {
        let dist = root.join("dist");
        let identity = dist.join("release-identity.json");
        // Watch the file when it exists, else the directory it would appear in, else the root.
        let watch = if identity.exists() {
            identity.clone()
        } else if dist.is_dir() {
            dist
        } else {
            root.clone()
        };
        println!("cargo:rerun-if-changed={}", watch.display());
        identity
    });

    let Some(Ok(bytes)) = identity.as_ref().map(fs::read) else {
        // No release-identity file (every build inside the host app): derive the identity from git, so the
        // binary never ships "sha256:unknown". The commit is HEAD; the tree digest is the SHA-256 of
        // membrane/engine's git tree id. Without git, the build still works and says so.
        let (found, kind) = match engine.as_deref().and_then(git_identity) {
            Some(found) => (Some(found), "git"),
            None => match engine.as_deref().and_then(snapshot_identity) {
                Some(found) => (Some(found), "snapshot"),
                None => match env_identity() {
                    Some(found) => (Some(found), "env"),
                    None => (None, ""),
                },
            },
        };
        match found {
            Some((commit, tree)) => {
                println!("cargo:rustc-env=RIGHTKIT_CONTEXT_SOURCE_COMMIT={commit}");
                println!("cargo:rustc-env=RIGHTKIT_CONTEXT_SOURCE_TREE_SHA256={tree}");
                emit(&commit, &tree, kind);
            }
            None => match content_identity(engine.as_deref().unwrap_or(&manifest)) {
                Some(tree) => {
                    println!(
                        "cargo:warning=release identity: no git, no .rightkit-source-identity.json and no RIGHTKIT_CONTEXT_SOURCE_* environment; this build derives releaseGeneration from a digest of the engine sources (release_identity_source=content), which differs from a git-derived build of the same commit."
                    );
                    println!("cargo:rustc-env=RIGHTKIT_CONTEXT_SOURCE_TREE_SHA256={tree}");
                    emit("", &tree, "content");
                }
                None => {
                    println!(
                        "cargo:warning=release identity missing, git is unavailable, no .rightkit-source-identity.json was found above the engine and the engine sources could not be read; this build reports releaseGeneration sha256:unknown."
                    );
                    emit("", "", "unknown");
                }
            },
        }
        return;
    };
    let value: Value = serde_json::from_slice(&bytes).expect("release identity must be valid JSON");
    let commit = value
        .get("commit")
        .and_then(Value::as_str)
        .filter(|value| lowercase_hex(value, 40))
        .expect("release identity commit must be 40 lowercase hex characters");
    let tree = value
        .get("sourceTreeSha256")
        .and_then(Value::as_str)
        .filter(|value| lowercase_hex(value, 64))
        .expect("release identity source tree must be 64 lowercase hex characters");
    let generation = value
        .get("releaseGeneration")
        .and_then(Value::as_str)
        .expect("release identity generation is required");
    assert_eq!(
        generation,
        format!("sha256:{tree}"),
        "release identity generation mismatch"
    );
    println!("cargo:rustc-env=RIGHTKIT_CONTEXT_SOURCE_COMMIT={commit}");
    println!("cargo:rustc-env=RIGHTKIT_CONTEXT_SOURCE_TREE_SHA256={tree}");
    emit(commit, tree, "file");
}
