use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReplayEffect {
    Paste { text: String },
    Enter,
    Stop,
    Send,
    Cancel,
}

impl ReplayEffect {
    pub(crate) fn slot(&self) -> EffectSlot {
        match self {
            Self::Paste { .. } => EffectSlot::Paste,
            Self::Enter => EffectSlot::Enter,
            Self::Stop | Self::Send | Self::Cancel => EffectSlot::TerminalAction,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum EffectSlot {
    Paste,
    Enter,
    TerminalAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedReplayEffect {
    pub(crate) session_id: String,
    pub(crate) effect: ReplayEffect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DuplicateReplayEffect {
    pub(crate) session_id: String,
    pub(crate) effect: ReplayEffect,
}

/// Records desktop effects requested by replay without mutating desktop state.
///
/// Each session may request at most one paste & one Enter. A duplicate is an
/// explicit test failure instead of a second simulated side effect.
#[derive(Default)]
pub(crate) struct ReplayEffectSink {
    pub(crate) effects: BTreeMap<(String, EffectSlot), ReplayEffect>,
}

impl ReplayEffectSink {
    pub(crate) fn record_paste(
        &mut self,
        session_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), DuplicateReplayEffect> {
        self.record(session_id.into(), ReplayEffect::Paste { text: text.into() })
    }

    pub(crate) fn record_enter(
        &mut self,
        session_id: impl Into<String>,
    ) -> Result<(), DuplicateReplayEffect> {
        self.record(session_id.into(), ReplayEffect::Enter)
    }

    pub(crate) fn record_terminal(
        &mut self,
        session_id: impl Into<String>,
        action: ReplayEffect,
    ) -> Result<(), DuplicateReplayEffect> {
        assert!(
            matches!(
                action,
                ReplayEffect::Stop | ReplayEffect::Send | ReplayEffect::Cancel
            ),
            "terminal replay effect must be STOP, SEND, or CANCEL"
        );
        self.record(session_id.into(), action)
    }

    pub(crate) fn record(
        &mut self,
        session_id: String,
        effect: ReplayEffect,
    ) -> Result<(), DuplicateReplayEffect> {
        let key = (session_id.clone(), effect.slot());
        if self.effects.contains_key(&key) {
            return Err(DuplicateReplayEffect { session_id, effect });
        }
        self.effects.insert(key, effect);
        Ok(())
    }

    pub(crate) fn effects_for(&self, session_id: &str) -> Vec<RecordedReplayEffect> {
        self.effects
            .iter()
            .filter(|((recorded_session, _), _)| recorded_session == session_id)
            .map(|((recorded_session, _), effect)| RecordedReplayEffect {
                session_id: recorded_session.clone(),
                effect: effect.clone(),
            })
            .collect()
    }

    pub(crate) fn assert_exactly_once(
        &self,
        session_id: &str,
        expected: &[ReplayEffect],
    ) -> Result<(), String> {
        let actual: Vec<_> = self
            .effects_for(session_id)
            .into_iter()
            .map(|recorded| recorded.effect)
            .collect();
        if actual == expected {
            Ok(())
        } else {
            Err(format!(
                "session {session_id}: expected effects {expected:?}, got {actual:?}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_intent_by_session_without_executing_it() {
        let mut sink = ReplayEffectSink::default();
        sink.record_paste("session-a", "hello").unwrap();
        sink.record_enter("session-a").unwrap();
        sink.record_paste("session-b", "other").unwrap();

        sink.assert_exactly_once(
            "session-a",
            &[
                ReplayEffect::Paste {
                    text: "hello".into(),
                },
                ReplayEffect::Enter,
            ],
        )
        .unwrap();
        assert_eq!(sink.effects_for("session-b").len(), 1);
    }

    #[test]
    fn rejects_duplicate_effect_slots_per_session() {
        let mut sink = ReplayEffectSink::default();
        sink.record_paste("session-a", "first").unwrap();

        let duplicate = sink.record_paste("session-a", "second").unwrap_err();
        assert_eq!(duplicate.session_id, "session-a");
        assert_eq!(
            duplicate.effect,
            ReplayEffect::Paste {
                text: "second".into()
            }
        );
        sink.assert_exactly_once(
            "session-a",
            &[ReplayEffect::Paste {
                text: "first".into(),
            }],
        )
        .unwrap();
    }

    #[test]
    fn rejects_second_terminal_action_regardless_of_kind() {
        let mut sink = ReplayEffectSink::default();
        sink.record_terminal("session-a", ReplayEffect::Stop)
            .unwrap();

        let duplicate = sink
            .record_terminal("session-a", ReplayEffect::Send)
            .unwrap_err();
        assert_eq!(duplicate.effect, ReplayEffect::Send);
        sink.assert_exactly_once("session-a", &[ReplayEffect::Stop])
            .unwrap();
    }

    #[test]
    fn exact_once_assertion_reports_missing_or_extra_effects() {
        let mut sink = ReplayEffectSink::default();
        sink.record_enter("session-a").unwrap();

        let error = sink
            .assert_exactly_once(
                "session-a",
                &[ReplayEffect::Paste {
                    text: "hello".into(),
                }],
            )
            .unwrap_err();
        assert!(error.contains("expected effects"));
        assert!(error.contains("Enter"));
    }
}
