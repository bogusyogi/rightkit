//! PDF editor save protocol (Dispatch C, lane C2): `prepare_pdf_save`'s structural recheck,
//! save-token lifecycle, and `commit_pdf_save`'s atomic write — `docs/PDF-EDITOR-NOW.md` §10.
//!
//! Owns the two-phase save handshake described in §10: Rust composes and structurally verifies a
//! candidate before ever handing it back (§10.1), the frontend independently renders/verifies it
//! (§10.2, entirely frontend-side — nothing here), and only then does a short-lived, single-use
//! token authorize one atomic disk write (§10.3). This file consumes the frozen
//! `compose_candidate` seam (`pdf_compose.rs`, lane C1) and the shared wire types it depends on
//! (`pdf_model.rs`, lane A1); it defines no new PDF-composition logic of its own.
//!
//! ## Judgment calls this file had to make (§10/§4/§14.1 leave these open)
//!
//! - **Byte channel shape for `prepare_pdf_save` (§4).** Incoming annotation bytes use base64 in
//!   normal JSON because WKWebView cannot complete this command with Tauri's raw request body.
//!   Candidate bytes return as a JSON byte array in the same response. This is larger than a raw
//!   response but reliable in WKWebView, while the raw response path fails during stream decoding.
//! - **No `AppState` exists in this codebase** (confirmed absent by `pdf_font.rs`'s and
//!   `pdf_inspect.rs`'s own notes) — this module introduces [`PdfSaveTokenStore`] as the Tauri
//!   `.manage()`-registered state for in-flight save tokens; wiring that `.manage()` call belongs
//!   to whichever lane owns `lib.rs`, out of this file's scope.
//! - **"Document session" (§10.3: "at most two candidates per document session"; §10.3: "token
//!   ... is invalidated after any source revision")** is not a concept this codebase threads
//!   anywhere yet, so both rules are scoped to `source_path`: at most two live tokens per source
//!   path (oldest evicted first), and a *successful* [`commit_pdf_save`] for a path invalidates
//!   every other outstanding token for that same path (the source's on-disk bytes just changed
//!   out from under them).
//! - **§11's frozen error taxonomy has no dedicated "token expired/unknown/replayed" variant.**
//!   Every token-lifecycle failure in [`commit_pdf_save`] (unknown token, expired token, already-
//!   consumed/replayed token, source changed since prepare) maps to
//!   [`PdfEditError::WriteConflict`] ("target changed before save") as the closest existing
//!   variant, per §14.1's own grouping of "expired/replayed/mismatched candidate token" as one
//!   fixture category distinct from `StaleSource` (which §10.1 reserves for the *prepare-time*
//!   source-SHA mismatch).
//! - **Single-use enforcement removes the token record on the very first `commit_pdf_save` call
//!   for it**, success or failure — a token that fails to commit (e.g. a same-path SHA race) is
//!   dead, not retryable; the caller must re-run `prepare_pdf_save` against fresh source bytes.
//!   This matches §10.3's "single-use" wording literally rather than only-consume-on-success.
//! - **Structural recheck (§10.1 step 7: "lopdf reopens candidate, checks page count/resources/
//!   content streams, & re-inspects expected text")** is implemented as: candidate parses via
//!   `lopdf::Document::load_mem`, has at least one page, every page's `/Contents` (when present)
//!   resolves without error, and every [`ExpectedTextCheck`] page has at least one text island
//!   (via `pdf_inspect::walk_page_graph`, the same walker `inspect_pdf_page` uses) whose text
//!   contains the expected substring. `ExpectedTextCheck` itself is a new type — §10.1 names the
//!   field (`expected_text: Vec<ExpectedTextCheck>`) but never defines the type; this file defines
//!   it as `{ page, text }`, the minimal shape `pdf_compose.rs` can populate per touched page and
//!   this recheck (and the frontend's `verifyPdfCandidate`, per §10.2) can check against.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::inspect::{content_stream_ids, walk_page_graph};
use crate::model::{
    document_sha256, effective_pdf_edit_commands, sha256_hex, PdfEditCommand, PdfEditError,
};

