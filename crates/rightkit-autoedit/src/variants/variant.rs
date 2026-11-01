//! Variant documents: creation from the cut engine, persistence, promotion.

use std::fs;
use std::path::{Path, PathBuf};

use crate::cuts::{
    apply_group, plan_cuts, undo_group, ActionGroup, AppliedGroup, CutConfig, CutPlan,
    RealiseOptions, TimeMap,
};
use rightkit_timeline::{Action, Fps, SourceInfo, Timeline, TimelineStore};
use rightkit_transcript::Transcript;
use serde::{Deserialize, Serialize};

use crate::variants::error::VariantError;

pub const VARIANT_KIND: &str = "cutright.variant";
const SCHEMA_VERSION: u32 = 1;
const DIR: &str = "variants";

/// Where a variant came from; recorded in `variant.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub schema_version: u32,
    pub kind: String,
    pub name: String,
    pub source_blake3: String,
    /// BLAKE3 of the transcript's compact JSON ([`transcript_blake3`]).
    pub transcript_blake3: String,
    pub preset: crate::cuts::Preset,
    /// True when `config` differs from the stock `preset` configuration.
    pub custom: bool,
    pub config: CutConfig,
    /// BLAKE3 of `plan.json`'s exact bytes.
    pub plan_blake3: String,
    /// Timeline head hash when the variant was created.
    pub timeline_blake3: String,
    pub fps: Fps,
    pub output_frames: i64,
    pub clips: usize,
    pub cuts: usize,
    pub kept_words: usize,
}

/// A named variant: its provenance, cut plan and own revision-backed timeline.
#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub provenance: Provenance,
    pub plan: CutPlan,
    pub store: TimelineStore,
}

impl Variant {
    pub fn time_map(&self) -> TimeMap {
        self.plan.time_map()
    }

    /// True once the variant's own timeline was edited after creation. Compare
    /// and seek data still describe the cut plan as created.
    pub fn is_edited(&self) -> bool {
        self.store.head_hash() != self.provenance.timeline_blake3
    }
}

/// BLAKE3 (hex) of the transcript's compact JSON: the identity a variant binds to.
pub fn transcript_blake3(t: &Transcript) -> String {
    rightkit_fs::digest::blake3_bytes(&serde_json::to_vec(t).expect("transcript serializes"))
}

/// Variant names are path components: `[a-z0-9][a-z0-9_-]{0,47}`.
pub fn validate_variant_name(name: &str) -> Result<(), VariantError> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.chars().enumerate().all(|(i, c)| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && (c == '-' || c == '_'))
        });
    if ok {
        Ok(())
    } else {
        Err(VariantError::BadName(name.into()))
    }
}

/// The registered source matching `transcript` in a timeline (e.g. the main one).
pub fn source_info_for(tl: &Timeline, transcript: &Transcript) -> Option<SourceInfo> {
    tl.sources
        .iter()
        .find(|s| s.id.as_str() == transcript.source_blake3)
        .cloned()
}

/// Plans `cfg` over `transcript` and realises it into a fresh timeline store
/// (V1 + linked narration on A1). Nothing is written to disk.
pub fn create_variant(
    name: &str,
    source: &SourceInfo,
    transcript: &Transcript,
    envelope: Option<&[f32]>,
    cfg: CutConfig,
    fps: Fps,
) -> Result<Variant, VariantError> {
    validate_variant_name(name)?;
    if source.id.as_str() != transcript.source_blake3 {
        return Err(VariantError::Mismatch(format!(
            "transcript describes source {} but the source is {}",
            transcript.source_blake3,
            source.id.as_str()
        )));
    }
    let plan = plan_cuts(transcript, envelope, &cfg, fps)?;
    let mut store = TimelineStore::new(fps);
    store.apply(Action::RegisterSource {
        source: source.clone(),
    })?;
    let group = plan.group(&RealiseOptions::default());
    apply_group(&mut store, &group)?;
    let provenance = Provenance {
        schema_version: SCHEMA_VERSION,
        kind: VARIANT_KIND.into(),
        name: name.into(),
        source_blake3: transcript.source_blake3.clone(),
        transcript_blake3: transcript_blake3(transcript),
        preset: cfg.preset,
        custom: cfg != CutConfig::for_preset(cfg.preset),
        config: cfg,
        plan_blake3: rightkit_fs::digest::blake3_bytes(&plan.to_json()),
        timeline_blake3: store.head_hash(),
        fps,
        output_frames: plan.time_map().output_frames(),
        clips: plan.kept.len(),
        cuts: plan.removed.len(),
        kept_words: plan.kept_words.len(),
    };
    Ok(Variant {
        name: name.into(),
        provenance,
        plan,
        store,
    })
}

