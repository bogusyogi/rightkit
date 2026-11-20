//! macOS media-control backend (spec section 7).
//!
//! Reading now-playing state: macOS blocks MediaRemote now-playing reads for unentitled
//! third-party processes, but allows them from Apple-signed `/usr/bin/osascript -l JavaScript`.
//! The host supplies the script (a bundled resource) through [`MacBackend::new`]; it is run
//! out-of-process with a hard kill deadline and must print one small JSON object
//! `{"bundle": <string>, "rate": <number>, "pid": <integer>}`.
//!
//! Sending pause/play: works IN-PROCESS from an unentitled third-party process via
//! `dlopen`/`dlsym` on `MediaRemote.framework`'s `MRMediaRemoteSendCommand`. The command has no
//! target: it goes to the current now-playing client. That is why the lifecycle only sends when
//! the observed now-playing client IS the recorded player. `send 2` (toggle) is never issued.

use crate::{classify_post_observation, event, Backend, CommandOutcome, Observation, Observed};
use rightkit_child_process::{run_command_capture, CaptureOptions, OwnedCommand};
use std::os::raw::{c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// Hard deadline for the now-playing-state osascript child.
const OSASCRIPT_TIMEOUT_MS: u64 = 2_000;
/// The probe prints one small JSON object; anything past this is dropped by the runner.
const OSASCRIPT_STDOUT_CAP: usize = 64 * 1024;

const MR_PAUSE: c_int = 1;
const MR_PLAY: c_int = 0;

#[derive(Clone, Debug, PartialEq)]
pub struct MacPlayer {
    pub bundle: String,
    pub pid: i64,
}

/// The backend. `script_path` returns the probe script's location (bundle layout is the host's).
pub struct MacBackend {
    script_path: fn() -> Option<PathBuf>,
}

impl MacBackend {
    pub const fn new(script_path: fn() -> Option<PathBuf>) -> Self {
        Self { script_path }
    }

    /// Runs the host's JXA probe with a hard kill deadline. Returns the raw JSON stdout on
    /// success.
    fn run_probe(&self) -> Option<String> {
        let script = (self.script_path)()?;
        let mut owned = OwnedCommand::new("/usr/bin/osascript");
        owned.command_mut().arg("-l").arg("JavaScript").arg(script);
        // The owner's capture runner drains stdout on its own thread, terminates and reaps the tree
        // at the deadline, and bounds its own cleanup.
        let options = CaptureOptions::default()
            .timeout(Duration::from_millis(OSASCRIPT_TIMEOUT_MS))
            .stdout_cap(OSASCRIPT_STDOUT_CAP)
            .stderr_cap(0);
        let output = run_command_capture(owned, None, &options).ok()?;
        if output.timed_out {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout.bytes).into_owned())
    }

    fn observe_one(&self) -> Option<Observed<MacPlayer>> {
        self.run_probe().and_then(|raw| parse_probe(&raw))
    }

    /// `MRMediaRemoteSendCommand` has no target: it goes to whatever client is now-playing at the
    /// instant it is sent. The lifecycle observes first and sends at once, but the observation is a
    /// snapshot taken ~100 ms earlier by the osascript child, so the client can change in between.
    /// Re-observe after sending: identity and expected playback state must both match. A different
    /// identity rejects ownership because the targetless command may have mutated that other
    /// player. Missing or same-identity state evidence remains ambiguous for bounded reconcile.
    fn command_landed_on(&self, player: &MacPlayer, expected_playing: bool) -> CommandOutcome {
        let observed = self.observe_one();
        match classify_post_observation(player, expected_playing, observed.as_ref()) {
            CommandOutcome::Confirmed => CommandOutcome::Confirmed,
            CommandOutcome::Ambiguous if observed.as_ref().is_some_and(|o| o.player == *player) => {
                event(
                    "media_command_state_unconfirmed",
                    &format!(
                        "intended={} expected_playing={expected_playing}",
                        player.bundle
                    ),
                );
                CommandOutcome::Ambiguous
            }
            CommandOutcome::Rejected => {
                let actual = observed
                    .as_ref()
                    .map(|observed| observed.player.bundle.as_str())
                    .unwrap_or("unknown");
                event(
                    "media_command_misdirected",
                    &format!("intended={} now_playing={actual}", player.bundle),
                );
                CommandOutcome::Rejected
            }
            CommandOutcome::Ambiguous => {
                event(
                    "media_command_unverified",
                    &format!("intended={}", player.bundle),
                );
                CommandOutcome::Ambiguous
            }
        }
    }

    fn target_state_matches(&self, player: &MacPlayer, expected_playing: bool) -> bool {
        matches!(
            self.observe_one(),
            Some(observed) if observed.player == *player && observed.playing == expected_playing
        )
    }
}

