//! The `engine_log` step: wait for an event in a JSON-lines log the engine writes, and prove no
//! forbidden text reached it.
//!
//! Fields:
//! * `path` (required): the log file, relative to the scenario's data directory (or absolute inside
//!   it). A missing file reads as an empty log while the `event` wait runs, and an empty or missing
//!   log at the end of the step is a failure unless `allow_empty`.
//! * `event`: the logged name (the `event` key of a line); the step waits up to `wait_s` (default 5,
//!   at most 60) for a line that has it. `fields`: a table of values that line must carry (paths use
//!   the `select` syntax of `[[step.expect]]`, values compare as JSON). `fields` needs `event`.
//! * `forbid`: substrings no line may contain. `forbid_held = true` adds every secret the scenario
//!   has read or made (`start_daemon`'s `secret_files` and `hold_files`, see
//!   [`super::StepState::hold`]).
//! * `allow_empty = true` accepts a missing or empty log, and a forbid check that compared nothing
//!   (for example `forbid_held` with no held secret). Without it both fail: a step that proved
//!   nothing must not pass.
//!
//! A failure names the line and the item, never the forbidden text. Result `{ events, lines, matched,
//! forbidden_checked }`.
use super::{flag, only, required, scenario_path, strings, uint};
use crate::check::select;
use crate::scenario::{pv, Ctx};
use crate::util::{err, Error, Result};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// Largest log the step reads.
const LOG_READ_MAX: u64 = 16 * 1024 * 1024;

pub(super) fn run(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    let path = scenario_path(
        &ctx.data_dir,
        &ctx.log_dir,
        &required(p, "engine_log", "path")?,
    )?;
    run_with(&path, ctx.steps.held(), p)
}

fn read_log(path: &Path) -> Result<String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > LOG_READ_MAX => err(format!(
            "{} is {} bytes, over the {LOG_READ_MAX} byte read bound",
            path.display(),
            meta.len()
        )),
        Ok(_) => std::fs::read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| Error(format!("read {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => err(format!("stat {}: {e}", path.display())),
    }
}

fn parse_line(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line.trim())
        .ok()
        .filter(Value::is_object)
}

fn event_name(line: &str) -> Option<String> {
    parse_line(line)?
        .get("event")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether `line` is `event` and carries every wanted (path, value) pair.
fn line_matches(line: &str, event: &str, wanted: &[(String, Value)]) -> bool {
    let Some(value) = parse_line(line) else {
        return false;
    };
    if value.get("event").and_then(Value::as_str) != Some(event) {
        return false;
    }
    wanted
        .iter()
        .all(|(path, want)| select(&value, path).is_some_and(|got| got == *want))
}

pub(super) fn run_with(
    path: &Path,
    held: &[(String, String)],
    p: &Map<String, Value>,
) -> Result<Value> {
    only(
        p,
        "engine_log",
        &[
            "path",
            "event",
            "fields",
            "forbid",
            "forbid_held",
            "wait_s",
            "allow_empty",
        ],
    )?;
    let allow_empty = flag(p, "allow_empty")?;
    let event = super::text(p, "event")?;
    let wanted: Vec<(String, Value)> = match pv(p, "fields") {
        None => Vec::new(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(_) => return err("`fields` must be a table"),
    };
    if !wanted.is_empty() && event.is_none() {
        return err("`fields` needs `event`");
    }
    let forbid = strings(p, "forbid")?;
    if forbid.iter().any(String::is_empty) {
        return err("`forbid` must hold only non-empty strings");
    }
    let forbid_held = flag(p, "forbid_held")?;
    if event.is_none() && forbid.is_empty() && !forbid_held {
        return err("engine_log needs `event`, `forbid` or `forbid_held`");
    }
    let wait = Duration::from_secs(uint(p, "wait_s")?.unwrap_or(5).min(60));
    let deadline = Instant::now() + wait;
    let (text, matched) = loop {
        let text = read_log(path)?;
        let matched = match &event {
            Some(name) => text.lines().any(|line| line_matches(line, name, &wanted)),
            None => true,
        };
        if matched || Instant::now() >= deadline {
            break (text, matched);
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut names: Vec<String> = Vec::new();
    for name in text.lines().filter_map(event_name) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if text.is_empty() && !allow_empty {
        return err(format!(
            "{} is missing or empty: an engine_log step must read the log it names (set allow_empty = true only when an empty log is the expected result)",
            path.display()
        ));
    }
    if !matched {
        let fields = if wanted.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = wanted.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!(" with fields {}", pairs.join(", "))
        };
        return err(format!(
            "no `{}` event{fields} in {} within {}s (events seen: {})",
            event.as_deref().unwrap_or(""),
            path.display(),
            wait.as_secs(),
            names.join(", ")
        ));
    }
    let mut needles: Vec<(String, &str)> = forbid
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("forbidden text #{}", i + 1), t.as_str()))
        .collect();
    if forbid_held {
        needles.extend(
            held.iter()
                .map(|(label, value)| (format!("held secret ({label})"), value.as_str())),
        );
    }
    for (number, line) in text.lines().enumerate() {
        if let Some((label, _)) = needles.iter().find(|(_, needle)| line.contains(*needle)) {
            let name = event_name(line).unwrap_or_else(|| "unparsed line".to_string());
            return err(format!(
                "{label} appears in {} on line {} (event {name})",
                path.display(),
                number + 1
            ));
        }
    }
    // A forbid check that compared nothing proves nothing (for example `forbid_held` with no held
    // secret): refused unless `allow_empty`.
    let checks_forbid = !forbid.is_empty() || forbid_held;
    if checks_forbid && needles.is_empty() && !allow_empty {
        return err(
            "engine_log checked no forbidden text: `forbid_held` found no held secret and no `forbid` was given (set allow_empty = true to accept this)",
        );
    }
    Ok(json!({
        "events": names,
        "lines": text.lines().count(),
        "matched": matched,
        "forbidden_checked": needles.len(),
    }))
}
