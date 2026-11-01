//! `files.patch` and `files.manage`: the remote tools that change files, tier 2. They need
//! `remoteTools` and `remoteFilesWrite` (both off by default), a target inside an approved folder
//! (`confine`, the same boundary the read tools use) and one local approval per act
//! (`approvals`).
//!
//! * `files.patch` either edits one existing UTF-8 file with exact-text replacements (each `old`
//!   must occur exactly once) or creates a new file (`create`, never over an existing one). The
//!   file's content hash can be pinned (`expectSha256`).
//! * `files.manage` makes a folder, moves, copies or sends to the Trash. A move or copy never
//!   replaces an existing name. Removal is the Trash only; nothing is deleted outright.
//!
//! Every write is verified: the new bytes are written to a temporary sibling and hashed there
//! before they replace anything (`AtomicWriter::commit_checked`), the target is re-read after the
//! rename and hashed again, and the answer says `verified: true` only when both matched. The
//! check that runs just before the rename also re-reads the target and the folder it sits in, so a
//! file that changed after the approval was asked for, or a folder swapped for a link, aborts the
//! write and leaves the original.
//!
//! Approval binding: the bind covers the tool, the door, the resolved target, the target's
//! current identity (content hash for a file, device and inode for a folder) and what will happen
//! to it. Change any of them and the approval no longer applies.
//!
//! Known limits: a swap of a folder component between the last check and the rename is narrowed,
//! not excluded (the same limit the read tools state in `confine`); removal is the Trash on macOS
//! (a rename into `~/.Trash`, so a file on a volume other than the home volume is refused instead
//! of copied and deleted) and the Recycle Bin on Windows (`trash::delete`, which chooses where the
//! item goes, so the answer has no `trashedAs`); other systems answer `unsupported_platform`.
//! `files.patch` with `files` checks every file before it writes any, takes one approval for the
//! set, and writes the files one at a time, each atomically: a failure part-way leaves the files
//! already written as they are and says how many.

use super::approvals;
use super::confine::{self, Resolved, Root};
use super::files::secret_file;
use super::redact;
use super::settings::Settings;
use super::{Origin, Refusal, Result};
use rightkit_fs::digest::sha256_bytes;
use rightkit_fs::{AtomicWriteOptions, AtomicWriter, FsError, Privacy, ReplaceMode};
use serde_json::{Value, json};
use std::fs::Metadata;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest file `files.patch` edits or creates (the reply and the approval summary stay small).
const PATCH_MAX_BYTES: u64 = super::settings::HARD_READ_MAX_BYTES;
/// The largest file `files.manage` copies; it is held in memory to be hashed.
const COPY_MAX_BYTES: u64 = 64 * 1024 * 1024;
const EDITS_MAX: usize = 64;
const OLD_MAX: usize = 64 * 1024;
const NEW_MAX: usize = 256 * 1024;
const NAME_MAX: usize = 255;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.as_str())),
        _ => Err(invalid(format!("{key} must be a string"))),
    }
}

fn need_text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    text_arg(args, key)?
        .filter(|t| !t.is_empty())
        .ok_or_else(|| invalid(format!("{key} is needed")))
}

/// The approval step every change goes through: no approval id asks for one, an id uses it.
pub(super) fn act_gate(
    origin: Origin,
    tool: &str,
    bind: &str,
    summary: &str,
    approval: Option<&str>,
) -> Result<()> {
    match approval.filter(|a| !a.is_empty()) {
        None => Err(approvals::request_act_held(origin, tool, bind, summary)),
        Some(id) => approvals::take_act_held(origin, tool, bind, id),
    }
}

fn write_failed(error: FsError) -> Refusal {
    match error {
        FsError::AlreadyExists(_) => {
            Refusal::new("already_exists", "something with that name already exists")
        }
        FsError::Io { source, .. } if source.to_string() == "source_changed" => Refusal::new(
            "source_changed",
            "the file or its folder changed after the approval was asked for; nothing was written",
        ),
        FsError::Io { source, .. } if source.to_string() == "write_unverified" => Refusal::new(
            "write_unverified",
            "the bytes in the temporary file were not the bytes meant to be written; nothing was written",
        ),
        other => Refusal::new(
            "write_failed",
            format!(
                "the write failed and the original was left as it was: {:?}",
                other.kind()
            ),
        ),
    }
}

