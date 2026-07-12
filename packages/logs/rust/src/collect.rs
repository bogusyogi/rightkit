use crate::redact::RedactionPolicy;
use crate::store::rotated_path;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const DIAGNOSTICS_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleMetadata {
    pub app_name: String,
    pub app_version: String,
    pub build: String,
    pub os: String,
    pub settings: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSource {
    pub name: String,
    pub path: PathBuf,
}

impl LogSource {
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionPolicy {
    pub max_events: usize,
    pub max_scan_bytes_per_file: u64,
    pub max_bundle_bytes: usize,
    pub include_rotated: bool,
    pub redaction: RedactionPolicy,
}

impl Default for CollectionPolicy {
    fn default() -> Self {
        Self {
            max_events: 500,
            max_scan_bytes_per_file: 512 * 1024,
            max_bundle_bytes: 512 * 1024,
            include_rotated: true,
            redaction: RedactionPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionStats {
    pub scanned_bytes: u64,
    pub parsed: usize,
    pub malformed: usize,
    pub dropped: usize,
    pub error_anchor_found: bool,
    pub rotated_read: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDiagnostics {
    pub name: String,
    pub file_name: String,
    pub events: Vec<Value>,
    pub stats: CollectionStats,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsBundle {
    pub schema_version: u16,
    pub metadata: BundleMetadata,
    pub sources: Vec<SourceDiagnostics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedEvents {
    pub events: Vec<Value>,
    pub stats: CollectionStats,
}

/// Apply the same bounded recent-window, last-error anchor, and recursive
/// redaction policy to events supplied by a database or another in-memory
/// source. This lets richer host support bundles share the Right Suite event
/// policy without replacing their existing envelope or storage model.
pub fn prepare_diagnostic_events(events: Vec<Value>, policy: &CollectionPolicy) -> PreparedEvents {
    let mut source = SourceDiagnostics {
        name: "events".to_string(),
        file_name: "events.jsonl".to_string(),
        stats: CollectionStats {
            parsed: events.len(),
            ..CollectionStats::default()
        },
        events,
        warnings: Vec::new(),
    };
    apply_global_event_policy(std::slice::from_mut(&mut source), policy.max_events);
    for event in &mut source.events {
        policy.redaction.redact(event);
    }
    PreparedEvents {
        events: source.events,
        stats: source.stats,
    }
}

pub fn collect_diagnostics(
    sources: &[LogSource],
    mut metadata: BundleMetadata,
    policy: &CollectionPolicy,
) -> DiagnosticsBundle {
    policy.redaction.redact(&mut metadata.settings);
    let mut bundle = DiagnosticsBundle {
        schema_version: DIAGNOSTICS_SCHEMA_VERSION,
        metadata,
        sources: sources
            .iter()
            .map(|source| collect_source(source, policy))
            .collect(),
    };
    apply_global_event_policy(&mut bundle.sources, policy.max_events);
    for source in &mut bundle.sources {
        for event in &mut source.events {
            policy.redaction.redact(event);
        }
    }
    enforce_bundle_limit(&mut bundle, policy.max_bundle_bytes);
    bundle
}

fn collect_source(source: &LogSource, policy: &CollectionPolicy) -> SourceDiagnostics {
    let mut result = SourceDiagnostics {
        name: source.name.clone(),
        file_name: source
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "events.jsonl".to_string()),
        events: Vec::new(),
        stats: CollectionStats::default(),
        warnings: Vec::new(),
    };

    if policy.include_rotated {
        let predecessor = rotated_path(&source.path);
        if predecessor.exists() {
            match read_jsonl_tail(&predecessor, policy.max_scan_bytes_per_file) {
                Ok(read) => {
                    result.stats.rotated_read = true;
                    merge_read(&mut result, read);
                }
                Err(error) => result
                    .warnings
                    .push(format!("rotated log unreadable: {}", error.kind())),
            }
        }
    }

    match read_jsonl_tail(&source.path, policy.max_scan_bytes_per_file) {
        Ok(read) => merge_read(&mut result, read),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            result.warnings.push("current log missing".to_string());
        }
        Err(error) => result
            .warnings
            .push(format!("current log unreadable: {}", error.kind())),
    }

    result
}

#[derive(Debug)]
struct TailRead {
    scanned_bytes: u64,
    parsed: Vec<Value>,
    malformed: usize,
}

fn read_jsonl_tail(path: &Path, max_bytes: u64) -> io::Result<TailRead> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = if max_bytes == 0 {
        length
    } else {
        length.saturating_sub(max_bytes)
    };
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let scanned_bytes = bytes.len() as u64;

    if start > 0 {
        if let Some(first_newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(0..=first_newline);
        } else {
            bytes.clear();
        }
    }

    let text = String::from_utf8_lossy(&bytes);
    let mut parsed = Vec::new();
    let mut malformed = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(value) if value.is_object() => parsed.push(value),
            Ok(_) | Err(_) => malformed += 1,
        }
    }
    Ok(TailRead {
        scanned_bytes,
        parsed,
        malformed,
    })
}

fn merge_read(result: &mut SourceDiagnostics, read: TailRead) {
    result.stats.scanned_bytes = result
        .stats
        .scanned_bytes
        .saturating_add(read.scanned_bytes);
    result.stats.parsed = result.stats.parsed.saturating_add(read.parsed.len());
    result.stats.malformed = result.stats.malformed.saturating_add(read.malformed);
    result.events.extend(read.parsed);
}

fn is_error_event(event: &Value) -> bool {
    let explicit = event
        .get("level")
        .and_then(Value::as_str)
        .is_some_and(|level| level.eq_ignore_ascii_case("error"));
    if explicit {
        return true;
    }
    event
        .get("event")
        .or_else(|| event.get("kind"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .is_some_and(|name| {
            name == "error"
                || name.ends_with("_error")
                || name.ends_with("_failed")
                || name.ends_with("_failure")
        })
}

fn event_timestamp(event: &Value) -> Option<i64> {
    event.get("ts_ms").and_then(Value::as_i64).or_else(|| {
        event
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
            .map(|timestamp| timestamp.timestamp_millis())
    })
}

fn apply_global_event_policy(sources: &mut [SourceDiagnostics], max_events: usize) {
    let mut total_events = sources
        .iter()
        .map(|source| source.events.len())
        .sum::<usize>();
    while total_events > max_events {
        let Some(source_index) = oldest_source_with_events(sources) else {
            break;
        };
        sources[source_index].events.remove(0);
        sources[source_index].stats.dropped = sources[source_index].stats.dropped.saturating_add(1);
        total_events -= 1;
    }

    let latest_error = sources
        .iter()
        .enumerate()
        .flat_map(|(source_index, source)| {
            source
                .events
                .iter()
                .filter(|event| is_error_event(event))
                .filter_map(move |event| {
                    event_timestamp(event).map(|timestamp| (timestamp, source_index))
                })
        })
        .max_by_key(|(timestamp, source_index)| (*timestamp, *source_index));

    if let Some((anchor_timestamp, anchor_source)) = latest_error {
        sources[anchor_source].stats.error_anchor_found = true;
        for source in sources {
            let before = source.events.len();
            source.events.retain(|event| {
                event_timestamp(event).is_none_or(|timestamp| timestamp >= anchor_timestamp)
            });
            source.stats.dropped = source
                .stats
                .dropped
                .saturating_add(before - source.events.len());
        }
        return;
    }

    // Legacy rows should normally carry `ts_ms`. If an error row does not,
    // preserve deterministic usefulness by anchoring only its own source;
    // cross-source ordering cannot be inferred safely without a timestamp.
    for source in sources {
        if let Some(error_index) = source.events.iter().rposition(is_error_event) {
            source.stats.error_anchor_found = true;
            if error_index > 0 {
                source.events.drain(0..error_index);
                source.stats.dropped = source.stats.dropped.saturating_add(error_index);
            }
        }
    }
}

fn enforce_bundle_limit(bundle: &mut DiagnosticsBundle, max_bytes: usize) {
    if max_bytes == 0 {
        for source in &mut bundle.sources {
            source.stats.dropped = source.stats.dropped.saturating_add(source.events.len());
            source.events.clear();
        }
        return;
    }

    while serialized_len(bundle) > max_bytes {
        let Some(source_index) = oldest_source_with_events(&bundle.sources) else {
            break;
        };
        bundle.sources[source_index].events.remove(0);
        bundle.sources[source_index].stats.dropped =
            bundle.sources[source_index].stats.dropped.saturating_add(1);
    }

    if serialized_len(bundle) > max_bytes {
        bundle.metadata.settings = Value::Null;
        for source in &mut bundle.sources {
            source.warnings.clear();
        }
    }
}

fn serialized_len(bundle: &DiagnosticsBundle) -> usize {
    serde_json::to_vec(bundle)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn oldest_source_with_events(sources: &[SourceDiagnostics]) -> Option<usize> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            source.events.first().map(|event| {
                let timestamp = event_timestamp(event).unwrap_or(i64::MIN);
                (index, timestamp)
            })
        })
        .min_by_key(|(index, timestamp)| (*timestamp, *index))
        .map(|(index, _)| index)
}
