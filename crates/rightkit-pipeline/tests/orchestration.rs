//! Orchestration against the fake genright-engine (a real child process that
//! emulates GenRight's contract-v2 CLI), including crash scenarios.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_pipeline::orchestrator::Failpoint;
use rightkit_pipeline::{
    DurationRule, Error, GenRightClient, GenerateSpec, JobListQuery, MediaKind, Orchestrator,
    ProcessRunner, RawOutput, RequestEntry, RequestState, Runner, WaitOptions, MAX_INPUT_ARG_BYTES,
};
use serde_json::{json, Value};
use tempfile::TempDir;

struct Env {
    fake: TempDir,
    project: TempDir,
}

fn env() -> Env {
    Env {
        fake: TempDir::new().unwrap(),
        project: TempDir::new().unwrap(),
    }
}

fn engine() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-genright-engine"))
}

fn orch(e: &Env, extra: &[(&str, &str)]) -> Orchestrator<ProcessRunner> {
    let mut r = ProcessRunner::new(engine())
        .with_env("FAKE_GENRIGHT_DIR", e.fake.path().to_string_lossy())
        .with_env("FAKE_PROJECT", e.project.path().to_string_lossy());
    for (k, v) in extra {
        r = r.with_env(*k, *v);
    }
    Orchestrator::new(e.project.path(), r)
}

fn spec(prompt: &str) -> GenerateSpec {
    GenerateSpec {
        kind: MediaKind::Image,
        provider: "fake".into(),
        model: "fake-img".into(),
        prompt: prompt.into(),
        params: json!({"aspect": "16:9"}),
    }
}

/// Fake-engine state. A missing file is a fresh engine; an unreadable or
/// unparsable file fails the test. Negative assertions ("no ack", "no
/// generate") must never pass because the state silently defaulted.
fn state(e: &Env) -> Value {
    let path = e.fake.path().join("state.json");
    match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)
            .unwrap_or_else(|err| panic!("corrupt fake state {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            json!({"jobs": [], "generate_calls": 0, "ack_log": []})
        }
        Err(err) => panic!("unreadable fake state {}: {err}", path.display()),
    }
}

fn generate_calls(e: &Env) -> i64 {
    state(e)["generate_calls"].as_i64().unwrap_or(0)
}

fn job_count(e: &Env) -> usize {
    state(e)["jobs"].as_array().map(Vec::len).unwrap_or(0)
}

fn ack_log(e: &Env) -> Vec<Value> {
    state(e)["ack_log"].as_array().cloned().unwrap_or_default()
}

fn fast() -> WaitOptions {
    WaitOptions {
        poll_interval: Duration::from_millis(5),
        timeout: Some(Duration::from_secs(30)),
        cancel: None,
    }
}

