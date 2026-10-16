//! `read_frame_bytes_deadline` and `write_frame_bytes_deadline` over in-memory streams with a
//! scripted readiness callback: no descriptor, no platform code.

use std::io::{self, Cursor, Read, Write};
use std::time::{Duration, Instant};

use rightkit_sidecar::length_prefixed::{
    read_frame_bytes, read_frame_bytes_deadline, write_frame_bytes, write_frame_bytes_deadline,
};
use rightkit_sidecar::FrameError;

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn always_ready(_: Duration) -> io::Result<bool> {
    Ok(true)
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame_bytes(&mut out, body, 1 << 20).unwrap();
    out
}

/// Hands out at most `step` bytes per call, so frames arrive in pieces.
struct Trickle {
    data: Cursor<Vec<u8>>,
    step: usize,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.step);
        self.data.read(&mut buf[..n])
    }
}

#[test]
fn a_frame_in_pieces_round_trips_and_matches_the_blocking_codec() {
    let body = vec![7u8; 100_000];
    let wire = frame(&body);
    let mut reader = Trickle {
        data: Cursor::new(wire.clone()),
        step: 3,
    };
    let got = read_frame_bytes_deadline(&mut reader, 1 << 20, soon(), &mut always_ready).unwrap();
    assert_eq!(got, body);
    let blocking = read_frame_bytes(&mut Cursor::new(wire), 1 << 20).unwrap().unwrap();
    assert_eq!(blocking, got);
}

#[test]
fn two_frames_read_back_to_back_stay_on_the_boundary() {
    let mut wire = frame(b"one");
    wire.extend(frame(b"second"));
    let mut reader = Cursor::new(wire);
    let a = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap();
    let b = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap();
    assert_eq!((a.as_slice(), b.as_slice()), (&b"one"[..], &b"second"[..]));
}

