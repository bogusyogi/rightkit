//! GenRight adoption: extension columns over GenRight's real on-disk schema,
//! its `submitted`/`running` vocabulary, provider-id crash safety, the per-attempt
//! spend reservation hook, the `assets(asset_id)` table, and GenRight's own wire.

use std::collections::HashMap;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_jobs::rusqlite::{params, Connection, OptionalExtension};
use rightkit_jobs::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Verbatim copy of `genright/crates/genright-gen/src/store.rs` SCHEMA (pre-swap).
const GENRIGHT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS jobs (
  job_id TEXT PRIMARY KEY,
  request_id TEXT UNIQUE,
  kind TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL,
  provider_job_id TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  est_usd REAL NOT NULL DEFAULT 0,
  actual_usd REAL,
  error_json TEXT,
  result_json TEXT,
  acked_by TEXT,
  acked_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status);
CREATE TABLE IF NOT EXISTS ledger (
  day TEXT NOT NULL,
  job_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  reserved_usd REAL NOT NULL,
  actual_usd REAL,
  at TEXT NOT NULL,
  PRIMARY KEY (job_id, attempt)
);
CREATE INDEX IF NOT EXISTS ledger_day ON ledger(day);
CREATE TABLE IF NOT EXISTS job_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  at TEXT NOT NULL,
  status TEXT NOT NULL,
  stage TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  message TEXT
);
CREATE INDEX IF NOT EXISTS job_events_job ON job_events(job_id, seq);
CREATE TABLE IF NOT EXISTS assets (
  asset_id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  mime TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS assets_job ON assets(job_id);
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

/// The app-owned part GenRight keeps (ledger + settings), declared as app schema.
const APP_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ledger (
  day TEXT NOT NULL, job_id TEXT NOT NULL, attempt INTEGER NOT NULL,
  reserved_usd REAL NOT NULL, actual_usd REAL, at TEXT NOT NULL,
  PRIMARY KEY (job_id, attempt));
CREATE INDEX IF NOT EXISTS ledger_day ON ledger(day);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

fn today() -> String {
    time::OffsetDateTime::now_utc().date().to_string()
}

/// GenRight's `ledger::reserve`, as a hook: atomic with the attempt count.
struct SpendHook;
impl StoreHook for SpendHook {
    fn on_attempt(&self, conn: &Connection, job: &JobRecord, attempt: i64) -> JobResult<()> {
        let cap: f64 = conn
            .query_row(
                "SELECT value FROM settings WHERE key='spend_cap_usd'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(5.0);
        let day = today();
        let (r, a): (f64, f64) = conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN actual_usd IS NULL THEN reserved_usd ELSE 0 END),0),
                    COALESCE(SUM(actual_usd),0) FROM ledger WHERE day=?1",
            [&day],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let est = job.ext_f64("est_usd").unwrap_or(0.0);
        if r + a + est > cap + 1e-9 {
            return Err(JobError::new(
                "spend_cap",
                format!("daily cap ${cap:.2} would be exceeded"),
            ));
        }
        conn.execute(
            "INSERT INTO ledger(day, job_id, attempt, reserved_usd, at) VALUES (?1,?2,?3,?4,?5)",
            params![day, job.job.job_id, attempt, est, "t"],
        )?;
        Ok(())
    }
    fn on_create(&self, _conn: &Connection, job: &JobRecord) -> JobResult<()> {
        if job.ext_str("provider") == Some("forbidden") {
            return Err(JobError::bad_input("provider not allowed"));
        }
        Ok(())
    }
}

fn genright_config() -> StoreConfig {
    StoreConfig::new()
        .column(ExtColumn::text("provider").not_null())
        .column(ExtColumn::text("model").not_null())
        .column(ExtColumn::text("provider_job_id"))
        .column(ExtColumn::real("est_usd").not_null().default(0))
        .column(ExtColumn::real("actual_usd"))
        .statuses(
            StatusModel::new("submitted", ["submitted", "running"])
                .allow("submitted", "running")
                .allow("running", "submitted"),
        )
        .artifacts(ArtifactNames::new("assets", "asset_id", "asset_id"))
        .external_id("provider_job_id")
        .app_schema(APP_SCHEMA)
        .user_version(None)
        .hook(Arc::new(SpendHook))
}

