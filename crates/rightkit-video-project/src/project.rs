//! Project directory: `project.json`, `timeline.json`, `media/`, `renders/`, `.lock`.
//!
//! `project.json` is written last during init, so its presence marks an initialized
//! project; every write goes through [`atomic_write`].

use std::fs;
use std::path::{Path, PathBuf};

use rightkit_timeline::{Action, ApplyOutcome, Fps, SourceId, Timeline, TimelineStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ProjectError;
use crate::fsutil::atomic_write;
use crate::lock::ProjectLock;

pub const PROJECT_FILE: &str = "project.json";
pub const TIMELINE_FILE: &str = "timeline.json";
pub const MEDIA_DIR: &str = "media";
pub const RENDERS_DIR: &str = "renders";
pub const PROJECT_SCHEMA_VERSION: u32 = 1;
const PROJECT_KIND: &str = "cutright.project";

/// Engine extensions `project.show` advertises to editor frontends; each id
/// opens a matching capability gate in `apps/cutright/src/editor`.
pub const EDITOR_EXTENSIONS: &[&str] = &[
    "edit.expect_cursor",
    "edit.batch",
    "edit.split",
    "edit.slip",
    "edit.ripple_delete",
    "media.waveform",
    "edit.captions",
    "edit.mute",
    "edit.swap_take",
    "edit.title_position",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    /// Owned copy under `media/`.
    Copy,
    /// Referenced in place; the path is absolute.
    Reference,
}

/// Where a registered source's bytes live. The id is the BLAKE3 of those bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub id: SourceId,
    /// Original file name.
    pub name: String,
    pub mode: SourceMode,
    /// Relative to the project directory for copies, absolute for references.
    pub path: String,
    pub size: u64,
    /// RFC 3339 UTC.
    pub added: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    pub schema_version: u32,
    pub kind: String,
    pub id: String,
    pub name: String,
    pub fps: Fps,
    /// RFC 3339 UTC.
    pub created: String,
    #[serde(default)]
    pub sources: Vec<SourceRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No lock; in-memory changes cannot be saved (dry runs, `show`).
    Read,
    /// Takes the exclusive project lock.
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitStatus {
    Created,
    AlreadyInitialized,
}

impl InitStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InitStatus::Created => "created",
            InitStatus::AlreadyInitialized => "already_initialized",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "revision")]
pub enum EditOutcome {
    /// A new revision with this sequence number was recorded.
    Recorded(u64),
    /// The action changed nothing.
    NoChange,
    /// The action equals the latest applied revision: a retry of a completed edit.
    AlreadyApplied,
}

impl EditOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            EditOutcome::Recorded(_) => "recorded",
            EditOutcome::NoChange => "no_change",
            EditOutcome::AlreadyApplied => "already_applied",
        }
    }
}

pub struct Project {
    pub dir: PathBuf,
    pub meta: ProjectMeta,
    pub store: TimelineStore,
    lock: Option<ProjectLock>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn read(path: &Path) -> Result<Vec<u8>, ProjectError> {
    fs::read(path).map_err(|e| ProjectError::io(format!("reading {}", path.display()), e))
}

fn absolute(dir: &Path) -> Result<PathBuf, ProjectError> {
    std::path::absolute(dir)
        .map_err(|e| ProjectError::io(format!("resolving {}", dir.display()), e))
}

fn meta_bytes(meta: &ProjectMeta) -> Vec<u8> {
    let mut b = serde_json::to_vec_pretty(meta).expect("project meta serializes");
    b.push(b'\n');
    b
}

fn load_meta(dir: &Path) -> Result<Option<ProjectMeta>, ProjectError> {
    let path = dir.join(PROJECT_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let meta: ProjectMeta = serde_json::from_slice(&read(&path)?)
        .map_err(|e| ProjectError::Corrupt(format!("{PROJECT_FILE}: {e}")))?;
    if meta.schema_version != PROJECT_SCHEMA_VERSION || meta.kind != PROJECT_KIND {
        return Err(ProjectError::Corrupt(format!(
            "{PROJECT_FILE}: unsupported schema {} / kind {:?}",
            meta.schema_version, meta.kind
        )));
    }
    Ok(Some(meta))
}

fn new_project_id(dir: &Path) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = rightkit_fs::digest::Blake3Hasher::new();
    h.update(dir.as_os_str().as_encoded_bytes());
    h.update(&nanos.to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.finalize()[..32].to_string()
}

fn default_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "project".into())
}

