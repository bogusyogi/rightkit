//! Shared helpers for recording the three asset facts (origin, evidence, rights) for files that
//! are not built by `descriptor::build`: imported files (`import`), pipeline run outputs and
//! supplied reference material. Nothing here guesses: a parent with no recorded facts reads
//! `unknown`, and unknown rights are never promoted to `cleared`.

use crate::host::asset_facts::{AssetFacts, EvidenceKind, Origin, OriginKind, Rights};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{store, Context, GenError, GenResult};

/// One input of a derived asset: its id (a library asset id, a run step asset id, or
/// `file:<path>` for a file the host holds no record of) and its recorded facts.
#[derive(Debug, Clone)]
pub struct Parent {
    pub id: String,
    pub facts: AssetFacts,
}

fn plain_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', '.'])
}

/// The library asset id when `path` is `<library>/<asset_id>/...` (lexically or after
/// canonicalising both sides) and that folder exists.
fn library_asset_id(ctx: &Context, path: &Path) -> Option<String> {
    let lib = ctx.library_dir();
    let lib_canon = lib.canonicalize().ok();
    let candidates = [Some(path.to_path_buf()), path.canonicalize().ok()];
    for candidate in candidates.into_iter().flatten() {
        for root in std::iter::once(&lib).chain(lib_canon.as_ref()) {
            if let Ok(rest) = candidate.strip_prefix(root) {
                if let Some(first) = rest.components().next() {
                    let id = first.as_os_str().to_string_lossy().into_owned();
                    if plain_id(&id) && root.join(&id).is_dir() {
                        return Some(id);
                    }
                }
            }
        }
    }
    None
}

/// A library asset as a parent; no recorded facts (an older asset) read `unknown`.
pub fn parent_of_asset_id(ctx: &Context, asset_id: &str) -> Parent {
    let facts = store::open(ctx)
        .ok()
        .and_then(|db| store::asset_facts(&db, asset_id).ok())
        .map(|(facts, _)| facts)
        .unwrap_or_default();
    Parent {
        id: asset_id.to_string(),
        facts,
    }
}

/// A file as a parent: a library file carries its asset's facts; any other file is unknown.
pub fn parent_of_path(ctx: &Context, path: &Path) -> Parent {
    match library_asset_id(ctx, path) {
        Some(id) => parent_of_asset_id(ctx, &id),
        None => Parent {
            id: format!("file:{}", path.to_string_lossy()),
            facts: AssetFacts::unknown(),
        },
    }
}

/// A recorded asset value (a pipeline step asset or a job-result asset) as a parent. Its own
/// `facts` win; else a library asset's recorded facts; else its path; else unknown.
pub fn parent_of_recorded(ctx: &Context, asset: &Value) -> Parent {
    let id = asset["asset_id"].as_str().unwrap_or_default();
    if asset["facts"].is_object() {
        let id = if id.is_empty() {
            "unknown".to_string()
        } else {
            id.to_string()
        };
        return Parent {
            id,
            facts: AssetFacts::from_value(&asset["facts"]),
        };
    }
    if plain_id(id) && ctx.library_dir().join(id).is_dir() {
        return parent_of_asset_id(ctx, id);
    }
    match asset["path"].as_str().filter(|p| !p.is_empty()) {
        Some(path) => parent_of_path(ctx, Path::new(path)),
        None => Parent {
            id: if id.is_empty() {
                "unknown".to_string()
            } else {
                id.to_string()
            },
            facts: AssetFacts::unknown(),
        },
    }
}

/// Pipeline native steps that only transform their parents' material, by what their code does.
/// Their output's evidence is the parents' evidence (see [`pipeline_evidence`]):
/// - `transcribe`: the speech recogniser reads the parent audio into subtitle cues; no new scene.
/// - `still_clip`: a Ken Burns pan and zoom over the given still; no new frames.
/// - `compose_video`: concatenates, scales, pads, mixes, burns subtitles and normalises loudness
///   over the parents. It creates no media of its own, so a generated parent carries its
///   `illustrative` evidence into the cut, and an all-authentic set stays `authentic`.
/// - `transcode`, `export`: re-encode or re-package existing material. The pipeline stamps neither
///   today (export and plan steps stay unstamped), so they are named here for any future caller.
const PIPELINE_TRANSFORMING: &[&str] = &[
    "transcribe",
    "still_clip",
    "compose_video",
    "transcode",
    "export",
    "dub_plan",
];

/// Pipeline native steps that synthesise new content, by what their code does:
/// - `translate`: a translation provider writes text the parents did not contain.
///
/// Their output is `illustrative` whatever the parents, as a synthesised job kind is.
const PIPELINE_SYNTHESISING: &[&str] = &["translate"];

