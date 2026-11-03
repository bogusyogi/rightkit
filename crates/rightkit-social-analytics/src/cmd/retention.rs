//! Retention feedback: `retention.import`, `retention.list`,
//! `retention.proposals`, `retention.apply`.
//!
//! An imported audience-retention CSV is bound to ONE exact render: the render
//! must appear in `renders.list`, its BLAKE3 hash is recomputed at import (and
//! re-verified at apply — a changed file is rejected), and the timeline
//! revision plus an output→source time map are recorded alongside. Drop-off
//! points in the CSV are mapped through that time map to source/timeline
//! positions and become reviewable edit proposals. Applying executes the cuts
//! as one canonical timeline revision (`Action::Batch` of `RippleDeleteRange`)
//! and writes a traceable revised-render request the render lane can consume.
//!
//! A render exported from a variant (a short or alternate cut) is applied to a
//! CHILD variant rebuilt from the exact exported revision of its parent. Each
//! drop's edges first snap to the nearest clause end within 3 s (the original
//! window and the snapped span are recorded on the candidate). The snapped drop
//! is then checked against the transcript: only a window that covers whole
//! spoken clauses on word-aligned edges is cut. Any other drop is kept as a
//! non-destructive proposal (`revised_opening`, `broll` or `no_change`) with its
//! reason. The parent, its siblings and the main timeline are never written.

use std::path::{Path, PathBuf};

use rightkit_timeline::{Action, Ticks, TimelineStore};
use rightkit_video_project::{Access, Project, ProjectError};
use serde::Deserialize;
use serde_json::{json, Value};

use rightkit_engine_host::project::{
    analysis_dir, bad, clip_ranges, io_err, load_transcript, read_json,
    renders_list as m_renders_list, write_json,
};
use rightkit_transcript::Transcript;

const SCHEMA: &str = "cutright.retention.v1";
const REQ_SCHEMA: &str = "cutright.render-request.v1";

/// Silence (ms) between two words that ends a spoken clause.
const CLAUSE_GAP_MS: i64 = 450;
/// Drops starting within this many output ms are opening drops; drops starting
/// within this many ms of the end are closing drops.
const OPENING_MS: i64 = 3_000;

/// Default drop threshold: a point-to-point fall of at least this many
/// retention points proposes a cut.
const DROP_MIN_POINTS: f64 = 4.0;
/// Handles (ms) kept around a drop-off cut.
const CUT_PAD_MS: i64 = 250;
/// Shortest run of kept footage (output ms) a cut may leave beside it.
const MIN_KEPT_MS: i64 = 1_000;
/// Radius (output ms) within which a candidate edge snaps to a clause end.
const SNAP_MS: i64 = 3_000;

fn retention_path(dir: &Path, name: &str) -> PathBuf {
    analysis_dir(dir).join(format!("retention-{name}.json"))
}

fn request_path(dir: &Path, name: &str) -> PathBuf {
    analysis_dir(dir).join(format!("render-request-{name}.json"))
}

fn load_retention(dir: &Path, name: &str) -> Result<Value, ProjectError> {
    let path = retention_path(dir, name);
    if !path.is_file() {
        return Err(bad(format!(
            "no retention import {name:?}; run retention.import first"
        )));
    }
    read_json(&path)
}

