//! Pure decisions around enabling global shortcuts during and after onboarding.
//!
//! The host owns the flags, the platform hooks and every word the user reads.

/// Why hotkey registration after onboarding is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationRefusal {
    /// Onboarding (and any legal acceptance the host ties to it) is not complete.
    OnboardingIncomplete,
    /// The realtime voice-control assets are missing.
    VoiceControlsUnavailable,
}

/// Messages for [`RegistrationRefusal`]; the host passes its own copy.
#[derive(Debug, Clone)]
pub struct RegistrationLabels {
    pub onboarding_incomplete: String,
    pub voice_controls_unavailable: String,
}

impl Default for RegistrationLabels {
    fn default() -> Self {
        Self {
            onboarding_incomplete:
                "onboarding and current legal acceptance are required; hotkeys remain gated".into(),
            voice_controls_unavailable:
                "Bundled realtime voice controls are unavailable. Reinstall the app.".into(),
        }
    }
}

/// The gate in front of registering hotkeys once onboarding finishes. Order (kept): onboarding
/// first, then voice controls. `voice_controls_missing` is only evaluated when onboarding is
/// complete.
pub fn registration_gate(
    onboarding_complete: bool,
    voice_controls_missing: impl FnOnce() -> bool,
    labels: &RegistrationLabels,
) -> Result<(), String> {
    registration_refusal(onboarding_complete, voice_controls_missing).map_or(Ok(()), |refusal| {
        Err(match refusal {
            RegistrationRefusal::OnboardingIncomplete => labels.onboarding_incomplete.clone(),
            RegistrationRefusal::VoiceControlsUnavailable => {
                labels.voice_controls_unavailable.clone()
            }
        })
    })
}

/// The refusal [`registration_gate`] words, if any.
pub fn registration_refusal(
    onboarding_complete: bool,
    voice_controls_missing: impl FnOnce() -> bool,
) -> Option<RegistrationRefusal> {
    if !onboarding_complete {
        return Some(RegistrationRefusal::OnboardingIncomplete);
    }
    if voice_controls_missing() {
        return Some(RegistrationRefusal::VoiceControlsUnavailable);
    }
    None
}

/// After the shortcut hook activated: whether the onboarding dictation test hotkeys switch on
/// (the hook is active and onboarding is not complete).
pub fn enable_onboarding_dictation_test(shortcuts_active: bool, onboarding_complete: bool) -> bool {
    shortcuts_active && !onboarding_complete
}
