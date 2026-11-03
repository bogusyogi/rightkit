//! Caller tagging for `job run` and `generate` (PENDING 228 follow-up; `brief run` has its own
//! registration in `brief.rs`).
//!
//! A caller names itself with `--caller <name>` or `<PREFIX>_CALLER`. Unlike `brief run` these two
//! commands have no `cli` default: with no caller named they behave exactly as before and tag
//! nothing. When a caller IS named, each file a completed job produced is tagged
//! (`<file>.caller.json`, `library::tag_in_place`) so the Gallery lists it under that caller.
//!
//! The same QA guard as `brief run` decides whether the real library may be touched
//! (`brief::registration_for`): a QA-marked (`<PREFIX>_QA`) engine never tags, and
//! neither does a run the caller isolated in its own data folder. These commands write straight into
//! the data folder in use, so there is never a copy into another library: the answer is `tag` or
//! `skip`, with the reason.
//!
//! The `--caller` flag is exported as `<PREFIX>_CALLER` by `main`, so the detached runner `generate`
//! starts inherits it and tags the outputs when the job completes. A tagging problem is reported
//! under `library` on the record and never fails a job that already completed.

use std::path::{Path, PathBuf};

use rightkit_media_gen::host::Caller;
use rightkit_media_gen::{api, library, Context};
use serde_json::{json, Value};

use crate::brief::{registration_for, DataRoot, Registration};

/// The caller this process was told to use (`<PREFIX>_CALLER`, which `main` also sets from
/// `--caller`). `Ok(None)` means none was named: no tagging, behaviour unchanged.
pub fn explicit_caller() -> Result<Option<Caller>, String> {
    match std::env::var(rightkit_media_gen::host::env_name("CALLER"))
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        Some(raw) => rightkit_media_gen::host::parse_caller(&raw).map(Some),
        None => Ok(None),
    }
}

/// Where these commands' outputs may be tagged: the folder in use is the explicit data folder when
/// one is set, else the per-user folder. Same decision table as `brief run`.
pub fn registration(
    explicit_root: Option<PathBuf>,
    user_root: &Path,
    qa: bool,
    debug: bool,
) -> Registration {
    let decided = match explicit_root {
        Some(path) => DataRoot::Explicit(path),
        None => DataRoot::UserLibrary,
    };
    registration_for(&decided, user_root, qa, debug)
}

fn current_registration() -> Registration {
    registration(
        rightkit_media_gen::host::explicit_data_root(),
        &rightkit_media_gen::host::user_data_root(),
        crate::brief::qa_marked(),
        false,
    )
}

/// Tag each of `files` per `mode`. `skipped` (with the reason), `tagged`, or `partial` when some
/// file could not be tagged (each failure names its file and error).
pub fn tag_files(files: &[PathBuf], caller: &Caller, mode: &Registration) -> Value {
    let head =
        |status: &str| json!({"caller": caller.slug, "label": caller.label, "status": status});
    match mode {
        Registration::Skip(reason) => {
            let mut v = head("skipped");
            v["reason"] = json!(reason);
            v
        }
        // Not reachable from `registration` (no copy into another library); never copies here.
        Registration::IntoUserLibrary => {
            let mut v = head("skipped");
            v["reason"] = json!("not_applicable");
            v
        }
        Registration::TagInPlace => {
            let mut tagged = Vec::new();
            let mut failed = Vec::new();
            for file in files {
                match library::tag_in_place(file, caller) {
                    Ok(path) => tagged.push(json!({"file": file.to_string_lossy(), "tag": path.to_string_lossy()})),
                    Err(e) => failed.push(json!({"file": file.to_string_lossy(), "error": {"code": e.code, "message": e.message}})),
                }
            }
            let mut v = head(if failed.is_empty() {
                "tagged"
            } else {
                "partial"
            });
            v["tagged"] = Value::Array(tagged);
            v["failed"] = Value::Array(failed);
            v
        }
    }
}

/// The files a completed job produced, from its stored result.
fn job_files(ctx: &Context, job_id: &str) -> Result<Vec<PathBuf>, rightkit_media_gen::GenError> {
    let result = api::job_result(ctx, &json!({"job_id": job_id}))?;
    Ok(result["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["path"].as_str().map(PathBuf::from))
        .collect())
}

/// Tag a completed job's outputs under `caller`; the `library` entry for the record.
pub fn tag_job(ctx: &Context, job_id: &str, caller: &Caller, mode: &Registration) -> Value {
    if let Registration::Skip(_) = mode {
        return tag_files(&[], caller, mode);
    }
    match job_files(ctx, job_id) {
        Ok(files) => tag_files(&files, caller, mode),
        Err(e) => json!({
            "caller": caller.slug, "label": caller.label, "status": "failed",
            "error": {"code": e.code, "message": e.message},
        }),
    }
}

/// `job run` / `generate --wait`: when a caller was named and the job completed, tag its outputs and
/// report under `library`. A no-op otherwise.
pub fn tag_completed(ctx: &Context, record: &mut Value) {
    let Ok(Some(caller)) = explicit_caller() else {
        return;
    };
    if record["status"] != "complete" {
        return;
    }
    let Some(job_id) = record["job_id"].as_str().map(String::from) else {
        return;
    };
    record["library"] = tag_job(ctx, &job_id, &caller, &current_registration());
}
