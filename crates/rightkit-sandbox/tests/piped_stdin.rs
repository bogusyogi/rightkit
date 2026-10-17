//! `Sandbox::spawn_piped`: a confined child fed through its stdin pipe (Unix; Windows pipes are
//! created per AppContainer and exercised by the CI-gated Windows tests).
#![cfg(unix)]

use rightkit_sandbox::*;
use std::io::Write;
use std::time::Duration;

fn sandbox() -> Sandbox {
    Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded))
}

#[test]
fn input_written_to_the_pipe_reaches_the_child_and_closing_it_ends_the_child() {
    if cfg!(target_os = "linux") && !is_landlock_available() {
        println!("skipped: no Landlock on this host, so no confined spawn");
        return;
    }
    let policy = SandboxPolicy::new().network(NetworkPolicy::Allow);
    let mut child = sandbox()
        .spawn_piped(&policy, SandboxCommand::new("/bin/cat"))
        .expect("spawn");
    let mut stdin = child.take_stdin().expect("a piped child has stdin");
    assert!(
        child.take_stdin().is_none(),
        "the write end is handed out once"
    );
    stdin.write_all(b"fed-through-the-pipe\n").unwrap();
    // End of input: `cat` exits only once the pipe closes.
    drop(stdin);
    let out = child.wait_with_output(Duration::from_secs(20)).unwrap();
    assert!(!out.timed_out, "cat must see end-of-file");
    assert_eq!(out.stdout, b"fed-through-the-pipe\n");
}

#[test]
fn a_plain_spawn_has_no_stdin_pipe() {
    if cfg!(target_os = "linux") && !is_landlock_available() {
        println!("skipped: no Landlock on this host, so no confined spawn");
        return;
    }
    let policy = SandboxPolicy::new().network(NetworkPolicy::Allow);
    let mut child = sandbox()
        .spawn(&policy, SandboxCommand::new("/bin/cat"))
        .expect("spawn");
    assert!(child.take_stdin().is_none());
    // stdin is the null device, so `cat` ends at once.
    let out = child.wait_with_output(Duration::from_secs(20)).unwrap();
    assert!(!out.timed_out);
    assert!(out.stdout.is_empty());
}

#[test]
fn a_supervised_child_has_no_stdin_pipe_and_no_loopback_exemption() {
    let mut child = sandbox()
        .spawn_supervised(
            SandboxCommand::new("/bin/echo").arg("x"),
            &EnvPolicy::default(),
        )
        .unwrap();
    assert!(child.take_stdin().is_none());
    assert!(!child.loopback_exempt());
    let _ = child.wait_with_output(Duration::from_secs(10)).unwrap();
}
