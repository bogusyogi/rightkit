//! `Sandbox::spawn_pty_stream` on a Windows host: an AppContainer child behind a ConPTY
//! pseudoconsole, created in one `CreateProcessW`. Real AppContainer, real console; nothing is
//! mocked. Every read of the stream is bounded by a 60 s timeout.

#![cfg(all(windows, feature = "pty"))]

use rightkit_sandbox::{
    remove_unused_appcontainer_profiles, AppContainerProfile, EnvPolicy, PtyError, PtyReader,
    PtySize, ResourceAuthority, ResourceProfile, Sandbox, SandboxCommand, SandboxError,
    SandboxPolicy, StreamedPty, TerminationOptions,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;

const READ_LIMIT: Duration = Duration::from_secs(60);

/// Serializes the tests of this file: the profile-sweep check in the last test reads the whole
/// per-user profile set, so no sibling test may create a profile at the same time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A unique canonical scratch directory under the temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rk-conpty-{name}-{}-{nanos}",
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

fn cmd_command(scratch: &Path, args: &[&str]) -> SandboxCommand {
    SandboxCommand {
        program: "cmd.exe".into(),
        args: args.iter().map(|a| (*a).into()).collect(),
        cwd: Some(scratch.to_path_buf()),
        env: vec![],
    }
}

fn write_policy(scratch: &Path) -> SandboxPolicy {
    SandboxPolicy::new().allow_write(scratch)
}

fn sandbox(profile: ResourceProfile) -> Sandbox {
    Sandbox::new(ResourceAuthority::new(profile))
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
    let deadline = Instant::now() + READ_LIMIT;
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

/// Reads until end of file, with each read bounded by the 60 s timeout.
async fn read_to_eof(reader: &mut PtyReader) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = tokio::time::timeout(READ_LIMIT, reader.read(&mut chunk))
            .await
            .expect("a read of the stream timed out")
            .unwrap_or(0);
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

/// Runs the child to its end: reads the stream to EOF while `wait()` observes the exit (which is
/// what closes the pseudoconsole), and returns the text and the success flag.
async fn run_to_end(streamed: &mut StreamedPty, reader: &mut PtyReader) -> (String, bool) {
    let (bytes, status) = tokio::join!(read_to_eof(reader), streamed.wait());
    (
        String::from_utf8_lossy(&bytes).into_owned(),
        status.expect("wait").success(),
    )
}

async fn cancel(streamed: &mut StreamedPty) {
    streamed
        .cancel(TerminationOptions::default())
        .await
        .expect("cancel must end the child");
}

#[tokio::test(flavor = "multi_thread")]
async fn confined_conpty_echo() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("echo");
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Expanded);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(scratch.path(), &["/c", "echo", "rk-conpty-ok"]),
        PtySize::default(),
    ));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");

    let text = read_until(&mut reader, "rk-conpty-ok").await;
    assert!(text.contains("rk-conpty-ok"));

    let status = tokio::time::timeout(READ_LIMIT, streamed.wait())
        .await
        .expect("wait timed out")
        .unwrap();
    assert!(status.success(), "cmd.exe /c echo must succeed: {status:?}");

    // The pseudoconsole closed after exit, so the stream reaches end of file.
    let rest = read_to_eof(&mut reader).await;
    drop(rest);
}