/// A save token is valid for this long after `prepare_pdf_save` issues it (§10.3).
const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

/// §10.3: "Keep at most two candidates per document session" (scoped to `source_path` — see
/// module doc comment).
const MAX_TOKENS_PER_SOURCE: usize = 2;

// -------------------------------------------------------------------------------------------
// Wire types this file owns (§10.1 names these fields but leaves their shapes to this lane)
// -------------------------------------------------------------------------------------------

/// One page-level assertion a composed candidate must satisfy before `prepare_pdf_save` will hand
/// out a token for it: page `page` must contain `text` somewhere in its extracted text. Built from
/// each [`PdfEditCommand`]'s "after" text, one entry per touched page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedTextCheck {
    pub page: u32,
    pub text: String,
}

/// JSON result returned by `prepare_pdf_save`. Candidate bytes use a numeric array because
/// WKWebView's raw-response and large-base64 decoding paths are unreliable here.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPdfSaveMeta {
    pub token: String,
    pub candidate_sha256: String,
    pub candidate_bytes: Vec<u8>,
    pub affected_pages: Vec<u32>,
    pub expected_text: Vec<ExpectedTextCheck>,
}

/// Result of a successful [`commit_pdf_save`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfCommitResult {
    pub target_path: String,
    pub document_sha256: String,
}

/// JSON request for `prepare_pdf_save`; byte content is base64 rather than a numeric JSON array.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparePdfSaveRequestMeta {
    pub source_path: String,
    pub expected_source_sha256: String,
    pub annotation_pdf_base64: String,
    pub source_commands: Vec<PdfEditCommand>,
}

// -------------------------------------------------------------------------------------------
// Token store (Tauri-managed state; see module doc comment on the missing `AppState`)
// -------------------------------------------------------------------------------------------

struct SaveTokenRecord {
    source_path: String,
    /// Source document SHA-256 at the moment this candidate was staged (i.e. the same value
    /// `stage_prepared_save`'s caller already verified as `actual_source_sha256`). Stored on the
    /// record itself so `commit_pdf_save`'s same-path recheck (§10.3) has a prepare-time value to
    /// compare against without needing a separate session ledger.
    source_sha256: String,
    candidate_bytes: Vec<u8>,
    created_at: Instant,
}

#[derive(Default)]
struct TokenStoreInner {
    tokens: HashMap<String, SaveTokenRecord>,
    /// Insertion-ordered token ids per source path, oldest first — drives §10.3's two-candidate
    /// cap.
    by_source: HashMap<String, VecDeque<String>>,
}

/// In-memory save-token registry for the prepare/commit handshake (§10.3). One instance is
/// managed as Tauri state (`app.manage(PdfSaveTokenStore::default())`, wired by whichever lane
/// owns `lib.rs`) and shared across `prepare_pdf_save` and `commit_pdf_save`.
#[derive(Default)]
pub struct PdfSaveTokenStore {
    inner: Mutex<TokenStoreInner>,
}

impl PdfSaveTokenStore {
    /// Registers a newly composed+verified candidate under a fresh token, evicting the oldest
    /// token for `source_path` if this would exceed [`MAX_TOKENS_PER_SOURCE`]. Returns the token.
    fn insert(&self, source_path: &str, source_sha256: &str, candidate_bytes: Vec<u8>) -> String {
        let token = random_save_token();
        let mut inner = self.inner.lock().expect("token store mutex poisoned");

        let mut evicted = Vec::new();
        {
            let queue = inner.by_source.entry(source_path.to_string()).or_default();
            while queue.len() >= MAX_TOKENS_PER_SOURCE {
                match queue.pop_front() {
                    Some(oldest) => evicted.push(oldest),
                    None => break,
                }
            }
        }
        for oldest in evicted {
            inner.tokens.remove(&oldest);
        }
        inner
            .by_source
            .entry(source_path.to_string())
            .or_default()
            .push_back(token.clone());
        inner.tokens.insert(
            token.clone(),
            SaveTokenRecord {
                source_path: source_path.to_string(),
                source_sha256: source_sha256.to_string(),
                candidate_bytes,
                created_at: Instant::now(),
            },
        );
        token
    }

