use crate::redact::RedactionPolicy;
use crate::scrub::scrub_value;
use crate::store::rotated_path;
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
        // Key redaction first, then recursive string scrub: a secret under an
        // innocuous key is caught by the value pass.
        policy.redaction.redact(event);
        scrub_value(event);
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
    scrub_value(&mut metadata.settings);
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
            scrub_value(event);
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
            .and_then(parse_rfc3339_millis)
    })
}

/// Unix milliseconds of an RFC 3339 timestamp (`2026-10-10T12:34:56[.fff](Z|+hh:mm)`), or
/// `None` when it is not one. Fractions past milliseconds are truncated.
fn parse_rfc3339_millis(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = b.get(range)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0_i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > month_days || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut at = 19;
    let mut millis = 0_i64;
    if b[at] == b'.' {
        let start = at + 1;
        at = start;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
        for (i, d) in b[start..at].iter().take(3).enumerate() {
            millis += i64::from(d - b'0') * [100, 10, 1][i];
        }
    }
    let offset_secs = match b.get(at..)? {
        b"Z" | b"z" => 0,
        [sign @ (b'+' | b'-'), rest @ ..] if rest.len() == 5 && rest[2] == b':' => {
            let hh = std::str::from_utf8(&rest[..2]).ok()?.parse::<i64>().ok()?;
            let mm = std::str::from_utf8(&rest[3..]).ok()?.parse::<i64>().ok()?;
            if hh > 23 || mm > 59 {
                return None;
            }
            let secs = hh * 3600 + mm * 60;
            if *sign == b'-' {
                -secs
            } else {
                secs
            }
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant), proleptic Gregorian.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    Some(secs * 1000 + millis)
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

#[cfg(test)]
mod rfc3339_tests {
    use super::parse_rfc3339_millis as parse;

    #[test]
    fn parses_utc_offsets_and_fractions() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2024-02-29T12:34:56Z"), Some(1_709_210_096_000));
        assert_eq!(
            parse("2024-02-29T12:34:56.789123Z"),
            Some(1_709_210_096_789)
        );
        assert_eq!(
            parse("2024-02-29T14:34:56.5+02:00"),
            Some(1_709_210_096_500)
        );
        assert_eq!(parse("2024-02-29T07:04:56-05:30"), Some(1_709_210_096_000));
        assert_eq!(parse("1969-12-31T23:59:59Z"), Some(-1000));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "yesterday",
            "2024-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2024-13-01T00:00:00Z",
            "2024-02-29T24:00:00Z",
            "2024-02-29T12:34:56",
            "2024-02-29T12:34:56.Z",
            "2024-02-29T12:34:56+0200",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
