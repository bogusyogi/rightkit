//! What an agent is told about the age of the repository graph, and the per-root Blueprint
//! status (#555). Pure composition over answers the caller already holds: a Blueprint `status`
//! answer for one root and the resident's status answer. Nothing here reads the graph, writes a
//! store or starts a watcher.
//!
//! The graph is reported only as far as it was observed. A status that did not answer is "not
//! checked", never "fresh"; a missing line is the only thing a fresh graph produces.

use serde_json::{json, Value};
use std::path::Path;

/// The roots enrolled in the installation registry, as recorded (not canonicalized).
pub fn enrolled_roots() -> Result<Vec<String>, String> {
    let registry =
        crate::authorization::load_installation_registry().map_err(|denial| denial.to_string())?;
    let mut roots: Vec<String> = registry
        .bindings()
        .iter()
        .map(|binding| binding.root.clone())
        .collect();
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Whether `cwd` is an enrolled root or sits inside one. A registry that cannot be read counts as
/// enrolled, so an unreadable registry never produces a wrong "not enrolled" instruction.
pub fn is_enrolled(cwd: &Path) -> bool {
    let Ok(roots) = enrolled_roots() else {
        return true;
    };
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let cwd = canonical(cwd);
    roots.iter().any(|root| {
        let root = canonical(Path::new(root));
        cwd == root || cwd.starts_with(&root)
    })
}

/// `secs` as a short age: "under a minute", "7 min", "3 h", "2 d".
pub fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => "under a minute".to_owned(),
        60..=3_599 => format!("{} min", secs / 60),
        3_600..=86_399 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    }
}

fn short(revision: &str) -> &str {
    revision.get(..7).unwrap_or(revision)
}

