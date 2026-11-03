//! Canonical source-anchor evidence & safe pull-request projection (A14).
//!
//! A canonical anchor binds a finding to exact original bytes: repository /
//! snapshot identity, whole-file digest, source side, a half-open byte range
//! & the line/column range derived from those bytes. Byte offsets are
//! authoritative; lines & columns are derived, never trusted from a claim.
//! Columns are 1-based Unicode-scalar counts (a tab counts once; no display
//! expansion). A leading UTF-8 BOM is a zero-width prefix of line 1: byte
//! offsets still count from the first file byte, so line 1 content starts at
//! byte 3 in a BOM file.
//!
//! This is an additive, versioned evidence extension. Anchors without it
//! (legacy v1/v2) are untouched & keep their existing meaning.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::report::LineRange;

pub const SOURCE_ANCHOR_EVIDENCE_VERSION: u32 = 1;
/// Reserved `finding.evidence` key carrying [`SourceAnchorEvidence`].
pub const SOURCE_ANCHOR_EVIDENCE_KEY: &str = "sourceAnchor";
/// Reserved report extension key carrying a [`PrDiff`] for review projection.
pub const PR_DIFF_EXTENSION_KEY: &str = "prDiff";

const BOM: &[u8] = b"\xef\xbb\xbf";

/// Which revision of a path the anchored bytes belong to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceSide {
    /// The selected (head) revision.
    Selected,
    /// The base revision a diff is taken against.
    Base,
}

impl SourceSide {
    pub fn label(self) -> &'static str {
        match self {
            SourceSide::Selected => "selected",
            SourceSide::Base => "base",
        }
    }
}

/// Half-open byte range `[start, end)` into the original file bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Canonical, byte-exact evidence for one positioned anchor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAnchorEvidence {
    pub version: u32,
    /// Repository identity the snapshot was taken from, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Frozen snapshot key the bytes were read from, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_key: Option<String>,
    pub path: String,
    pub side: SourceSide,
    /// `sha256:` digest of the whole original file bytes on `side`.
    pub file_digest: String,
    pub byte_range: ByteRange,
    /// 1-based inclusive line range derived from `byte_range`.
    pub lines: LineRange,
    /// 1-based scalar column of `byte_range.start` on `lines.start`.
    pub start_column: u32,
    /// 1-based scalar column of `byte_range.end` (exclusive) on `lines.end`.
    pub end_column: u32,
    /// `sha256:` digest of exactly `bytes[byte_range]`, terminators included.
    pub span_digest: String,
}

/// Outcome of validating canonical anchor evidence. Only `Validated` may be
/// treated as positioned; every other variant is a recorded failure, never a
/// silent rebase or a clean result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnchorValidation {
    Validated,
    UnsupportedVersion,
    UnsafePath,
    EmptyDigest,
    SnapshotMismatch,
    MissingFile,
    FileDigestMismatch,
    NonUtf8File,
    ReversedRange,
    ByteRangeOutOfBounds,
    /// A byte offset splits a multi-byte scalar.
    InvalidUtf8Boundary,
    /// A byte offset falls between the `\r` & `\n` of a CRLF terminator.
    InsideLineTerminator,
    LineColumnMismatch,
    SpanDigestMismatch,
}

impl AnchorValidation {
    pub fn is_validated(self) -> bool {
        self == AnchorValidation::Validated
    }

    /// Stable machine code for recording a failed position separately.
    pub fn code(self) -> &'static str {
        match self {
            AnchorValidation::Validated => "validated",
            AnchorValidation::UnsupportedVersion => "unsupported-version",
            AnchorValidation::UnsafePath => "unsafe-path",
            AnchorValidation::EmptyDigest => "empty-digest",
            AnchorValidation::SnapshotMismatch => "snapshot-mismatch",
            AnchorValidation::MissingFile => "missing-file",
            AnchorValidation::FileDigestMismatch => "file-digest-mismatch",
            AnchorValidation::NonUtf8File => "non-utf8-file",
            AnchorValidation::ReversedRange => "reversed-range",
            AnchorValidation::ByteRangeOutOfBounds => "byte-range-out-of-bounds",
            AnchorValidation::InvalidUtf8Boundary => "invalid-utf8-boundary",
            AnchorValidation::InsideLineTerminator => "inside-line-terminator",
            AnchorValidation::LineColumnMismatch => "line-column-mismatch",
            AnchorValidation::SpanDigestMismatch => "span-digest-mismatch",
        }
    }
}

