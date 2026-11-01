//! Frame request/cache/scheduler tests — CPU only.
//!
//! Pin the stale-frame contract: revisions, cancellations and project switches
//! change identity, and identity mismatches can only miss/cancel, never serve.

use std::time::{Duration, Instant};

use rightkit_compose::native::{
    FrameCache, FrameDeadline, FrameKey, FramePriority, FrameRequest, FrameResource,
    FrameScheduler, SchedulerError, SnapshotIdentity,
};

fn identity(rev: u64) -> SnapshotIdentity {
    SnapshotIdentity {
        project: "proj".into(),
        revision: rev,
        timeline_blake3: format!("hash{rev}"),
    }
}

fn key(rev: u64, frame: i64) -> FrameKey {
    FrameKey {
        identity: identity(rev),
        frame,
        width: 1920,
        height: 1080,
        quality_scale: 1.0,
    }
}

#[test]
fn export_and_full_preview_share_frame_identity() {
    // An export frame (full quality, delivery size) and a full-quality preview
    // request at the same revision/frame resolve to the same cache key.
    let preview = key(3, 42);
    let export = FrameKey {
        identity: identity(3),
        frame: 42,
        width: 1920,
        height: 1080,
        quality_scale: 1.0,
    };
    assert_eq!(preview.cache_key(), export.cache_key());
    // A scaled preview is a different frame.
    let scrub = FrameKey {
        quality_scale: 0.5,
        width: 960,
        height: 540,
        ..preview.clone()
    };
    assert_ne!(scrub.cache_key(), preview.cache_key());
}

#[test]
fn cache_is_byte_bounded_and_lru() {
    let mut cache: FrameCache<Vec<u8>> = FrameCache::new(300);
    for frame in 0..4 {
        let inserted = cache.insert(FrameResource {
            key: key(1, frame),
            byte_len: 100,
            payload: vec![frame as u8],
        });
        assert!(inserted);
    }
    // 300-byte budget holds 3 frames; frame 0 was evicted.
    assert_eq!(cache.len(), 3);
    assert_eq!(cache.used_bytes(), 300);
    assert!(cache.get(&key(1, 0)).is_none());
    assert!(cache.get(&key(1, 3)).is_some());
    // An oversized frame is refused, not stored.
    assert!(!cache.insert(FrameResource {
        key: key(1, 99),
        byte_len: 400,
        payload: vec![],
    }));
}

#[test]
fn invalidate_identity_drops_every_stale_frame() {
    let mut cache: FrameCache<Vec<u8>> = FrameCache::new(10_000);
    for rev in [1, 2] {
        for frame in 0..3 {
            cache.insert(FrameResource {
                key: key(rev, frame),
                byte_len: 10,
                payload: vec![],
            });
        }
    }
    assert_eq!(cache.len(), 6);
    // New revision supersedes rev 1.
    cache.invalidate_identity(&identity(1));
    assert_eq!(cache.len(), 3);
    for frame in 0..3 {
        assert!(cache.get(&key(1, frame)).is_none());
        assert!(cache.get(&key(2, frame)).is_some());
    }
}

#[test]
fn scheduler_orders_by_deadline_then_priority() {
    let mut sched = FrameScheduler::new(16, identity(1));
    let later = FrameRequest {
        key: key(1, 2),
        deadline: FrameDeadline {
            present_by: Instant::now() + Duration::from_millis(50),
            priority: FramePriority::Realtime,
        },
    };
    let sooner = FrameRequest {
        key: key(1, 1),
        deadline: FrameDeadline {
            present_by: Instant::now() + Duration::from_millis(10),
            priority: FramePriority::Realtime,
        },
    };
    sched.request(later).unwrap();
    sched.request(sooner.clone()).unwrap();
    // Earliest deadline wins even though it was queued second.
    assert_eq!(sched.next().unwrap().key.frame, 1);
    assert_eq!(sched.next().unwrap().key.frame, 2);
    assert!(sched.next().is_none());
}

#[test]
fn scheduler_rejects_stale_identity_and_advance_cancels() {
    let mut sched = FrameScheduler::new(16, identity(1));
    let stale = FrameRequest {
        key: key(0, 5),
        deadline: FrameDeadline::immediate(FramePriority::Interactive),
    };
    assert!(matches!(
        sched.request(stale),
        Err(SchedulerError::StaleIdentity)
    ));
    // Queue work for current identity, then advance (project switch/revision):
    // everything pending is cancelled and old-identity requests now fail.
    sched
        .request(FrameRequest {
            key: key(1, 1),
            deadline: FrameDeadline::immediate(FramePriority::Interactive),
        })
        .unwrap();
    sched
        .request(FrameRequest {
            key: key(1, 2),
            deadline: FrameDeadline::immediate(FramePriority::Realtime),
        })
        .unwrap();
    sched.advance(identity(2));
    assert_eq!(sched.stats().cancelled, 2);
    assert_eq!(sched.pending(), 0);
    assert!(sched.next().is_none());
    assert!(matches!(
        sched.request(FrameRequest {
            key: key(1, 3),
            deadline: FrameDeadline::immediate(FramePriority::Interactive),
        }),
        Err(SchedulerError::StaleIdentity)
    ));
}

#[test]
fn scheduler_drops_expired_realtime_but_keeps_interactive() {
    let mut sched = FrameScheduler::new(16, identity(1));
    let past = Instant::now() - Duration::from_millis(5);
    sched
        .request(FrameRequest {
            key: key(1, 1),
            deadline: FrameDeadline {
                present_by: past,
                priority: FramePriority::Realtime,
            },
        })
        .unwrap();
    sched
        .request(FrameRequest {
            key: key(1, 2),
            deadline: FrameDeadline {
                present_by: past,
                priority: FramePriority::Interactive,
            },
        })
        .unwrap();
    // Expired realtime work is dropped; expired interactive work is still done
    // (a seek is useless to drop — the user is waiting for exactly that frame).
    let next = sched.next().unwrap();
    assert_eq!(next.key.frame, 2);
    assert_eq!(sched.stats().dropped_expired, 1);
}

#[test]
fn scheduler_overflow_drops_lowest_priority() {
    let mut sched = FrameScheduler::new(2, identity(1));
    for (frame, priority) in [
        (1, FramePriority::Background),
        (2, FramePriority::Interactive),
    ] {
        sched
            .request(FrameRequest {
                key: key(1, frame),
                deadline: FrameDeadline {
                    present_by: Instant::now() + Duration::from_secs(60),
                    priority,
                },
            })
            .unwrap();
    }
    // Third, higher-priority request overflows the queue: Background drops.
    sched
        .request(FrameRequest {
            key: key(1, 3),
            deadline: FrameDeadline {
                present_by: Instant::now() + Duration::from_secs(60),
                priority: FramePriority::Realtime,
            },
        })
        .unwrap();
    assert_eq!(sched.pending(), 2);
    let mut frames = vec![
        sched.next().unwrap().key.frame,
        sched.next().unwrap().key.frame,
    ];
    frames.sort();
    assert_eq!(frames, vec![2, 3]);
    assert_eq!(sched.stats().dropped_overflow, 1);
}
