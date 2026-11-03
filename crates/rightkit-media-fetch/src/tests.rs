use super::*;

mod commands;
mod output_limits;

#[test]
fn verified_ffmpeg_refuses_a_path_that_does_not_exist() {
    let missing = std::env::temp_dir().join("rightkit-missing-ffmpeg-for-verify-test");
    let err = verify_ffmpeg_path(&missing).unwrap_err();
    // Hosts without an approved FFmpeg artifact (Linux) refuse before looking at the path.
    if cfg!(any(target_os = "macos", windows)) {
        assert!(err.starts_with("failed to launch ffmpeg"), "{err}");
    } else {
        assert!(err.contains("no approved FFmpeg artifact"), "{err}");
    }
}

#[test]
fn images_action_plans_no_media_fetch_and_no_wav() {
    let plan = FetchPlan::from_media_spec(Some(MediaSpec {
        action: MediaAction::Images,
        video: Some(VideoQuality::P1080),
        audio: None,
    }));
    assert_eq!(plan.download, MediaDownload::None);
    assert!(!plan.needs_wav);
}