impl Project {
    /// Decides what `init` would do without touching anything.
    pub fn plan_init(dir: &Path, fps: Fps) -> Result<InitStatus, ProjectError> {
        let dir = absolute(dir)?;
        match load_meta(&dir)? {
            Some(meta) if meta.fps == fps => Ok(InitStatus::AlreadyInitialized),
            Some(meta) => Err(ProjectError::ProjectExists(format!(
                "{} already exists with fps {}/{}; refusing to change it",
                dir.display(),
                meta.fps.num,
                meta.fps.den
            ))),
            None => {
                let timeline = dir.join(TIMELINE_FILE);
                if timeline.exists()
                    && read(&timeline)? != TimelineStore::new(fps).to_canonical_json()
                {
                    return Err(ProjectError::ProjectExists(format!(
                        "{} has a {TIMELINE_FILE} but no {PROJECT_FILE}",
                        dir.display()
                    )));
                }
                Ok(InitStatus::Created)
            }
        }
    }

    /// Creates the project (idempotent: re-running with the same fps changes nothing).
    pub fn init(
        dir: &Path,
        name: Option<&str>,
        fps: Fps,
    ) -> Result<(InitStatus, ProjectMeta), ProjectError> {
        let dir = absolute(dir)?;
        fs::create_dir_all(&dir)
            .map_err(|e| ProjectError::io(format!("creating {}", dir.display()), e))?;
        let _lock = ProjectLock::acquire(&dir)?;
        let status = Self::plan_init(&dir, fps)?;
        for sub in [MEDIA_DIR, RENDERS_DIR] {
            let p = dir.join(sub);
            fs::create_dir_all(&p)
                .map_err(|e| ProjectError::io(format!("creating {}", p.display()), e))?;
        }
        if status == InitStatus::AlreadyInitialized {
            let meta = load_meta(&dir)?.expect("planned as initialized");
            return Ok((status, meta));
        }
        atomic_write(
            &dir.join(TIMELINE_FILE),
            &TimelineStore::new(fps).to_canonical_json(),
        )?;
        let meta = ProjectMeta {
            schema_version: PROJECT_SCHEMA_VERSION,
            kind: PROJECT_KIND.into(),
            id: new_project_id(&dir),
            name: name
                .map(str::to_string)
                .unwrap_or_else(|| default_name(&dir)),
            fps,
            created: now_rfc3339(),
            sources: Vec::new(),
        };
        atomic_write(&dir.join(PROJECT_FILE), &meta_bytes(&meta))?;
        Ok((status, meta))
    }

    pub fn open(dir: &Path, access: Access) -> Result<Self, ProjectError> {
        let dir = absolute(dir)?;
        if !dir.join(PROJECT_FILE).is_file() {
            return Err(ProjectError::NotAProject(dir.display().to_string()));
        }
        let lock = match access {
            Access::Read => None,
            Access::Write => Some(ProjectLock::acquire(&dir)?),
        };
        let meta =
            load_meta(&dir)?.ok_or_else(|| ProjectError::NotAProject(dir.display().to_string()))?;
        let timeline = dir.join(TIMELINE_FILE);
        if !timeline.is_file() {
            return Err(ProjectError::Corrupt(format!("missing {TIMELINE_FILE}")));
        }
        let store = TimelineStore::from_canonical_json(&read(&timeline)?)?;
        if store.timeline().fps != meta.fps {
            return Err(ProjectError::Corrupt(format!(
                "{TIMELINE_FILE} fps differs from {PROJECT_FILE}"
            )));
        }
        if access == Access::Write {
            for sub in [MEDIA_DIR, RENDERS_DIR] {
                let p = dir.join(sub);
                fs::create_dir_all(&p)
                    .map_err(|e| ProjectError::io(format!("creating {}", p.display()), e))?;
            }
        }
        Ok(Self {
            dir,
            meta,
            store,
            lock,
        })
    }

