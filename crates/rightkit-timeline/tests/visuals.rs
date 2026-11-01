//! Durable clip visuals: keyed opacity, transform, crop and cross-fade are
//! stored on the timeline, survive save and reopen, undo to the prior values,
//! and reject out-of-range keys before they can reach a renderer.

mod common;
use common::*;
use rightkit_timeline::*;

fn key<T>(secs: f64, value: T) -> ClipKey<T> {
    ClipKey {
        at: secs_ticks(secs),
        value,
    }
}

fn secs_ticks(s: f64) -> Ticks {
    Ticks((s * TICKS_PER_SECOND as f64).round() as i64)
}

fn visuals() -> ClipVisualKeys {
    ClipVisualKeys {
        opacity: vec![key(0.0, 1000), key(1.0, 250)],
        transform: vec![
            key(
                0.0,
                KeyedTransform {
                    dx: 0,
                    dy: 0,
                    scale: 1000,
                },
            ),
            key(
                1.0,
                KeyedTransform {
                    dx: 100,
                    dy: -50,
                    scale: 500,
                },
            ),
        ],
        crop: vec![
            key(
                0.0,
                KeyedCrop {
                    x: 0,
                    y: 0,
                    width: 1000,
                    height: 1000,
                },
            ),
            key(
                1.0,
                KeyedCrop {
                    x: 250,
                    y: 0,
                    width: 750,
                    height: 500,
                },
            ),
        ],
        cross_fade_in_frames: 12,
    }
}

fn base() -> TimelineStore {
    store_with(vec![
        video_source("a", 20.0, false),
        insert("c1", "a", 0, 0.0, 3.0),
    ])
}

fn set(s: &mut TimelineStore, v: ClipVisualKeys) {
    s.apply(Action::SetClipVisuals {
        clip_id: "c1".into(),
        visuals: v,
    })
    .unwrap();
}

#[test]
fn keys_round_trip_through_save_and_reopen() {
    let mut s = base();
    set(&mut s, visuals());
    let bytes = s.to_canonical_json();
    let reopened = TimelineStore::from_canonical_json(&bytes).unwrap();
    assert_eq!(reopened.timeline(), s.timeline());
    assert_eq!(
        reopened.timeline().corrections.visuals.get("c1"),
        Some(&visuals())
    );
    assert_eq!(reopened.to_canonical_json(), bytes);
}

#[test]
fn undo_restores_prior_visuals_and_redo_reapplies() {
    let mut s = base();
    let before = s.head_hash();
    set(&mut s, visuals());
    let mut changed = visuals();
    changed.cross_fade_in_frames = 0;
    set(&mut s, changed.clone());
    assert_eq!(s.timeline().corrections.visuals.get("c1"), Some(&changed));

    s.undo().unwrap();
    assert_eq!(s.timeline().corrections.visuals.get("c1"), Some(&visuals()));
    s.undo().unwrap();
    assert!(s.timeline().corrections.visuals.is_empty());
    assert_eq!(s.head_hash(), before);

    s.redo().unwrap();
    s.redo().unwrap();
    assert_eq!(s.timeline().corrections.visuals.get("c1"), Some(&changed));

    // undo still works after a reopen
    let mut r = TimelineStore::from_canonical_json(&s.to_canonical_json()).unwrap();
    r.undo().unwrap();
    assert_eq!(r.timeline().corrections.visuals.get("c1"), Some(&visuals()));
}

#[test]
fn empty_visuals_clear_the_entry() {
    let mut s = base();
    set(&mut s, visuals());
    set(&mut s, ClipVisualKeys::default());
    assert!(s.timeline().corrections.visuals.is_empty());
}

#[test]
fn projects_without_visuals_keep_their_canonical_shape() {
    let s = base();
    let text = String::from_utf8(s.to_canonical_json()).unwrap();
    assert!(!text.contains("\"visuals\""));
    assert!(!text.contains("cross_fade_in_frames"));
}

#[test]
fn out_of_range_visuals_are_rejected_without_a_trace() {
    let mut s = base();
    let mut bad = visuals();
    bad.crop = vec![key(
        0.0,
        KeyedCrop {
            x: 900,
            y: 0,
            width: 200,
            height: 100,
        },
    )];
    let err = s
        .apply(Action::SetClipVisuals {
            clip_id: "c1".into(),
            visuals: bad,
        })
        .unwrap_err();
    assert!(matches!(err, TimelineError::InvalidRange(_, _)));
    assert!(s.timeline().corrections.visuals.is_empty());

    let mut unsorted = visuals();
    unsorted.opacity = vec![key(1.0, 500), key(0.5, 500)];
    assert!(s
        .apply(Action::SetClipVisuals {
            clip_id: "c1".into(),
            visuals: unsorted,
        })
        .is_err());
}

#[test]
fn visuals_on_unknown_clip_are_rejected() {
    let mut s = base();
    let err = s
        .apply(Action::SetClipVisuals {
            clip_id: "missing".into(),
            visuals: visuals(),
        })
        .unwrap_err();
    assert_eq!(err, TimelineError::UnknownClip("missing".into()));
}

