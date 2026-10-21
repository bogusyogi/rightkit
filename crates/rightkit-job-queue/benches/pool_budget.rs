//! Runtime budget for rightkit-job-queue's staged store pool.
//!
//! Submit synthetic jobs to a scratch database, run them through `StagedPool`, wait for its
//! lane's idle callback, then measure the live idle pool before shutting it down. Pool threads
//! and database files are confined to isolated scratch storage: the harness-provisioned
//! `RIGHTKIT_RUNTIME_SCRATCH_ROOT` when set (CI runtime qualification), else the platform
//! scratch volume for manual runs on a host that has one.
//!
//! Run: `rightkit cargo test --profile release-iterate -p rightkit-job-queue --features store,supervisor --bench pool_budget -- 10000 30`

use rightkit_child_process::{observe_idle_with_threads, process_usage, BudgetLabels};
use rightkit_job_queue::{
    Job, JobControl, JobLocks, JobResult, JobStore, NewJob, PoolConfig, RunOutcome, Staged,
    StagedHandler, StagedPool, COMPLETE,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

struct Handler {
    computed: AtomicUsize,
    idled: AtomicUsize,
    idle_tx: mpsc::Sender<()>,
}

impl StagedHandler for Handler {
    type Prepared = String;
    type Lane = ();

    fn new_lane(&self) {}

    fn prepare(&self, job: &Job, _ctl: &JobControl) -> JobResult<Staged<String>> {
        Ok(Staged::Compute(job.job_id.clone()))
    }

    fn compute(
        &self,
        _lane: &mut (),
        _job: &Job,
        work: String,
        _ctl: &JobControl,
    ) -> JobResult<Value> {
        self.computed.fetch_add(1, Ordering::Relaxed);
        Ok(json!({"id": work}))
    }

    fn idle(&self, _lane: &mut ()) {
        self.idled.fetch_add(1, Ordering::Relaxed);
        let _ = self.idle_tx.send(());
    }
}

fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("RIGHTKIT_RUNTIME_SCRATCH_ROOT") {
        return PathBuf::from(root);
    }
    let local = if cfg!(target_os = "macos") {
        PathBuf::from("/Volumes/D/scratch")
    } else {
        PathBuf::from(r"D:\scratch")
    };
    assert!(
        local.parent().is_some_and(|parent| parent.exists()),
        "no scratch volume at {}: set RIGHTKIT_RUNTIME_SCRATCH_ROOT to isolated scratch storage",
        local.display()
    );
    local
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let jobs: usize = args.first().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let idle_s: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);
    assert!(
        (1..=100_000).contains(&jobs),
        "job count must be 1..=100000"
    );
    assert!(
        (3..=3600).contains(&idle_s),
        "idle window must be 3..=3600 seconds"
    );

    let scratch = scratch_root();
    std::fs::create_dir_all(&scratch).expect("create scratch root");
    let dir = tempfile::Builder::new()
        .prefix("rightkit-job-queue-pool-budget-")
        .tempdir_in(&scratch)
        .expect("create scratch state directory");
    let store = Arc::new(JobStore::open(dir.path().join("jobs.sqlite")).expect("open store"));
    let mut ids = Vec::with_capacity(jobs);
    for n in 0..jobs {
        let (job, _) = store
            .submit(&NewJob {
                request_id: None,
                kind: "budget".into(),
                request: json!({"n": n}),
            })
            .expect("submit synthetic job");
        ids.push(job.job_id);
    }

    let (idle_tx, idle_rx) = mpsc::channel();
    let handler = Arc::new(Handler {
        computed: AtomicUsize::new(0),
        idled: AtomicUsize::new(0),
        idle_tx,
    });
    let before = process_usage().expect("process usage before pool");
    let mut outcomes = 0usize;
    let (outcome_tx, outcome_rx) = mpsc::channel::<(String, JobResult<RunOutcome>)>();
    let outcome_tx = Arc::new(outcome_tx);
    let pool = StagedPool::start(
        store,
        JobLocks::new(dir.path().join("locks")),
        handler.clone(),
        PoolConfig::new(|| 4)
            .handoff(4)
            .idle_after(Duration::from_secs(3))
            .on_outcome({
                let outcome_tx = outcome_tx.clone();
                Arc::new(move |id, result| {
                    let _ = outcome_tx.send((id.to_string(), result));
                })
            }),
        |runner| runner,
    )
    .expect("start staged pool");

    let started = std::time::Instant::now();
    for id in &ids {
        pool.enqueue(id).expect("enqueue synthetic job");
    }
    for _ in 0..jobs {
        let (id, result) = outcome_rx
            .recv_timeout(Duration::from_secs(1800))
            .expect("pool job outcome timed out");
        match result.expect("pool run") {
            RunOutcome::Finished(job) if job.status == COMPLETE => {}
            other => panic!("{id}: unexpected pool outcome {other:?}"),
        }
        outcomes += 1;
    }
    let load_s = started.elapsed().as_secs_f64();
    idle_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("pool lane did not enter idle lifecycle");
    let warm = process_usage().expect("process usage after pool load");
    println!(
        "warm-up: {outcomes} staged jobs in {load_s:.2} s ({:.0} jobs/s), computed {}, idle callbacks {}, resident {:.1} MiB (before pool {:.1} MiB)",
        outcomes as f64 / load_s,
        handler.computed.load(Ordering::Relaxed),
        handler.idled.load(Ordering::Relaxed),
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );

    let idle = observe_idle_with_threads(Duration::from_secs(idle_s)).expect("idle window");
    println!("RESULT pool_jobs={} jobs", outcomes);
    println!(
        "RESULT lane_idle_callbacks={} callbacks",
        handler.idled.load(Ordering::Relaxed)
    );
    print!(
        "{}",
        idle.observation.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-job-queue",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "calls",
                scale: outcomes as u64,
                what: "staged pool jobs through scratch SQLite store, followed by live dispatcher/lane idle",
                scope: "synthetic",
            },
        )
    );
    print!("{}", idle.threads.result_lines(idle.observation.elapsed, 8));
    pool.shutdown();
    println!("pool shutdown: dispatcher, job workers, and lane joined");
}
