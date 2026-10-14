//! Deadline-bounded framing over a real named pipe. The client end is
//! overlapped (what the crate requires); the server end is an ordinary
//! blocking handle, so the test can drive it from std.
#![cfg(windows)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rightkit_sidecar::{read_frame_deadline, write_frame_deadline, DeadlineFramed, FrameError};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// `ERROR_PIPE_CONNECTED`: the client connected before the server asked.
const ERROR_PIPE_CONNECTED: i32 = 535;

/// A blocking server end and an overlapped client end of one named pipe.
fn pipe() -> (File, DeadlineFramed<OwnedHandle>) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name: Vec<u16> = format!(
        r"\\.\pipe\rightkit-framed-sidecar-test-{}-{n}",
        std::process::id()
    )
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect();

    // SAFETY: NUL-terminated wide name, default security, a single instance.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            65536,
            65536,
            0,
            null(),
        )
    };
    assert_ne!(server, INVALID_HANDLE_VALUE, "CreateNamedPipeW failed");

    // SAFETY: NUL-terminated wide name; the pipe instance was just created.
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            null_mut(),
        )
    };
    assert_ne!(client, INVALID_HANDLE_VALUE, "CreateFileW failed");

    // SAFETY: synchronous connect on a blocking server handle.
    if unsafe { ConnectNamedPipe(server, null_mut()) } == 0 {
        let err = std::io::Error::last_os_error();
        assert_eq!(err.raw_os_error(), Some(ERROR_PIPE_CONNECTED), "{err}");
    }

    // SAFETY: both handles are valid, owned here, and closed exactly once.
    let server = unsafe { OwnedHandle::from_raw_handle(server) };
    let client = unsafe { OwnedHandle::from_raw_handle(client) };
    (File::from(server), DeadlineFramed::new(client).unwrap())
}

#[test]
fn named_pipe_round_trip_both_directions() {
    let (mut server, mut client) = pipe();
    let deadline = Instant::now() + Duration::from_secs(10);

    client.write_frame(b"hello", deadline, 1024).unwrap();
    let mut header = [0u8; 4];
    server.read_exact(&mut header).unwrap();
    assert_eq!(u32::from_be_bytes(header), 5);
    let mut body = [0u8; 5];
    server.read_exact(&mut body).unwrap();
    assert_eq!(&body, b"hello");

    server.write_all(&[0, 0, 0, 3]).unwrap();
    server.write_all(b"abc").unwrap();
    assert_eq!(client.read_frame(deadline, 1024).unwrap(), b"abc");
}

#[test]
fn free_functions_round_trip_over_the_raw_handle() {
    let (mut server, client) = pipe();
    let deadline = Instant::now() + Duration::from_secs(10);

    write_frame_deadline(client.get_ref(), b"raw", deadline, 1024).unwrap();
    let mut header = [0u8; 4];
    server.read_exact(&mut header).unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
    server.read_exact(&mut body).unwrap();
    assert_eq!(body, b"raw");

    server.write_all(&[0, 0, 0, 4]).unwrap();
    server.write_all(b"back").unwrap();
    let back = read_frame_deadline(client.get_ref(), deadline, 1024).unwrap();
    assert_eq!(back, b"back");
}

#[test]
fn named_pipe_silent_peer_times_out_with_no_bytes_consumed() {
    let (server, mut client) = pipe();

    match client.read_frame(Instant::now() + Duration::from_millis(150), 1024) {
        Err(FrameError::TimedOut { partial_bytes }) => assert_eq!(partial_bytes, 0),
        other => panic!("expected TimedOut, got {other:?}"),
    }
    drop(server);
}

#[test]
fn named_pipe_large_frame_across_partial_transfers() {
    let (mut server, mut client) = pipe();
    let body: Vec<u8> = (0..3usize << 20).map(|i| (i % 251) as u8).collect();
    let expected = body.clone();

    // The 64 KiB pipe buffer forces every 3 MiB write to complete in parts.
    let reader = std::thread::spawn(move || {
        let mut header = [0u8; 4];
        server.read_exact(&mut header).unwrap();
        let mut got = vec![0u8; u32::from_be_bytes(header) as usize];
        server.read_exact(&mut got).unwrap();
        got
    });

    client
        .write_frame(&body, Instant::now() + Duration::from_secs(30), 4 << 20)
        .unwrap();
    let got = reader.join().unwrap();
    assert!(got == expected, "frame differs after partial transfers");
}