fn media_files(project: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(project.join("media"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn submit_and_finish(o: &Orchestrator<ProcessRunner>) -> String {
    let sub = o.submit(&spec("a red fox"), None).unwrap();
    o.wait(&sub.request_id, &fast()).unwrap();
    sub.request_id
}

#[test]
fn capability_check_rejects_old_and_future_engines_before_any_call() {
    for (env_key, mode, want) in [
        ("FAKE_HIDE", "job.list", "capability"),
        ("FAKE_HIDE", "asset.resolve", "capability"),
        ("FAKE_HIDE", "progress.ndjson", "capability"),
        ("FAKE_DOCTOR", "v1", "contract1"),
        ("FAKE_DOCTOR", "v3", "contract3"),
    ] {
        let e = env();
        let o = orch(&e, &[(env_key, mode)]);
        let err = o.submit(&spec("x"), None).unwrap_err();
        match (want, &err) {
            ("capability", Error::MissingCapability { .. })
            | (
                "contract1",
                Error::ContractVersion {
                    found: 1,
                    required: 2,
                },
            )
            | (
                "contract3",
                Error::ContractVersion {
                    found: 3,
                    required: 2,
                },
            ) => {}
            _ => panic!("{mode}: unexpected {err:?}"),
        }
        assert_eq!(generate_calls(&e), 0);
        assert!(
            !e.project.path().join("imports/journal.json").exists(),
            "nothing journaled"
        );
    }
    let e = env();
    let info = orch(&e, &[]).ensure_ready().unwrap().clone();
    assert_eq!(info.contract_version, 2);
    assert_eq!(info.capability("job.list"), Some(1));
    assert_eq!(info.capability("asset.resolve"), Some(1));
}

#[test]
fn missing_engine_is_a_typed_spawn_error() {
    let e = env();
    let o = Orchestrator::new(
        e.project.path(),
        ProcessRunner::new(e.fake.path().join("absent")),
    );
    assert!(matches!(o.ensure_ready(), Err(Error::Spawn { .. })));
}

#[test]
fn request_never_reaching_genright_is_submitted_once_on_recovery() {
    let e = env();
    let o = orch(&e, &[]);
    o.journal()
        .update(|d| {
            d.requests.insert(
                "req-lost".into(),
                RequestEntry {
                    request_id: "req-lost".into(),
                    kind: MediaKind::Video,
                    provider: "fake".into(),
                    model: "fake-vid".into(),
                    prompt: "lost".into(),
                    params: json!({}),
                    state: RequestState::Pending,
                    job_id: None,
                    error: None,
                    created_at: 1,
                    place: None,
                },
            );
            Ok(())
        })
        .unwrap();
    let out = o.recover().unwrap();
    assert_eq!(out[0].1.as_ref().unwrap().job_id, "job-1");
    assert_eq!(job_count(&e), 1);
    assert_eq!(state(&e)["jobs"][0]["kind"], "video");
    assert!(o.recover().unwrap().is_empty());
    assert_eq!(generate_calls(&e), 1);
}

#[test]
fn repeated_status_calls_are_stable() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    for _ in 0..4 {
        let s = o.job_status(&rid).unwrap();
        assert_eq!(s.status, "complete");
        assert_eq!(s.job_id, "job-1");
    }
    assert_eq!(job_count(&e), 1);
    assert_eq!(generate_calls(&e), 1);
    // Reattaching with a fresh orchestrator (restart) works the same.
    let again = orch(&e, &[]).wait(&rid, &fast()).unwrap();
    assert_eq!(again.status, "complete");
}

#[test]
fn import_interrupted_after_copy_before_journal_write_recovers_without_duplicates() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    o.set_failpoint(Some(Failpoint::AfterCopyBeforeJournal));
    // The kill point is named exactly: a different point firing fails here.
    assert!(
        matches!(
            o.import(&rid),
            Err(Error::Injected("after copy, before journal write"))
        ),
        "failpoint did not fire at the copy/journal boundary"
    );
    assert_eq!(media_files(e.project.path()).len(), 1, "copy is durable");
    assert!(o.imports().unwrap().is_empty(), "journal not written");
    assert!(ack_log(&e).is_empty(), "no ack before the journal record");
    assert!(state(&e)["jobs"][0]["acked_by"].is_null());

    // Restart and import again: adopts the copy, records once, then acks.
    let o2 = orch(&e, &[]);
    let recs = o2.import(&rid).unwrap();
    assert_eq!(recs.len(), 1);
    assert!(recs[0].acked);
    assert_eq!(o2.imports().unwrap().len(), 1);
    assert_eq!(media_files(e.project.path()).len(), 1);
    assert_eq!(ack_log(&e).len(), 1);
}

#[test]
fn import_interrupted_after_journal_before_ack_acks_on_retry() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    o.set_failpoint(Some(Failpoint::AfterJournalBeforeAck));
    assert!(
        matches!(
            o.import(&rid),
            Err(Error::Injected("after journal write, before ack"))
        ),
        "failpoint did not fire at the journal/ack boundary"
    );
    let rec = o.imports().unwrap().remove(0);
    assert!(!rec.acked);
    assert!(ack_log(&e).is_empty());
    assert!(state(&e)["jobs"][0]["acked_by"].is_null());

    let o2 = orch(&e, &[]);
    let recs = o2.import(&rid).unwrap();
    assert!(recs[0].acked);
    assert_eq!(o2.imports().unwrap().len(), 1);
    // The retry must actually ack: the engine records the consumer.
    assert_eq!(ack_log(&e).len(), 1, "retry skipped the ack");
    assert_eq!(state(&e)["jobs"][0]["acked_by"], "rightkit");
    assert_eq!(media_files(e.project.path()).len(), 1);
}

#[test]
fn reimport_is_idempotent_and_repairs_a_deleted_copy() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    let first = o.import(&rid).unwrap();
    let second = o.import(&rid).unwrap();
    assert_eq!(first, second);
    assert_eq!(o.imports().unwrap().len(), 1);
    assert_eq!(media_files(e.project.path()).len(), 1);
    assert_eq!(ack_log(&e).len(), 1, "acked once");

    std::fs::remove_file(&first[0].media_path).unwrap();
    let third = o.import(&rid).unwrap();
    assert_eq!(third[0].sha256, first[0].sha256);
    assert!(third[0].media_path.exists());
    assert_eq!(o.imports().unwrap().len(), 1);
}

#[test]
fn sha_mismatch_is_rejected_with_no_residue_and_no_ack() {
    let e = env();
    let o = orch(&e, &[("FAKE_BAD_SHA", "1")]);
    let rid = submit_and_finish(&o);
    let err = o.import(&rid).unwrap_err();
    assert!(matches!(err, Error::ShaMismatch { .. }), "{err:?}");
    assert!(
        media_files(e.project.path()).is_empty(),
        "no copy, no temp file left"
    );
    assert!(o.imports().unwrap().is_empty());
    assert!(ack_log(&e).is_empty());
}

#[test]
fn ack_failure_keeps_the_import_and_flush_acks_retries() {
    let e = env();
    let o = orch(&e, &[("FAKE_ACK_FAIL", "1")]);
    let rid = submit_and_finish(&o);
    let recs = o.import(&rid).unwrap();
    assert!(!recs[0].acked);
    assert!(recs[0].media_path.exists());

    let o2 = orch(&e, &[]);
    assert_eq!(o2.flush_acks().unwrap(), 1);
    assert!(o2.imports().unwrap()[0].acked);
    assert_eq!(o2.flush_acks().unwrap(), 0);
    assert_eq!(state(&e)["jobs"][0]["acked_by"], "rightkit");
}

