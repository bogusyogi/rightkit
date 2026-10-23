// rightkit-sidecar 0.3.0 consumed by exact registry pin, outside the workspace.
//
// Proves the bounded JSON-line framing a sidecar uses: encode_json_line ends the frame with one
// newline; write_json_line emits the same bytes; read_json_line returns the frame, then Ok(None)
// on clean EOF; a frame over the byte limit is refused with FrameError::TooLarge and nothing is
// written. Public API only.
// Does not prove: deadline-bounded transfers, pipes or child processes, ReadOptions, or
// oversized-line resynchronisation.
// State: in-memory buffers only. No files, no network, no process spawn, no user data.
use rightkit_sidecar::{encode_json_line, read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};
use std::io::Cursor;

fn main() {
    let frame = json!({"id": 7, "method": "status", "params": {"ok": true}});
    let encoded = encode_json_line(&frame, 4096).expect("frame fits the limit");
    assert_eq!(
        encoded.last(),
        Some(&b'\n'),
        "encoded frame ends with one newline"
    );
    assert_eq!(
        encoded.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "exactly one newline"
    );

    let mut written = Vec::new();
    write_json_line(&mut written, &frame, 4096).expect("write fits the limit");
    assert_eq!(written, encoded, "write_json_line emits the encoded bytes");

    let mut reader = Cursor::new(written);
    let mut buffer = Vec::new();
    let first: Option<Value> =
        read_json_line(&mut reader, &mut buffer, 4096).expect("first frame reads");
    assert_eq!(first, Some(frame), "frame round-trips");
    let eof: Option<Value> = read_json_line(&mut reader, &mut buffer, 4096).expect("clean EOF");
    assert!(eof.is_none(), "clean EOF yields None");

    let large = json!({"blob": "x".repeat(64)});
    let mut refused = Vec::new();
    let error = write_json_line(&mut refused, &large, 16).expect_err("oversized frame is refused");
    assert!(
        matches!(error, FrameError::TooLarge { limit: 16 }),
        "limit error names the limit"
    );
    assert!(
        refused.is_empty(),
        "nothing is written for an oversized frame"
    );

    println!("OK rightkit-sidecar 0.3.0 (registry consumer)");
}