/// Parses the probe's JSON into the one now-playing client (if any)
/// and whether it is playing (`rate > 0`). A client with no playback
/// rate is unknown, not paused: it is not observed at all, so no
/// command can be sent on its behalf.
fn parse_probe(raw: &str) -> Option<Observed<MacPlayer>> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let bundle = value.get("bundle").and_then(serde_json::Value::as_str)?;
    let rate = value.get("rate").and_then(serde_json::Value::as_f64)?;
    let pid = value
        .get("pid")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    Some(Observed {
        player: MacPlayer {
            bundle: bundle.to_string(),
            pid,
        },
        playing: rate > 0.0,
    })
}

type SendCommandFn = unsafe extern "C" fn(c_int, *const c_void) -> bool;

fn send_command_fn() -> Option<SendCommandFn> {
    static SYMBOL: OnceLock<Option<usize>> = OnceLock::new();
    let address = *SYMBOL.get_or_init(|| unsafe {
        let path = c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote";
        let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            event("media_backend_error", "dlopen_failed");
            return None;
        }
        let symbol_name = c"MRMediaRemoteSendCommand";
        let symbol = libc::dlsym(handle, symbol_name.as_ptr());
        if symbol.is_null() {
            event("media_backend_error", "dlsym_failed");
            return None;
        }
        Some(symbol as usize)
    });
    // SAFETY: `address`, when present, was returned by `dlsym` for
    // `MRMediaRemoteSendCommand`, whose documented signature matches
    // `SendCommandFn`.
    address.map(|address| unsafe { std::mem::transmute::<usize, SendCommandFn>(address) })
}

fn send_command(pause: bool) -> bool {
    let Some(send) = send_command_fn() else {
        return false;
    };
    let command = if pause { MR_PAUSE } else { MR_PLAY };
    // SAFETY: `send` is `MRMediaRemoteSendCommand`; it accepts a command
    // constant and an optional options dictionary (null is documented as
    // valid). Never called with the toggle command (2).
    unsafe { send(command, std::ptr::null()) }
}

impl Backend for MacBackend {
    type Player = MacPlayer;

    fn observe(&self) -> Observation<MacPlayer> {
        match self.observe_one() {
            Some(observed) => Observation::Complete(vec![observed]),
            None => Observation::Unavailable,
        }
    }

    fn pause(&self, player: &MacPlayer) -> CommandOutcome {
        // MediaRemote's command is global. Recheck immediately before
        // sending so a stale lifecycle snapshot is not used. A switch
        // during the call remains inherently ambiguous: this API has no
        // target parameter.
        if !self.target_state_matches(player, true) {
            return CommandOutcome::Rejected;
        }
        let sent = send_command(true);
        event(
            "media_pause_issued",
            &format!("bundle={} sent={sent}", player.bundle),
        );
        if !sent {
            CommandOutcome::Rejected
        } else {
            self.command_landed_on(player, false)
        }
    }

    fn play(&self, player: &MacPlayer) -> CommandOutcome {
        if !self.target_state_matches(player, false) {
            return CommandOutcome::Rejected;
        }
        let sent = send_command(false);
        event(
            "media_play_issued",
            &format!("bundle={} sent={sent}", player.bundle),
        );
        if !sent {
            CommandOutcome::Rejected
        } else {
            self.command_landed_on(player, true)
        }
    }
}
