//! Durable capture of a detached child's stderr: the child writes a raw file, a later drain turns
//! it into scrubbed, prefixed, rotated text. The "child" here is the handle `stderr_file` returns.

use rightkit_logs::{DetachedLog, DrainReport, KnownSecrets, RotatingTextWriter};
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
    // The first line after a compaction may be the tail of a line whose start was dropped:
    // the marker stands for it. Lines after it are written normally.
    child.write_all(b"next\n").unwrap();
    assert_eq!(log.drain().unwrap().lines, 1);
    child.write_all(b"after\n").unwrap();
    assert_eq!(log.drain().unwrap().lines, 1);
    assert_eq!(
        sink_text(dir.path()),
        "0123456789abcdef-more\n[redacted: line after raw-file compaction omitted]\nafter\n"
    );
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
fn a_position_beyond_the_file_restarts_at_the_beginning_with_the_first_line_guarded() {
    // A saved position past the end means the raw file was compacted or replaced after it was
    // saved: reading restarts at offset 0 and the first line becomes the compaction marker, since
    // its start may be the tail of a line that was cut (0.3.4 compaction guard).
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"cut-tail\nkept\n").unwrap();
    fs::write(dir.path().join("job.log.raw.pos"), "9999").unwrap();
    assert_eq!(log.drain().unwrap().lines, 2);
    assert_eq!(
        sink_text(dir.path()),
        "[redacted: line after raw-file compaction omitted]\nkept\n"
    );
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
/// 64 KiB: a line longer than this (before its line break) is omitted whole.
const SEGMENT: usize = 64 * 1024;
/// The one line written in place of an omitted line.
const OMITTED: &str = "[redacted: long line omitted]";

/// Writes `payload` as the child's stderr and drains it once.
fn drain_payload(secrets: KnownSecrets, payload: &[u8]) -> (String, DrainReport) {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path())).with_known_secrets(secrets);
    let mut child = log.stderr_file().unwrap();
    child.write_all(payload).unwrap();
    drop(child);
    let report = log.drain().unwrap();
    (sink_text(dir.path()), report)
}

/// One line of `x` filler with `needle` placed so the 64 KiB point falls `inside` bytes into it,
/// then filler of `y`, then a line break. The needles used here hold no `x` or `y`.
fn line_with(needle: &[u8], inside: usize) -> Vec<u8> {
    let mut line = vec![b'x'; SEGMENT - inside];
    line.extend_from_slice(needle);
    line.extend_from_slice(&[b'y'; 100]);
    line.push(b'\n');
    line
}

fn percent_encode(text: &str, upper: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else if upper {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push_str(&format!("%{byte:02x}"));
        }
    }
    out
}

#[test]
fn a_long_sk_token_spanning_the_cut_writes_no_suffix_byte() {
    let token = "sk-proj-Qm7Zt3Vb9LpR2nXc5Dk8Hs1Fw4Jy6Ga0UeTz3Nq8Rb5Wv2Kc7Hd";
    for secrets in [
        KnownSecrets::new(),
        KnownSecrets::from_values(["unrelated-known-secret"]),
    ] {
        for before_cut in [1usize, 3, 4, 8, 20, token.len() - 1] {
            let mut payload = b"ok line\n".to_vec();
            payload.extend_from_slice(&vec![b'x'; SEGMENT - before_cut]);
            payload.extend_from_slice(token.as_bytes());
            payload.extend_from_slice(b" and some more words\nafter\n");
            let (text, report) = drain_payload(secrets.clone(), &payload);
            assert_eq!(
                text,
                format!("ok line\n{OMITTED}\nafter\n"),
                "before_cut {before_cut}"
            );
            assert_eq!(report.lines, 3);
            assert_eq!(report.bytes, payload.len() as u64);
            assert_eq!(report.pending_bytes, 0);
        }
    }
}

#[test]
fn known_secrets_at_the_cut_never_reach_the_sink() {
    let raw = "CUSTOMTOKENabcdefghijk";
    let multibyte = "pässwörd-ключ-密码";
    let cases: Vec<(&str, String)> = vec![
        (raw, raw.to_string()),
        (raw, percent_encode(raw, true)),
        (
            "pa ss/word@CUSTOMTOKEN",
            "pa%20ss%2Fword%40CUSTOMTOKEN".to_string(),
        ),
        (multibyte, multibyte.to_string()),
        (multibyte, percent_encode(multibyte, false)),
        (
            "TOPCUSTOMSECRET\nBOTTOMCUSTOMSECRET",
            "BOTTOMCUSTOMSECRET".to_string(),
        ),
    ];
    for (secret, form) in cases {
        for inside in 0..=form.len() {
            let payload = line_with(form.as_bytes(), inside);
            let (text, report) = drain_payload(KnownSecrets::from_values([secret]), &payload);
            assert_eq!(text, format!("{OMITTED}\n"), "{form} inside {inside}");
            assert_eq!(report.lines, 1);
            assert_eq!(report.bytes, payload.len() as u64);
        }
    }
}