#[test]
fn result_cleaned_after_ack_still_answers_from_the_journal() {
    let e = env();
    let o = orch(&e, &[("FAKE_CLEAN_ON_ACK", "1")]);
    let rid = submit_and_finish(&o);
    let first = o.import(&rid).unwrap();
    assert!(first[0].acked);
    // GenRight released the file; re-import must still succeed from our copy.
    assert!(!e.fake.path().join("library/asset-job-1").exists());
    let again = o.import(&rid).unwrap();
    assert_eq!(first, again);
}

#[test]
fn result_cleaned_before_any_import_is_a_typed_error() {
    let e = env();
    let o = orch(&e, &[("FAKE_CLEAN_ON_ACK", "1")]);
    let rid = submit_and_finish(&o);
    // Someone else acked it, so GenRight cleaned the result.
    o.client().job_ack("job-1", "other").unwrap();
    assert!(matches!(o.import(&rid), Err(Error::ResultCleaned { .. })));
    assert!(o.imports().unwrap().is_empty());
}

#[test]
fn relocated_or_missing_source_is_a_typed_error_not_a_partial_import() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    std::fs::remove_dir_all(e.fake.path().join("library")).unwrap();
    let err = o.import(&rid).unwrap_err();
    assert!(matches!(err, Error::SourceMissing { .. }), "{err:?}");
    assert!(media_files(e.project.path()).is_empty());
    assert!(o.imports().unwrap().is_empty());
    assert!(ack_log(&e).is_empty());
}

#[test]
fn import_before_completion_is_not_ready() {
    let e = env();
    let o = orch(&e, &[("FAKE_STEPS", "1000")]);
    let sub = o.submit(&spec("slow"), None).unwrap();
    let err = o.import(&sub.request_id).unwrap_err();
    assert!(matches!(err, Error::NotReady { .. }), "{err:?}");
    let err = o
        .wait(
            &sub.request_id,
            &WaitOptions {
                poll_interval: Duration::from_millis(1),
                timeout: Some(Duration::from_millis(30)),
                cancel: None,
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::WaitTimeout { .. }));
}

#[test]
fn permanent_genright_rejection_is_recorded_and_not_retried() {
    let e = env();
    let o = orch(&e, &[]);
    let mut bad = spec("x");
    bad.model = "nope".into();
    let err = o.submit(&bad, Some("req-bad")).unwrap_err();
    assert!(matches!(err, Error::RequestFailed { ref code, .. } if code == "unknown_model"));
    assert_eq!(o.request("req-bad").unwrap().state, RequestState::Failed);
    let calls = generate_calls(&e);
    assert!(matches!(
        o.submit(&bad, Some("req-bad")),
        Err(Error::RequestFailed { .. })
    ));
    assert!(o.recover().unwrap().is_empty());
    assert_eq!(generate_calls(&e), calls);
}