impl std::fmt::Display for AnchorValidation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// Position derived from original bytes for one byte range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedPosition {
    pub lines: LineRange,
    pub start_column: u32,
    pub end_column: u32,
    pub file_digest: String,
    pub span_digest: String,
}

/// A snapshot-relative path: no absolute prefix, drive/colon, backslash, NUL,
/// or empty / `.` / `..` components. Non-ASCII paths are valid.
pub fn safe_snapshot_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn bom_len(bytes: &[u8]) -> usize {
    if bytes.starts_with(BOM) {
        BOM.len()
    } else {
        0
    }
}

/// `(content_start, content_end)` per line; `content_end` excludes the
/// `\r\n` / `\n` terminator. Matches the legacy line split: a trailing
/// newline does not create a phantom final line, & an empty file has none.
fn line_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut start = bom_len(bytes);
    if bytes.is_empty() {
        return spans;
    }
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            let mut end = index;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            spans.push((start, end));
            start = index + 1;
        }
        index += 1;
    }
    if start < bytes.len() {
        spans.push((start, bytes.len()));
    }
    spans
}

fn line_column_at(
    text: &str,
    spans: &[(usize, usize)],
    offset: usize,
) -> Result<(u32, u32), AnchorValidation> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return Err(AnchorValidation::ByteRangeOutOfBounds);
    }
    if !text.is_char_boundary(offset) {
        return Err(AnchorValidation::InvalidUtf8Boundary);
    }
    if offset > 0 && offset < bytes.len() && bytes[offset - 1] == b'\r' && bytes[offset] == b'\n' {
        return Err(AnchorValidation::InsideLineTerminator);
    }
    if spans.is_empty() {
        // Empty file (or BOM only): the sole representable offset is the end.
        return Ok((1, 1));
    }
    if offset <= bom_len(bytes) {
        return Ok((1, 1));
    }
    // Line whose content start is the greatest start <= offset. An offset on
    // a terminator belongs to the line it terminates; an offset exactly at
    // EOF after a final newline is clamped to the last real line's end.
    let mut line = spans.partition_point(|(start, _)| *start <= offset);
    if line == 0 {
        line = 1;
    }
    if offset == bytes.len() && line > spans.len() {
        line = spans.len();
    }
    let line = line.min(spans.len());
    let (start, content_end) = spans[line - 1];
    // An offset on (or, at EOF, after) the terminator still measures only
    // the line's content.
    let upto = offset.min(content_end).max(start);
    let column = text[start..upto].chars().count() as u32 + 1;
    Ok((line as u32, column))
}

/// Derives lines, columns & digests for `range` from the original bytes.
/// Fails closed on non-UTF-8 bytes, reversed / out-of-bounds ranges, offsets
/// that split a scalar & offsets inside a CRLF terminator.
pub fn derive_position(
    bytes: &[u8],
    range: ByteRange,
) -> Result<DerivedPosition, AnchorValidation> {
    if range.end < range.start {
        return Err(AnchorValidation::ReversedRange);
    }
    if range.end > bytes.len() as u64 {
        return Err(AnchorValidation::ByteRangeOutOfBounds);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| AnchorValidation::NonUtf8File)?;
    let spans = line_spans(text);
    let (start_line, start_column) = line_column_at(text, &spans, range.start as usize)?;
    let (end_line, end_column) = line_column_at(text, &spans, range.end as usize)?;
    Ok(DerivedPosition {
        lines: LineRange {
            start: start_line,
            end: end_line,
        },
        start_column,
        end_column,
        file_digest: sha256_hex(bytes),
        span_digest: sha256_hex(&bytes[range.start as usize..range.end as usize]),
    })
}

