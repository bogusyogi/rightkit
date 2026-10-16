use std::io::{self, Write};

use rightkit_sidecar::{
    encode_json_line, write_encoded_line, write_encoded_line_flush, write_json_line, FrameError,
};
use serde::Serialize;
use serde_json::value::RawValue;

#[derive(Debug, PartialEq, Serialize)]
struct Frame {
    id: u64,
    value: String,
}

/// Records whether `flush` ran, and how many bytes were written.
#[derive(Default)]
struct Recorder {
    bytes: Vec<u8>,
    flushes: usize,
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn frame(value: &str) -> Frame {
    Frame {
        id: 7,
        value: value.into(),
    }
}

#[test]
fn encoded_bytes_equal_what_write_json_line_emits() {
    let value = frame("ok");
    let encoded = encode_json_line(&value, 128).unwrap();

    let mut written = Vec::new();
    write_json_line(&mut written, &value, 128).unwrap();

    assert_eq!(encoded, written);
    assert_eq!(
        encoded,
        br#"{"id":7,"value":"ok"}"#.iter().copied().chain(*b"\n").collect::<Vec<u8>>()
    );
}

#[test]
fn encode_enforces_limit_on_payload_excluding_newline() {
    let value = frame("abc");
    let payload_len = serde_json::to_vec(&value).unwrap().len();

    let at_limit = encode_json_line(&value, payload_len).unwrap();
    assert_eq!(at_limit.len(), payload_len + 1);

    let over = encode_json_line(&value, payload_len - 1);
    assert!(matches!(over, Err(FrameError::TooLarge { limit }) if limit == payload_len - 1));
}

#[test]
fn encode_refuses_embedded_newline_and_write_json_line_writes_nothing() {
    let raw = RawValue::from_string("{\n\"id\":1}".to_string()).unwrap();

    let encoded = encode_json_line(&raw, 128);
    assert!(matches!(encoded, Err(FrameError::EmbeddedNewline)));

    let mut sink = Vec::new();
    let result = write_json_line(&mut sink, &raw, 128);
    assert!(matches!(result, Err(FrameError::EmbeddedNewline)));
    assert!(sink.is_empty());
}

#[test]
fn write_encoded_line_writes_the_frame_without_flushing() {
    let encoded = encode_json_line(&frame("ok"), 128).unwrap();
    let mut out = Recorder::default();

    write_encoded_line(&mut out, &encoded, 128).unwrap();

    assert_eq!(out.bytes, encoded);
    assert_eq!(out.flushes, 0);
}

#[test]
fn write_encoded_line_flush_writes_and_flushes_once() {
    let encoded = encode_json_line(&frame("ok"), 128).unwrap();
    let mut out = Recorder::default();

    write_encoded_line_flush(&mut out, &encoded, 128).unwrap();

    assert_eq!(out.bytes, encoded);
    assert_eq!(out.flushes, 1);
}

#[test]
fn write_encoded_line_refuses_missing_terminator() {
    let mut out = Recorder::default();
    let result = write_encoded_line_flush(&mut out, br#"{"id":1}"#, 128);
    assert!(matches!(result, Err(FrameError::MissingTerminator)));
    assert!(out.bytes.is_empty());
    assert_eq!(out.flushes, 0);

    let empty = write_encoded_line(&mut out, b"", 128);
    assert!(matches!(empty, Err(FrameError::MissingTerminator)));
    assert!(out.bytes.is_empty());
}

#[test]
fn write_encoded_line_refuses_interior_or_doubled_newlines() {
    let mut out = Recorder::default();

    let interior = write_encoded_line_flush(&mut out, b"{\"a\":\n1}\n", 128);
    assert!(matches!(interior, Err(FrameError::EmbeddedNewline)));

    let doubled = write_encoded_line_flush(&mut out, b"{}\n\n", 128);
    assert!(matches!(doubled, Err(FrameError::EmbeddedNewline)));

    assert!(out.bytes.is_empty());
    assert_eq!(out.flushes, 0);
}

#[test]
fn write_encoded_line_refuses_over_limit_without_writing() {
    let mut out = Recorder::default();

    let result = write_encoded_line_flush(&mut out, b"12345\n", 4);
    assert!(matches!(result, Err(FrameError::TooLarge { limit: 4 })));
    assert!(out.bytes.is_empty());
    assert_eq!(out.flushes, 0);

    write_encoded_line(&mut out, b"1234\n", 4).unwrap();
    assert_eq!(out.bytes, b"1234\n");
}
