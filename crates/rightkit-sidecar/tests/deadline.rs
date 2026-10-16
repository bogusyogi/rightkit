//! Deadline-bounded framing over real Unix descriptors: socketpairs and
//! `std::io::pipe`. Peers are either plain blocking streams or threads, so the
//! tests exercise the poll(2) wait path and partial transfers.
#![cfg(unix)]

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use rightkit_sidecar::length_prefixed;
use rightkit_sidecar::{read_frame_deadline, write_frame_deadline, DeadlineFramed, FrameError};

const LIMIT: usize = 8 << 20;

fn soon(within: Duration) -> Instant {
    Instant::now() + within
}

/// A framed end plus a plain blocking peer end.
fn pair() -> (DeadlineFramed<UnixStream>, UnixStream) {
    let (ours, peer) = UnixStream::pair().unwrap();
    (DeadlineFramed::new(ours).unwrap(), peer)
}

fn timed_out_bytes(err: FrameError) -> usize {
    match err {
        FrameError::TimedOut { partial_bytes } => partial_bytes,
        other => panic!("expected TimedOut, got {other:?}"),
    }
}

#[test]
fn socketpair_round_trip_both_directions() {
    let (mut ours, mut peer) = pair();
    ours.write_frame(b"ping", soon(Duration::from_secs(5)), 1024)
        .unwrap();
    let got = length_prefixed::read_frame_bytes(&mut peer, 1024)
        .unwrap()
        .unwrap();
    assert_eq!(got, b"ping");

    length_prefixed::write_frame_bytes(&mut peer, b"pong", 1024).unwrap();
    let back = ours.read_frame(soon(Duration::from_secs(5)), 1024).unwrap();
    assert_eq!(back, b"pong");
}

#[test]
fn pipe_round_trip_with_std_pipe() {
    let (reader, writer) = std::io::pipe().unwrap();
    let mut rx = DeadlineFramed::new(reader).unwrap();
    let mut tx = DeadlineFramed::new(writer).unwrap();

    tx.write_frame(b"over a pipe", soon(Duration::from_secs(5)), 1024)
        .unwrap();
    let got = rx.read_frame(soon(Duration::from_secs(5)), 1024).unwrap();
    assert_eq!(got, b"over a pipe");
}

#[test]
fn free_functions_round_trip_on_a_nonblocking_socket() {
    let (ours, mut peer) = UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();

    write_frame_deadline(&ours, b"raw", soon(Duration::from_secs(5)), 1024).unwrap();
    let got = length_prefixed::read_frame_bytes(&mut peer, 1024)
        .unwrap()
        .unwrap();
    assert_eq!(got, b"raw");

    length_prefixed::write_frame_bytes(&mut peer, b"back", 1024).unwrap();
    let back = read_frame_deadline(&ours, soon(Duration::from_secs(5)), 1024).unwrap();
    assert_eq!(back, b"back");
}

#[test]
fn free_functions_reject_a_blocking_descriptor_without_touching_it() {
    let (ours, _peer) = UnixStream::pair().unwrap();

    let err = read_frame_deadline(&ours, soon(Duration::from_secs(1)), 1024).unwrap_err();
    assert!(matches!(&err, FrameError::Io(e) if e.kind() == ErrorKind::InvalidInput));

    let err = write_frame_deadline(&ours, b"x", soon(Duration::from_secs(1)), 1024).unwrap_err();
    assert!(matches!(&err, FrameError::Io(e) if e.kind() == ErrorKind::InvalidInput));

    // The mode was checked, not changed: the descriptor is still blocking.
    use std::os::unix::io::AsRawFd;
    // SAFETY: F_GETFL only reads status flags of a descriptor we own.
    let flags = unsafe { libc::fcntl(ours.as_raw_fd(), libc::F_GETFL) };
    assert_eq!(flags & libc::O_NONBLOCK, 0);
}

#[test]
fn silent_peer_times_out_with_no_bytes_consumed_and_stream_still_usable() {
    let (mut ours, mut peer) = pair();
    let start = Instant::now();

    let err = ours
        .read_frame(soon(Duration::from_millis(100)), 1024)
        .unwrap_err();
    assert_eq!(timed_out_bytes(err), 0);
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(90) && waited < Duration::from_secs(5),
        "waited {waited:?}"
    );

    // Nothing was consumed, so the next frame still parses from a clean boundary.
    length_prefixed::write_frame_bytes(&mut peer, b"later", 1024).unwrap();
    let got = ours.read_frame(soon(Duration::from_secs(5)), 1024).unwrap();
    assert_eq!(got, b"later");
}

#[test]
fn partial_header_reports_the_bytes_already_received() {
    let (mut ours, mut peer) = pair();
    peer.write_all(&[0, 0]).unwrap();

    let err = ours
        .read_frame(soon(Duration::from_millis(150)), 1024)
        .unwrap_err();
    assert_eq!(timed_out_bytes(err), 2);
}

#[test]
fn partial_body_reports_header_plus_received_body_bytes() {
    let (mut ours, mut peer) = pair();
    peer.write_all(&10u32.to_be_bytes()).unwrap();
    peer.write_all(b"abc").unwrap();

    let err = ours
        .read_frame(soon(Duration::from_millis(150)), 1024)
        .unwrap_err();
    assert_eq!(timed_out_bytes(err), 4 + 3);
}