    pub fn is_writable(&self) -> bool {
        self.lock.is_some()
    }

    /// Stale-lock owner this open recovered from, if any.
    pub fn recovered_stale_lock(&self) -> Option<&crate::lock::LockOwner> {
        self.lock.as_ref().and_then(|l| l.recovered_stale.as_ref())
    }

    fn require_write(&self) -> Result<(), ProjectError> {
        if self.lock.is_none() {
            return Err(ProjectError::BadInput(
                "project was opened read-only".into(),
            ));
        }
        Ok(())
    }

    pub fn save_timeline(&self) -> Result<(), ProjectError> {
        self.require_write()?;
        atomic_write(
            &self.dir.join(TIMELINE_FILE),
            &self.store.to_canonical_json(),
        )
    }

    pub fn save_meta(&self) -> Result<(), ProjectError> {
        self.require_write()?;
        atomic_write(&self.dir.join(PROJECT_FILE), &meta_bytes(&self.meta))
    }

    pub fn timeline(&self) -> &Timeline {
        self.store.timeline()
    }

    pub fn source_record(&self, id: &SourceId) -> Option<&SourceRecord> {
        self.meta.sources.iter().find(|s| &s.id == id)
    }

    pub fn source_path(&self, record: &SourceRecord) -> PathBuf {
        match record.mode {
            SourceMode::Copy => self.dir.join(&record.path),
            SourceMode::Reference => PathBuf::from(&record.path),
        }
    }

    /// Applies an action in memory. A byte-identical repeat of the latest applied
    /// revision is reported as [`EditOutcome::AlreadyApplied`] (every action either
    /// fails or is a no-op when repeated, so this only ever answers a retry).
    pub fn apply(&mut self, action: Action) -> Result<EditOutcome, ProjectError> {
        let c = self.store.cursor();
        if c > 0 && self.store.revisions()[c - 1].action == action {
            return Ok(EditOutcome::AlreadyApplied);
        }
        Ok(match self.store.apply(action)? {
            ApplyOutcome::Recorded(seq) => EditOutcome::Recorded(seq),
            ApplyOutcome::NoChange => EditOutcome::NoChange,
        })
    }

    /// Applies an action and persists the timeline when it changed.
    pub fn edit(&mut self, action: Action) -> Result<EditOutcome, ProjectError> {
        self.require_write()?;
        let outcome = self.apply(action)?;
        if matches!(outcome, EditOutcome::Recorded(_)) {
            self.save_timeline()?;
        }
        Ok(outcome)
    }

    /// Undo in memory. With `expect_cursor` (the cursor the caller saw before
    /// asking), a retry of a completed undo reports `Ok(false)` instead of undoing twice.
    pub fn undo_in_memory(&mut self, expect_cursor: Option<usize>) -> Result<bool, ProjectError> {
        if let Some(expected) = expect_cursor {
            let actual = self.store.cursor();
            if actual + 1 == expected {
                return Ok(false);
            }
            if actual != expected {
                return Err(ProjectError::CursorMismatch { expected, actual });
            }
        }
        self.store.undo()?;
        Ok(true)
    }

    pub fn redo_in_memory(&mut self, expect_cursor: Option<usize>) -> Result<bool, ProjectError> {
        if let Some(expected) = expect_cursor {
            let actual = self.store.cursor();
            if actual == expected + 1 {
                return Ok(false);
            }
            if actual != expected {
                return Err(ProjectError::CursorMismatch { expected, actual });
            }
        }
        self.store.redo()?;
        Ok(true)
    }