/// Evidence of a pipeline output, from the `native:<action>` model name that
/// `pipeline_workflows::stamp_pipeline_facts` stamps with the parents.
///
/// | step                       | class        | evidence                                                  |
/// |----------------------------|--------------|-----------------------------------------------------------|
/// | transforming (table above) | transforming | `authentic` if every parent is authentic; `illustrative` if any parent is; else `unknown` |
/// | transforming, no parents   | transforming | `unknown` (nothing establishes the input was authentic)   |
/// | synthesising (table above) | synthesising | `illustrative`, whatever the parents                      |
/// | any other action, or a model not `native:<action>` | not named | `unknown`, never guessed as either meaning |
///
/// Transforming evidence matches `crate::host::asset_facts::job_evidence` for transforming
/// job kinds, so a pure transcode of authentic material stays `authentic`.
fn pipeline_evidence(model: &str, parents: &[Parent]) -> EvidenceKind {
    let Some(action) = model.strip_prefix("native:") else {
        return EvidenceKind::Unknown;
    };
    if PIPELINE_SYNTHESISING.contains(&action) {
        return EvidenceKind::Illustrative;
    }
    if !PIPELINE_TRANSFORMING.contains(&action) || parents.is_empty() {
        return EvidenceKind::Unknown;
    }
    if parents
        .iter()
        .all(|p| p.facts.evidence == EvidenceKind::Authentic)
    {
        EvidenceKind::Authentic
    } else if parents
        .iter()
        .any(|p| p.facts.evidence == EvidenceKind::Illustrative)
    {
        EvidenceKind::Illustrative
    } else {
        EvidenceKind::Unknown
    }
}

/// Facts of an asset the host itself produced from `parents` (a pipeline compose, a transcode).
/// Origin is `generated` by the named tool with the parents listed; rights are inherited from the
/// parents (cleared only when every parent is cleared, unknown when any parent is unknown).
/// Evidence follows the step's kind per `pipeline_evidence`. Rights are never changed by it.
pub fn derived_facts(provider: &str, model: &str, job_id: &str, parents: &[Parent]) -> AssetFacts {
    let mut ids: Vec<String> = Vec::new();
    for p in parents {
        if !ids.contains(&p.id) {
            ids.push(p.id.clone());
        }
    }
    let rights: Vec<Rights> = parents.iter().map(|p| p.facts.rights.clone()).collect();
    let evidence = pipeline_evidence(model, parents);
    AssetFacts::generated(provider, model, job_id, ids, &rights, evidence)
}

/// Facts of a file the user supplied: the given origin kind, the source it came from, and only the
/// evidence and rights the caller actually knows (`unknown` when not stated).
pub fn recorded_facts(
    kind: OriginKind,
    source: Option<String>,
    evidence: EvidenceKind,
    rights: Rights,
) -> AssetFacts {
    AssetFacts {
        origin: Origin {
            kind,
            source,
            ..Origin::default()
        },
        evidence,
        rights,
    }
}

/// Write the `facts.json` the gallery scan reads beside a library asset (`<dir>/facts.json`).
/// `extra` keys are merged at the top level beside `facts` and `scope`.
pub fn write_facts_file(
    dir: &Path,
    facts: &AssetFacts,
    scope: &Value,
    extra: Option<Value>,
) -> GenResult<()> {
    let mut body = json!({"facts": facts.to_value(), "scope": scope});
    if let (Some(extra), Some(object)) = (
        extra.as_ref().and_then(Value::as_object),
        body.as_object_mut(),
    ) {
        for (k, v) in extra {
            if k != "facts" && k != "scope" {
                object.insert(k.clone(), v.clone());
            }
        }
    }
    rightkit_fs::atomic_write(&dir.join("facts.json"), body.to_string().as_bytes())
        .map_err(|e| GenError::new("io", format!("cannot write facts.json: {e}")))
}

/// Facts of material the user supplied whose evidence and rights nothing establishes: origin
/// `supplied` from `source`, evidence `unknown`, rights `unknown`. `consent_refs` are references
/// to consent the caller recorded (a likeness or voice consent record); they are carried as
/// stated and never raise the status above `unknown`.
pub fn supplied_unknown(source: Option<String>, consent_refs: Vec<String>) -> AssetFacts {
    recorded_facts(
        OriginKind::Supplied,
        source,
        EvidenceKind::Unknown,
        Rights {
            consent_refs,
            ..Rights::default()
        },
    )
}

/// `<file>.facts.json` beside `file`: the file name with `.facts.json` appended. Written by
/// [`write_sibling_facts`], read by [`crate::gallery`].
pub fn sibling_facts_path(file: &Path) -> PathBuf {
    let mut name = file
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".facts.json");
    file.with_file_name(name)
}

/// Write `facts` as `<file>.facts.json` beside a file that is not in the library (the gallery
/// reads only the library's per-folder `facts.json`). Returns the sidecar path.
pub fn write_sibling_facts(file: &Path, facts: &AssetFacts) -> GenResult<PathBuf> {
    let path = sibling_facts_path(file);
    rightkit_fs::atomic_write(
        &path,
        json!({"facts": facts.to_value(), "scope": Value::Null})
            .to_string()
            .as_bytes(),
    )
    .map_err(|e| GenError::new("io", format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}
