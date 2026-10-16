//! Standalone single instance: a second launch in a separate process forwards its typed message to
//! the live primary; the primary's guard releases on drop; malformed payloads are refused; and an
//! arbitration that cannot be decided is `Failed`, never a silent second primary.
//!
//! The secondary is this test binary re-executed with `--exact child_second_launch`, because
//! arbitration checks process identity (the same approach as the crate's `instance` tests).

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use rightkit_daemon::{Client, Suite};
use rightkit_tauri_shell::single_instance::MESSAGE_METHOD;
use rightkit_tauri_shell::{ClaimFailure, ClaimOutcome, InstanceMessage, SingleInstance};
use serde_json::json;

const ID: &str = "test-shell";
const ROOT_ENV: &str = "RK_SINGLE_INSTANCE_TEST_ROOT";

/// The claim's result as text, so a failed assertion names the branch that was taken.
fn describe(outcome: &ClaimOutcome) -> String {
    match outcome {
        ClaimOutcome::Primary(_) => "primary".to_owned(),
        ClaimOutcome::Forwarded => "forwarded".to_owned(),
        ClaimOutcome::Failed(failure) => format!("failed: {failure}"),
    }
}

#[test]
fn second_launch_forwards_its_typed_message_to_the_primary() {
    let suite = Suite::per_run_temp("shell-single-instance").unwrap();
    let (tx, rx) = mpsc::channel();
    let primary = match SingleInstance::claim_in(
        &suite,
        ID,
        InstanceMessage::from_process(),
        move |message| {
            let _ = tx.send(message);
        },
    ) {
        ClaimOutcome::Primary(guard) => guard,
        _ => panic!("the first claim must be the primary"),
    };

    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_second_launch", "--nocapture"])
        .env(ROOT_ENV, suite.root())
        .status()
        .unwrap();
    assert!(
        child.success(),
        "the secondary launch must forward and exit cleanly"
    );

    let message = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(message.action.as_deref(), Some("show-section"));
    assert_eq!(message.args.get("section"), Some(&json!("x")));
    assert!(message.argv.iter().any(|a| a == "child_second_launch"));
    assert_eq!(
        message.cwd,
        std::env::current_dir().unwrap().to_string_lossy()
    );
    // The callback ran once for the one forwarded launch.
    assert!(rx.try_recv().is_err());

    // Dropping the primary releases the claim: the next launch becomes the primary.
    drop(primary);
    assert!(SingleInstance::claim_in(&suite, ID, InstanceMessage::default(), |_| {}).is_primary());
}

#[test]
fn a_malformed_forwarded_payload_is_refused_and_never_reaches_the_callback() {
    let suite = Suite::per_run_temp("shell-single-instance-bad").unwrap();
    let (tx, rx) = mpsc::channel();
    let _primary =
        match SingleInstance::claim_in(&suite, ID, InstanceMessage::default(), move |m| {
            let _ = tx.send(m);
        }) {
            ClaimOutcome::Primary(guard) => guard,
            _ => panic!("the first claim must be the primary"),
        };

    let client = Client::connect(&suite, &format!("{ID}-instance"), ID).unwrap();
    // `argv` must be a list and `args` must be an object.
    assert!(client
        .call(
            MESSAGE_METHOD,
            json!({"argv": "not-a-list", "cwd": "", "args": {}})
        )
        .is_err());
    assert!(client
        .call(
            MESSAGE_METHOD,
            json!({"argv": [], "cwd": "", "args": ["not", "an", "object"]})
        )
        .is_err());
    assert!(
        rx.try_recv().is_err(),
        "a refused payload must not run the callback"
    );

    // A well-formed payload is accepted on the same channel.
    assert!(client
        .call(
            MESSAGE_METHOD,
            json!({"argv": [], "cwd": "", "action": "noop", "args": {}})
        )
        .is_ok());
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap()
            .action
            .as_deref(),
        Some("noop")
    );
}

#[test]
fn an_undecidable_claim_fails_closed_instead_of_becoming_a_primary() {
    // A root that is a regular file cannot host the endpoint: the claim must be Failed, not Primary.
    let dir = tempfile::tempdir().unwrap();
    let file_root = dir.path().join("not-a-directory");
    std::fs::write(&file_root, b"x").unwrap();
    let suite = match Suite::at(&file_root) {
        Ok(suite) => suite,
        // Some platforms refuse the root up front; the claim cannot be attempted at all, which is
        // the same fail-closed outcome for the caller.
        Err(_) => return,
    };
    let outcome = SingleInstance::claim_in(&suite, ID, InstanceMessage::default(), |_| {});
    assert!(
        matches!(outcome, ClaimOutcome::Failed(_)),
        "an undecidable claim must be Failed"
    );
}

