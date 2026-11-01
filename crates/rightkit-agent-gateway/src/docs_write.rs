//! `docs.write`: the remote tool that creates or edits a DOCX, an XLSX or a PDF inside an approved
//! folder, tier 2, through the ChatGPT gateway (`docs_write`). It needs `remoteTools`, `remoteDocs`
//! and `remoteFilesWrite` (all off by default), a target inside an approved folder (`confine`) and
//! one local approval per call (`approvals`), exactly like `files.patch`.
//!
//! The writing is RightKit's `rightkit-docs` (typed operations, zip and XML hardening, macros and
//! encryption refused, formulas only where an operation says `formula`, the edit rechecks the
//! file's hash immediately before it replaces it). the host app adds the boundary around it:
//!
//! * `ops` is a list of `rightkit_docs::Op` (JSON, `kind` names the operation). A call either
//!   creates a new DOCX or XLSX (the first operation is `createDocx` or `createXlsx`; the name
//!   must end `.docx` or `.xlsx` and must not exist, nothing is ever overwritten by a create) or
//!   edits an existing file (`replaceText`, `appendBlocks`, `setCells`, `appendRows` for DOCX and
//!   XLSX; `replaceText`, `addPdfText`, `selectPdfPages` for PDF). A new PDF is not offered. A PDF
//!   edit never replaces the original (R15): it is written to `<name> (edited).pdf` beside it
//!   (`path` in the answer names the copy).
//! * The approval binds the tool, the door, the resolved target, the target's current SHA-256 (an
//!   edit) and the hash of the exact operations: a changed file or a changed list needs a new one.
//!   It shows the operation kinds and counts, never the text.
//! * After the write the file is read back through the same confinement and its hash must equal
//!   the writer's receipt (`verified`).
//! * The writer runs on its own thread with a deadline and shares the pool of `docs.read`
//!   (`docs::Slot`), so a hostile file cannot pile up threads. A write that passes the deadline is
//!   reported `timed_out`; it may still finish, so check the file.
//!
//! Secret-named files and credential folders are refused. Activity records the tool, the origin,
//! the extension, the number of operations and the outcome, never a path or any text.

use super::approvals;
use super::confine::{self, Root};
use super::docs;
use super::files::secret_file;
use super::files_write::{act_gate, slot};
use super::redact;
use super::settings::Settings;
use super::{Origin, Refusal, Result};
use rightkit_docs::{DocsError, Limits, Op, WriteOptions, WriteReceipt};
use rightkit_fs::digest::sha256_bytes;
use serde_json::{Value, json};
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Most operations one call may carry.
const OPS_MAX: usize = 64;
/// Most bytes the operations may take as JSON.
const OPS_JSON_MAX: usize = 2 * 1024 * 1024;
/// Longest the writer may run before the call gives up on it.
/// Under the gateway's 45 s call limit (provider.rs), so the caller gets this answer, not a bare
/// gateway timeout (#579 D16).
const WRITE_TIMEOUT: Duration = Duration::from_secs(40);
/// Stack of the writer thread (the PDF editor recurses through page trees).
const WORKER_STACK: usize = 32 * 1024 * 1024;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// The writer's refusal as a host app code. Reading limits and the format refusals keep the codes
/// `docs.read` gives them.
fn failure(error: DocsError) -> Refusal {
    match error {
        DocsError::SourceChanged => Refusal::new(
            "source_changed",
            "the file changed after it was read; nothing was written",
        ),
        DocsError::TargetExists => {
            Refusal::new("already_exists", "something with that name already exists")
        }
        DocsError::TargetMissing => Refusal::new("not_found", "the file to edit does not exist"),
        DocsError::SavedWithWarning(_) => Refusal::new(
            "write_unverified",
            "the file was replaced but a step after it failed; check the file before relying on it",
        ),
        DocsError::Io(_) => Refusal::new(
            "write_failed",
            "the write failed and the original was left as it was",
        ),
        DocsError::InvalidOp(_)
        | DocsError::UnsafeFormula(_)
        | DocsError::WrongFormat { .. }
        | DocsError::NotFound { .. } => {
            let shown: String = redact::redact(&error.to_string())
                .0
                .chars()
                .take(300)
                .collect();
            invalid(format!("{shown}; nothing was changed"))
        }
        other => docs::refusal(other),
    }
}

