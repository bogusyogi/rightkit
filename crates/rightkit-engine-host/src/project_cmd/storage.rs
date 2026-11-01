//! `storage.status` / `storage.clear`: regenerable cache usage for the
//! Settings screen (app-ui.md §3.9). Caches live under `<project>/.cache`
//! (thumbnails, waveforms) and the engine data dir's `cache/`; deleting them
//! only costs recompute time — no project or media state is touched.
//! The waveform and thumbnail roots are rightkit-cache `DiskCache`s, sized and
//! cleared through it, including the flat files pre-port builds left in them (the
//! cache's foreign files); the rest of `<project>/.cache` and the data-dir cache are
//! split into loose top-level entries; directory sizing & clearing belongs to rightkit-cache.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_cache::{Config as CacheConfig, DiskCache};
use serde::Deserialize;
use serde_json::{json, Value};

use rightkit_video_project::ProjectError;

use crate::env::Env;
use crate::project::{bad, io_err, thumb_cache_root};

/// One cache directory in scope, in report order.
enum Scope {
    /// Loose top-level entries, excluding separately reported managed roots & in-flight temps.
    Loose { path: PathBuf, skip: Vec<PathBuf> },
    /// A rightkit-cache `DiskCache` root, sized and cleared through rightkit-cache.
    Managed {
        category: &'static str,
        path: PathBuf,
    },
}

/// Cache directories in scope: the project's `.cache` (its DiskCache roots get their own
/// rows) plus, when a project dir is given, the engine data-dir cache.
fn cache_scope(env: &Env, dir: Option<&Path>) -> Result<Vec<Scope>, ProjectError> {
    let mut scope = Vec::new();
    if let Some(d) = dir {
        // `waveform` mirrors `m_media_waveform` in media_cmd.rs.
        let managed = [
            ("waveform", d.join(".cache").join("waveform")),
            ("thumbs", thumb_cache_root(d)),
        ];
        scope.push(Scope::Loose {
            path: d.join(".cache"),
            skip: managed.iter().map(|(_, p)| p.clone()).collect(),
        });
        scope.extend(
            managed
                .into_iter()
                .map(|(category, path)| Scope::Managed { category, path }),
        );
    }
    scope.push(Scope::Loose {
        path: env.app_root.path("cache").map_err(|e| bad(e.to_string()))?,
        skip: Vec::new(),
    });
    Ok(scope)
}

/// Bytes per managed root: its indexed entries plus its foreign files (flat files that
/// pre-port builds left under the root). A missing root reports 0 and is not created.
fn managed_bytes(scope: &[Scope]) -> Result<Vec<(&'static str, u64)>, ProjectError> {
    let mut sizes = Vec::new();
    for s in scope {
        if let Scope::Managed { category, path } = s {
            sizes.push((*category, managed_root_bytes(path)?));
        }
    }
    Ok(sizes)
}

fn managed_root_bytes(root: &Path) -> Result<u64, ProjectError> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(io_err(format!("inspecting {}", root.display()))(e)),
        Ok(_) => {}
    }
    let cache =
        DiskCache::open_read_only(root).map_err(io_err(format!("opening {}", root.display())))?;
    let foreign = cache
        .foreign_files()
        .map_err(io_err(format!("reading {}", root.display())))?;
    Ok(cache.stats().bytes + foreign.bytes)
}

fn bytes_of(sizes: &[(&'static str, u64)], category: &str) -> u64 {
    sizes
        .iter()
        .find(|(c, _)| *c == category)
        .map_or(0, |(_, b)| *b)
}

/// The host chooses the cache scopes; RightKit owns recursive sizing, deletion & temp safety.
fn loose_entries(path: &Path, skip: &[PathBuf]) -> Result<Vec<std::fs::DirEntry>, ProjectError> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(format!("reading {}", path.display()))(e)),
    };
    entries
        .filter_map(|entry| match entry {
            Ok(entry) if skip.contains(&entry.path()) || entry.file_name() == ".tmp" => None,
            entry => Some(entry.map_err(io_err(format!("reading {}", path.display())))),
        })
        .collect()
}

fn loose_bytes(path: &Path, skip: &[PathBuf]) -> Result<u64, ProjectError> {
    let mut bytes = 0u64;
    for entry in loose_entries(path, skip)? {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(io_err(format!("inspecting {}", path.display())))?;
        bytes += if metadata.is_dir() {
            managed_root_bytes(&path)?
        } else {
            metadata.len()
        };
    }
    Ok(bytes)
}