#[test]
fn a_clean_end_between_frames_is_unexpected_eof_not_none() {
    let mut reader = Cursor::new(Vec::new());
    let err = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap_err();
    match err {
        FrameError::Io(e) => {
            assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
            assert!(e.to_string().contains("before a frame header"), "{e}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn short_headers_and_bodies_are_unexpected_eof() {
    for wire in [vec![0, 0], {
        let mut w = frame(b"abcdef");
        w.truncate(7);
        w
    }] {
        let mut reader = Cursor::new(wire);
        let err =
            read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap_err();
        assert!(
            matches!(&err, FrameError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof),
            "{err:?}"
        );
    }
}

#[test]
fn zero_and_oversized_lengths_are_refused_before_the_body() {
    let mut reader = Cursor::new(vec![0, 0, 0, 0, 1, 2]);
    let err = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap_err();
    assert!(
        matches!(&err, FrameError::Io(e) if e.kind() == io::ErrorKind::InvalidData),
        "{err:?}"
    );

    let mut reader = Cursor::new(frame(&[1u8; 65]));
    let err = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap_err();
    assert!(matches!(err, FrameError::TooLarge { limit: 64 }), "{err:?}");
    // Header consumed, body not.
    assert_eq!(reader.position(), 4);
}

#[test]
fn a_peer_that_never_becomes_ready_times_out_on_a_boundary() {
    let mut reader = Cursor::new(frame(b"never read"));
    let deadline = Instant::now() + Duration::from_millis(40);
    let mut not_ready = |left: Duration| -> io::Result<bool> {
        std::thread::sleep(left.min(Duration::from_millis(5)));
        Ok(false)
    };
    let err = read_frame_bytes_deadline(&mut reader, 64, deadline, &mut not_ready).unwrap_err();
    assert!(matches!(err, FrameError::TimedOut { partial_bytes: 0 }), "{err:?}");
    assert_eq!(reader.position(), 0);
}

#[test]
fn a_stall_mid_frame_reports_the_bytes_consumed() {
    // Ready for the first 6 bytes (4-byte header + 2 body bytes), then never.
    let wire = frame(b"abcdefgh");
    let mut reader = Trickle {
        data: Cursor::new(wire),
        step: 2,
    };
    let mut calls = 0;
    let mut stalls = |left: Duration| -> io::Result<bool> {
        calls += 1;
        if calls <= 3 {
            Ok(true)
        } else {
            std::thread::sleep(left.min(Duration::from_millis(5)));
            Ok(false)
        }
    };
    let deadline = Instant::now() + Duration::from_millis(60);
    let err = read_frame_bytes_deadline(&mut reader, 64, deadline, &mut stalls).unwrap_err();
    assert!(matches!(err, FrameError::TimedOut { partial_bytes: 6 }), "{err:?}");
}

#[test]
fn would_block_waits_for_the_next_readiness() {
    struct Flaky {
        data: Cursor<Vec<u8>>,
        block_next: bool,
    }
    impl Read for Flaky {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.block_next {
                self.block_next = false;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.block_next = true;
            let n = buf.len().min(1);
            self.data.read(&mut buf[..n])
        }
    }
    let mut reader = Flaky {
        data: Cursor::new(frame(b"xyz")),
        block_next: true,
    };
    let got = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut always_ready).unwrap();
    assert_eq!(got, b"xyz");
}

#[test]
fn a_readiness_error_fails_the_call() {
    let mut reader = Cursor::new(frame(b"abc"));
    let mut failing = |_: Duration| -> io::Result<bool> { Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone")) };
    let err = read_frame_bytes_deadline(&mut reader, 64, soon(), &mut failing).unwrap_err();
    assert!(
        matches!(&err, FrameError::Io(e) if e.kind() == io::ErrorKind::BrokenPipe),
        "{err:?}"
    );
}

#[test]
fn a_frame_written_in_pieces_equals_the_blocking_encoding() {
    struct Small {
        out: Vec<u8>,
        step: usize,
    }
    impl Write for Small {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = buf.len().min(self.step);
            self.out.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Small {
        out: Vec::new(),
        step: 5,
    };
    write_frame_bytes_deadline(&mut sink, b"hello frame", 64, soon(), &mut always_ready).unwrap();
    assert_eq!(sink.out, frame(b"hello frame"));
}

#[test]
fn an_empty_or_oversized_body_writes_nothing() {
    let mut out = Vec::new();
    let err =
        write_frame_bytes_deadline(&mut out, b"", 64, soon(), &mut always_ready).unwrap_err();
    assert!(
        matches!(&err, FrameError::Io(e) if e.kind() == io::ErrorKind::InvalidData),
        "{err:?}"
    );
    let err = write_frame_bytes_deadline(&mut out, &[0u8; 65], 64, soon(), &mut always_ready)
        .unwrap_err();
    assert!(matches!(err, FrameError::TooLarge { limit: 64 }), "{err:?}");
    assert!(out.is_empty());
}

#[test]
fn a_write_that_stalls_reports_the_bytes_the_peer_received() {
    struct Four {
        out: Vec<u8>,
    }
    impl Write for Four {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = buf.len().min(4);
            self.out.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Four { out: Vec::new() };
    let mut calls = 0;
    let mut stalls = |left: Duration| -> io::Result<bool> {
        calls += 1;
        if calls <= 2 {
            Ok(true)
        } else {
            std::thread::sleep(left.min(Duration::from_millis(5)));
            Ok(false)
        }
    };
    let deadline = Instant::now() + Duration::from_millis(60);
    let err = write_frame_bytes_deadline(&mut sink, b"0123456789", 64, deadline, &mut stalls)
        .unwrap_err();
    assert!(matches!(err, FrameError::TimedOut { partial_bytes: 8 }), "{err:?}");
    assert_eq!(sink.out.len(), 8);
}

#[test]
fn a_zero_byte_write_is_write_zero() {
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let err = write_frame_bytes_deadline(&mut Full, b"abc", 64, soon(), &mut always_ready)
        .unwrap_err();
    assert!(
        matches!(&err, FrameError::Io(e) if e.kind() == io::ErrorKind::WriteZero),
        "{err:?}"
    );
}
