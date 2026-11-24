//! `Sandbox::spawn_pty_stream` end to end on Unix: a lossless async terminal, confinement, staged
//! cancel, slot release and the terminal size read back from the kernel. Nothing is mocked.

#![cfg(all(unix, feature = "pty"))]

use rightkit_sandbox::process_tree::is_alive;
use rightkit_sandbox::{
    NetworkPolicy, PtyError, PtyReader, PtySize, ResourceAuthority, ResourceProfile, Sandbox,
    SandboxCommand, SandboxError, SandboxPolicy, StreamedPty, TerminationOptions,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;

const LIMIT: Duration = Duration::from_secs(60);

/// A unique scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rk-pty-stream-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(fs::canonicalize(&dir).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sandbox(profile: ResourceProfile) -> Sandbox {
    Sandbox::new(ResourceAuthority::new(profile))
}

fn sleep_command() -> SandboxCommand {
    SandboxCommand::new("sleep").arg("30")
}

fn confined_policy(root: &Path) -> SandboxPolicy {
    SandboxPolicy::new()
        .allow_read(root)
        .network(NetworkPolicy::Deny)
}

/// Landlock is the only Linux confinement; without it the launch fails closed, so skip.
fn confinement_available(test: &str) -> bool {
    if cfg!(target_os = "linux") && !rightkit_sandbox::is_landlock_available() {
        eprintln!("skipping {test}: Landlock is not available on this host");
        return false;
    }
    true
}

fn expect_spawned(result: Result<StreamedPty, PtyError>) -> StreamedPty {
    match result {
        Ok(streamed) => streamed,
        Err(error) => panic!("spawn_pty_stream failed: {error}"),
    }
}

fn expect_failed(result: Result<StreamedPty, PtyError>) -> PtyError {
    match result {
        Ok(_) => panic!("spawn_pty_stream must fail"),
        Err(error) => error,
    }
}

async fn read_until(reader: &mut PtyReader, needle: &str) -> String {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + LIMIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let n = tokio::time::timeout(left, reader.read(&mut chunk))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for {needle:?}; saw {:?}",
                    String::from_utf8_lossy(&seen)
                )
            })
            .unwrap_or(0);
        if n == 0 {
            panic!(
                "stream ended before {needle:?}; saw {:?}",
                String::from_utf8_lossy(&seen)
            );
        }
        seen.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&seen).into_owned();
        if text.contains(needle) {
            return text;
        }
    }
}

async fn read_to_eof(reader: &mut PtyReader) -> Vec<u8> {
    let mut out = Vec::new();
    tokio::time::timeout(LIMIT, reader.read_to_end(&mut out))
        .await
        .expect("the stream must reach end of file")
        .ok();
    out
}