/// The name part of a requested path, checked: no separators (it came from `file_name`), not a
/// secret-named file, not reserved, and on Windows no stream (`:`) or trailing dot or space.
fn clean_name(name: &std::ffi::OsStr) -> Result<String> {
    let text = name.to_string_lossy().into_owned();
    if text.is_empty() || text.len() > NAME_MAX || text == "." || text == ".." {
        return Err(invalid("that is not a usable file name"));
    }
    if text.contains(".rktmp-") {
        return Err(invalid("that name is reserved"));
    }
    if cfg!(windows) && (text.contains(':') || text.ends_with('.') || text.ends_with(' ')) {
        return Err(invalid("that name is not usable on Windows"));
    }
    if secret_file(Path::new(&text)) || confine::sensitive(Path::new(&text)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that name usually holds secrets or credentials; remote tools never write it",
        ));
    }
    Ok(text)
}

/// A folder to put a new name in, and the name: the requested path's parent resolved and opened
/// like a read's folder. The handle is kept so the folder can be checked again before the act.
pub(super) struct Slot {
    pub(super) folder: Resolved,
    pub(super) handle: std::fs::File,
    pub(super) name: String,
    pub(super) target: PathBuf,
}

pub(super) fn slot(roots: &[Root], requested: &str) -> Result<Slot> {
    let path = Path::new(requested);
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(invalid("a path with a file name is needed"));
    };
    let name = clean_name(name)?;
    let folder = confine::resolve(roots, &parent.to_string_lossy())?;
    let handle = confine::open_dir(&roots[folder.root], &folder.path)?;
    let target = folder.path.join(&name);
    // The target must be new: no entry of any kind, link included.
    match std::fs::symlink_metadata(&target) {
        Ok(_) => {
            return Err(Refusal::new(
                "already_exists",
                "something with that name already exists",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Refusal::new(
                "read_failed",
                format!("could not look at the target: {}", e.kind()),
            ));
        }
    }
    Ok(Slot {
        folder,
        handle,
        name,
        target,
    })
}

/// A folder or file's identity for an approval bind: a file by size and modification time, a
/// folder by what names it (device and inode where the platform has them).
fn identity(meta: &Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.is_dir() {
            format!("d:{}:{}", meta.dev(), meta.ino())
        } else {
            format!(
                "f:{}:{}:{}:{}",
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.mtime_nsec() + meta.mtime() * 1_000_000_000
            )
        }
    }
    #[cfg(not(unix))]
    {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        if meta.is_dir() {
            "d".to_string()
        } else {
            format!("f:{}:{modified}", meta.len())
        }
    }
}

fn read_all(file: std::fs::File, cap: u64, too_large: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
    if bytes.len() as u64 > cap {
        return Err(Refusal::new("too_large", too_large));
    }
    Ok(bytes)
}

fn options(replace: ReplaceMode) -> AtomicWriteOptions {
    AtomicWriteOptions {
        private: false,
        create_parents: false,
        durable: true,
        replace,
        retry_sharing_violation: None,
        privacy: Privacy::Inherit,
    }
}

/// Writes `bytes` to `target` through a temporary sibling: hashed there before the rename, then
/// `still_ok` (the caller's last look at what is being replaced) runs, then the rename. The target
/// is read again afterwards and must hold exactly `bytes`.
pub(super) fn write_verified(
    roots: &[Root],
    root: usize,
    target: &Path,
    bytes: &[u8],
    replace: ReplaceMode,
    still_ok: impl FnOnce() -> bool,
) -> Result<()> {
    let want = sha256_bytes(bytes);
    let mut writer = AtomicWriter::create_with(target, options(replace)).map_err(write_failed)?;
    writer.write_all(bytes).map_err(write_failed)?;
    writer
        .commit_checked(|temp| {
            let mut staged = Vec::new();
            std::fs::File::open(temp)?
                .take((bytes.len() as u64).saturating_add(1))
                .read_to_end(&mut staged)?;
            if sha256_bytes(&staged) != want {
                return Err(std::io::Error::other("write_unverified"));
            }
            if !still_ok() {
                return Err(std::io::Error::other("source_changed"));
            }
            Ok(())
        })
        .map_err(write_failed)?;
    // Read back what the name holds now, through the same confinement as a read.
    let again = confine::resolve(roots, &target.to_string_lossy())?;
    let (file, _) = confine::open_regular(roots, &again)?;
    let held = read_all(
        file,
        bytes.len() as u64,
        "the file grew while it was checked",
    )?;
    if again.root != root || sha256_bytes(&held) != want {
        return Err(Refusal::new(
            "write_unverified",
            "the file does not hold what was written; check it before relying on it",
        ));
    }
    Ok(())
}

