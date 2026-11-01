#![allow(dead_code)]
use rightkit_timeline::*;

pub const FPS30: Fps = Fps { num: 30, den: 1 };

pub fn sid(name: &str) -> SourceId {
    SourceId::new(rightkit_fs::digest::blake3_bytes(name.as_bytes())).unwrap()
}

pub fn secs(s: f64) -> Ticks {
    Ticks((s * TICKS_PER_SECOND as f64).round() as i64)
}

pub fn video_source(name: &str, seconds: f64, has_audio: bool) -> Action {
    Action::RegisterSource {
        source: SourceInfo {
            id: sid(name),
            duration: secs(seconds),
            video: Some(VideoStreamInfo {
                width: 1920,
                height: 1080,
                duration: None,
            }),
            has_audio,
        },
    }
}

pub fn audio_source(name: &str, seconds: f64) -> Action {
    Action::RegisterSource {
        source: SourceInfo {
            id: sid(name),
            duration: secs(seconds),
            video: None,
            has_audio: true,
        },
    }
}

pub fn insert(id: &str, source: &str, index: usize, from: f64, to: f64) -> Action {
    Action::InsertClip {
        track_id: "V1".into(),
        index,
        clip: VideoClip {
            id: id.into(),
            source: sid(source),
            source_in: secs(from),
            source_out: secs(to),
        },
    }
}

pub fn narration(
    id: &str,
    source: &str,
    start: f64,
    from: f64,
    to: f64,
    link: Option<&str>,
) -> Action {
    Action::AddAudioClip {
        track_id: "A1".into(),
        clip: AudioClip {
            id: id.into(),
            source: sid(source),
            trim_in: secs(from),
            trim_out: secs(to),
            start: secs(start),
            gain: MilliDb::ZERO,
            link: link.map(Into::into),
            under: UnderAudio::Keep,
        },
    }
}

pub fn store_with(actions: Vec<Action>) -> TimelineStore {
    let mut s = TimelineStore::new(FPS30);
    for a in actions {
        s.apply(a).unwrap();
    }
    s
}