    pub fn undo(&mut self, expect_cursor: Option<usize>) -> Result<bool, ProjectError> {
        self.require_write()?;
        let changed = self.undo_in_memory(expect_cursor)?;
        if changed {
            self.save_timeline()?;
        }
        Ok(changed)
    }

    pub fn redo(&mut self, expect_cursor: Option<usize>) -> Result<bool, ProjectError> {
        self.require_write()?;
        let changed = self.redo_in_memory(expect_cursor)?;
        if changed {
            self.save_timeline()?;
        }
        Ok(changed)
    }

    /// Current revision cursor (count of applied revisions).
    pub fn cursor(&self) -> usize {
        self.store.cursor()
    }

    /// True when the revision at `cursor` was produced by `action` (retry probe:
    /// an edit resent after landing reports `AlreadyApplied` instead of a
    /// cursor mismatch).
    pub fn last_action_matches(&self, action: &Action) -> bool {
        let c = self.store.cursor();
        c > 0 && self.store.revisions()[c - 1].action == *action
    }

    pub fn revision_summary(&self) -> Value {
        json!({
            "cursor": self.store.cursor(),
            "total": self.store.revisions().len(),
            "can_undo": self.store.can_undo(),
            "can_redo": self.store.can_redo(),
            "head_hash": self.store.head_hash(),
        })
    }

    /// Timeline summary plus revision cursor, as JSON.
    pub fn show(&self) -> Value {
        let tl = self.timeline();
        let layout: Vec<Value> = tl
            .video_tracks
            .iter()
            .flat_map(|t| {
                tl.placements(t).into_iter().map(move |p| {
                    json!({"track": t.id, "clip": p.clip_id, "start_frame": p.start_frame, "frames": p.frames})
                })
            })
            .collect();
        let sources: Vec<Value> = tl
            .sources
            .iter()
            .map(|s| {
                let record = self.source_record(&s.id);
                json!({
                    "info": s,
                    "name": record.map(|r| r.name.clone()),
                    "mode": record.map(|r| r.mode),
                })
            })
            .collect();
        // Sources that carry a validated transcript under analysis/ (independent
        // word-edge evidence for transcript-driven editing).
        let transcripts: Vec<Value> = self
            .meta
            .sources
            .iter()
            .filter(|r| {
                self.dir
                    .join("analysis")
                    .join(format!("transcript-{}.json", r.id.as_str()))
                    .is_file()
            })
            .map(|r| json!({"source": r.id.as_str()}))
            .collect();
        json!({
            "project": {
                "id": self.meta.id,
                "name": self.meta.name,
                "dir": self.dir.display().to_string(),
                "created": self.meta.created,
                "fps": self.meta.fps,
            },
            "revision": self.revision_summary(),
            "extensions": EDITOR_EXTENSIONS,
            "summary": {
                "total_frames": tl.total_frames(),
                "duration_ms": tl.fps.frame_to_ticks(tl.total_frames()).to_units(1000),
                "sources": tl.sources.len(),
                "video_clips": tl.video_tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
                "audio_clips": tl.audio_tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
            },
            "sources": sources,
            "layout": layout,
            "timeline": tl,
            "transcripts": transcripts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_timeline::{SourceInfo, Ticks, VideoClip, VideoStreamInfo};

    const FPS30: Fps = Fps { num: 30, den: 1 };

    fn sid(n: &str) -> SourceId {
        SourceId::new(rightkit_fs::digest::blake3_bytes(n.as_bytes())).unwrap()
    }

    fn register(n: &str) -> Action {
        Action::RegisterSource {
            source: SourceInfo {
                id: sid(n),
                duration: Ticks::from_millis(10_000),
                video: Some(VideoStreamInfo {
                    width: 64,
                    height: 64,
                    duration: None,
                }),
                has_audio: false,
            },
        }
    }

    fn insert(id: &str, n: &str) -> Action {
        Action::InsertClip {
            track_id: "V1".into(),
            index: 0,
            clip: VideoClip {
                id: id.into(),
                source: sid(n),
                source_in: Ticks::ZERO,
                source_out: Ticks::from_millis(1000),
            },
        }
    }

    #[test]
    fn open_requires_a_project_and_reopens_history() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            Project::open(d.path(), Access::Read).err().unwrap().code(),
            "not_a_project"
        );
        let dir = d.path().join("p");
        Project::init(&dir, None, FPS30).unwrap();
        {
            let mut p = Project::open(&dir, Access::Write).unwrap();
            p.edit(register("a")).unwrap();
            p.edit(insert("c1", "a")).unwrap();
        }
        let mut p = Project::open(&dir, Access::Write).unwrap();
        assert_eq!(p.store.cursor(), 2);
        assert!(p.undo(None).unwrap());
        drop(p);
        let p = Project::open(&dir, Access::Read).unwrap();
        assert_eq!(p.store.cursor(), 1);
        assert!(p.store.can_redo());
    }

    #[test]
    fn writers_are_exclusive_but_readers_are_not() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p");
        Project::init(&dir, None, FPS30).unwrap();
        let w = Project::open(&dir, Access::Write).unwrap();
        assert_eq!(
            Project::open(&dir, Access::Write).err().unwrap().code(),
            "project_locked"
        );
        Project::open(&dir, Access::Read).unwrap();
        drop(w);
        Project::open(&dir, Access::Write).unwrap();
    }

