//! Reader contract additions for 0.2.0: observation of every received line, raw-byte
//! limit counting, and empty-line handling.

use std::io::{BufReader, Cursor};

use rightkit_sidecar::{
    read_json_line_or_oversized_observed, read_line_bytes_observed, FrameError, Incoming,
    LimitCounts, ReadOptions, ReceivedLine, Terminator, WhitespaceSet,
};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
struct Frame {
    id: u64,
    value: String,
}

fn skip_blank(skip_empty: bool, skip_ws: bool) -> ReadOptions {
    ReadOptions {
        skip_empty_lines: skip_empty,
        skip_whitespace_only_lines: skip_ws,
        ..ReadOptions::default()
    }
}

#[test]
fn default_options_keep_today_behaviour() {
    let opts = ReadOptions::default();
    assert!(opts.skip_empty_lines, "empty lines are skipped by default");
    assert_eq!(opts.limit_counts, LimitCounts::Payload);
    assert!(!opts.skip_whitespace_only_lines);
    assert_eq!(opts.whitespace, WhitespaceSet::Ascii);
}

fn line(raw_len: usize, terminator: Terminator) -> ReceivedLine {
    ReceivedLine::new(raw_len, terminator)
}

#[test]
fn every_received_line_is_observed_before_any_skip_decision() {
    // Blank, whitespace-only, a frame, an oversized line, a malformed line, and an
    // unterminated final frame. The observer must see each line's raw byte count and its
    // terminator, including the lines the reader then skips or rejects.
    let mut input = Vec::new();
    input.extend_from_slice(b"\r\n"); // 2 CrLf: CRLF-only, skipped
    input.extend_from_slice(b"\n"); // 1 Lf: LF-only, skipped
    input.extend_from_slice(b"   \n"); // 4 Lf: whitespace-only, skipped under skip_ws
    input.extend_from_slice(b"{\"id\":1,\"value\":\"a\"}\n"); // 21 Lf: frame
    input.extend_from_slice(&[b'x'; 100]); // 101 Lf: oversized for limit 64
    input.push(b'\n');
    input.extend_from_slice(b"{\n"); // 2 Lf: malformed JSON, rejected after observation
    input.extend_from_slice(b"{\"id\":2,\"value\":\"b\"}"); // 20 None: unterminated final frame

    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let mut seen: Vec<ReceivedLine> = Vec::new();
    let opts = skip_blank(true, true);

    let first: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |n| {
            seen.push(n)
        })
        .unwrap();
    assert!(matches!(first, Some(Incoming::Frame(Frame { id: 1, .. }))));
    assert_eq!(
        seen,
        vec![
            line(2, Terminator::CrLf),
            line(1, Terminator::Lf),
            line(4, Terminator::Lf),
            line(21, Terminator::Lf),
        ]
    );

    let oversized: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |n| {
            seen.push(n)
        })
        .unwrap();
    assert!(matches!(oversized, Some(Incoming::Oversized(_))));
    assert_eq!(seen.last(), Some(&line(101, Terminator::Lf)));

    let malformed = read_json_line_or_oversized_observed::<_, Frame>(
        &mut reader,
        &mut buf,
        64,
        opts,
        &mut |n| seen.push(n),
    );
    assert!(
        matches!(malformed, Err(FrameError::Json(_))),
        "{malformed:?}"
    );
    assert_eq!(
        seen.last(),
        Some(&line(2, Terminator::Lf)),
        "a line rejected as JSON is still observed"
    );

    let last: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |n| {
            seen.push(n)
        })
        .unwrap();
    assert!(matches!(last, Some(Incoming::Frame(Frame { id: 2, .. }))));
    assert_eq!(
        seen.last(),
        Some(&line(20, Terminator::None)),
        "an unterminated final line reports Terminator::None"
    );

    let end: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |n| {
            seen.push(n)
        })
        .unwrap();
    assert!(end.is_none());
    assert_eq!(seen.len(), 7, "clean EOF observes nothing");
}