    /// Test-only, non-consuming liveness check: `Ok(())` if `token` exists and has not expired.
    /// (The caller-less `fetch_prepared_pdf_save_candidate` command that cloned the whole
    /// candidate out through this lookup was removed; `prepare_pdf_save` already returns the bytes.)
    #[cfg(test)]
    fn peek(&self, token: &str) -> Result<(), PdfEditError> {
        let inner = self.inner.lock().expect("token store mutex poisoned");
        let record = inner.tokens.get(token).ok_or(PdfEditError::WriteConflict)?;
        if record.created_at.elapsed() > TOKEN_TTL {
            return Err(PdfEditError::WriteConflict);
        }
        Ok(())
    }

    /// Single-use consume for [`commit_pdf_save`]: removes and returns the record regardless of
    /// whether it turns out to be expired (still consumed — see module doc comment on single-use
    /// semantics). Returns `Err(WriteConflict)` for an unknown/already-consumed/replayed token,
    /// or an expired one.
    fn consume(&self, token: &str) -> Result<SaveTokenRecord, PdfEditError> {
        let mut inner = self.inner.lock().expect("token store mutex poisoned");
        let Some(record) = inner.tokens.remove(token) else {
            return Err(PdfEditError::WriteConflict);
        };
        if let Some(queue) = inner.by_source.get_mut(&record.source_path) {
            queue.retain(|t| t != token);
        }
        if record.created_at.elapsed() > TOKEN_TTL {
            return Err(PdfEditError::WriteConflict);
        }
        Ok(record)
    }

    /// §10.3 "invalidated after any source revision" (scoped to `source_path` — see module doc
    /// comment): drops every remaining token for `source_path`, e.g. after a successful commit
    /// changes that path's on-disk bytes out from under any other outstanding token.
    pub fn invalidate_source(&self, source_path: &str) {
        let mut inner = self.inner.lock().expect("token store mutex poisoned");
        if let Some(queue) = inner.by_source.remove(source_path) {
            for token in queue {
                inner.tokens.remove(&token);
            }
        }
    }

    #[cfg(test)]
    fn live_token_count(&self, source_path: &str) -> usize {
        let inner = self.inner.lock().expect("token store mutex poisoned");
        inner
            .by_source
            .get(source_path)
            .map(|q| q.len())
            .unwrap_or(0)
    }
}

/// Mirrors `path_broker.rs`'s `random_token` shape (16 random bytes, lowercase hex) so save
/// tokens look and behave like every other opaque token this codebase hands to the frontend.
fn random_save_token() -> String {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).expect("OS RNG unavailable");
    let mut token = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut token, "{byte:02x}");
    }
    token
}

// -------------------------------------------------------------------------------------------
// §10.1 — Prepare: structural recheck + staging (pure/testable core)
// -------------------------------------------------------------------------------------------