#[test]
fn a_line_of_64_kib_or_less_is_written_whole_and_a_longer_one_is_omitted() {
    let known = || KnownSecrets::from_values(["NEVERPRESENT-0123456789"]);
    for secrets in [KnownSecrets::new(), known()] {
        let mut exact = vec![b'x'; SEGMENT];
        exact.push(b'\n');
        let (text, report) = drain_payload(secrets.clone(), &exact);
        assert_eq!(report.lines, 1);
        assert_eq!(text.len(), SEGMENT + 1);
        assert!(text.bytes().take(SEGMENT).all(|b| b == b'x') && text.ends_with('\n'));

        let mut over = vec![b'x'; SEGMENT + 1];
        over.push(b'\n');
        let (text, report) = drain_payload(secrets.clone(), &over);
        assert_eq!(text, format!("{OMITTED}\n"));
        assert_eq!(report.bytes, over.len() as u64);
    }
}

#[test]
fn short_non_secret_lines_are_written_byte_exact() {
    let lines = [
        "plain text",
        "tabs\tand  double  spaces and trailing ",
        "unicode \u{e9} \u{20ac} \u{1d11e} \u{43a}\u{43b}\u{44e}\u{447}",
        "",
        "[json] {\"a\": [1, 2, 3], \"b\": null}",
    ];
    let long = "lorem ipsum dolor sit amet ".repeat(2000);
    for secrets in [
        KnownSecrets::new(),
        KnownSecrets::from_values(["NEVERPRESENT-0123456789"]),
    ] {
        let mut payload = String::new();
        let mut expected = String::new();
        for line in lines.iter().copied().chain(std::iter::once(long.as_str())) {
            payload.push_str(line);
            payload.push_str("\r\n");
            expected.push_str(line);
            expected.push('\n');
        }
        let (text, report) = drain_payload(secrets, payload.as_bytes());
        assert_eq!(text, expected);
        assert_eq!(report.lines, 6);
        assert_eq!(report.bytes, payload.len() as u64);
    }
}

#[test]
fn drain_omits_a_long_line_once_and_keeps_its_neighbours() {
    let mut payload = b"short\n".to_vec();
    payload.extend_from_slice(&vec![b'a'; 200_000]);
    payload.extend_from_slice(b"\nafter\n");
    let (text, report) = drain_payload(KnownSecrets::new(), &payload);
    assert_eq!(text, format!("short\n{OMITTED}\nafter\n"));
    assert_eq!(report.lines, 3);
    assert_eq!(report.bytes, payload.len() as u64);
    assert_eq!(report.pending_bytes, 0);
}

#[test]
fn a_line_under_the_limit_waits_and_one_that_grows_past_it_is_omitted() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path()));
    let mut child = log.stderr_file().unwrap();
    child.write_all(&vec![b'a'; 40_000]).unwrap();
    let first = log.drain().unwrap();
    assert_eq!(
        (first.lines, first.bytes, first.pending_bytes),
        (0, 0, 40_000)
    );
    assert_eq!(sink_text(dir.path()), "");

    child.write_all(&vec![b'a'; 40_000]).unwrap();
    let second = log.drain().unwrap();
    assert_eq!(
        (second.lines, second.bytes, second.pending_bytes),
        (1, 80_000, 0)
    );
    assert_eq!(sink_text(dir.path()), format!("{OMITTED}\n"));

    child.write_all(b"\nafter\n").unwrap();
    let third = log.drain().unwrap();
    assert_eq!(third.lines, 1);
    assert_eq!(sink_text(dir.path()), format!("{OMITTED}\nafter\n"));
}

