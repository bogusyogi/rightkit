use std::io::{BufReader, Cursor};

use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Frame {
    id: u64,
    value: String,
}

#[test]
fn writes_one_compact_newline_terminated_frame() {
    let mut output = Vec::new();
    write_json_line(
        &mut output,
        &Frame {
            id: 7,
            value: "ok".into(),
        },
        128,
    )
    .unwrap();

    assert_eq!(
        output,
        br#"{"id":7,"value":"ok"}
"#
    );
}

#[test]
fn reads_crlf_blank_lines_and_final_unterminated_frame() {
    let input = b"\r\n{\"id\":1,\"value\":\"first\"}\r\n{\"id\":2,\"value\":\"last\"}";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buffer = Vec::new();

    let first: Frame = read_json_line(&mut reader, &mut buffer, 128)
        .unwrap()
        .unwrap();
    let last: Frame = read_json_line(&mut reader, &mut buffer, 128)
        .unwrap()
        .unwrap();
    let eof: Option<Frame> = read_json_line(&mut reader, &mut buffer, 128).unwrap();

    assert_eq!(first.id, 1);
    assert_eq!(last.id, 2);
    assert_eq!(eof, None);
}

#[test]
fn oversized_input_is_consumed_before_returning_an_error() {
    let input = b"{\"value\":\"0123456789\"}\n{\"id\":3,\"value\":\"next\"}\n";
    let mut reader = BufReader::with_capacity(4, Cursor::new(input));
    let mut buffer = Vec::new();

    let error = read_json_line::<_, Frame>(&mut reader, &mut buffer, 16).unwrap_err();
    assert!(matches!(error, FrameError::TooLarge { limit: 16 }));

    let next: Frame = read_json_line(&mut reader, &mut buffer, 128)
        .unwrap()
        .unwrap();
    assert_eq!(next.id, 3);
}

#[test]
fn oversized_output_writes_nothing() {
    let mut output = b"existing".to_vec();
    let error = write_json_line(
        &mut output,
        &Frame {
            id: 1,
            value: "too large".into(),
        },
        8,
    )
    .unwrap_err();

    assert!(matches!(error, FrameError::TooLarge { limit: 8 }));
    assert_eq!(output, b"existing");
}

#[test]
fn malformed_frame_does_not_desynchronize_the_next_frame() {
    let input = b"not-json\n{\"id\":4,\"value\":\"valid\"}\n";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut buffer = Vec::new();

    assert!(matches!(
        read_json_line::<_, Frame>(&mut reader, &mut buffer, 128),
        Err(FrameError::Json(_))
    ));
    let valid: Frame = read_json_line(&mut reader, &mut buffer, 128)
        .unwrap()
        .unwrap();
    assert_eq!(valid.id, 4);
}