#[tokio::test(flavor = "multi_thread")]
async fn confined_conpty_is_really_confined() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("confined");
    let other = Scratch::new("other");
    let secret = other.path().join("secret.txt");
    fs::write(&secret, "TOP-SECRET-rk").unwrap();
    let secret_arg = secret.to_string_lossy().into_owned();

    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Expanded);

    let mut confined = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(scratch.path(), &["/c", "type", &secret_arg]),
        PtySize::default(),
    ));
    let mut reader = confined.take_reader().unwrap().expect("a reader");
    let (text, _) = run_to_end(&mut confined, &mut reader).await;
    assert!(
        text.contains("Access is denied"),
        "the AppContainer child must be denied: {text:?}"
    );
    assert!(!text.contains("TOP-SECRET-rk"), "{text:?}");

    // Unconfined control: the same command reads the secret through the same kind of console.
    let mut control = expect_spawned(sandbox.spawn_pty_stream(
        None,
        cmd_command(scratch.path(), &["/c", "type", &secret_arg]),
        PtySize::default(),
    ));
    let mut reader = control.take_reader().unwrap().expect("a reader");
    let (text, _) = run_to_end(&mut control, &mut reader).await;
    assert!(
        text.contains("TOP-SECRET-rk"),
        "the unconfined control must read the secret: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn conpty_stream_is_lossless_and_has_backpressure() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("lossless");
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Expanded);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(
            scratch.path(),
            &["/c", "for /L %i in (1,1,20000) do @echo line-%i"],
        ),
        PtySize::default(),
    ));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");

    // Nobody reads for 2 s: the producer must block on the 64 KiB bridge, not drop output.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let (text, _) = run_to_end(&mut streamed, &mut reader).await;
    assert!(
        text.contains("line-20000"),
        "the last line must arrive: tail {:?}",
        &text[text.len().saturating_sub(200)..]
    );
    assert!(
        text.len() > 150_000,
        "no ring cap: expected more than 150000 bytes, got {}",
        text.len()
    );
}

/// Names of the processes below `root` (its children, theirs, and so on), taken from one
/// snapshot of the process table. A process whose parent already exited still lists that parent
/// id, so descendants orphaned by a kill are found too.
fn descendant_names(root: u32) -> Vec<String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Csv -NoTypeInformation",
        ])
        .output()
        .expect("powershell must list the processes");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let rows: Vec<(u32, u32, String)> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.trim().trim_matches('"').split("\",\"").collect();
            if fields.len() != 3 {
                return None;
            }
            Some((
                fields[0].parse().ok()?,
                fields[1].parse().ok()?,
                fields[2].to_string(),
            ))
        })
        .collect();
    let mut tree = vec![root];
    let mut names = Vec::new();
    let mut grew = true;
    while grew {
        grew = false;
        for (pid, parent, name) in &rows {
            if tree.contains(parent) && !tree.contains(pid) {
                tree.push(*pid);
                names.push(name.clone());
                grew = true;
            }
        }
    }
    names
}

