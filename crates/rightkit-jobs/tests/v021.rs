//! 0.2.1 additions: explicit event status/attempt, optional `retryable` in
//! errors, and a runner that emits no implicit events.

use std::io::{BufReader, Cursor};
use std::sync::{Arc, Mutex};

use rightkit_jobs::*;
use serde_json::{json, Value};

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "k".into(),
        request: json!({}),
    }
}

fn two_status() -> StatusModel {
    StatusModel::new("submitted", ["submitted", "running"])
        .allow("submitted", "running")
        .allow("running", "submitted")
}

#[test]
fn add_event_with_records_explicit_status_and_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(
        dir.path().join("j.db"),
        StoreConfig::new().statuses(two_status()),
    )
    .unwrap();
    let (job, _) = store.submit(&new_job()).unwrap();
    let id = job.job_id;
    // Default: from the row (status `submitted`, attempts 0).
    let d = store.add_event(&id, "downloading", None, None).unwrap();
    assert_eq!((d.status.as_str(), d.attempt), ("submitted", 0));
    // Explicit status and attempt.
    let e = store
        .add_event_with(
            &id,
            Some("running"),
            "downloading",
            Some(7),
            Some(0.5),
            Some("m"),
        )
        .unwrap();
    assert_eq!(
        (e.status.as_str(), e.attempt, e.progress),
        ("running", 7, Some(0.5))
    );
    // Partial: status only keeps the row attempt; attempt only keeps the row status.
    let s = store
        .add_event_with(&id, Some("running"), "x", None, None, None)
        .unwrap();
    assert_eq!((s.status.as_str(), s.attempt), ("running", 0));
    let a = store
        .add_event_with(&id, None, "x", Some(3), None, None)
        .unwrap();
    assert_eq!((a.status.as_str(), a.attempt), ("submitted", 3));
    // Stored as given.
    let evs = store.events_after(&id, 0).unwrap();
    assert_eq!(evs[1].status, "running");
    assert_eq!(evs[1].attempt, 7);
    // Unknown status and unknown job are refused.
    assert_eq!(
        store
            .add_event_with(&id, Some("bogus"), "x", None, None, None)
            .unwrap_err()
            .code,
        "bad_input"
    );
    assert_eq!(
        store
            .add_event_with("nope", None, "x", None, None, None)
            .unwrap_err()
            .code,
        "job_not_found"
    );
}

#[test]
fn retryable_is_optional_in_stored_errors() {
    let err = JobError::retryable("rate", "slow down");
    assert_eq!(err.to_json()["retryable"], true);
    assert!(err.to_json_with(false).get("retryable").is_none());

    for (include, cfg) in [
        (true, StoreConfig::new()),
        (true, StoreConfig::new().include_retryable(true)),
        (false, StoreConfig::new().include_retryable(false)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open_configured(dir.path().join("j.db"), cfg).unwrap();
        let (a, _) = store.submit(&new_job()).unwrap();
        store.record_error(&a.job_id, &err).unwrap();
        let e = store.get(&a.job_id).unwrap().error.unwrap();
        assert_eq!(e.get("retryable").is_some(), include, "recorded: {e}");
        assert_eq!(e["code"], "rate");
        store.fail(&a.job_id, &err).unwrap();
        let e = store.get(&a.job_id).unwrap().error.unwrap();
        assert_eq!(e.get("retryable").is_some(), include, "failed: {e}");
        assert_eq!(e["message"], "slow down");
        let v = store.get(&a.job_id).unwrap().to_value();
        assert_eq!(v["error"].get("retryable").is_some(), include);
    }
}

#[test]
fn retryable_is_stripped_from_errors_stored_before_the_option() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("j.db");
    let id = {
        let store = JobStore::open(&db).unwrap();
        let (a, _) = store.submit(&new_job()).unwrap();
        store
            .fail(&a.job_id, &JobError::new("boom", "bad"))
            .unwrap();
        assert_eq!(
            store.get(&a.job_id).unwrap().error.unwrap()["retryable"],
            false
        );
        a.job_id
    };
    let store =
        JobStore::open_configured(&db, StoreConfig::new().include_retryable(false)).unwrap();
    let e = store.get(&id).unwrap().error.unwrap();
    assert_eq!(e, json!({"code": "boom", "message": "bad"}));
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Fails;
impl CommandSet for Fails {
    fn methods(&self) -> Vec<String> {
        vec!["fail".into()]
    }
    fn call(&self, _: &str, _: &Value, _: bool, _: &Sink) -> JobResult<Value> {
        Err(JobError::retryable("later", "try again"))
    }
}

#[test]
fn dispatcher_error_retryable_is_optional() {
    for include in [true, false] {
        let d = Dispatcher::new("t", "1")
            .with(Arc::new(Fails))
            .with_error_retryable(include);
        let out = SharedBuf::default();
        serve(
            Arc::new(d),
            BufReader::new(Cursor::new(
                r#"{"type":"request","id":1,"method":"fail","params":{}}"#,
            )),
            out.clone(),
            false,
        )
        .unwrap();
        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let resp: Value = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|f| f["type"] == "response")
            .unwrap();
        assert_eq!(resp["error"].get("retryable").is_some(), include, "{resp}");
        assert_eq!(resp["error"]["code"], "later");
    }
}

struct Own;
impl JobHandler for Own {
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value> {
        ctl.progress("submitted", None, None);
        if job.kind == "fail" {
            return Err(JobError::new("nope", "failed"));
        }
        ctl.progress("downloading", Some(0.9), None);
        Ok(json!({"ok": true}))
    }
}

fn stages(store: &JobStore, id: &str) -> Vec<String> {
    store
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.stage)
        .collect()
}

#[test]
fn runner_implicit_events_can_be_turned_off() {
    for (implicit, ok_stages, fail_stages) in [
        (
            true,
            vec!["started", "submitted", "downloading", "complete"],
            vec!["started", "submitted", "failed"],
        ),
        (false, vec!["submitted", "downloading"], vec!["submitted"]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::open(dir.path().join("j.db")).unwrap());
        let runner = Runner::new(
            store.clone(),
            JobLocks::new(dir.path().join("locks")),
            Arc::new(Own),
        )
        .with_implicit_events(implicit);
        assert_eq!(runner.implicit_events(), implicit);
        let (a, _) = store.submit(&new_job()).unwrap();
        let RunOutcome::Finished(j) = runner.run(&a.job_id).unwrap() else {
            panic!("not finished")
        };
        assert_eq!(j.status, COMPLETE);
        assert_eq!(stages(&store, &a.job_id), ok_stages);

        let (b, _) = store
            .submit(&NewJob {
                kind: "fail".into(),
                ..new_job()
            })
            .unwrap();
        let RunOutcome::Finished(j) = runner.run(&b.job_id).unwrap() else {
            panic!("not finished")
        };
        assert_eq!(j.status, FAILED);
        assert_eq!(stages(&store, &b.job_id), fail_stages);
    }
}