/// Finds `render` in renders.list and returns its record plus the file hash.
/// When the record carries a `hash` field (render-lane receipts), a mismatch
/// is a hard error: the CSV must bind to the exact exported bytes.
fn bound_render(dir: &Path, render: &str) -> Result<(Value, String), ProjectError> {
    let list = m_renders_list(dir)?;
    let rec = list["renders"]
        .as_array()
        .and_then(|rs| rs.iter().find(|r| r["name"].as_str() == Some(render)))
        .cloned()
        .ok_or_else(|| {
            bad(format!(
                "no render {render:?} in this project; render first"
            ))
        })?;
    let path = rec["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| bad("render record has no path"))?;
    let hash = rightkit_fs::digest::blake3_file(&path)
        .map_err(io_err(format!("hashing {}", path.display())))?;
    // Render-lane receipt (record.output_blake3) or a flat hash field.
    let expected = rec["record"]["output_blake3"]
        .as_str()
        .or_else(|| rec["hash"].as_str());
    // An export without a render record has no revision/time-map identity:
    // binding it would silently fall back to whatever the timeline is now.
    let expected = expected.ok_or_else(|| {
        bad(format!(
            "render {render:?} has no cutright.render.v1 record; re-render it to bind retention"
        ))
    })?;
    if expected != hash {
        return Err(bad(format!(
            "render {render:?} changed since it was exported (recorded hash {expected}, file now {hash}); refusing to bind retention"
        )));
    }
    Ok((rec, hash))
}

/// Output→source map for a render: prefers the render record's `time_map`
/// (lane render's receipt); falls back to the current timeline's clip ranges.
/// One output→source segment: `(out_start_ms, out_end_ms, source,
/// source_in_ms, source_out_ms, clip_id)`.
type OutSeg = (i64, i64, String, i64, i64, Option<String>);

/// One render time-map row as an output→source segment.
fn out_seg(s: &Value) -> Option<OutSeg> {
    // Render-lane rows use out_in_ms/out_end_ms; accept the
    // out_start_ms/out_end_ms spelling too.
    let start = s["out_in_ms"]
        .as_i64()
        .or_else(|| s["out_start_ms"].as_i64())?;
    let end = s["out_end_ms"].as_i64()?;
    Some((
        start,
        end,
        s["source"].as_str()?.to_string(),
        s["source_in_ms"].as_i64()?,
        s["source_out_ms"].as_i64()?,
        s["clip_id"].as_str().map(String::from),
    ))
}

/// Video segments of a render record's time map. Audio rows repeat the same
/// output span, so the clause check reads video rows only.
fn video_segments(rec: &Value) -> Vec<OutSeg> {
    rec["record"]["time_map"]
        .as_array()
        .map(|tm| {
            tm.iter()
                .filter(|s| s["kind"].as_str() == Some("video"))
                .filter_map(out_seg)
                .collect()
        })
        .unwrap_or_default()
}

fn time_map_for(p: &Project, rec: &Value) -> (Vec<OutSeg>, &'static str) {
    let tm_val = rec["record"]["time_map"]
        .as_array()
        .or_else(|| rec["time_map"].as_array());
    if let Some(tm) = tm_val {
        let segs: Vec<_> = tm.iter().filter_map(out_seg).collect();
        if !segs.is_empty() {
            return (segs, "render_record");
        }
    }
    let mut segs = Vec::new();
    for (source, ranges) in clip_ranges(p.timeline()) {
        for r in ranges {
            let len = (r.out_ms - r.in_ms) as i64;
            segs.push((
                r.out_off_ms as i64,
                r.out_off_ms as i64 + len,
                source.clone(),
                r.in_ms as i64,
                r.out_ms as i64,
                Some(r.clip_id.clone()),
            ));
        }
    }
    segs.sort_by_key(|s| s.0);
    (segs, "timeline")
}

/// Maps an output-ms window onto source positions.
fn map_window(tm: &[OutSeg], out_start: i64, out_end: i64) -> Vec<Value> {
    tm.iter()
        .filter(|s| out_start < s.1 && out_end > s.0)
        .map(|s| {
            let in_at = |out: i64| (s.3 + (out - s.0).clamp(0, s.1 - s.0)).clamp(s.3, s.4);
            let mut m = json!({
                "source": s.2,
                "source_in_ms": in_at(out_start.max(s.0)),
                "source_out_ms": in_at(out_end.min(s.1)),
                "out_start_ms": out_start.max(s.0),
                "out_end_ms": out_end.min(s.1),
            });
            if let Some(cid) = &s.5 {
                m["clip_id"] = json!(cid);
            }
            m
        })
        .collect()
}

/// Parses `time,retention` CSV rows into `(out_ms, retention)` points.
fn parse_rows(csv: &str) -> Result<Vec<(i64, f64)>, ProjectError> {
    let mut rows = Vec::new();
    for (n, line) in csv.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (n == 0 && line.to_lowercase().starts_with("time")) {
            continue;
        }
        let mut parts = line.split([',', '\t']);
        let t: f64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| bad(format!("retention row {}: bad time", n + 1)))?;
        let r: f64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| bad(format!("retention row {}: bad value", n + 1)))?;
        rows.push(((t * 1000.0).round() as i64, r));
    }
    if rows.is_empty() {
        return Err(bad("retention CSV has no rows"));
    }
    rows.sort_by_key(|r| r.0);
    Ok(rows)
}

/// Point-to-point falls of at least `drop_min` points → output-ms cut windows.
fn drop_offs(rows: &[(i64, f64)], drop_min: f64) -> Vec<(i64, i64, f64)> {
    let mut out = Vec::new();
    for w in rows.windows(2) {
        let delta = w[1].1 - w[0].1;
        if delta <= -drop_min {
            let (mut s, mut e) = (w[0].0, w[1].0);
            if e <= s {
                e = s + 1;
            }
            // Keep a short handle around the drop.
            s = (s - CUT_PAD_MS).max(0);
            e += CUT_PAD_MS;
            out.push((s, e, -delta));
        }
    }
    out
}

/// Builds the stored document's drop-off proposals for a bound render.
fn proposals_for(
    rec: &Value,
    p: &Project,
    rows: &[(i64, f64)],
    drop_min: f64,
) -> (Vec<Value>, &'static str) {
    let (tm, origin) = time_map_for(p, rec);
    let props: Vec<Value> = drop_offs(rows, drop_min)
        .into_iter()
        .enumerate()
        .map(|(i, (s, e, drop))| {
            json!({
                "id": format!("drop-{}", i + 1),
                "out_start_ms": s,
                "out_end_ms": e,
                "drop_points": (drop * 10.0).round() / 10.0,
                "suggested": "cut",
                "source_mappings": map_window(&tm, s, e),
            })
        })
        .collect();
    (props, origin)
}