/// `files.patch`. `args`: `path`; exactly one of `create` (the new file's text) and `edits` (a
/// list of `{old, new}`); `expectSha256` (edits only: refused `source_changed` when the file's
/// hash differs); `approval` (the id of a request the user approved). Or `files` instead of
/// those four: up to `FILES_MAX` of `{path, create | edits, expectSha256}` under one approval
/// (`multi`).
pub fn patch(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    settings.require_files_write()?;
    let roots = confine::approved_roots(settings)?;
    let approval = text_arg(args, "approval")?;
    // Several files in one call: one approval for the whole set.
    if let Value::Array(files) = &args["files"] {
        if !(args["path"].is_null()
            && args["create"].is_null()
            && args["edits"].is_null()
            && args["expectSha256"].is_null())
        {
            return Err(invalid(
                "give files (a list) or path with create or edits, not both",
            ));
        }
        return multi(&roots, origin, files, approval);
    }
    if !args["files"].is_null() {
        return Err(invalid("files must be a list"));
    }
    let path = need_text(args, "path")?;
    match (&args["create"], &args["edits"]) {
        (Value::String(content), Value::Null) => create(&roots, origin, path, content, approval),
        (Value::Null, Value::Array(edits)) => edit(
            &roots,
            origin,
            path,
            edits,
            text_arg(args, "expectSha256")?,
            approval,
        ),
        _ => Err(invalid(
            "give exactly one of create (text for a new file) and edits (a list of {old, new})",
        )),
    }
}

/// A new file, checked and ready to be written.
struct CreatePlan<'a> {
    place: Slot,
    content: &'a str,
    new_sha: String,
    shown: String,
}

fn plan_create<'a>(roots: &[Root], path: &str, content: &'a str) -> Result<CreatePlan<'a>> {
    if content.len() as u64 > PATCH_MAX_BYTES {
        return Err(Refusal::new(
            "too_large",
            format!("a file made here is limited to {PATCH_MAX_BYTES} bytes"),
        ));
    }
    if content.contains('\0') {
        return Err(invalid(
            "create is text and must not contain a NUL character",
        ));
    }
    let place = slot(roots, path)?;
    let new_sha = sha256_bytes(content.as_bytes());
    let shown = confine::display(&place.target);
    Ok(CreatePlan {
        place,
        content,
        new_sha,
        shown,
    })
}

fn create(
    roots: &[Root],
    origin: Origin,
    path: &str,
    content: &str,
    approval: Option<&str>,
) -> Result<Value> {
    let plan = plan_create(roots, path, content)?;
    let bind = approvals::bind_of(&[
        b"files.patch/create",
        origin.as_str().as_bytes(),
        plan.shown.as_bytes(),
        plan.new_sha.as_bytes(),
    ]);
    let summary = format!(
        "create {} ({} bytes) in an approved folder",
        plan.place.name,
        content.len()
    );
    act_gate(origin, "files.patch", &bind, &summary, approval)?;
    apply_create(roots, &plan)
}

fn apply_create(roots: &[Root], plan: &CreatePlan) -> Result<Value> {
    let place = &plan.place;
    let parent = place.folder.path.clone();
    write_verified(
        roots,
        place.folder.root,
        &place.target,
        plan.content.as_bytes(),
        ReplaceMode::NoReplace,
        || confine::still_here(&place.handle, &parent),
    )?;
    Ok(json!({
        "ok": true,
        "path": plan.shown,
        "root": roots[place.folder.root].index,
        "created": true,
        "bytes": plan.content.len(),
        "sha256": plan.new_sha,
        "verified": true,
    }))
}