/// Runs the writer on a worker thread with a deadline.
fn run_write(target: PathBuf, ops: Vec<Op>, options: WriteOptions) -> Result<WriteReceipt> {
    let slot = docs::Slot::take()?;
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("rightkit-docs-write".into())
        .stack_size(WORKER_STACK)
        .spawn(move || {
            let _slot = slot;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                rightkit_docs::write(&target, &ops, &options)
            }));
            let _ = send.send(outcome);
        })
        .map_err(|e| {
            Refusal::new(
                "write_failed",
                format!("could not start the writer: {}", e.kind()),
            )
        })?;
    match receive.recv_timeout(WRITE_TIMEOUT) {
        Ok(Ok(Ok(receipt))) => Ok(receipt),
        Ok(Ok(Err(error))) => Err(failure(error)),
        Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(Refusal::new(
            "write_failed",
            "the writer failed before it finished; the original was left as it was",
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Refusal::new(
            "timed_out",
            format!(
                "writing took longer than {} seconds and was given up; the write may have completed or may still finish, read the file back (its hash) before writing again",
                WRITE_TIMEOUT.as_secs()
            ),
        )),
    }
}

/// The limits of one write: the package limits `docs.read` enforces and the file cap.
fn limits(settings: &Settings) -> Limits {
    Limits {
        max_input_bytes: settings.docs_max_bytes,
        max_total_bytes: super::package::MAX_TOTAL_BYTES,
        max_part_bytes: super::package::MAX_ENTRY_BYTES,
        max_entries: super::package::MAX_ENTRIES,
        max_ratio: super::package::MAX_RATIO,
        ..Limits::default()
    }
}

fn parse_ops(args: &Value) -> Result<(Vec<Op>, String)> {
    let Value::Array(list) = &args["ops"] else {
        return Err(invalid("ops is needed: a list of document operations"));
    };
    if list.is_empty() || list.len() > OPS_MAX {
        return Err(invalid(format!("ops holds 1 to {OPS_MAX} operations")));
    }
    if serde_json::to_vec(list)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
        > OPS_JSON_MAX
    {
        return Err(Refusal::new(
            "too_large",
            format!("the operations are limited to {OPS_JSON_MAX} bytes"),
        ));
    }
    let ops: Vec<Op> = serde_json::from_value(Value::Array(list.clone())).map_err(|e| {
        let shown: String = e.to_string().chars().take(200).collect();
        invalid(format!("ops is not a valid operation list: {shown}"))
    })?;
    // The hash the approval binds: the parsed operations, serialized the one way.
    let hash = sha256_bytes(&serde_json::to_vec(&ops).unwrap_or_default());
    Ok((ops, hash))
}

fn kind_of(op: &Op) -> &'static str {
    match op {
        Op::CreateDocx { .. } => "createDocx",
        Op::CreateXlsx { .. } => "createXlsx",
        Op::ReplaceText { .. } => "replaceText",
        Op::AppendBlocks { .. } => "appendBlocks",
        Op::SetCells { .. } => "setCells",
        Op::AppendRows { .. } => "appendRows",
        Op::AddPdfText { .. } => "addPdfText",
        Op::SelectPdfPages { .. } => "selectPdfPages",
        #[allow(unreachable_patterns)]
        _ => "operation",
    }
}

