//! Export of delivery records as JSON, Markdown, plain text or CSV. Every user-visible string in
//! a Markdown, text or CSV export (title, field labels, outcome and source wording, CSV column
//! names) comes in through [`ExportLabels`]; the defaults are neutral English.

use crate::delivery::{DeliveryOutcome, DeliveryRecord, DeliverySource};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryExportFormat {
    Json,
    Markdown,
    PlainText,
    Csv,
}

impl HistoryExportFormat {
    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("json") => Ok(Self::Json),
            Some(extension) if extension.eq_ignore_ascii_case("md") => Ok(Self::Markdown),
            Some(extension) if extension.eq_ignore_ascii_case("txt") => Ok(Self::PlainText),
            Some(extension) if extension.eq_ignore_ascii_case("csv") => Ok(Self::Csv),
            _ => Err("history export must use a .json, .md, .txt or .csv file".to_string()),
        }
    }
}

/// The wording of an export. Templates take named placeholders, replaced in one pass:
/// `pasted_unverified` takes `{reason}`, `copied_fallback` takes `{reason}`, `error` takes
/// `{code}` and `{message}`, `source_file_transcription` takes `{file_name}` and
/// `source_cloud_sync` takes `{device}`. An unknown placeholder is left as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportLabels {
    /// Heading of a Markdown export and first line of a text export.
    pub title: String,
    pub records: String,
    pub id: String,
    pub source: String,
    pub target: String,
    pub method: String,
    pub delivery_time: String,
    pub recording_time: String,
    pub transcript: String,
    pub pasted: String,
    pub pasted_unverified: String,
    pub copied_fallback: String,
    pub error: String,
    pub cancelled: String,
    pub draft: String,
    pub source_dictation: String,
    pub source_file_transcription: String,
    pub source_cloud_sync: String,
    pub unknown_app: String,
    /// The eleven CSV header names, in column order.
    pub csv_columns: [String; 11],
}

impl Default for ExportLabels {
    fn default() -> Self {
        let s = |value: &str| value.to_string();
        Self {
            title: s("Transcript history"),
            records: s("Records"),
            id: s("ID"),
            source: s("Source"),
            target: s("Target"),
            method: s("Method"),
            delivery_time: s("Delivery time"),
            recording_time: s("Recording time"),
            transcript: s("Transcript"),
            pasted: s("Pasted"),
            pasted_unverified: s("Pasted (unverified, {reason})"),
            copied_fallback: s("Copied fallback ({reason})"),
            error: s("Error ({code}): {message}"),
            cancelled: s("Cancelled"),
            draft: s("Draft"),
            source_dictation: s("Dictation"),
            source_file_transcription: s("File transcription: {file_name}"),
            source_cloud_sync: s("Cloud sync: {device}"),
            unknown_app: s("Unknown app"),
            csv_columns: [
                s("delivery_id"),
                s("delivered_at"),
                s("delivered_at_ms"),
                s("outcome"),
                s("source"),
                s("target"),
                s("method"),
                s("elapsed_ms"),
                s("recording_ms"),
                s("transcript"),
                s("raw_transcript"),
            ],
        }
    }
}