pub fn m_retention_import(
    dir: &Path,
    params: &Value,
    dry_run: bool,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        name: Option<String>,
        render: String,
        csv: String,
        drop_min: Option<f64>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let rows = parse_rows(&q.csv)?;
    let p = Project::open(dir, Access::Read)?;
    let (rec, hash) = bound_render(dir, &q.render)?;
    let name = q.name.unwrap_or_else(|| {
        q.render.replace(
            |c: char| !(c.is_alphanumeric() || c == '-' || c == '_'),
            "_",
        )
    });
    let revision = rec["record"]["origin"]["timeline_cursor"]
        .as_i64()
        .or_else(|| rec["revision"].as_i64())
        .or_else(|| rec["cursor"].as_i64())
        .unwrap_or_else(|| p.revision_summary()["cursor"].as_i64().unwrap_or(0));
    let drop_min = q.drop_min.unwrap_or(DROP_MIN_POINTS);
    let (proposals, tm_origin) = proposals_for(&rec, &p, &rows, drop_min);
    let doc = json!({
        "schema": SCHEMA,
        "name": name,
        "render": q.render,
        "render_path": rec["path"],
        "render_hash": hash,
        "revision": revision,
        "time_map_origin": tm_origin,
        "drop_min": drop_min,
        "rows": rows.iter().map(|(ms, r)| json!({"out_ms": ms, "retention": r})).collect::<Vec<_>>(),
        "drop_offs": proposals,
    });
    if !dry_run {
        write_json(&retention_path(dir, &name), &doc)?;
    }
    Ok(json!({
        "status": "imported",
        "dry_run": dry_run,
        "name": name,
        "render": q.render,
        "render_hash": hash,
        "revision": revision,
        "points": rows.len(),
        "drop_offs": doc["drop_offs"].as_array().map(|d| d.len()).unwrap_or(0),
        "time_map_origin": tm_origin,
    }))
}

pub fn m_retention_list(dir: &Path) -> Result<Value, ProjectError> {
    let root = analysis_dir(dir);
    let mut out = Vec::new();
    if root.is_dir() {
        for entry in std::fs::read_dir(&root)
            .map_err(io_err("reading analysis".into()))?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("retention-") && name.ends_with(".json") {
                if let Ok(doc) = read_json::<Value>(&entry.path()) {
                    out.push(json!({
                        "name": doc["name"],
                        "render": doc["render"],
                        "render_hash": doc["render_hash"],
                        "revision": doc["revision"],
                        "points": doc["rows"].as_array().map(|r| r.len()).unwrap_or(0),
                        "drop_offs": doc["drop_offs"].as_array().map(|d| d.len()).unwrap_or(0),
                    }));
                }
            }
        }
    }
    Ok(json!({"retention": out}))
}