const DOTTED: &str = "app.mailright";
const DOTTED_ROOT_ENV: &str = "RK_SINGLE_INSTANCE_DOTTED_ROOT";

#[test]
fn a_dotted_bundle_id_is_normalised_and_the_second_launch_is_forwarded() {
    let suite = Suite::per_run_temp("shell-single-instance-dotted").unwrap();
    let (tx, rx) = mpsc::channel();
    let primary =
        match SingleInstance::claim_in(&suite, DOTTED, InstanceMessage::default(), move |m| {
            let _ = tx.send(m);
        }) {
            ClaimOutcome::Primary(guard) => guard,
            other => panic!(
                "the first claim must be the primary, got {}",
                describe(&other)
            ),
        };
    // The service is named from the normalised id.
    assert!(Client::connect(&suite, "app-mailright-instance", "app-mailright").is_ok());
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_dotted_second_launch", "--nocapture"])
        .env(DOTTED_ROOT_ENV, suite.root())
        .status()
        .unwrap();
    assert!(child.success(), "the dotted secondary must be forwarded");
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(primary);
}

#[test]
fn child_dotted_second_launch() {
    let Some(root) = std::env::var_os(DOTTED_ROOT_ENV) else {
        return;
    };
    let suite = Suite::at(std::path::PathBuf::from(root)).unwrap();
    let outcome = SingleInstance::claim_in(
        &suite,
        "App.MailRight",
        InstanceMessage::from_process(),
        |_| panic!("the secondary callback must not run"),
    );
    assert!(
        matches!(outcome, ClaimOutcome::Forwarded),
        "got {}",
        describe(&outcome)
    );
}

#[test]
fn an_id_that_cannot_name_a_service_is_a_typed_failure() {
    let suite = Suite::per_run_temp("shell-single-instance-invalid").unwrap();
    let long = "a".repeat(40);
    for id in ["", long.as_str()] {
        let outcome = SingleInstance::claim_in(&suite, id, InstanceMessage::default(), |_| {});
        assert!(
            matches!(outcome, ClaimOutcome::Failed(ClaimFailure::InvalidId(_))),
            "id {id:?}: got {}",
            describe(&outcome)
        );
    }
    // The longest legal id (39 bytes -> a 48-byte service name) must bind on every host: the
    // socket fallback is a bounded name, so no $TMPDIR the machine has can push it over the
    // `sockaddr_un.sun_path` limit.
    let longest = "a".repeat(39);
    let outcome = SingleInstance::claim_in(&suite, &longest, InstanceMessage::default(), |_| {});
    assert!(
        outcome.is_primary(),
        "a 39-byte id must be primary: got {}",
        describe(&outcome)
    );
}