fn sha(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A GenRight database as the pre-swap engine leaves it.
fn genright_fixture(dir: &Path) -> PathBuf {
    let db = dir.join("genright.db");
    let asset = dir.join("a-1.png");
    std::fs::write(&asset, b"png-bytes").unwrap();
    let c = Connection::open(&db).unwrap();
    c.execute_batch(GENRIGHT_SCHEMA).unwrap();
    let desc = json!({"job_id":"j-done","assets":[{"asset_id":"a-1","path":asset.to_string_lossy(),"sha256":sha(&asset),"bytes":9,"mime":"image/png"}]});
    let ins = "INSERT INTO jobs (job_id, request_id, kind, provider, model, request_json, status, provider_job_id, attempts, est_usd, actual_usd, error_json, result_json, acked_by, acked_at, created_at, updated_at) VALUES (?1,?2,'image','fal','flux',?3,?4,?5,?6,?7,?8,NULL,?9,NULL,NULL,?10,?10)";
    c.execute(
        ins,
        params![
            "j-done",
            "rid-done",
            "{\"prompt\":\"a\"}",
            "complete",
            "p-0",
            1,
            0.5,
            0.5,
            desc.to_string(),
            "2026-01-01T00:00:01Z"
        ],
    )
    .unwrap();
    c.execute(
        ins,
        params![
            "j-poll",
            None::<String>,
            "{\"prompt\":\"b\"}",
            "running",
            "p-9",
            1,
            0.25,
            None::<f64>,
            None::<String>,
            "2026-01-01T00:00:02Z"
        ],
    )
    .unwrap();
    c.execute(
        ins,
        params![
            "j-new",
            "rid-new",
            "{\"prompt\":\"c\"}",
            "submitted",
            None::<String>,
            0,
            1.0,
            None::<f64>,
            None::<String>,
            "2026-01-01T00:00:03Z"
        ],
    )
    .unwrap();
    let day = today();
    c.execute(
        "INSERT INTO ledger VALUES (?1,'j-done',1,0.5,0.5,'t')",
        [&day],
    )
    .unwrap();
    c.execute(
        "INSERT INTO ledger VALUES (?1,'j-poll',1,0.25,NULL,'t')",
        [&day],
    )
    .unwrap();
    for stage in ["submitted", "complete"] {
        c.execute("INSERT INTO job_events (job_id, at, status, stage, attempt, message) VALUES ('j-done','t',?1,?1,1,'m')", [stage]).unwrap();
    }
    c.execute(
        "INSERT INTO assets VALUES ('a-1','j-done',?1,?2,9,'image/png','t')",
        params![asset.to_string_lossy(), sha(&asset)],
    )
    .unwrap();
    c.execute("INSERT INTO settings VALUES ('spend_cap_usd','2.0')", [])
        .unwrap();
    db
}

fn dump(db: &Path, sql: &str) -> Vec<Vec<String>> {
    let c = Connection::open(db).unwrap();
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    st.query_map([], |r| {
        (0..n)
            .map(|i| {
                r.get::<_, rightkit_jobs::rusqlite::types::Value>(i)
                    .map(|v| format!("{v:?}"))
            })
            .collect::<Result<Vec<_>, _>>()
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

const ORIGINAL_JOB_COLS: &str = "SELECT job_id, request_id, kind, provider, model, request_json, status, provider_job_id, attempts, est_usd, actual_usd, error_json, result_json, acked_by, acked_at, created_at, updated_at FROM jobs ORDER BY job_id";

fn has_table(db: &Path, t: &str) -> bool {
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [t],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
}

fn ledger_rows(store: &JobStore, job_id: &str) -> i64 {
    store
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM ledger WHERE job_id=?1",
                [job_id],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

fn gen_job(rid: Option<&str>) -> NewJob {
    NewJob {
        request_id: rid.map(Into::into),
        kind: "image".into(),
        request: json!({"prompt": "x"}),
    }
}

fn gen_ext(est: f64) -> Ext {
    json!({"provider": "fal", "model": "flux", "est_usd": est})
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn adopts_an_existing_genright_database_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let db = genright_fixture(dir.path());
    let jobs_before = dump(&db, ORIGINAL_JOB_COLS);
    let ledger_before = dump(&db, "SELECT * FROM ledger ORDER BY job_id, attempt");
    let settings_before = dump(&db, "SELECT * FROM settings");
    let assets_before = dump(&db, "SELECT * FROM assets");
    let events_before = dump(
        &db,
        "SELECT seq, job_id, at, status, stage, attempt, message FROM job_events ORDER BY seq",
    );

    // 0.1's failure mode is now explicit and touches nothing.
    let e = JobStore::open(&db)
        .err()
        .expect("default config must refuse GenRight's schema");
    assert_eq!(e.code, "schema");
    assert!(
        e.message.contains("provider") && e.message.contains("model"),
        "{}",
        e.message
    );
    assert!(!has_table(&db, "artifacts"));

    let store = JobStore::open_configured(&db, genright_config()).unwrap();
    assert!(store.adoption().existing_jobs_table);
    assert_eq!(
        store.adoption().added_columns,
        vec![
            "job_events.progress".to_string(),
            "jobs.attempt_gen".to_string()
        ]
    );
    assert!(
        !has_table(&db, "artifacts"),
        "uses assets, creates no default artifact table"
    );

    assert_eq!(
        dump(&db, ORIGINAL_JOB_COLS),
        jobs_before,
        "jobs rows unchanged"
    );
    assert_eq!(
        dump(&db, "SELECT * FROM ledger ORDER BY job_id, attempt"),
        ledger_before
    );
    assert_eq!(dump(&db, "SELECT * FROM settings"), settings_before);
    assert_eq!(dump(&db, "SELECT * FROM assets"), assets_before);
    assert_eq!(
        dump(
            &db,
            "SELECT seq, job_id, at, status, stage, attempt, message FROM job_events ORDER BY seq"
        ),
        events_before
    );

    // Existing rows map onto records with GenRight's wire fields.
    let done = store.record("j-done").unwrap();
    let v = done.to_value();
    assert_eq!(
        (
            v["provider"].as_str(),
            v["model"].as_str(),
            v["provider_job_id"].as_str()
        ),
        (Some("fal"), Some("flux"), Some("p-0"))
    );
    assert_eq!(
        (v["est_usd"].as_f64(), v["actual_usd"].as_f64()),
        (Some(0.5), Some(0.5))
    );
    assert_eq!(v["status"], "complete");
    assert_eq!(
        done.job.result.as_ref().unwrap()["assets"][0]["asset_id"],
        "a-1"
    );
    assert_eq!(
        store.record_by_request("rid-new").unwrap().ext["est_usd"],
        json!(1.0)
    );
    assert_eq!(store.non_terminal_ids().unwrap(), vec!["j-poll", "j-new"]);
    let evs = store.events_after("j-done", 0).unwrap();
    assert_eq!(evs.len(), 2);
    assert!(evs.iter().all(|e| e.progress.is_none()));
    let a = store.get_artifact("a-1").unwrap();
    assert!(store.verify_artifact(&a).unwrap());
    assert_eq!(store.artifact_value(&a)["asset_id"], "a-1");
    assert!(store.artifact_value(&a).get("artifact_id").is_none());

    // Legacy in-flight jobs: one with a provider id resumes, one without submits.
    assert_eq!(
        store.begin_submission("j-poll", 3).unwrap(),
        Some(Submission::Resume {
            external_id: "p-9".into(),
            attempt: 1
        })
    );
    assert!(matches!(
        store.begin_submission("j-new", 3).unwrap(),
        Some(Submission::Submit {
            attempt: 1,
            replay: false,
            ..
        })
    ));
    assert_eq!(ledger_rows(&store, "j-new"), 1);

    // New jobs work against the adopted schema; NOT NULL ext is enforced up front.
    assert_eq!(store.submit(&gen_job(None)).unwrap_err().code, "bad_input");
    let (r, dedup) = store
        .submit_ext(&gen_job(Some("rid-x")), &gen_ext(0.1))
        .unwrap();
    assert!(!dedup && r.job.status == "submitted" && r.ext["provider"] == "fal");
    assert!(
        store
            .submit_ext(&gen_job(Some("rid-x")), &gen_ext(0.1))
            .unwrap()
            .1
    );
    drop(store);
    // Reopen is idempotent: nothing more to add.
    let again = JobStore::open_configured(&db, genright_config()).unwrap();
    assert!(again.adoption().added_columns.is_empty());
    assert_eq!(again.list(&ListFilter::default()).unwrap().0.len(), 4);
}

#[test]
fn status_transitions_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(dir.path().join("j.db"), genright_config()).unwrap();
    let (r, _) = store.submit_ext(&gen_job(None), &gen_ext(0.0)).unwrap();
    let id = r.job.job_id;
    assert_eq!(r.job.status, "submitted");
    let e = store
        .transition(&id, &Transition::to("queued"))
        .unwrap_err();
    assert_eq!(e.code, "illegal_transition", "undeclared status");
    let up = store
        .transition(&id, &Transition::to("running"))
        .unwrap()
        .unwrap();
    assert_eq!(up.job.status, "running");
    let back = store
        .transition(&id, &Transition::to("submitted"))
        .unwrap()
        .unwrap();
    assert_eq!(back.job.status, "submitted");
    assert!(store.complete(&id, &json!({"ok": true})).unwrap());
    assert!(store
        .transition(&id, &Transition::to("running"))
        .unwrap()
        .is_none());
    assert_eq!(
        store.get(&id).unwrap().status,
        "complete",
        "terminal never moves"
    );

    // Default vocabulary: running may not go back to queued.
    let plain = JobStore::open(dir.path().join("p.db")).unwrap();
    let (j, _) = plain.submit(&gen_job(None)).unwrap();
    assert_eq!(j.status, QUEUED);
    plain.begin_attempt(&j.job_id).unwrap();
    assert_eq!(
        plain
            .transition(&j.job_id, &Transition::to(QUEUED))
            .unwrap_err()
            .code,
        "illegal_transition"
    );

    // A model that names an undeclared status is refused at open.
    let bad = StoreConfig::new().statuses(StatusModel::new("pending", ["queued"]));
    assert_eq!(
        JobStore::open_configured(dir.path().join("b.db"), bad)
            .err()
            .unwrap()
            .code,
        "config"
    );
}

#[test]
fn spend_hook_rolls_back_with_the_attempt_and_the_insert() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open_configured(dir.path().join("j.db"), genright_config()).unwrap();
    store
        .write(|c| Ok(c.execute("INSERT INTO settings VALUES ('spend_cap_usd','1.0')", [])?))
        .unwrap();
    let (a, _) = store.submit_ext(&gen_job(None), &gen_ext(0.75)).unwrap();
    let (b, _) = store.submit_ext(&gen_job(None), &gen_ext(0.5)).unwrap();
    assert!(matches!(
        store.begin_submission(&a.job.job_id, 3).unwrap(),
        Some(Submission::Submit { attempt: 1, .. })
    ));
    // 0.75 + 0.5 > 1.0: the reservation fails, so the attempt is not counted either.
    let e = store.begin_submission(&b.job.job_id, 3).unwrap_err();
    assert_eq!(e.code, "spend_cap");
    let b_now = store.record(&b.job.job_id).unwrap();
    assert_eq!(
        b_now.job.attempts, 0,
        "attempt rolled back with the reservation"
    );
    assert_eq!(ledger_rows(&store, &b.job.job_id), 0);
    assert!(store.submissions(&b.job.job_id).unwrap().is_empty());
    assert_eq!(b_now.job.status, "submitted");

    // A failing create hook rolls back the job row itself.
    let mut ext = gen_ext(0.0);
    ext.insert("provider".into(), json!("forbidden"));
    assert_eq!(
        store
            .submit_ext(&gen_job(Some("nope")), &ext)
            .unwrap_err()
            .code,
        "bad_input"
    );
    assert_eq!(
        store.get_by_request("nope").unwrap_err().code,
        "job_not_found"
    );

    // Through the Runner (per-run attempts): a refused attempt fails the job, unspent.
    struct Never;
    impl JobHandler for Never {
        fn run(&self, _: &Job, _: &JobControl) -> JobResult<Value> {
            panic!("must not run when the hook refuses the attempt")
        }
    }
    let store = Arc::new(store);
    let runner = Runner::new(
        store.clone(),
        JobLocks::new(dir.path().join("locks")),
        Arc::new(Never),
    );
    match runner.run(&b.job.job_id).unwrap() {
        RunOutcome::Finished(j) => {
            assert_eq!(j.status, FAILED);
            assert_eq!(j.error.unwrap()["code"], "spend_cap");
            assert_eq!(j.attempts, 0);
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(ledger_rows(&store, &b.job.job_id), 0);
}

/// Provider double: idempotent on the submission key, scripted poll results.
#[derive(Default)]
struct FakeProvider {
    by_key: Mutex<HashMap<String, String>>,
    polls: Mutex<HashMap<String, Vec<&'static str>>>,
    script: Mutex<Vec<Vec<&'static str>>>,
    submits: AtomicUsize,
}
impl FakeProvider {
    fn scripted(script: Vec<Vec<&'static str>>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            ..Self::default()
        })
    }
    /// Charges once per distinct key; a replayed key returns the same id.
    fn submit(&self, key: &str) -> String {
        let mut m = self.by_key.lock().unwrap();
        if let Some(id) = m.get(key) {
            return id.clone();
        }
        self.submits.fetch_add(1, Ordering::SeqCst);
        let id = format!("p-{}", m.len() + 1);
        let mut s = self.script.lock().unwrap();
        let polls = if s.is_empty() {
            vec!["done"]
        } else {
            s.remove(0)
        };
        self.polls.lock().unwrap().insert(id.clone(), polls);
        m.insert(key.into(), id.clone());
        id
    }
    fn poll(&self, id: &str) -> &'static str {
        let mut p = self.polls.lock().unwrap();
        let q = p.get_mut(id).expect("polled an unknown provider id");
        if q.len() > 1 {
            q.remove(0)
        } else {
            q[0]
        }
    }
}

/// GenRight's runner loop over the generic store.
struct GenHandler {
    provider: Arc<FakeProvider>,
}
impl JobHandler for GenHandler {
    fn run(&self, _job: &Job, ctl: &JobControl) -> JobResult<Value> {
        loop {
            match ctl.begin_submission()? {
                None => return Err(JobError::cancelled()),
                Some(Submission::Exhausted { .. }) => {
                    return Err(JobError::new(
                        "max_attempts",
                        "provider failed on every allowed attempt",
                    ))
                }
                Some(Submission::Submit {
                    idempotency_key, ..
                }) => {
                    let pid = self.provider.submit(&idempotency_key);
                    ctl.accept_submission(&pid, Some("running"))?;
                }
                Some(Submission::Resume { external_id, .. }) => {
                    match self.provider.poll(&external_id) {
                        "running" => {
                            return Err(JobError::retryable("offline", "provider still running"))
                        }
                        "failed" => {
                            ctl.fail_submission(
                                &JobError::new("provider_failed", "confirmed failure"),
                                Some("submitted"),
                            )?;
                        }
                        _ => return Ok(json!({"provider_job_id": external_id})),
                    }
                }
            }
        }
    }
}

fn gen_runner(db: &Path, locks: &Path, p: &Arc<FakeProvider>) -> Runner {
    let store = Arc::new(JobStore::open_configured(db, genright_config()).unwrap());
    Runner::new(
        store,
        JobLocks::new(locks),
        Arc::new(GenHandler {
            provider: p.clone(),
        }),
    )
    .with_attempt_policy(AttemptPolicy::ByHandler)
    .with_max_attempts(3)
}

#[test]
fn crash_and_restart_never_double_submits() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("j.db");
    let provider = FakeProvider::scripted(vec![]);

    // Process 1 counts attempt 1 (reserving spend) and dies before the provider id is stored.
    let store = JobStore::open_configured(&db, genright_config()).unwrap();
    let (r, _) = store.submit_ext(&gen_job(None), &gen_ext(0.5)).unwrap();
    let id = r.job.job_id.clone();
    let Some(Submission::Submit {
        attempt: 1,
        idempotency_key: k1,
        replay: false,
    }) = store.begin_submission(&id, 3).unwrap()
    else {
        panic!("first submission")
    };
    let _accepted_by_provider = provider.submit(&k1); // reached the provider, then crash
    drop(store);

    // Process 2: same attempt and key, no second reservation; the provider dedups.
    let store = JobStore::open_configured(&db, genright_config()).unwrap();
    let Some(Submission::Submit {
        attempt: 1,
        idempotency_key: k1b,
        replay: true,
    }) = store.begin_submission(&id, 3).unwrap()
    else {
        panic!("replay")
    };
    assert_eq!(k1, k1b);
    let pid = provider.submit(&k1b);
    store.accept_submission(&id, &pid, Some("running")).unwrap();
    drop(store);

    // Process 3: stored provider id means poll, never resubmit.
    let store = JobStore::open_configured(&db, genright_config()).unwrap();
    assert_eq!(
        store.begin_submission(&id, 3).unwrap(),
        Some(Submission::Resume {
            external_id: pid.clone(),
            attempt: 1
        })
    );
    let rec = store.record(&id).unwrap();
    assert_eq!((rec.job.status.as_str(), rec.job.attempts), ("running", 1));
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert_eq!(ledger_rows(&store, &id), 1);
    drop(store);

    // Runner, GenRight loop: confirmed failure resubmits; a pause then restart resumes by id.
    let p = FakeProvider::scripted(vec![vec!["failed"], vec!["running", "done"]]);
    let runner = gen_runner(&db, &dir.path().join("locks"), &p);
    let (j, _) = runner
        .store()
        .submit_ext(&gen_job(None), &gen_ext(0.25))
        .unwrap();
    let out = runner.run(&j.job.job_id).unwrap();
    assert!(matches!(out, RunOutcome::Paused(_)), "{out:?}");
    drop(runner);
    let runner = gen_runner(&db, &dir.path().join("locks"), &p);
    assert!(runner.recoverable_ids().unwrap().contains(&j.job.job_id));
    let RunOutcome::Finished(done) = runner.run(&j.job.job_id).unwrap() else {
        panic!()
    };
    assert_eq!((done.status.as_str(), done.attempts), (COMPLETE, 2));
    assert_eq!(
        p.submits.load(Ordering::SeqCst),
        2,
        "one per attempt, none on resume"
    );
    let subs: Vec<String> = runner
        .store()
        .submissions(&j.job.job_id)
        .unwrap()
        .into_iter()
        .map(|s| s.state)
        .collect();
    assert_eq!(subs, ["failed", "accepted"]);
    assert_eq!(ledger_rows(runner.store(), &j.job.job_id), 2);

    // Bounded resubmission: three confirmed failures then max_attempts.
    let p = FakeProvider::scripted(vec![
        vec!["failed"],
        vec!["failed"],
        vec!["failed"],
        vec!["failed"],
    ]);
    let runner = gen_runner(&db, &dir.path().join("locks"), &p);
    let (j, _) = runner
        .store()
        .submit_ext(&gen_job(None), &gen_ext(0.0))
        .unwrap();
    let RunOutcome::Finished(f) = runner.run(&j.job.job_id).unwrap() else {
        panic!()
    };
    assert_eq!((f.status.as_str(), f.attempts), (FAILED, 3));
    assert_eq!(f.error.unwrap()["code"], "max_attempts");
    assert_eq!(p.submits.load(Ordering::SeqCst), 3);
}

#[test]
fn custom_assets_table_and_wire_names() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("j.db");
    let store = JobStore::open_configured(&db, genright_config()).unwrap();
    assert!(has_table(&db, "assets") && !has_table(&db, "artifacts"));
    let (r, _) = store.submit_ext(&gen_job(None), &gen_ext(0.0)).unwrap();
    let f = dir.path().join("x.png");
    std::fs::write(&f, b"abc").unwrap();
    let a = store
        .add_artifact_with_id(&r.job.job_id, "sha-abc", &f, "image/png")
        .unwrap();
    // Idempotent by id (GenRight records descriptors with INSERT OR REPLACE).
    store
        .add_artifact_with_id(&r.job.job_id, "sha-abc", &f, "image/png")
        .unwrap();
    assert_eq!(store.artifacts(&r.job.job_id).unwrap(), vec![a.clone()]);
    let n: i64 = store
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM assets WHERE asset_id='sha-abc'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(n, 1);
    let v = store.artifact_value(&a);
    assert_eq!(v["asset_id"], "sha-abc");
    assert!(v.get("artifact_id").is_none());

    // Default names are unchanged.
    let plain = JobStore::open(dir.path().join("p.db")).unwrap();
    let (j, _) = plain.submit(&gen_job(None)).unwrap();
    let pa = plain.add_artifact(&j.job_id, &f, "image/png").unwrap();
    assert!(plain.artifact_value(&pa)["artifact_id"].is_string());
    assert!(has_table(&dir.path().join("p.db"), "artifacts"));
}

