//! Shell journeys through the REAL `coderight run` binary: exit codes (including non-1 codes),
//! stdout/stderr shape, destructive guard, timeout with process-tree kill, live output while a
//! command is still running, background processes (poll / kill) and a Windows-only PowerShell
//! tool check. Commands are written so they behave identically in `sh` and PowerShell.
//! Retires the unit tests of `shell`, `background_shell`, `output_compaction`
//! result shaping and the destructive-command guard.

use crate::support::e2e_tools as t;
use std::time::{Duration, Instant};
use t::*;

fn shell(cmd: &str) -> Step {
    call("shell", json!({ "command": cmd }))
}

#[test]
fn exit_codes_streams_and_the_destructive_guard() {
    let p = sequence(
        vec![
            shell("echo hello-out"),
            shell("echo partial; exit 3"),
            shell("exit 42"),
            shell(&stderr_cmd("to-stderr")),
            shell("mkfs.ext4 /nonexistent-device-e2e"),
            call("shell", json!({"command": ""})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 6);

    assert!(!r[0].error, "{}", r[0].text);
    assert_eq!(r[0].text.trim(), "hello-out");

    assert!(r[1].error);
    assert!(r[1].text.starts_with("[exit 3]\n"), "{:?}", r[1].text);
    assert!(r[1].text.contains("partial"), "{:?}", r[1].text);

    assert!(r[2].error);
    assert!(r[2].text.starts_with("[exit 42]\n"), "{:?}", r[2].text);

    assert!(!r[3].error, "{}", r[3].text);
    assert!(r[3].text.contains("[stderr]\nto-stderr"), "{:?}", r[3].text);

    assert!(r[4].error);
    assert_eq!(
        r[4].text,
        "Blocked by destructive guard (filesystem wipe (mkfs/wipefs)): this command is refused in any mode."
    );

    assert!(r[5].error);
    assert_eq!(
        r[5].text,
        "invalid_request: shell command is empty or contains NUL"
    );
}

#[test]
fn commands_run_in_the_workspace_with_git_on_path() {
    let p = sequence(
        vec![
            shell("git --version"),
            shell("echo cwd-proof > cwd-proof.txt"),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 2);
    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text.trim().starts_with("git version "),
        "{:?}",
        r[0].text
    );
    assert!(!r[1].error, "{}", r[1].text);
    assert!(
        rig.exists("cwd-proof.txt"),
        "command must run with the session root as cwd"
    );
}

#[test]
fn timeout_kills_the_command_and_it_never_finishes_its_work() {
    let p = sequence(
        vec![
            call(
                "shell",
                json!({"command":"sleep 6; echo late > late.txt","timeout_ms":1500}),
            ),
            // Outlive the original command: had it survived it would have written late.txt.
            shell(&sleep_cmd(9)),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let started = Instant::now();
    let out = rig.run("go");
    let r = expect_results(&out, 2);
    assert!(r[0].error);
    assert!(
        r[0].text
            .starts_with("Command timed out after 1500ms: sleep 6; echo late > late.txt (pid "),
        "{:?}",
        r[0].text
    );
    assert!(!r[1].error, "{}", r[1].text);
    assert!(
        !rig.exists("late.txt"),
        "timed-out command must have been killed"
    );
    assert!(started.elapsed() < Duration::from_secs(100));
}

#[test]
fn background_process_can_be_polled_and_killed() {
    fn bg_id(text: &str) -> String {
        format!(
            "bg-{}",
            number_after(text, "bg-").expect("a bg-<n> shell id")
        )
    }
    let p = sequence(
        vec![
            call(
                "shell",
                json!({"command":"echo bg-started; sleep 8; echo late > bg-late.txt","run_in_background":true}),
            ),
            shell(&sleep_cmd(2)),
            call_with(
                "get_shell_output",
                |seen| json!({"shell_id": bg_id(&seen[0])}),
            ),
            call_with("kill_shell", |seen| json!({"shell_id": bg_id(&seen[0])})),
            shell(&sleep_cmd(1)),
            call_with(
                "get_shell_output",
                |seen| json!({"shell_id": bg_id(&seen[0])}),
            ),
            call("get_shell_output", json!({"shell_id":"bg-999999"})),
            call("kill_shell", json!({"shell_id":"bg-999999"})),
            // Outlive the background command's own sleep.
            shell(&sleep_cmd(9)),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 9);

    let id = bg_id(&r[0].text);
    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text
            .starts_with(&format!("Started background shell '{id}'. ")),
        "{}",
        r[0].text
    );
    assert!(
        r[0].text.contains(&format!(
            "Poll get_shell_output(shell_id='{id}') for output; stop it with kill_shell(shell_id='{id}')."
        )),
        "{}",
        r[0].text
    );
    assert!(!r[2].error);
    assert!(
        r[2].text.starts_with("[running]"),
        "poll: {:?}; sleep step: {:?}",
        r[2].text,
        r[1].text
    );
    assert!(r[2].text.contains("bg-started"), "{:?}", r[2].text);
    // kill_shell waits for the reaped terminal status and reports it typed.
    assert_eq!(
        r[3].text,
        format!("Killed '{id}' [killed by cancel] exit={{\"state\":\"killedByCancel\"}}")
    );
    assert!(r[5].text.starts_with("[killed]"), "{:?}", r[5].text);
    assert!(r[6].error);
    assert_eq!(r[6].text, "No background shell with id 'bg-999999'.");
    assert!(r[7].error);
    assert_eq!(r[7].text, "No background shell with id 'bg-999999'.");
    assert!(
        !rig.exists("bg-late.txt"),
        "killed background command must not finish its work"
    );
}

#[cfg(windows)]
#[test]
fn powershell_tool_reports_its_own_exit_code() {
    let p = sequence(
        vec![
            call(
                "powershell",
                json!({"command":"$PSVersionTable.PSVersion.Major"}),
            ),
            call(
                "powershell",
                json!({"command":"Write-Output ps-ok; exit 5"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 2);
    assert!(!r[0].error, "{}", r[0].text);
    let major: u32 = r[0].text.trim().parse().expect("PowerShell major version");
    assert!(major >= 5, "{major}");
    assert!(r[1].error);
    assert!(r[1].text.starts_with("[exit 5]\n"), "{:?}", r[1].text);
    assert!(r[1].text.contains("ps-ok"), "{:?}", r[1].text);
}
