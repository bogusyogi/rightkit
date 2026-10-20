//! Durable capture of a detached child's stderr: the child writes a raw file, a later drain turns
//! it into scrubbed, prefixed, rotated text. The "child" here is the handle `stderr_file` returns.

use rightkit_logs::{DetachedLog, KnownSecrets, RotatingTextWriter};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn sink(dir: &Path) -> Arc<RotatingTextWriter> {
    Arc::new(RotatingTextWriter::new(dir.join("job.log"), 1 << 20).with_generations(3))
}

fn sink_text(dir: &Path) -> String {
    fs::read_to_string(dir.join("job.log")).unwrap_or_default()
}

#[test]
fn drain_scrubs_prefixes_and_keeps_an_unfinished_line() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path())).with_prefix("[job] ");
    let mut child = log.stderr_file().unwrap();
    child
        .write_all(b"starting in /Users/ada/project\nmail ada@example.com failed\r\npartial")
        .unwrap();

    let report = log.drain().unwrap();
    assert_eq!(report.lines, 2);
    assert!(report.pending_bytes > 0, "the unfinished line stays raw");
    let text = sink_text(dir.path());
    assert!(
        text.contains("[job] starting in /Users/<user>/project"),
        "{text}"
    );
    assert!(!text.contains("ada@example.com"), "{text}");
    assert!(!text.contains("partial"), "{text}");
    assert!(log.has_pending());

    child.write_all(b" line\n").unwrap();
    let report = log.drain().unwrap();
    assert_eq!(report.lines, 1);
    assert!(sink_text(dir.path()).contains("[job] partial line"));
    assert!(!log.has_pending());
}

#[test]
fn a_second_drain_repeats_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"one\ntwo\n").unwrap();
    assert_eq!(log.drain().unwrap().lines, 2);
    assert_eq!(log.drain().unwrap().lines, 0);
    child.write_all(b"three\n").unwrap();
    assert_eq!(log.drain().unwrap().lines, 1);
    assert_eq!(sink_text(dir.path()), "one\ntwo\nthree\n");
}

#[test]
fn a_restarted_process_continues_from_the_saved_position() {
    let dir = tempfile::tempdir().unwrap();
    let first = DetachedLog::new(sink(dir.path()));
    let mut child = first.stderr_file().unwrap();
    child.write_all(b"before restart\n").unwrap();
    assert_eq!(first.drain().unwrap().lines, 1);
    child.write_all(b"after restart\nlast words").unwrap();
    drop(first);
    drop(child);

    // The parent is gone; a new launch finds the raw file and finishes it.
    let second = DetachedLog::new(sink(dir.path()));
    assert!(second.has_pending());
    let report = second.finish().unwrap();
    assert_eq!(report.lines, 2);
    assert!(report.finished);
    assert_eq!(
        sink_text(dir.path()),
        "before restart\nafter restart\nlast words\n"
    );
    assert!(!second.raw_path().exists());
    assert!(!dir.path().join("job.log.raw.pos").exists());
    assert!(!second.has_pending());
}

#[test]
fn known_secrets_are_removed_from_drained_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()))
        .with_known_secrets(KnownSecrets::from_values(["zzz-custom-key-123"]));
    let mut child = log.stderr_file().unwrap();
    child
        .write_all(b"auth failed for zzz-custom-key-123\nurl ?k=zzz-custom-key-123\n")
        .unwrap();
    log.drain().unwrap();
    let text = sink_text(dir.path());
    assert!(!text.contains("zzz-custom-key-123"), "{text}");
    assert!(text.contains("auth failed for [redacted]"), "{text}");
}

#[test]
fn an_unterminated_last_line_is_scrubbed_by_finish() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()))
        .with_known_secrets(KnownSecrets::from_values(["tail-secret-value"]));
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"ends with tail-secret-value").unwrap();
    drop(child);
    log.finish().unwrap();
    assert_eq!(sink_text(dir.path()), "ends with [redacted]\n");
}

