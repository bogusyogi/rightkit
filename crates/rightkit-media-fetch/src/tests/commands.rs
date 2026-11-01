use crate::process::fetch_command;
use rightkit_child_process::{run_streaming, StreamOptions};

#[test]
fn cancelable_command_kills_child_process() {
    #[cfg(windows)]
    let mut command = fetch_command(&["cmd".to_string(), "/C".to_string()]).expect("command");
    #[cfg(windows)]
    command.command_mut().arg("ping -n 6 127.0.0.1 > NUL");
    #[cfg(not(windows))]
    let mut command = fetch_command(&["sh".to_string(), "-c".to_string()]).expect("command");
    #[cfg(not(windows))]
    command.command_mut().arg("sleep 5");
    let output = run_streaming(
        command,
        &StreamOptions::new(),
        &|| true,
        &mut |_| {},
        &mut |_| {},
    )
    .expect("a cancelled child is a normal return");
    assert!(output.cancelled);
}