struct GenWire;
impl PayloadMapper for GenWire {
    fn input(&self, method: &str, mut p: Value) -> JobResult<Value> {
        if method == "job.wait" {
            if let Some(t) = p.get("timeout_s").cloned() {
                p["wait_timeout_s"] = t;
            }
        }
        if method == "job.submit" {
            // GenRight's generate input carries provider/model at the top level.
            let ext = json!({"provider": p["provider"], "model": p["model"], "est_usd": p["est_usd"].as_f64().unwrap_or(0.0)});
            p["ext"] = ext;
        }
        Ok(p)
    }
    fn output(&self, _method: &str, mut v: Value) -> JobResult<Value> {
        if let Some(o) = v.as_object_mut() {
            o.remove("deduplicated");
        }
        Ok(v)
    }
    fn event(&self, mut e: Value) -> Value {
        if let Some(o) = e.as_object_mut() {
            o.remove("seq");
        }
        e
    }
    fn error(&self, _method: &str, mut e: JobError) -> JobError {
        if e.code == "artifact_not_found" {
            e.code = "asset_not_found".into();
        }
        e
    }
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

#[test]
fn custom_stdio_contract_keeps_the_app_wire() {
    let dir = tempfile::tempdir().unwrap();
    let p = FakeProvider::scripted(vec![]);
    let runner = Arc::new(gen_runner(
        &dir.path().join("j.db"),
        &dir.path().join("locks"),
        &p,
    ));
    let cmds = JobCommands::new(runner.clone())
        .with_name("artifact.resolve", "asset.resolve")
        .without("job.recover")
        .without("job.recoverable")
        .without("job.events")
        .with_mapper(Arc::new(GenWire));
    let d = Arc::new(
        Dispatcher::new("genright-engine", "9.9.9")
            .without_builtin("ping")
            .with_doctor(Arc::new(
                |dry, methods| json!({"contract": "genright", "dry_run": dry, "commands": methods}),
            ))
            .with_cli_target("asset", "asset_id")
            .with(Arc::new(cmds)),
    );
    let methods = d.methods();
    assert!(methods.contains(&"asset.resolve".to_string()));
    for gone in [
        "artifact.resolve",
        "job.recover",
        "job.recoverable",
        "job.events",
        "ping",
    ] {
        assert!(!methods.contains(&gone.to_string()), "{gone}");
    }

    let input = [
        r#"{"type":"request","id":1,"method":"doctor"}"#,
        r#"{"type":"request","id":2,"method":"ping"}"#,
        r#"{"type":"request","id":3,"method":"job.submit","params":{"kind":"image","request_id":"g1","provider":"fal","model":"flux","est_usd":0.1,"request":{"prompt":"x"},"run":true}}"#,
        r#"{"type":"request","id":5,"method":"asset.resolve","params":{"asset_id":"missing"}}"#,
        r#"{"type":"request","id":6,"method":"job.recover"}"#,
        r#"{"type":"request","id":7,"method":"shutdown"}"#,
    ]
    .join("\n");
    let out = SharedBuf::default();
    serve(
        d.clone(),
        BufReader::new(Cursor::new(input)),
        out.clone(),
        false,
    )
    .unwrap();
    let frames: Vec<Value> = String::from_utf8(out.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let resp = |id: u64| {
        frames
            .iter()
            .find(|f| f["type"] == "response" && f["id"] == id)
            .unwrap_or_else(|| panic!("no response {id}"))
            .clone()
    };
    assert_eq!(resp(1)["result"]["contract"], "genright");
    assert_eq!(resp(2)["error"]["code"], "unknown_method");
    let sub = resp(3);
    assert_eq!(sub["result"]["status"], "complete", "{sub}");
    assert_eq!(sub["result"]["provider"], "fal");
    assert_eq!(sub["result"]["provider_job_id"], "p-1");
    assert!(sub["result"].get("deduplicated").is_none());
    let events: Vec<&Value> = frames
        .iter()
        .filter(|f| f["type"] == "event" && f["id"] == 3)
        .collect();
    assert!(!events.is_empty() && events.iter().all(|e| e["event"].get("seq").is_none()));
    assert_eq!(resp(5)["error"]["code"], "asset_not_found");
    assert_eq!(resp(6)["error"]["code"], "unknown_method");

    // `timeout_s` reaches the canonical `wait_timeout_s` (default would be 30s).
    let sink: Sink = Arc::new(|_| {});
    d.call(
        "job.submit",
        &json!({"kind": "image", "request_id": "g2", "provider": "fal", "model": "flux"}),
        false,
        &sink,
    )
    .unwrap();
    // serve joined the submit thread; wait follows the finished job.
    let wait = json!({"result": d.call("job.wait", &json!({"request_id": "g1", "timeout_s": 20}), false, &sink).unwrap()});
    assert_eq!(
        (
            wait["result"]["timed_out"].as_bool(),
            wait["result"]["model"].as_str()
        ),
        (Some(false), Some("flux"))
    );
    let t = std::time::Instant::now();
    let w = d
        .call(
            "job.wait",
            &json!({"request_id": "g2", "timeout_s": 0}),
            false,
            &sink,
        )
        .unwrap();
    assert!(w["timed_out"] == true && t.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(w["status"], "submitted");

    // CLI: `asset resolve <id>` fills asset_id; the result uses the asset wire id.
    let job_id = runner.store().get_by_request("g1").unwrap().job_id;
    let f = dir.path().join("a.png");
    std::fs::write(&f, b"img").unwrap();
    runner
        .store()
        .add_artifact_with_id(&job_id, "a-7", &f, "image/png")
        .unwrap();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let code = run_cli(
        &d,
        ["asset", "resolve", "a-7"].iter().map(|s| s.to_string()),
        &mut std::io::empty(),
        &mut o,
        &mut e,
    );
    let v: Value = serde_json::from_slice(&o).unwrap();
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        (v["asset_id"].as_str(), v["verified"].as_bool()),
        (Some("a-7"), Some(true))
    );
}