/// The lines of `text` that look most like `old` (a window of up to 20 lines, matched line by line
/// ignoring surrounding spaces), with their line numbers, bounded and passed through the secret
/// redactor: a hint for an edit whose `old` was not found. `None` when nothing is alike. It only
/// ever shows lines of the file the caller is already editing.
fn closest_region(text: &str, old: &str) -> Option<String> {
    const WINDOW_MAX: usize = 20;
    const SCAN_MAX: usize = 50_000;
    const LINE_CHARS: usize = 200;
    const HINT_CHARS: usize = 2000;
    let want: Vec<&str> = old.lines().map(str::trim).collect();
    let have: Vec<&str> = text.lines().take(SCAN_MAX).collect();
    if want.is_empty() || have.is_empty() {
        return None;
    }
    let k = want.len().min(WINDOW_MAX).min(have.len());
    let like = |a: &str, b: &str| -> u32 {
        if a.is_empty() || b.is_empty() {
            0
        } else if a == b {
            2
        } else if a.len() >= 4 && b.len() >= 4 && (a.contains(b) || b.contains(a)) {
            1
        } else {
            0
        }
    };
    let mut best = (0u32, 0usize);
    for start in 0..=have.len() - k {
        let score: u32 = (0..k).map(|i| like(have[start + i].trim(), want[i])).sum();
        if score > best.0 {
            best = (score, start);
        }
    }
    if best.0 == 0 {
        return None;
    }
    let start = best.1;
    let mut shown = String::new();
    for (i, line) in have[start..start + k].iter().enumerate() {
        let line: String = line.chars().take(LINE_CHARS).collect();
        shown.push_str(&format!("{}: {line}\n", start + i + 1));
        if shown.len() > HINT_CHARS {
            break;
        }
    }
    let (shown, _) = redact::redact(&shown);
    Some(format!(
        "Closest text, lines {}-{}:\n{}",
        start + 1,
        start + k,
        shown.trim_end()
    ))
}

/// An edit of an existing file, checked and ready to be written.
struct EditPlan {
    resolved: Resolved,
    text: String,
    old_sha: String,
    new_sha: String,
    shown: String,
    name: String,
    old_bytes: usize,
    count: usize,
}

fn plan_edit(
    roots: &[Root],
    path: &str,
    edits: &[Value],
    expect: Option<&str>,
) -> Result<EditPlan> {
    if edits.is_empty() || edits.len() > EDITS_MAX {
        return Err(invalid(format!("edits holds 1 to {EDITS_MAX} items")));
    }
    let mut pairs: Vec<(&str, &str)> = Vec::with_capacity(edits.len());
    for item in edits {
        let (Some(old), Some(new)) = (item["old"].as_str(), item["new"].as_str()) else {
            return Err(invalid("each edit is {old, new}, both text"));
        };
        if old.is_empty() || old.len() > OLD_MAX || new.len() > NEW_MAX || new.contains('\0') {
            return Err(invalid(format!(
                "old is 1 to {OLD_MAX} bytes and new at most {NEW_MAX} bytes, without NUL"
            )));
        }
        pairs.push((old, new));
    }
    let resolved = confine::resolve(roots, path)?;
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is never written by remote tools",
        ));
    }
    let (file, meta) = confine::open_regular(roots, &resolved)?;
    if meta.len() > PATCH_MAX_BYTES {
        return Err(Refusal::new(
            "too_large",
            format!("a file edited here is limited to {PATCH_MAX_BYTES} bytes"),
        ));
    }
    let before = read_all(file, PATCH_MAX_BYTES, "the file grew while it was read")?;
    let old_sha = sha256_bytes(&before);
    let old_bytes = before.len();
    if let Some(expect) = expect
        && !expect.eq_ignore_ascii_case(&old_sha)
    {
        return Err(Refusal::new(
            "source_changed",
            "the file's hash is not the expectSha256 given; read it again",
        ));
    }
    let mut text = match String::from_utf8(before) {
        Ok(text) if !text.contains('\0') => text,
        _ => {
            return Err(Refusal::new(
                "unsupported_format",
                "files.patch edits UTF-8 text files only",
            ));
        }
    };
    for (position, (old, new)) in pairs.iter().enumerate() {
        match text.matches(old).count() {
            1 => text = text.replacen(old, new, 1),
            0 => {
                let hint = closest_region(&text, old)
                    .map(|h| format!(" {h}"))
                    .unwrap_or_default();
                return Err(Refusal::new(
                    "edit_not_found",
                    format!(
                        "edit {} did not match the file; nothing was changed.{hint}",
                        position + 1
                    ),
                ));
            }
            _ => {
                let lines: Vec<String> = text
                    .match_indices(old)
                    .take(8)
                    .map(|(at, _)| (text[..at].matches('\n').count() + 1).to_string())
                    .collect();
                return Err(Refusal::new(
                    "edit_ambiguous",
                    format!(
                        "edit {} matches more than once (starting at lines {}); give more context; nothing was changed",
                        position + 1,
                        lines.join(", ")
                    ),
                ));
            }
        }
        if text.len() as u64 > PATCH_MAX_BYTES {
            return Err(Refusal::new(
                "too_large",
                "the edited file would be too large",
            ));
        }
    }
    let new_sha = sha256_bytes(text.as_bytes());
    if new_sha == old_sha {
        return Err(invalid("the edits leave the file as it is"));
    }
    let shown = confine::display(&resolved.path);
    let name = resolved
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(EditPlan {
        resolved,
        text,
        old_sha,
        new_sha,
        shown,
        name,
        old_bytes,
        count: pairs.len(),
    })
}