#[test]
fn journal_is_atomic_and_corruption_is_never_overwritten() {
    let e = env();
    let o = orch(&e, &[]);
    submit_and_finish(&o);
    let dir = e.project.path().join("imports");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|d| d.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| n == "journal.json" || n == "journal.lock"),
        "{names:?}"
    );

    let path = dir.join("journal.json");
    std::fs::write(&path, b"{ not json").unwrap();
    let o2 = orch(&e, &[]);
    assert!(matches!(
        o2.submit(&spec("y"), None),
        Err(Error::JournalCorrupt { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
}

#[test]
fn bad_ids_are_refused() {
    let e = env();
    let o = orch(&e, &[]);
    assert!(matches!(
        o.submit(&spec("x"), Some("../evil")),
        Err(Error::BadId(_))
    ));
    assert!(matches!(
        o.import_job("a/b", None, None),
        Err(Error::BadId(_))
    ));
    assert_eq!(generate_calls(&e), 0);
}

#[test]
fn job_cancel_stops_a_running_job_and_finished_jobs_keep_their_result() {
    let e = env();
    let o = orch(&e, &[("FAKE_STEPS", "1000")]);
    let sub = o.submit(&spec("slow"), Some("req-cancel")).unwrap();
    let rec = o.client().job_cancel(&sub.job_id).unwrap();
    assert_eq!(rec.status, "cancelled");
    // A cancelled job is terminal and stays cancelled across polls.
    let done = o.wait(&sub.request_id, &fast()).unwrap();
    assert_eq!(done.status, "cancelled");
    assert_eq!(o.job_status(&sub.request_id).unwrap().status, "cancelled");
    assert!(o.import(&sub.request_id).is_err(), "nothing to import");
    assert!(o.imports().unwrap().is_empty());

    // Cancelling a complete job is a no-op: its result stays importable.
    // (A fresh fake: this one completes after the default two polls.)
    let e2 = env();
    let o2 = orch(&e2, &[]);
    let sub2 = o2.submit(&spec("done"), None).unwrap();
    o2.wait(&sub2.request_id, &fast()).unwrap();
    assert_eq!(
        o2.client().job_cancel(&sub2.job_id).unwrap().status,
        "complete"
    );
    assert_eq!(o2.import(&sub2.request_id).unwrap().len(), 1);
}

#[test]
fn record_cost_writes_job_costs_onto_the_jobs_imports() {
    let e = env();
    let o = orch(&e, &[]);
    let recs = o
        .generate_and_import(&spec("cost"), Some("req-cost"), &fast())
        .unwrap();
    assert_eq!(recs[0].est_usd, None);
    assert_eq!(recs[0].actual_usd, None);
    // The fake reports the job's estimate and charge on the record.
    let job = o.job_status("req-cost").unwrap();
    assert_eq!(job.est_usd, Some(0.02));
    assert_eq!(job.actual_usd, Some(0.021));
    o.record_cost("job-1", job.est_usd, job.actual_usd).unwrap();
    let imps = o.imports().unwrap();
    assert_eq!(imps[0].est_usd, Some(0.02));
    assert_eq!(imps[0].actual_usd, Some(0.021));
    // Existing values are kept; a later pass does not clobber provenance.
    o.record_cost("job-1", Some(9.9), Some(9.9)).unwrap();
    assert_eq!(o.imports().unwrap()[0].actual_usd, Some(0.021));
    // An unrelated job's records are untouched.
    o.record_cost("job-99", Some(1.0), None).unwrap();
    assert_eq!(o.imports().unwrap()[0].est_usd, Some(0.02));
}

// ---- contract v2: recovery, resolve, progress, probe, validation ----------

fn collect_events(o: &Orchestrator<ProcessRunner>) -> Arc<Mutex<Vec<Value>>> {
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = events.clone();
    o.set_event_sink(move |v| sink.lock().unwrap().push(v));
    events
}

/// Journal a request GenRight has (job-1, created before the "crash") plus
/// `others` newer jobs.
fn crashed_request_with_newer_jobs(e: &Env, others: usize) {
    let o = orch(e, &[("FAKE_CRASH_AFTER_GENERATE", "1")]);
    assert!(o.submit(&spec("crash"), Some("req-crash")).is_err());
    let plain = orch(e, &[]);
    for i in 0..others {
        plain
            .submit(
                &spec(&format!("other {i}")),
                Some(&format!("req-other-{i}")),
            )
            .unwrap();
    }
}

#[test]
fn recovery_uses_job_status_by_request_id_even_when_the_list_cannot_show_it() {
    let e = env();
    crashed_request_with_newer_jobs(&e, 3);
    assert_eq!(generate_calls(&e), 4);
    // The list is capped at 2 rows: only the dedicated request-id lookup
    // can find job-1.
    let o = orch(&e, &[("FAKE_LIST_CAP", "2")]);
    let out = o.recover().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].1.as_ref().unwrap().job_id, "job-1");
    assert_eq!(generate_calls(&e), 4, "generate not called again");
    assert_eq!(job_count(&e), 4);
}

#[test]
fn job_not_found_is_authoritative_and_submits_exactly_once() {
    let e = env();
    let o = orch(&e, &[]);
    o.journal()
        .update(|d| {
            d.requests.insert(
                "req-lost".into(),
                RequestEntry {
                    request_id: "req-lost".into(),
                    kind: MediaKind::Image,
                    provider: "fake".into(),
                    model: "fake-img".into(),
                    prompt: "lost".into(),
                    params: json!({}),
                    state: RequestState::Pending,
                    job_id: None,
                    error: None,
                    created_at: 1,
                    place: None,
                },
            );
            Ok(())
        })
        .unwrap();
    assert!(o
        .client()
        .job_status_by_request("req-lost")
        .unwrap()
        .is_none());
    let out = o.recover().unwrap();
    assert_eq!(out[0].1.as_ref().unwrap().job_id, "job-1");
    assert_eq!(generate_calls(&e), 1);
    assert!(o.recover().unwrap().is_empty());
}

#[test]
fn job_list_filters_and_reports_truncation() {
    let e = env();
    let o = orch(&e, &[("FAKE_LIST_CAP", "2")]);
    for i in 0..3 {
        o.submit(&spec(&format!("p{i}")), Some(&format!("req-{i}")))
            .unwrap();
    }
    let page = o.client().job_page(&JobListQuery::default()).unwrap();
    assert_eq!(page.jobs.len(), 2);
    assert!(page.truncated);
    assert_eq!(page.jobs[0].job_id, "job-3", "newest first");
    let one = o
        .client()
        .job_page(&JobListQuery {
            request_id: Some("req-0".into()),
            ..JobListQuery::default()
        })
        .unwrap();
    assert_eq!(one.jobs.len(), 1);
    assert!(!one.truncated);
}

struct ScriptRunner(Script);
type Script = Box<dyn Fn(&[&str]) -> String + Send + Sync>;

impl Runner for ScriptRunner {
    fn run(&self, words: &[&str], _input: Option<&str>) -> rightkit_pipeline::Result<RawOutput> {
        Ok(RawOutput {
            stdout: (self.0)(words),
            stderr: String::new(),
            code: Some(0),
        })
    }
}

#[test]
fn job_list_page_infers_truncation_when_the_engine_omits_the_flag() {
    let row = |n: usize| json!({"job_id": format!("j{n}"), "status": "complete"});
    let client = GenRightClient::new(ScriptRunner(Box::new(move |_| {
        json!({"jobs": [row(1), row(2)]}).to_string()
    })));
    let q = |limit| JobListQuery {
        limit: Some(limit),
        ..JobListQuery::default()
    };
    assert!(client.job_page(&q(2)).unwrap().truncated);
    assert!(!client.job_page(&q(3)).unwrap().truncated);
}

#[test]
fn job_record_carries_the_v2_fields() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    let rec = o.job_status(&rid).unwrap();
    assert_eq!(rec.status, "complete");
    assert!(rec.error.is_null());
    assert_eq!(rec.request["prompt"], "a red fox");
    assert_eq!(rec.progress["stage"], "complete");
    assert_eq!(rec.progress["event"], "job.progress");
    o.import(&rid).unwrap();
    let rec = o.job_status(&rid).unwrap();
    assert_eq!(rec.acked_by.as_deref(), Some("rightkit"));
    assert!(rec.acked_at.is_some());
}

