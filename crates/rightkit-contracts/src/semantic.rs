//! Cross-field rules that JSON Schema cannot express. Each rule runs only after
//! the document has passed its schema, so these functions may assume the
//! shapes the schema guarantees and only check relationships between fields.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::{Contract, ContractError};

type Rule = Result<(), String>;

pub(crate) fn check(contract: Contract, document: &Value) -> Result<(), ContractError> {
    let outcome = match contract {
        Contract::ContentBrief => content_brief(document),
        Contract::DeliveryEnvelope => delivery_envelope(document),
        Contract::RecordSession => record_session(document),
        Contract::RecordEvent => record_event(document),
        Contract::DemoPlan => demo_plan(document),
        Contract::DaemonApi => daemon_api(document),
        // The semantic rules of channel-memory live in rightkit-channel-memory.
        Contract::ChannelMemory => Ok(()),
    };
    outcome.map_err(|rule| ContractError::Semantic { contract, rule })
}

static NULL: Value = Value::Null;

fn items<'a>(parent: &'a Value, key: &str) -> &'a [Value] {
    match parent.get(key).and_then(Value::as_array) {
        Some(array) => array.as_slice(),
        None => &[],
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn unique<'a>(values: impl IntoIterator<Item = &'a str>, what: &str) -> Rule {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(format!("duplicate {what} `{value}`"));
        }
    }
    Ok(())
}

