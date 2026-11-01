//! rightkit-sidecar: framed transports over a real loopback TCP socket between two threads: the typed JSON-lines wire
//! envelope with an oversized frame that is drained, answered by id and resynchronised, the length-prefixed binary-safe
//! framing, and (unix) deadline-bounded framing over an OS pipe including a real timeout.
use rightkit_sidecar::length_prefixed::{read_frame, write_frame, write_frame_bytes};
use rightkit_sidecar::wire::{Frame, WireError};
use rightkit_sidecar::{
    read_json_line_or_oversized, write_json_line_flush, FrameError, Incoming, ReadLimits,
};
use serde_json::{json, Value};
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const LIMIT: usize = 1024;

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-sidecar: {what}"))
    }
}

fn e<T: std::fmt::Debug>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-sidecar: {what}: {err:?}")
}

fn read_response(reader: &mut BufReader<TcpStream>, buf: &mut Vec<u8>) -> Result<Frame, String> {
    match read_json_line_or_oversized::<_, Frame>(reader, buf, LIMIT * 8, ReadLimits::default())
        .map_err(e("client read"))?
    {
        Some(Incoming::Frame(f)) => Ok(f),
        Some(Incoming::Oversized(_)) => {
            Err("rightkit-sidecar: the client saw an oversized response".into())
        }
        None => Err("rightkit-sidecar: the server closed before answering".into()),
    }
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    // ---- Typed JSON-lines envelope over loopback.
    let listener = TcpListener::bind("127.0.0.1:0").map_err(e("bind"))?;
    let addr = listener.local_addr().map_err(e("local_addr"))?;
    let server = thread::spawn(move || -> Result<usize, String> {
        let (stream, _) = listener.accept().map_err(e("accept"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(e("timeout"))?;
        let mut writer = stream.try_clone().map_err(e("clone"))?;
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        let mut served = 0;
        loop {
            match read_json_line_or_oversized::<_, Frame>(
                &mut reader,
                &mut buf,
                LIMIT,
                ReadLimits::default(),
            )
            .map_err(e("server read"))?
            {
                Some(Incoming::Frame(Frame::Request {
                    id, method, params, ..
                })) => {
                    served += 1;
                    write_json_line_flush(
                        &mut writer,
                        &Frame::ok(id, json!({ "method": method, "params": params })),
                        LIMIT * 8,
                    )
                    .map_err(e("server write"))?;
                }
                Some(Incoming::Oversized(o)) => {
                    served += 1;
                    let id = o.id().unwrap_or(0);
                    write_json_line_flush(
                        &mut writer,
                        &Frame::err(
                            id,
                            WireError::new("frame_too_large", format!("{} bytes", o.bytes)),
                        ),
                        LIMIT * 8,
                    )
                    .map_err(e("server write err"))?;
                }
                Some(Incoming::Frame(Frame::Bye { .. })) | None => return Ok(served),
                Some(Incoming::Frame(_)) => {}
            }
        }
    });

    let stream = TcpStream::connect(addr).map_err(e("connect"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(e("timeout"))?;
    let mut writer = stream.try_clone().map_err(e("clone"))?;
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();

    write_json_line_flush(
        &mut writer,
        &Frame::request(1, "echo", json!({ "k": "v" })),
        LIMIT,
    )
    .map_err(e("write request 1"))?;
    match read_response(&mut reader, &mut buf)? {
        Frame::Response {
            id: 1,
            ok: true,
            result: Some(r),
            ..
        } => ensure(
            r == json!({ "method": "echo", "params": { "k": "v" } }),
            &format!("echo result was {r}"),
        )?,
        other => return Err(format!("rightkit-sidecar: request 1 got {other:?}")),
    }

    // An oversized frame: drained, answered by its id, and the stream is still on a frame boundary.
    let big = format!(
        r#"{{"type":"request","id":2,"method":"big","params":"{}"}}"#,
        "x".repeat(LIMIT * 4)
    );
    writer
        .write_all(big.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(e("write oversized"))?;
    match read_response(&mut reader, &mut buf)? {
        Frame::Response {
            id: 2,
            ok: false,
            error: Some(err),
            ..
        } => ensure(
            err.code == "frame_too_large",
            &format!("oversize error code {}", err.code),
        )?,
        other => {
            return Err(format!(
                "rightkit-sidecar: the oversized frame got {other:?}"
            ))
        }
    }
    write_json_line_flush(&mut writer, &Frame::request(3, "after", json!(null)), LIMIT)
        .map_err(e("write request 3"))?;
    match read_response(&mut reader, &mut buf)? {
        Frame::Response { id: 3, ok: true, .. } => {}
        other => return Err(format!("rightkit-sidecar: the stream did not resynchronise after the oversized frame: {other:?}")),
    }
    // The writer refuses an over-limit frame without writing a byte.
    let huge = Frame::request(4, "huge", json!("y".repeat(LIMIT * 2)));
    ensure(
        matches!(
            write_json_line_flush(&mut writer, &huge, LIMIT),
            Err(FrameError::TooLarge { .. })
        ),
        "an over-limit frame was written",
    )?;
    write_json_line_flush(
        &mut writer,
        &Frame::Bye {
            reason: "done".into(),
        },
        LIMIT,
    )
    .map_err(e("write bye"))?;
    let served = server
        .join()
        .map_err(|_| "rightkit-sidecar: the server thread panicked".to_string())??;
    ensure(
        served == 3,
        &format!("server served {served} frames, expected 3"),
    )?;

    // ---- Length-prefixed framing (binary-safe) over loopback.
    let listener = TcpListener::bind("127.0.0.1:0").map_err(e("bind 2"))?;
    let addr = listener.local_addr().map_err(e("local_addr 2"))?;
    let echo = thread::spawn(move || -> Result<usize, String> {
        let (mut s, _) = listener.accept().map_err(e("accept 2"))?;
        s.set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(e("timeout 2"))?;
        let mut n = 0;
        while let Some(v) =
            read_frame::<_, Value>(&mut s, LIMIT).map_err(e("prefixed server read"))?
        {
            n += 1;
            write_frame(&mut s, &json!({ "echo": v }), LIMIT)
                .map_err(e("prefixed server write"))?;
        }
        Ok(n)
    });
    let mut c = TcpStream::connect(addr).map_err(e("connect 2"))?;
    c.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(e("timeout c2"))?;
    let payload = json!({ "text": "line one\nline two", "n": [1, 2, 3] });
    write_frame(&mut c, &payload, LIMIT).map_err(e("prefixed write"))?;
    let back: Value = read_frame(&mut c, LIMIT)
        .map_err(e("prefixed read"))?
        .ok_or("rightkit-sidecar: prefixed echo closed")?;
    ensure(
        back == json!({ "echo": payload }),
        &format!("prefixed echo was {back}"),
    )?;
    ensure(
        matches!(
            write_frame_bytes(&mut c, &vec![b'z'; LIMIT + 1], LIMIT),
            Err(FrameError::TooLarge { .. })
        ),
        "an over-limit length-prefixed frame was written",
    )?;
    drop(c);
    let n = echo
        .join()
        .map_err(|_| "rightkit-sidecar: the prefixed server panicked".to_string())??;
    ensure(
        n == 1,
        &format!("prefixed server read {n} frames, expected 1"),
    )?;

    // ---- Deadline-bounded framing over an OS pipe (the production sidecar transport), unix only: the Windows form needs
    // an overlapped handle, which std::io::pipe does not make.
    #[cfg(unix)]
    let deadline_checked = {
        use rightkit_sidecar::DeadlineFramed;
        use std::time::Instant;
        let (r, w) = std::io::pipe().map_err(e("pipe"))?;
        let mut rx = DeadlineFramed::new(r).map_err(e("DeadlineFramed rx"))?;
        let mut tx = DeadlineFramed::new(w).map_err(e("DeadlineFramed tx"))?;
        tx.write_frame(
            b"over a pipe",
            Instant::now() + Duration::from_secs(5),
            LIMIT,
        )
        .map_err(e("deadline write"))?;
        let got = rx
            .read_frame(Instant::now() + Duration::from_secs(5), LIMIT)
            .map_err(e("deadline read"))?;
        ensure(got == b"over a pipe", "deadline round trip")?;
        match rx.read_frame(Instant::now() + Duration::from_millis(150), LIMIT) {
            Err(FrameError::TimedOut { partial_bytes: 0 }) => {}
            other => {
                return Err(format!(
                    "rightkit-sidecar: an empty pipe did not time out cleanly: {other:?}"
                ))
            }
        }
        true
    };
    #[cfg(not(unix))]
    let deadline_checked = false;

    Ok(
        json!({ "json_lines_served": served, "length_prefixed_frames": n, "deadline_pipe_checked": deadline_checked }),
    )
}
