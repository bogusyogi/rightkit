//! Read-only, body-free send diagnostics. Each source retains its own clock.
use super::{
    brand::brand,
    links,
    store::Store,
    trace::{self, Event},
};
use serde::Serialize;
use std::collections::BTreeMap;

/// The usage text of the trace command, naming the host app's command.
pub fn help() -> String {
    format!(
        "usage: {} chat trace [<message-id>|--latest] [--json]\n\nShow local & peer send stages, unfinished work, slow stages & queue dwell.\nNo argument selects latest outbound trace. Missing/older/unreachable evidence is explicit.\nDurations never subtract timestamps from different computers.",
        brand().cli
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Unfinished {
    stage: String,
    span: String,
    phase: String,
    observation_age_ms: Option<u64>,
    execution_state: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    source: &'static str,
    status: &'static str,
    detail: Option<String>,
    events: Vec<Event>,
    unfinished: Vec<Unfinished>,
    slow_completed: Vec<Event>,
    queue_dwell: Vec<Event>,
}

fn source(name: &'static str, status: &'static str, events: Vec<Event>) -> Source {
    let mut open = BTreeMap::new();
    for e in &events {
        if matches!(e.phase.as_str(), "start" | "waiting")
            && !events.iter().any(|closed| {
                closed.span == e.span && matches!(closed.phase.as_str(), "finish" | "resumed")
            })
        {
            open.insert(e.span.clone(), e);
        }
    }
    let unfinished = open
        .values()
        .map(|e| Unfinished {
            stage: e.stage.clone(),
            span: e.span.clone(),
            phase: e.phase.clone(),
            observation_age_ms: e.elapsed_ms,
            execution_state: "unknown",
        })
        .collect::<Vec<_>>();
    let slow_completed = events
        .iter()
        .filter(|e| {
            matches!(e.phase.as_str(), "finish" | "resumed")
                && e.elapsed_ms.is_some_and(|ms| ms >= 1000)
        })
        .cloned()
        .collect();
    let queue_dwell = events
        .iter()
        .filter(|e| {
            e.phase == "resumed"
                && events
                    .iter()
                    .any(|waiting| waiting.span == e.span && waiting.phase == "waiting")
        })
        .cloned()
        .collect();
    Source {
        source: name,
        status: if status == "available" && !unfinished.is_empty() {
            "incomplete"
        } else {
            status
        },
        detail: None,
        events,
        unfinished,
        slow_completed,
        queue_dwell,
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    // Exact UUID syntax also prevents remote-shell/path interpretation.
    if !trace::valid_id(id) {
        return Err("trace needs a UUID message-id".into());
    }
    Ok(())
}

/// Internal remote endpoint. Deliberately returns only Event fields, never peer metadata.
pub fn local(args: Vec<String>, machine: bool) -> Result<(), String> {
    let ids = args
        .iter()
        .filter(|s| s.as_str() != "--json")
        .collect::<Vec<_>>();
    let [id] = ids.as_slice() else {
        return Err(format!(
            "usage: {} chat trace-local <message-id> --json",
            brand().cli
        ));
    };
    if !machine && !args.iter().any(|s| s == "--json") {
        return Err("trace-local requires --json".into());
    }
    validate_id(id)?;
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let events = trace::read_local(&store, id)?;
    if events.len() > 256 {
        return Err("trace event limit exceeded".into());
    }
    println!("{}", serde_json::json!({"events": events}));
    Ok(())
}

pub fn run(args: Vec<String>, machine: bool) -> Result<(), String> {
    let mut id = None;
    let mut latest = false;
    let mut json = machine;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--latest" if !latest => latest = true,
            _ if !arg.starts_with('-') && id.is_none() => id = Some(arg),
            _ => return Err(help()),
        }
    }
    if latest && id.is_some() {
        return Err(help());
    }
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let id = match id {
        Some(id) => Some(id),
        None => trace::latest(&store)?,
    };
    let Some(id) = id else {
        if json {
            println!(
                "{}",
                serde_json::json!({"messageId": null, "status": "unknown_or_older", "sources": []})
            );
        } else {
            println!("No outbound trace recorded (unknown or older send).");
        }
        return Ok(());
    };
    validate_id(&id)?;
    let events = trace::read_local(&store, &id)?;
    let local_status = if events.is_empty() {
        "unknown_or_older"
    } else {
        "available"
    };
    let mut sources = vec![source("local", local_status, events)];
    let remote = match trace::peer(&store, &id)? {
        None => {
            let mut s = source("peer", "not_recorded", vec![]);
            s.detail =
                Some("No linked peer recorded; same-computer sends have local stages only.".into());
            s
        }
        Some(peer) => match links::find(&store, &peer) {
            None => {
                let mut s = source("peer", "unreachable", vec![]);
                s.detail = Some(format!(
                    "Recorded peer link is missing; check {} chat status.",
                    brand().cli
                ));
                s
            }
            Some(link) => match links::fetch_trace(&store, &link, &id) {
                Ok(events) if events.len() <= 256 => {
                    let status = if events.is_empty() {
                        "unknown_or_older"
                    } else {
                        "available"
                    };
                    source("peer", status, events)
                }
                Ok(_) => source("peer", "invalid_response", vec![]),
                Err(error) => {
                    let status = if error.contains("invalid trace") || error.contains("exceeds") {
                        "invalid_response"
                    } else {
                        "unreachable_or_unsupported"
                    };
                    let mut s = source("peer", status, vec![]);
                    s.detail = Some(
                        error
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(512)
                            .collect(),
                    );
                    s
                }
            },
        },
    };
    sources.push(remote);
    if json {
        println!(
            "{}",
            serde_json::json!({"messageId": id, "sources": sources, "slowThresholdMs": 1000})
        );
    } else {
        println!("Trace {id}");
        for s in sources {
            println!("{}: {}", s.source, s.status);
            if let Some(detail) = &s.detail {
                println!("  {detail}");
            }
            for e in &s.events {
                let elapsed = e
                    .elapsed_ms
                    .map(|ms| format!(" {ms} ms"))
                    .unwrap_or_default();
                println!("  {} {}: {}{}", e.stage, e.phase, e.outcome, elapsed);
            }
            for e in &s.unfinished {
                let age = e
                    .observation_age_ms
                    .map(|ms| format!("; observed {ms} ms since start/wait"))
                    .unwrap_or_else(|| "; age unknown".into());
                println!("  unfinished {}{}; execution state unknown", e.stage, age);
            }
            for e in &s.slow_completed {
                println!(
                    "  slow completed: {} {} ms",
                    e.stage,
                    e.elapsed_ms.unwrap_or(0)
                );
            }
            for e in &s.queue_dwell {
                let duration = e
                    .elapsed_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "unknown".into());
                println!("  queue dwell: {} {duration}", e.stage);
            }
        }
    }
    Ok(())
}