/// Verifies a just-composed candidate is structurally sound and contains every expected-text
/// assertion, then stages it in `store` under a fresh token. Separated from the `#[tauri::command]`
/// wrapper so it is directly unit-testable against hand-built [`lopdf::Document`] fixtures with no
/// filesystem/broker/IPC involved (mirrors `pdf_inspect.rs`'s `walk_page_graph`/`inspect_pdf_page`
/// split).
///
/// `actual_source_sha256` is the freshly read source bytes' digest; `expected_source_sha256` is
/// what the frontend's session believed it was editing. A mismatch means the backing file changed
/// since the edit session started (§10.1 step 2) — rejected before composition is even trusted.
pub fn stage_prepared_save(
    store: &PdfSaveTokenStore,
    source_path: &str,
    expected_source_sha256: &str,
    actual_source_sha256: &str,
    candidate_bytes: Vec<u8>,
    affected_pages: Vec<u32>,
    expected_text: Vec<ExpectedTextCheck>,
) -> Result<PreparedPdfSaveMeta, PdfEditError> {
    if expected_source_sha256 != actual_source_sha256 {
        return Err(PdfEditError::StaleSource);
    }

    structurally_verify_candidate(&candidate_bytes, &expected_text)?;

    let candidate_sha256 = sha256_hex(&candidate_bytes);
    let returned_candidate_bytes = candidate_bytes.clone();
    let token = store.insert(source_path, actual_source_sha256, candidate_bytes);

    Ok(PreparedPdfSaveMeta {
        token,
        candidate_sha256,
        candidate_bytes: returned_candidate_bytes,
        affected_pages,
        expected_text,
    })
}

/// §10.1 step 7: reopen the candidate independently of whatever composed it, and refuse to hand
/// out a token for anything that doesn't survive a fresh parse — "never trust your own composer
/// blindly." See module doc comment for exactly what this checks.
fn structurally_verify_candidate(
    candidate_bytes: &[u8],
    expected_text: &[ExpectedTextCheck],
) -> Result<(), PdfEditError> {
    let doc =
        crate::inspect::load_pdf(candidate_bytes).map_err(|_| PdfEditError::VerificationFailed)?;

    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(PdfEditError::VerificationFailed);
    }

    for page_id in pages.values() {
        let page_obj = doc
            .get_object(*page_id)
            .map_err(|_| PdfEditError::VerificationFailed)?;
        let page_dict = page_obj
            .as_dict()
            .map_err(|_| PdfEditError::VerificationFailed)?;
        if let Ok(lopdf::Object::Reference(id)) = page_dict.get(b"Resources") {
            doc.get_object(*id)
                .map_err(|_| PdfEditError::VerificationFailed)?;
        }
        if let Ok(contents) = page_dict.get(b"Contents") {
            // A stream, an array of streams, or an indirect reference to an array of streams.
            if !matches!(
                contents,
                lopdf::Object::Reference(_) | lopdf::Object::Array(_)
            ) {
                return Err(PdfEditError::VerificationFailed);
            }
            let ids =
                content_stream_ids(&doc, contents).map_err(|_| PdfEditError::VerificationFailed)?;
            for id in ids {
                let obj = doc
                    .get_object(id)
                    .map_err(|_| PdfEditError::VerificationFailed)?;
                obj.as_stream()
                    .map_err(|_| PdfEditError::VerificationFailed)?;
            }
        }
    }

    let candidate_sha256 = document_sha256(candidate_bytes);
    for check in expected_text {
        let graph = walk_page_graph(&doc, &candidate_sha256, check.page)
            .map_err(|_| PdfEditError::VerificationFailed)?;
        let found = graph
            .islands
            .iter()
            .any(|island| island.text.contains(&check.text));
        if !found {
            return Err(PdfEditError::VerificationFailed);
        }
    }

    Ok(())
}

/// Picks which bytes `prepare_pdf_save` should compose `source_commands` on top of. The frontend
/// sends pdf.js's `saveDocument()` output as `annotation_pdf`: a one-byte sentinel `[0]` when there
/// are no annotation edits, or (per pdf.js's own incremental-save format) the original file bytes
/// verbatim followed by appended annotation objects/xref when there are. Composing on top of the
/// annotation bytes (rather than discarding them) is what makes annotation edits survive Save.
pub fn select_compose_base<'a>(
    source: &'a [u8],
    annotation_pdf: &'a [u8],
) -> Result<&'a [u8], PdfEditError> {
    if annotation_pdf.len() <= 1 {
        return Ok(source);
    }
    if annotation_pdf.len() > source.len() && annotation_pdf.starts_with(source) {
        return Ok(annotation_pdf);
    }
    Err(PdfEditError::AnnotationSerializationFailed)
}