/// The one line `membrane context pull` writes beside its answer, or `None` when the graph was
/// observed fresh. `status` is the Blueprint `status` answer for the pulled repository, or `None`
/// when that status could not be read in time.
///
/// Exact texts (the part after the colon varies only in the bracketed facts):
/// - stale: `Membrane graph: STALE, index behind HEAD (indexed abc1234, HEAD def5678), store last
///   written 3 h ago; code-graph facts may be out of date (blueprint refresh).`
///   (or `, 4 indexed files changed since the build` / `, files changed since the build`)
/// - unknown or unavailable: `Membrane graph: freshness UNKNOWN, store last written 3 h ago;
///   not verified against HEAD or the working tree (blueprint refresh).`
/// - folder not enrolled: `Membrane graph: NOT ENROLLED for this folder; no code-graph facts (host
///   blueprint enroll).`
/// - enrolled, no graph: `Membrane graph: NOT BUILT for this repository; no code-graph facts (host
///   blueprint build).`
/// - unreadable: `Membrane graph: UNREADABLE store; no code-graph facts (host blueprint
///   build).`
/// - no answer: `Membrane graph: freshness NOT CHECKED (status did not answer in time); code-graph
///   facts may be out of date.`
pub fn staleness_line(status: Option<&Value>, enrolled: bool) -> Option<String> {
    let Some(status) = status else {
        return Some(
            "Membrane graph: freshness NOT CHECKED (status did not answer in time); \
             code-graph facts may be out of date."
                .to_owned(),
        );
    };
    let age = status
        .get("storeAgeSecs")
        .and_then(Value::as_u64)
        .map(|secs| format!(", store last written {} ago", age_text(secs)))
        .unwrap_or_default();
    match status.get("state").and_then(Value::as_str) {
        Some("fresh") => None,
        Some("stale") => {
            let receipt = status.get("freshnessReceipt");
            let indexed = receipt
                .and_then(|r| r.pointer("/generation/indexed_revision"))
                .and_then(Value::as_str);
            let head = receipt
                .and_then(|r| r.pointer("/current/vcs_revision"))
                .and_then(Value::as_str);
            let changed = receipt
                .and_then(|r| r.pointer("/staleSources/paths"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let why = match (indexed, head) {
                (Some(indexed), Some(head)) if indexed != head => format!(
                    ", index behind HEAD (indexed {}, HEAD {})",
                    short(indexed),
                    short(head)
                ),
                _ if changed > 0 => format!(", {changed} indexed files changed since the build"),
                _ => ", files changed since the build".to_owned(),
            };
            Some(format!(
                "Membrane graph: STALE{why}{age}; code-graph facts may be out of date \
                 (blueprint refresh)."
            ))
        }
        Some("missing") if !enrolled => Some(
            "Membrane graph: NOT ENROLLED for this folder; no code-graph facts \
             (blueprint enroll)."
                .to_owned(),
        ),
        Some("missing") => Some(
            "Membrane graph: NOT BUILT for this repository; no code-graph facts \
             (blueprint build)."
                .to_owned(),
        ),
        Some("corrupt") => Some(
            "Membrane graph: UNREADABLE store; no code-graph facts (blueprint build).".to_owned(),
        ),
        // "unknown", "unavailable" and anything this build does not know: not verified.
        _ => Some(format!(
            "Membrane graph: freshness UNKNOWN{age}; not verified against HEAD or the working \
             tree (blueprint refresh)."
        )),
    }
}

fn same_root(left: &str, right: &str) -> bool {
    let canonical = |path: &str| {
        std::fs::canonicalize(Path::new(path))
            .map(|path| path.to_string_lossy().to_lowercase())
            .unwrap_or_else(|_| path.to_lowercase())
    };
    left.eq_ignore_ascii_case(right) || canonical(left) == canonical(right)
}

/// One enrolled root in `membrane blueprint status --all`: its last build (generation and the
/// last write to its store), its freshness state, and its own watcher state from the resident.
/// `graph` is that root's Blueprint `status` answer (`None` when it did not answer);
/// `resident` is the resident's status answer (`None` when no resident answered, which is
/// reported as `watcher: {"state": "no_resident"}`, never as a healthy watcher).
pub fn root_status(root: &str, graph: Option<&Value>, resident: Option<&Value>) -> Value {
    let field = |name: &str| graph.and_then(|graph| graph.get(name)).cloned();
    let freshness_state = graph
        .and_then(|graph| graph.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("not_checked");
    let watcher = match resident {
        None => json!({"state": "no_resident"}),
        Some(resident) => {
            let blueprint = resident.get("blueprint");
            let entry = blueprint
                .and_then(|b| b.get("watcherIdentity"))
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find(|item| {
                        item.get("root")
                            .and_then(Value::as_str)
                            .is_some_and(|candidate| same_root(candidate, root))
                    })
                });
            let for_root = |list: Option<&Value>| {
                list.and_then(Value::as_array)
                    .and_then(|items| {
                        items.iter().find(|item| {
                            item.get("root")
                                .and_then(Value::as_str)
                                .is_some_and(|candidate| same_root(candidate, root))
                        })
                    })
                    .cloned()
            };
            // Failures and skips are looked up for every enrolled root, watched or not: a root
            // that is `not_watched` is usually one of them (D4).
            let failures = for_root(blueprint.and_then(|b| b.get("buildFailures")));
            let skipped = for_root(blueprint.and_then(|b| b.get("skippedRepositories")));
            match entry {
                Some(entry) => json!({
                    "state": entry.get("state"),
                    "ready": entry.get("ready"),
                    "detail": entry.get("detail"),
                    "eventSequence": entry.get("eventSequence"),
                    "lastEvent": entry.get("lastEvent"),
                    "buildFailures": failures,
                    "skipped": skipped,
                }),
                // The resident answered but holds no watcher for this root: skipped (oversized,
                // ignored by a parent repository), failing to build, or not yet built.
                None => json!({
                    "state": "not_watched",
                    "buildFailures": failures,
                    "skipped": skipped,
                }),
            }
        }
    };
    json!({
        "root": root,
        "freshness": freshness_state,
        "staleness": staleness_line(graph, true),
        "generationId": field("generationId"),
        "complete": field("complete"),
        "lastBuild": {
            "storeModifiedAtMs": field("storeModifiedAtMs"),
            "storeAgeSecs": field("storeAgeSecs"),
        },
        "watcher": watcher,
    })
}
