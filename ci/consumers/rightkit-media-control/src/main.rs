use rightkit_media_control::{classify_post_observation, CommandOutcome, Observed};

fn main() {
    #[cfg(target_os = "macos")]
    let _mac_backend = rightkit_media_control::mac::MacBackend::new;
    let paused = Observed {
        player: 1,
        playing: false,
    };
    assert_eq!(
        classify_post_observation(&1, false, Some(&paused)),
        CommandOutcome::Confirmed
    );
    assert_eq!(
        classify_post_observation(&2, false, Some(&paused)),
        CommandOutcome::Rejected
    );
    assert_eq!(
        classify_post_observation(&1, true, Some(&paused)),
        CommandOutcome::Ambiguous
    );
    assert_eq!(
        classify_post_observation(&1, false, None),
        CommandOutcome::Ambiguous
    );
    println!("OK rightkit-media-control (registry consumer)");
}