/// The stored proposals for one import (recomputed when the render record
/// gains a better time map).
pub fn m_retention_proposals(
    dir: &Path,
    params: &Value,
    dry_run: bool,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        name: String,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let mut doc = load_retention(dir, &q.name)?;
    let p = Project::open(dir, Access::Read)?;
    // Re-bind: the render must still exist and still hash identically.
    let (rec, hash) = bound_render(dir, doc["render"].as_str().unwrap_or_default())?;
    if doc["render_hash"].as_str() != Some(hash.as_str()) {
        return Err(bad(format!(
            "render {:?} changed since import; re-import the retention CSV",
            doc["render"]
        )));
    }
    let rows: Vec<(i64, f64)> = doc["rows"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| Some((r["out_ms"].as_i64()?, r["retention"].as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    let drop_min = doc["drop_min"].as_f64().unwrap_or(DROP_MIN_POINTS);
    let (props, origin) = proposals_for(&rec, &p, &rows, drop_min);
    doc["drop_offs"] = json!(props);
    doc["time_map_origin"] = json!(origin);
    if !dry_run {
        write_json(&retention_path(dir, &q.name), &doc)?;
    }
    Ok(json!({
        "name": q.name,
        "dry_run": dry_run,
        "render": doc["render"],
        "revision": doc["revision"],
        "time_map_origin": origin,
        "drop_offs": doc["drop_offs"],
    }))
}

/// Applies selected drop-off cuts to the timeline as ONE revision and writes
/// a revised-render request. Rejects when the bound render no longer matches
/// the imported hash.
pub fn m_retention_apply(dir: &Path, params: &Value, dry_run: bool) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        name: String,
        drops: Option<Vec<String>>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let doc = load_retention(dir, &q.name)?;
    let render = doc["render"].as_str().unwrap_or_default().to_string();
    // Hard binding check: the export must be byte-identical to import time.
    let (rec, hash) = bound_render(dir, &render)?;
    let origin = rec["record"]["origin"].clone();
    let kind = origin["kind"].as_str().unwrap_or("unknown").to_string();
    if kind != "main" && kind != "variant" {
        return Err(bad(format!(
            "render {render:?} is a {kind} export; its retention cannot cut the main timeline"
        )));
    }
    if doc["render_hash"].as_str() != Some(hash.as_str()) {
        return Err(bad(format!(
            "render {render:?} changed since retention import; refusing to apply stale analysis"
        )));
    }
    let wanted: Option<std::collections::BTreeSet<&str>> = q
        .drops
        .as_ref()
        .map(|d| d.iter().map(String::as_str).collect());
    let drops: Vec<&Value> = doc["drop_offs"]
        .as_array()
        .map(|ds| {
            ds.iter()
                .filter(|d| {
                    wanted
                        .as_ref()
                        .is_none_or(|w| w.contains(d["id"].as_str().unwrap_or_default()))
                })
                .collect()
        })
        .unwrap_or_default();
    if drops.is_empty() {
        return Err(bad("no drop-offs selected"));
    }
    if kind == "variant" {
        return apply_variant(dir, &q.name, &render, &origin, &rec, &hash, &drops, dry_run);
    }
    // Output ranges are in the exported revision's time, so they are only
    // valid while the timeline still is that revision.
    let p_now = Project::open(dir, Access::Read)?;
    let head_now = p_now.revision_summary()["head_hash"].clone();
    if origin["timeline_head_hash"].is_null() || origin["timeline_head_hash"] != head_now {
        return Err(bad(format!(
            "timeline changed since render {render:?} was exported; re-render and re-import retention before applying"
        )));
    }
    // Every candidate is screened against the transcript and the kept-length
    // rule before any cut is applied; refused candidates are listed, not cut.
    let segs = main_segments(&p_now, &rec);
    let total_ms = segs.iter().map(|g| g.1).max().unwrap_or(0);
    let screened = screen(&p_now, &segs, &drops, total_ms)?;
    drop(p_now);
    let windows = cut_windows(&screened.safe);
    let cuts = windows.len();
    let outcome = overall_outcome(&screened.decisions);
    let refused = refused_list(&screened.decisions);
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {"command": "retention.apply", "name": q.name, "cuts": cuts},
            "render": render,
            "outcome": outcome,
            "decisions": screened.decisions,
            "refused": refused,
        }));
    }
    if cuts == 0 {
        return Ok(json!({
            "status": "proposed",
            "name": q.name,
            "render": render,
            "outcome": outcome,
            "cuts": 0,
            "decisions": screened.decisions,
            "refused": refused,
        }));
    }
    let actions: Vec<Action> = windows
        .iter()
        .map(|&(s, e)| Action::RippleDeleteRange {
            start: Ticks::from_millis(s),
            end: Ticks::from_millis(e),
        })
        .collect();
    let mut p = Project::open(dir, Access::Write)?;
    p.edit(Action::Batch { actions })
        .map_err(|e| bad(format!("retention cut failed: {e}")))?;
    p.save_timeline()?;
    let revision = p.revision_summary();
    let request = json!({
        "schema": REQ_SCHEMA,
        "reason": format!("retention drop-off cuts from {}", q.name),
        "base_render": render,
        "base_revision": doc["revision"],
        "applied_revision": revision,
        "cuts": windows
            .iter()
            .map(|&(s, e)| json!({"out_start_ms": s, "out_end_ms": e}))
            .collect::<Vec<_>>(),
    });
    write_json(&request_path(dir, &q.name), &request)?;
    Ok(json!({
        "status": "applied",
        "name": q.name,
        "cuts": cuts,
        "revision": revision,
        "outcome": outcome,
        "decisions": screened.decisions,
        "refused": refused,
        "render_request": request_path(dir, &q.name).display().to_string(),
    }))
}