/// The kinds of the operations with their counts, for the approval text: no text of the document.
fn describe(ops: &[Op]) -> String {
    let mut seen: Vec<(&'static str, usize)> = Vec::new();
    for op in ops {
        let kind = kind_of(op);
        match seen.iter_mut().find(|(k, _)| *k == kind) {
            Some(entry) => entry.1 += 1,
            None => seen.push((kind, 1)),
        }
    }
    seen.iter()
        .map(|(kind, count)| format!("{kind} x{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the write tells the caller.
fn answer(
    roots: &[Root],
    root: usize,
    shown: &str,
    ops: usize,
    receipt: &WriteReceipt,
    verified: bool,
) -> Value {
    let clean = |text: &str| redact::redact(text).0;
    let losses: Vec<Value> = receipt
        .losses
        .iter()
        .take(20)
        .map(|loss| json!({"part": clean(&loss.part), "kind": clean(&loss.kind), "detail": clean(&loss.detail)}))
        .collect();
    let warnings: Vec<String> = receipt.warnings.iter().take(20).map(|w| clean(w)).collect();
    json!({
        "ok": true,
        "path": shown,
        "root": roots[root].index,
        "format": receipt.format.as_str(),
        "created": receipt.created,
        "ops": ops,
        "opsApplied": receipt.ops_applied,
        "replacements": receipt.replacements,
        "cellsSet": receipt.cells_set,
        "rowsAppended": receipt.rows_appended,
        "bytes": receipt.bytes_written,
        "previousSha256": receipt.source_sha256,
        "sha256": receipt.sha256,
        "losses": losses,
        "warnings": warnings,
        "verified": verified,
    })
}

/// The file at `path`, read back through the confinement, hashes to `want`.
fn holds(roots: &[Root], path: &Path, root: usize, want: &str, cap: u64) -> bool {
    let Ok(again) = confine::resolve(roots, &path.to_string_lossy()) else {
        return false;
    };
    let Ok((file, _)) = confine::open_regular(roots, &again) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    again.root == root && sha256_bytes(&bytes).eq_ignore_ascii_case(want)
}

/// Whether `bytes` are a PDF (the header may follow up to 1024 bytes of junk, as in `docs`).
fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|window| window == b"%PDF-")
}

/// A new file beside a PDF, `<name> (edited).pdf`, then `<name> (edited 2).pdf` and so on, holding
/// the bytes that were approved. It is created with `create_new`, so nothing is overwritten.
fn make_pdf_copy(parent: &Path, source: &Path, bytes: &[u8]) -> Result<PathBuf> {
    use std::io::Write;
    let stem = source
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    let extension = source
        .extension()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pdf".into());
    for number in 1..=99u32 {
        let tag = if number == 1 {
            "edited".to_string()
        } else {
            format!("edited {number}")
        };
        let candidate = parent.join(format!("{stem} ({tag}).{extension}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(Refusal::new(
                        "write_failed",
                        format!("could not make the copy: {}", e.kind()),
                    ));
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(Refusal::new(
                    "write_failed",
                    format!("could not make the copy: {}", e.kind()),
                ));
            }
        }
    }
    Err(Refusal::new(
        "already_exists",
        "too many edited copies of that PDF already exist beside it",
    ))
}

