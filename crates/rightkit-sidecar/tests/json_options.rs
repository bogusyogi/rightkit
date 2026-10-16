//! The 0.2.0 reader and writer options: whitespace-only lines, UTF-8 errors,
//! raw-line reads, the discard cap, flushing writes, uncapped frames, and
//! handing the buffered remainder back after a handshake.

use std::io::{BufReader, Cursor, Read, Write};

use rightkit_sidecar::{
    into_inner_with_remainder, read_json_line, read_json_line_opts, read_json_line_or_oversized,
    read_line_bytes, write_json_line, write_json_line_flush, FrameError, Incoming, ReadLimits,
    ReadOptions, WhitespaceSet, UNCAPPED,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Frame {
    id: u64,
    value: String,
}

fn opts(skip_whitespace_only_lines: bool) -> ReadOptions {
    ReadOptions {
        limits: ReadLimits::default(),
        skip_whitespace_only_lines,
        ..ReadOptions::default()
    }
}

fn unicode_opts() -> ReadOptions {
    ReadOptions {
        whitespace: WhitespaceSet::Unicode,
        ..opts(true)
    }
}

#[test]
fn whitespace_only_lines_are_skipped_only_when_asked() {
    let input = b"  \t\x0b\x0c \r\n{\"id\":1,\"value\":\"a\"}\n";

    let mut reader = BufReader::new(Cursor::new(input.to_vec()));
    let mut buf = Vec::new();
    let frame: Frame = read_json_line_opts(&mut reader, &mut buf, 128, opts(true))
        .unwrap()
        .unwrap();
    assert_eq!(frame.id, 1);

    let mut reader = BufReader::new(Cursor::new(input.to_vec()));
    let err = read_json_line_opts::<_, Frame>(&mut reader, &mut buf, 128, opts(false)).unwrap_err();
    assert!(matches!(err, FrameError::Json(_)), "{err:?}");
}

#[test]
fn non_ascii_whitespace_is_still_a_frame_when_skipping() {
    // U+00A0 (no-break space) is not ASCII blank, so the line is malformed JSON.
    let input = "\u{a0}\n{\"id\":1,\"value\":\"a\"}\n".as_bytes().to_vec();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let err = read_json_line_opts::<_, Frame>(&mut reader, &mut buf, 128, opts(true)).unwrap_err();
    assert!(matches!(err, FrameError::Json(_)), "{err:?}");
}

#[test]
fn unicode_whitespace_lines_are_skipped_only_in_unicode_mode() {
    // U+00A0 (no-break space), U+2003 (em space), and U+3000 (ideographic space) are
    // White_Space for str::trim, so each line is blank under the Unicode set.
    let input = "\u{a0}\r\n\u{2003}\t\u{3000}\n{\"id\":1,\"value\":\"a\"}\n"
        .as_bytes()
        .to_vec();

    let mut reader = BufReader::new(Cursor::new(input.clone()));
    let mut buf = Vec::new();
    let frame: Frame = read_json_line_opts(&mut reader, &mut buf, 128, unicode_opts())
        .unwrap()
        .unwrap();
    assert_eq!(frame.id, 1);

    // The default ASCII set treats those lines as frames, so the first one fails to parse.
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let err = read_json_line_opts::<_, Frame>(&mut reader, &mut buf, 128, opts(true)).unwrap_err();
    assert!(matches!(err, FrameError::Json(_)), "{err:?}");
}

#[test]
fn unicode_mode_still_skips_ascii_blank_lines() {
    let input = b" \t\x0b\x0c\r\n{\"id\":3,\"value\":\"c\"}\n";
    let mut reader = BufReader::new(Cursor::new(input.to_vec()));
    let mut buf = Vec::new();
    let frame: Frame = read_json_line_opts(&mut reader, &mut buf, 128, unicode_opts())
        .unwrap()
        .unwrap();
    assert_eq!(frame.id, 3);
}

#[test]
fn byte_order_mark_is_not_whitespace_for_trim_so_the_line_is_a_frame() {
    // U+FEFF is not White_Space, so str::trim keeps it and the line is not blank.
    assert_eq!("\u{feff}".trim(), "\u{feff}");
    let input = "\u{feff}\n{\"id\":1,\"value\":\"a\"}\n".as_bytes().to_vec();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let err =
        read_json_line_opts::<_, Frame>(&mut reader, &mut buf, 128, unicode_opts()).unwrap_err();
    assert!(matches!(err, FrameError::Json(_)), "{err:?}");
}

#[test]
fn invalid_utf8_keeps_the_ascii_rule_in_unicode_mode() {
    // Not valid UTF-8 and not ASCII blank, so it is a frame and reports InvalidUtf8.
    let mut input = vec![0xc2, b'\n'];
    input.extend_from_slice(b"{\"id\":4,\"value\":\"d\"}\n");
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let err =
        read_json_line_opts::<_, Frame>(&mut reader, &mut buf, 128, unicode_opts()).unwrap_err();
    assert!(matches!(err, FrameError::InvalidUtf8 { .. }), "{err:?}");
    let next: Frame = read_json_line_opts(&mut reader, &mut buf, 128, unicode_opts())
        .unwrap()
        .unwrap();
    assert_eq!(next.id, 4);
}

#[test]
fn invalid_utf8_is_its_own_error_and_the_next_line_still_reads() {
    let mut input = vec![b'{', 0xff, b'}', b'\n'];
    input.extend_from_slice(b"{\"id\":2,\"value\":\"ok\"}\n");
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();

    match read_json_line::<_, Frame>(&mut reader, &mut buf, 128) {
        Err(FrameError::InvalidUtf8 { len }) => assert_eq!(len, 3),
        other => panic!("expected InvalidUtf8, got {other:?}"),
    }
    let next: Frame = read_json_line(&mut reader, &mut buf, 128).unwrap().unwrap();
    assert_eq!(next.id, 2);
}

#[test]
fn read_line_bytes_returns_raw_lines_without_utf8_or_json_checks() {
    let input = b"\r\n\xff\xfe\r\nplain\n".to_vec();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();

    let first = read_line_bytes(&mut reader, &mut buf, 128, opts(true)).unwrap();
    assert_eq!(first, Some(vec![0xff, 0xfe]));
    let second = read_line_bytes(&mut reader, &mut buf, 128, opts(true)).unwrap();
    assert_eq!(second, Some(b"plain".to_vec()));
    let end = read_line_bytes(&mut reader, &mut buf, 128, opts(true)).unwrap();
    assert_eq!(end, None);
}

#[test]
fn read_line_bytes_over_the_limit_is_too_large() {
    let mut reader = BufReader::new(Cursor::new(b"0123456789012345678901234\n".to_vec()));
    let mut buf = Vec::new();
    match read_line_bytes(&mut reader, &mut buf, 8, opts(false)) {
        Err(FrameError::TooLarge { limit }) => assert_eq!(limit, 8),
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn zero_max_discard_stops_at_the_cap_without_draining_further() {
    let mut input = vec![b'x'; 4096];
    input.extend_from_slice(b"\n{\"id\":9,\"value\":\"n\"}\n");

    // The reader buffers 16 bytes per fill, so the first chunk crosses the
    // 8-byte cap and nothing beyond it may be read.
    let mut reader = BufReader::with_capacity(16, Cursor::new(input.clone()));
    let mut buf = Vec::new();
    let strict = ReadOptions {
        limits: ReadLimits {
            max_discard: 0,
            deadline: None,
        },
        skip_whitespace_only_lines: false,
        ..ReadOptions::default()
    };
    let err = read_json_line_opts::<_, Frame>(&mut reader, &mut buf, 8, strict).unwrap_err();
    assert!(
        matches!(&err, FrameError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData),
        "{err:?}"
    );
    let consumed = reader.get_ref().position();
    assert!(consumed <= 16, "read {consumed} bytes past the cap");

    // The default budget drains the same oversized line and resynchronises.
    let mut reader = BufReader::with_capacity(16, Cursor::new(input));
    match read_json_line_or_oversized::<_, Frame>(&mut reader, &mut buf, 8, ReadLimits::default())
        .unwrap()
    {
        Some(Incoming::Oversized(over)) => assert_eq!(over.limit, 8),
        other => panic!("expected Oversized, got {other:?}"),
    }
    let next: Frame = read_json_line(&mut reader, &mut buf, 128).unwrap().unwrap();
    assert_eq!(next.id, 9);
}

/// Counts flushes so a test can tell whether a write flushed.
struct Flushes {
    bytes: Vec<u8>,
    flushes: usize,
}

impl Write for Flushes {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn write_json_line_flush_flushes_and_plain_write_does_not() {
    let frame = Frame {
        id: 3,
        value: "f".into(),
    };

    let mut plain = Flushes {
        bytes: Vec::new(),
        flushes: 0,
    };
    write_json_line(&mut plain, &frame, 128).unwrap();
    assert_eq!(plain.flushes, 0);

    let mut flushed = Flushes {
        bytes: Vec::new(),
        flushes: 0,
    };
    write_json_line_flush(&mut flushed, &frame, 128).unwrap();
    assert_eq!(flushed.flushes, 1);
    assert_eq!(flushed.bytes, plain.bytes);
    assert!(flushed.bytes.ends_with(b"\n"));
}

#[test]
fn uncapped_accepts_a_large_frame_and_a_bounded_limit_refuses_it() {
    let frame = Frame {
        id: 1,
        value: "x".repeat(3 << 20),
    };
    let mut out = Vec::new();
    write_json_line(&mut out, &frame, UNCAPPED).unwrap();

    let mut reader = BufReader::new(Cursor::new(out.clone()));
    let mut buf = Vec::new();
    let got: Frame = read_json_line_opts(&mut reader, &mut buf, UNCAPPED, opts(false))
        .unwrap()
        .unwrap();
    assert_eq!(got.value.len(), 3 << 20);

    let mut reader = BufReader::new(Cursor::new(out));
    match read_json_line::<_, Frame>(&mut reader, &mut buf, 1 << 20) {
        Err(FrameError::TooLarge { .. }) => {}
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn into_inner_with_remainder_returns_bytes_read_past_the_frame() {
    let input =
        b"{\"id\":1,\"value\":\"a\"}\n{\"id\":2,\"value\":\"b\"}\n{\"id\":3,\"value\":\"c\"}\n";
    let mut reader = BufReader::with_capacity(64, Cursor::new(input.to_vec()));
    let mut buf = Vec::new();

    let first: Frame = read_json_line(&mut reader, &mut buf, 128).unwrap().unwrap();
    assert_eq!(first.id, 1);
    let buffered = reader.buffer().len();
    assert!(
        buffered > 0,
        "the reader must hold bytes past the first frame"
    );

    let (mut inner, mut rest) = into_inner_with_remainder(reader);
    inner.read_to_end(&mut rest).unwrap();
    assert_eq!(
        rest,
        b"{\"id\":2,\"value\":\"b\"}\n{\"id\":3,\"value\":\"c\"}\n"
    );
}