/// Secondary-process entry point. Does nothing unless the parent set the suite root.
#[test]
fn child_second_launch() {
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        return;
    };
    let suite = Suite::at(std::path::PathBuf::from(root)).unwrap();
    let message = InstanceMessage::from_process()
        .with_action("show-section")
        .with_arg("section", "x");
    match SingleInstance::claim_in(&suite, ID, message, |_| {
        panic!("the secondary launch must not run the primary's callback")
    }) {
        ClaimOutcome::Forwarded => {}
        ClaimOutcome::Primary(_) => panic!("the secondary launch became a primary"),
        ClaimOutcome::Failed(reason) => panic!("the secondary launch failed closed: {reason}"),
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_live_legacy_holder_receives_the_launch_and_a_stale_socket_is_ignored() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    use rightkit_tauri_shell::ClaimOptions;

    // Unique per run so no other socket in /tmp can share the stem (or be matched by its prefix scan).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() % 1_000_000_000)
        .unwrap_or(0);
    let identifier = format!("rk.legacy-test-{}-{nanos}", std::process::id());
    let stem = identifier.replace(['.', '-'], "_");
    let socket = std::path::PathBuf::from(format!("/tmp/{stem}_si.sock"));
    let _ = std::fs::remove_file(&socket);

    // A live legacy holder: a listener that reads one launch to end-of-stream.
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut payload = Vec::new();
            let _ = stream.read_to_end(&mut payload);
            let _ = tx.send(payload);
        }
        // `listener` drops here, closing the live holder's socket.
    });
    let suite = Suite::per_run_temp("shell-legacy-live").unwrap();
    let options = ClaimOptions::with_legacy(&identifier);
    let outcome =
        SingleInstance::claim_in_with(&suite, ID, options, InstanceMessage::from_process(), |_| {
            panic!("the launch must go to the legacy holder, not to a new primary")
        });
    assert!(
        matches!(outcome, ClaimOutcome::Forwarded),
        "a live legacy holder must receive the launch"
    );
    let payload = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // The plugin's format: cwd, two NULs, then the argv joined by NUL.
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let text = String::from_utf8_lossy(&payload).into_owned();
    assert!(text.contains("\0\0"), "payload: {text:?}");
    assert!(text.contains(&exe), "payload: {text:?}");
    // The live holder is fully gone (thread joined, listener closed) before the stale half starts.
    holder.join().unwrap();
    let _ = std::fs::remove_file(&socket);

    // A stale socket file (nothing listening) is not a live holder: this launch becomes primary.
    //
    // The stale file is made by a helper process that binds the path and exits, never by
    // `UnixListener::bind` + drop in this process. std creates the socket and sets close-on-exec in a
    // second call (macOS has no atomic form), and the other tests in this binary re-execute this
    // binary: a fork+exec on another thread inside that window inherits the listening descriptor,
    // and the path keeps accepting connections after this process closed its own copy. macOS CI
    // showed exactly that: `connect` returned `Ok(UnixStream)` where the precondition below wants a
    // refusal. The helper process has no children, so nothing can hold a copy of its descriptor.
    let stale_id = format!("{identifier}-stale");
    let stale_socket = legacy_fixture::socket(&stale_id);
    let _ = std::fs::remove_file(&stale_socket);
    legacy_fixture::run(&stale_socket, "stale");
    // Precondition: nothing answers on the stale path. If this fails the fixture is wrong, not the claim.
    match std::os::unix::net::UnixStream::connect(&stale_socket) {
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
        other => panic!(
            "the stale socket {} must refuse connections, got {other:?}",
            stale_socket.display()
        ),
    }
    let suite = Suite::per_run_temp("shell-legacy-stale").unwrap();
    let outcome = SingleInstance::claim_in_with(
        &suite,
        ID,
        ClaimOptions::with_legacy(&stale_id),
        InstanceMessage::default(),
        |_| {},
    );
    assert!(
        outcome.is_primary(),
        "a stale legacy socket must not block the claim, got {}",
        describe(&outcome)
    );
    let _ = std::fs::remove_file(&stale_socket);
}

/// Fixtures for the macOS legacy-socket cases. Each socket is made by a helper process (this binary
/// re-executed, see `child_legacy_fixture`), so no descriptor of it ever exists in this process and
/// no fork of this process can inherit one.
#[cfg(target_os = "macos")]
mod legacy_fixture {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub const SOCKET_ENV: &str = "RK_LEGACY_FIXTURE_SOCKET";
    pub const MODE_ENV: &str = "RK_LEGACY_FIXTURE_MODE";
    pub const SIDECAR_PID_PREFIX: &str = "rk-legacy-fixture-sidecar-pid=";