/// `docs.write`. `args`: `path`; `ops` (see the module text); `expectSha256` (an edit: refused
/// `source_changed` unless the file's hash is this); `approval` (the id of a request the user
/// approved).
pub fn write(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    settings.require_docs()?;
    settings.require_files_write()?;
    let roots = confine::approved_roots(settings)?;
    let path = match &args["path"] {
        Value::String(text) if !text.is_empty() => text.as_str(),
        _ => return Err(invalid("path is needed")),
    };
    let approval = match &args["approval"] {
        Value::Null => None,
        Value::String(text) => Some(text.as_str()),
        _ => return Err(invalid("approval must be a string")),
    };
    let expect = match &args["expectSha256"] {
        Value::Null => None,
        Value::String(text) if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Some(text.to_ascii_lowercase())
        }
        _ => return Err(invalid("expectSha256 is a 64-character hex hash")),
    };
    let (ops, ops_hash) = parse_ops(args)?;
    let creating = matches!(ops[0], Op::CreateDocx { .. } | Op::CreateXlsx { .. });
    if ops[1..]
        .iter()
        .any(|op| matches!(op, Op::CreateDocx { .. } | Op::CreateXlsx { .. }))
        || (!creating
            && ops
                .iter()
                .any(|op| matches!(op, Op::CreateDocx { .. } | Op::CreateXlsx { .. })))
    {
        return Err(invalid(
            "a create operation is only the first operation of a new file",
        ));
    }
    if creating {
        create(
            settings,
            &roots,
            origin,
            path,
            &ops,
            &ops_hash,
            expect.as_deref(),
            approval,
        )
    } else {
        edit(
            settings,
            &roots,
            origin,
            path,
            &ops,
            &ops_hash,
            expect.as_deref(),
            approval,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn create(
    settings: &Settings,
    roots: &[Root],
    origin: Origin,
    path: &str,
    ops: &[Op],
    ops_hash: &str,
    expect: Option<&str>,
    approval: Option<&str>,
) -> Result<Value> {
    if expect.is_some() {
        return Err(invalid(
            "expectSha256 is for an edit; a create makes a new file",
        ));
    }
    let wanted = match ops[0] {
        Op::CreateDocx { .. } => ".docx",
        _ => ".xlsx",
    };
    if !path.to_ascii_lowercase().ends_with(wanted) {
        return Err(invalid(format!(
            "a new file of this kind is named *{wanted}"
        )));
    }
    let place = slot(roots, path)?;
    let shown = confine::display(&place.target);
    let bind = approvals::bind_of(&[
        b"docs.write/create",
        origin.as_str().as_bytes(),
        shown.as_bytes(),
        ops_hash.as_bytes(),
    ]);
    let summary = format!(
        "create {} in an approved folder: {}",
        place.name,
        describe(ops)
    );
    act_gate(origin, "docs.write", &bind, &summary, approval)?;
    if !confine::still_here(&place.handle, &place.folder.path) {
        return Err(Refusal::new(
            "source_changed",
            "the folder changed after the approval was asked for; nothing was written",
        ));
    }
    let options = WriteOptions {
        limits: limits(settings),
        expect_sha256: None,
        overwrite: false,
    };
    let receipt = run_write(place.target.clone(), ops.to_vec(), options)?;
    let verified = holds(
        roots,
        &place.target,
        place.folder.root,
        &receipt.sha256,
        settings.docs_max_bytes,
    );
    if !verified {
        return Err(Refusal::new(
            "write_unverified",
            "the file does not hold what was written; check it before relying on it",
        ));
    }
    Ok(answer(
        roots,
        place.folder.root,
        &shown,
        ops.len(),
        &receipt,
        true,
    ))
}

#[allow(clippy::too_many_arguments)]
fn edit(
    settings: &Settings,
    roots: &[Root],
    origin: Origin,
    path: &str,
    ops: &[Op],
    ops_hash: &str,
    expect: Option<&str>,
    approval: Option<&str>,
) -> Result<Value> {
    let resolved = confine::resolve(roots, path)?;
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is never written by remote tools",
        ));
    }
    let (file, meta) = confine::open_regular(roots, &resolved)?;
    let cap = settings.docs_max_bytes;
    let too_large = || {
        Refusal::new(
            "too_large",
            format!("a document is limited to {cap} bytes (remoteDocsMaxBytes)"),
        )
    };
    if meta.len() > cap {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
    if bytes.len() as u64 > cap {
        return Err(too_large());
    }
    let old_sha = sha256_bytes(&bytes);
    if let Some(expect) = expect
        && !expect.eq_ignore_ascii_case(&old_sha)
    {
        return Err(Refusal::new(
            "source_changed",
            "the file's hash is not the expectSha256 given; read it again",
        ));
    }
    let shown = confine::display(&resolved.path);
    let name = resolved
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bind = approvals::bind_of(&[
        b"docs.write/edit",
        origin.as_str().as_bytes(),
        shown.as_bytes(),
        old_sha.as_bytes(),
        ops_hash.as_bytes(),
    ]);
    // R15 (#578 D4): a PDF is never replaced; the edit goes to a new file beside it.
    let pdf = is_pdf(&bytes);
    let bind = if pdf {
        approvals::bind_of(&[bind.as_bytes(), b"pdf-copy"])
    } else {
        bind
    };
    let summary = if pdf {
        format!(
            "edit a copy of {name} ({} bytes; the original stays as it is): {}",
            bytes.len(),
            describe(ops)
        )
    } else {
        format!("edit {name} ({} bytes): {}", bytes.len(), describe(ops))
    };
    act_gate(origin, "docs.write", &bind, &summary, approval)?;
    let parent = resolved
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| invalid("that path has no folder"))?;
    let parent_handle = confine::open_dir(&roots[resolved.root], &parent)?;
    if !confine::still_here(&parent_handle, &parent) {
        return Err(Refusal::new(
            "source_changed",
            "the folder changed after the approval was asked for; nothing was written",
        ));
    }
    // The writer rechecks this hash itself, as its last step before the rename.
    let options = WriteOptions {
        limits: limits(settings),
        expect_sha256: Some(old_sha),
        overwrite: false,
    };
    let (target, shown) = if pdf {
        let copy = make_pdf_copy(&parent, &resolved.path, &bytes)?;
        let shown = confine::display(&copy);
        (copy, shown)
    } else {
        (resolved.path.clone(), shown)
    };
    let receipt = match run_write(target.clone(), ops.to_vec(), options) {
        Ok(receipt) => receipt,
        Err(error) => {
            // A refused or failed write leaves no stray copy, except after a timeout, where the
            // writer may still be working on it.
            if pdf && error.code != "timed_out" {
                let _ = std::fs::remove_file(&target);
            }
            return Err(error);
        }
    };
    let verified = holds(roots, &target, resolved.root, &receipt.sha256, cap);
    if !verified {
        return Err(Refusal::new(
            "write_unverified",
            "the file does not hold what was written; check it before relying on it",
        ));
    }
    Ok(answer(
        roots,
        resolved.root,
        &shown,
        ops.len(),
        &receipt,
        true,
    ))
}
