use std::io::{BufReader, Cursor};

use rightkit_sidecar::{read_json_line, write_json_line, FrameError};
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

use rightkit_sidecar::{read_json_line_with, ReadLimits};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Child half: emits what the parent asks for on stdout, like a misbehaving sidecar.
#[test]
fn child_emitter() {
    let Ok(mode) = std::env::var("RK_FRAMED_MODE") else {
        return;
    };
    let mut out = std::io::stdout().lock();
    out.write_all(b"\nRK_BEGIN\n").unwrap();
    out.flush().unwrap();
    match mode.as_str() {
        // Endless bytes, never a newline; stops when the parent closes the pipe.
        "endless" => {
            let chunk = [b'x'; 8192];
            while out.write_all(&chunk).is_ok() {}
        }
        // 3 MiB oversized frame, then a valid one.
        "oversize_then_ok" => {
            let chunk = [b'y'; 65536];
            for _ in 0..48 {
                out.write_all(&chunk).unwrap();
            }
            out.write_all(b"\n{\"id\":9,\"value\":\"after\"}\n")
                .unwrap();
        }
        // A frame that trickles forever.
        "trickle" => loop {
            if out.write_all(b"z").and_then(|()| out.flush()).is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        },
        _ => {}
    }
}

fn spawn_child(mode: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_emitter", "--nocapture"])
        .env("RK_FRAMED_MODE", mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Skip the libtest preamble the emitter child prints before its payload.
fn skip_preamble(reader: &mut impl std::io::BufRead) {
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "child exited before RK_BEGIN"
        );
        if line.trim_end().ends_with("RK_BEGIN") {
            return;
        }
    }
}

/// Counts bytes pulled so memory use and consumption can be asserted.
struct Counting<R>(R, usize);
impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        self.1 += n;
        Ok(n)
    }
}

#[test]
fn child_that_never_sends_a_newline_cannot_exhaust_memory() {
    let mut child = spawn_child("endless");
    let mut reader = BufReader::new(Counting(child.stdout.take().unwrap(), 0));
    skip_preamble(&mut reader);
    let before = reader.get_ref().1;
    let mut buffer = Vec::new();
    let limits = ReadLimits {
        max_discard: 1 << 20,
        deadline: None,
    };
    let err = read_json_line_with::<_, Frame>(&mut reader, &mut buffer, 1024, limits).unwrap_err();
    match err {
        FrameError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
        other => panic!("expected bounded-discard cut-off, got {other}"),
    }
    assert!(
        buffer.capacity() <= 4096,
        "buffer grew to {}",
        buffer.capacity()
    );
    let read = reader.get_ref().1 - before;
    assert!(read < (1 << 20) + 64 * 1024, "read {read} bytes");
    drop(reader);
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn oversized_frame_from_a_child_is_skipped_and_the_stream_resynchronises() {
    let mut child = spawn_child("oversize_then_ok");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    skip_preamble(&mut reader);
    let mut buffer = Vec::new();
    let err = read_json_line::<_, Frame>(&mut reader, &mut buffer, 1024).unwrap_err();
    assert!(matches!(err, FrameError::TooLarge { limit: 1024 }), "{err}");
    assert!(
        buffer.capacity() <= 4096,
        "buffer grew to {}",
        buffer.capacity()
    );
    let next: Frame = read_json_line(&mut reader, &mut buffer, 1024)
        .unwrap()
        .unwrap();
    assert_eq!(next.id, 9);
    child.wait().unwrap();
}

#[test]
fn trickling_child_hits_the_deadline() {
    let mut child = spawn_child("trickle");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    skip_preamble(&mut reader);
    let mut buffer = Vec::new();
    let limits = ReadLimits {
        max_discard: usize::MAX,
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(300)),
    };
    let err =
        read_json_line_with::<_, Frame>(&mut reader, &mut buffer, 1 << 20, limits).unwrap_err();
    match err {
        FrameError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
        other => panic!("expected deadline, got {other}"),
    }
    drop(reader);
    child.kill().unwrap();
    child.wait().unwrap();
}
