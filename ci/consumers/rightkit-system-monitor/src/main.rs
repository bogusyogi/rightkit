//! Registry consumer for rightkit-system-monitor 0.1.0: builds against the published API only.
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
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-system-monitor consumer: all checks passed");
}