fn edit(
    roots: &[Root],
    origin: Origin,
    path: &str,
    edits: &[Value],
    expect: Option<&str>,
    approval: Option<&str>,
) -> Result<Value> {
    let plan = plan_edit(roots, path, edits, expect)?;
    let bind = approvals::bind_of(&[
        b"files.patch/edit",
        origin.as_str().as_bytes(),
        plan.shown.as_bytes(),
        plan.old_sha.as_bytes(),
        plan.new_sha.as_bytes(),
    ]);
    let summary = format!(
        "edit {}: {} change(s), {} to {} bytes",
        plan.name,
        plan.count,
        plan.old_bytes,
        plan.text.len()
    );
    act_gate(origin, "files.patch", &bind, &summary, approval)?;
    apply_edit(roots, &plan)
}

fn apply_edit(roots: &[Root], plan: &EditPlan) -> Result<Value> {
    let resolved = &plan.resolved;
    let text = &plan.text;
    let parent_dir = resolved
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| invalid("that path has no folder"))?;
    let parent_handle = confine::open_dir(&roots[resolved.root], &parent_dir)?;
    let expected_old = plan.old_sha.clone();
    let check_roots: Vec<Root> = roots.to_vec();
    let check_resolved = resolved.clone();
    write_verified(
        roots,
        resolved.root,
        &resolved.path,
        text.as_bytes(),
        ReplaceMode::Rename,
        move || {
            // The file must still be the one that was read, in the folder that was opened.
            if !confine::still_here(&parent_handle, &parent_dir) {
                return false;
            }
            let Ok((file, _)) = confine::open_regular(&check_roots, &check_resolved) else {
                return false;
            };
            read_all(file, PATCH_MAX_BYTES, "grew")
                .map(|now| sha256_bytes(&now) == expected_old)
                .unwrap_or(false)
        },
    )?;
    Ok(json!({
        "ok": true,
        "path": plan.shown,
        "root": roots[resolved.root].index,
        "edits": plan.count,
        "bytes": text.len(),
        "previousSha256": plan.old_sha,
        "sha256": plan.new_sha,
        "verified": true,
    }))
}

/// Most files one `files.patch` call may change.
const FILES_MAX: usize = 16;

enum FilePlan<'a> {
    Create(CreatePlan<'a>),
    Edit(EditPlan),
}

impl FilePlan<'_> {
    fn shown(&self) -> &str {
        match self {
            FilePlan::Create(plan) => &plan.shown,
            FilePlan::Edit(plan) => &plan.shown,
        }
    }
}