fn clear_loose(path: &Path, skip: &[PathBuf]) -> Result<u64, ProjectError> {
    let mut freed = 0u64;
    for entry in loose_entries(path, skip)? {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(io_err(format!("inspecting {}", path.display())))?;
        freed += if metadata.is_dir() {
            clear_managed(&path)?
        } else {
            std::fs::remove_file(&path).map_err(io_err(format!("clearing {}", path.display())))?;
            metadata.len()
        };
    }
    Ok(freed)
}

/// Clear one rightkit-cache root, then its foreign files, and return the bytes both removed.
/// Opened writable only when it exists, so a missing root is not created. The stale-temp age is
/// `Duration::MAX`, so opening sweeps only temp files whose owner pid is dead; a live producer's
/// in-flight output is never removed here.
fn clear_managed(root: &Path) -> Result<u64, ProjectError> {
    if !root.is_dir() {
        return Ok(0);
    }
    let config = CacheConfig::new(u64::MAX).stale_tmp_after(Duration::MAX);
    let cache =
        DiskCache::open(root, config).map_err(io_err(format!("opening {}", root.display())))?;
    let cleared = cache.clear();
    let foreign = cache
        .clear_foreign()
        .map_err(io_err(format!("clearing {}", root.display())))?;
    Ok(cleared.bytes + foreign.bytes)
}

pub fn m_storage_status(
    params: &Value,
    env: &Env,
    product_folder: &str,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        dir: Option<PathBuf>,
    }
    let p: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let scope = cache_scope(env, p.dir.as_deref())?;
    let managed = managed_bytes(&scope)?;
    let mut rows = Vec::new();
    let mut total = 0u64;
    for s in &scope {
        let (path, bytes) = match s {
            Scope::Loose { path, skip } => (path, loose_bytes(path, skip)?),
            Scope::Managed { category, path } => (path, bytes_of(&managed, category)),
        };
        total += bytes;
        rows.push(json!({"path": path.display().to_string(), "bytes": bytes}));
    }
    Ok(json!({
        "cache_dirs": rows,
        "cache_bytes": total,
        // R16: where dropped-footage projects live when Settings › Storage ›
        // Project folder is unset. Never the user's footage folder.
        "default_project_root": default_project_root(env, product_folder)?.display().to_string(),
    }))
}

/// `~/Movies/<product>` on macOS, `%USERPROFILE%\Videos\<product>` on Windows.
// See docs/rightkit-exceptions/M6.md#b2: user content stays outside app data.
pub fn default_project_root(env: &Env, product_folder: &str) -> Result<PathBuf, ProjectError> {
    let home = env
        .home
        .as_deref()
        .filter(|home| home.is_absolute())
        .ok_or_else(|| bad("no absolute user home resolved for the default project folder"))?;
    Ok(home
        .join(if env.os == "windows" {
            "Videos"
        } else {
            "Movies"
        })
        .join(product_folder))
}

pub fn m_storage_clear(params: &Value, env: &Env, dry_run: bool) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        dir: Option<PathBuf>,
    }
    let p: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let scope = cache_scope(env, p.dir.as_deref())?;
    // A dry run sizes the managed roots without clearing them. A real clear reports the bytes
    // that rightkit-cache's own clear and clear_foreign returned.
    let sizes = if dry_run {
        managed_bytes(&scope)?
    } else {
        Vec::new()
    };
    let mut freed = 0u64;
    for s in &scope {
        match s {
            Scope::Loose { path, skip } => {
                freed += if dry_run {
                    loose_bytes(path, skip)?
                } else {
                    clear_loose(path, skip)?
                };
            }
            Scope::Managed { category, path } => {
                if dry_run {
                    freed += bytes_of(&sizes, category);
                } else {
                    freed += clear_managed(path)?;
                }
            }
        }
    }
    Ok(json!({"status": "cleared", "freed_bytes": freed, "dry_run": dry_run}))
}

#[cfg(test)]
mod project_root_tests {
    use super::*;

    // Native known-folder APIs cannot produce missing/relative homes through a CLI e2e.
    #[test]
    fn default_project_root_refuses_missing_or_relative_home() {
        let suite = rightkit_daemon::Suite::per_run_temp("rightkit-project-root").unwrap();
        let mut env = Env::new("macos", None, suite.app_root("TestApp").unwrap(), None);
        assert!(default_project_root(&env, "TestApp").is_err());
        env.home = Some(PathBuf::from("relative-home"));
        assert!(default_project_root(&env, "TestApp").is_err());
        env.home = Some(suite.root().to_path_buf());
        let root = default_project_root(&env, "TestApp").unwrap();
        assert!(root.is_absolute());
        assert_eq!(root, suite.root().join("Movies/TestApp"));
        env.os = "windows".into();
        assert_eq!(
            default_project_root(&env, "TestApp").unwrap(),
            suite.root().join("Videos/TestApp")
        );
    }
}
