//! The whole-screen mode of `rightkit-record-helper` (rightkit#443):
//!
//! ```text
//! rightkit-record-helper capture --screen --fps 60 --lossless --out <dir>
//!     [--target virtual|monitor:N] [--seconds <n>] [--markers <file>]
//! rightkit-record-helper monitors
//! ```
//!
//! `capture --screen` records the virtual screen (or one monitor) into `<dir>/screen.rkv` with
//! per-frame capture-clock timestamps and writes `<dir>/screen.json` when it ends. It reads
//! commands from stdin, one per line: `mark <label>` stamps a marker on the frames' clock (appended
//! to `--markers`, default `<dir>/markers.ndjson`, and echoed on stdout as a `marker` event), and
//! `stop` ends the take. It also ends at `--seconds`, and, when no `--seconds` is given, when stdin
//! closes. stdout carries one JSON object per line; the last is the summary. Nothing here is
//! compressed: whole-screen capture is lossless only, so `--lossless` is required.
//!
//! Works on Windows (Windows.Graphics.Capture, in process) and macOS (through the Swift helper
//! `rightkit-record-macos-helper`, found as for the window recorder).

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_record::{list_monitors, Code, RecordError, ScreenRecorder, ScreenSpec, ScreenTarget};
use serde_json::{json, Value};

/// The longest `--seconds`.
pub const MAX_SECONDS: u32 = 3600;

const USAGE: &str = "usage: rightkit-record-helper capture --screen --fps <1..120> --lossless --out <dir> [--target virtual|monitor:N] [--seconds <n>] [--markers <file>]\n       rightkit-record-helper monitors";

#[derive(Debug, PartialEq, Eq)]
pub struct ScreenArgs {
    pub target: ScreenTarget,
    pub out: PathBuf,
    pub fps: u32,
    pub seconds: Option<u32>,
    pub markers: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Capture(ScreenArgs),
    Monitors,
}

/// Whether `argv` is a whole-screen command, which this module (not the window CLI) runs.
pub fn wants(argv: &[String]) -> bool {
    match argv.first().map(String::as_str) {
        Some("monitors") => true,
        Some("capture") => argv.iter().any(|a| a == "--screen"),
        _ => false,
    }
}

fn invalid(message: impl Into<String>) -> RecordError {
    RecordError::new(Code::ArgInvalid, message)
}

pub fn parse(argv: &[String]) -> Result<Command, RecordError> {
    let (subcommand, rest) = argv.split_first().ok_or_else(|| invalid(USAGE))?;
    match subcommand.as_str() {
        "monitors" if rest.is_empty() => Ok(Command::Monitors),
        "monitors" => Err(invalid("monitors takes no arguments")),
        "capture" => parse_capture(rest).map(Command::Capture),
        other => Err(invalid(format!("unknown subcommand '{other}'"))),
    }
}

fn parse_capture(args: &[String]) -> Result<ScreenArgs, RecordError> {
    let mut screen = false;
    let mut lossless = false;
    let mut target = None;
    let mut out = None;
    let mut fps = None;
    let mut seconds = None;
    let mut markers = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--screen" => screen = true,
            "--lossless" => lossless = true,
            "--target" | "--out" | "--fps" | "--seconds" | "--markers" => {
                let value = it
                    .next()
                    .ok_or_else(|| invalid(format!("{flag} needs a value")))?;
                match flag.as_str() {
                    "--target" => set_once(&mut target, flag, parse_target(value)?)?,
                    "--out" => set_once(&mut out, flag, non_empty_path(flag, value)?)?,
                    "--fps" => set_once(&mut fps, flag, parse_fps(value)?)?,
                    "--seconds" => set_once(&mut seconds, flag, parse_seconds(value)?)?,
                    _ => set_once(&mut markers, flag, non_empty_path(flag, value)?)?,
                }
            }
            other => return Err(invalid(format!("unknown flag '{other}'"))),
        }
    }
    if !screen {
        return Err(invalid("--screen is required"));
    }
    if !lossless {
        return Err(invalid(
            "--lossless is required: whole-screen capture is lossless only",
        ));
    }
    Ok(ScreenArgs {
        target: target.unwrap_or(ScreenTarget::Virtual),
        out: required(out, "--out")?,
        fps: required(fps, "--fps")?,
        seconds,
        markers,
    })
}

