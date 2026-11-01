//! External microphone alignment command.
//!
//! Registration remains `source.add`; this command verifies both immutable
//! sources, aligns their decoded PCM, and records a source-level timeline
//! attachment before any editorial cut is applied.

use std::path::Path;

use rightkit_timeline::{Action, MicAttachment, Timeline, TimelineStore};
use rightkit_video_project::audio_sync::{align_files, SAMPLE_RATE};
use rightkit_video_project::source::verify_record;
use rightkit_video_project::{Access, Project, ProjectError};
use serde_json::{json, Value};

use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::media_tools;

pub const METHODS: &[&str] = &["mic.align"];

/// Copy parent source metadata and external-mic mappings into a derived
/// timeline before it is persisted or rendered. Derived timelines may contain
/// only a subset of parent clips, but source-level mappings remain valid for
/// every retained camera occurrence.
pub fn inherit_attachments(
    parent: &Timeline,
    derived: &mut TimelineStore,
) -> Result<(), ProjectError> {
    for attachment in &parent.mic_attachments {
        let used = derived.timeline().video_tracks.iter().any(|track| {
            track
                .clips
                .iter()
                .any(|clip| clip.source == attachment.camera)
        }) || derived.timeline().audio_tracks.iter().any(|track| {
            track
                .clips
                .iter()
                .any(|clip| clip.source == attachment.camera)
        });
        if !used {
            continue;
        }
        for id in [&attachment.camera, &attachment.microphone] {
            let source = parent
                .source(id)
                .ok_or_else(|| bad(format!("mic attachment references missing source {id}")))?
                .clone();
            if let Some(existing) = derived.timeline().source(id) {
                if existing != &source {
                    return Err(bad(format!(
                        "derived timeline source {id} differs from parent"
                    )));
                }
            } else {
                derived.apply(Action::RegisterSource { source })?;
            }
        }
        derived.apply(Action::AttachMicrophone {
            attachment: attachment.clone(),
        })?;
    }
    Ok(())
}

fn bad(message: impl Into<String>) -> ProjectError {
    ProjectError::BadInput(message.into())
}

fn source<'a>(
    p: &'a Project,
    selector: &str,
    label: &str,
) -> Result<&'a rightkit_video_project::SourceRecord, ProjectError> {
    let mut matches = p
        .meta
        .sources
        .iter()
        .filter(|record| record.id.as_str() == selector || record.name == selector);
    let first = matches
        .next()
        .ok_or_else(|| bad(format!("no {label} source {selector:?} in this project")))?;
    if matches.next().is_some() {
        return Err(bad(format!(
            "{label} source name {selector:?} is ambiguous; use its BLAKE3 id"
        )));
    }
    Ok(first)
}

fn alignment_json(alignment: &rightkit_video_project::audio_sync::AudioAlignment) -> Value {
    json!({
        "offset": alignment.offset,
        "offset_ms": alignment.offset.to_units(1000),
        "lag_samples": alignment.lag_samples,
        "sample_rate": alignment.sample_rate,
        "confidence": alignment.confidence,
        "confidence_milli": (alignment.confidence * 1000.0).round().clamp(0.0, 1000.0) as u32,
        "overlap_samples": alignment.overlap_samples,
    })
}

/// Align external mic to camera and persist its source-level mapping.
pub fn handle_method(
    method: &str,
    params: &Value,
    env: &Env,
    dry_run: bool,
) -> Option<Result<Value, ProjectError>> {
    if method != "mic.align" {
        return None;
    }
    Some(align(params, env, dry_run))
}

fn align(params: &Value, env: &Env, dry_run: bool) -> Result<Value, ProjectError> {
    let dir = params
        .get("dir")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing param: dir"))?;
    let camera_selector = params
        .get("camera")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing param: camera"))?;
    let microphone_selector = params
        .get("microphone")
        .or_else(|| params.get("mic"))
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing param: microphone"))?;
    let max_offset_ms = params
        .get("max_offset_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    if max_offset_ms == 0 || max_offset_ms > 180_000 {
        return Err(bad("max_offset_ms must be between 1 and 180000"));
    }
    let expected_cursor = params.get("expect_cursor").and_then(Value::as_u64);
    let tools = media_tools(env)?;
    let mut p = Project::open(
        Path::new(dir),
        if dry_run { Access::Read } else { Access::Write },
    )?;
    if let Some(expected) = expected_cursor {
        let actual = p.cursor() as u64;
        if actual != expected {
            return Err(ProjectError::CursorMismatch {
                expected: expected as usize,
                actual: actual as usize,
            });
        }
    }
    let camera = source(&p, camera_selector, "camera")?;
    let microphone = source(&p, microphone_selector, "microphone")?;
    let camera_id = camera.id.clone();
    let microphone_id = microphone.id.clone();
    let camera_info = p
        .timeline()
        .source(&camera_id)
        .ok_or_else(|| bad("camera source is registered in metadata but not on timeline"))?;
    if camera_info.video.is_none() || !camera_info.has_audio {
        return Err(bad("camera source must contain video and an audio stream"));
    }
    let microphone_info = p
        .timeline()
        .source(&microphone_id)
        .ok_or_else(|| bad("microphone source is registered in metadata but not on timeline"))?;
    if !microphone_info.has_audio {
        return Err(bad("microphone source must contain an audio stream"));
    }
    let camera_file = verify_record(&p, camera)?.to_owned();
    let microphone_file = verify_record(&p, microphone)?.to_owned();
    let result = align_files(&tools, &camera_file, &microphone_file, max_offset_ms)?;
    // A source can be changed while FFmpeg reads it; the immutable-source gate
    // is checked on both sides of the typed decode.
    verify_record(&p, camera)?;
    verify_record(&p, microphone)?;
    let confidence_milli = (result.confidence * 1000.0).round().clamp(0.0, 1000.0) as u32;
    let attachment = MicAttachment {
        camera: camera_id,
        microphone: microphone_id,
        offset: result.offset,
        sample_rate: SAMPLE_RATE,
        lag_samples: result.lag_samples,
        confidence_milli,
    };
    let attach = Action::AttachMicrophone {
        attachment: attachment.clone(),
    };
    if dry_run {
        // Validate the exact action against the read-only in-memory timeline;
        // this rejects invalid pairs (including camera == microphone) while
        // preserving dry-run's zero-write guarantee.
        p.apply(attach)?;
        return Ok(json!({
            "dry_run": true,
            "status": "would_attach",
            "alignment": alignment_json(&result),
            "attachment": attachment,
            "revision": p.revision_summary(),
        }));
    }
    let outcome = p.edit(attach)?;
    let status = match outcome {
        rightkit_video_project::EditOutcome::Recorded(_) => "attached",
        rightkit_video_project::EditOutcome::NoChange => "already_attached",
        rightkit_video_project::EditOutcome::AlreadyApplied => "already_attached",
    };
    Ok(json!({
        "status": status,
        "alignment": alignment_json(&result),
        "attachment": attachment,
        "revision": p.revision_summary(),
    }))
}