#[test]
fn a_terminator_split_across_read_chunks_is_still_crlf() {
    // A one-byte reader puts the CR and the LF in different chunks.
    let input = b"{\"id\":5,\"value\":\"c\"}\r\n".to_vec();
    let mut reader = BufReader::with_capacity(1, Cursor::new(input));
    let mut buf = Vec::new();
    let mut seen = Vec::new();
    let got: Option<Incoming<Frame>> = read_json_line_or_oversized_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |n| seen.push(n),
    )
    .unwrap();
    assert!(matches!(got, Some(Incoming::Frame(Frame { id: 5, .. }))));
    assert_eq!(seen, vec![line(22, Terminator::CrLf)]);
}

#[test]
fn unterminated_lines_at_the_limit_are_reported_as_none_in_both_modes() {
    // limit = 64. Each case is one line with no terminator: the stream ends after its bytes.
    //   MAX-1 (63) and MAX (64) unterminated are complete frames in both modes; the consumer
    //   tells them from "MAX-1 + LF" by Terminator::None and raw_len.
    //   MAX+1 (65) unterminated is oversized in both modes.
    //   An oversized line cut by EOF (100) is oversized, reported with Terminator::None.
    const LIMIT: usize = 64;
    for counts in [LimitCounts::Payload, LimitCounts::RawLineBytes] {
        for (raw, frame_expected) in [(LIMIT - 1, true), (LIMIT, true), (LIMIT + 1, false)] {
            let (accepted, observed) = unterminated(LIMIT, raw, counts);
            assert_eq!(accepted, frame_expected, "{counts:?} raw={raw}");
            assert_eq!(
                observed,
                line(raw, Terminator::None),
                "{counts:?} raw={raw}"
            );
        }
        let (accepted, observed) = unterminated(LIMIT, 100, counts);
        assert!(!accepted, "{counts:?} oversized unterminated line");
        assert_eq!(
            observed,
            line(100, Terminator::None),
            "{counts:?} oversized"
        );
    }
}

/// One line of `raw` bytes with no terminator, then EOF. Returns whether it was a frame,
/// and what the observer reported.
fn unterminated(limit: usize, raw: usize, counts: LimitCounts) -> (bool, ReceivedLine) {
    let mut reader = BufReader::new(Cursor::new(oversize_or_payload(raw)));
    let mut buf = Vec::new();
    let mut seen = Vec::new();
    let opts = ReadOptions {
        limit_counts: counts,
        ..ReadOptions::default()
    };
    let got: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, limit, opts, &mut |n| {
            seen.push(n)
        })
        .unwrap();
    assert_eq!(seen.len(), 1, "one line, one observation");
    (matches!(got, Some(Incoming::Frame(_))), seen[0])
}

/// A JSON frame of exactly `p` bytes, or, when `p` is above the frame template size
/// that `payload` can express, a run of non-JSON bytes. Oversized lines are never parsed.
fn oversize_or_payload(p: usize) -> Vec<u8> {
    if p >= 19 {
        payload(p)
    } else {
        vec![b'x'; p]
    }
}

#[test]
fn oversized_terminated_lines_report_their_terminator() {
    // 100 bytes plus LF is raw 101 Lf; 100 bytes plus CRLF is raw 102 CrLf. Both are
    // oversized, and both are consumed through the terminator so the next frame reads.
    for counts in [LimitCounts::Payload, LimitCounts::RawLineBytes] {
        let mut lf = vec![b'x'; 100];
        lf.push(b'\n');
        lf.extend_from_slice(b"{\"id\":3,\"value\":\"d\"}\n");
        let mut reader = BufReader::new(Cursor::new(lf));
        let mut buf = Vec::new();
        let mut seen = Vec::new();
        let opts = ReadOptions {
            limit_counts: counts,
            ..ReadOptions::default()
        };
        let first: Option<Incoming<Frame>> =
            read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |n| {
                seen.push(n)
            })
            .unwrap();
        assert!(matches!(first, Some(Incoming::Oversized(_))));
        assert_eq!(seen, vec![line(101, Terminator::Lf)], "{counts:?}");
        let next: Option<Incoming<Frame>> =
            read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |_| {})
                .unwrap();
        assert!(matches!(next, Some(Incoming::Frame(Frame { id: 3, .. }))));

        let mut crlf = vec![b'x'; 100];
        crlf.extend_from_slice(b"\r\n");
        let mut reader = BufReader::new(Cursor::new(crlf));
        let mut buf = Vec::new();
        let mut seen = Vec::new();
        let got: Option<Incoming<Frame>> =
            read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |n| {
                seen.push(n)
            })
            .unwrap();
        assert!(matches!(got, Some(Incoming::Oversized(_))));
        assert_eq!(seen, vec![line(102, Terminator::CrLf)], "{counts:?}");
    }
}