fn content_brief(doc: &Value) -> Rule {
    let revision = doc.get("revision").and_then(Value::as_u64).unwrap_or(0);
    if let Some(superseded) = doc.get("supersedes").and_then(Value::as_u64) {
        if superseded + 1 != revision {
            return Err(format!("revision {revision} must supersede revision {} (its immediate predecessor), not {superseded}", revision.saturating_sub(1)));
        }
    }

    let stages = items(doc, "stages");
    let stage_ids: Vec<&str> = stages.iter().map(|stage| text(stage, "id")).collect();
    unique(stage_ids.iter().copied(), "stage id")?;
    for stage in stages {
        for dependency in items(stage, "dependsOn").iter().filter_map(Value::as_str) {
            if !stage_ids.contains(&dependency) {
                return Err(format!(
                    "stage `{}` depends on `{dependency}`, which no stage declares",
                    text(stage, "id")
                ));
            }
            if dependency == text(stage, "id") {
                return Err(format!("stage `{dependency}` depends on itself"));
            }
        }
    }
    acyclic(stages)?;

    if let Some(scenario) = doc.get("scenario") {
        let targets: Vec<&str> = items(scenario, "targets")
            .iter()
            .map(|target| text(target, "targetId"))
            .collect();
        unique(targets.iter().copied(), "target id")?;
        let steps = items(scenario, "steps");
        unique(steps.iter().map(|step| text(step, "stepId")), "step id")?;
        for step in steps {
            if let Some(target) = step
                .get("action")
                .and_then(|action| action.get("targetId"))
                .and_then(Value::as_str)
            {
                if !targets.contains(&target) {
                    return Err(format!(
                        "step `{}` acts on unknown target `{target}`",
                        text(step, "stepId")
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Stages must form a DAG. Repeatedly remove stages whose dependencies are all
/// removed; whatever remains is on a cycle.
fn acyclic(stages: &[Value]) -> Rule {
    let mut remaining: Vec<(&str, Vec<&str>)> = stages
        .iter()
        .map(|stage| {
            (
                text(stage, "id"),
                items(stage, "dependsOn")
                    .iter()
                    .filter_map(Value::as_str)
                    .collect(),
            )
        })
        .collect();
    while !remaining.is_empty() {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = remaining.iter().map(|(id, _)| *id).collect();
            return Err(format!("stage dependencies form a cycle among {stuck:?}"));
        }
        remaining.retain(|(id, _)| !ready.contains(id));
        for (_, deps) in &mut remaining {
            deps.retain(|dep| !ready.contains(dep));
        }
    }
    Ok(())
}

fn delivery_envelope(doc: &Value) -> Rule {
    let Some(payload) = doc.get("payload") else {
        return Ok(());
    };
    if text(payload, "type") == "carousel" {
        for (position, slide) in items(payload, "slides").iter().enumerate() {
            let expected = position as u64 + 1;
            if slide.get("index").and_then(Value::as_u64) != Some(expected) {
                return Err(format!("carousel slide indices must run 1..n in order; slide {position} should be index {expected}"));
            }
        }
    }
    Ok(())
}

fn record_session(doc: &Value) -> Rule {
    let status = text(doc, "status");
    let has_reason = doc.get("reason").is_some();
    if status == "complete" && has_reason {
        return Err("a complete bundle must not carry a reason".into());
    }
    if status != "complete" && !has_reason {
        return Err(format!("a {status} bundle must name its reason"));
    }

    let epochs = items(doc, "geometryEpochs");
    let mut previous_start = 0;
    for (position, epoch) in epochs.iter().enumerate() {
        if epoch.get("epoch").and_then(Value::as_u64) != Some(position as u64) {
            return Err(format!("geometry epochs must be numbered 0..n-1 in order; entry {position} is out of place"));
        }
        let start = epoch.get("startMs").and_then(Value::as_u64).unwrap_or(0);
        if start < previous_start {
            return Err(format!(
                "geometry epoch {position} starts before epoch {}",
                position.saturating_sub(1)
            ));
        }
        previous_start = start;
    }

    let surfaces = items(doc.get("target").unwrap_or(&NULL), "surfaces");
    unique(
        surfaces.iter().map(|surface| text(surface, "surfaceId")),
        "surface id",
    )?;
    let surface_ids: Vec<&str> = surfaces
        .iter()
        .map(|surface| text(surface, "surfaceId"))
        .collect();
    for surface in surfaces {
        if let Some(parent) = surface.get("parentSurfaceId").and_then(Value::as_str) {
            if !surface_ids.contains(&parent) {
                return Err(format!(
                    "surface `{}` names unknown parent `{parent}`",
                    text(surface, "surfaceId")
                ));
            }
        }
        if surface.get("supported").and_then(Value::as_bool) == Some(false)
            && surface.get("unsupportedReason").is_none()
        {
            return Err(format!(
                "unsupported surface `{}` must state why",
                text(surface, "surfaceId")
            ));
        }
    }

    unique(
        items(doc, "tracks")
            .iter()
            .map(|track| text(track, "trackId")),
        "track id",
    )?;
    let dependency_keys: Vec<String> = items(doc, "dependencies")
        .iter()
        .map(|dependency| format!("{}:{}", text(dependency, "kind"), text(dependency, "id")))
        .collect();
    unique(dependency_keys.iter().map(String::as_str), "dependency")?;
    Ok(())
}

fn record_event(doc: &Value) -> Rule {
    if let Some(timing) = doc.get("timing") {
        let ordered = ["admissionNs", "dispatchNs", "completionNs"];
        let mut previous: Option<(&str, u64)> = None;
        for key in ordered {
            if let Some(value) = timing.get(key).and_then(Value::as_u64) {
                if let Some((earlier, earlier_value)) = previous {
                    if value < earlier_value {
                        return Err(format!(
                            "timing {key} ({value}) precedes {earlier} ({earlier_value})"
                        ));
                    }
                }
                previous = Some((key, value));
            }
        }
    }
    if text(doc, "kind") == "input"
        && doc.get("admitted").and_then(Value::as_bool) == Some(false)
        && doc.get("denied").is_none()
    {
        return Err("an input that was not admitted must carry a denied reason".into());
    }
    Ok(())
}

fn demo_plan(doc: &Value) -> Rule {
    let narration = doc.get("narration").unwrap_or(&NULL);
    let words = items(narration, "words");
    unique(words.iter().map(|word| text(word, "wordId")), "word id")?;
    for word in words {
        let start = word.get("startMs").and_then(Value::as_u64).unwrap_or(0);
        let end = word.get("endMs").and_then(Value::as_u64).unwrap_or(0);
        if end <= start {
            return Err(format!(
                "word `{}` ends before it starts",
                text(word, "wordId")
            ));
        }
    }
    let word_ids: Vec<&str> = words.iter().map(|word| text(word, "wordId")).collect();

    let mut cursor = 0u64;
    for (position, segment) in items(narration, "timeMap").iter().enumerate() {
        let out_start = segment
            .get("outputStartMs")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let out_end = segment
            .get("outputEndMs")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if out_start != cursor {
            return Err(format!("time map segment {position} starts at {out_start} ms but the previous segment ended at {cursor} ms"));
        }
        if out_end <= out_start {
            return Err(format!(
                "time map segment {position} has no output duration"
            ));
        }
        if text(segment, "kind") == "play" {
            let source_start = segment
                .get("sourceStartMs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let source_end = segment
                .get("sourceEndMs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if source_end <= source_start {
                return Err(format!("play segment {position} has no source span"));
            }
            let speed = segment.get("speed").and_then(Value::as_f64).unwrap_or(1.0);
            let expected_source = (out_end - out_start) as f64 * speed;
            let actual_source = (source_end - source_start) as f64;
            if (expected_source - actual_source).abs() > 1.0 {
                return Err(format!("play segment {position}: source span {actual_source} ms does not equal output span times speed {speed}"));
            }
        }
        cursor = out_end;
    }
    let duration = narration
        .get("durationMs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if cursor != duration {
        return Err(format!(
            "time map ends at {cursor} ms but narration durationMs is {duration}"
        ));
    }

    let actions = items(doc, "timeline");
    unique(
        actions.iter().map(|action| text(action, "actionId")),
        "action id",
    )?;
    for action in actions {
        for key in ["start", "end"] {
            if let Some(word) = action
                .get(key)
                .and_then(|time| time.get("anchor"))
                .and_then(|anchor| anchor.get("wordId"))
                .and_then(Value::as_str)
            {
                if !word_ids.contains(&word) {
                    return Err(format!(
                        "action `{}` anchors to unknown word `{word}`",
                        text(action, "actionId")
                    ));
                }
            }
        }
    }
    Ok(())
}

fn daemon_api(doc: &Value) -> Rule {
    let routes = items(doc, "routes");
    let route_ids: Vec<&str> = routes.iter().map(|route| text(route, "routeId")).collect();
    unique(route_ids.iter().copied(), "route id")?;
    if let Some(upload_routes) = doc
        .get("upload")
        .and_then(|upload| upload.get("routes"))
        .and_then(Value::as_object)
    {
        for (role, value) in upload_routes {
            let route = value.as_str().unwrap_or("");
            if !route_ids.contains(&route) {
                return Err(format!(
                    "upload {role} names route `{route}`, which the API does not declare"
                ));
            }
        }
    }
    for exchange in items(doc, "exchanges") {
        let route = text(exchange, "routeId");
        if !route_ids.contains(&route) {
            return Err(format!(
                "exchange names route `{route}`, which the API does not declare"
            ));
        }
    }
    Ok(())
}
