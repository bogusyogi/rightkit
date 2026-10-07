//! A fake speech host process that speaks HeardRight's v1 wire protocol from the
//! host side, written from `heardright-runtime/src/speech_service/{mod,files,conn,transport}.rs`
//! and deliberately independent of this crate's types (raw JSON only), so the
//! client is tested against the wire format and not against itself.
//!
//! `hr_fake_host <suite-root> <role> <protocol-minor> <state-dir> [--wait-free]`
//!
//! Behaviour is selected by the requested file name:
//!   `*handoff*`   first request per state-dir gets `speech_service_host_handoff`
//!   `*stall*`     never answers
//!   `*slow*`      3 s of work (minor >= 1: progress frames every 200 ms)
//!   `*dictation*` holds while `<state-dir>/dictation_active` exists
//!                 (minor >= 1: `paused_for_dictation` progress frames; minor 0: silent)
//!   otherwise     answers at once
//! The last file request's payload is written to `<state-dir>/last_request.json`; a
//! client disconnect during a job writes `<state-dir>/cancelled`.
//! Like HeardRight's yield watcher, a live `takeover.json` from an outranking role
//! makes this host fail in-flight jobs with the handoff error, remove `service.json`
//! and exit. `--wait-free` binds only once no live `service.json` exists (the
//! requester side of a takeover).

#[cfg(unix)]
mod host {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const ROLES: [&str; 4] = [
        "heard_right_shell",
        "heard_right_headless",
        "scrape_right",
        "cut_right",
    ];
    const ALLOWED: [&str; 4] = [
        "engine_health",
        "engine_capabilities",
        "engine_info",
        "file_transcription_request",
    ];
    static TRACE: AtomicU64 = AtomicU64::new(1);

    fn rank(role: &str) -> u8 {
        ROLES
            .iter()
            .position(|r| *r == role)
            .map(|p| p as u8)
            .unwrap_or(99)
    }

    fn alive(pid: u32) -> bool {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        pid != 0 && pid <= i32::MAX as u32 && unsafe { kill(pid as i32, 0) } == 0
    }

    fn frame(schema: &str, rid: &str, payload: Value, error: Value) -> String {
        let f = json!({
            "protocol_major": 1, "protocol_minor": 0, "schema_name": schema, "schema_version": 1,
            "engine_version": "fake-hr-host/1", "request_id": rid, "session_id": null,
            "trace_id": format!("svc-{}", TRACE.fetch_add(1, Ordering::Relaxed)),
            "payload": payload, "error": error,
        });
        format!("{f}\n")
    }

    fn error_frame(rid: &str, code: &str, message: &str, retryable: bool) -> String {
        frame(
            "engine_error",
            rid,
            Value::Null,
            json!({"code": code, "message": message, "retryable": retryable, "diagnostics": null}),
        )
    }

    struct Cfg {
        role: String,
        minor: u64,
        state: PathBuf,
        accepting: AtomicBool,
    }

    fn engines() -> Value {
        json!([
            {"id": "parakeet", "languages": ["en", "es", "fr", "de", "it", "pt", "nl"], "ready": true, "loaded": true, "model": "parakeet-tdt-v3"},
            {"id": "whisper", "languages": ["en", "es", "fr", "de", "ja", "zh"], "ready": true, "loaded": false, "model": "whisper-large-v3-turbo"},
            {"id": "future", "languages": [], "ready": false, "loaded": false}
        ])
    }

    enum Msg {
        Line(String),
        Closed,
    }

