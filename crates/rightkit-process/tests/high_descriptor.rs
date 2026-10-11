#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

//! A pipe writer held at a very high descriptor number must not outlive the spawn in the
//! parent-death helper. The helper never execs, so close-on-exec does not release what it
//! inherited: a pipe writer held there keeps the reader from ever seeing end of file.
//!
//! The writer is duplicated **before** `spawn`, in this process, so it exists when the macOS
//! helper is forked (inside `spawn`) and is far above any highest-descriptor-plus-slack bound
//! computed earlier. The previous version of this test duplicated it in the engine's
//! `pre_exec`, which runs after the helper was forked, so the helper never held it and the test
//! could not fail.
//!
//! What this file cannot do is make the kernel refuse to list the descriptor table: that needs
//! a crate-private seam. The fail-closed case, with the writer injected after the helper's
//! pipes exist and immediately before its fork and the listing forced to fail, is
//! `an_unlistable_descriptor_table_fails_the_spawn_and_frees_a_writer_above_any_bound` in
//! `src/process_unix.rs`.

use std::{process::Stdio, time::Duration};

use rightkit_process::OwnedCommand;

const HIGH: libc::c_int = 3000;

#[test]
fn a_writer_held_at_a_very_high_descriptor_before_the_spawn_does_not_hold_the_pipe_open() {
    // SAFETY: plain syscalls on descriptors this test owns.
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
        let wanted = 8192.min(limit.rlim_max);
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted;
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
        }
        let mut ends = [0 as libc::c_int; 2];
        assert_eq!(libc::pipe(ends.as_mut_ptr()), 0);
        let [reader, writer] = ends;
        libc::fcntl(reader, libc::F_SETFD, libc::FD_CLOEXEC);
        // The writer lands far above anything a bound taken from the lowest descriptors
        // would cover, and exists before the helper is forked. Close-on-exec, like a
        // descriptor another thread opened properly: only the never-exec helper can keep it.
        let high = libc::fcntl(writer, libc::F_DUPFD_CLOEXEC, HIGH);
        assert!(high >= HIGH, "the duplicate must land above {HIGH}");
        libc::close(writer);

        let mut command = OwnedCommand::new("/bin/sleep");
        command
            .command_mut()
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.kill_on_parent_exit(true);
        let mut child = command.spawn().expect("spawn");
        // The only copies left are the ones the helper inherited at its fork.
        libc::close(high);

        let mut pending = libc::pollfd {
            fd: reader,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = libc::poll(&mut pending, 1, Duration::from_secs(4).as_millis() as i32);
        let mut byte = 0u8;
        let read = if ready > 0 {
            libc::read(reader, (&mut byte as *mut u8).cast(), 1)
        } else {
            -1
        };
        libc::close(reader);
        let _ = child.terminate_tree();
        assert!(
            ready > 0 && read == 0,
            "no end of file within 4 s: a helper kept the pipe writer open (poll {ready}, read {read})"
        );
    }
}
