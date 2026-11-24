//! Registry consumer for rightkit-system-monitor 0.3.1: builds against the published API only.
use rightkit_system_monitor::{processes, procs, set_state_directory, system_status};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let status = system_status();
    println!("ok: system status read ({} disks)", status.disks.len());
    check(
        !procs().is_empty(),
        "the process table lists at least this process",
    );
    let groups = processes::group(&[]);
    let _ = groups;
    println!("ok: an empty process list groups");
    let dir = std::env::temp_dir().join(format!(
        "rightkit-system-monitor-consumer-{}",
        std::process::id()
    ));
    check(
        set_state_directory(dir.clone()),
        "the host app names the state folder once",
    );
    check(
        !set_state_directory(dir.clone()),
        "a second state folder is refused",
    );
    #[cfg(windows)]
    {
        let report = rightkit_system_monitor::drive_health_windows::report(&dir, &[], 0);
        check(
            report.drives.is_empty() && report.alerts.is_empty(),
            "the Windows drive-health report of no mounts is empty",
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    history_journey();
    println!("rightkit-system-monitor consumer: all checks passed");
}

fn history_journey() {
    use rightkit_system_monitor::system_history::{Sample, SystemHistory};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    // The real OS samplers run on a thread this consumer owns, as a host does. A fixed host
    // clock makes the exclusive cursor the only thing that advances the sample time.
    let history = Arc::new(SystemHistory::with_retention(Duration::from_secs(60)));
    let start = || {
        let history = Arc::clone(&history);
        thread::spawn(move || history.run(|| 10_000).expect("native system tables"))
    };
    let wait_sample = |since: Option<u64>| -> Sample {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(sample) = history.samples(since).into_iter().next() {
                return sample;
            }
            check(
                Instant::now() < deadline,
                "a sample arrives by the deadline",
            );
            thread::sleep(Duration::from_millis(25));
        }
    };
    let worker = start();
    let first = wait_sample(None);
    check(
        first.ts == 10_000,
        "the first sample carries the host clock",
    );
    check(
        first.net_down.is_none() && first.net_up.is_none(),
        "the first network rate is unmeasured",
    );
    check(!history.lease("", None), "an empty lease id is refused");
    check(
        !history.lease(&"x".repeat(257), None),
        "an over-long lease id is refused",
    );
    check(history.lease("monitor", Some(60_000)), "a lease is granted");
    history.pause();
    worker.join().expect("the paused runner returns");
    check(
        !history.lease("monitor", None),
        "no lease is granted while paused",
    );
    check(
        !history.samples(None).is_empty(),
        "retained samples survive a pause",
    );
    check(history.resume(), "resume re-arms a joined runner");
    let worker = start();
    let second = wait_sample(Some(first.ts));
    check(
        second.ts == first.ts + 1,
        "the exclusive cursor sees the next sample after a resume",
    );
    history.stop();
    worker.join().expect("the stopped runner returns");
    check(!history.resume(), "a stopped history cannot resume");
    check(
        !history.lease("monitor", None),
        "no lease is granted after stop",
    );
    println!("ok: history pause, resume, cursor and stop");
}