    fn serve(cfg: Arc<Cfg>, stream: UnixStream) {
        let mut writer = stream.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stream);
            loop {
                let mut line = String::new();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Msg::Closed);
                        return;
                    }
                    Ok(_) => {
                        if tx.send(Msg::Line(line)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        // rid, path, started, engine echo
        type Job = (String, String, Instant, Option<(String, Option<String>)>);
        let mut job: Option<Job> = None;
        let mut last_progress = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Msg::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some((_, path, ..)) = job {
                        let _ = std::fs::write(cfg.state.join("cancelled"), path);
                    }
                    return;
                }
                Ok(Msg::Line(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = writer.write_all(
                                error_frame(
                                    "",
                                    "E_BAD_FRAME",
                                    &format!("malformed frame: {e}"),
                                    false,
                                )
                                .as_bytes(),
                            );
                            continue;
                        }
                    };
                    let rid = v["request_id"].as_str().unwrap_or("").to_string();
                    if v["protocol_major"] != 1
                        || v["schema_version"] != 1
                        || v["schema_name"].as_str().is_none()
                        || v["trace_id"].as_str().is_none()
                    {
                        let _ = writer.write_all(
                            error_frame(&rid, "E_BAD_FRAME", "invalid frame", false).as_bytes(),
                        );
                        continue;
                    }
                    let schema = v["schema_name"].as_str().unwrap().to_string();
                    if !ALLOWED.contains(&schema.as_str()) {
                        let _ = writer.write_all(
                            error_frame(
                                &rid,
                                "speech_service_request_not_allowed",
                                "request not allowed on the speech service",
                                false,
                            )
                            .as_bytes(),
                        );
                        continue;
                    }
                    let kind = v["payload"]["kind"].as_str().unwrap_or("");
                    let out = match (schema.as_str(), kind) {
                        ("engine_health", "health") => frame(
                            "engine_health",
                            &rid,
                            json!({"kind":"health","status":"ok","diagnostics":null}),
                            Value::Null,
                        ),
                        ("engine_capabilities", "capabilities") => {
                            let mut p = json!({"kind":"capabilities","capabilities":["file_transcription"]});
                            if cfg.minor >= 1 {
                                p["engines"] = engines();
                            }
                            frame("engine_capabilities", &rid, p, Value::Null)
                        }
                        ("engine_info", "engine_info") => frame(
                            "engine_info",
                            &rid,
                            json!({"kind":"engine_info","mode":"speech_service","cold_load_s":1.5,"engine_version":"fake-hr-host/1"}),
                            Value::Null,
                        ),
                        ("file_transcription_request", "file_transcription_request") => {
                            let Some(path) = v["payload"]["path"].as_str() else {
                                let _ = writer.write_all(
                                    error_frame(&rid, "E_BAD_FRAME", "missing file path", false)
                                        .as_bytes(),
                                );
                                continue;
                            };
                            let _ = std::fs::write(
                                cfg.state.join("last_request.json"),
                                v["payload"].to_string(),
                            );
                            if !cfg.accepting.load(Ordering::SeqCst) {
                                let _ = writer.write_all(
                                    error_frame(
                                        &rid,
                                        "speech_service_host_handoff",
                                        "speech service host is handing off",
                                        true,
                                    )
                                    .as_bytes(),
                                );
                                continue;
                            }
                            if path.contains("handoff") {
                                let marker = cfg.state.join("handoff_done");
                                if !marker.exists() {
                                    let _ = std::fs::write(&marker, b"1");
                                    let _ = writer.write_all(
                                        error_frame(
                                            &rid,
                                            "speech_service_host_handoff",
                                            "speech service host is handing off",
                                            true,
                                        )
                                        .as_bytes(),
                                    );
                                    continue;
                                }
                            }
                            // Engine selection exists only on a minor >= 1 host.
                            let mut echo = None;
                            if cfg.minor >= 1 {
                                let lang = v["payload"]["language"].as_str().map(str::to_string);
                                let engine = match v["payload"]["engine"].as_str() {
                                    None | Some("parakeet") => "parakeet".to_string(),
                                    Some("auto") => {
                                        if matches!(lang.as_deref(), Some("ja") | Some("zh")) {
                                            "whisper".into()
                                        } else {
                                            "parakeet".into()
                                        }
                                    }
                                    Some("whisper") => "whisper".into(),
                                    Some(other) => {
                                        let _ = writer.write_all(
                                            error_frame(
                                                &rid,
                                                "speech_service_engine_unavailable",
                                                &format!("engine {other} is not ready"),
                                                false,
                                            )
                                            .as_bytes(),
                                        );
                                        continue;
                                    }
                                };
                                echo = Some((engine, lang));
                            }
                            if path.contains("stall")
                                || path.contains("slow")
                                || path.contains("dictation")
                            {
                                job = Some((rid, path.to_string(), Instant::now(), echo));
                                last_progress = Instant::now() - Duration::from_secs(1);
                                continue;
                            }
                            result(&rid, &echo)
                        }
                        _ => error_frame(&rid, "E_BAD_FRAME", "payload schema mismatch", false),
                    };
                    let _ = writer.write_all(out.as_bytes());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !cfg.accepting.load(Ordering::SeqCst) {
                if let Some((rid, ..)) = job.take() {
                    let _ = writer.write_all(
                        error_frame(
                            &rid,
                            "speech_service_host_handoff",
                            "speech service host is handing off",
                            true,
                        )
                        .as_bytes(),
                    );
                }
            }
            if let Some((rid, path, started, echo)) = job.clone() {
                if path.contains("stall") {
                    continue;
                }
                let paused =
                    path.contains("dictation") && cfg.state.join("dictation_active").exists();
                if cfg.minor >= 1 && last_progress.elapsed() >= Duration::from_millis(200) {
                    last_progress = Instant::now();
                    let state = if paused {
                        "paused_for_dictation"
                    } else {
                        "running"
                    };
                    let frac = if paused {
                        Value::Null
                    } else {
                        json!((started.elapsed().as_secs_f64() / 3.0).min(0.99))
                    };
                    let _ = writer.write_all(frame("file_transcription_progress", &rid, json!({"kind":"file_transcription_progress","fraction":frac,"state":state,"message":null}), Value::Null).as_bytes());
                }
                let done = if path.contains("slow") {
                    started.elapsed() >= Duration::from_secs(3)
                } else {
                    !paused && started.elapsed() >= Duration::from_millis(300)
                };
                if done {
                    job = None;
                    let _ = writer.write_all(result(&rid, &echo).as_bytes());
                }
            }
        }
    }

    fn result(rid: &str, echo: &Option<(String, Option<String>)>) -> String {
        let text = match echo {
            Some((e, _)) => format!("hello from {e}"),
            None => "hello from parakeet".to_string(),
        };
        let mut p = json!({"kind":"file_transcription_result","text":text,"srt":"1\n00:00:00,000 --> 00:00:00,400\nhello\n","vtt":"WEBVTT\n","words":[{"text":"hello","start_ms":0,"end_ms":400}]});
        if let Some((e, l)) = echo {
            p["engine"] = json!(e);
            p["language"] = json!(l);
        }
        frame("file_transcription_result", rid, p, Value::Null)
    }

    pub fn main() {
        let a: Vec<String> = std::env::args().skip(1).collect();
        let (root, role, minor, state) = (
            PathBuf::from(&a[0]),
            a[1].clone(),
            a[2].parse::<u64>().unwrap(),
            PathBuf::from(&a[3]),
        );
        let dir = root.join("runtime").join("speech-service");
        let record_path = dir.join("service.json");
        let live = |p: &Path| -> bool {
            std::fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| v["pid"].as_u64())
                .is_some_and(|pid| alive(pid as u32))
        };
        if a.iter().any(|x| x == "--wait-free") {
            while live(&record_path) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sock = dir.join("speech.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)).unwrap();
        let me = std::process::id();
        let rec = json!({"protocol_major": 1, "protocol_minor": minor, "role": role, "pid": me, "endpoint": sock.to_string_lossy(), "engine_version": "fake-hr-host/1"});
        let tmp = dir.join(format!("service.json.{me}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(&rec).unwrap()).unwrap();
        std::fs::rename(&tmp, &record_path).unwrap();
        let cfg = Arc::new(Cfg {
            role: role.clone(),
            minor,
            state,
            accepting: AtomicBool::new(true),
        });

        let c = cfg.clone();
        let (rp, tk) = (record_path.clone(), dir.join("takeover.json"));
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(100));
            let Some(t) = std::fs::read(&tk)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            else {
                continue;
            };
            let (trole, tpid) = (
                t["role"].as_str().unwrap_or("").to_string(),
                t["pid"].as_u64().unwrap_or(0) as u32,
            );
            if tpid != std::process::id() && rank(&trole) < rank(&c.role) && alive(tpid) {
                c.accepting.store(false, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(400));
                if std::fs::read(&rp)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .is_some_and(|v| v["pid"] == std::process::id())
                {
                    let _ = std::fs::remove_file(&rp);
                }
                std::thread::sleep(Duration::from_millis(500));
                std::process::exit(0);
            }
        });

        println!("READY {me}");
        for stream in listener.incoming().flatten() {
            let c = cfg.clone();
            std::thread::spawn(move || serve(c, stream));
        }
    }
}

fn main() {
    #[cfg(unix)]
    host::main();
    #[cfg(not(unix))]
    eprintln!("hr_fake_host: the Windows named-pipe variant is exercised on the Windows host");
}