    /// A legacy identifier whose socket stem no other socket in `/tmp` can share or match by the
    /// prefix scan.
    pub fn identifier(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() % 1_000_000_000)
            .unwrap_or(0);
        format!("rk.legacy-{tag}-{}-{nanos}", std::process::id())
    }

    /// The plugin's socket for `identifier`: `/tmp/<identifier with . and - as _>_si.sock`.
    pub fn socket(identifier: &str) -> PathBuf {
        PathBuf::from(format!(
            "/tmp/{}_si.sock",
            identifier.replace(['.', '-'], "_")
        ))
    }

    /// Run the helper process in `mode` ("stale": bind the path and exit; "leak": bind it, hand the
    /// listening descriptor to a sidecar and exit) and wait for it. Returns its stdout.
    pub fn run(socket: &Path, mode: &str) -> String {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_legacy_fixture", "--nocapture"])
            .env(SOCKET_ENV, socket)
            .env(MODE_ENV, mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "the {mode} fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The pid the "leak" helper reported for its sidecar.
    pub fn sidecar_pid(stdout: &str) -> u32 {
        stdout
            .split(SIDECAR_PID_PREFIX)
            .nth(1)
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .and_then(|digits| digits.parse().ok())
            .unwrap_or_else(|| panic!("the fixture did not report its sidecar: {stdout:?}"))
    }

    /// The socket file and the sidecar a fixture leaves behind, removed even when an assertion fails.
    pub struct Leftovers {
        pub socket: PathBuf,
        pub sidecar: Option<u32>,
    }

    impl Leftovers {
        pub fn new(socket: PathBuf) -> Self {
            let _ = std::fs::remove_file(&socket);
            Self {
                socket,
                sidecar: None,
            }
        }
    }

    impl Drop for Leftovers {
        fn drop(&mut self) {
            if let Some(pid) = self.sidecar {
                let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
            }
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

/// Helper-process entry point for `legacy_fixture::run`. Does nothing unless the parent set the
/// socket path.
#[cfg(target_os = "macos")]
#[allow(clippy::zombie_processes)]
#[test]
fn child_legacy_fixture() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixListener;
    use std::process::Stdio;

    let Some(socket) = std::env::var_os(legacy_fixture::SOCKET_ENV) else {
        return;
    };
    let listener = UnixListener::bind(&socket).unwrap();
    match std::env::var(legacy_fixture::MODE_ENV).as_deref() {
        // The listener closes with this process: the path stays, nothing answers on it.
        Ok("stale") => drop(listener),
        // A sidecar inherits the listening descriptor (as its stdin) and outlives this process: what
        // a descriptor leaked into a child looks like once the app that created it has exited.
        Ok("leak") => {
            let sidecar = Command::new("/bin/sleep")
                .arg("120")
                .stdin(Stdio::from(OwnedFd::from(listener)))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("{}{}", legacy_fixture::SIDECAR_PID_PREFIX, sidecar.id());
            // Neither waited for nor killed: it must outlive this process.
            std::mem::forget(sidecar);
        }
        other => panic!("unknown fixture mode {other:?}"),
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_listener_kept_open_by_a_sidecar_after_its_owner_exits_is_not_a_live_holder() {
    use rightkit_tauri_shell::ClaimOptions;

    let identifier = legacy_fixture::identifier("leak");
    let mut leftovers = legacy_fixture::Leftovers::new(legacy_fixture::socket(&identifier));
    // The "app" creates the listening socket, gives a sidecar a copy of the descriptor and exits.
    // It has been reaped when `run` returns, so its pid names nothing, while the sidecar lives on.
    let stdout = legacy_fixture::run(&leftovers.socket, "leak");
    leftovers.sidecar = Some(legacy_fixture::sidecar_pid(&stdout));
    // Precondition: the socket still accepts connections, held open by the sidecar alone. If this
    // fails the fixture is wrong, not the claim.
    drop(
        std::os::unix::net::UnixStream::connect(&leftovers.socket)
            .expect("the sidecar must keep the listener accepting after its creator exited"),
    );

    // A launch written to that socket would wait in a queue nobody reads and the app would never
    // open: the claim must see that nothing serves it and become the primary.
    let suite = Suite::per_run_temp("shell-legacy-leak").unwrap();
    let outcome = SingleInstance::claim_in_with(
        &suite,
        ID,
        ClaimOptions::with_legacy(&identifier),
        InstanceMessage::default(),
        |_| {},
    );
    assert!(
        outcome.is_primary(),
        "a listener only a sidecar keeps open is not a live holder, got {}",
        describe(&outcome)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_leaked_listener_does_not_stop_the_hold_from_taking_over_the_legacy_socket() {
    use std::io::Write;

    use rightkit_tauri_shell::ClaimOptions;

    let identifier = legacy_fixture::identifier("holdleak");
    let mut leftovers = legacy_fixture::Leftovers::new(legacy_fixture::socket(&identifier));
    let stdout = legacy_fixture::run(&leftovers.socket, "leak");
    leftovers.sidecar = Some(legacy_fixture::sidecar_pid(&stdout));

    let suite = Suite::per_run_temp("shell-legacy-holdleak").unwrap();
    let (tx, rx) = mpsc::channel();
    let outcome = SingleInstance::claim_in_with(
        &suite,
        ID,
        ClaimOptions::with_legacy(&identifier).holding_legacy(),
        InstanceMessage::default(),
        move |m| {
            let _ = tx.send(m);
        },
    );
    assert!(
        outcome.is_primary(),
        "the hold must replace a leaked listener, got {}",
        describe(&outcome)
    );

    // An old build now finds this process serving the legacy path and its launch arrives.
    {
        let mut old = std::os::unix::net::UnixStream::connect(&leftovers.socket).unwrap();
        old.write_all(b"/work\0\0/old\0--x").unwrap();
    }
    let message = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(message.cwd, "/work");
    assert_eq!(message.argv, ["/old", "--x"]);
}
