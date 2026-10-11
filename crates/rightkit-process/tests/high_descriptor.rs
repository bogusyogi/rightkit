#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

//! A descriptor another thread duplicates far above the others must not outlive the spawn in
//! the parent-death helper. The helper never execs, so close-on-exec does not release what it
//! inherited: a pipe writer held there keeps the reader from ever seeing end of file.

use std::{os::unix::process::CommandExt, process::Stdio, time::Duration};

use rightkit_process::OwnedCommand;

const HIGH: libc::c_int = 3000;

#[test]
fn a_writer_duplicated_to_a_very_high_descriptor_does_not_hold_the_pipe_open() {
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
        for fd in ends {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }

        let mut command = OwnedCommand::new("/bin/sleep");
        command
            .command_mut()
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Runs in the new process before the library's own hook, like another thread's dup2
        // landing above any bound computed earlier. Close-on-exec is set on the duplicate.
        command.command_mut().pre_exec(move || {
            if libc::dup2(writer, HIGH) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::fcntl(HIGH, libc::F_SETFD, libc::FD_CLOEXEC);
            Ok(())
        });
        command.kill_on_parent_exit(true);
        let mut child = command.spawn().expect("spawn");
        libc::close(writer);

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
