//! Async helpers for callers that build tokio commands (issue #121). Run with
//! `--features tokio`.
#![cfg(feature = "tokio")]

use rightkit_child_process::{tokio_support, NoWindow, TerminationOptions};
use std::time::{Duration, Instant};

#[test]
fn no_window_chains_on_a_tokio_command() {
    let mut command = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", "exit 0"]);
        c
    } else {
        tokio::process::Command::new("true")
    };
    // Chains and returns the same command.
    command.no_window().kill_on_drop(true);
}

#[tokio::test]
async fn a_tokio_command_with_no_window_still_runs() {
    let mut command = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", "exit 0"]);
        c
    } else {
        tokio::process::Command::new("true")
    };
    let status = command.no_window().status().await.expect("run");
    assert!(status.success());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::num::NonZeroU32;

    fn leader(script: &str) -> tokio::process::Child {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", script]).process_group(0);
        command.spawn().expect("spawn")
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only tests for existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[tokio::test]
    async fn terminate_tree_stops_a_group_leader_and_its_descendants() {
        let mut child = leader("sleep 600 & wait");
        // The grandchild's pid is not needed: the group must be empty afterwards.
        let pid = child.id().expect("pid") as i32;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        let status = tokio_support::terminate_tree(
            &mut child,
            TerminationOptions::default().with_grace(Duration::from_secs(5)),
        )
        .await
        .expect("terminate");
        assert!(!status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
        // SAFETY: existence test of the group.
        assert!(
            unsafe { libc::kill(-pid, 0) } != 0,
            "the group still has members"
        );
    }

    #[tokio::test]
    async fn terminate_tree_escalates_to_kill_when_term_is_ignored() {
        let mut child = leader("trap '' TERM; while :; do sleep 1; done");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        let status = tokio_support::terminate_tree(
            &mut child,
            TerminationOptions::default().with_grace(Duration::from_millis(300)),
        )
        .await
        .expect("terminate");
        assert!(!status.success());
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn terminate_tree_on_an_exited_child_returns_its_status() {
        let mut child = leader("exit 3");
        child.wait().await.expect("wait");
        let status = tokio_support::terminate_tree(&mut child, TerminationOptions::default())
            .await
            .expect("terminate");
        assert_eq!(status.code(), Some(3));
    }

    #[tokio::test]
    async fn terminate_process_group_empties_a_group_without_a_child_handle() {
        let child = leader("sleep 600 & wait");
        let pgid = child.id().expect("pid");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let emptied = tokio_support::terminate_process_group(
            NonZeroU32::new(pgid).unwrap(),
            TerminationOptions::default().with_grace(Duration::from_secs(2)),
        )
        .await
        .expect("terminate group");
        // The leader is a zombie until reaped, so the group may still list it: reap, then look.
        let mut child = child;
        let _ = child.wait().await;
        assert!(emptied || !alive(-(pgid as i32)));
        assert!(!alive(-(pgid as i32)));
    }

    #[tokio::test]
    async fn terminate_process_group_refuses_this_processs_own_group() {
        // SAFETY: getpgrp has no preconditions.
        let own = unsafe { libc::getpgrp() } as u32;
        let error = tokio_support::terminate_process_group(
            NonZeroU32::new(own).unwrap(),
            TerminationOptions::default(),
        )
        .await
        .expect_err("must refuse");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn terminate_tree_stops_a_windows_child() {
    let mut command = tokio::process::Command::new("ping");
    command.args(["-n", "30", "127.0.0.1"]).no_window();
    let mut child = command.spawn().expect("spawn");
    let status = tokio_support::terminate_tree(&mut child, TerminationOptions::default())
        .await
        .expect("terminate");
    assert!(!status.success());
}