/// Derives one [`ExpectedTextCheck`] per page touched by `commands`, from each command's "after"
/// text — the substring `structurally_verify_candidate` (and the frontend's `verifyPdfCandidate`,
/// §10.2) must find on that page post-composition.
pub fn expected_text_checks(commands: &[PdfEditCommand]) -> Vec<ExpectedTextCheck> {
    effective_pdf_edit_commands(commands)
        .into_iter()
        .map(|command| match command {
            PdfEditCommand::ReplaceSourceText { locator, after, .. } => ExpectedTextCheck {
                page: locator.page,
                text: after.text.clone(),
            },
            PdfEditCommand::AddText { page, text, .. } => ExpectedTextCheck {
                page: *page,
                text: text.clone(),
            },
            PdfEditCommand::CorrectOcrLine { page, text, .. } => ExpectedTextCheck {
                page: *page,
                text: text.clone(),
            },
        })
        .collect()
}

// -------------------------------------------------------------------------------------------
// §10.3 — Commit: token resolution + atomic write (pure/testable core + Tauri command)
// -------------------------------------------------------------------------------------------

/// Resolves and consumes `token`, returning the bytes to write and the commit result — but
/// performs no filesystem I/O itself beyond what `read_current_source_for_recheck` does.
/// `read_current_source_for_recheck` is called only for a same-path Save (§10.3: "Same-path Save
/// rechecks source SHA immediately before atomic replacement") and must return the *current*
/// on-disk bytes at `source_path`, compared against the SHA-256 recorded when the token's
/// candidate was staged ([`SaveTokenRecord::source_sha256`]). Kept filesystem-free otherwise so
/// token lifecycle (expiry/replay/conflict) is directly unit-testable.
pub fn resolve_commit<F>(
    store: &PdfSaveTokenStore,
    token: &str,
    target_path: Option<String>,
    read_current_source_for_recheck: F,
) -> Result<(String, Vec<u8>, PdfCommitResult), PdfEditError>
where
    F: FnOnce(&str) -> Result<Vec<u8>, PdfEditError>,
{
    let record = store.consume(token)?;
    let target = target_path.unwrap_or_else(|| record.source_path.clone());

    if target == record.source_path {
        let current_bytes = read_current_source_for_recheck(&record.source_path)?;
        let current_sha256 = document_sha256(&current_bytes);
        if current_sha256 != record.source_sha256 {
            return Err(PdfEditError::WriteConflict);
        }
    }

    let document_sha256_value = document_sha256(&record.candidate_bytes);
    let result = PdfCommitResult {
        target_path: target.clone(),
        document_sha256: document_sha256_value,
    };
    Ok((record.source_path, record.candidate_bytes, result))
}

