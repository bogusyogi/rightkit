//! Windows UI Automation bridge: a persistent MTA COM worker serves read-only
//! probes, while value mutations run on a detached one-shot thread behind a
//! permanent circuit breaker. Public API below is re-exported from `api`.

mod api;
mod protocol;
mod worker;

pub(crate) use api::{
    capture_stop_target, focused_element_identity, focused_field_is_password, focused_text_input,
    focused_text_with_identity, release_stop_target, restore_stop_target, stop_target_is_gone,
    stop_target_names_web_container, stop_target_text, validate_stop_target,
};
pub(crate) use protocol::{FocusedElementIdentity, StopTarget, TextProof};