/// `files.patch` with `files`: a list of `{path, create | edits, expectSha256}`. Every file is
/// checked first (any problem refuses the whole call and nothing is changed), the user approves
/// the set once, bound to every target's current and new hash, then each file is written
/// atomically and verified in order. A failure part-way stops there and the refusal names how many
/// files were already changed; the changed files stay as written.
fn multi(roots: &[Root], origin: Origin, files: &[Value], approval: Option<&str>) -> Result<Value> {
    if files.is_empty() || files.len() > FILES_MAX {
        return Err(invalid(format!("files holds 1 to {FILES_MAX} items")));
    }
    let mut plans: Vec<FilePlan> = Vec::with_capacity(files.len());
    for (position, item) in files.iter().enumerate() {
        let number = position + 1;
        let tag = |e: Refusal| {
            Refusal::new(
                e.code,
                format!("file {number}: {}; nothing was changed", e.message),
            )
        };
        let Some(fields) = item.as_object() else {
            return Err(invalid(format!("file {number} must be an object")));
        };
        if fields
            .keys()
            .any(|k| !matches!(k.as_str(), "path" | "create" | "edits" | "expectSha256"))
        {
            return Err(invalid(format!(
                "file {number}: only path, create, edits and expectSha256 are accepted"
            )));
        }
        let path = need_text(item, "path").map_err(tag)?;
        let plan = match (&item["create"], &item["edits"]) {
            (Value::String(content), Value::Null) => {
                if !item["expectSha256"].is_null() {
                    return Err(invalid(format!(
                        "file {number}: expectSha256 goes with edits only"
                    )));
                }
                FilePlan::Create(plan_create(roots, path, content).map_err(tag)?)
            }
            (Value::Null, Value::Array(edits)) => FilePlan::Edit(
                plan_edit(
                    roots,
                    path,
                    edits,
                    text_arg(item, "expectSha256").map_err(tag)?,
                )
                .map_err(tag)?,
            ),
            _ => {
                return Err(invalid(format!(
                    "file {number}: give exactly one of create (text) and edits (a list of {{old, new}})"
                )));
            }
        };
        if plans.iter().any(|other| other.shown() == plan.shown()) {
            return Err(invalid(format!(
                "file {number}: the same path is named twice"
            )));
        }
        plans.push(plan);
    }
    let mut parts: Vec<&[u8]> = vec![&b"files.patch/multi"[..], origin.as_str().as_bytes()];
    for plan in &plans {
        match plan {
            FilePlan::Create(p) => {
                parts.extend([&b"create"[..], p.shown.as_bytes(), p.new_sha.as_bytes()]);
            }
            FilePlan::Edit(p) => {
                parts.extend([
                    &b"edit"[..],
                    p.shown.as_bytes(),
                    p.old_sha.as_bytes(),
                    p.new_sha.as_bytes(),
                ]);
            }
        }
    }
    let bind = approvals::bind_of(&parts);
    let names: Vec<String> = plans
        .iter()
        .map(|plan| match plan {
            FilePlan::Create(p) => format!("create {}", p.place.name),
            FilePlan::Edit(p) => format!("edit {}", p.name),
        })
        .collect();
    let summary = format!("change {} files: {}", plans.len(), names.join(", "));
    act_gate(origin, "files.patch", &bind, &summary, approval)?;
    let mut done: Vec<Value> = Vec::with_capacity(plans.len());
    for (position, plan) in plans.iter().enumerate() {
        let outcome = match plan {
            FilePlan::Create(p) => apply_create(roots, p),
            FilePlan::Edit(p) => apply_edit(roots, p),
        };
        match outcome {
            Ok(result) => done.push(result),
            Err(e) => {
                return Err(Refusal::new(
                    e.code,
                    format!(
                        "file {}: {}; {} file(s) before it were already changed and stay as written, this one and the {} after it were not",
                        position + 1,
                        e.message,
                        position,
                        plans.len() - position - 1
                    ),
                ));
            }
        }
    }
    Ok(json!({
        "ok": true,
        "count": done.len(),
        "files": done,
    }))
}

/// `files.manage`. `args`: `op` (`mkdir`, `move`, `copy` or `trash`), `path`, `to` (move and copy:
/// the new path), `approval`.
pub fn manage(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    settings.require_files_write()?;
    let roots = confine::approved_roots(settings)?;
    let op = need_text(args, "op")?;
    let path = need_text(args, "path")?;
    let approval = text_arg(args, "approval")?;
    match op {
        "mkdir" => mkdir(&roots, origin, path, approval),
        "move" => relocate(
            &roots,
            origin,
            path,
            need_text(args, "to")?,
            approval,
            false,
        ),
        "copy" => relocate(&roots, origin, path, need_text(args, "to")?, approval, true),
        "trash" => trash_op(&roots, origin, path, approval),
        _ => Err(invalid("op is mkdir, move, copy or trash")),
    }
}

fn mkdir(roots: &[Root], origin: Origin, path: &str, approval: Option<&str>) -> Result<Value> {
    let place = slot(roots, path)?;
    let shown = confine::display(&place.target);
    let bind = approvals::bind_of(&[
        b"files.manage/mkdir",
        origin.as_str().as_bytes(),
        shown.as_bytes(),
    ]);
    let summary = format!("make the folder {} in an approved folder", place.name);
    act_gate(origin, "files.manage", &bind, &summary, approval)?;
    if !confine::still_here(&place.handle, &place.folder.path) {
        return Err(source_changed());
    }
    // `create_dir` makes one level and fails if the name exists, so it cannot enter a link.
    std::fs::create_dir(&place.target).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => {
            Refusal::new("already_exists", "something with that name already exists")
        }
        kind => Refusal::new("write_failed", format!("could not make the folder: {kind}")),
    })?;
    let made = confine::resolve(roots, &place.target.to_string_lossy())?;
    let (_handle, meta) = confine::open_any(roots, &made)?;
    if !meta.is_dir() {
        return Err(Refusal::new(
            "write_unverified",
            "the new name is not a folder",
        ));
    }
    Ok(json!({
        "ok": true,
        "op": "mkdir",
        "path": shown,
        "root": roots[place.folder.root].index,
        "verified": true,
    }))
}