/// Retention on a variant-origin export (a short or alternate cut). The export's
/// own revision is rebuilt from its parent variant; each selected drop is checked
/// against the transcript; the safe cuts go into a NEW child variant whose history
/// starts at that revision. Held drops are returned as proposals with reasons.
#[allow(clippy::too_many_arguments)]
fn apply_variant(
    dir: &Path,
    retention: &str,
    render: &str,
    origin: &Value,
    rec: &Value,
    hash: &str,
    drops: &[&Value],
    dry_run: bool,
) -> Result<Value, ProjectError> {
    let parent_name = origin["name"]
        .as_str()
        .ok_or_else(|| bad(format!("render {render:?} records no parent variant")))?
        .to_string();
    let export_head = origin["timeline_head_hash"]
        .as_str()
        .ok_or_else(|| bad(format!("render {render:?} records no timeline head")))?
        .to_string();
    let export_cursor = origin["timeline_cursor"]
        .as_u64()
        .ok_or_else(|| bad(format!("render {render:?} records no timeline cursor")))?;
    let parent = rightkit_autoedit::variants::load_variant(dir, &parent_name)
        .map_err(|e| bad(e.to_string()))?;
    let mut store = store_at_export(
        &parent.store,
        &parent_name,
        export_cursor as usize,
        &export_head,
    )?;
    let segs = video_segments(rec);
    if segs.is_empty() {
        return Err(bad(format!(
            "render {render:?} has no video time map; re-render it to bind retention"
        )));
    }
    let total_ms = segs.iter().map(|g| g.1).max().unwrap_or(0);
    let p = Project::open(dir, Access::Read)?;
    let screened = screen(&p, &segs, drops, total_ms)?;
    let decisions = screened.decisions;
    let windows = cut_windows(&screened.safe);
    let cuts = windows.len();
    let outcome = overall_outcome(&decisions);
    let refused = refused_list(&decisions);
    let child_name = child_variant_name(&parent_name, retention);
    rightkit_autoedit::variants::validate_variant_name(&child_name)
        .map_err(|e| bad(e.to_string()))?;
    if rightkit_autoedit::variants::list_variants(dir)
        .map_err(|e| bad(e.to_string()))?
        .contains(&child_name)
    {
        return Err(bad(format!(
            "retention child variant {child_name:?} already exists; retention applies once per export"
        )));
    }
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "retention.apply",
                "name": retention,
                "variant": child_name,
                "cuts": cuts,
                "outcome": outcome,
            },
            "render": render,
            "decisions": decisions,
            "refused": refused,
        }));
    }
    if cuts > 0 {
        let actions: Vec<Action> = windows
            .iter()
            .map(|&(s, e)| Action::RippleDeleteRange {
                start: Ticks::from_millis(s),
                end: Ticks::from_millis(e),
            })
            .collect();
        store
            .apply(Action::Batch { actions })
            .map_err(|e| bad(format!("retention cut failed: {e}")))?;
    }
    let child_head = store.head_hash();
    let child_cursor = store.cursor();
    let edited = child_head != export_head;
    let mut provenance = parent.provenance.clone();
    provenance.name = child_name.clone();
    provenance.timeline_blake3 = export_head.clone();
    provenance.output_frames = store.timeline().total_frames();
    let child = rightkit_autoedit::variants::Variant {
        name: child_name.clone(),
        provenance,
        plan: parent.plan.clone(),
        store,
    };
    let link = rightkit_autoedit::variants::ParentLink {
        parent: parent_name.clone(),
        parent_head_hash: export_head.clone(),
        parent_cursor: export_cursor,
        export_blake3: Some(hash.to_string()),
        relation: format!("retention:{retention}"),
    };
    let timeline = rightkit_autoedit::variants::save_child_variant(dir, &child, &link)
        .map_err(|e| bad(e.to_string()))?;
    let request = if cuts > 0 {
        let path = request_path(dir, retention);
        let body = json!({
            "schema": REQ_SCHEMA,
            "reason": format!("retention drop-off cuts from {retention}"),
            "base_render": render,
            "base_variant": parent_name,
            "base_revision": {"head_hash": export_head, "cursor": export_cursor},
            "child_variant": child_name,
            "applied_revision": {"head_hash": child_head, "cursor": child_cursor},
            "cuts": windows
                .iter()
                .map(|&(s, e)| json!({"out_start_ms": s, "out_end_ms": e}))
                .collect::<Vec<_>>(),
        });
        write_json(&path, &body)?;
        json!(path.display().to_string())
    } else {
        Value::Null
    };
    let status = if cuts > 0 { "applied" } else { "proposed" };
    Ok(json!({
        "status": status,
        "name": retention,
        "render": render,
        "outcome": outcome,
        "cuts": cuts,
        "decisions": decisions,
        "refused": refused,
        "parent": {
            "variant": parent_name,
            "head_hash": export_head,
            "cursor": export_cursor,
            "export_blake3": hash,
        },
        "child": {
            "variant": child_name,
            "timeline": timeline.display().to_string(),
            "head_hash": child_head,
            "cursor": child_cursor,
            "edited": edited,
        },
        "render_request": request,
    }))
}

/// The parent's store moved to the exported revision: undone or redone to
/// `cursor`, then checked against the exported `head`. Fails when that revision
/// is no longer in the parent's history or its hash differs.
fn store_at_export(
    parent: &TimelineStore,
    name: &str,
    cursor: usize,
    head: &str,
) -> Result<TimelineStore, ProjectError> {
    let gone = |detail: String| {
        bad(format!(
            "export of variant {name:?} no longer matches its revision ({detail}); re-render and re-import retention"
        ))
    };
    let mut s = parent.clone();
    while s.cursor() > cursor {
        s.undo().map_err(|e| gone(e.to_string()))?;
    }
    while s.cursor() < cursor {
        s.redo().map_err(|e| gone(e.to_string()))?;
    }
    if s.head_hash() != head {
        return Err(gone(format!("revision {cursor} hashes differently")));
    }
    Ok(s)
}

/// `<parent>-ret-<retention>`, kept within the 48-character variant name limit.
fn child_variant_name(parent: &str, retention: &str) -> String {
    let tag: String = retention
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    let suffix = format!("-ret-{tag}");
    let base: String = parent.chars().take(48 - suffix.len()).collect();
    format!("{base}{suffix}")
}