#[test]
fn removing_the_clip_prunes_its_visuals() {
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::RemoveClip {
        clip_id: "c1".into(),
    })
    .unwrap();
    assert!(s.timeline().corrections.visuals.is_empty());
}

fn transform(dx: i64, dy: i64, scale: i64) -> KeyedTransform {
    KeyedTransform { dx, dy, scale }
}

fn crop(x: i64, y: i64, width: i64, height: i64) -> KeyedCrop {
    KeyedCrop {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn split_divides_keys_and_rebases_the_right_half() {
    // Keys in `visuals()` sit at 0s and 1s. Split at 0.5s: each value at the
    // split is the linear value of the curve there (opacity 1000->250 gives 625).
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::SplitClip {
        clip_id: "c1".into(),
        at: secs_ticks(0.5),
        new_clip_id: "c2".into(),
    })
    .unwrap();

    let v = &s.timeline().corrections.visuals;
    let left = v.get("c1").expect("left half keeps keys");
    let right = v.get("c2").expect("right half gets keys");

    assert_eq!(
        left.opacity,
        vec![key(0.0, 1000), key(0.5, 625)],
        "left opacity closes at the split"
    );
    assert_eq!(
        right.opacity,
        vec![key(0.0, 625), key(0.5, 250)],
        "right opacity is rebased to its first frame"
    );
    assert_eq!(
        left.transform,
        vec![
            key(0.0, transform(0, 0, 1000)),
            key(0.5, transform(50, -25, 750)),
        ]
    );
    assert_eq!(
        right.transform,
        vec![
            key(0.0, transform(50, -25, 750)),
            key(0.5, transform(100, -50, 500)),
        ]
    );
    assert_eq!(
        left.crop,
        vec![
            key(0.0, crop(0, 0, 1000, 1000)),
            key(0.5, crop(125, 0, 875, 750))
        ]
    );
    assert_eq!(
        right.crop,
        vec![
            key(0.0, crop(125, 0, 875, 750)),
            key(0.5, crop(250, 0, 750, 500))
        ]
    );
    // The incoming cross-fade stays with the left half; the right half is a cut.
    assert_eq!(left.cross_fade_in_frames, 12);
    assert_eq!(right.cross_fade_in_frames, 0);
}

#[test]
fn split_after_the_last_key_leaves_a_held_value_on_the_right() {
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::SplitClip {
        clip_id: "c1".into(),
        at: secs_ticks(1.5),
        new_clip_id: "c2".into(),
    })
    .unwrap();
    let v = &s.timeline().corrections.visuals;
    assert_eq!(
        v.get("c1").unwrap().opacity,
        vec![key(0.0, 1000), key(1.0, 250)]
    );
    assert_eq!(v.get("c2").unwrap().opacity, vec![key(0.0, 250)]);
}

#[test]
fn undo_of_a_split_restores_the_keys_to_one_clip() {
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::SplitClip {
        clip_id: "c1".into(),
        at: secs_ticks(0.5),
        new_clip_id: "c2".into(),
    })
    .unwrap();
    s.undo().unwrap();
    let v = &s.timeline().corrections.visuals;
    assert_eq!(v.get("c1"), Some(&visuals()));
    assert!(!v.contains_key("c2"));
}

#[test]
fn slip_moves_keys_with_the_clip_content() {
    // A slip of +0.5s moves the source window forward, so each key moves back
    // 0.5s with its content: the value at 0.5s becomes the new start.
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::SlipClip {
        clip_id: "c1".into(),
        delta: secs_ticks(0.5),
    })
    .unwrap();
    let v = s.timeline().corrections.visuals.get("c1").unwrap().clone();
    assert_eq!(v.opacity, vec![key(0.0, 625), key(0.5, 250)]);
    assert_eq!(
        v.transform,
        vec![
            key(0.0, transform(50, -25, 750)),
            key(0.5, transform(100, -50, 500)),
        ]
    );
    assert_eq!(
        v.crop,
        vec![
            key(0.0, crop(125, 0, 875, 750)),
            key(0.5, crop(250, 0, 750, 500))
        ]
    );
    assert_eq!(v.cross_fade_in_frames, 12);
}

#[test]
fn slip_past_the_last_key_holds_the_last_value() {
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::SlipClip {
        clip_id: "c1".into(),
        delta: secs_ticks(1.5),
    })
    .unwrap();
    let v = s.timeline().corrections.visuals.get("c1").unwrap().clone();
    assert_eq!(v.opacity, vec![key(0.0, 250)]);
    assert_eq!(v.transform, vec![key(0.0, transform(100, -50, 500))]);
    assert_eq!(v.crop, vec![key(0.0, crop(250, 0, 750, 500))]);
}

#[test]
fn undo_of_a_slip_restores_the_keys() {
    let mut s = base();
    set(&mut s, visuals());
    s.apply(Action::SlipClip {
        clip_id: "c1".into(),
        delta: secs_ticks(0.5),
    })
    .unwrap();
    s.undo().unwrap();
    assert_eq!(s.timeline().corrections.visuals.get("c1"), Some(&visuals()));
}
