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
//!   path (oldest evicted first), and a *successful* `commit_pdf_save` for a path invalidates
//!   every other outstanding token for that same path (the source's on-disk bytes just changed
//!   out from under them).
//! - **§11's frozen error taxonomy has no dedicated "token expired/unknown/replayed" variant.**
//!   Every token-lifecycle failure in `commit_pdf_save` (unknown token, expired token, already-
//!   consumed/replayed token, source changed since prepare) maps to
//!   [`PdfEditError::WriteConflict`] ("target changed before save") as the closest existing
//!   variant, per §14.1's own grouping of "expired/replayed/mismatched candidate token" as one
//!   fixture category distinct from `StaleSource` (which §10.1 reserves for the *prepare-time*
//!   source-SHA mismatch).
//! - **Single-use enforcement removes the token record on the very first `commit_pdf_save` call
//!   for it**, success or failure — a token that fails to commit (e.g. a same-path SHA race) is
//!   dead, not retryable; the caller must re-run `prepare_pdf_save` against fresh source bytes.
//!   This matches §10.3's "single-use" wording literally rather than only-consume-on-success.
//! - **"Same path" is a caller-supplied comparison.** [`resolve_commit_with`] takes the decision as
//!   a closure; [`resolve_commit`] compares the strings. A path alias (`dir/./memo.pdf`, a hard
//!   link, a case variant) defeats the strings, so [`crate::commit::commit_pdf_save`] passes a
//!   filesystem-identity comparison, and holds a per-destination lock across recheck, replacement
//!   and sibling invalidation. [`stage_prepared_save_against_base`] additionally refuses a
//!   candidate in which a page lost a resource it resolved in the bytes it was composed from.
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