/// The payload is `p` bytes of JSON; `p` is at least 19 so the template fits.
fn payload(p: usize) -> Vec<u8> {
    assert!(p >= 19);
    let mut line = b"{\"id\":1,\"value\":\"".to_vec();
    line.extend(std::iter::repeat_n(b'x', p - 19));
    line.extend_from_slice(b"\"}");
    assert_eq!(line.len(), p);
    line
}

/// Read one line of raw length `raw` (payload plus `term`) under `counts`, then a valid
/// frame, and report whether the first line was accepted. The second frame proves the
/// reader resynchronised.
fn accepted(limit: usize, raw: usize, term: &[u8], counts: LimitCounts) -> bool {
    let mut input = payload(raw - term.len());
    input.extend_from_slice(term);
    input.extend_from_slice(b"{\"id\":9,\"value\":\"z\"}\n");
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let opts = ReadOptions {
        limit_counts: counts,
        ..ReadOptions::default()
    };
    let first: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, limit, opts, &mut |_| {})
            .unwrap();
    let is_frame = matches!(first, Some(Incoming::Frame(_)));
    let second: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, limit, opts, &mut |_| {})
            .unwrap();
    assert!(
        matches!(second, Some(Incoming::Frame(Frame { id: 9, .. }))),
        "the next frame must resynchronise"
    );
    is_frame
}

#[test]
fn limit_boundary_table_for_lf_and_crlf_in_both_modes() {
    // limit = 64, so MAX-2 .. MAX+1 is raw 62 .. 65 (terminator included).
    //
    // Payload counts the bytes before the terminator (one `\r` before `\n` is not counted):
    // accepted iff payload <= 64.
    // RawLineBytes counts every byte as read, the `\n` or `\r\n` included: accepted iff raw <= 64.
    // That matches HeardRight's `read_line` over `take(MAX_LINE)`, which closes a line whose
    // raw bytes, terminator included, exceed MAX_LINE.
    const LIMIT: usize = 64;
    let cases = [
        // (terminator, raw, payload mode accepted, raw mode accepted)
        (&b"\n"[..], LIMIT - 2, true, true),
        (&b"\n"[..], LIMIT - 1, true, true),
        (&b"\n"[..], LIMIT, true, true),
        (&b"\n"[..], LIMIT + 1, true, false),
        (&b"\r\n"[..], LIMIT - 2, true, true),
        (&b"\r\n"[..], LIMIT - 1, true, true),
        (&b"\r\n"[..], LIMIT, true, true),
        (&b"\r\n"[..], LIMIT + 1, true, false),
    ];
    for (term, raw, payload_ok, raw_ok) in cases {
        let label = format!("term={:?} raw={raw}", String::from_utf8_lossy(term));
        assert_eq!(
            accepted(LIMIT, raw, term, LimitCounts::Payload),
            payload_ok,
            "Payload {label}"
        );
        assert_eq!(
            accepted(LIMIT, raw, term, LimitCounts::RawLineBytes),
            raw_ok,
            "RawLineBytes {label}"
        );
    }
}

#[test]
fn raw_mode_refuses_one_byte_past_the_limit_on_lf_lines_that_payload_mode_accepts() {
    // The gap the table exposes: at raw MAX+1, payload mode accepts an LF line, while raw
    // mode refuses it.
    assert!(accepted(64, 65, b"\n", LimitCounts::Payload));
    assert!(!accepted(64, 65, b"\n", LimitCounts::RawLineBytes));
}

