use super::*;

#[test]
fn sustained_repeat_drops_capture_and_admits_a_fresh_session() {
    let block = vec![0.0, 0.25, -0.25];
    let first = validate_capture_pcm(&block, None).unwrap().history();
    let second = validate_capture_pcm(&block, Some(&first)).unwrap();
    assert!(second.discarded_repeat());
    assert!(validate_capture_pcm(&block, Some(&second.history())).is_err());

    // The worker drops the failed stream, so the next start reopens it.
    assert!(capture_should_reopen(true, false));
    assert!(validate_capture_pcm(&block, None).is_ok());
}