#[test]
fn descriptor_is_read_from_assets_only_with_per_asset_provider_and_media() {
    let e = env();
    let o = orch(&e, &[]);
    let mut s = spec("pic");
    s.kind = MediaKind::Video;
    let recs = o.generate_and_import(&s, Some("req-v"), &fast()).unwrap();
    let r = &recs[0];
    assert_eq!(r.provider, "asset-fake");
    assert_eq!(r.model, "asset-fake-img");
    assert_eq!(r.media["width"], 1920);
    assert_eq!(r.media["height"], 1080);
    assert_eq!(r.media["duration_s"], 5.5);
    assert_eq!(r.kind, Some(MediaKind::Video));
    assert!(!r.needs_probe);
}

#[test]
fn null_probe_is_flagged_so_the_caller_falls_back_to_ffprobe() {
    let e = env();
    let o = orch(&e, &[("FAKE_NULL_MEDIA", "1")]);
    let r = o
        .generate_and_import(&spec("nul"), None, &fast())
        .unwrap()
        .remove(0);
    assert!(r.media["width"].is_null());
    assert!(r.needs_probe, "image without dimensions must be probed");
    assert!(rightkit_pipeline::media_needs_probe(
        &json!({"kind": "voice", "duration_s": null}),
        "audio/wav"
    ));
    assert!(!rightkit_pipeline::media_needs_probe(
        &json!({"kind": "voice", "duration_s": 2.0}),
        "audio/wav"
    ));
    assert!(rightkit_pipeline::media_needs_probe(
        &json!({"kind": "video", "width": 1, "height": 1, "duration_s": null}),
        "video/mp4"
    ));
}

#[test]
fn descriptor_parsing_ignores_flattening_and_falls_back_to_top_level_provider() {
    let client = GenRightClient::new(ScriptRunner(Box::new(|_| {
        // Flattened fields point at a different asset: they must be ignored.
        json!({
            "job_id": "j1", "provider": "top", "model": "top-model",
            "asset_id": "FLAT", "path": "/nope", "sha256": "0",
            "assets": [{"asset_id": "a1", "path": "/p/a1.png", "sha256": "ab",
                "bytes": 3, "mime": "image/png",
                "media": {"kind": "image", "width": 2, "height": 3, "duration_s": null}}],
        })
        .to_string()
    })));
    let d = client.job_result("j1").unwrap();
    assert_eq!(d.assets.len(), 1);
    assert_eq!(d.assets[0].asset_id, "a1");
    assert_eq!(d.assets[0].provider, None);
    assert_eq!(
        (d.provider.as_str(), d.model.as_str()),
        ("top", "top-model")
    );

    let client = GenRightClient::new(ScriptRunner(Box::new(|_| {
        json!({"job_id": "j1", "asset_id": "FLAT", "path": "/nope", "sha256": "0"}).to_string()
    })));
    assert!(matches!(
        client.job_result("j1"),
        Err(Error::BadOutput { .. })
    ));
}

#[test]
fn asset_resolve_finds_a_moved_library_and_verifies_sha256() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    std::fs::rename(e.fake.path().join("library"), e.fake.path().join("moved")).unwrap();
    let events = collect_events(&o);
    let recs = o.import(&rid).unwrap();
    assert_eq!(recs.len(), 1);
    let bytes = std::fs::read(&recs[0].media_path).unwrap();
    assert_eq!(recs[0].sha256, rightkit_fs::digest::sha256_bytes(&bytes));
    assert!(recs[0].acked);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|v| v["event"] == "genright.resolved"));
}

