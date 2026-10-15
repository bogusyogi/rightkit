//! Live check of the macOS frontmost-application reader the QA background proof stands on.
#![cfg(target_os = "macos")]
use rightkit_qa_driver::mac;

#[test]
fn frontmost_pid_and_name_are_readable_and_agree_with_the_process_table() {
    let pid = mac::frontmost_pid().expect("the frontmost pid must be readable");
    assert!(pid > 0);
    let name = mac::frontmost_name().expect("the frontmost name must be readable");
    // The CI defect: the literal lsappinfo key came back as the name.
    assert!(
        !name.is_empty() && name != "LSDisplayName" && name != "name",
        "{name:?}"
    );
    // The pid is a live process.
    let alive = unsafe { libc_kill(pid, 0) } == 0;
    assert!(alive, "frontmost pid {pid} is not a live process");
    // Two reads in a row do not flap to unavailable.
    assert!(mac::frontmost_pid().is_some());
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