async fn cancel(streamed: &mut StreamedPty) {
    streamed
        .cancel(TerminationOptions::default())
        .await
        .expect("cancel must end the child");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_mebibytes_arrive_complete() {
    const BYTES: usize = 2 * 1024 * 1024;
    let sandbox = sandbox(ResourceProfile::Practical);
    let command = SandboxCommand::new("sh")
        .arg("-c")
        .arg(format!("head -c {BYTES} /dev/zero | tr '\\0' x"));
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(None, command, PtySize::default()));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");

    let out = read_to_eof(&mut reader).await;
    assert_eq!(
        out.len(),
        BYTES,
        "every byte the child wrote must arrive; the 1 MiB ring of PtySession could not keep them"
    );
    assert!(out.iter().all(|b| *b == b'x'));
    let status = tokio::time::timeout(LIMIT, streamed.wait())
        .await
        .expect("wait")
        .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn input_is_echoed_and_the_streams_are_taken_once() {
    let sandbox = sandbox(ResourceProfile::Practical);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        None,
        SandboxCommand::new("sh"),
        PtySize::default(),
    ));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");
    let mut writer = streamed.take_writer().unwrap().expect("a writer");
    assert!(streamed.take_reader().unwrap().is_none());
    assert!(streamed.take_writer().unwrap().is_none());

    writer.write_input(b"echo hi\r").await.unwrap();
    // The terminal echoes "echo hi" and the shell then prints its output on a line of its own.
    let text = read_until(&mut reader, "\nhi\r").await;
    assert!(text.contains("\nhi\r"), "{text:?}");

    cancel(&mut streamed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confined_child_cannot_read_outside_its_root_and_the_stream_shows_the_error() {
    if !confinement_available("a_confined_child_cannot_read_outside_its_root") {
        return;
    }
    let inside = Scratch::new("inside");
    let outside = Scratch::new("outside");
    fs::write(inside.path().join("ok.txt"), "INSIDE-OK").unwrap();
    fs::write(outside.path().join("secret.txt"), "OUTSIDE-SECRET").unwrap();

    let policy = confined_policy(inside.path());
    let script = format!(
        "cat {}; echo; cat {}",
        inside.path().join("ok.txt").display(),
        outside.path().join("secret.txt").display()
    );
    let command = SandboxCommand::new("sh").arg("-c").arg(script);
    let sandbox = sandbox(ResourceProfile::Practical);
    let mut streamed =
        expect_spawned(sandbox.spawn_pty_stream(Some(&policy), command, PtySize::default()));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");

    let out = String::from_utf8_lossy(&read_to_eof(&mut reader).await).into_owned();
    assert!(
        out.contains("INSIDE-OK"),
        "the granted root is readable: {out:?}"
    );
    assert!(
        !out.contains("OUTSIDE-SECRET"),
        "a file outside the root must not be readable: {out:?}"
    );
    assert!(
        out.contains("Permission denied") || out.contains("Operation not permitted"),
        "the stream must deliver the error text: {out:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_ends_the_child_and_its_grandchild() {
    let sandbox = sandbox(ResourceProfile::Practical);
    let command = SandboxCommand::new("sh")
        .arg("-c")
        .arg("sleep 60 & echo gc=$!:end; wait");
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(None, command, PtySize::default()));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");
    let child = streamed.id();

    let text = read_until(&mut reader, ":end").await;
    let start = text.find("gc=").unwrap() + 3;
    let grandchild: u32 = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .expect("the grandchild pid");
    assert!(is_alive(child), "the child runs before cancel");
    assert!(is_alive(grandchild), "the grandchild runs before cancel");

    let status = tokio::time::timeout(
        Duration::from_secs(20),
        streamed.cancel(TerminationOptions::default()),
    )
    .await
    .expect("cancel must return promptly")
    .unwrap();
    assert!(!status.success());

    let deadline = Instant::now() + Duration::from_secs(10);
    while is_alive(grandchild) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!is_alive(child), "the child is gone after cancel");
    assert!(!is_alive(grandchild), "the grandchild is gone after cancel");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_background_slot_is_released_after_drop() {
    let sandbox = sandbox(ResourceProfile::Practical);
    let first = expect_spawned(sandbox.spawn_pty_stream(None, sleep_command(), PtySize::default()));

    match expect_failed(sandbox.spawn_pty_stream(None, sleep_command(), PtySize::default())) {
        PtyError::Sandbox(SandboxError::ResourceDenied(_)) => {}
        other => panic!("a second spawn under one slot must be denied, got {other}"),
    }

    drop(first);
    let mut third =
        expect_spawned(sandbox.spawn_pty_stream(None, sleep_command(), PtySize::default()));
    cancel(&mut third).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_a_tokio_runtime_the_spawn_is_an_io_error_and_takes_no_slot() {
    let sandbox = sandbox(ResourceProfile::Practical);

    let error = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                expect_failed(sandbox.spawn_pty_stream(None, sleep_command(), PtySize::default()))
            })
            .join()
            .unwrap()
    });
    assert!(
        matches!(error, PtyError::Io(_)),
        "no runtime must be PtyError::Io, got {error}"
    );

    // The one slot is still free.
    let mut streamed =
        expect_spawned(sandbox.spawn_pty_stream(None, sleep_command(), PtySize::default()));
    cancel(&mut streamed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pixel_size_survives_spawn_and_resize() {
    let sandbox = sandbox(ResourceProfile::Practical);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        None,
        SandboxCommand::new("sleep").arg("30"),
        PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 800,
            pixel_height: 600,
        },
    ));
    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 800,
            pixel_height: 600
        }
    );

    streamed
        .resize(PtySize::new(40, 120).with_pixels(1024, 768))
        .unwrap();
    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 1024,
            pixel_height: 768
        }
    );

    // A later size without pixels clears them.
    streamed.resize(PtySize::new(10, 20)).unwrap();
    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 10,
            cols: 20,
            pixel_width: 0,
            pixel_height: 0
        }
    );

    cancel(&mut streamed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn default_and_clamped_sizes_reach_the_kernel() {
    let sandbox = sandbox(ResourceProfile::Expanded);

    let mut default =
        expect_spawned(sandbox.spawn_pty_stream(None, sleep_command(), PtySize::default()));
    assert_eq!(
        default.winsize().unwrap(),
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0
        }
    );
    cancel(&mut default).await;

    // The max(1) clamp is on rows and cols only; pixels are copied verbatim.
    let mut clamped = expect_spawned(sandbox.spawn_pty_stream(
        None,
        sleep_command(),
        PtySize {
            rows: 0,
            cols: 0,
            pixel_width: 5,
            pixel_height: 7,
        },
    ));
    assert_eq!(
        clamped.winsize().unwrap(),
        PtySize {
            rows: 1,
            cols: 1,
            pixel_width: 5,
            pixel_height: 7
        }
    );
    cancel(&mut clamped).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pixel_size_survives_a_confined_spawn() {
    if !confinement_available("pixel_size_survives_a_confined_spawn") {
        return;
    }
    let root = Scratch::new("confined-size");
    let policy = confined_policy(root.path());
    let sandbox = sandbox(ResourceProfile::Practical);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        SandboxCommand::new("sleep").arg("30"),
        PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 800,
            pixel_height: 600,
        },
    ));
    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 800,
            pixel_height: 600
        }
    );

    streamed
        .resize(PtySize::new(40, 120).with_pixels(1024, 768))
        .unwrap();
    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 1024,
            pixel_height: 768
        }
    );

    cancel(&mut streamed).await;
}
