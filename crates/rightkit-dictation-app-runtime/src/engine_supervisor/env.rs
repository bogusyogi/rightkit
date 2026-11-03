// Sidecar environment scrub.
//
// `start()` runs `env_allowlist` (env_clear + a fixed passthrough list) on the child `Command`,
// which already drops every ambient variable. This module adds the owner rule for provider keys.

use std::process::Command;

pub(crate) fn strip_l3_env(command: &mut Command) {
    // Owner rule (rightkit-credentials `handoff`): an ambient copy of a provider key must not be
    // inherited, whatever the hand-off frame carries.
    rightkit_dictation_core::engine::provider_handoff::scrub_sidecar_env(command);
}
