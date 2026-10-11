//! Names every re-exported `rightkit-process` type through `rightkit_media::` only.

use rightkit_media::{
    spawn_streaming, OwnedChild, ReapOutcome, StdioMode, StdioPolicy, TerminationOptions,
    WaitOutcome,
};

#[test]
fn process_types_are_nameable_without_a_rightkit_process_dependency() {
    let _spawn: fn(
        &dyn rightkit_media::FfmpegBinary,
        &rightkit_media::FfmpegCommand,
        StdioPolicy,
    ) -> Result<OwnedChild, rightkit_media::RunError> = spawn_streaming;
    let _wait: fn(&mut OwnedChild, std::time::Duration) -> std::io::Result<WaitOutcome> =
        OwnedChild::wait_or_kill;
    let _reap: Option<ReapOutcome> = None;
    let _options = TerminationOptions::default();
    assert_eq!(StdioPolicy::default().stdin, StdioMode::Null);
}