/// Result of a successful `commit_pdf_save`.
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

    /// The source path `token` was staged against, or `None` for an unknown or already-consumed
    /// token. An expired token still reports it (consuming it is `consume`'s job): the commit
    /// needs the destination to serialise on before it touches the token.
    pub(crate) fn source_path_of(&self, token: &str) -> Option<String> {
        let inner = self.inner.lock().expect("token store mutex poisoned");
        inner
            .tokens
            .get(token)
            .map(|record| record.source_path.clone())
    }

    /// Every source path that has a live token, each once.
    pub(crate) fn source_paths(&self) -> Vec<String> {
        let inner = self.inner.lock().expect("token store mutex poisoned");
        inner
            .by_source
            .iter()
            .filter(|(_, queue)| !queue.is_empty())
            .map(|(path, _)| path.clone())
            .collect()
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
///
/// This entry point sees only digests, so it cannot compare the candidate with the bytes it was
/// composed from. Prefer [`stage_prepared_save_against_base`], which also refuses a candidate in
/// which a page no longer resolves a font, XObject or graphics state it resolved before.
pub fn stage_prepared_save(
    store: &PdfSaveTokenStore,
    source_path: &str,
    expected_source_sha256: &str,
    actual_source_sha256: &str,
    candidate_bytes: Vec<u8>,
    affected_pages: Vec<u32>,
    expected_text: Vec<ExpectedTextCheck>,
) -> Result<PreparedPdfSaveMeta, PdfEditError> {
    stage_checked(
        store,
        source_path,
        expected_source_sha256,
        actual_source_sha256,
        None,
        candidate_bytes,
        affected_pages,
        expected_text,
    )
}

/// [`stage_prepared_save`] plus a comparison with the bytes the candidate was composed from
/// (`base_bytes`: the source, or the annotation-extended source `select_compose_base` picked).
/// Besides the structural recheck, every page of the candidate must still resolve every font,
/// XObject, graphics state, colour space, pattern, shading and property list name that page
/// resolved in `base_bytes`, to the same object: an edit may add a resource, never hide one
/// (adding a font to a page whose `/Resources` sat on an ancestor `/Pages` node once did). A
/// `base_bytes` that does not parse is `VerificationFailed`.
#[allow(clippy::too_many_arguments)]
pub fn stage_prepared_save_against_base(
    store: &PdfSaveTokenStore,
    source_path: &str,
    expected_source_sha256: &str,
    actual_source_sha256: &str,
    base_bytes: &[u8],
    candidate_bytes: Vec<u8>,
    affected_pages: Vec<u32>,
    expected_text: Vec<ExpectedTextCheck>,
) -> Result<PreparedPdfSaveMeta, PdfEditError> {
    stage_checked(
        store,
        source_path,
        expected_source_sha256,
        actual_source_sha256,
        Some(base_bytes),
        candidate_bytes,
        affected_pages,
        expected_text,
    )
}

#[allow(clippy::too_many_arguments)]
fn stage_checked(
    store: &PdfSaveTokenStore,
    source_path: &str,
    expected_source_sha256: &str,
    actual_source_sha256: &str,
    base_bytes: Option<&[u8]>,
    candidate_bytes: Vec<u8>,
    affected_pages: Vec<u32>,
    expected_text: Vec<ExpectedTextCheck>,
) -> Result<PreparedPdfSaveMeta, PdfEditError> {
    if expected_source_sha256 != actual_source_sha256 {
        return Err(PdfEditError::StaleSource);
    }

    structurally_verify_candidate_against(&candidate_bytes, &expected_text, base_bytes)?;

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

/// The structural recheck and - when the bytes the candidate was composed from are given - the
/// check that no page lost a resource name it resolved there.
fn structurally_verify_candidate_against(
    candidate_bytes: &[u8],
    expected_text: &[ExpectedTextCheck],
    base_bytes: Option<&[u8]>,
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

    if let Some(base_bytes) = base_bytes {
        let base =
            crate::inspect::load_pdf(base_bytes).map_err(|_| PdfEditError::VerificationFailed)?;
        let before = crate::inspect::resource_fingerprint(&base)
            .map_err(|_| PdfEditError::VerificationFailed)?;
        let after = crate::inspect::resource_fingerprint(&doc)
            .map_err(|_| PdfEditError::VerificationFailed)?;
        if !crate::inspect::resources_preserved(&before, &after) {
            return Err(PdfEditError::VerificationFailed);
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
/// candidate was staged (`SaveTokenRecord::source_sha256`). Kept filesystem-free otherwise so
/// token lifecycle (expiry/replay/conflict) is directly unit-testable.
///
/// "Same path" here is string equality, which a path alias (`/tmp/./memo.pdf`, a hard link, a case
/// variant on a case-insensitive volume) defeats. A host that touches the filesystem should call
/// [`resolve_commit_with`] with a real file-identity comparison; [`crate::commit::commit_pdf_save`]
/// does.
pub fn resolve_commit<F>(
    store: &PdfSaveTokenStore,
    token: &str,
    target_path: Option<String>,
    read_current_source_for_recheck: F,
) -> Result<(String, Vec<u8>, PdfCommitResult), PdfEditError>
where
    F: FnOnce(&str) -> Result<Vec<u8>, PdfEditError>,
{
    resolve_commit_with(
        store,
        token,
        target_path,
        |source, target| source == target,
        read_current_source_for_recheck,
    )
}

/// [`resolve_commit`] with the "is the target the source file?" decision supplied by the caller:
/// `same_file(source_path, target_path)` is called once, after the token is consumed, and the
/// source recheck runs exactly when it returns `true`. Passing a comparison of filesystem
/// identities (device and file id, canonical path as the fallback) closes the alias bypass that
/// plain string equality leaves open.
pub fn resolve_commit_with<F, S>(
    store: &PdfSaveTokenStore,
    token: &str,
    target_path: Option<String>,
    same_file: S,
    read_current_source_for_recheck: F,
) -> Result<(String, Vec<u8>, PdfCommitResult), PdfEditError>
where
    F: FnOnce(&str) -> Result<Vec<u8>, PdfEditError>,
    S: FnOnce(&str, &str) -> bool,
{
    let record = store.consume(token)?;
    let target = target_path.unwrap_or_else(|| record.source_path.clone());

    if same_file(&record.source_path, &target) {
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
