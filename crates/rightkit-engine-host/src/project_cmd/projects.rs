//! `projects.create {root, name}` — app-ui.md §3.1 + ruling R16. Dropped
//! footage creates a project *under the configured project folder* (never the
//! footage directory): the new project dir is `root/name`, falling back to
//! `name 2`, `name 3`, … when the candidate is occupied. A candidate counts as
//! occupied when it exists and is non-empty, so a user's stray `Interview/`
//! folder is never written into.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use rightkit_video_project::{Project, ProjectError};

use crate::project::{bad, parse_fps};

/// First free `root/name` (then `root/name 2`, …). An existing directory
/// counts as taken when it already contains anything.
fn free_candidate(root: &Path, stem: &str) -> PathBuf {
    let taken = |d: &Path| {
        if !d.exists() {
            return false;
        }
        // An empty directory is fine to adopt; anything else is occupied.
        std::fs::read_dir(d)
            .map(|mut r| r.next().is_some())
            .unwrap_or(true)
    };
    for i in 0..100u32 {
        let name = if i == 0 {
            stem.to_string()
        } else {
            format!("{stem} {}", i + 1)
        };
        let dir = root.join(&name);
        if !taken(&dir) {
            return dir;
        }
    }
    // Practically unreachable; give the caller an error rather than writing
    // into an occupied folder.
    root.join(stem)
}

pub fn m_projects_create(params: &Value, dry_run: bool) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        root: PathBuf,
        name: String,
        fps: Option<Value>,
    }
    let p: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let stem = p.name.trim();
    if stem.is_empty() {
        return Err(bad("missing param: name"));
    }
    if stem.contains('/') || stem.contains('\\') {
        return Err(bad("name must not contain path separators"));
    }
    let fps = match &p.fps {
        None => parse_fps("30")?,
        Some(Value::Number(n)) => parse_fps(&n.to_string())?,
        Some(Value::String(s)) => parse_fps(s)?,
        Some(_) => return Err(bad("fps must be a number or string")),
    };
    let dir = free_candidate(&p.root, stem);
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| stem.to_string());
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {"command": "projects.create", "dir": dir.display().to_string(), "name": name},
        }));
    }
    let (status, meta) = Project::init(&dir, Some(&name), fps)?;
    Ok(json!({
        "status": status.as_str(),
        "dir": dir.display().to_string(),
        "project": {"id": meta.id, "name": meta.name, "fps": meta.fps, "created": meta.created},
    }))
}