/// Byte range covering the content of `lines` (terminators excluded, except
/// terminators between the first & last line, which lie inside the range).
pub fn line_byte_range(bytes: &[u8], lines: LineRange) -> Option<ByteRange> {
    if lines.start == 0 || lines.end < lines.start {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let spans = line_spans(text);
    let (start, _) = *spans.get(lines.start as usize - 1)?;
    let (_, end) = *spans.get(lines.end as usize - 1)?;
    Some(ByteRange {
        start: start as u64,
        end: end as u64,
    })
}

/// Builds canonical evidence from original bytes. The claimed position is
/// the byte range; lines & columns are always derived here.
pub fn build_evidence(
    path: &str,
    side: SourceSide,
    repository: Option<&str>,
    snapshot_key: Option<&str>,
    bytes: &[u8],
    byte_range: ByteRange,
) -> Result<SourceAnchorEvidence, AnchorValidation> {
    if !safe_snapshot_path(path) {
        return Err(AnchorValidation::UnsafePath);
    }
    let derived = derive_position(bytes, byte_range)?;
    Ok(SourceAnchorEvidence {
        version: SOURCE_ANCHOR_EVIDENCE_VERSION,
        repository: repository.map(ToOwned::to_owned),
        snapshot_key: snapshot_key.map(ToOwned::to_owned),
        path: path.to_owned(),
        side,
        file_digest: derived.file_digest,
        byte_range,
        lines: derived.lines,
        start_column: derived.start_column,
        end_column: derived.end_column,
        span_digest: derived.span_digest,
    })
}

/// Structural validation that needs no file bytes: version, path safety,
/// recorded snapshot identity, non-empty digests & sane ranges. Byte-level
/// truth still requires [`validate_evidence_against_bytes`].
pub fn validate_evidence_claims(
    evidence: &SourceAnchorEvidence,
    expected_snapshot_key: Option<&str>,
) -> AnchorValidation {
    if evidence.version != SOURCE_ANCHOR_EVIDENCE_VERSION {
        return AnchorValidation::UnsupportedVersion;
    }
    if !safe_snapshot_path(&evidence.path) {
        return AnchorValidation::UnsafePath;
    }
    if evidence.file_digest.trim().is_empty() || evidence.span_digest.trim().is_empty() {
        return AnchorValidation::EmptyDigest;
    }
    if let Some(expected) = expected_snapshot_key {
        if evidence.snapshot_key.as_deref() != Some(expected) {
            return AnchorValidation::SnapshotMismatch;
        }
    }
    if evidence.byte_range.end < evidence.byte_range.start {
        return AnchorValidation::ReversedRange;
    }
    if evidence.lines.start == 0
        || evidence.lines.end < evidence.lines.start
        || evidence.start_column == 0
        || evidence.end_column == 0
    {
        return AnchorValidation::LineColumnMismatch;
    }
    AnchorValidation::Validated
}

/// Full validation against the original bytes of the claimed side. `bytes`
/// is `None` when the path is outside the snapshot; that is `MissingFile`,
/// never a pass. A snapshot or file-digest mismatch rejects the reference —
/// it is never silently rebased onto different bytes.
pub fn validate_evidence_against_bytes(
    evidence: &SourceAnchorEvidence,
    expected_snapshot_key: Option<&str>,
    bytes: Option<&[u8]>,
) -> AnchorValidation {
    let claims = validate_evidence_claims(evidence, expected_snapshot_key);
    if !claims.is_validated() {
        return claims;
    }
    let Some(bytes) = bytes else {
        return AnchorValidation::MissingFile;
    };
    match derive_position(bytes, evidence.byte_range) {
        Err(failure) => failure,
        Ok(derived) => {
            if derived.file_digest != evidence.file_digest {
                AnchorValidation::FileDigestMismatch
            } else if derived.lines != evidence.lines
                || derived.start_column != evidence.start_column
                || derived.end_column != evidence.end_column
            {
                AnchorValidation::LineColumnMismatch
            } else if derived.span_digest != evidence.span_digest {
                AnchorValidation::SpanDigestMismatch
            } else {
                AnchorValidation::Validated
            }
        }
    }
}

/// Where an exact span occurs in a file. Repeated text is `Ambiguous`; the
/// first occurrence is never chosen automatically.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanLocation {
    Unique(ByteRange),
    Ambiguous { occurrences: u64 },
    NotFound,
    EmptySpan,
}