#[test]
fn an_empty_line_is_skipped_by_default() {
    let input = b"\n\r\n{\"id\":1,\"value\":\"a\"}\n".to_vec();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let got: Option<Incoming<Frame>> = read_json_line_or_oversized_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(got, Some(Incoming::Frame(Frame { id: 1, .. }))));
}

#[test]
fn a_leading_empty_line_is_an_error_when_empty_lines_are_not_skipped() {
    // HeardRight's ready reader parses the first line and fails on a leading blank line.
    let input = b"\r\n{\"id\":1,\"value\":\"a\"}\n".to_vec();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let opts = skip_blank(false, false);
    let first = read_json_line_or_oversized_observed::<_, Frame>(
        &mut reader,
        &mut buf,
        64,
        opts,
        &mut |_| {},
    );
    assert!(matches!(first, Err(FrameError::EmptyLine)), "{first:?}");
    // The empty line is consumed, so the next call reads the frame.
    let next: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |_| {}).unwrap();
    assert!(matches!(next, Some(Incoming::Frame(Frame { id: 1, .. }))));
}

#[test]
fn empty_and_whitespace_only_lines_are_different_cases() {
    // Empty ("" or "\r") is governed by skip_empty_lines alone.
    // Whitespace-only (not empty) is governed by skip_whitespace_only_lines.
    let frame = b"{\"id\":1,\"value\":\"a\"}\n";

    // skip_empty=false, skip_ws=true: the whitespace-only line is skipped, the empty one is not.
    let mut input = b"   \n".to_vec();
    input.extend_from_slice(b"\n");
    input.extend_from_slice(frame);
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let opts = skip_blank(false, true);
    let first = read_json_line_or_oversized_observed::<_, Frame>(
        &mut reader,
        &mut buf,
        64,
        opts,
        &mut |_| {},
    );
    assert!(matches!(first, Err(FrameError::EmptyLine)), "{first:?}");
    let next: Option<Incoming<Frame>> =
        read_json_line_or_oversized_observed(&mut reader, &mut buf, 64, opts, &mut |_| {}).unwrap();
    assert!(matches!(next, Some(Incoming::Frame(Frame { id: 1, .. }))));

    // skip_empty=false, skip_ws=false: the whitespace-only line is a frame, so it is malformed JSON.
    let mut reader = BufReader::new(Cursor::new(b"   \n".to_vec()));
    let mut buf = Vec::new();
    let strict = skip_blank(false, false);
    let got = read_json_line_or_oversized_observed::<_, Frame>(
        &mut reader,
        &mut buf,
        64,
        strict,
        &mut |_| {},
    );
    assert!(matches!(got, Err(FrameError::Json(_))), "{got:?}");

    // The Unicode set skips U+00A0 as whitespace; it is not empty, so skip_empty=false does not fire.
    let mut reader = BufReader::new(Cursor::new("\u{a0}\n".as_bytes().to_vec()));
    let mut buf = Vec::new();
    let unicode = ReadOptions {
        skip_empty_lines: false,
        skip_whitespace_only_lines: true,
        whitespace: WhitespaceSet::Unicode,
        ..ReadOptions::default()
    };
    let got = read_json_line_or_oversized_observed::<_, Frame>(
        &mut reader,
        &mut buf,
        64,
        unicode,
        &mut |_| {},
    );
    assert!(
        matches!(got, Ok(None)),
        "U+00A0 line skipped, then EOF: {got:?}"
    );
}

// ---- Raw observed reader -------------------------------------------------------------

