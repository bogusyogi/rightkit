//! The macOS side of the [`Backend`] seam: [`MacBackend`] is a [`ProcessBackend`] over the
//! `rightkit-record-macos-helper` binary, launched and reaped through `rightkit-process`.
//!
//! The helper is a signed Swift binary owned by this crate (`helpers/macos`, a SwiftPM
//! package). It captures one window with ScreenCaptureKit (`SCStream` with an `SCStreamOutput`
//! frame callback, window-scoped by pid and window id, `onScreenWindowsOnly: false`) and encodes
//! with `AVAssetWriter` into `clean.mp4`. It speaks the helper wire protocol of
//! [`crate::protocol`] on stdin and stdout. The core owns the bundle; the helper writes only the
//! media file. Frame hashes are `blake3:` over the packed BGRA pixels.
//!
//! Screen-recording permission belongs to the helper identity, not to the core process, so
//! [`Backend::permission`] reports what the helper read with `CGPreflightScreenCaptureAccess`
//! (which never prompts) when it said `hello`. The helper never prompts; it fails closed with
//! `permission_denied` before writing anything.
//!
//! Building and signing for tests: `helpers/macos/build.sh` (release build, ad-hoc signature).
//! Production signing and notarization are done by the release tooling, not here.
//!
//! Locating the helper: [`MacBackend::helper_path`] reads `RIGHTKIT_RECORD_MACOS_HELPER`, then
//! looks for `rightkit-record-macos-helper` next to the running executable.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rightkit_contracts::types::record_session::RecorderOs;
use rightkit_process::{OwnedChild, OwnedCommand, StderrTail};

use crate::backend::{
    Backend, BackendPlan, Finished, HelperEvent, HelperIdentity, Permission, Source, StartInfo,
};
use crate::error::{Code, RecordError, Result};
use crate::process::ProcessBackend;

#[cfg(test)]
mod golden;
#[cfg(test)]
mod live;

/// File name of the helper binary.
pub const HELPER_BINARY: &str = "rightkit-record-macos-helper";
/// Environment variable naming the helper binary.
pub const HELPER_ENV: &str = "RIGHTKIT_RECORD_MACOS_HELPER";

/// How long a helper gets to exit after its final response before it is terminated.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// Stderr kept for diagnostics.
const STDERR_TAIL_BYTES: usize = 16 * 1024;

pub struct MacBackend {
    inner: ProcessBackend,
    child: OwnedChild,
    stderr: StderrTail,
}

impl MacBackend {
    /// The helper binary: `RIGHTKIT_RECORD_MACOS_HELPER` if set, else next to the running
    /// executable (or one directory up, where cargo puts test binaries' siblings).
    pub fn helper_path() -> Option<PathBuf> {
        if let Some(path) = env::var_os(HELPER_ENV) {
            let path = PathBuf::from(path);
            return path.is_file().then_some(path);
        }
        let exe = env::current_exe().ok()?;
        let dir = exe.parent()?;
        let found = [Some(dir), dir.parent()]
            .into_iter()
            .flatten()
            .map(|d| d.join(HELPER_BINARY))
            .find(|candidate| candidate.is_file());
        found
    }

    /// Launches the helper at [`MacBackend::helper_path`].
    pub fn launch_default() -> Result<Self> {
        let path = Self::helper_path().ok_or_else(|| {
            RecordError::new(
                Code::NotFound,
                format!("{HELPER_BINARY} not found; set {HELPER_ENV} or build helpers/macos"),
            )
        })?;
        Self::launch(&path)
    }

    /// Launches `helper` and reads its identity. The helper is owned by this backend and
    /// terminated when the backend is dropped.
    pub fn launch(helper: &Path) -> Result<Self> {
        Self::launch_with_env(helper, &[])
    }

    /// As [`MacBackend::launch`], with extra environment variables for the helper.
    pub fn launch_with_env(helper: &Path, envs: &[(&str, &str)]) -> Result<Self> {
        let mut command = OwnedCommand::new(helper);
        command
            .command_mut()
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            RecordError::new(
                Code::Helper,
                format!("the helper {} could not start: {e}", helper.display()),
            )
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.take_stdin(), child.take_stdout(), child.take_stderr())
        else {
            return Err(RecordError::new(Code::Helper, "the helper has no stdio"));
        };
        let mut stderr = StderrTail::plain(stderr, STDERR_TAIL_BYTES);
        match ProcessBackend::connect(stdout, stdin) {
            Ok(inner) => Ok(MacBackend {
                inner,
                child,
                stderr,
            }),
            Err(error) => {
                let _ = child.terminate_tree();
                let tail = stderr.finish_within(Duration::from_secs(1));
                Err(RecordError::new(
                    error.code,
                    format!("{} (helper stderr: {tail})", error.message),
                ))
            }
        }
    }

    /// The helper's recent stderr, for diagnostics.
    pub fn helper_stderr(&self) -> String {
        self.stderr.snapshot()
    }

    /// Gives a helper that has sent `bye` a moment to exit, then terminates it.
    fn reap(&mut self) {
        if !matches!(self.child.wait_timeout(EXIT_GRACE), Ok(Some(_))) {
            let _ = self.child.terminate_tree();
        }
    }
}

impl Backend for MacBackend {
    fn os(&self) -> RecorderOs {
        self.inner.os()
    }

    fn os_build(&self) -> String {
        self.inner.os_build()
    }

    fn helper(&self) -> HelperIdentity {
        self.inner.helper()
    }

    fn permission(&self) -> Permission {
        self.inner.permission()
    }

    fn sources(&self) -> Result<Vec<Source>> {
        self.inner.sources()
    }

    fn start(&mut self, plan: &BackendPlan) -> Result<StartInfo> {
        self.inner.start(plan)
    }

    fn poll(&mut self) -> Vec<HelperEvent> {
        self.inner.poll()
    }

    fn stop(&mut self) -> Result<Finished> {
        let finished = self.inner.stop();
        self.reap();
        finished
    }

    fn abort(&mut self) {
        self.inner.abort();
        self.reap();
    }
}