fn parse_target(value: &str) -> Result<ScreenTarget, RecordError> {
    ScreenTarget::parse(value).ok_or_else(|| {
        invalid(format!(
            "--target must be virtual or monitor:N, got '{value}'"
        ))
    })
}

fn parse_fps(value: &str) -> Result<u32, RecordError> {
    match value.parse::<u32>() {
        Ok(fps) if (1..=120).contains(&fps) => Ok(fps),
        _ => Err(invalid(format!(
            "--fps must be between 1 and 120, got '{value}'"
        ))),
    }
}

fn parse_seconds(value: &str) -> Result<u32, RecordError> {
    match value.parse::<u32>() {
        Ok(seconds) if (1..=MAX_SECONDS).contains(&seconds) => Ok(seconds),
        _ => Err(invalid(format!(
            "--seconds must be between 1 and {MAX_SECONDS}"
        ))),
    }
}

fn non_empty_path(flag: &str, value: &str) -> Result<PathBuf, RecordError> {
    if value.is_empty() {
        return Err(invalid(format!("{flag} must not be empty")));
    }
    Ok(PathBuf::from(value))
}

fn set_once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<(), RecordError> {
    if slot.is_some() {
        return Err(invalid(format!("{flag} given more than once")));
    }
    *slot = Some(value);
    Ok(())
}

fn required<T>(slot: Option<T>, flag: &str) -> Result<T, RecordError> {
    slot.ok_or_else(|| invalid(format!("{flag} is required")))
}

/// 2 for bad arguments, 3 for refusals about the target, output or permission, 1 otherwise.
fn exit_code(code: Code) -> i32 {
    match code {
        Code::ArgInvalid => 2,
        Code::NotFound
        | Code::Unsupported
        | Code::PermissionDenied
        | Code::OutExists
        | Code::CodecUnavailable => 3,
        _ => 1,
    }
}

/// Runs the command and returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let outcome = parse(argv).and_then(|command| match command {
        Command::Monitors => monitors(),
        Command::Capture(args) => capture(&args),
    });
    match outcome {
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
                json!({ "ok": false, "code": err.code.as_str(), "message": err.message })
            );
            eprintln!("{err}");
            exit_code(err.code)
        }
    }
}

fn monitors() -> Result<Value, RecordError> {
    let list: Vec<Value> = list_monitors()?.iter().map(|m| m.to_json()).collect();
    Ok(json!({ "ok": true, "monitors": list }))
}

fn capture(args: &ScreenArgs) -> Result<Value, RecordError> {
    let mut spec = ScreenSpec::new(args.target, args.fps, true, args.out.clone());
    spec.markers = args.markers.clone();
    let mut recorder = ScreenRecorder::start_default(spec)?;
    let started = Instant::now();
    println!(
        "{}",
        json!({
            "event": "started",
            "clock": recorder.started().clock,
            "hostNsAtStart": recorder.started().host_ns_at_start,
            "width": recorder.started().width,
            "height": recorder.started().height,
        })
    );

    // Commands arrive on stdin, one per line. The reader ends at end of input.
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = args
        .seconds
        .map(|seconds| started + Duration::from_secs(u64::from(seconds)));
    let mut stdin_open = true;
    loop {
        if recorder.pump().is_some() {
            break;
        }
        if deadline.is_some_and(|due| Instant::now() >= due) {
            break;
        }
        let line = if stdin_open {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => Some(line),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    // Without --seconds, closing stdin is how a driver says stop.
                    if deadline.is_none() {
                        break;
                    }
                    stdin_open = false;
                    None
                }
            }
        } else {
            thread::sleep(Duration::from_millis(50));
            None
        };
        let Some(line) = line else { continue };
        let command = line.trim_end();
        if command == "stop" {
            break;
        }
        match command.strip_prefix("mark ") {
            Some(label) => {
                let marker = recorder.mark(label)?;
                println!(
                    "{}",
                    json!({ "event": "marker", "marker": marker.to_json() })
                );
            }
            None if command.is_empty() => {}
            None => eprintln!(
                "rightkit-record-helper: ignored input line (expected 'mark <label>' or 'stop')"
            ),
        }
    }

    let take = recorder.stop()?;
    Ok(json!({
        "ok": take.clean && take.frames > 0,
        "outDir": take.out,
        "frames": take.frames,
        "dropped": take.dropped,
        "markers": take.markers,
        "width": take.width,
        "height": take.height,
        "clean": take.clean,
        "failure": take.failure,
        "elapsedMs": started.elapsed().as_millis() as u64,
    }))
}
