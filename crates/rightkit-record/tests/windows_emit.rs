//! Drives the Windows backend's event emission through the core's protocol
//! checks. Frames are synthetic and no window is involved, so this runs on any
//! Windows host. It exercises the same emitter the capture worker calls.
#![cfg(windows)]

mod common;

use common::{read_manifest, recorder, request, Scratch};
use rightkit_contracts::types::record_event::DropReason;
use rightkit_contracts::types::record_session::BundleStatus;
use rightkit_record::fake::FakeBackend;
use rightkit_record::hash::blake3_text;
use rightkit_record::windows::emit::Emitter;
use rightkit_record::windows::Losses;
use rightkit_record::HelperEvent;

const T0: u64 = 1_000_000;
const FPS: u32 = 60;
const BASE_NS: u64 = 9_000_000_000;
/// One 60 Hz frame interval in nanoseconds.
const INTERVAL_NS: u64 = 1_000_000_000 / 60;

/// Synthetic packed BGRA pixels for one frame, distinct per index.
fn pixels(index: u64) -> Vec<u8> {
    (0..64u64).map(|b| (b as u8).wrapping_add(index as u8)).collect()
}

/// Runs arrivals through the emitter in the worker's order: offer, drain the
/// closed drop runs, then keep the frame and hash its pixels.
fn emit(arrivals: &[(u64, Losses)]) -> Vec<HelperEvent> {
    let mut emitter = Emitter::new(FPS);
    let mut events = Vec::new();
    for &(host_ns, losses) in arrivals {
        let kept = emitter.offer(host_ns, losses);
        emitter.drain(&mut events);
        if kept {
            let frame = emitter.keep(host_ns);
            events.push(frame.event(blake3_text(&pixels(frame.index))));
        }
    }
    emitter.finish(&mut events);
    events
}

#[test]
fn synthetic_windows_frames_pass_the_core_protocol_checks() {
    let mut arrivals = Vec::new();
    // 30 frames at 60 Hz. Two frames are lost to callback backpressure before frame 12.
    for i in 0..30u64 {
        let losses = if i == 12 { Losses { backpressure: 2, unknown: 0 } } else { Losses::default() };
        arrivals.push((BASE_NS + i * INTERVAL_NS, losses));
    }
    // Static content: three seconds with no frames delivered, then frames resume.
    let resume = BASE_NS + 29 * INTERVAL_NS + 3_000_000_000;
    for i in 0..10u64 {
        arrivals.push((resume + i * INTERVAL_NS, Losses::default()));
    }

    let events = emit(&arrivals);
    assert!(matches!(events.first(), Some(HelperEvent::Ready(_))), "readiness marker comes first");
    let drops: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            HelperEvent::Dropped { count, reason, .. } => Some((*count, *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(drops, vec![(2, DropReason::Backpressure)], "silence adds no drop event");

    let scratch = Scratch::new("win-synthetic");
    let out = scratch.bundle("take");
    let mut backend = FakeBackend::windows();
    backend.push(events);
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).expect("start");
    let outcome = rec.record_stop().expect("stop");
    assert_eq!(outcome.status, BundleStatus::Complete, "{outcome:?}");
    assert_eq!(outcome.reason, None);
    let manifest = read_manifest(&out);
    assert_eq!(manifest["frames"]["count"], 40, "30 frames plus 10 after the silence");
    assert_eq!(manifest["frames"]["droppedCount"], 2);
}

#[test]
fn static_content_with_no_losses_seals_complete_with_no_drops() {
    let arrivals = vec![
        (BASE_NS, Losses::default()),
        (BASE_NS + 300 * INTERVAL_NS, Losses::default()),
        (BASE_NS + 600 * INTERVAL_NS, Losses::default()),
    ];
    let events = emit(&arrivals);
    assert!(events.iter().all(|e| !matches!(e, HelperEvent::Dropped { .. })), "{events:?}");

    let scratch = Scratch::new("win-silence");
    let out = scratch.bundle("take");
    let mut backend = FakeBackend::windows();
    backend.push(events);
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).expect("start");
    let outcome = rec.record_stop().expect("stop");
    assert_eq!(outcome.status, BundleStatus::Complete, "{outcome:?}");
    assert_eq!(read_manifest(&out)["frames"]["droppedCount"], 0);
}