pub fn locate_span(bytes: &[u8], span: &[u8]) -> SpanLocation {
    if span.is_empty() {
        return SpanLocation::EmptySpan;
    }
    if span.len() > bytes.len() {
        return SpanLocation::NotFound;
    }
    let mut found: Option<ByteRange> = None;
    let mut occurrences = 0u64;
    for (index, window) in bytes.windows(span.len()).enumerate() {
        if window == span {
            occurrences += 1;
            if found.is_none() {
                found = Some(ByteRange {
                    start: index as u64,
                    end: (index + span.len()) as u64,
                });
            }
        }
    }
    match (found, occurrences) {
        (Some(range), 1) => SpanLocation::Unique(range),
        (_, 0) => SpanLocation::NotFound,
        _ => SpanLocation::Ambiguous { occurrences },
    }
}

/// Result of attempting to carry a confirmed anchor to a new revision. The
/// original evidence is never mutated: a relocation is only a `Candidate`
/// that a verifier must re-derive & re-confirm in its new context before it
/// can replace the confirmed position. Failed & ambiguous outcomes are
/// recorded as themselves.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "outcome"
)]
pub enum ReanchorOutcome {
    /// Same file bytes & same byte range still validate on the new snapshot.
    Unchanged {
        evidence: SourceAnchorEvidence,
    },
    /// The exact span occurs exactly once, at a different offset. Lineage
    /// only; not a confirmed finding position until reverified.
    Candidate {
        candidate: SourceAnchorEvidence,
        previous_byte_range: ByteRange,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_snapshot_key: Option<String>,
    },
    Ambiguous {
        occurrences: u64,
    },
    NotFound,
    Invalid {
        reason: AnchorValidation,
    },
}

/// Proposes lineage for `original` on new bytes. `span_bytes` must be the
/// exact bytes the original anchor covered (checked against its span
/// digest); changed source or context yields `NotFound`/`Ambiguous`, never
/// a fuzzy first match.
pub fn propose_reanchor(
    original: &SourceAnchorEvidence,
    new_repository: Option<&str>,
    new_snapshot_key: Option<&str>,
    new_bytes: Option<&[u8]>,
    span_bytes: &[u8],
) -> ReanchorOutcome {
    if sha256_hex(span_bytes) != original.span_digest {
        return ReanchorOutcome::Invalid {
            reason: AnchorValidation::SpanDigestMismatch,
        };
    }
    let Some(new_bytes) = new_bytes else {
        return ReanchorOutcome::Invalid {
            reason: AnchorValidation::MissingFile,
        };
    };
    // Unchanged bytes at the same offset re-derive to identical evidence
    // (apart from snapshot identity, which the caller re-records).
    if sha256_hex(new_bytes) == original.file_digest {
        return match build_evidence(
            &original.path,
            original.side,
            new_repository.or(original.repository.as_deref()),
            new_snapshot_key.or(original.snapshot_key.as_deref()),
            new_bytes,
            original.byte_range,
        ) {
            Ok(evidence) => ReanchorOutcome::Unchanged { evidence },
            Err(reason) => ReanchorOutcome::Invalid { reason },
        };
    }
    match locate_span(new_bytes, span_bytes) {
        SpanLocation::Unique(range) => match build_evidence(
            &original.path,
            original.side,
            new_repository.or(original.repository.as_deref()),
            new_snapshot_key,
            new_bytes,
            range,
        ) {
            // Any relocation (even at the same offset under changed
            // surrounding bytes) is only a candidate until reverified.
            Ok(candidate) => ReanchorOutcome::Candidate {
                candidate,
                previous_byte_range: original.byte_range,
                previous_snapshot_key: original.snapshot_key.clone(),
            },
            Err(reason) => ReanchorOutcome::Invalid { reason },
        },
        SpanLocation::Ambiguous { occurrences } => ReanchorOutcome::Ambiguous { occurrences },
        SpanLocation::NotFound | SpanLocation::EmptySpan => ReanchorOutcome::NotFound,
    }
}

// ----- pull-request projection -----------------------------------------------

/// One file of an exact PR diff. `base_path` / `head_path` are `None` for an
/// added / deleted file; a rename carries both. Hunk line ranges are the
/// 1-based inclusive lines of that side present in the diff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrDiffFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_path: Option<String>,
    #[serde(default)]
    pub base_hunks: Vec<LineRange>,
    #[serde(default)]
    pub head_hunks: Vec<LineRange>,
}

/// The exact diff a review comment would be attached to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrDiff {
    /// PR head the diff was computed at; must equal the report's head.
    pub head_oid: String,
    pub files: Vec<PrDiffFile>,
}

