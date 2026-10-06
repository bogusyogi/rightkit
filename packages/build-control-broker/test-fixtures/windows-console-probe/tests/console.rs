//! A RightKit-managed job must run inside an invisible console that its descendants inherit;
//! otherwise every console-subsystem child a test spawns (cmd, git, powershell) opens a window.
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetConsoleCP() -> u32;
    fn GetConsoleWindow() -> isize;
}
#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn IsWindowVisible(hwnd: isize) -> i32;
}

#[cfg(windows)]
#[test]
fn managed_job_console_is_attached_and_invisible() {
    let attached = unsafe { GetConsoleCP() } != 0;
    let hwnd = unsafe { GetConsoleWindow() };
    let visible = hwnd != 0 && unsafe { IsWindowVisible(hwnd) } != 0;
    eprintln!("console attached={attached} hwnd={hwnd} visible={visible}");
    assert!(attached, "no console: each console-subsystem child would open its own window");
    assert!(!visible, "the job's console window is visible");
    let status = std::process::Command::new("cmd").args(["/c", "exit 0"]).status().unwrap();
    assert!(status.success(), "cmd /c exit 0 failed");
}
