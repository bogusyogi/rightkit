use rightkit_logs::{redact_quoted, scrub_text, scrub_value};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CRASH_SCHEMA_VERSION: u32 = 1;
const MAX_MESSAGE_CHARS: usize = 2_000;
const MAX_BACKTRACE_BYTES: usize = 48 * 1024;

/// A crash report that is already redacted when constructed: there is no
/// "raw" form to leak. Contains process/thread state only, never app content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashReport {
    pub schema_version: u32,
    /// `panic`, `hang`, `sidecar_crash`, or an app-defined fixed string.
    pub kind: String,
    pub app_name: String,
    pub app_version: String,
    pub build: Option<String>,
    pub os: String,
    pub arch: String,
    pub unix_time: u64,
    pub pid: u32,
    pub thread: String,
    pub location: Option<String>,
    pub message: String,
    pub backtrace: String,
    /// File names (not paths) of local-only native captures next to this report.
    #[serde(default)]
    pub native_artifacts: Vec<String>,
    /// Set when a native capture was attempted but not kept cleanly, for example
    /// `sample_publish_failed` (the sample could not be scrubbed and published, so none was kept).
    /// A fixed code, optionally followed by a leftover raw file's name; never a path or content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_capture_note: Option<String>,
    /// Which process boundary produced the report (`window`, `engine`, `helper`), when the host
    /// declared one. A fixed code, never a path or content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_kind: Option<String>,
}

pub(crate) struct RawCrash<'a> {
    pub kind: &'a str,
    pub app_name: &'a str,
    pub app_version: &'a str,
    pub build: Option<&'a str>,
    pub thread: &'a str,
    pub location: Option<&'a str>,
    pub message: &'a str,
    pub backtrace: &'a str,
    pub process_kind: Option<&'a str>,
}

impl CrashReport {
    pub(crate) fn redacted(raw: RawCrash<'_>, unix_time: u64) -> Self {
        let message: String = scrub_text(&redact_quoted(raw.message))
            .chars()
            .take(MAX_MESSAGE_CHARS)
            .collect();
        let mut backtrace = scrub_text(raw.backtrace);
        if backtrace.len() > MAX_BACKTRACE_BYTES {
            let mut cut = MAX_BACKTRACE_BYTES;
            while !backtrace.is_char_boundary(cut) {
                cut -= 1;
            }
            backtrace.truncate(cut);
            backtrace.push_str("\n[truncated]");
        }
        Self {
            schema_version: CRASH_SCHEMA_VERSION,
            kind: sanitize_kind(raw.kind),
            app_name: scrub_text(raw.app_name),
            app_version: scrub_text(raw.app_version),
            build: raw.build.map(scrub_text),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            unix_time,
            pid: std::process::id(),
            thread: scrub_text(raw.thread),
            location: raw.location.map(scrub_text),
            message,
            backtrace,
            native_artifacts: Vec::new(),
            native_capture_note: None,
            process_kind: raw.process_kind.map(sanitize_kind),
        }
    }

    /// Final defence-in-depth pass over the serialized form, used again before
    /// anything is queued or bundled.
    pub fn to_scrubbed_value(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        scrub_value(&mut value);
        value
    }
}

/// App-supplied secret strings (API keys, database keys, tokens) removed from every report and
/// native text artifact, on top of the pattern scrub. Held sorted longest first so a secret that
/// contains another is removed whole. `Debug` never prints the values.
#[derive(Clone, Default)]
pub(crate) struct SecretList(Vec<String>);

impl std::fmt::Debug for SecretList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretList({} values)", self.0.len())
    }
}

impl SecretList {
    /// Adds `values`; empty strings are ignored (they would match everywhere).
    pub(crate) fn extend(&mut self, values: Vec<String>) {
        self.0.extend(values.into_iter().filter(|v| !v.is_empty()));
        self.0.sort_by_key(|v| std::cmp::Reverse(v.len()));
        self.0.dedup();
    }

    pub(crate) fn as_slice(&self) -> &[String] {
        &self.0
    }
}

/// Replaces every occurrence of each known secret in `text` with `[redacted]`.
pub(crate) fn redact_known(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() && out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), "[redacted]");
        }
    }
    out
}

/// Applies [`redact_known`] to every string in `value`, object keys included.
pub(crate) fn redact_known_value(value: &mut Value, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    match value {
        Value::String(s) => *s = redact_known(s, secrets),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|v| redact_known_value(v, secrets)),
        Value::Object(map) => {
            let old = std::mem::take(map);
            for (key, mut v) in old {
                redact_known_value(&mut v, secrets);
                map.insert(redact_known(&key, secrets), v);
            }
        }
        _ => {}
    }
}

/// `kind` lands in file names: keep a small safe alphabet.
pub(crate) fn sanitize_kind(kind: &str) -> String {
    let cleaned: String = kind
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect();
    if cleaned.is_empty() {
        "unknown".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_note_is_omitted_when_empty_and_older_reports_still_parse() {
        let report = CrashReport::redacted(
            RawCrash {
                kind: "panic",
                app_name: "App",
                app_version: "1",
                build: None,
                thread: "main",
                location: None,
                message: "boom",
                backtrace: "",
                process_kind: None,
            },
            1,
        );
        let mut value = serde_json::to_value(&report).unwrap();
        assert!(value.get("native_capture_note").is_none());
        value.as_object_mut().unwrap().remove("native_artifacts");
        let parsed: CrashReport = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.native_capture_note, None);

        let mut noted = report;
        noted.native_capture_note = Some("sample_publish_failed".into());
        let value = noted.to_scrubbed_value();
        assert_eq!(value["native_capture_note"], "sample_publish_failed");
    }

    #[test]
    fn known_secrets_are_removed_longest_first_in_text_and_nested_values() {
        let mut list = SecretList::default();
        list.extend(vec![
            String::new(),
            "hunter2".into(),
            "hunter2-and-more".into(),
        ]);
        assert_eq!(list.as_slice().len(), 2, "the empty string is ignored");
        assert_eq!(
            redact_known("k=hunter2-and-more, j=hunter2", list.as_slice()),
            "k=[redacted], j=[redacted]"
        );
        let mut value = serde_json::json!({"a": ["x hunter2"], "hunter2": {"b": "hunter2"}});
        redact_known_value(&mut value, list.as_slice());
        assert_eq!(
            value,
            serde_json::json!({"a": ["x [redacted]"], "[redacted]": {"b": "[redacted]"}})
        );
        assert_eq!(format!("{list:?}"), "SecretList(2 values)");
    }
}
