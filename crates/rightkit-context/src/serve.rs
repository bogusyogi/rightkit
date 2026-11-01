//! `membrane serve`: the resident the host app runs.
//!
//! The host app's launcher spawns this with a stdin pipe it holds open for the resident's life and owns
//! its lifetime (rightkit-daemon's `private::watch_stdin_eof`). It takes the owner lock first, so
//! a second instance prints "already serving" and exits 0; it exits within 2 s of the pipe
//! closing (the parent going, even by SIGKILL), and on SIGTERM or console close. Everything
//! resident (watchers, Blueprint refresh, indexers, the endpoint) lives in
//! `rightkit_context_runtime::serve`.

use rightkit_context_runtime::serve::{run_resident, ResidentExit};

/// Run `membrane serve`; returns the process exit code.
pub fn run_serve() -> i32 {
    match run_resident() {
        Ok(ResidentExit::Stopped) => crate::EXIT_OK,
        Ok(ResidentExit::AlreadyServing) => {
            println!("already serving");
            crate::EXIT_OK
        }
        Err(error) => {
            eprintln!("membrane serve: {error}");
            crate::EXIT_INTERNAL_ERROR
        }
    }
}