    #[test]
    fn undo_redo_with_expected_cursor_are_retry_safe() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p");
        Project::init(&dir, None, FPS30).unwrap();
        let mut p = Project::open(&dir, Access::Write).unwrap();
        p.edit(register("a")).unwrap();
        p.edit(insert("c1", "a")).unwrap();
        assert!(p.undo(Some(2)).unwrap());
        assert!(!p.undo(Some(2)).unwrap(), "retry must not undo twice");
        assert_eq!(p.store.cursor(), 1);
        assert_eq!(p.undo(Some(5)).unwrap_err().code(), "cursor_mismatch");
        assert!(p.redo(Some(1)).unwrap());
        assert!(!p.redo(Some(1)).unwrap());
        assert_eq!(p.store.cursor(), 2);
        assert_eq!(p.redo(None).unwrap_err().code(), "nothing_to_redo");
    }

    #[test]
    fn read_only_project_cannot_save() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p");
        Project::init(&dir, None, FPS30).unwrap();
        let mut p = Project::open(&dir, Access::Read).unwrap();
        assert_eq!(p.apply(register("a")).unwrap(), EditOutcome::Recorded(1));
        assert!(p.save_timeline().is_err());
        assert!(p.edit(register("b")).is_err());
        let again = Project::open(&dir, Access::Read).unwrap();
        assert_eq!(again.store.cursor(), 0);
    }

    #[test]
    fn show_advertises_editor_extensions_and_transcripts() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p");
        Project::init(&dir, None, FPS30).unwrap();
        let p = Project::open(&dir, Access::Read).unwrap();
        let shown = p.show();
        let exts: Vec<&str> = shown["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        for needed in [
            "edit.expect_cursor",
            "edit.batch",
            "edit.split",
            "edit.slip",
        ] {
            assert!(exts.contains(&needed), "missing {needed}");
        }
        assert_eq!(shown["transcripts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn cursor_probe_matches_only_the_head_action() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p");
        Project::init(&dir, None, FPS30).unwrap();
        let mut p = Project::open(&dir, Access::Write).unwrap();
        assert_eq!(p.cursor(), 0);
        assert!(!p.last_action_matches(&register("a")));
        p.edit(register("a")).unwrap();
        p.edit(insert("c1", "a")).unwrap();
        assert_eq!(p.cursor(), 2);
        assert!(p.last_action_matches(&insert("c1", "a")));
        assert!(!p.last_action_matches(&register("a")));
        p.undo(None).unwrap();
        assert!(p.last_action_matches(&register("a")));
    }
}