/// Replace `{name}` placeholders in one pass, so a substituted value is never rescanned.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let replaced = after.find('}').and_then(|close| {
            let name = &after[..close];
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value, close))
        });
        match replaced {
            Some((value, close)) => {
                output.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                output.push('{');
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

pub fn format_history(
    records: &[DeliveryRecord],
    format: HistoryExportFormat,
    labels: &ExportLabels,
) -> Result<String, String> {
    match format {
        HistoryExportFormat::Json => serde_json::to_string_pretty(records)
            .map_err(|error| format!("serialize history export: {error}")),
        HistoryExportFormat::Markdown => Ok(format_markdown(records, labels)),
        HistoryExportFormat::PlainText => Ok(format_plain_text(records, labels)),
        HistoryExportFormat::Csv => Ok(format_csv(records, labels)),
    }
}

fn timestamp(ms: u64) -> String {
    i64::try_from(ms / 1000)
        .ok()
        .and_then(|seconds| rightkit_time::DateTime::from_unix(seconds, 0))
        .filter(|time| time.year <= 9999)
        .map(|time| time.format_rfc3339())
        .unwrap_or_else(|| format!("epoch-ms:{ms}"))
}

fn outcome(record: &DeliveryRecord, labels: &ExportLabels) -> String {
    match &record.outcome {
        DeliveryOutcome::Pasted => labels.pasted.clone(),
        DeliveryOutcome::PasteSentUnconfirmed { reason } => {
            fill(&labels.pasted_unverified, &[("reason", reason.as_str())])
        }
        DeliveryOutcome::CopiedFallback { reason } => fill(
            &labels.copied_fallback,
            &[("reason", reason.to_string().as_str())],
        ),
        DeliveryOutcome::Error { code, message } => fill(
            &labels.error,
            &[("code", code.as_str()), ("message", message.as_str())],
        ),
        DeliveryOutcome::Cancelled => labels.cancelled.clone(),
        DeliveryOutcome::Draft => labels.draft.clone(),
    }
}

fn source(record: &DeliveryRecord, labels: &ExportLabels) -> String {
    match record.source.as_ref() {
        None | Some(DeliverySource::Dictation) => labels.source_dictation.clone(),
        Some(DeliverySource::FileTranscription { file_name }) => fill(
            &labels.source_file_transcription,
            &[("file_name", file_name.as_str())],
        ),
        Some(DeliverySource::CloudSync { device }) => {
            fill(&labels.source_cloud_sync, &[("device", device.as_str())])
        }
    }
}

fn target(record: &DeliveryRecord, labels: &ExportLabels) -> String {
    let app = record
        .target
        .process_name
        .as_deref()
        .or(record.target.window_title.as_deref())
        .unwrap_or(labels.unknown_app.as_str());
    match record.target.window_title.as_deref() {
        Some(window) if record.target.process_name.as_deref() != Some(window) => {
            format!("{app} — {window}")
        }
        _ => app.to_string(),
    }
}

fn transcript_blockquote(text: &str) -> String {
    if text.is_empty() {
        return ">".to_string();
    }
    text.lines()
        .map(|line| format!("> {}", line.replace('\r', "")))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_markdown(records: &[DeliveryRecord], labels: &ExportLabels) -> String {
    let mut output = format!(
        "# {}\n\n{}: {}\n\n",
        labels.title,
        labels.records,
        records.len()
    );
    for (index, record) in records.iter().enumerate() {
        output.push_str(&format!(
            "## {} — {}\n\n{}\n\n- {}: `{}`\n- {}: {}\n- {}: {}\n- {}: {:?}\n- {}: {} ms\n",
            timestamp(record.delivered_at_ms),
            outcome(record, labels),
            transcript_blockquote(&record.transcript),
            labels.id,
            record.delivery_id,
            labels.source,
            source(record, labels),
            labels.target,
            target(record, labels),
            labels.method,
            record.method,
            labels.delivery_time,
            record.elapsed_ms,
        ));
        if let Some(recording_ms) = record.recording_ms {
            output.push_str(&format!(
                "- {}: {} ms\n",
                labels.recording_time, recording_ms
            ));
        }
        if index + 1 < records.len() {
            output.push('\n');
        }
    }
    output
}

fn format_plain_text(records: &[DeliveryRecord], labels: &ExportLabels) -> String {
    let mut output = format!(
        "{}\n{}\n{}: {}\n\n",
        labels.title,
        "=".repeat(labels.title.chars().count()),
        labels.records,
        records.len()
    );
    for (index, record) in records.iter().enumerate() {
        output.push_str(&format!(
            "[{}] {}\n{}: {}\n{}: {}\n{}: {}\n{}: {:?}\n{}: {} ms\n",
            timestamp(record.delivered_at_ms),
            outcome(record, labels),
            labels.id,
            record.delivery_id,
            labels.source,
            source(record, labels),
            labels.target,
            target(record, labels),
            labels.method,
            record.method,
            labels.delivery_time,
            record.elapsed_ms,
        ));
        if let Some(recording_ms) = record.recording_ms {
            output.push_str(&format!("{}: {} ms\n", labels.recording_time, recording_ms));
        }
        output.push_str(&format!("{}:\n", labels.transcript));
        output.push_str(&record.transcript);
        output.push('\n');
        if index + 1 < records.len() {
            output.push_str("\n--------------------\n\n");
        }
    }
    output
}

fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn format_csv(records: &[DeliveryRecord], labels: &ExportLabels) -> String {
    let mut output = labels
        .csv_columns
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    output.push('\n');
    for record in records {
        let fields = [
            record.delivery_id.clone(),
            timestamp(record.delivered_at_ms),
            record.delivered_at_ms.to_string(),
            outcome(record, labels),
            source(record, labels),
            target(record, labels),
            format!("{:?}", record.method),
            record.elapsed_ms.to_string(),
            record
                .recording_ms
                .map(|value| value.to_string())
                .unwrap_or_default(),
            record.transcript.clone(),
            record.raw_transcript.clone().unwrap_or_default(),
        ];
        output.push_str(
            &fields
                .iter()
                .map(|value| csv_field(value))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push('\n');
    }
    output
}
