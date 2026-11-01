//! rightkit-job-queue: a durable job on a real SQLite store: submit (idempotent by request id), run through the Runner with a handler,
//! read the stored result back; a failing kind ends failed with its error.
use std::sync::Arc;

use rightkit_job_queue::{
    Job, JobControl, JobError, JobHandler, JobLocks, JobResult, JobStore, NewJob, Runner,
};
use serde_json::{json, Value};

struct Double;

impl JobHandler for Double {
    fn run(&self, job: &Job, _ctl: &JobControl) -> JobResult<Value> {
        match job.kind.as_str() {
            "double" => Ok(json!({ "n": job.request["n"].as_i64().unwrap_or(0) * 2 })),
            other => Err(JobError::new(
                "unknown_kind",
                format!("no handler for {other}"),
            )),
        }
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("job_queue")?;
    std::fs::create_dir_all(dir.join("locks")).map_err(|e| e.to_string())?;
    let store =
        Arc::new(JobStore::open(dir.join("jobs.sqlite")).map_err(|e| format!("open: {e}"))?);
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.join("locks")),
        Arc::new(Double),
    );
    let new = NewJob {
        request_id: Some("probe-1".into()),
        kind: "double".into(),
        request: json!({ "n": 21 }),
    };
    // `submit` returns `(job, deduplicated)`.
    let (job, first_dedup) = store.submit(&new).map_err(|e| format!("submit: {e}"))?;
    let (again, again_dedup) = store.submit(&new).map_err(|e| format!("resubmit: {e}"))?;
    if first_dedup || !again_dedup || again.job_id != job.job_id {
        return Err(format!("a resubmission with the same request id was not deduplicated (first deduplicated {first_dedup}, again {again_dedup})"));
    }
    runner.run(&job.job_id).map_err(|e| format!("run: {e}"))?;
    let done = store.get(&job.job_id).map_err(|e| format!("get: {e}"))?;
    if done.result != Some(json!({ "n": 42 })) {
        return Err(format!(
            "job ended {} with result {:?}",
            done.status, done.result
        ));
    }
    let (bad, _) = store
        .submit(&NewJob {
            request_id: None,
            kind: "nope".into(),
            request: json!({}),
        })
        .map_err(|e| format!("submit bad: {e}"))?;
    let _ = runner.run(&bad.job_id);
    let failed = store
        .get(&bad.job_id)
        .map_err(|e| format!("get bad: {e}"))?;
    if failed.error.is_none() {
        return Err(format!(
            "an unknown kind ended {} without an error",
            failed.status
        ));
    }
    Ok(json!({ "status": done.status, "result": done.result, "failed_status": failed.status }))
}
