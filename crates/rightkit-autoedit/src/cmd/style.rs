//! `style.measure {dir, source?, preset?}`: measures an editing style from a
//! project's finished reference edit (#93).
//!
//! The reference edit is the source's time map written by `autoedit.apply`; its
//! kept segments give the kept source ranges. The transcript supplies the words.
//! Without `source`, the project must have exactly one source with an edit.

use std::path::Path;

use crate::cuts::{EditingStyle, Preset as CutPreset, TimeMap};
use rightkit_timeline::TICKS_PER_SECOND;
use rightkit_video_project::{Access, Project, ProjectError};
use serde_json::{json, Value};

use rightkit_engine_host::project::{bad, find_source, load_transcript, read_json, timemap_path};

fn parse_preset(name: Option<&str>) -> Result<CutPreset, ProjectError> {
    match name.unwrap_or("natural") {
        "tight" => Ok(CutPreset::Tight),
        "natural" => Ok(CutPreset::Natural),
        "punchy" => Ok(CutPreset::Punchy),
        other => Err(bad(format!(
            "preset must be tight, natural or punchy, not {other:?}"
        ))),
    }
}

fn ticks_to_ms(ticks: i64) -> i64 {
    ticks * 1000 / TICKS_PER_SECOND
}

pub fn m_style_measure(dir: &Path, params: &Value) -> Result<Value, ProjectError> {
    let p = Project::open(dir, Access::Read)?;
    let base = parse_preset(params.get("preset").and_then(Value::as_str))?;

    let source_id = match params.get("source").and_then(Value::as_str) {
        Some(s) => find_source(&p, s)?.id.as_str().to_string(),
        None => {
            let edited: Vec<String> = p
                .meta
                .sources
                .iter()
                .map(|s| s.id.as_str().to_string())
                .filter(|id| timemap_path(&p.dir, id).is_file())
                .collect();
            match edited.as_slice() {
                [one] => one.clone(),
                [] => return Err(bad("no finished reference edit: run autoedit.apply first")),
                _ => return Err(bad("several sources have edits: pass source")),
            }
        }
    };

    let time_map: TimeMap = read_json(&timemap_path(&p.dir, &source_id))?;
    let kept_ms: Vec<(i64, i64)> = time_map
        .segments
        .iter()
        .map(|s| (ticks_to_ms(s.source_in.0), ticks_to_ms(s.source_out.0)))
        .collect();
    let transcript = load_transcript(&p, &source_id)?;

    let (style, measurement) =
        EditingStyle::measure(&transcript, &kept_ms, base).map_err(|e| bad(e.to_string()))?;
    Ok(json!({
        "source": source_id,
        "style": style,
        "measurement": measurement,
    }))
}