/// The non-destructive proposal for a held drop: an opening drop asks for a
/// revised opening, a closing drop is left as it is, and a drop in the middle
/// is covered with B-roll so the narration runs on.
fn held_outcome(s: i64, total_ms: i64) -> &'static str {
    if s < OPENING_MS {
        "revised_opening"
    } else if s >= total_ms - OPENING_MS {
        "no_change"
    } else {
        "broll"
    }
}

/// `cut` when every selected drop is cut; otherwise the first held outcome.
fn overall_outcome(decisions: &[Value]) -> String {
    decisions
        .iter()
        .filter_map(|d| d["outcome"].as_str())
        .find(|o| *o != "cut")
        .unwrap_or("cut")
        .to_string()
}

/// Output-ms positions of every clause end in the video segments: the gap just
/// after a word that ends a spoken clause, mapped to output time. A segment whose
/// transcript cannot be read contributes no edges; `check_drop` refuses any
/// candidate that touches it.
fn clause_edges(p: &Project, segs: &[OutSeg]) -> Vec<i64> {
    let mut outs = Vec::new();
    for seg in segs {
        let Ok(t) = load_transcript(p, &seg.2) else {
            continue;
        };
        outs.extend(segment_edges(&Clauses::new(&t), seg));
    }
    outs.sort_unstable();
    outs.dedup();
    outs
}

/// Clause ends of one transcript that fall inside one output segment, in output
/// ms. A word counts only when it ends inside the segment's source range, and its
/// output position is `seg.0 + (word_end - seg.3)`.
fn segment_edges(c: &Clauses, seg: &OutSeg) -> Vec<i64> {
    let mut outs = Vec::new();
    for (i, w) in c.words.iter().enumerate() {
        if c.last[i] != i || w.end < seg.3 || w.end > seg.4 {
            continue;
        }
        let out = seg.0 + (w.end - seg.3);
        if (seg.0..=seg.1).contains(&out) {
            outs.push(out);
        }
    }
    outs
}

/// The span a candidate is screened on. A window covering no spoken word (only
/// silence or dead air) is the safest cut, so it keeps its own span. Any other
/// window snaps both edges to clause ends within SNAP_MS, or has no span (None)
/// and is refused as `clause_edge`.
fn snap_window(edges: &[i64], s0: i64, e0: i64, silence_only: bool) -> Option<(i64, i64)> {
    if silence_only {
        return Some((s0, e0));
    }
    // Both edges may be nearest to the same clause end (a short window around one
    // end); take the in-range pair with s < e that moves the edges least.
    let near = |x: i64| {
        edges
            .iter()
            .copied()
            .filter(move |&o| (o - x).abs() <= SNAP_MS)
    };
    near(s0)
        .flat_map(|s| near(e0).map(move |e| (s, e)))
        .filter(|&(s, e)| s < e)
        .min_by_key(|&(s, e)| ((s - s0).abs() + (e - e0).abs(), s, e))
}

/// Verdict on one candidate edit (a drop window in output ms).
enum Check {
    /// Whole spoken clauses on word-aligned edges (or no speech): may be cut.
    Cut { detail: String, span: Value },
    /// A transcript check failed: the candidate is refused with this reason.
    Hold {
        check: &'static str,
        reason: String,
        clause: Option<String>,
        span: Value,
    },
}

/// A transcript word as the clause check sees it (source ms).
struct Spoken {
    text: String,
    start: i64,
    end: i64,
}

/// A source transcript split into spoken clauses. `first[i]` and `last[i]` are
/// the first and last word index of word `i`'s clause. A clause ends at sentence
/// punctuation or before a pause of at least CLAUSE_GAP_MS.
struct Clauses {
    words: Vec<Spoken>,
    first: Vec<usize>,
    last: Vec<usize>,
}