#[test]
fn a_restart_in_the_middle_of_an_omitted_line_writes_no_second_marker() {
    let dir = tempfile::tempdir().unwrap();
    let first = DetachedLog::new(sink(dir.path()));
    let mut child = first.stderr_file().unwrap();
    child.write_all(&vec![b'a'; 100_000]).unwrap();
    let report = first.drain().unwrap();
    assert_eq!(
        (report.lines, report.bytes, report.pending_bytes),
        (1, 100_000, 0)
    );
    assert_eq!(sink_text(dir.path()), format!("{OMITTED}\n"));
    let saved = fs::read_to_string(dir.path().join("job.log.raw.pos")).unwrap();
    assert_eq!(saved.trim(), "100000 omit");
    drop(first);

    // A new process: the rest of the line is discarded, then ordinary lines resume.
    child.write_all(&vec![b'b'; 50_000]).unwrap();
    child.write_all(b"\nnext\n").unwrap();
    let second = DetachedLog::new(sink(dir.path()));
    let report = second.drain().unwrap();
    assert_eq!((report.lines, report.bytes), (1, 50_006));
    assert_eq!(sink_text(dir.path()), format!("{OMITTED}\nnext\n"));
    let saved = fs::read_to_string(dir.path().join("job.log.raw.pos")).unwrap();
    assert_eq!(saved.trim(), "150006");
}

#[test]
fn finish_after_a_restart_in_an_omitted_line_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let first = DetachedLog::new(sink(dir.path()));
    let mut child = first.stderr_file().unwrap();
    child.write_all(&vec![b'a'; 100_000]).unwrap();
    first.drain().unwrap();
    child.write_all(&vec![b'b'; 1_000]).unwrap();
    drop(child);
    drop(first);

    let second = DetachedLog::new(sink(dir.path()));
    let report = second.finish().unwrap();
    assert!(report.finished);
    assert_eq!(report.lines, 0);
    assert_eq!(sink_text(dir.path()), format!("{OMITTED}\n"));
    assert!(!second.raw_path().exists());
    assert!(!dir.path().join("job.log.raw.pos").exists());
}

#[test]
fn finish_turns_an_unterminated_long_line_into_the_marker_alone() {
    let dir = tempfile::tempdir().unwrap();
    let log = DetachedLog::new(sink(dir.path())).with_prefix("[job] ");
    let mut child = log.stderr_file().unwrap();
    child.write_all(b"kept\n").unwrap();
    let mut open = vec![b'x'; SEGMENT - 3];
    open.extend_from_slice(b"sk-abcdef0123456789ABCDEF0123456789");
    child.write_all(&open).unwrap();
    drop(child);
    let report = log.finish().unwrap();
    assert!(report.finished);
    assert_eq!(report.lines, 2);
    assert_eq!(
        sink_text(dir.path()),
        format!("[job] kept\n[job] {OMITTED}\n")
    );
    assert!(!log.raw_path().exists());
}

#[test]
fn a_multiline_known_secret_is_removed_fragment_by_fragment() {
    let secrets = || KnownSecrets::from_values(["TOPCUSTOMSECRET\nBOTTOMCUSTOMSECRET"]);
    let payload: &[u8] = b"first TOPCUSTOMSECRET\nBOTTOMCUSTOMSECRET last\n\
        q=TOPCUSTOMSECRET%0ABOTTOMCUSTOMSECRET\n\
        j=TOPCUSTOMSECRET\\nBOTTOMCUSTOMSECRET\n\
        win TOPCUSTOMSECRET\r\nBOTTOMCUSTOMSECRET\r\n";
    let (text, report) = drain_payload(secrets(), payload);
    assert!(!text.contains("CUSTOMSECRET"), "{text}");
    assert_eq!(report.lines, 6);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "first [redacted]");
    assert_eq!(lines[1], "[redacted] last");
    assert_eq!(lines[4], "win [redacted]");
    assert_eq!(lines[5], "[redacted]");

    // Across a restart: each process removes its own lines' fragment.
    let dir = tempfile::tempdir().unwrap();
    let first = DetachedLog::new(sink(dir.path())).with_known_secrets(secrets());
    let mut child = first.stderr_file().unwrap();
    child
        .write_all(b"TOPCUSTOMSECRET\nBOTTOMCUSTOMSECRET")
        .unwrap();
    let report = first.drain().unwrap();
    assert_eq!(report.lines, 1);
    assert_eq!(sink_text(dir.path()), "[redacted]\n");
    drop(first);
    drop(child);

    // Finish, unterminated last line holding the second fragment.
    let second = DetachedLog::new(sink(dir.path())).with_known_secrets(secrets());
    let report = second.finish().unwrap();
    assert!(report.finished);
    assert_eq!(report.lines, 1);
    assert_eq!(sink_text(dir.path()), "[redacted]\n[redacted]\n");
}
