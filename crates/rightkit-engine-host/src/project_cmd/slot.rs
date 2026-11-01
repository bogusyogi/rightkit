//! Card slot media: registered video or a materialised still, with revisioned undo.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rightkit_timeline::{Action, SlotFill, SlotRect, SourceId, Ticks, TICKS_PER_SECOND};
use rightkit_video_project::source::{hash_file, register_in_place, verify_record};
use rightkit_video_project::still::still_to_clip;
use rightkit_video_project::{Access, Project, ProjectError};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::env::Env;
use crate::project::{bad, media_tools};

pub fn m_slot_fill(
    dir: &Path,
    params: &Value,
    env: &Env,
    dry: bool,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        card_id: String,
        image: Option<PathBuf>,
        source: Option<SourceId>,
        source_in_s: Option<f64>,
        rect: Option<SlotRect>,
        #[serde(default)]
        clear: bool,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    if q.clear {
        if q.image.is_some() || q.source.is_some() || q.source_in_s.is_some() || q.rect.is_some() {
            return Err(bad(
                "clear cannot be combined with image, source, source_in_s or rect",
            ));
        }
    } else if q.image.is_some() == q.source.is_some() {
        return Err(bad("slot.fill needs exactly one of image or source"));
    }
    let rect = q.rect.unwrap_or(SlotRect::FULL);
    // Bound dimensions before validate adds them to the origin (untrusted i64s).
    if !(1..=1000).contains(&rect.width) || !(1..=1000).contains(&rect.height) {
        return Err(bad("slot rectangle dimensions must be in 1..=1000"));
    }
    rect.validate(&q.card_id)?;
    let seconds = q.source_in_s.unwrap_or(0.0);
    let ticks = seconds * TICKS_PER_SECOND as f64;
    if !seconds.is_finite() || seconds < 0.0 || ticks >= i64::MAX as f64 {
        return Err(bad(
            "source_in_s must be finite, non-negative and within the timeline range",
        ));
    }
    // Match the timeline's half-down rule at the seconds boundary.
    let rounded = if ticks.fract() > 0.5 {
        ticks.ceil()
    } else {
        ticks.floor()
    };
    let source_in = Ticks(rounded as i64);
    let mut p = Project::open(dir, if dry { Access::Read } else { Access::Write })?;
    let frames = p
        .timeline()
        .cards
        .iter()
        .find(|c| c.id == q.card_id)
        .ok_or_else(|| bad(format!("unknown card: {}", q.card_id)))?
        .frames;

    let fill = if q.clear {
        None
    } else if let Some(image) = &q.image {
        if !image.is_file() {
            return Err(ProjectError::SourceNotFound(image.display().to_string()));
        }
        if source_in >= p.meta.fps.frame_to_ticks(frames) {
            return Err(bad("slot fill starts outside the still clip"));
        }
        let (hash, _) = hash_file(image)?;
        let rel = format!("media/still-{}-{frames}f.mp4", &hash.as_str()[..16]);
        if dry {
            return Ok(json!({"dry_run":true,"would":{
                "command":"slot.fill", "card_id":q.card_id, "image":image,
                "clip":rel, "frames":frames, "source_in":source_in, "rect":rect
            }}));
        }
        let tools = media_tools(env)?;
        let out = p.dir.join(&rel);
        if !out.is_file() {
            still_to_clip(
                &tools,
                image,
                &out,
                p.meta.fps,
                frames,
                Arc::new(AtomicBool::new(false)),
            )?;
            if hash_file(image)?.0 != hash {
                let _ = std::fs::remove_file(&out);
                return Err(ProjectError::SourceChanged(image.display().to_string()));
            }
        }
        let source = register_in_place(&mut p, &tools, &rel, None)?.info.id;
        Some(SlotFill {
            source,
            source_in,
            rect,
        })
    } else {
        let source = q.source.expect("validated source or image");
        let record = p
            .source_record(&source)
            .ok_or_else(|| bad(format!("source is not registered: {source}")))?;
        let info = p
            .timeline()
            .source(&source)
            .ok_or_else(|| bad(format!("source is not registered: {source}")))?;
        if info.video.is_none() {
            return Err(bad("slot fill source has no video stream"));
        }
        let duration = info
            .video
            .as_ref()
            .and_then(|v| v.duration)
            .unwrap_or(info.duration);
        if source_in >= duration {
            return Err(bad("slot fill starts outside the source"));
        }
        verify_record(&p, record)?;
        Some(SlotFill {
            source,
            source_in,
            rect,
        })
    };
    let action = Action::SetSlotFill {
        card_id: q.card_id.clone(),
        fill: fill.clone(),
    };
    if dry {
        p.apply(action)?;
        return Ok(json!({"dry_run":true,"would":{
            "command":"slot.fill", "card_id":q.card_id, "fill":fill
        }}));
    }
    let outcome = p.edit(action)?;
    Ok(json!({
        "status":outcome.as_str(), "card_id":q.card_id, "fill":fill,
        "revision":p.revision_summary()
    }))
}
