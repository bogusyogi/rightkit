//! A writer-owned clear truncates only the regular file the writer opened privately. A link
//! swapped in at the active daily path must be refused, and its target must keep its bytes.

#![cfg(feature = "tracing")]

use rightkit_logs::{tracing_layer, TracingFileConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing_subscriber::Registry;

const OUTSIDE_BYTES: &[u8] = b"outside data that must survive a clear\n";

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-logs-clear-links-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create clear-links fixture directory");
    path
}

/// The single daily file the writer opened at setup.
fn active_daily_file(dir: &Path, prefix: &str) -> PathBuf {
    let stem = format!("{prefix}.");
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&stem) && name.ends_with(".jsonl"))
        })
        .collect();
    assert_eq!(found.len(), 1, "exactly one active daily file: {found:?}");
    found.remove(0)
}

#[cfg(unix)]
fn make_file_link(link: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_file_link(link: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Move the writer's open daily file aside (allowed while it is open) and put the link made by
/// `make_link` at the active name, so the active pathname now resolves to something the writer
/// never opened. Returns false (the test skips) when the link cannot be created on this host.
fn swap_in_link(
    dir: &Path,
    active: &Path,
    make_link: impl FnOnce(&Path) -> std::io::Result<()>,
) -> bool {
    fs::rename(active, dir.join("moved-aside.bak")).expect("move the open daily file aside");
    match make_link(active) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("skipping: cannot create a link here ({error})");
            false
        }
    }
}

#[test]
fn clear_refuses_a_symlink_at_the_active_path_and_keeps_its_target() {
    let dir = temp_dir("symlink");
    let outside = temp_dir("symlink-outside").join("outside.txt");
    fs::write(&outside, OUTSIDE_BYTES).expect("seed outside file");

    let (_layer, guard) = tracing_layer::<Registry>(TracingFileConfig::new(&dir, "linkclear"));
    let active = active_daily_file(&dir, "linkclear");
    if !swap_in_link(&dir, &active, |link| make_file_link(link, &outside)) {
        return;
    }

    let result = guard.clear();
    assert!(
        result.is_err(),
        "clear must refuse a link at the active path"
    );
    assert_eq!(
        fs::read(&outside).expect("read outside file"),
        OUTSIDE_BYTES,
        "the linked target must not be truncated"
    );
    drop(guard);
    assert_eq!(
        fs::read(&outside).expect("read outside file after shutdown"),
        OUTSIDE_BYTES,
        "shutdown must not write through or truncate the linked target"
    );
}

#[cfg(windows)]
#[test]
fn clear_refuses_a_junction_at_the_active_path_and_keeps_its_target() {
    let dir = temp_dir("junction");
    let outside_dir = temp_dir("junction-outside");
    let outside = outside_dir.join("outside.txt");
    fs::write(&outside, OUTSIDE_BYTES).expect("seed outside file");

    let (_layer, guard) = tracing_layer::<Registry>(TracingFileConfig::new(&dir, "junctionclear"));
    let active = active_daily_file(&dir, "junctionclear");
    let linked = swap_in_link(&dir, &active, |link| {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(&outside_dir)
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("mklink /J failed"))
        }
    });
    if !linked {
        return;
    }

    assert!(
        guard.clear().is_err(),
        "clear must refuse a reparse point at the active path"
    );
    assert_eq!(
        fs::read(&outside).expect("read outside file"),
        OUTSIDE_BYTES,
        "the junction target must be untouched"
    );
    drop(guard);
}