#[test]
fn deadline_already_passed_does_not_touch_the_stream() {
    let (mut ours, mut peer) = pair();
    length_prefixed::write_frame_bytes(&mut peer, b"kept", 1024).unwrap();

    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("monotonic clock is past one second");
    let err = ours.read_frame(past, 1024).unwrap_err();
    assert_eq!(timed_out_bytes(err), 0);

    let got = ours.read_frame(soon(Duration::from_secs(5)), 1024).unwrap();
    assert_eq!(got, b"kept");
}

#[test]
fn large_frame_written_across_partial_writes_resumes_until_complete() {
    let (mut ours, mut peer) = pair();
    let body: Vec<u8> = (0..4usize << 20).map(|i| (i % 251) as u8).collect();
    let expected = body.clone();

    // The reader waits first, so the 4 MiB write must fill the socket buffer,
    // wait for POLLOUT, and resume.
    let reader = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        length_prefixed::read_frame_bytes(&mut peer, LIMIT)
            .unwrap()
            .unwrap()
    });

    ours.write_frame(&body, soon(Duration::from_secs(30)), LIMIT)
        .unwrap();
    let got = reader.join().unwrap();
    assert!(got == expected, "frame differs after partial writes");
}

#[test]
fn large_frame_read_across_partial_reads_resumes_until_complete() {
    let (mut ours, mut peer) = pair();
    let body: Vec<u8> = (0..4usize << 20).map(|i| (i % 253) as u8).collect();
    let expected = body.clone();

    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        length_prefixed::write_frame_bytes(&mut peer, &body, LIMIT).unwrap();
    });

    let got = ours
        .read_frame(soon(Duration::from_secs(30)), LIMIT)
        .unwrap();
    writer.join().unwrap();
    assert!(got == expected, "frame differs after partial reads");
}

#[test]
fn write_to_a_peer_that_never_reads_times_out_with_partial_progress() {
    let (mut ours, _peer) = pair();
    let body = vec![7u8; 8 << 20];
    let total = 4 + body.len();

    let err = ours
        .write_frame(&body, soon(Duration::from_millis(200)), LIMIT)
        .unwrap_err();
    let partial = timed_out_bytes(err);
    assert!(
        partial > 0 && partial < total,
        "partial {partial} of {total}"
    );
}

#[test]
fn declared_length_above_the_limit_is_too_large_and_body_is_not_consumed() {
    let (mut ours, mut peer) = pair();
    peer.write_all(&1000u32.to_be_bytes()).unwrap();

    match ours.read_frame(soon(Duration::from_secs(5)), 100) {
        Err(FrameError::TooLarge { limit }) => assert_eq!(limit, 100),
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn zero_length_frame_is_invalid_data() {
    let (mut ours, mut peer) = pair();
    peer.write_all(&0u32.to_be_bytes()).unwrap();

    match ours.read_frame(soon(Duration::from_secs(5)), 100) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), ErrorKind::InvalidData),
        other => panic!("expected InvalidData, got {other:?}"),
    }
}

#[test]
fn peer_close_mid_frame_and_on_a_boundary_are_unexpected_eof() {
    let (mut ours, mut peer) = pair();
    peer.write_all(&10u32.to_be_bytes()).unwrap();
    peer.write_all(b"abc").unwrap();
    drop(peer);
    match ours.read_frame(soon(Duration::from_secs(5)), LIMIT) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), ErrorKind::UnexpectedEof),
        other => panic!("expected UnexpectedEof mid-frame, got {other:?}"),
    }

    let (mut ours, peer) = pair();
    drop(peer);
    match ours.read_frame(soon(Duration::from_secs(5)), LIMIT) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), ErrorKind::UnexpectedEof),
        other => panic!("expected UnexpectedEof at boundary, got {other:?}"),
    }
}

#[test]
fn write_too_large_or_empty_writes_nothing() {
    let (mut ours, mut peer) = pair();

    match ours.write_frame(&[1u8; 200], soon(Duration::from_secs(5)), 100) {
        Err(FrameError::TooLarge { limit }) => assert_eq!(limit, 100),
        other => panic!("expected TooLarge, got {other:?}"),
    }
    match ours.write_frame(&[], soon(Duration::from_secs(5)), 100) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), ErrorKind::InvalidData),
        other => panic!("expected InvalidData, got {other:?}"),
    }

    peer.set_nonblocking(true).unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(
        peer.read(&mut byte).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
fn timed_out_display_names_the_partial_count() {
    let text = FrameError::TimedOut { partial_bytes: 3 }.to_string();
    assert!(text.contains('3'), "{text}");
}

#[test]
fn into_inner_returns_the_stream_still_in_nonblocking_mode() {
    let (framed, _peer) = pair();
    let mut inner = framed.into_inner();
    let mut byte = [0u8; 1];
    // Nothing is queued and the descriptor is nonblocking, so read reports
    // WouldBlock rather than parking the test thread.
    assert_eq!(
        inner.read(&mut byte).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}