impl Clauses {
    fn new(t: &Transcript) -> Clauses {
        let words: Vec<Spoken> = t
            .words
            .iter()
            .map(|w| Spoken {
                text: w.text.clone(),
                start: w.start_ms as i64,
                end: w.end_ms as i64,
            })
            .collect();
        let n = words.len();
        let ends: Vec<bool> = (0..n)
            .map(|i| {
                let sentence_end = words[i]
                    .text
                    .trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}'])
                    .ends_with(['.', '?', '!', '\u{2026}']);
                let pause = i + 1 == n || words[i + 1].start - words[i].end >= CLAUSE_GAP_MS;
                sentence_end || pause
            })
            .collect();
        let mut first = Vec::with_capacity(n);
        let mut start = 0;
        for (i, &ends_here) in ends.iter().enumerate() {
            first.push(start);
            if ends_here {
                start = i + 1;
            }
        }
        let mut last = vec![0; n];
        let mut end = 0;
        for (i, &ends_here) in ends.iter().enumerate().rev() {
            if ends_here {
                end = i;
            }
            last[i] = end;
        }
        Clauses { words, first, last }
    }

    /// Index of the word that a cut edge at `x` (source ms) would split.
    fn word_over(&self, x: i64) -> Option<usize> {
        self.words.iter().position(|w| w.start < x && x < w.end)
    }

    fn text(&self, a: usize, b: usize) -> String {
        self.words[a..=b]
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Checks one candidate window (output ms) against the transcript of every video
/// segment it touches. An edge that falls inside a segment must sit in a gap
/// between words, and the words the window covers must be whole clauses. The
/// verdict carries the transcript span the candidate covers (null without speech).
fn check_drop(p: &Project, segs: &[OutSeg], s: i64, e: i64) -> Result<Check, ProjectError> {
    let mut first_span: Option<Value> = None;
    for seg in segs.iter().filter(|g| s < g.1 && e > g.0) {
        let t = match load_transcript(p, &seg.2) {
            Ok(t) => t,
            Err(err) => {
                return Ok(Check::Hold {
                    check: "transcript",
                    reason: format!(
                        "source {} has no usable transcript ({err}); the drop cannot be checked against speech",
                        seg.2
                    ),
                    clause: None,
                    span: Value::Null,
                })
            }
        };
        let c = Clauses::new(&t);
        // Source position of an output ms inside this segment.
        let src = |out: i64| (seg.3 + (out - seg.0)).clamp(seg.3, seg.4);
        let (sa, sb) = (src(s.max(seg.0)), src(e.min(seg.1)));
        let edge = json!({"source": seg.2.clone(), "start_ms": sa, "end_ms": sb});
        let start_cut = if s > seg.0 { c.word_over(sa) } else { None };
        if let Some(w) = start_cut {
            return Ok(Check::Hold {
                check: "word_edge",
                reason: format!(
                    "drop start {s} ms falls inside the word {:?}",
                    c.words[w].text
                ),
                clause: None,
                span: edge,
            });
        }
        let end_cut = if e < seg.1 { c.word_over(sb) } else { None };
        if let Some(w) = end_cut {
            return Ok(Check::Hold {
                check: "word_edge",
                reason: format!(
                    "drop end {e} ms falls inside the word {:?}",
                    c.words[w].text
                ),
                clause: None,
                span: edge,
            });
        }
        let hit: Vec<usize> = (0..c.words.len())
            .filter(|&i| {
                let w = &c.words[i];
                (sa..sb).contains(&((w.start + w.end) / 2))
            })
            .collect();
        if let (Some(&lo), Some(&hi)) = (hit.first(), hit.last()) {
            let (a, b) = (c.first[lo], c.last[hi]);
            if a != lo || b != hi {
                let clause = c.text(a, b);
                return Ok(Check::Hold {
                    check: "clause_whole",
                    reason: format!(
                        "drop {s}-{e} ms covers part of the spoken clause {clause:?}; cutting it would break the clause"
                    ),
                    clause: Some(clause.clone()),
                    span: json!({
                        "source": seg.2.clone(),
                        "start_ms": c.words[a].start,
                        "end_ms": c.words[b].end,
                        "text": clause,
                    }),
                });
            }
            first_span.get_or_insert_with(|| {
                json!({
                    "source": seg.2.clone(),
                    "start_ms": c.words[lo].start,
                    "end_ms": c.words[hi].end,
                    "text": c.text(lo, hi),
                })
            });
        }
    }
    Ok(Check::Cut {
        detail: "window covers whole spoken clauses on word-aligned edges, or no speech".into(),
        span: first_span.unwrap_or(Value::Null),
    })
}

/// The candidates after screening: one decision per input drop, in input order,
/// and the snapped windows (output ms) that passed every check and may be cut.
struct Screened {
    decisions: Vec<Value>,
    safe: Vec<(i64, i64)>,
}

/// Screens every candidate edit before any cut is applied. A window covering
/// spoken words first snaps its edges to the nearest clause end within SNAP_MS; a
/// silence-only window keeps its span. The decision records the original window
/// and the snapped span (equal to the original for silence). The screened window
/// then gets its transcript span and the check that decided it: word edges and
/// whole clauses (`check_drop`), then the minimum kept length (`min_kept_check`).
/// A candidate that fails a check, or has no clause edge in range, is refused
/// with its reason and is never cut.
fn screen(
    p: &Project,
    segs: &[OutSeg],
    drops: &[&Value],
    total_ms: i64,
) -> Result<Screened, ProjectError> {
    let edges = clause_edges(p, segs);
    let mut decisions = Vec::with_capacity(drops.len());
    let mut passed: Vec<(usize, i64, i64)> = Vec::new();
    for (i, d) in drops.iter().enumerate() {
        let s0 = d["out_start_ms"].as_i64().unwrap_or(0);
        let e0 = d["out_end_ms"].as_i64().unwrap_or(0);
        let original = json!({"out_start_ms": s0, "out_end_ms": e0});
        // The transcript requirement comes first: without a readable transcript
        // the window cannot be snapped or checked, so it is held as `transcript`.
        let verdict0 = check_drop(p, segs, s0, e0)?;
        if let Check::Hold {
            check: "transcript",
            reason,
            span,
            ..
        } = &verdict0
        {
            decisions.push(json!({
                "id": d["id"],
                "outcome": held_outcome(s0, total_ms),
                "check": "transcript",
                "out_start_ms": s0,
                "out_end_ms": e0,
                "original_window": original,
                "snapped_span": Value::Null,
                "reason": reason.clone(),
                "clause": Value::Null,
                "transcript_span": span.clone(),
            }));
            continue;
        }
        // Checked on the original window: a null span means no spoken word.
        let silence_only = matches!(
            verdict0,
            Check::Cut { ref span, .. } if span.is_null()
        );
        let Some((s, e)) = snap_window(&edges, s0, e0, silence_only) else {
            decisions.push(json!({
                "id": d["id"],
                "outcome": held_outcome(s0, total_ms),
                "check": "clause_edge",
                "out_start_ms": s0,
                "out_end_ms": e0,
                "original_window": original,
                "snapped_span": Value::Null,
                "reason": format!(
                    "no clause end within {SNAP_MS} ms of both edges of the drop {s0}-{e0} ms"
                ),
                "clause": Value::Null,
                "transcript_span": Value::Null,
            }));
            continue;
        };
        let (outcome, check, reason, clause, span) = match check_drop(p, segs, s, e)? {
            Check::Cut { detail, span } => ("cut", "passed", detail, None, span),
            Check::Hold {
                check,
                reason,
                clause,
                span,
            } => (held_outcome(s, total_ms), check, reason, clause, span),
        };
        if outcome == "cut" {
            passed.push((i, s, e));
        }
        decisions.push(json!({
            "id": d["id"],
            "outcome": outcome,
            "check": check,
            "out_start_ms": s,
            "out_end_ms": e,
            "original_window": original,
            "snapped_span": {"out_start_ms": s, "out_end_ms": e},
            "reason": reason,
            "clause": clause,
            "transcript_span": span,
        }));
    }
    let spans: Vec<(i64, i64)> = passed.iter().map(|&(_, s, e)| (s, e)).collect();
    let mut safe = Vec::new();
    for (&(i, s, e), verdict) in passed.iter().zip(min_kept_check(&spans, total_ms)) {
        match verdict {
            Ok(()) => safe.push((s, e)),
            Err(run) => {
                decisions[i]["outcome"] = json!(held_outcome(s, total_ms));
                decisions[i]["check"] = json!("min_kept_ms");
                decisions[i]["reason"] = json!(format!(
                    "the cut would leave a kept run of {run} ms beside it (minimum {MIN_KEPT_MS} ms)"
                ));
            }
        }
    }
    Ok(Screened { decisions, safe })
}

/// Minimum-kept-length pass over candidate windows (output ms, input order).
/// Walks them in output order: a window may not leave a kept run shorter than
/// MIN_KEPT_MS before it (from the previous cut or the start), and the last
/// accepted window may not leave one shorter than that after it. Returns, per
/// window, `Ok` or `Err(run_ms)` with the refused run length.
fn min_kept_check(spans: &[(i64, i64)], total_ms: i64) -> Vec<Result<(), i64>> {
    let mut out = vec![Ok(()); spans.len()];
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| spans[i].0);
    let mut accepted: Vec<usize> = Vec::new();
    let mut kept_from = 0i64;
    for i in order {
        let (s, e) = spans[i];
        let kept = s - kept_from;
        if kept > 0 && kept < MIN_KEPT_MS {
            out[i] = Err(kept);
        } else {
            accepted.push(i);
            kept_from = kept_from.max(e);
        }
    }
    let tail = total_ms - kept_from;
    if tail > 0 && tail < MIN_KEPT_MS {
        if let Some(i) = accepted.pop() {
            out[i] = Err(tail);
        }
    }
    out
}

/// The refused candidates, each with its check, reason and transcript span.
fn refused_list(decisions: &[Value]) -> Vec<Value> {
    decisions
        .iter()
        .filter(|d| d["outcome"] != "cut")
        .map(|d| {
            json!({
                "id": d["id"],
                "outcome": d["outcome"],
                "check": d["check"],
                "reason": d["reason"],
                "transcript_span": d["transcript_span"],
            })
        })
        .collect()
}

/// Output→source segments that the main timeline's cuts are checked against:
/// the export's video rows, else the current timeline's clip ranges.
fn main_segments(p: &Project, rec: &Value) -> Vec<OutSeg> {
    let video = video_segments(rec);
    if video.is_empty() {
        time_map_for(p, rec).0
    } else {
        video
    }
}

/// Screened output windows merged, latest first: each ripple delete then leaves
/// every earlier window at its exported position.
fn cut_windows(spans: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut w: Vec<(i64, i64)> = spans.iter().copied().filter(|&(s, e)| e > s).collect();
    w.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (s, e) in w {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged.reverse();
    merged
}