#[test]
fn resolved_file_with_wrong_content_is_a_sha_mismatch_with_no_residue() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    std::fs::rename(e.fake.path().join("library"), e.fake.path().join("moved")).unwrap();
    let file = std::fs::read_dir(e.fake.path().join("moved/asset-job-1"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    std::fs::write(&file, b"tampered").unwrap();
    let err = o.import(&rid).unwrap_err();
    assert!(matches!(err, Error::ShaMismatch { .. }), "{err:?}");
    assert!(media_files(e.project.path()).is_empty());
    assert!(o.imports().unwrap().is_empty());
    assert!(ack_log(&e).is_empty());
}

#[test]
fn resolve_saying_not_exists_or_unknown_asset_is_source_missing() {
    let e = env();
    let o = orch(&e, &[]);
    let rid = submit_and_finish(&o);
    std::fs::remove_dir_all(e.fake.path().join("library")).unwrap();
    let err = o.import(&rid).unwrap_err();
    assert!(matches!(err, Error::SourceMissing { .. }), "{err:?}");
    let res = o.client().asset_resolve("asset-job-1").unwrap().unwrap();
    assert!(!res.exists);
    assert!(o.client().asset_resolve("nope").unwrap().is_none());
    assert!(media_files(e.project.path()).is_empty());
    assert!(ack_log(&e).is_empty());
}

#[test]
fn asset_corrupt_from_genright_is_a_sha_mismatch() {
    let client = GenRightClient::new(ScriptRunner(Box::new(|_| {
        json!({"error": {"code": "asset_corrupt", "message": "sha256 mismatch"}}).to_string()
    })));
    assert!(matches!(
        client.asset_resolve("a1"),
        Err(Error::Engine { ref code, .. }) if code == "asset_corrupt"
    ));
}

#[test]
fn job_progress_events_are_forwarded_once_each() {
    let e = env();
    let o = orch(&e, &[("FAKE_STDERR", "1")]);
    let events = collect_events(&o);
    let rid = submit_and_finish(&o);
    assert_eq!(o.client().job_events("job-1").unwrap().len(), 2);
    let ev = events.lock().unwrap();
    let stages: Vec<&str> = ev
        .iter()
        .filter(|v| v["event"] == "genright.job_progress" && v["request_id"] == rid.as_str())
        .filter_map(|v| v["stage"].as_str())
        .collect();
    assert_eq!(stages, vec!["polling", "complete"], "{ev:?}");
    let done = ev
        .iter()
        .find(|v| v["event"] == "genright.job_progress" && v["stage"] == "complete")
        .unwrap();
    assert_eq!(done["job_id"], "job-1");
    assert_eq!(done["status"], "complete");
    assert_eq!(done["attempt"], 1);
    assert!(done["at"].is_string());
}

#[test]
fn progress_event_maps_job_progress_and_keeps_unknown_lines_generic() {
    use rightkit_pipeline::progress_event;
    let line = json!({"event": "job.progress", "job_id": "j", "status": "running",
        "stage": "downloading", "attempt": 2, "at": "t", "message": "m"});
    let ev = progress_event(&line, Some("r"), None);
    assert_eq!(ev["event"], "genright.job_progress");
    assert_eq!(
        (ev["stage"].as_str(), ev["message"].as_str()),
        (Some("downloading"), Some("m"))
    );
    assert_eq!(ev["request_id"], "r");
    let other = progress_event(&json!({"event": "x"}), None, Some("cmd"));
    assert_eq!(other["event"], "genright.progress");
    assert_eq!(other["data"]["event"], "x");
}

fn vspec(provider: &str, model: &str, duration: Value) -> GenerateSpec {
    let mut s = spec("clip");
    s.kind = MediaKind::Video;
    s.provider = provider.into();
    s.model = model.into();
    s.params = json!({ "duration": duration });
    s
}

#[test]
fn durations_come_from_the_engine_catalog_and_bad_ones_are_refused_before_journaling() {
    let e = env();
    let o = orch(&e, &[]);
    let cat = o.models().unwrap();
    assert_eq!(
        cat.get("wavespeed", "wan-ultra").unwrap().durations,
        DurationRule::Set { secs: vec![5, 8] }
    );
    assert_eq!(
        cat.get("wavespeed", "kling-3-pro").unwrap().durations,
        DurationRule::Range { min: 3, max: 15 }
    );
    assert_eq!(
        cat.get("fal", "fal-ai/veo3.1/fast").unwrap().durations,
        DurationRule::Default { secs: 4 }
    );
    assert_eq!(
        cat.cheapest("image").unwrap().model,
        "fal-ai/flux-2/klein/9b"
    );
    assert_eq!(cat.get("heygen", "avatar-video").unwrap().kind, "avatar");

    for (p, m, bad) in [
        ("wavespeed", "wan-ultra", json!(6)),
        ("wavespeed", "wan-ultra", json!("abc")),
        ("wavespeed", "kling-3-pro", json!(2)),
        ("wavespeed", "kling-3-pro", json!("16s")),
        ("fake", "fake-vid", json!(4)),
    ] {
        let err = o
            .submit(&vspec(p, m, bad.clone()), Some("req-clip"))
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidParams { ref field, .. } if field == "params.duration"),
            "{m} {bad}: {err:?}"
        );
    }
    let v = o
        .check_spec(&vspec("wavespeed", "wan-ultra", json!(6)))
        .unwrap_err();
    assert_eq!(v.allowed, ["5", "8"]);
    assert!(v.message.contains("5 or 8"));
    assert!(o.requests().unwrap().is_empty(), "nothing journaled");
    assert_eq!(generate_calls(&e), 0);

    for (p, m, ok) in [
        ("wavespeed", "wan-ultra", json!(5)),
        ("wavespeed", "wan-ultra", json!("8s")),
        ("wavespeed", "kling-3-pro", json!(15)),
        ("fal", "fal-ai/veo3.1/fast", json!(6)), // label gives a default only; the engine decides
    ] {
        o.submit(&vspec(p, m, ok), None).unwrap();
    }
    // Unknown models are the engine's call (unknown_model), not ours.
    o.submit(&vspec("other", "mystery", json!(7)), None)
        .unwrap();
}

const DUR: &[(&str, &str)] = &[("FAKE_DURATIONS", "1")];