/// The stock `tight` and `natural` variants of one source.
pub fn create_preset_variants(
    source: &SourceInfo,
    transcript: &Transcript,
    envelope: Option<&[f32]>,
    fps: Fps,
) -> Result<Vec<Variant>, VariantError> {
    [
        ("tight", CutConfig::tight()),
        ("natural", CutConfig::natural()),
    ]
    .into_iter()
    .map(|(n, c)| create_variant(n, source, transcript, envelope, c, fps))
    .collect()
}

fn variant_dir(project: &Path, name: &str) -> PathBuf {
    project.join(DIR).join(name)
}

fn io_err(p: &Path) -> impl FnOnce(std::io::Error) -> VariantError + '_ {
    move |source| VariantError::Io {
        path: p.display().to_string(),
        source,
    }
}

/// Writes `variants/<name>/{timeline,plan,variant}.json`. Fails if the variant
/// exists unless `overwrite`. The main timeline file is never touched.
pub fn save_variant(project: &Path, v: &Variant, overwrite: bool) -> Result<PathBuf, VariantError> {
    validate_variant_name(&v.name)?;
    let dir = variant_dir(project, &v.name);
    if dir.join("variant.json").exists() && !overwrite {
        return Err(VariantError::Exists(v.name.clone()));
    }
    fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    rightkit_fs::atomic_write(&dir.join("plan.json"), &v.plan.to_json())
        .map_err(|e| io_err(&dir.join("plan.json"))(std::io::Error::other(e)))?;
    rightkit_fs::atomic_write(&dir.join("timeline.json"), &v.store.to_canonical_json())
        .map_err(|e| io_err(&dir.join("timeline.json"))(std::io::Error::other(e)))?;
    let mut prov = serde_json::to_vec_pretty(&v.provenance).expect("provenance serializes");
    prov.push(b'\n');
    // variant.json last: its presence marks a complete variant.
    rightkit_fs::atomic_write(&dir.join("variant.json"), &prov)
        .map_err(|e| io_err(&dir.join("variant.json"))(std::io::Error::other(e)))?;
    Ok(dir.join("timeline.json"))
}

/// The exact parent revision a child variant was derived from, stored beside
/// the child as `variants/<child>/parent.json`. The child's `variant.json` is
/// unchanged by it; the link is optional and only written for children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentLink {
    /// Parent variant name.
    pub parent: String,
    /// Parent timeline head hash at the derived revision.
    pub parent_head_hash: String,
    /// Parent applied-revision count at the derived revision.
    pub parent_cursor: u64,
    /// BLAKE3 of the render export the child was derived from, when one bound it.
    pub export_blake3: Option<String>,
    /// What derived the child, e.g. `retention:audience`.
    pub relation: String,
}

const PARENT_FILE: &str = "parent.json";

/// Writes a child variant and its [`ParentLink`]. Fails if the child already
/// exists, before anything is written, so an existing link is never replaced.
/// The parent and every other variant are not touched.
pub fn save_child_variant(
    project: &Path,
    v: &Variant,
    parent: &ParentLink,
) -> Result<PathBuf, VariantError> {
    validate_variant_name(&v.name)?;
    let dir = variant_dir(project, &v.name);
    if dir.join("variant.json").exists() {
        return Err(VariantError::Exists(v.name.clone()));
    }
    fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    let mut link = serde_json::to_vec_pretty(parent).expect("parent link serializes");
    link.push(b'\n');
    rightkit_fs::atomic_write(&dir.join(PARENT_FILE), &link)
        .map_err(|e| io_err(&dir.join(PARENT_FILE))(std::io::Error::other(e)))?;
    save_variant(project, v, false)
}

fn read(path: &Path) -> Result<Vec<u8>, VariantError> {
    fs::read(path).map_err(io_err(path))
}

/// Loads and verifies a variant: plan hash, timeline replay (every revision's
/// hash) and name/source agreement.
pub fn load_variant(project: &Path, name: &str) -> Result<Variant, VariantError> {
    validate_variant_name(name)?;
    let dir = variant_dir(project, name);
    let vpath = dir.join("variant.json");
    if !vpath.is_file() {
        return Err(VariantError::NotFound(name.into()));
    }
    let bad = |detail: String| VariantError::Integrity {
        name: name.into(),
        detail,
    };
    let provenance: Provenance =
        serde_json::from_slice(&read(&vpath)?).map_err(|e| VariantError::Json {
            path: vpath.display().to_string(),
            detail: e.to_string(),
        })?;
    if provenance.kind != VARIANT_KIND || provenance.schema_version != SCHEMA_VERSION {
        return Err(bad("unexpected kind or schema version".into()));
    }
    if provenance.name != name {
        return Err(bad(format!("document names {:?}", provenance.name)));
    }
    let plan_bytes = read(&dir.join("plan.json"))?;
    if rightkit_fs::digest::blake3_bytes(&plan_bytes) != provenance.plan_blake3 {
        return Err(bad("plan.json does not match its recorded BLAKE3".into()));
    }
    let plan: CutPlan = serde_json::from_slice(&plan_bytes).map_err(|e| VariantError::Json {
        path: dir.join("plan.json").display().to_string(),
        detail: e.to_string(),
    })?;
    if plan.source.as_str() != provenance.source_blake3 {
        return Err(bad("plan source differs from provenance".into()));
    }
    let store = TimelineStore::from_canonical_json(&read(&dir.join("timeline.json"))?)?;
    Ok(Variant {
        name: name.into(),
        provenance,
        plan,
        store,
    })
}

