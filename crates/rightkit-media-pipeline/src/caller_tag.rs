//! Caller tagging for `job run` and `generate` (PENDING 228 follow-up; `brief run` has its own
//! registration in `brief.rs`).
//!
//! A caller names itself with `--caller <name>` or `<PREFIX>_CALLER`. Unlike `brief run` these two
//! commands have no `cli` default: with no caller named they behave exactly as before and tag
//! nothing. When a caller IS named, each file a completed job produced is tagged
//! (`<file>.caller.json`, `library::tag_in_place`) so the Gallery lists it under that caller.
//!
//! The same QA guard as `brief run` decides whether the real library may be touched
//! (`brief::registration_for`): a QA-marked (`<PREFIX>_QA`) or debug-build engine never tags, and
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
        rightkit_media_gen::host::qa_marked(),
        cfg!(debug_assertions),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> Caller {
        rightkit_media_gen::host::parse_caller("PostApp").unwrap()
    }

    #[test]
    fn registration_follows_the_brief_run_guard() {
        let user = std::env::temp_dir().join("rightkit-caller-tag-user-root-does-not-exist");
        let iso = Some(std::env::temp_dir().join("rightkit-caller-tag-isolated"));
        // The per-user folder: tagged; a QA-marked or debug engine never tags.
        assert_eq!(
            registration(None, &user, false, false),
            Registration::TagInPlace
        );
        assert_eq!(
            registration(None, &user, true, false),
            Registration::Skip("qa_marked")
        );
        assert_eq!(
            registration(None, &user, false, true),
            Registration::Skip("debug_build")
        );
        // An isolated explicit folder stays isolated.
        assert_eq!(
            registration(iso, &user, false, false),
            Registration::Skip("explicit_data_folder")
        );
    }

    #[test]
    fn caller_tagging_on_job_run_tags_a_completed_jobs_files_and_only_when_allowed() {
        let dir =
            std::env::temp_dir().join(format!("rightkit-caller-job-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = Context::new(dir.clone());
        let file = dir.join("out.png");
        std::fs::write(&file, b"png").unwrap();

        let db = rightkit_media_gen::store::open(&ctx).unwrap();
        let (job, _) = rightkit_media_gen::store::insert_or_existing(
            &db,
            &rightkit_media_gen::store::NewJob {
                request_id: Some("caller-tag-job-run"),
                kind: "image",
                provider: "fake",
                model: "m",
                request_json: "{}",
                est_usd: 0.0,
            },
        )
        .unwrap();
        rightkit_media_gen::store::mark_complete(
            &db,
            &job.job_id,
            &json!({"assets": [{"asset_id": "a1", "path": file.to_string_lossy()}]}),
        )
        .unwrap();

        // Guard says skip: nothing is written beside the file.
        let skipped = tag_job(
            &ctx,
            &job.job_id,
            &caller(),
            &Registration::Skip("qa_marked"),
        );
        assert_eq!(
            (skipped["status"].as_str(), skipped["reason"].as_str()),
            (Some("skipped"), Some("qa_marked"))
        );
        assert!(!dir.join("out.png.caller.json").exists());

        // Guard says tag: the job's file carries the caller tag the Gallery reads.
        let tagged = tag_job(&ctx, &job.job_id, &caller(), &Registration::TagInPlace);
        assert_eq!(tagged["status"], "tagged", "{tagged}");
        let body: Value =
            serde_json::from_slice(&std::fs::read(dir.join("out.png.caller.json")).unwrap())
                .unwrap();
        assert_eq!(
            (body["caller"].as_str(), body["label"].as_str()),
            (Some("postapp"), Some("PostApp"))
        );

        // Another caller cannot retag the same file: reported, the original tag stays.
        let other = rightkit_media_gen::host::parse_caller("CutApp").unwrap();
        let clash = tag_job(&ctx, &job.job_id, &other, &Registration::TagInPlace);
        assert_eq!(clash["status"], "partial", "{clash}");
        let body: Value =
            serde_json::from_slice(&std::fs::read(dir.join("out.png.caller.json")).unwrap())
                .unwrap();
        assert_eq!(body["caller"], "postapp");

        // A job that is not complete tags nothing (no result to read).
        let missing = tag_job(&ctx, "no-such-job", &caller(), &Registration::TagInPlace);
        assert_eq!(missing["status"], "failed");
        let _ = std::fs::remove_dir_all(dir);
    }
}