#[test]
fn raw_reader_observes_every_line_and_returns_frame_bytes() {
    let mut input = Vec::new();
    input.extend_from_slice(b"\r\n");
    input.extend_from_slice(b"\n");
    input.extend_from_slice(b"   \n");
    input.extend_from_slice(b"{\"id\":1,\"value\":\"a\"}\n");
    input.extend_from_slice(&[b'x'; 100]);
    input.push(b'\n');
    input.extend_from_slice(b"{\n");
    input.extend_from_slice(b"{\"id\":2,\"value\":\"b\"}");

    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let mut seen: Vec<ReceivedLine> = Vec::new();
    let opts = skip_blank(true, true);

    let first =
        read_line_bytes_observed(&mut reader, &mut buf, 64, opts, &mut |n| seen.push(n)).unwrap();
    assert_eq!(first.as_deref(), Some(&b"{\"id\":1,\"value\":\"a\"}"[..]));
    assert!(buf.is_empty(), "a returned line leaves buf empty");
    assert_eq!(
        seen,
        vec![
            line(2, Terminator::CrLf),
            line(1, Terminator::Lf),
            line(4, Terminator::Lf),
            line(21, Terminator::Lf),
        ]
    );

    let oversized =
        read_line_bytes_observed(&mut reader, &mut buf, 64, opts, &mut |n| seen.push(n));
    assert!(
        matches!(oversized, Err(FrameError::TooLarge { limit: 64 })),
        "{oversized:?}"
    );
    assert!(buf.is_empty(), "an oversized line leaves buf empty");
    assert_eq!(seen.last(), Some(&line(101, Terminator::Lf)));

    // The raw reader does not parse, so a malformed line comes back as its bytes.
    let malformed =
        read_line_bytes_observed(&mut reader, &mut buf, 64, opts, &mut |n| seen.push(n));
    assert_eq!(malformed.unwrap().as_deref(), Some(&b"{"[..]));
    assert_eq!(seen.last(), Some(&line(2, Terminator::Lf)));

    let last =
        read_line_bytes_observed(&mut reader, &mut buf, 64, opts, &mut |n| seen.push(n)).unwrap();
    assert_eq!(last.as_deref(), Some(&b"{\"id\":2,\"value\":\"b\"}"[..]));
    assert_eq!(seen.last(), Some(&line(20, Terminator::None)));

    let end = read_line_bytes_observed(&mut reader, &mut buf, 64, opts, &mut |n| seen.push(n));
    assert!(matches!(end, Ok(None)));
    assert_eq!(seen.len(), 7, "clean EOF observes nothing");
}

#[test]
fn raw_reader_returns_an_invalid_json_line_intact() {
    let input = b"{\"id\":7,\n".to_vec();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let got = read_line_bytes_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    );
    assert_eq!(got.unwrap().as_deref(), Some(&b"{\"id\":7,"[..]));
}

#[test]
fn raw_reader_returns_invalid_utf8_intact() {
    let input = vec![b'{', 0xff, b'}', b'\n'];
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let got = read_line_bytes_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    );
    assert_eq!(got.unwrap().as_deref(), Some(&[b'{', 0xff, b'}'][..]));
}

/// `read_line_bytes_observed` with the same boundary table as the JSON variant.
fn raw_accepted(
    limit: usize,
    raw: usize,
    term: &[u8],
    counts: LimitCounts,
) -> (bool, ReceivedLine) {
    let mut input = payload(raw - term.len());
    input.extend_from_slice(term);
    raw_one(limit, input, counts)
}

fn raw_unterminated(limit: usize, raw: usize, counts: LimitCounts) -> (bool, ReceivedLine) {
    raw_one(limit, payload(raw), counts)
}

fn raw_one(limit: usize, input: Vec<u8>, counts: LimitCounts) -> (bool, ReceivedLine) {
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buf = Vec::new();
    let mut seen = Vec::new();
    let opts = ReadOptions {
        limit_counts: counts,
        ..ReadOptions::default()
    };
    let got = read_line_bytes_observed(&mut reader, &mut buf, limit, opts, &mut |n| seen.push(n));
    assert_eq!(seen.len(), 1, "one line, one observation");
    (got.is_ok(), seen[0])
}

#[test]
fn raw_reader_limit_boundary_table_for_lf_and_crlf_in_both_modes() {
    const LIMIT: usize = 64;
    let cases = [
        (&b"\n"[..], LIMIT - 2, true, true),
        (&b"\n"[..], LIMIT - 1, true, true),
        (&b"\n"[..], LIMIT, true, true),
        (&b"\n"[..], LIMIT + 1, true, false),
        (&b"\r\n"[..], LIMIT - 2, true, true),
        (&b"\r\n"[..], LIMIT - 1, true, true),
        (&b"\r\n"[..], LIMIT, true, true),
        (&b"\r\n"[..], LIMIT + 1, true, false),
    ];
    for (term, raw, payload_ok, raw_ok) in cases {
        let label = format!("term={:?} raw={raw}", String::from_utf8_lossy(term));
        let (ok, observed) = raw_accepted(LIMIT, raw, term, LimitCounts::Payload);
        assert_eq!(ok, payload_ok, "Payload {label}");
        assert_eq!(observed.raw_len, raw, "Payload {label}");
        let (ok, observed) = raw_accepted(LIMIT, raw, term, LimitCounts::RawLineBytes);
        assert_eq!(ok, raw_ok, "RawLineBytes {label}");
        assert_eq!(observed.raw_len, raw, "RawLineBytes {label}");
    }
}