/// Names of the complete variants stored under the project, sorted.
pub fn list_variants(project: &Path) -> Result<Vec<String>, VariantError> {
    let root = project.join(DIR);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&root).map_err(io_err(&root))? {
        let entry = entry.map_err(io_err(&root))?;
        if entry.path().join("variant.json").is_file() {
            if let Some(n) = entry.file_name().to_str() {
                if validate_variant_name(n).is_ok() {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Removes a variant's directory (its documents only; renders are not owned here).
pub fn delete_variant(project: &Path, name: &str) -> Result<(), VariantError> {
    validate_variant_name(name)?;
    let dir = variant_dir(project, name);
    if !dir.join("variant.json").is_file() {
        return Err(VariantError::NotFound(name.into()));
    }
    fs::remove_dir_all(&dir).map_err(io_err(&dir))
}

/// What [`promote_variant`] recorded on the main timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub variant: String,
    pub applied: AppliedGroup,
    pub removed_video_clips: usize,
    pub removed_audio_clips: usize,
    /// Titles are kept as they were; their frames are not retimed.
    pub titles_kept: usize,
}

/// Replaces the main timeline's cut structure (V1 clips and the audio clips on
/// A1 or linked to them) with the variant's, as one all-or-nothing group of
/// ordinary revisions: remove old clips, register the source, insert the
/// variant's clips. Titles and other tracks are kept. Returns a promotion with
/// zero revisions when main already has exactly the variant's cut structure.
pub fn promote_variant(
    main: &mut TimelineStore,
    variant: &Variant,
) -> Result<Promotion, VariantError> {
    let opts = RealiseOptions::default();
    let vtl = variant.store.timeline();
    let mtl = main.timeline();
    if mtl.video_tracks == vtl.video_tracks && mtl.audio_tracks == vtl.audio_tracks {
        return Ok(Promotion {
            variant: variant.name.clone(),
            applied: AppliedGroup {
                group_id: format!("promote-{}-noop", variant.name),
                first_seq: None,
                last_seq: None,
                revisions: 0,
            },
            removed_video_clips: 0,
            removed_audio_clips: 0,
            titles_kept: mtl.overlay_tracks.iter().map(|t| t.clips.len()).sum(),
        });
    }

    let mut actions = Vec::new();
    for s in &vtl.sources {
        actions.push(Action::RegisterSource { source: s.clone() });
    }
    let video_ids: Vec<String> = mtl
        .video_tracks
        .iter()
        .filter(|t| t.id == opts.video_track)
        .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
        .collect();
    let audio_ids: Vec<String> = mtl
        .audio_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(move |c| (t.id.as_str(), c)))
        .filter(|(tid, c)| {
            *tid == opts.audio_track || c.link.as_ref().is_some_and(|l| video_ids.contains(l))
        })
        .map(|(_, c)| c.id.clone())
        .collect();
    for id in &audio_ids {
        actions.push(Action::RemoveAudioClip {
            clip_id: id.clone(),
        });
    }
    for id in &video_ids {
        actions.push(Action::RemoveClip {
            clip_id: id.clone(),
        });
    }
    let build = variant.plan.group(&opts);
    actions.extend(build.actions);
    let group = ActionGroup {
        id: format!("promote-{}-{}", variant.name, &build.id["autocut-".len()..]),
        label: format!("Promote variant {:?} to main", variant.name),
        video_clip_ids: build.video_clip_ids,
        audio_clip_ids: build.audio_clip_ids,
        actions,
    };
    let titles_kept = mtl.overlay_tracks.iter().map(|t| t.clips.len()).sum();
    let applied = apply_group(main, &group)?;
    Ok(Promotion {
        variant: variant.name.clone(),
        applied,
        removed_video_clips: video_ids.len(),
        removed_audio_clips: audio_ids.len(),
        titles_kept,
    })
}

/// Undoes a promotion; it must still be the latest edit on `main`.
pub fn undo_promotion(main: &mut TimelineStore, p: &Promotion) -> Result<(), VariantError> {
    Ok(undo_group(main, &p.applied)?)
}
