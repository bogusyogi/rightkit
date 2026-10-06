//! End-to-end over real child processes (the test binary re-executes itself as
//! the misbehaving peer): the typed wire envelope, oversize-with-id handling
//! and the length-prefixed codec, validated by content.

use std::io::{BufReader, Read, Write};
use std::process::{Command, Stdio};

use rightkit_framed_sidecar::length_prefixed::{read_frame, read_frame_bytes, write_frame, write_frame_bytes};
use rightkit_framed_sidecar::wire::{Frame, WireError};
use rightkit_framed_sidecar::{read_json_line_or_oversized, FrameError, Incoming, ReadLimits};
use serde_json::{json, Value};

fn spawn(test: &str, mode: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("RK_WIRE_MODE", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Skip libtest's preamble up to a marker line.
fn skip_preamble(r: &mut impl std::io::BufRead) {
    loop {
        let mut l = String::new();
        assert!(r.read_line(&mut l).unwrap() > 0, "child exited before marker");
        if l.trim_end().ends_with("RK_BEGIN") {
            return;
        }
    }
}

/// Child: speaks JSON lines on stdout.
#[test]
fn child_lines() {
    let Ok(mode) = std::env::var("RK_WIRE_MODE") else { return };
    if mode != "lines" {
        return;
    }
    {
        let mut out = std::io::stdout().lock();
        out.write_all(b"\nRK_BEGIN\n").unwrap();
        out.flush().unwrap();
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, r#"{{"type":"hello","engine":"fx","version":"2.1","contract_version":1,"unknown_extra":true}}"#).unwrap();
    writeln!(out, r#"{{"type":"ready"}}"#).unwrap();
    writeln!(out, r#"{{"type":"event","session_id":"s1","event":{{"level":0.5}}}}"#).unwrap();
    writeln!(out, r#"{{"type":"event","id":42,"event":{{"stage":"x"}}}}"#).unwrap();
    // A 2 MiB response for id 42 (over the 64 KiB limit the parent uses).
    write!(out, r#"{{"type":"response","id":42,"ok":true,"result":"{}"}}"#, "r".repeat(2 << 20)).unwrap();
    writeln!(out).unwrap();
    writeln!(out, r#"{{"type":"response","id":43,"ok":false,"error":{{"code":"nope","message":"m","retryable":true}}}}"#).unwrap();
    writeln!(out, r#"{{"type":"bye","reason":"done"}}"#).unwrap();
    out.flush().unwrap();
}

#[test]
fn typed_envelope_from_a_real_child_with_oversized_response_identified_by_id() {
    let mut child = spawn("child_lines", "lines");
    let mut r = BufReader::new(child.stdout.take().unwrap());
    skip_preamble(&mut r);
    let mut buf = Vec::new();
    let lim = ReadLimits::default();
    let mut next = |r: &mut BufReader<_>, buf: &mut Vec<u8>| read_json_line_or_oversized::<_, Frame>(r, buf, 64 * 1024, lim).unwrap().unwrap();

    match next(&mut r, &mut buf) {
        Incoming::Frame(Frame::Hello { engine, version, contract_version }) => {
            assert_eq!((engine.as_str(), version.as_str(), contract_version), ("fx", "2.1", 1));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(next(&mut r, &mut buf), Incoming::Frame(Frame::Ready)));
    match next(&mut r, &mut buf) {
        Incoming::Frame(Frame::Event { id: None, session_id: Some(s), event }) => {
            assert_eq!(s, "s1");
            assert_eq!(event["level"], 0.5);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(next(&mut r, &mut buf), Incoming::Frame(Frame::Event { id: Some(42), .. })));
    // The 2 MiB response is drained, never buffered, and still attributable.
    match next(&mut r, &mut buf) {
        Incoming::Oversized(o) => {
            assert_eq!(o.id(), Some(42));
            assert_eq!(o.limit, 64 * 1024);
            assert!(o.bytes > 2 << 20, "bytes {}", o.bytes);
            assert!(o.head.len() <= 4096 && o.head.starts_with(b"{\"type\":\"response\""));
        }
        other => panic!("{other:?}"),
    }
    assert!(buf.capacity() <= 128 * 1024, "buffer grew to {}", buf.capacity());
    // The stream is back on a frame boundary: a typed error response follows.
    match next(&mut r, &mut buf) {
        Incoming::Frame(Frame::Response { id: 43, ok: false, error: Some(e), .. }) => {
            assert_eq!(e, WireError::retryable("nope", "m"));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(next(&mut r, &mut buf), Incoming::Frame(Frame::Bye { reason }) if reason == "done"));
    child.wait().unwrap();
}

/// Child: a length-prefixed echo server on stdio. Echoes `{"echo":<request>}`;
/// a frame with `"quit":true` ends it. Optional hostile tail per mode.
#[test]
fn child_prefixed() {
    let Ok(mode) = std::env::var("RK_WIRE_MODE") else { return };
    if !mode.starts_with("prefixed") {
        return;
    }
    let mut out = std::io::stdout().lock();
    out.write_all(b"\nRK_BEGIN\n").unwrap();
    out.flush().unwrap();
    let mut stdin = std::io::stdin().lock();
    loop {
        let v: Value = match read_frame(&mut stdin, 1 << 20) {
            Ok(Some(v)) => v,
            _ => return,
        };
        write_frame(&mut out, &json!({"echo": v}), 1 << 20).unwrap();
        if v["quit"] == true {
            break;
        }
    }
    match mode.as_str() {
        // Announce 1 GiB then send 3 bytes and hang up mid-body.
        "prefixed_liar" => {
            out.write_all(&(1u32 << 30).to_be_bytes()).unwrap();
            out.write_all(b"{\"a").unwrap();
        }
        // A zero-length frame.
        "prefixed_zero" => out.write_all(&0u32.to_be_bytes()).unwrap(),
        // Truncated inside the length prefix.
        "prefixed_short_len" => out.write_all(&[0, 0]).unwrap(),
        _ => {}
    }
    out.flush().unwrap();
    // This process is a protocol peer; libtest's trailing status text would corrupt its framed
    // stdout after the final response.
    std::process::exit(0);
}

fn roundtrip(mode: &str) -> (BufReader<std::process::ChildStdout>, std::process::Child, std::process::ChildStdin) {
    let mut child = spawn("child_prefixed", mode);
    let mut r = BufReader::new(child.stdout.take().unwrap());
    skip_preamble(&mut r);
    let mut stdin = child.stdin.take().unwrap();
    // Multi-line payload: legal here, impossible in JSON lines framing.
    write_frame_bytes(&mut stdin, b"{\"text\":\"a\\nb\"}", 1 << 20).unwrap();
    let got: Value = read_frame(&mut r, 1 << 20).unwrap().unwrap();
    assert_eq!(got["echo"]["text"], "a\nb");
    write_frame(&mut stdin, &json!({"quit": true}), 1 << 20).unwrap();
    let got: Value = read_frame(&mut r, 1 << 20).unwrap().unwrap();
    assert_eq!(got["echo"]["quit"], true);
    (r, child, stdin)
}

#[test]
fn prefixed_roundtrip_carries_newlines_and_ends_cleanly() {
    let (mut r, mut child, stdin) = roundtrip("prefixed");
    drop(stdin);
    let mut tail = Vec::new();
    r.read_to_end(&mut tail).unwrap();
    child.wait().unwrap();
}

#[test]
fn prefixed_announced_length_over_limit_is_rejected_without_reading_the_body() {
    let (mut r, mut child, _stdin) = roundtrip("prefixed_liar");
    // 1 GiB announced against a 4 KiB limit: refused from the prefix alone.
    match read_frame_bytes(&mut r, 4096) {
        Err(FrameError::TooLarge { limit: 4096 }) => {}
        other => panic!("{other:?}"),
    }
    // The body was left unread, so the stream is desynchronised by contract.
    let mut rest = Vec::new();
    r.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"{\"a");
    child.wait().unwrap();
}

#[test]
fn prefixed_truncated_body_zero_length_and_short_prefix_are_distinct_errors() {
    // Same child, but the parent allows the announced size so the body read hits EOF.
    let (mut r, mut child, _stdin) = roundtrip("prefixed_liar");
    match read_frame_bytes(&mut r, 2 << 30) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof),
        other => panic!("{other:?}"),
    }
    child.wait().unwrap();

    let (mut r, mut child, _stdin) = roundtrip("prefixed_zero");
    match read_frame_bytes(&mut r, 4096) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
        other => panic!("{other:?}"),
    }
    child.wait().unwrap();

    let (mut r, mut child, _stdin) = roundtrip("prefixed_short_len");
    match read_frame_bytes(&mut r, 4096) {
        Err(FrameError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof),
        other => panic!("{other:?}"),
    }
    child.wait().unwrap();
}

#[test]
fn writer_refuses_oversize_and_empty_bodies_without_writing() {
    let mut sink = Vec::new();
    assert!(matches!(write_frame_bytes(&mut sink, &[b'x'; 33], 32), Err(FrameError::TooLarge { limit: 32 })));
    assert!(matches!(write_frame_bytes(&mut sink, b"", 32), Err(FrameError::Io(_))));
    assert!(sink.is_empty());
}
