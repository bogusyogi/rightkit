//! Windows named-pipe path: a real pipe server (created in this test with the Win32
//! API, speaking HeardRight's v1 line protocol) at the endpoint name `endpoint_for`
//! produces, discovered through a real `service.json`, and driven by the real client
//! (`connect` handshake = health round trip, then `capabilities`). The Unix e2e suite
//! covers the socket transport; this covers `\\.\pipe\orthic-labs-speech-...`.
#![cfg(windows)]

use rightkit_speech_client::*;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};

fn frame(schema: &str, rid: &str, payload: Value) -> String {
    format!(
        "{}\n",
        json!({
            "protocol_major": 1, "protocol_minor": 0, "schema_name": schema, "schema_version": 1,
            "engine_version": "pipe-test-host/1", "request_id": rid, "session_id": null,
            "trace_id": "pipe-test", "payload": payload, "error": null,
        })
    )
}

#[test]
fn client_round_trips_health_and_capabilities_over_a_real_named_pipe() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = endpoint_for(root.path(), "S-1-5-TEST");
    assert!(
        endpoint.starts_with(r"\\.\pipe\orthic-labs-speech-S-1-5-TEST-"),
        "{endpoint}"
    );

    // Pipe server: bound before the record is published, like a real host.
    let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    assert!(!handle.is_invalid(), "{}", std::io::Error::last_os_error());
    let raw = handle.0 as usize;
    let server = std::thread::spawn(move || {
        let h = windows::Win32::Foundation::HANDLE(raw as RawHandle);
        // A client that connected before this call yields ERROR_PIPE_CONNECTED, which is fine.
        let _ = unsafe { ConnectNamedPipe(h, None) };
        let file = unsafe { File::from_raw_handle(raw as RawHandle) };
        let mut writer = file.try_clone().unwrap();
        let mut reader = BufReader::new(file);
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return seen;
            }
            let v: Value = serde_json::from_str(&line).unwrap();
            let rid = v["request_id"].as_str().unwrap().to_string();
            let schema = v["schema_name"].as_str().unwrap().to_string();
            let out = match schema.as_str() {
                "engine_health" => frame(
                    "engine_health",
                    &rid,
                    json!({"kind":"health","status":"ok","diagnostics":null}),
                ),
                "engine_capabilities" => frame(
                    "engine_capabilities",
                    &rid,
                    json!({"kind":"capabilities","capabilities":["file_transcription"]}),
                ),
                other => panic!("unexpected schema {other}"),
            };
            seen.push(schema);
            writer.write_all(out.as_bytes()).unwrap();
            writer.flush().unwrap();
        }
    });

    let dir = service_dir(root.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        service_record_path(root.path()),
        json!({
            "protocol_major": SERVICE_PROTOCOL_MAJOR, "protocol_minor": 0,
            "service": SERVICE_NAME, "role": HostRole::CutRight.as_str(), "rank": 3,
            "pid": std::process::id(), "endpoint": endpoint,
            "version": "pipe-test", "engine_version": "pipe-test",
        })
        .to_string(),
    )
    .unwrap();

    let record = read_live_record(root.path()).expect("live record");
    assert_eq!(record.endpoint, endpoint);

    let mut client = connect(root.path()).expect("connect + health handshake over the pipe");
    assert_eq!(client.role(), HostRole::CutRight);
    let caps = client
        .capabilities(Duration::from_secs(5))
        .expect("capabilities round trip over the pipe");
    assert!(caps
        .capabilities
        .iter()
        .any(|c| c == CAPABILITY_FILE_TRANSCRIPTION));
    drop(client);

    let seen = server.join().unwrap();
    assert_eq!(seen, ["engine_health", "engine_capabilities"]);
}
