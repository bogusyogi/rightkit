//! Standalone single instance: a second launch in a separate process forwards its typed message to
//! the live primary; the primary's guard releases on drop; malformed payloads are refused; and an
//! arbitration that cannot be decided is `Failed`, never a silent second primary.
//!
//! The secondary is this test binary re-executed with `--exact child_second_launch`, because
//! arbitration checks process identity (the same approach as the crate's `instance` tests).

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use rightkit_service::{Client, Suite};
use rightkit_shell::single_instance::MESSAGE_METHOD;
use rightkit_shell::{ClaimOutcome, InstanceMessage, SingleInstance};
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

    use rightkit_shell::ClaimOptions;

    let identifier = format!("rk.legacy-test-{}", std::process::id());
    let stem = identifier.replace(['.', '-'], "_");
    let socket = std::path::PathBuf::from(format!("/tmp/{stem}_si.sock"));
    let _ = std::fs::remove_file(&socket);

    // A live legacy holder: a listener that reads one launch to end-of-stream.
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut payload = Vec::new();
            let _ = stream.read_to_end(&mut payload);
            let _ = tx.send(payload);
        }
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
    let _ = std::fs::remove_file(&socket);

    // A stale socket file (nothing listening) is not a live holder: this launch becomes primary.
    let stale_id = format!("{identifier}-stale");
    let stale_stem = stale_id.replace(['.', '-'], "_");
    let stale_socket = std::path::PathBuf::from(format!("/tmp/{stale_stem}_si.sock"));
    let _ = std::fs::remove_file(&stale_socket);
    drop(UnixListener::bind(&stale_socket).unwrap());
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