#[test]
fn raw_reader_unterminated_cases_in_both_modes() {
    const LIMIT: usize = 64;
    for counts in [LimitCounts::Payload, LimitCounts::RawLineBytes] {
        for (raw, ok_expected) in [(LIMIT - 1, true), (LIMIT, true), (LIMIT + 1, false)] {
            let (ok, observed) = raw_unterminated(LIMIT, raw, counts);
            assert_eq!(ok, ok_expected, "{counts:?} raw={raw}");
            assert_eq!(
                observed,
                line(raw, Terminator::None),
                "{counts:?} raw={raw}"
            );
        }
        let (ok, observed) = raw_one(LIMIT, vec![b'x'; 100], counts);
        assert!(!ok, "{counts:?} oversized unterminated");
        assert_eq!(
            observed,
            line(100, Terminator::None),
            "{counts:?} oversized"
        );
    }
}

#[test]
fn raw_reader_empty_line_is_an_error_with_an_empty_buffer() {
    let mut reader = BufReader::new(Cursor::new(b"\r\n{\"id\":1,\"value\":\"a\"}\n".to_vec()));
    let mut buf = Vec::new();
    let mut seen = Vec::new();
    let opts = skip_blank(false, false);
    let first = read_line_bytes_observed(&mut reader, &mut buf, 64, opts, &mut |n| seen.push(n));
    assert!(matches!(first, Err(FrameError::EmptyLine)), "{first:?}");
    assert!(buf.is_empty());
    assert_eq!(seen, vec![line(2, Terminator::CrLf)]);
}

#[test]
fn json_variant_leaves_the_raw_line_in_buf_on_a_json_error() {
    let mut reader = BufReader::new(Cursor::new(
        b"{\"id\":7,\n{\"id\":8,\"value\":\"c\"}\n".to_vec(),
    ));
    let mut buf = Vec::new();
    let first: Result<Option<Incoming<Frame>>, FrameError> = read_json_line_or_oversized_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    );
    assert!(matches!(first, Err(FrameError::Json(_))), "{first:?}");
    assert_eq!(buf, b"{\"id\":7,", "the raw line is kept for the caller");
    // The next read starts a fresh line and clears the kept bytes.
    let next: Option<Incoming<Frame>> = read_json_line_or_oversized_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(next, Some(Incoming::Frame(Frame { id: 8, .. }))));
    assert!(buf.is_empty(), "a parsed frame leaves buf empty");
}

#[test]
fn json_variant_leaves_invalid_utf8_in_buf() {
    let mut reader = BufReader::new(Cursor::new(vec![b'{', 0xff, b'}', b'\n']));
    let mut buf = Vec::new();
    let got: Result<Option<Incoming<Frame>>, FrameError> = read_json_line_or_oversized_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    );
    assert!(
        matches!(got, Err(FrameError::InvalidUtf8 { len: 3 })),
        "{got:?}"
    );
    assert_eq!(buf, vec![b'{', 0xff, b'}']);
}

#[test]
fn oversized_line_leaves_buf_empty() {
    let mut reader = BufReader::new(Cursor::new(vec![b'x'; 100]));
    let mut buf = Vec::new();
    let got: Result<Option<Incoming<Frame>>, FrameError> = read_json_line_or_oversized_observed(
        &mut reader,
        &mut buf,
        64,
        ReadOptions::default(),
        &mut |_| {},
    );
    assert!(matches!(got, Ok(Some(Incoming::Oversized(_)))), "{got:?}");
    assert!(buf.is_empty());
}