/// Why a validated finding cannot be an inline PR comment. In every case the
/// finding remains a summary finding; it is never erased or moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrProjectionReason {
    NotValidated,
    HeadMismatch,
    PathNotInDiff,
    /// The anchored path does not exist on the evidence's side of the diff.
    SideFileMissing,
    LinesOutsideDiff,
    /// Lines are in the diff but not within one contiguous hunk.
    LinesNonContiguousInDiff,
}

impl PrProjectionReason {
    pub fn code(self) -> &'static str {
        match self {
            PrProjectionReason::NotValidated => "not-validated",
            PrProjectionReason::HeadMismatch => "head-mismatch",
            PrProjectionReason::PathNotInDiff => "path-not-in-diff",
            PrProjectionReason::SideFileMissing => "side-file-missing",
            PrProjectionReason::LinesOutsideDiff => "lines-outside-diff",
            PrProjectionReason::LinesNonContiguousInDiff => "lines-noncontiguous-in-diff",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "projection"
)]
pub enum PrProjection {
    /// Representable as an inline comment on this exact diff. `line` is the
    /// final line on `side`; `start_line` is set only for a multi-line span
    /// wholly inside one hunk. Selected maps to the platform's right side,
    /// Base to its left/deleted side.
    Inline {
        path: String,
        side: SourceSide,
        line: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_line: Option<u32>,
    },
    SummaryOnly {
        reason: PrProjectionReason,
    },
}

/// Projects validated evidence onto an exact PR diff. `validation` must be
/// the byte-level [`validate_evidence_against_bytes`] result for the side's
/// bytes (a caller without bytes can only ever obtain `SummaryOnly` by
/// passing its real validation outcome). `report_head` is the head recorded
/// in the report scope; a PR whose head has moved gets no inline comments.
pub fn project_to_pr(
    evidence: &SourceAnchorEvidence,
    validation: AnchorValidation,
    report_head: &str,
    diff: &PrDiff,
) -> PrProjection {
    let summary = |reason| PrProjection::SummaryOnly { reason };
    if !validation.is_validated() {
        return summary(PrProjectionReason::NotValidated);
    }
    if diff.head_oid != report_head {
        return summary(PrProjectionReason::HeadMismatch);
    }
    let (wanted_path, hunks_of) = match evidence.side {
        SourceSide::Selected => (
            evidence.path.as_str(),
            (|file: &PrDiffFile| (&file.head_path, &file.head_hunks))
                as fn(&PrDiffFile) -> (&Option<String>, &Vec<LineRange>),
        ),
        SourceSide::Base => (
            evidence.path.as_str(),
            (|file: &PrDiffFile| (&file.base_path, &file.base_hunks))
                as fn(&PrDiffFile) -> (&Option<String>, &Vec<LineRange>),
        ),
    };
    let mut path_in_diff = false;
    let mut side_file: Option<&PrDiffFile> = None;
    for file in &diff.files {
        let matches_either = file.head_path.as_deref() == Some(wanted_path)
            || file.base_path.as_deref() == Some(wanted_path);
        if matches_either {
            path_in_diff = true;
        }
        let (side_path, _) = hunks_of(file);
        if side_path.as_deref() == Some(wanted_path) {
            side_file = Some(file);
        }
    }
    let Some(file) = side_file else {
        return summary(if path_in_diff {
            PrProjectionReason::SideFileMissing
        } else {
            PrProjectionReason::PathNotInDiff
        });
    };
    let (_, hunks) = hunks_of(file);
    let lines = evidence.lines;
    let in_one_hunk = hunks
        .iter()
        .any(|hunk| hunk.start <= lines.start && lines.end <= hunk.end);
    if in_one_hunk {
        return PrProjection::Inline {
            path: evidence.path.clone(),
            side: evidence.side,
            line: lines.end,
            start_line: (lines.start < lines.end).then_some(lines.start),
        };
    }
    let covered_at_all = (lines.start..=lines.end).all(|line| {
        hunks
            .iter()
            .any(|hunk| hunk.start <= line && line <= hunk.end)
    });
    summary(if covered_at_all {
        PrProjectionReason::LinesNonContiguousInDiff
    } else {
        PrProjectionReason::LinesOutsideDiff
    })
}
