//! Registry consumer for rightkit-disk-cleanup 0.1.0: builds against the published API only.
//! It scans only a tree it creates under the temp directory and deletes nothing else.
use rightkit_disk_cleanup::{cleanup_scan, scan_paths, store, ScanOptions};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let dir = std::env::temp_dir().join(format!("rightkit-disk-cleanup-consumer-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).expect("temp dir");
    std::fs::write(dir.join("sub").join("a.bin"), vec![0u8; 4096]).expect("fixture file");
    let report = scan_paths(std::slice::from_ref(&dir), &ScanOptions::default());
    check(report.roots.len() == 1, "the scan reports its root");
    check(!report.entries.is_empty(), "the scan lists the fixture entries");
    check(store::validate_id("../escape").is_err(), "a path-like snapshot id is refused");
    check(cleanup_scan::set_state_app_name("ConsumerCheck"), "the host app names its state folder once");
    check(!cleanup_scan::set_state_app_name("Other"), "a second name is refused");
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-disk-cleanup consumer: all checks passed");
}