fn has_powershell(names: &[String]) -> bool {
    names
        .iter()
        .any(|name| name.eq_ignore_ascii_case("powershell.exe"))
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_kills_the_whole_confined_tree() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("cancel");
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Expanded);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(
            scratch.path(),
            &[
                "/c",
                "start /b powershell -NoProfile -Command Start-Sleep 60 & powershell -NoProfile -Command Start-Sleep 60",
            ],
        ),
        PtySize::default(),
    ));
    let _reader = streamed.take_reader().unwrap().expect("a reader");
    let child = streamed.id();

    // Wait until the tree really holds a powershell, so the check after cancel means something.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !has_powershell(&descendant_names(child)) {
        assert!(
            Instant::now() < deadline,
            "no powershell appeared below the child"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let status = tokio::time::timeout(
        Duration::from_secs(10),
        streamed.cancel(TerminationOptions::default()),
    )
    .await
    .expect("cancel must return within 10 s")
    .expect("cancel must return Ok(status)");
    assert!(!status.success(), "a cancelled tree does not succeed");

    let left = descendant_names(child);
    assert!(
        !has_powershell(&left),
        "no powershell may survive below the child: {left:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slot_released_on_drop() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("slot");
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Practical);
    let hold = || cmd_command(scratch.path(), &["/k"]);

    let first = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        hold(),
        PtySize::default(),
    ));

    match expect_failed(sandbox.spawn_pty_stream(Some(&policy), hold(), PtySize::default())) {
        PtyError::Sandbox(SandboxError::ResourceDenied(_)) => {}
        other => panic!("a second spawn under one slot must be denied, got {other}"),
    }

    drop(first);
    let mut third = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        hold(),
        PtySize::default(),
    ));
    cancel(&mut third).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_read_is_refused_before_any_process() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("deny");
    let sandbox = sandbox(ResourceProfile::Practical);
    let denying = write_policy(scratch.path()).deny_read(scratch.path().join("x"));

    match expect_failed(sandbox.spawn_pty_stream(
        Some(&denying),
        cmd_command(scratch.path(), &["/k"]),
        PtySize::default(),
    )) {
        PtyError::Sandbox(SandboxError::Unsupported(_)) => {}
        other => panic!("a deny_read policy must be Unsupported, got {other}"),
    }

    // No slot leaked: the single Practical slot is still free for the next spawn.
    let policy = write_policy(scratch.path());
    let mut next = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(scratch.path(), &["/k"]),
        PtySize::default(),
    ));
    cancel(&mut next).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_entry_point() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("profile");
    let policy = write_policy(scratch.path());
    let profile = AppContainerProfile::for_policy(&policy).expect("the container profile");
    let sandbox = sandbox(ResourceProfile::Expanded);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream_in_profile(
        &profile,
        cmd_command(scratch.path(), &["/c", "echo", "rk-conpty-ok"]),
        &EnvPolicy::default(),
        PtySize::default(),
    ));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");

    let text = read_until(&mut reader, "rk-conpty-ok").await;
    assert!(text.contains("rk-conpty-ok"));
    let status = tokio::time::timeout(READ_LIMIT, streamed.wait())
        .await
        .expect("wait timed out")
        .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_and_input() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("resize");
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Expanded);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(scratch.path(), &[]),
        PtySize::default(),
    ));
    let mut reader = streamed.take_reader().unwrap().expect("a reader");
    let mut writer = streamed.take_writer().unwrap().expect("a writer");

    writer.write_input(b"echo hi\r\n").await.unwrap();
    let text = read_until(&mut reader, "hi").await;
    assert!(text.contains("hi"));
    // `^-` makes the typed text differ from the output, so this proves cmd.exe ran the line.
    writer.write_input(b"echo rk^-ok\r\n").await.unwrap();
    let text = read_until(&mut reader, "rk-ok").await;
    assert!(text.contains("rk-ok"));

    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0
        }
    );
    streamed
        .resize(PtySize::new(40, 120).with_pixels(800, 600))
        .unwrap();
    // The Windows side accepts the pixels and ignores them, so reading back reports 0.
    assert_eq!(
        streamed.winsize().unwrap(),
        PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0
        }
    );

    cancel(&mut streamed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn conpty_accepts_and_ignores_pixel_size() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("pixels");
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Expanded);
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(scratch.path(), &["/k"]),
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
            pixel_width: 0,
            pixel_height: 0
        }
    );
    cancel(&mut streamed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_a_tokio_runtime_no_profile_is_created() {
    let _serial = SERIAL.lock().await;
    let scratch = Scratch::new("no-runtime");
    // A grant set no other test uses, so the profile it would create is unique to this test.
    let policy = write_policy(scratch.path());
    let sandbox = sandbox(ResourceProfile::Practical);

    // Drop every unused profile first; a profile this spawn created would be unused afterwards.
    remove_unused_appcontainer_profiles().expect("the first sweep");

    let error = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                expect_failed(sandbox.spawn_pty_stream(
                    Some(&policy),
                    cmd_command(scratch.path(), &["/k"]),
                    PtySize::default(),
                ))
            })
            .join()
            .unwrap()
    });
    assert!(
        matches!(error, PtyError::Io(_)),
        "no runtime must be PtyError::Io, got {error}"
    );

    let report = remove_unused_appcontainer_profiles().expect("the second sweep");
    assert!(
        report.removed.is_empty(),
        "the failed spawn must not have created a profile: {report:?}"
    );

    // No slot leaked either.
    let mut streamed = expect_spawned(sandbox.spawn_pty_stream(
        Some(&policy),
        cmd_command(scratch.path(), &["/k"]),
        PtySize::default(),
    ));
    cancel(&mut streamed).await;
}
