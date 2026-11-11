//! Wire types of the history reader: rows, pages, transcript details and imports.
//!
//! These are the serde shapes a client exchanges with a library host. Field names and tags are
//! persisted or on the wire, so they do not change.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::HistoryRecord;

/// One history row as the client sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRow {
    pub id: String,
    pub url: String,
    /// The host's kind tag, as stored (see [`HistoryRecord::kind`]).
    pub kind: String,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub duration_s: Option<f64>,
    pub created_at: String,
    pub txt_path: Option<String>,
    pub md_path: Option<String>,
    pub srt_path: Option<String>,
    pub slide_dir: Option<String>,
    pub snippet: Option<String>,
    pub media_path: Option<String>,
    pub media_meta: Option<String>,
    pub media_action: Option<String>,
}

/// A transcript with its variants and attached media.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptDetail {
    pub id: String,
    pub url: String,
    /// The host's kind tag, as stored (see [`HistoryRecord::kind`]).
    pub kind: String,
    pub title: Option<String>,
    /// Full text on `GetTranscriptFull`; a bounded preview on `GetTranscript`
    /// (see `variants` and `GetTranscriptVariant` for the full text).
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub md_text: Option<String>,
    #[serde(default)]
    pub timed_text: Option<String>,
    #[serde(default)]
    pub timed_md_text: Option<String>,
    #[serde(default)]
    pub srt: Option<String>,
    pub slides: Vec<String>,
    pub media_path: Option<String>,
    pub media_meta: Option<String>,
    pub media_action: Option<String>,
    /// Per-variant availability and byte size. Empty on `GetTranscriptFull`.
    #[serde(default)]
    pub variants: Vec<TranscriptVariantInfo>,
    /// Seconds into the source where transcription stopped, when the host capped it. `None`
    /// means the transcript covers the full source.
    #[serde(default)]
    pub truncated_at_s: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptVariantKind {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "md_text")]
    MdText,
    #[serde(rename = "timed_text")]
    TimedText,
    #[serde(rename = "timed_md_text")]
    TimedMdText,
    #[serde(rename = "srt")]
    Srt,
}

impl TranscriptVariantKind {
    pub const ALL: [TranscriptVariantKind; 5] = [
        Self::Text,
        Self::MdText,
        Self::TimedText,
        Self::TimedMdText,
        Self::Srt,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptVariantInfo {
    pub kind: TranscriptVariantKind,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptVariant {
    pub id: String,
    pub kind: TranscriptVariantKind,
    /// `None` when the row has no such variant.
    pub text: Option<String>,
}

/// A transcript arriving over iCloud sync (WS-12), flattened from the
/// CloudKit `Transcript` record. `item_id` is the canonical cross-device
/// identity — a re-import upserts the same history row rather than
/// duplicating it. Media never syncs; `timed_json` is the opaque timing blob
/// (currently unused by the phone, kept so the field survives round-trips).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptImport {
    pub item_id: String,
    pub title: Option<String>,
    pub text: String,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub timed_json: Option<String>,
    /// Source URL or original filename.
    pub source: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub duration_s: Option<f64>,
    pub created_at_ms: i64,
    #[serde(default)]
    pub origin_device_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Default and maximum rows per history page.
pub const HISTORY_PAGE_SIZE: u32 = 100;
pub const HISTORY_PAGE_MAX: u32 = 200;

/// One page of history rows. `next_cursor` is `Some` while more rows may
/// remain; pass it back as `cursor` to continue. A page can be short (even
/// empty) with a cursor when a text search skipped many non-matching rows.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryPage {
    pub rows: Vec<HistoryRow>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// Rows matching the request's query, across all pages. Set on the first
    /// page (no cursor) only; `None` on continuation pages and from engines
    /// that predate it.
    #[serde(default)]
    pub total: Option<u64>,
}

/// Server-side order of a history page. `Newest` is the default and the only
/// order the keyset cursor walks directly; the others materialize the matching
/// rows and page by offset.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistorySort {
    #[default]
    Newest,
    Oldest,
    Longest,
    Shortest,
}

impl From<HistoryRecord> for HistoryRow {
    fn from(r: HistoryRecord) -> Self {
        HistoryRow {
            id: r.id,
            url: r.url,
            kind: r.kind,
            title: r.title,
            lang: r.lang,
            duration_s: r.duration_s,
            created_at: r.created_at,
            txt_path: r.txt_path,
            md_path: r.md_path,
            srt_path: r.srt_path,
            slide_dir: r.slide_dir,
            snippet: r.snippet,
            media_path: r.media_path,
            media_meta: r.media_meta,
            media_action: r.media_action,
        }
    }
}

/// The settings the history functions read: where the library lives and where saved media goes.
#[derive(Debug, Clone, Default)]
pub struct HistorySettings {
    /// The output folder holding `history.db` and the transcript artifacts.
    pub output_dir: String,
    /// The user's chosen media download folder, when set.
    pub downloads_dir: Option<String>,
    /// The folder saved media goes to when `downloads_dir` is unset or blank (for example the
    /// OS downloads folder plus an app subfolder). `None` falls back to `<output_dir>/downloads`.
    pub default_downloads_dir: Option<PathBuf>,
}

impl HistorySettings {
    /// The media download folder: the chosen one, else the host default, else
    /// `<output_root>/downloads`.
    pub fn media_download_dir(&self, output_root: &Path) -> PathBuf {
        self.downloads_dir
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.default_downloads_dir.clone())
            .unwrap_or_else(|| output_root.join("downloads"))
    }
}
