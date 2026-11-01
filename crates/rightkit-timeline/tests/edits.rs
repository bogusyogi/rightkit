mod common;
use common::*;
use rightkit_timeline::*;

// Kept unit tests: hard-to-provoke rejections an engine-level e2e cannot
// drive without a contrived timeline. Ripple/trim/move/split/slip/batch/
// undo/redo behaviour is covered end to end by
// `cutright-engine/tests/editing_e2e.rs`.

fn base() -> TimelineStore {
    store_with(vec![
        video_source("a", 20.0, true),
        video_source("b", 20.0, false),
        audio_source("voice", 30.0),
        insert("c1", "a", 0, 0.0, 2.0), // 60f at 0
        insert("c2", "b", 1, 0.0, 3.0), // 90f at 60
    ])
}

#[test]
fn ripple_that_would_push_narration_before_zero_is_rejected() {
    let mut s = base();
    s.apply(narration("n", "voice", 1.0, 0.0, 1.0, Some("c2")))
        .unwrap(); // c2 at 2s, n at 1s
    let r = s.apply(Action::RemoveClip {
        clip_id: "c1".into(),
    }); // c2 shifts -2s
    assert!(matches!(r, Err(TimelineError::NegativeStart(_))));
}

#[test]
fn linking_requires_a_narration_track() {
    let mut s = base();
    s.apply(Action::AddAudioTrack {
        track_id: "M1".into(),
        role: AudioRole::Music,
    })
    .unwrap();
    s.apply(Action::AddAudioClip {
        track_id: "M1".into(),
        clip: AudioClip {
            id: "m".into(),
            source: sid("voice"),
            trim_in: secs(0.0),
            trim_out: secs(1.0),
            start: secs(0.0),
            gain: MilliDb::ZERO,
            link: None,
            under: UnderAudio::Keep,
        },
    })
    .unwrap();
    assert!(matches!(
        s.apply(Action::LinkNarration {
            audio_id: "m".into(),
            video_id: "c1".into()
        }),
        Err(TimelineError::NotNarration(_))
    ));
}
