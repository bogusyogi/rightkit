//! Windows backend (spec section 8): GSMTC via the `windows` crate.

use super::backend::{Backend, CommandOutcome, Observation, Observed};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

#[derive(Clone)]
pub(super) struct WinPlayer {
    app_id: String,
    session: Session,
}

impl PartialEq for WinPlayer {
    fn eq(&self, other: &Self) -> bool {
        let mine = com_identity(&self.session);
        mine != 0 && mine == com_identity(&other.session)
    }
}

/// COM identity: the stable `IUnknown` pointer per live session object.
fn com_identity(session: &Session) -> usize {
    windows::core::Interface::cast::<windows::core::IUnknown>(session)
        .map(|unknown| windows::core::Interface::as_raw(&unknown) as usize)
        .unwrap_or(0)
}

#[derive(Default)]
pub(super) struct WinBackend {
    // Session COM identities are stable within one manager, not across
    // RequestAsync calls. Keep this owner alive for the worker lifetime.
    manager: std::sync::OnceLock<Manager>,
}

impl WinBackend {
    fn manager(&self) -> Option<&Manager> {
        if self.manager.get().is_none() {
            let manager = Manager::RequestAsync().ok()?.join().ok()?;
            let _ = self.manager.set(manager);
        }
        self.manager.get()
    }
}

impl Backend for WinBackend {
    type Player = WinPlayer;

    /// Blocking `.join()` calls are fine here: this always runs on the
    /// media worker thread, never the recording path.
    fn observe(&self) -> Observation<WinPlayer> {
        let Some(manager) = self.manager() else {
            return Observation::Unavailable;
        };
        let Ok(sessions) = manager.GetSessions() else {
            return Observation::Unavailable;
        };
        let mut out = Vec::new();
        for session in sessions {
            let Ok(info) = session.GetPlaybackInfo() else {
                continue;
            };
            let Ok(status) = info.PlaybackStatus() else {
                continue;
            };
            let app_id = session
                .SourceAppUserModelId()
                .map(|id| id.to_string())
                .unwrap_or_default();
            out.push(Observed {
                player: WinPlayer { app_id, session },
                playing: status == Status::Playing,
            });
        }
        Observation::Complete(out)
    }

    fn pause(&self, player: &WinPlayer) -> CommandOutcome {
        let sent = player
            .session
            .TryPauseAsync()
            .ok()
            .and_then(|op| op.join().ok())
            .unwrap_or(false);
        crate::event(
            "media_pause_issued",
            &format!("app={} sent={sent}", player.app_id),
        );
        if !sent {
            CommandOutcome::Rejected
        } else {
            confirm_state(self, player, false)
        }
    }

    fn play(&self, player: &WinPlayer) -> CommandOutcome {
        let sent = player
            .session
            .TryPlayAsync()
            .ok()
            .and_then(|op| op.join().ok())
            .unwrap_or(false);
        crate::event(
            "media_play_issued",
            &format!("app={} sent={sent}", player.app_id),
        );
        if !sent {
            CommandOutcome::Rejected
        } else {
            confirm_state(self, player, true)
        }
    }
}

fn confirm_state(
    backend: &WinBackend,
    player: &WinPlayer,
    expected_playing: bool,
) -> CommandOutcome {
    match backend.observe() {
        Observation::Complete(observed) => match observed.iter().find(|o| o.player == *player) {
            Some(observed) if observed.playing == expected_playing => CommandOutcome::Confirmed,
            Some(_) => CommandOutcome::Ambiguous,
            None => CommandOutcome::Ambiguous,
        },
        Observation::Unavailable => CommandOutcome::Ambiguous,
    }
}
