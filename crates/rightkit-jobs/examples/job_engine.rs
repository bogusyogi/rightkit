//! Test engine: `job_engine <suite-root> <data-dir> <role> <rank> <stall-ms>`.
//! Hosts the `job.*` contract on service `jobs-demo` with demo job kinds.
use rightkit_jobs::*;
use rightkit_service::*;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

struct H {
    log: PathBuf,
    role: String,
}
impl H {
    fn log(&self, what: &str, job: &Job, n: i64) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log) {
            let _ = writeln!(f, "{t} {what} {} {n}", job.kind);
        }
    }
}
impl JobHandler for H {
    fn run(&self, job: &Job, ctl: &JobControl) -> JobResult<Value> {
        let req = &job.request;
        let ms = |k: &str, d: u64| Duration::from_millis(req[k].as_u64().unwrap_or(d));
        match job.kind.as_str() {
            "windows" => {
                let steps = req["steps"].as_i64().unwrap_or(5);
                for i in 0..steps {
                    ctl.step_boundary()?;
                    self.log("step_start", job, i);
                    std::thread::sleep(ms("step_ms", 100));
                    ctl.progress("step", Some((i + 1) as f64 / steps as f64), Some(&format!("window {i}")));
                    self.log("step_end", job, i);
                }
                Ok(json!({"steps": steps, "host": self.role}))
            }
            "dictation" => {
                self.log("dictation_start", job, 0);
                let end = std::time::Instant::now() + ms("ms", 500);
                while std::time::Instant::now() < end {
                    ctl.step_boundary()?;
                    std::thread::sleep(Duration::from_millis(25));
                }
                self.log("dictation_end", job, 0);
                Ok(json!({"host": self.role}))
            }
            "hang" => loop {
                std::thread::sleep(Duration::from_millis(50));
                if let Some(e) = ctl.abort_reason() {
                    return Err(e);
                }
            },
            "steady" => {
                for i in 0..15 {
                    std::thread::sleep(Duration::from_millis(200));
                    ctl.progress("tick", Some(i as f64 / 15.0), None);
                }
                Ok(json!({"host": self.role}))
            }
            other => Err(JobError::bad_input(format!("unknown kind {other}"))),
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let suite = Suite::at(&a[0]).expect("suite");
    let data = PathBuf::from(&a[1]);
    std::fs::create_dir_all(&data).unwrap();
    let store = Arc::new(JobStore::open(data.join("jobs.db")).unwrap());
    let gate = Gate::new();
    let handler = Arc::new(H { log: data.join("steps.log"), role: a[2].clone() });
    let stall: u64 = a[4].parse().unwrap_or(0);
    let mut runner = Runner::new(store, JobLocks::new(data.join("locks")), handler).with_gate(gate.clone());
    if stall > 0 {
        runner = runner.with_stall_timeout(Duration::from_millis(stall));
    }
    let d = Arc::new(Dispatcher::new("job-engine", "0.1").with(Arc::new(JobCommands::new(Arc::new(runner)))));
    let jobs = ["job.submit", "job.status", "job.result", "job.events", "job.cancel", "job.run", "job.wait"];
    let allow = Allowlist::new().allow("alpha", jobs).allow("beta", ["job.status"]);
    let mut cfg = ServiceConfig::new("jobs-demo", &a[2], a[3].parse().unwrap(), "test", allow);
    cfg.handoff_grace = Duration::from_secs(5);
    cfg.takeover_timeout = Duration::from_secs(20);
    match host(&suite, cfg, Arc::new(DispatcherHandler::new(d).with_gate(gate))) {
        Ok(h) => {
            println!("READY");
            h.wait_released(Duration::from_secs(300));
            println!("RELEASED");
        }
        Err(e) => {
            println!("ERR {e}");
            std::process::exit(4);
        }
    }
}