#[test]
fn structured_durations_are_read_when_the_engine_advertises_the_capability() {
    let e = env();
    let o = orch(&e, DUR);
    assert_eq!(
        o.ensure_ready().unwrap().capability("models.durations"),
        Some(1)
    );
    let cat = o.models().unwrap();
    assert!(cat.structured_durations);
    let m = |p: &str, n: &str| cat.get(p, n).unwrap();
    assert_eq!(
        m("fal", "fal-ai/veo3.1/fast").durations,
        DurationRule::Set {
            secs: vec![4, 6, 8]
        }
    );
    assert_eq!(m("fal", "fal-ai/veo3.1/fast").default_duration_s, Some(4));
    assert_eq!(
        m("fake", "fake-one").durations,
        DurationRule::Set { secs: vec![4] }
    );
    assert_eq!(
        m("wavespeed", "kling-3-pro").durations,
        DurationRule::Range { min: 3, max: 15 }
    );
    assert_eq!(
        m("wavespeed", "wan-ultra").durations,
        DurationRule::Set { secs: vec![5, 8] }
    );
    let it = m("wavespeed", "infinitetalk");
    assert_eq!(it.durations, DurationRule::Range { min: 3, max: 600 });
    assert_eq!(it.default_duration_s, None);
    // No duration fields: nothing enforced, and the label is not parsed.
    assert_eq!(m("fake", "fake-voice").durations, DurationRule::Unspecified);
    assert_eq!(
        m("heygen", "avatar-video").durations,
        DurationRule::Unspecified
    );
}

#[test]
fn with_the_capability_veo_five_seconds_is_refused_before_journaling_and_six_is_accepted() {
    let e = env();
    let o = orch(&e, DUR);
    for bad in [json!(5), json!("5s"), json!(10)] {
        let err = o
            .submit(
                &vspec("fal", "fal-ai/veo3.1/fast", bad.clone()),
                Some("req-veo"),
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidParams { ref field, ref message }
                if field == "params.duration" && message.contains("4 or 6 or 8")),
            "{bad}: {err:?}"
        );
    }
    // A one-value array is a closed set too.
    let err = o
        .submit(&vspec("fake", "fake-one", json!(6)), None)
        .unwrap_err();
    assert!(matches!(err, Error::InvalidParams { .. }), "{err:?}");
    assert_eq!(
        o.check_spec(&vspec("fal", "fal-ai/veo3.1/fast", json!(5)))
            .unwrap_err()
            .allowed,
        ["4", "6", "8"]
    );
    assert!(o.requests().unwrap().is_empty(), "nothing journaled");
    assert_eq!(generate_calls(&e), 0);

    o.submit(&vspec("fal", "fal-ai/veo3.1/fast", json!(6)), None)
        .unwrap();
    o.submit(&vspec("fake", "fake-one", json!(4)), None)
        .unwrap();
    assert_eq!(generate_calls(&e), 2);
}

#[test]
fn with_the_capability_kling_ranges_and_wan_sets_are_enforced() {
    let e = env();
    let o = orch(&e, DUR);
    for (p, m, bad) in [
        ("wavespeed", "kling-3-pro", json!(2)),
        ("wavespeed", "kling-3-pro", json!(16)),
        ("wavespeed", "wan-ultra", json!(6)),
        ("wavespeed", "wan-ultra", json!("abc")),
        ("wavespeed", "infinitetalk", json!(601)),
    ] {
        let err = o.submit(&vspec(p, m, bad.clone()), None).unwrap_err();
        assert!(
            matches!(err, Error::InvalidParams { ref field, .. } if field == "params.duration"),
            "{m} {bad}: {err:?}"
        );
    }
    assert!(o.requests().unwrap().is_empty());
    for (p, m, ok) in [
        ("wavespeed", "kling-3-pro", json!(3)),
        ("wavespeed", "kling-3-pro", json!(15)),
        ("wavespeed", "wan-ultra", json!(8)),
        ("wavespeed", "infinitetalk", json!(600)),
    ] {
        o.submit(&vspec(p, m, ok), None).unwrap();
    }
}

#[test]
fn with_the_capability_an_omitted_duration_gets_the_model_default() {
    let e = env();
    let o = orch(&e, DUR);
    let mut s = vspec("wavespeed", "kling-3-pro", json!(null));
    o.submit(&s, Some("req-default")).unwrap();
    s.params = json!({"aspect": "16:9"});
    o.submit(&s, Some("req-default-2")).unwrap();
    let rows = o.requests().unwrap();
    let get = |id: &str| {
        rows.iter()
            .find(|r| r.request_id == id)
            .unwrap()
            .params
            .clone()
    };
    assert_eq!(get("req-default")["duration"], 5);
    assert_eq!(
        get("req-default-2"),
        json!({"aspect": "16:9", "duration": 5})
    );
    // Replaying the same spec under the same id stays idempotent.
    o.submit(&s, Some("req-default-2")).unwrap();
    assert_eq!(generate_calls(&e), 2);
    // No default (InfiniteTalk) and non-video specs are left alone.
    o.submit(
        &vspec("wavespeed", "infinitetalk", json!(null)),
        Some("req-it"),
    )
    .unwrap();
    let rows = o.requests().unwrap();
    let it = rows.iter().find(|r| r.request_id == "req-it").unwrap();
    assert!(it.params["duration"].is_null());
}

