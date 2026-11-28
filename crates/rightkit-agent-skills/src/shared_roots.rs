//! The shared `.agents/skills` convention (mcp-plugins-skills D32 / D-parity-3): Codex, pi and
//! OpenCode read a skill folder at `~/.agents/skills` and at `<project>/.agents/skills`.
//!
//! Scope and activation (R13): the user folder is the user's own and is scanned like the host's
//! own user folder. A project folder is repository-controlled, so it is returned only for a
//! `trusted` workspace, and what it holds is only ever *discovered* until a user activates it:
//! the activation (workspace trust key + slug + `SKILL.md` digest) lives in [`ActivationStore`]
//! under the state root, and only activated, unchanged skills are staged for a session.

use std::path::{Path, PathBuf};

/// The folder name both Codex and pi use, under a home or project root.
pub const SHARED_SKILL_DIR: &str = ".agents/skills";

/// The project folders both conventions use, in precedence order: the shared `.agents/skills`
/// (Codex, pi, OpenCode) then `.claude/skills` (Claude Code, OpenCode).
pub const PROJECT_SKILL_DIRS: [&str; 2] = [SHARED_SKILL_DIR, ".claude/skills"];

/// The user-level shared skill folder under `home`.
pub fn user_shared_skill_dir(home: &Path) -> PathBuf {
    home.join(SHARED_SKILL_DIR)
}

/// The user's Claude Code skills folder under `home`.
pub fn user_claude_skill_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("skills")
}

/// The project-level skill folders of `workspace`, or none when the workspace is not trusted
/// (project-scoped config takes effect only in a trusted workspace, tools rule 13).
pub fn project_skill_roots(workspace: &Path, trusted: bool) -> Vec<PathBuf> {
    if !trusted {
        return Vec::new();
    }
    PROJECT_SKILL_DIRS
        .iter()
        .map(|dir| workspace.join(dir))
        .collect()
}

/// The project-level shared skill folder of `workspace` (`.agents/skills`), or `None` when the
/// workspace is not trusted.
pub fn project_shared_skill_dir(workspace: &Path, trusted: bool) -> Option<PathBuf> {
    trusted.then(|| workspace.join(SHARED_SKILL_DIR))
}

/// SHA-256 of a skill file, hex. A changed `SKILL.md` has a new digest and needs re-activation.
pub fn skill_digest(content: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(content)
}

/// The per-workspace activation record (mcp R13), kept under the state root and never in the
/// repository: for each workspace trust key, the slugs a user activated and the digest of the
/// `SKILL.md` they activated. A corrupt or missing file means nothing is activated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivationStore {
    /// workspace trust key -> slug -> digest
    workspaces: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

impl ActivationStore {
    /// Read the store at `path`; anything unreadable is an empty store (fail closed).
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::default();
        };
        let mut store = Self::default();
        if let Some(workspaces) = value.get("workspaces").and_then(|v| v.as_object()) {
            for (key, skills) in workspaces {
                let Some(skills) = skills.as_object() else {
                    continue;
                };
                for (slug, entry) in skills {
                    if let Some(digest) = entry.get("digest").and_then(|d| d.as_str()) {
                        store
                            .workspaces
                            .entry(key.clone())
                            .or_default()
                            .insert(slug.clone(), digest.to_string());
                    }
                }
            }
        }
        store
    }

    /// Whether `slug` is activated in the workspace `key` for exactly this content digest.
    pub fn is_active(&self, key: &str, slug: &str, digest: &str) -> bool {
        self.workspaces
            .get(key)
            .and_then(|skills| skills.get(slug))
            .is_some_and(|recorded| recorded == digest)
    }

    pub fn activate(&mut self, key: &str, slug: &str, digest: &str) {
        self.workspaces
            .entry(key.to_string())
            .or_default()
            .insert(slug.to_string(), digest.to_string());
    }

    /// Returns whether the slug had been activated.
    pub fn deactivate(&mut self, key: &str, slug: &str) -> bool {
        let removed = self
            .workspaces
            .get_mut(key)
            .is_some_and(|skills| skills.remove(slug).is_some());
        if self
            .workspaces
            .get(key)
            .is_some_and(|skills| skills.is_empty())
        {
            self.workspaces.remove(key);
        }
        removed
    }

    /// Write the store atomically (temp file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut workspaces = serde_json::Map::new();
        for (key, skills) in &self.workspaces {
            let mut entries = serde_json::Map::new();
            for (slug, digest) in skills {
                entries.insert(slug.clone(), serde_json::json!({ "digest": digest }));
            }
            workspaces.insert(key.clone(), serde_json::Value::Object(entries));
        }
        let body = serde_json::to_vec_pretty(&serde_json::json!({ "workspaces": workspaces }))
            .map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, body)?;
        std::fs::rename(&temp, path)
    }
}

