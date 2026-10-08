//! rightkit-record-helper: the out-of-process Windows capture helper (RK3).
//!
//! A thin CLI over `rightkit_record::windows::WindowsBackend`. The core owns the
//! bundle. This binary writes only the media file, and prints one JSON summary
//! of the take on stdout. rightkit-process will own this helper later.

#[cfg(windows)]
mod args;

#[cfg(windows)]
fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let outcome = args::parse(&argv).and_then(|command| match command {
        args::Command::Capture(capture) => windows_run::run(&capture),
    });
    let code = match outcome {
        Ok(summary) => {
            println!("{summary}");
            if summary["ok"] == true {
                0
            } else {
                1
            }
        }
        Err(err) => {
            println!(
                "{}",
                serde_json::json!({ "ok": false, "code": err.code.as_str(), "message": err.message })
            );
            eprintln!("{err}");
            exit_code(err.code)
        }
    };
    std::process::exit(code);
}

/// 2 for bad arguments, 3 for refusals about the window, output or permission, 1 otherwise.
#[cfg(windows)]
fn exit_code(code: rightkit_record::Code) -> i32 {
    use rightkit_record::Code;
    match code {
        Code::ArgInvalid => 2,
        Code::NotFound | Code::Unsupported | Code::PermissionDenied | Code::OutExists => 3,
        _ => 1,
    }
}

#[cfg(windows)]
mod windows_run {
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};

    use rightkit_contracts::types::record_session::VisibilityState;
    use rightkit_record::backend::{Backend, BackendPlan, HelperEvent};
    use rightkit_record::windows::WindowsBackend;
    use rightkit_record::{Code, PixelMasking, RecordError};
    use serde_json::{json, Value};

    use crate::args::CaptureArgs;

    /// Counts what the helper reports during one take.
    #[derive(Default)]
    struct Counts {
        ready: bool,
        frames: u64,
        dropped_backpressure: u64,
        dropped_unknown: u64,
        failure: Option<String>,
    }

    impl Counts {
        fn record(&mut self, event: HelperEvent) {
            match event {
                HelperEvent::Ready(_) => {
                    self.ready = true;
                    self.frames += 1;
                }
                HelperEvent::Frame(_) => self.frames += 1,
                HelperEvent::Dropped { count, reason, .. } => {
                    use rightkit_contracts::types::record_event::DropReason;
                    match reason {
                        DropReason::Backpressure => self.dropped_backpressure += u64::from(count),
                        _ => self.dropped_unknown += u64::from(count),
                    }
                }
                HelperEvent::Failed(failure) => {
                    self.failure.get_or_insert_with(|| failure.reason());
                }
                HelperEvent::Exited => {
                    self.failure.get_or_insert_with(|| "helper_lost".into());
                }
                HelperEvent::Geometry { .. } => {}
            }
        }

        fn drain(&mut self, backend: &mut WindowsBackend) {
            for event in backend.poll() {
                self.record(event);
            }
        }
    }

    pub fn run(args: &CaptureArgs) -> Result<Value, RecordError> {
        prepare_out_dir(args)?;
        let mut backend = WindowsBackend::new(args.codec);
        let plan = BackendPlan {
            out_dir: args.out.clone(),
            window_id: args.window_id,
            // The Windows backend addresses the window by HWND; the pid is not used.
            pid: 0,
            fps: args.fps,
            // The helper reports the native scale it captured at, not a requested one.
            scale: 1.0,
            audio: Vec::new(),
            visibility: VisibilityState::OffScreenVisible,
            masking: PixelMasking::Declared(Vec::new()),
        };
        let started = Instant::now();
        let info = backend.start(&plan)?;

        let mut counts = Counts::default();
        let deadline = started + Duration::from_secs(u64::from(args.seconds));
        while Instant::now() < deadline && counts.failure.is_none() {
            thread::sleep(Duration::from_millis(100));
            counts.drain(&mut backend);
        }

        let clean = if counts.failure.is_none() {
            let finished = backend.stop()?;
            counts.drain(&mut backend);
            finished.clean
        } else {
            backend.abort();
            counts.drain(&mut backend);
            false
        };

        let ok = clean && counts.failure.is_none() && counts.ready;
        Ok(json!({
            "ok": ok,
            "outDir": args.out,
            "codec": info.codec,
            "requestedFps": args.fps,
            "width": info.width,
            "height": info.height,
            "dpi": info.dpi,
            "scale": info.scale,
            "boundsPt": {
                "x": info.bounds_pt.x,
                "y": info.bounds_pt.y,
                "width": info.bounds_pt.width,
                "height": info.bounds_pt.height,
            },
            "osBuild": info.os_build,
            "encoder": backend.encoder(),
            "frames": counts.frames,
            "readyMarker": counts.ready,
            "droppedBackpressure": counts.dropped_backpressure,
            "droppedUnknown": counts.dropped_unknown,
            "clean": clean,
            "failure": counts.failure,
            "elapsedMs": started.elapsed().as_millis() as u64,
            "summary": backend.summary(),
        }))
    }

    /// Creates the output directory and refuses one that already holds files.
    fn prepare_out_dir(args: &CaptureArgs) -> Result<(), RecordError> {
        fs::create_dir_all(&args.out)
            .map_err(|e| RecordError::new(Code::Io, format!("create out dir failed: {e}")))?;
        let mut entries = fs::read_dir(&args.out)
            .map_err(|e| RecordError::new(Code::Io, format!("read out dir failed: {e}")))?;
        if entries.next().is_some() {
            return Err(RecordError::new(
                Code::OutExists,
                format!("output directory is not empty: {}", args.out.display()),
            ));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn main() {
    let body = serde_json::json!({
        "ok": false,
        "code": "unsupported",
        "message": "rightkit-record-helper captures only on Windows; the macOS helper is not part of this crate",
    });
    println!("{body}");
    eprintln!("{}", body["message"].as_str().unwrap_or_default());
    std::process::exit(1);
}
