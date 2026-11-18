//! Backend trait and the shared, platform-independent pause/resume lifecycle.

use std::sync::Mutex;

/// One observed player and whether it is playing right now.
pub struct Observed<P> {
    pub player: P,
    pub playing: bool,
}

/// A probe failure is distinct from a successful probe with no players.
pub enum Observation<P> {
    Complete(Vec<Observed<P>>),
    Unavailable,
}

/// An accepted command whose expected state cannot be proven remains pending
/// for reconciliation instead of silently losing ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Confirmed,
    Ambiguous,
    Rejected,
}

/// Classify post-command evidence without treating a different player as a
/// recoverable ambiguity. A target switch means the global command may have
/// mutated that other player, so intended-player ownership must be rejected.
pub fn classify_post_observation<P: PartialEq>(
    intended: &P,
    expected_playing: bool,
    observed: Option<&Observed<P>>,
) -> CommandOutcome {
    match observed {
        Some(observed) if &observed.player == intended && observed.playing == expected_playing => {
            CommandOutcome::Confirmed
        }
        Some(observed) if &observed.player == intended => CommandOutcome::Ambiguous,
        Some(_) => CommandOutcome::Rejected,
        None => CommandOutcome::Ambiguous,
    }
}

/// A minimal backend: observe players, and send explicit pause/play to one
/// identified player. No toggle, no metadata beyond identity.
pub trait Backend {
    type Player: Clone + PartialEq + Send + 'static;

    /// Every player the OS currently reports, with its playing state.
    /// `Unavailable` means the probe failed; it must never look like a
    /// positive "not playing" result.
    fn observe(&self) -> Observation<Self::Player>;
    /// Explicit pause plus post-command state confirmation.
    fn pause(&self, player: &Self::Player) -> CommandOutcome;
    /// Explicit play plus post-command state confirmation.
    fn play(&self, player: &Self::Player) -> CommandOutcome;
}

/// Upper bound on distinct players paused by one recording start (Windows
/// can expose several sessions; macOS exposes one).
const MAX_PAUSES_PER_START: usize = 8;

pub(super) struct PausedRecord<P> {
    pub(super) session_id: String,
    pub(super) player: P,
}

/// The shared lifecycle. Generic over the backend so macOS/Windows only need
/// to supply the three-function `Backend` impl.
pub(super) struct Lifecycle<B: Backend> {
    pub(super) backend: B,
    pub(super) paused: Mutex<Vec<PausedRecord<B::Player>>>,
}

impl<B: Backend> Lifecycle<B> {
    pub(super) fn new(backend: B) -> Self {
        Lifecycle {
            backend,
            paused: Mutex::new(Vec::new()),
        }
    }

    /// Recording start: pause everything currently playing and remember it
    /// under `session_id`. Each player is paused at most once per start; a
    /// player whose pause fails is skipped, not a reason to stop pausing
    /// the others.
    pub(super) fn begin(&self, session_id: &str) {
        let already: Vec<B::Player> = self
            .paused
            .lock()
            .map(|paused| paused.iter().map(|record| record.player.clone()).collect())
            .unwrap_or_default();
        let mut count = 0usize;
        let Observation::Complete(observed_players) = self.backend.observe() else {
            return;
        };
        for observed in observed_players {
            if count >= MAX_PAUSES_PER_START {
                break;
            }
            if !observed.playing || already.contains(&observed.player) {
                continue;
            }
            match self.backend.pause(&observed.player) {
                CommandOutcome::Rejected => continue,
                // Keep accepted and unverified pauses for the stop path's
                // bounded reconciliation instead of losing ownership.
                CommandOutcome::Confirmed | CommandOutcome::Ambiguous => {}
            };
            count += 1;
            if let Ok(mut paused) = self.paused.lock() {
                paused.push(PausedRecord {
                    session_id: session_id.to_string(),
                    player: observed.player,
                });
            }
        }
    }

    /// Recording stop/cancel/failure for one session: play back and forget
    /// everything this session paused, unless it is already playing again.
    pub(super) fn restore_for_session(&self, session_id: &str) {
        let records = match self.paused.lock() {
            Ok(mut paused) => {
                let mut mine = Vec::new();
                paused.retain(|record| {
                    if record.session_id == session_id {
                        mine.push(PausedRecord {
                            session_id: record.session_id.clone(),
                            player: record.player.clone(),
                        });
                        false
                    } else {
                        true
                    }
                });
                mine
            }
            Err(_) => return,
        };
        self.reconcile(records);
    }

    /// Play back and forget everything any session paused.
    pub(super) fn restore_any(&self) {
        let records = match self.paused.lock() {
            Ok(mut paused) => paused.drain(..).collect(),
            Err(_) => return,
        };
        self.reconcile(records);
    }

    /// Play each recorded player only if it is observed right now and is
    /// not playing. Unobserved (gone, replaced by another player, or the
    /// observation failed) means no command: on macOS a play command goes
    /// to whatever is now-playing, so identity must be positively confirmed.
    fn reconcile(&self, records: Vec<PausedRecord<B::Player>>) {
        if records.is_empty() {
            return;
        }
        let observed = match self.backend.observe() {
            Observation::Unavailable => match self.backend.observe() {
                Observation::Unavailable => {
                    crate::event(
                        "media_restore_observation_failed",
                        "bounded_probe_exhausted",
                    );
                    return;
                }
                Observation::Complete(observed) => observed,
            },
            Observation::Complete(observed) => observed,
        };
        for record in records {
            match observed.iter().find(|o| o.player == record.player) {
                // A positively confirmed pause followed by playing means
                // user resumed it. Pending ownership is also abandoned here
                // because this stop path has used its bounded follow-up probe.
                Some(observed) if observed.playing => {
                    crate::event(
                        "media_restore_outcome",
                        "already_playing",
                    );
                    continue;
                }
                Some(_) => match self.backend.play(&record.player) {
                    CommandOutcome::Confirmed => {
                        crate::event(
                            "media_restore_outcome",
                            "play_confirmed",
                        );
                    }
                    // A restore command is one-shot. Retrying after an
                    // ambiguous global command could undo a user's pause.
                    outcome @ (CommandOutcome::Ambiguous | CommandOutcome::Rejected) => {
                        crate::event(
                            "media_restore_outcome",
                            &format!("play_{outcome:?}"),
                        );
                    }
                },
                // A pending pause that cannot be positively matched is
                // abandoned after this bounded stop reconciliation.
                None => {
                    crate::event(
                        "media_restore_outcome",
                        "owned_player_not_observed",
                    );
                }
            }
        }
    }
}
