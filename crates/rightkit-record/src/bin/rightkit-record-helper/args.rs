//! Command-line parsing for the Windows helper. Pure, so it is unit tested without a window.

use std::path::PathBuf;

use rightkit_record::windows::Codec;
use rightkit_record::{Code, RecordError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureArgs {
    pub window_id: u64,
    pub out: PathBuf,
    pub fps: u32,
    pub seconds: u32,
    pub codec: Codec,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Capture(CaptureArgs),
}

pub const MAX_SECONDS: u32 = 3600;

const USAGE: &str =
    "usage: rightkit-record-helper capture --hwnd <decimal> --out <dir> --fps <30|60> --seconds <n> [--codec h264|hevc]";

pub fn parse(args: &[String]) -> Result<Command, RecordError> {
    let (subcommand, rest) = args.split_first().ok_or_else(|| invalid(USAGE))?;
    match subcommand.as_str() {
        "capture" => parse_capture(rest).map(Command::Capture),
        other => Err(invalid(format!("unknown subcommand '{other}'"))),
    }
}

fn parse_capture(args: &[String]) -> Result<CaptureArgs, RecordError> {
    let mut hwnd = None;
    let mut out = None;
    let mut fps = None;
    let mut seconds = None;
    let mut codec = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .ok_or_else(|| invalid(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--hwnd" => set_once(&mut hwnd, flag, parse_hwnd(value)?)?,
            "--out" => {
                if value.is_empty() {
                    return Err(invalid("--out must not be empty"));
                }
                set_once(&mut out, flag, PathBuf::from(value))?;
            }
            "--fps" => set_once(&mut fps, flag, parse_fps(value)?)?,
            "--seconds" => set_once(&mut seconds, flag, parse_seconds(value)?)?,
            "--codec" => set_once(&mut codec, flag, parse_codec(value)?)?,
            other => return Err(invalid(format!("unknown flag '{other}'"))),
        }
    }
    Ok(CaptureArgs {
        window_id: required(hwnd, "--hwnd")?,
        out: required(out, "--out")?,
        fps: required(fps, "--fps")?,
        seconds: required(seconds, "--seconds")?,
        codec: codec.unwrap_or(Codec::H264),
    })
}

fn parse_codec(value: &str) -> Result<Codec, RecordError> {
    Codec::parse(value)
        .ok_or_else(|| invalid(format!("--codec must be h264 or hevc, got '{value}'")))
}

fn parse_hwnd(value: &str) -> Result<u64, RecordError> {
    let raw: u64 = value
        .parse()
        .map_err(|_| invalid(format!("--hwnd must be a decimal integer, got '{value}'")))?;
    if raw == 0 || raw > isize::MAX as u64 {
        return Err(invalid(format!("--hwnd out of range: {value}")));
    }
    Ok(raw)
}

fn parse_fps(value: &str) -> Result<u32, RecordError> {
    match value {
        "30" => Ok(30),
        "60" => Ok(60),
        _ => Err(invalid(format!("--fps must be 30 or 60, got '{value}'"))),
    }
}

fn parse_seconds(value: &str) -> Result<u32, RecordError> {
    let seconds: u32 = value
        .parse()
        .map_err(|_| invalid(format!("--seconds must be a whole number, got '{value}'")))?;
    if (1..=MAX_SECONDS).contains(&seconds) {
        Ok(seconds)
    } else {
        Err(invalid(format!(
            "--seconds must be between 1 and {MAX_SECONDS}"
        )))
    }
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

fn invalid(message: impl Into<String>) -> RecordError {
    RecordError::new(Code::ArgInvalid, message)
}