#[test]
fn a_fully_drained_large_raw_file_is_emptied_and_capture_continues() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path())).with_compact_at(16);
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"0123456789abcdef-more\n").unwrap();
    let report = log.drain().unwrap();
    assert!(report.compacted);
    assert_eq!(fs::metadata(log.raw_path()).unwrap().len(), 0);
    child.write_all(b"next\n").unwrap();
    assert_eq!(log.drain().unwrap().lines, 1);
    assert_eq!(sink_text(dir.path()), "0123456789abcdef-more\nnext\n");
}

#[test]
fn compaction_off_leaves_the_raw_file_until_finish() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path())).with_compact_at(0);
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"0123456789abcdef-more\n").unwrap();
    drop(child);
    assert!(!log.drain().unwrap().compacted);
    assert!(fs::metadata(log.raw_path()).unwrap().len() > 0);
    assert!(log.finish().unwrap().finished);
    assert!(!log.raw_path().exists());
}

#[test]
fn a_line_over_64_kib_is_split_and_a_cut_credential_prefix_is_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let mut child = log.stderr_file().unwrap();
    let mut line = vec![b'x'; 64 * 1024 - 4];
    line.extend_from_slice(b" sk-");
    line.extend_from_slice(b"tail\n");
    child.write_all(&line).unwrap();
    let report = log.drain().unwrap();
    assert_eq!(report.lines, 2);
    let text = sink_text(dir.path());
    assert!(
        !text.contains("sk-"),
        "the cut credential prefix is redacted"
    );
}

#[test]
fn a_missing_raw_file_is_an_empty_drain_and_a_finished_one() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    assert_eq!(log.drain().unwrap().lines, 0);
    assert!(!log.has_pending());
    assert!(log.finish().unwrap().finished);
}

#[test]
fn a_stale_position_for_a_gone_raw_file_is_cleared_on_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("job.log.raw.pos"), "9999").unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let mut child = log.stderr_file().unwrap();
    assert!(!dir.path().join("job.log.raw.pos").exists());
    child.write_all(b"fresh\n").unwrap();
    assert_eq!(log.drain().unwrap().lines, 1);
}

#[test]
fn a_position_beyond_the_file_restarts_at_the_beginning() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"kept\n").unwrap();
    fs::write(dir.path().join("job.log.raw.pos"), "9999").unwrap();
    assert_eq!(log.drain().unwrap().lines, 1);
    assert_eq!(sink_text(dir.path()), "kept\n");
}

#[test]
fn the_sink_rotates_drained_text() {
    let dir = tempfile::tempdir().unwrap();
    let small =
        Arc::new(RotatingTextWriter::new(dir.path().join("job.log"), 40).with_generations(2));
    let log = DetachedLog::new(small);
    let mut child = log.stderr_file().unwrap();
    for i in 0..10 {
        writeln!(child, "line number {i} with padding").unwrap();
    }
    assert_eq!(log.drain().unwrap().lines, 10);
    assert!(dir.path().join("job.log.1").exists());
    assert!(!dir.path().join("job.log.3").exists());
}

#[test]
fn a_background_drainer_drains_while_the_child_writes() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(DetachedLog::new(sink(dir.path())));
    let mut child = log.stderr_file().unwrap();
    let drainer = log.spawn_drainer(Duration::from_millis(10)).unwrap();
    child.write_all(b"streamed\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sink_text(dir.path()).contains("streamed") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    drainer.stop();
    assert_eq!(sink_text(dir.path()), "streamed\n");
    assert_eq!(log.last_error(), None);
}

#[test]
fn a_custom_raw_path_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("private").join("child.err");
    let log = DetachedLog::new(sink(dir.path())).with_raw_path(&raw);
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"x\n").unwrap();
    assert_eq!(log.raw_path(), raw);
    assert_eq!(log.drain().unwrap().lines, 1);
    assert!(raw.exists());
}

#[cfg(unix)]
#[test]
fn the_raw_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let _child = log.stderr_file().unwrap();
    let mode = fs::metadata(log.raw_path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_symlink_at_the_raw_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    fs::write(&target, "keep").unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("job.log.raw")).unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    assert!(log.stderr_file().is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
}