fn source_changed() -> Refusal {
    Refusal::new(
        "source_changed",
        "the source or its folder changed after the approval was asked for; nothing was done",
    )
}

/// The existing file or folder a manage op acts on, opened and verified, with its identity.
fn existing(roots: &[Root], path: &str) -> Result<(Resolved, Metadata)> {
    // `resolve` follows links, so a link would stand for what it points at; act on real names only.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("that path is a link; name the item itself"));
    }
    let resolved = confine::resolve(roots, path)?;
    if resolved.relative.as_os_str().is_empty() {
        return Err(invalid(
            "an approved folder itself is never moved or removed",
        ));
    }
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is never changed by remote tools",
        ));
    }
    let (_handle, meta) = confine::open_any(roots, &resolved)?;
    Ok((resolved, meta))
}

fn relocate(
    roots: &[Root],
    origin: Origin,
    path: &str,
    to: &str,
    approval: Option<&str>,
    copy: bool,
) -> Result<Value> {
    let tool_op = if copy { "copy" } else { "move" };
    let (from, meta) = existing(roots, path)?;
    if copy && meta.is_dir() {
        return Err(invalid(
            "copy takes a file; copy a folder's files one by one",
        ));
    }
    let place = slot(roots, to)?;
    if place.target.starts_with(&from.path) {
        return Err(invalid("a folder cannot be put inside itself"));
    }
    let shown_from = confine::display(&from.path);
    let shown_to = confine::display(&place.target);
    let from_token = identity(&meta);
    // A copy is bound to the bytes it will copy.
    let (bytes, sha) = if copy {
        if meta.len() > COPY_MAX_BYTES {
            return Err(Refusal::new(
                "too_large",
                format!("a copy is limited to {COPY_MAX_BYTES} bytes"),
            ));
        }
        let (file, _) = confine::open_regular(roots, &from)?;
        let bytes = read_all(file, COPY_MAX_BYTES, "the file grew while it was read")?;
        let sha = sha256_bytes(&bytes);
        (bytes, sha)
    } else {
        (Vec::new(), String::new())
    };
    let bind = approvals::bind_of(&[
        format!("files.manage/{tool_op}").as_bytes(),
        origin.as_str().as_bytes(),
        shown_from.as_bytes(),
        from_token.as_bytes(),
        sha.as_bytes(),
        shown_to.as_bytes(),
    ]);
    let from_name = from
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let summary = format!(
        "{tool_op} {from_name} to {} (both in approved folders)",
        place.name
    );
    act_gate(origin, "files.manage", &bind, &summary, approval)?;
    if !confine::still_here(&place.handle, &place.folder.path) {
        return Err(source_changed());
    }
    // The source must be the same thing the approval was bound to.
    let (_, again) = existing(roots, path)?;
    if identity(&again) != from_token {
        return Err(source_changed());
    }
    if copy {
        let parent = place.folder.path.clone();
        write_verified(
            roots,
            place.folder.root,
            &place.target,
            &bytes,
            ReplaceMode::NoReplace,
            || confine::still_here(&place.handle, &parent),
        )?;
    } else {
        rightkit_fs::rename_exclusive(&from.path, &place.target).map_err(|e| match e {
            FsError::AlreadyExists(_) => {
                Refusal::new("already_exists", "something with that name already exists")
            }
            other => Refusal::new(
                "write_failed",
                format!(
                    "the move failed (a move stays on one volume): {:?}",
                    other.kind()
                ),
            ),
        })?;
        // The name that was moved is gone, and the new one is the same thing.
        let moved = confine::resolve(roots, &place.target.to_string_lossy())?;
        let (_handle, now) = confine::open_any(roots, &moved)?;
        if std::fs::symlink_metadata(&from.path).is_ok() || !same_object(&meta, &now) {
            return Err(Refusal::new(
                "write_unverified",
                "the move did not leave the item only under its new name",
            ));
        }
    }
    Ok(json!({
        "ok": true,
        "op": tool_op,
        "from": shown_from,
        "path": shown_to,
        "root": roots[place.folder.root].index,
        "bytes": if copy { json!(bytes.len()) } else { Value::Null },
        "sha256": if copy { json!(sha) } else { Value::Null },
        "verified": true,
    }))
}

