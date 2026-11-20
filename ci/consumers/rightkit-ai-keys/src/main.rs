use rightkit_ai_keys::{
    classify_key_read, classify_status, validate_key, KeyCheckResult, SidecarKeyState,
};

fn main() {
    assert_eq!(classify_status(204), KeyCheckResult::Valid);
    assert_eq!(classify_status(403), KeyCheckResult::Rejected);
    assert_eq!(classify_status(302), KeyCheckResult::Unverified);
    assert_eq!(classify_key_read(Ok(String::new())).state, SidecarKeyState::Absent);
    assert_eq!(
        classify_key_read(Err("synthetic failure".to_owned())).state,
        SidecarKeyState::ReadError,
    );
    assert!(validate_key("Synthetic provider", "\n\t").is_err());
}