#[test]
fn without_the_capability_durations_behave_as_before() {
    let e = env();
    let o = orch(&e, &[]);
    assert_eq!(
        o.ensure_ready().unwrap().capability("models.durations"),
        None
    );
    assert!(!o.models().unwrap().structured_durations);
    // Label parsing: Veo's "4 s" is a default only, passed through.
    o.submit(&vspec("fal", "fal-ai/veo3.1/fast", json!(5)), None)
        .unwrap();
    o.submit(&vspec("fal", "fal-ai/veo3.1/fast", json!(6)), None)
        .unwrap();
    assert!(o
        .submit(&vspec("wavespeed", "wan-ultra", json!(6)), None)
        .is_err());
    // No default is injected.
    o.submit(
        &vspec("wavespeed", "kling-3-pro", json!(null)),
        Some("req-nodef"),
    )
    .unwrap();
    let rows = o.requests().unwrap();
    let r = rows.iter().find(|r| r.request_id == "req-nodef").unwrap();
    assert!(r.params["duration"].is_null());
    assert_eq!(
        o.models()
            .unwrap()
            .get("fal", "fal-ai/veo3.1/fast")
            .unwrap()
            .default_duration_s,
        None
    );
}

#[test]
fn a_known_model_must_match_the_requested_kind() {
    let e = env();
    let o = orch(&e, &[]);
    let mut s = spec("x");
    s.provider = "wavespeed".into();
    s.model = "wan-ultra".into(); // a video model asked for an image
    let err = o.submit(&s, None).unwrap_err();
    assert!(
        matches!(err, Error::InvalidParams { ref field, .. } if field == "kind"),
        "{err:?}"
    );
    assert_eq!(o.check_spec(&s).unwrap_err().allowed, ["video"]);
    assert!(o.requests().unwrap().is_empty());
}

#[test]
fn structured_duration_fields_are_honored_when_an_engine_adds_them() {
    let client = GenRightClient::new(ScriptRunner(Box::new(|_| {
        json!([
            {"provider": "p", "model": "a", "kind": "video", "label": "A (4 s)", "est_usd": 1.0, "durations": [8, 6]},
            {"provider": "p", "model": "b", "kind": "video", "label": "B", "est_usd": 1.0, "min_duration_s": 2, "max_duration_s": 4},
        ])
        .to_string()
    })));
    let cat = client.models().unwrap();
    assert_eq!(
        cat.get("p", "a").unwrap().durations,
        DurationRule::Set { secs: vec![6, 8] }
    );
    assert_eq!(
        cat.get("p", "b").unwrap().durations,
        DurationRule::Range { min: 2, max: 4 }
    );
    assert!(cat.validate(&vspec("p", "a", json!(4))).is_err());
    assert!(cat.validate(&vspec("p", "b", json!(3))).is_ok());
}

/// Run `words` on the fake with an open, never-written stdin pipe and report
/// whether it returned within `wait`.
fn returns_with_open_stdin(extra: &[(&str, &str)], words: &[&str], wait: Duration) -> bool {
    let e = env();
    let mut cmd = std::process::Command::new(engine());
    cmd.args(words)
        .env("FAKE_GENRIGHT_DIR", e.fake.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let _held_open = child.stdin.take(); // never written, never closed
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        if start.elapsed() >= wait {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn engine_stdin_is_read_only_with_input_dash_and_calls_never_depend_on_it() {
    // The fake models the contract: an open unwritten pipe does not block.
    let inp = r#"{"limit":1}"#;
    assert!(returns_with_open_stdin(
        &[],
        &["doctor"],
        Duration::from_secs(20)
    ));
    assert!(returns_with_open_stdin(
        &[],
        &["job", "list", "--input", inp],
        Duration::from_secs(20)
    ));
    // ...and it has teeth: the pre-fix engine hangs on the same pipe.
    assert!(!returns_with_open_stdin(
        &[("FAKE_LEGACY_STDIN", "1")],
        &["doctor"],
        Duration::from_millis(700)
    ));

    // Every runner call works even against the legacy (stdin-reading) fake,
    // because the runner never leaves a stdin pipe open.
    let e = env();
    let o = orch(&e, &[("FAKE_LEGACY_STDIN", "1")]);
    let id = submit_and_finish(&o);
    assert_eq!(o.import(&id).unwrap().len(), 1);

    // Payloads above the argument limit travel via `--input -` + stdin.
    let e = env();
    let o = orch(&e, &[]);
    let big = "x".repeat(MAX_INPUT_ARG_BYTES * 3);
    let sub = o.submit(&spec(&big), None).unwrap();
    assert_eq!(
        state(&e)["jobs"][0]["prompt"].as_str().unwrap().len(),
        big.len(),
        "{sub:?}"
    );
}

/// Read-only live check against an installed GenRight (`doctor`, `job list`;
/// never `generate`). Run with `GENRIGHT_ENGINE=<path> cargo test -- --ignored`.
#[test]
#[ignore = "needs an installed GenRight (GENRIGHT_ENGINE or the macOS default path)"]
fn live_installed_engine_is_ready_and_lists_jobs() {
    let path = std::env::var("GENRIGHT_ENGINE").unwrap_or_else(|_| {
        "/Applications/GenRight.app/Contents/MacOS/genright-engine".to_string()
    });
    let project = TempDir::new().unwrap();
    let o = Orchestrator::for_engine(project.path(), path);
    match o.ensure_ready() {
        Ok(info) => eprintln!(
            "live: ready, app {} contract {}",
            info.app_version, info.contract_version
        ),
        Err(e) => eprintln!("live: NOT ready: {e}"),
    }
    let page = o.client().job_page(&JobListQuery::default()).unwrap();
    eprintln!(
        "live: {} jobs, truncated {}",
        page.jobs.len(),
        page.truncated
    );
}
