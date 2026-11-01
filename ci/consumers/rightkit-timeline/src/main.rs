// rightkit-timeline 0.1.0 consumed by exact registry pin (default features), outside the workspace.
//
// Proves the edit model a host drives: registering sources, inserting video clips, the timeline
// duration in ticks, a rejected edit leaving the timeline unchanged, undo, and the OpenTimelineIO
// export of the result passing the crate's own shape validator.
// Does not prove: FCPXML export with media collection, the render-plan compiler's FFmpeg run (no
// FFmpeg here), persistence to disk. No network, no files.
use rightkit_timeline::otio::{to_otio, validate_otio, ExportOptions};
use rightkit_timeline::*;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn sid(c: char) -> SourceId {
    SourceId::new(std::iter::repeat(c).take(64).collect::<String>()).expect("64 hex characters")
}

fn secs(s: f64) -> Ticks {
    Ticks((s * TICKS_PER_SECOND as f64).round() as i64)
}

fn main() {
    let mut store = TimelineStore::new(Fps { num: 30, den: 1 });
    for (c, len) in [('a', 20.0), ('b', 20.0)] {
        let source = SourceInfo {
            id: sid(c),
            duration: secs(len),
            video: Some(VideoStreamInfo {
                width: 1920,
                height: 1080,
                duration: None,
            }),
            has_audio: true,
        };
        store
            .apply(Action::RegisterSource { source })
            .expect("register");
    }
    let insert = |id: &str, c: char, index: usize, from: f64, to: f64| Action::InsertClip {
        track_id: "V1".into(),
        index,
        clip: VideoClip {
            id: id.into(),
            source: sid(c),
            source_in: secs(from),
            source_out: secs(to),
        },
    };
    store
        .apply(insert("c1", 'a', 0, 0.0, 2.0))
        .expect("insert c1");
    store
        .apply(insert("c2", 'b', 1, 0.0, 3.0))
        .expect("insert c2");
    check(
        store.timeline().total_frames() == 150,
        "two clips give a five-second timeline",
    );
    let before = store.timeline().total_frames();
    check(
        store
            .apply(Action::RemoveClip {
                clip_id: "missing".into(),
            })
            .is_err(),
        "removing an unknown clip is refused",
    );
    check(
        store.timeline().total_frames() == before,
        "a refused edit leaves the timeline unchanged",
    );
    store.undo().expect("undo");
    check(
        store.timeline().total_frames() == 60,
        "undo removes the last insert",
    );
    // Media paths must be absolute on the host (a leading `/` is not absolute on Windows).
    let media = std::env::temp_dir();
    let sources: std::collections::BTreeMap<SourceId, std::path::PathBuf> =
        [('a', "a.mov"), ('b', "b.mov")]
            .into_iter()
            .map(|(c, p)| (sid(c), media.join(p)))
            .collect();
    let doc = to_otio(store.timeline(), &sources, &ExportOptions::new("consumer"));
    check(
        matches!(&doc, Ok((d, _)) if validate_otio(d).is_ok()),
        "the OTIO export passes the shape validator",
    );
    println!("rightkit-timeline consumer: all checks passed");
}