/// One skill found in a project's skill folders. A candidate is not a loaded skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSkillCandidate {
    pub slug: String,
    pub path: PathBuf,
    /// SHA-256 of the `SKILL.md` as it is now.
    pub digest: String,
    /// True only when a user activated exactly this content in this workspace (R13).
    pub activated: bool,
}

/// List the skill folders (`<slug>/SKILL.md`) of the project's skill folders, sorted by slug; a
/// slug present in both folders is listed once, from `.agents/skills`. Empty for an untrusted
/// workspace. Symlinks are not followed. `activated` is read from `store` for `key`.
pub fn discover_project_skill_candidates(
    workspace: &Path,
    trusted: bool,
    key: &str,
    store: &ActivationStore,
) -> Vec<ProjectSkillCandidate> {
    let mut out: Vec<ProjectSkillCandidate> = Vec::new();
    for dir in project_skill_roots(workspace, trusted) {
        // A symlinked skill root would pull in folders from outside the repository.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }
            let Some(slug) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if out.iter().any(|existing| existing.slug == slug) {
                continue;
            }
            let path = entry.path().join("SKILL.md");
            let Some(content) = read_project_skill_file(&path) else {
                continue;
            };
            let digest = skill_digest(&content);
            out.push(ProjectSkillCandidate {
                activated: store.is_active(key, &slug, &digest),
                slug,
                path,
                digest,
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// Copy each activated candidate's `SKILL.md` (exactly the activated content) into
/// `out_dir/<slug>/SKILL.md` and return `out_dir`, or `None` when nothing is activated. The
/// session then scans `out_dir` like any installed skill folder, so an activated project skill
/// reaches the prompt and `invoke_skill` the same way, and a file edited after activation is not
/// copied. `out_dir` is rebuilt each call.
pub fn materialize_activated_project_skills(
    workspace: &Path,
    trusted: bool,
    key: &str,
    store: &ActivationStore,
    out_dir: &Path,
) -> std::io::Result<Option<PathBuf>> {
    // Rebuilt each call: a failed removal must not leave a de-activated skill staged.
    match std::fs::remove_dir_all(out_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let activated: Vec<_> = discover_project_skill_candidates(workspace, trusted, key, store)
        .into_iter()
        .filter(|candidate| candidate.activated)
        .collect();
    if activated.is_empty() {
        return Ok(None);
    }
    for candidate in activated {
        let Some(content) = read_project_skill_file(&candidate.path) else {
            continue;
        };
        // Re-check the digest of the bytes actually copied.
        if skill_digest(&content) != candidate.digest {
            continue;
        }
        let target = out_dir.join(&candidate.slug);
        std::fs::create_dir_all(&target)?;
        std::fs::write(target.join("SKILL.md"), content)?;
    }
    Ok(Some(out_dir.to_path_buf()))
}

/// Read a project `SKILL.md` only when it is a regular file (not a symlink, FIFO or device)
/// no larger than [`crate::registry_install::MAX_SKILL_SOURCE_BYTES`]. A symlinked `SKILL.md`
/// could otherwise copy a secret such as `~/.ssh/id_rsa` into the prompt, and a FIFO or
/// `/dev/zero` would hang or exhaust session create (audit 2026-11-05 skills M1).
fn read_project_skill_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let limit = crate::registry_install::MAX_SKILL_SOURCE_BYTES as u64;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > limit {
        return None;
    }
    // The open refuses a link at the final component and anything that is not a regular file, so
    // a symlink swapped in after the check above is not followed.
    let file = rightkit_fs::open_read_nofollow(path, &rightkit_fs::ReadOptions::new()).ok()?;
    let mut raw = Vec::new();
    file.take(limit + 1).read_to_end(&mut raw).ok()?;
    (raw.len() as u64 <= limit).then_some(raw)
}