/// Whether two metadata describe one file or folder (device and inode where there are any).
fn same_object(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (a.dev(), a.ino()) == (b.dev(), b.ino())
    }
    #[cfg(not(unix))]
    {
        a.is_dir() == b.is_dir() && a.len() == b.len()
    }
}

fn trash_op(roots: &[Root], origin: Origin, path: &str, approval: Option<&str>) -> Result<Value> {
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (roots, origin, path, approval);
        Err(Refusal::new(
            "unsupported_platform",
            "sending to the Trash is built for macOS and Windows only, and nothing is ever deleted outright",
        ))
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        let (resolved, meta) = existing(roots, path)?;
        let shown = confine::display(&resolved.path);
        let token = identity(&meta);
        let bind = approvals::bind_of(&[
            b"files.manage/trash",
            origin.as_str().as_bytes(),
            shown.as_bytes(),
            token.as_bytes(),
        ]);
        let name = resolved
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let summary = format!(
            "move {} {name} to the Trash",
            if meta.is_dir() {
                "the folder"
            } else {
                "the file"
            }
        );
        act_gate(origin, "files.manage", &bind, &summary, approval)?;
        let (_, again) = existing(roots, path)?;
        if identity(&again) != token {
            return Err(source_changed());
        }
        // macOS: a rename into `~/.Trash`, so where it landed is known and checked. Windows: the
        // Recycle Bin chooses where the item goes, so only the item's absence from its old place
        // can be checked.
        #[cfg(target_os = "macos")]
        let in_trash = Some(move_to_trash(&resolved.path)?);
        #[cfg(windows)]
        let in_trash: Option<PathBuf> = {
            recycle(&resolved.path)?;
            None
        };
        if std::fs::symlink_metadata(&resolved.path).is_ok() {
            return Err(Refusal::new(
                "write_unverified",
                "the item is still where it was after the move",
            ));
        }
        if let Some(in_trash) = &in_trash {
            let landed = std::fs::symlink_metadata(in_trash)
                .map_err(|_| Refusal::new("write_unverified", "the item is not in the Trash"))?;
            if !same_object(&meta, &landed) {
                return Err(Refusal::new(
                    "write_unverified",
                    "what is in the Trash is not what was moved",
                ));
            }
        }
        Ok(json!({
            "ok": true,
            "op": "trash",
            "path": shown,
            "root": roots[resolved.root].index,
            "trashedAs": in_trash
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            "verified": true,
        }))
    }
}

/// Windows: sends `path` to the Recycle Bin (the same `trash` crate call the cleanup commands use,
/// `engine/src/cleanup.rs`); it recycles and never deletes outright.
#[cfg(windows)]
fn recycle(path: &Path) -> Result<()> {
    trash::delete(path).map_err(|e| {
        Refusal::new(
            "trash_failed",
            format!("the item could not be sent to the Recycle Bin: {e}"),
        )
    })
}

/// Moves `path` into `~/.Trash` under a name that is not taken (a number or the time is added when
/// the name is in use), by an exclusive rename. A file on another volume is refused.
#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<PathBuf> {
    let home = rightkit_app_host::account_home();
    let trash = home.join(".Trash");
    if !std::fs::symlink_metadata(&trash).is_ok_and(|m| m.is_dir()) {
        return Err(Refusal::new(
            "write_failed",
            "the Trash folder was not found",
        ));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| invalid("that path has no name"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for attempt in 0..64u32 {
        let candidate = if attempt == 0 {
            name.clone()
        } else {
            match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem} {stamp}-{attempt}.{ext}"),
                _ => format!("{name} {stamp}-{attempt}"),
            }
        };
        let destination = trash.join(&candidate);
        match rightkit_fs::rename_exclusive(path, &destination) {
            Ok(()) => return Ok(destination),
            Err(FsError::AlreadyExists(_)) => continue,
            Err(other) => {
                return Err(Refusal::new(
                    "trash_failed",
                    format!(
                        "the item could not be moved to the Trash (an item on another volume is not copied): {:?}",
                        other.kind()
                    ),
                ));
            }
        }
    }
    Err(Refusal::new(
        "trash_failed",
        "no free name was found in the Trash",
    ))
}
