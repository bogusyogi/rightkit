// rightkit-logs 0.3.4 consumed by exact registry pin, outside the workspace.
//
// Proves: KnownSecrets redaction (exact value and its percent-encoded form, Debug never shows
// the value); scrub_text pattern redaction (Bearer token, e-mail); KnownSecrets::scrub_text
// ordering; DetachedLog capture: a stand-in "child" appends raw stderr to the private raw file,
// drain moves complete lines into the rotating sink scrubbed and prefixed, an unfinished last
// line waits, finish drains the rest and removes the raw file; a raw file never reaches the sink
// unscrubbed. Whole-line omission: a detached line over 64 KiB is replaced by one
// "[redacted: long line omitted]" line (written once across drains) with no byte of it written, so
// a known secret and an sk- token straddling the 64 KiB cut leave no fragment; a line of exactly
// 64 KiB is written whole. Public API only.
// Does not prove: the optional http / tracing / testing features, the consented outbox and
// uploader, a real detached child process, cross-process drain locking, rotation under load.
// State: temp dir only, removed on drop. No network.
use rightkit_logs::{
    scrub_text, DetachedLog, KnownSecrets, RotatingTextWriter, KNOWN_SECRET_MARKER,
};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rk-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() {
    let tmp = TempDir::new("logs-packed");
    let dir = tmp.0.clone();

    // Known-value redaction: exact and percent-encoded forms.
    let secret = "pk-live key/with+chars-98765";
    let secrets = KnownSecrets::from_values([secret]);
    assert_eq!(secrets.len(), 1);
    assert!(!format!("{secrets:?}").contains("98765"), "Debug never prints the value");
    let text = format!("failed with {secret} and enc pk-live%20key%2Fwith%2Bchars-98765 done");
    assert!(secrets.is_exposed(&text));
    let redacted = secrets.redact(&text);
    assert!(!redacted.contains("98765"), "no fragment survives: {redacted}");
    assert!(redacted.contains(KNOWN_SECRET_MARKER));
    assert!(!secrets.is_exposed(&redacted));
    assert_eq!(KnownSecrets::new().redact("same"), "same", "empty set changes nothing");

    // Pattern scrub.
    let scrubbed = scrub_text("Bearer abcdef123 sent to ada@example.com");
    assert_eq!(scrubbed, "Bearer [redacted:credential] sent to [redacted:email]");
    let both = secrets.scrub_text(&format!("{secret} Bearer abcdef123"));
    assert!(!both.contains("98765") && !both.contains("abcdef123"), "{both}");

    // DetachedLog: raw child output in, scrubbed prefixed lines out.
    let sink = Arc::new(RotatingTextWriter::new(dir.join("job.log"), 1 << 20).with_generations(2));
    let log = DetachedLog::new(Arc::clone(&sink))
        .with_prefix("[job] ")
        .with_known_secrets(secrets.clone());
    assert!(!log.has_pending(), "nothing pending before any output");
    {
        // Stand-in for a detached child: it only needs an append handle on the raw file.
        let mut child_stderr = log.stderr_file().expect("stderr_file");
        writeln!(child_stderr, "starting with token {secret}").unwrap();
        writeln!(child_stderr, "auth header Bearer abcdef123").unwrap();
        write!(child_stderr, "unfinished tail").unwrap();
        child_stderr.flush().unwrap();
    }
    assert!(log.has_pending());
    let first = log.drain().expect("drain");
    assert_eq!(first.lines, 2, "two complete lines drained");
    assert!(first.pending_bytes > 0, "the unfinished line waits for its newline");
    assert!(!first.skipped && !first.finished);
    let after_first = std::fs::read_to_string(sink.path()).expect("read sink");
    assert!(after_first.contains("[job] starting with token [redacted]"), "{after_first}");
    assert!(after_first.contains("Bearer [redacted:credential]"), "{after_first}");
    assert!(!after_first.contains("98765") && !after_first.contains("abcdef123"));
    assert!(!after_first.contains("unfinished tail"), "partial line not yet written");

    let done = log.finish().expect("finish");
    assert!(done.finished, "finish removes the raw file");
    assert!(!log.raw_path().exists(), "raw file gone after finish");
    let after_finish = std::fs::read_to_string(sink.path()).expect("read sink 2");
    assert!(after_finish.contains("[job] unfinished tail"), "{after_finish}");
    assert!(!after_finish.contains("98765"));
    assert_eq!(sink.status().failed_appends, 0);

    // Whole-line omission over the public API: the known secret and a pattern token straddle the
    // 64 KiB cut of one unterminated line; none of the line may reach the sink.
    const LIMIT: usize = 64 * 1024;
    let token = "sk-proj-Qm7Zt3Vb9LpR2nXc5Dk8Hs1Fw4Jy6Ga0UeTz3Nq8Rb5Wv2Kc7Hd";
    let long_sink =
        Arc::new(RotatingTextWriter::new(dir.join("long.log"), 1 << 20).with_generations(2));
    let long = DetachedLog::new(Arc::clone(&long_sink))
        .with_prefix("[long] ")
        .with_known_secrets(secrets.clone());
    {
        let mut child = long.stderr_file().expect("stderr_file long");
        child.write_all(b"before\n").unwrap();
        child.write_all("x".repeat(LIMIT - 7).as_bytes()).unwrap();
        child.write_all(secret.as_bytes()).unwrap();
        child.write_all(b" ").unwrap();
        child.write_all(token.as_bytes()).unwrap();
        child.flush().unwrap();
    }
    let first_long = long.drain().expect("drain long");
    assert_eq!(first_long.lines, 2, "the short line and the omission marker");
    let mid = std::fs::read_to_string(long_sink.path()).expect("read long sink");
    assert_eq!(mid, "[long] before\n[long] [redacted: long line omitted]\n", "{mid}");
    {
        // The child finishes the over-long line, then writes a normal line and a 64 KiB line.
        let mut child = long.stderr_file().expect("stderr_file long again");
        child.write_all(b" and the rest of the long line\nafter\n").unwrap();
        child.write_all("x".repeat(LIMIT).as_bytes()).unwrap();
        child.write_all(b"\n").unwrap();
        child.flush().unwrap();
    }
    let second_long = long.drain().expect("drain long 2");
    assert_eq!(second_long.lines, 2, "no second marker: the normal line and the 64 KiB line");
    let long_text = std::fs::read_to_string(long_sink.path()).expect("read long sink 2");
    assert_eq!(long_text.matches("long line omitted").count(), 1, "marker written once");
    assert!(long_text.contains("[long] after\n"), "the next line is written");
    assert!(
        long_text.ends_with(&format!("[long] {}\n", "x".repeat(LIMIT))),
        "a line of exactly 64 KiB is written whole"
    );
    for fragment in ["98765", "pk-live", "sk-proj", "Qm7Zt3", "Kc7Hd", "the rest of the long line"] {
        assert!(!long_text.contains(fragment), "fragment {fragment} reached the sink");
    }
    assert!(long.finish().expect("finish long").finished);
    assert!(!long.raw_path().exists(), "long raw file gone after finish");
    assert_eq!(long_sink.status().failed_appends, 0);

    drop(long);
    drop(long_sink);
    drop(log);
    drop(sink);
    drop(tmp);
    assert!(!dir.exists(), "temp dir removed");
    println!("OK rightkit-logs 0.3.4 (sealed registry consumer)");
}