// -------------------------------------------------------------------------------------------
// Tests (§14.1: token/stale/conflict fixtures)
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// One-page PDF whose single `Tj` run is exactly the text
    /// `structurally_verify_candidate`'s expected-text recheck looks for — the crate's shared
    /// `build_one_page_pdf_bytes` fixture (base-14 Helvetica/WinAnsiEncoding, verified against real
    /// lopdf/PDF-spec structure).
    use crate::test_fixtures::build_one_page_pdf_bytes as build_pdf;

    fn hello_pdf() -> Vec<u8> {
        build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET")
    }

    #[test]
    fn structural_recheck_accepts_wellformed_candidate_with_matching_text() {
        let bytes = hello_pdf();
        let checks = vec![ExpectedTextCheck {
            page: 1,
            text: "Hello".to_string(),
        }];
        assert!(structurally_verify_candidate(&bytes, &checks).is_ok());
    }

    #[test]
    fn structural_recheck_accepts_contents_that_is_an_indirect_array_of_streams() {
        let doc = crate::test_fixtures::custom_page_pdf(
            |d| vec![("F1", crate::test_fixtures::helvetica_font(d))],
            &[b"BT /F1 12 Tf 72 700 Td ", b"(Hello) Tj ET"],
            true,
        );
        let bytes = crate::test_fixtures::pdf_bytes(&doc);
        let checks = vec![ExpectedTextCheck {
            page: 1,
            text: "Hello".to_string(),
        }];
        assert!(structurally_verify_candidate(&bytes, &checks).is_ok());
    }

    #[test]
    fn structural_recheck_rejects_missing_expected_text() {
        let bytes = hello_pdf();
        let checks = vec![ExpectedTextCheck {
            page: 1,
            text: "Goodbye".to_string(),
        }];
        let err = structurally_verify_candidate(&bytes, &checks).unwrap_err();
        assert!(matches!(err, PdfEditError::VerificationFailed));
    }

    #[test]
    fn structural_recheck_rejects_unparseable_candidate() {
        let bytes = b"not a pdf at all".to_vec();
        let err = structurally_verify_candidate(&bytes, &[]).unwrap_err();
        assert!(matches!(err, PdfEditError::VerificationFailed));
    }

    #[test]
    fn stage_prepared_save_rejects_stale_source_before_touching_candidate() {
        let store = PdfSaveTokenStore::default();
        let bytes = hello_pdf();
        let err = stage_prepared_save(
            &store,
            "/doc.pdf",
            "sha-expected",
            "sha-actual-different",
            bytes,
            vec![1],
            vec![],
        )
        .unwrap_err();
        assert!(matches!(err, PdfEditError::StaleSource));
        assert_eq!(store.live_token_count("/doc.pdf"), 0);
    }

    #[test]
    fn stage_prepared_save_issues_token_for_valid_candidate() {
        let store = PdfSaveTokenStore::default();
        let bytes = hello_pdf();
        let checks = vec![ExpectedTextCheck {
            page: 1,
            text: "Hello".to_string(),
        }];
        let meta =
            stage_prepared_save(&store, "/doc.pdf", "sha-x", "sha-x", bytes, vec![1], checks)
                .expect("valid candidate stages");
        assert_eq!(meta.token.len(), 32);
        assert_eq!(meta.affected_pages, vec![1]);
        assert_eq!(store.live_token_count("/doc.pdf"), 1);
    }

    #[test]
    fn stage_prepared_save_rejects_verification_failure_without_issuing_token() {
        let store = PdfSaveTokenStore::default();
        let bytes = hello_pdf();
        let checks = vec![ExpectedTextCheck {
            page: 1,
            text: "Not Present".to_string(),
        }];
        let err = stage_prepared_save(&store, "/doc.pdf", "sha-x", "sha-x", bytes, vec![1], checks)
            .unwrap_err();
        assert!(matches!(err, PdfEditError::VerificationFailed));
        assert_eq!(store.live_token_count("/doc.pdf"), 0);
    }

    #[test]
    fn store_evicts_oldest_token_beyond_two_per_source() {
        let store = PdfSaveTokenStore::default();
        let t1 = store.insert("/doc.pdf", "sha", hello_pdf());
        let t2 = store.insert("/doc.pdf", "sha", hello_pdf());
        let t3 = store.insert("/doc.pdf", "sha", hello_pdf());

        assert_eq!(store.live_token_count("/doc.pdf"), 2);
        assert!(store.peek(&t1).is_err(), "oldest token must be evicted");
        assert!(store.peek(&t2).is_ok());
        assert!(store.peek(&t3).is_ok());
    }

    #[test]
    fn peek_unknown_token_is_write_conflict() {
        let store = PdfSaveTokenStore::default();
        let err = store.peek("does-not-exist").unwrap_err();
        assert!(matches!(err, PdfEditError::WriteConflict));
    }

    #[test]
    fn commit_consumes_token_so_replay_fails() {
        let store = PdfSaveTokenStore::default();
        let sha = document_sha256(&hello_pdf());
        let token = store.insert("/doc.pdf", &sha, hello_pdf());

        let first = resolve_commit(&store, &token, None, |_| Ok(hello_pdf()));
        assert!(first.is_ok());

        let replay = resolve_commit(&store, &token, None, |_| Ok(hello_pdf()));
        let err = replay.unwrap_err();
        assert!(matches!(err, PdfEditError::WriteConflict));
    }

    #[test]
    fn commit_rejects_expired_token() {
        let store = PdfSaveTokenStore::default();
        let token = random_save_token();
        {
            let mut inner = store.inner.lock().unwrap();
            inner
                .by_source
                .entry("/doc.pdf".to_string())
                .or_default()
                .push_back(token.clone());
            inner.tokens.insert(
                token.clone(),
                SaveTokenRecord {
                    source_path: "/doc.pdf".to_string(),
                    source_sha256: document_sha256(&hello_pdf()),
                    candidate_bytes: hello_pdf(),
                    created_at: Instant::now() - TOKEN_TTL - Duration::from_secs(1),
                },
            );
        }

        let err = resolve_commit(&store, &token, None, |_| Ok(hello_pdf())).unwrap_err();
        assert!(matches!(err, PdfEditError::WriteConflict));
        // Expiry still consumes the token (single-use is unconditional — see module doc comment).
        assert!(store.peek(&token).is_err());
    }

    #[test]
    fn commit_rejects_source_changed_since_prepare_for_same_path_save() {
        let store = PdfSaveTokenStore::default();
        let prepare_time_sha = document_sha256(&hello_pdf());
        let token = store.insert("/doc.pdf", &prepare_time_sha, hello_pdf());
        let different_current_bytes = build_pdf(b"BT /F1 12 Tf 72 700 Td (Changed) Tj ET");

        let err = resolve_commit(
            &store,
            &token,
            None, // same-path Save
            |_| Ok(different_current_bytes.clone()),
        )
        .unwrap_err();
        assert!(matches!(err, PdfEditError::WriteConflict));
    }

    #[test]
    fn commit_allows_same_path_save_when_source_unchanged() {
        let store = PdfSaveTokenStore::default();
        let source_bytes = hello_pdf();
        let prepare_time_sha = document_sha256(&source_bytes);
        let token = store.insert("/doc.pdf", &prepare_time_sha, source_bytes.clone());

        let (source_path, bytes, result) =
            resolve_commit(&store, &token, None, |_| Ok(source_bytes.clone()))
                .expect("unchanged source commits cleanly");
        assert_eq!(source_path, "/doc.pdf");
        assert_eq!(result.target_path, "/doc.pdf");
        assert_eq!(bytes, source_bytes);
    }

    #[test]
    fn commit_save_as_does_not_recheck_source_sha() {
        // Save As targets a different path than the source; §10.3 only requires the immediate
        // SHA recheck for a *same-path* Save, so this must succeed even though the closure below
        // would fail the recheck if it were (wrongly) invoked.
        let store = PdfSaveTokenStore::default();
        let sha = document_sha256(&hello_pdf());
        let token = store.insert("/doc.pdf", &sha, hello_pdf());

        let (source_path, _bytes, result) =
            resolve_commit(&store, &token, Some("/doc-copy.pdf".to_string()), |_| {
                panic!("same-path recheck must not run for Save As")
            })
            .expect("Save As commits without a same-path recheck");
        assert_eq!(source_path, "/doc.pdf");
        assert_eq!(result.target_path, "/doc-copy.pdf");
    }

    #[test]
    fn invalidate_source_drops_every_remaining_token_for_that_path() {
        let store = PdfSaveTokenStore::default();
        let sha = document_sha256(&hello_pdf());
        let t1 = store.insert("/doc.pdf", &sha, hello_pdf());
        assert!(store.peek(&t1).is_ok());

        store.invalidate_source("/doc.pdf");

        assert!(store.peek(&t1).is_err());
        assert_eq!(store.live_token_count("/doc.pdf"), 0);
    }

    #[test]
    fn select_compose_base_uses_source_for_sentinel_annotation_bytes() {
        let source = hello_pdf();
        let sentinel = vec![0u8];
        let base = select_compose_base(&source, &sentinel).expect("sentinel selects source");
        assert_eq!(base, source.as_slice());
    }

    #[test]
    fn select_compose_base_uses_annotation_bytes_when_incremental_update() {
        let source = hello_pdf();
        let mut annotation_pdf = source.clone();
        annotation_pdf.extend_from_slice(b"\n% appended annotation objects + new xref\n");
        let base =
            select_compose_base(&source, &annotation_pdf).expect("incremental update selects it");
        assert_eq!(base, annotation_pdf.as_slice());
    }

    #[test]
    fn select_compose_base_rejects_annotation_bytes_not_extending_source() {
        let source = hello_pdf();
        let unrelated = build_pdf(b"BT /F1 12 Tf 72 700 Td (Different) Tj ET");
        let err = select_compose_base(&source, &unrelated).unwrap_err();
        assert!(matches!(err, PdfEditError::AnnotationSerializationFailed));
    }

    #[test]
    fn expected_text_checks_derives_one_entry_per_command() {
        use crate::model::{
            PdfColor, PdfPoint, PdfRect, PdfSourceLocator, PdfTextAppearance, PdfTextEditState,
        };

        let appearance = PdfTextAppearance {
            font_id: "f".to_string(),
            family: "F".to_string(),
            size_pt: 12.0,
            color: PdfColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                alpha: 1.0,
            },
        };
        let commands = vec![
            PdfEditCommand::ReplaceSourceText {
                id: "e1".to_string(),
                locator: PdfSourceLocator {
                    document_sha256: "sha".to_string(),
                    page: 2,
                    spans: vec![],
                    island_digest: "digest".to_string(),
                },
                before: PdfTextEditState {
                    text: "old".to_string(),
                    appearance: appearance.clone(),
                },
                after: PdfTextEditState {
                    text: "new text".to_string(),
                    appearance: appearance.clone(),
                },
                available_bounds: PdfRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
            },
            PdfEditCommand::AddText {
                id: "a1".to_string(),
                page: 3,
                anchor: PdfPoint { x: 1.0, y: 1.0 },
                text: "added text".to_string(),
                appearance,
            },
        ];

        let checks = expected_text_checks(&commands);
        assert_eq!(checks.len(), 2);
        assert_eq!(
            checks[0],
            ExpectedTextCheck {
                page: 2,
                text: "new text".to_string()
            }
        );
        assert_eq!(
            checks[1],
            ExpectedTextCheck {
                page: 3,
                text: "added text".to_string()
            }
        );
    }

    #[test]
    fn expected_text_checks_last_ocr_correction_wins_by_line_id() {
        use crate::model::{PdfColor, PdfOcrBackgroundProof, PdfRect, PdfTextAppearance};
        let appearance = PdfTextAppearance {
            font_id: "f".to_string(),
            family: "F".to_string(),
            size_pt: 12.0,
            color: PdfColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                alpha: 1.0,
            },
        };
        let correction = |id: &str, text: &str| PdfEditCommand::CorrectOcrLine {
            id: id.to_string(),
            page: 4,
            line_id: "line-1".to_string(),
            rect: PdfRect {
                x: 1.0,
                y: 1.0,
                width: 100.0,
                height: 20.0,
            },
            text: text.to_string(),
            appearance: appearance.clone(),
            cover_color: PdfColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                alpha: 1.0,
            },
            background_proof: PdfOcrBackgroundProof::NearUniform,
        };
        let checks = expected_text_checks(&[correction("old", "old"), correction("new", "new")]);
        assert_eq!(
            checks,
            vec![ExpectedTextCheck {
                page: 4,
                text: "new".to_string()
            }]
        );
    }
}
