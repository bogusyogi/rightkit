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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_full_capture_command() {
        let parsed = parse(&argv(&[
            "capture",
            "--hwnd",
            "24775762",
            "--out",
            "D:/tmp/out",
            "--fps",
            "60",
            "--seconds",
            "5",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            Command::Capture(CaptureArgs {
                window_id: 24_775_762,
                out: PathBuf::from("D:/tmp/out"),
                fps: 60,
                seconds: 5,
                codec: Codec::H264,
            })
        );
    }

    #[test]
    fn codec_defaults_to_h264_and_accepts_each_value() {
        let base = [
            "capture",
            "--hwnd",
            "1",
            "--out",
            "o",
            "--fps",
            "30",
            "--seconds",
            "1",
        ];
        let Command::Capture(default) = parse(&argv(&base)).unwrap();
        assert_eq!(default.codec, Codec::H264);
        for (value, codec) in [("h264", Codec::H264), ("hevc", Codec::Hevc)] {
            let mut items = base.to_vec();
            items.extend(["--codec", value]);
            let Command::Capture(c) = parse(&argv(&items)).unwrap();
            assert_eq!(c.codec, codec, "codec {value}");
        }
    }

    #[test]
    fn rejects_unknown_and_duplicate_codec_including_the_retired_bmp_path() {
        let base = [
            "capture",
            "--hwnd",
            "1",
            "--out",
            "o",
            "--fps",
            "30",
            "--seconds",
            "1",
        ];
        for bad in ["vp9", "bmp"] {
            let mut items = base.to_vec();
            items.extend(["--codec", bad]);
            let err = parse(&argv(&items)).unwrap_err();
            assert_eq!(err.code, Code::ArgInvalid);
            assert!(err.message.contains(bad));
        }
        let mut dup = base.to_vec();
        dup.extend(["--codec", "h264", "--codec", "hevc"]);
        assert!(parse(&argv(&dup))
            .unwrap_err()
            .message
            .contains("more than once"));
    }

    #[test]
    fn flag_order_does_not_matter() {
        let parsed = parse(&argv(&[
            "capture",
            "--seconds",
            "2",
            "--fps",
            "30",
            "--out",
            "o",
            "--hwnd",
            "7",
        ]))
        .unwrap();
        let Command::Capture(c) = parsed;
        assert_eq!((c.window_id, c.fps, c.seconds), (7, 30, 2));
    }

    #[test]
    fn rejects_unsupported_fps() {
        let err = parse(&argv(&[
            "capture",
            "--hwnd",
            "1",
            "--out",
            "o",
            "--fps",
            "24",
            "--seconds",
            "1",
        ]))
        .unwrap_err();
        assert_eq!(err.code, Code::ArgInvalid);
    }

    #[test]
    fn rejects_zero_hwnd_and_non_decimal_hwnd() {
        for bad in ["0", "0x1A", "-5", "abc"] {
            let err = parse(&argv(&[
                "capture",
                "--hwnd",
                bad,
                "--out",
                "o",
                "--fps",
                "30",
                "--seconds",
                "1",
            ]))
            .unwrap_err();
            assert_eq!(err.code, Code::ArgInvalid, "hwnd {bad}");
        }
    }

    #[test]
    fn rejects_seconds_out_of_range() {
        for bad in ["0", "3601", "x"] {
            let err = parse(&argv(&[
                "capture",
                "--hwnd",
                "1",
                "--out",
                "o",
                "--fps",
                "30",
                "--seconds",
                bad,
            ]))
            .unwrap_err();
            assert_eq!(err.code, Code::ArgInvalid, "seconds {bad}");
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_flags() {
        let missing = parse(&argv(&[
            "capture", "--hwnd", "1", "--out", "o", "--fps", "30",
        ]))
        .unwrap_err();
        assert!(missing.message.contains("--seconds"));
        let dup = parse(&argv(&[
            "capture",
            "--hwnd",
            "1",
            "--hwnd",
            "2",
            "--out",
            "o",
            "--fps",
            "30",
            "--seconds",
            "1",
        ]))
        .unwrap_err();
        assert!(dup.message.contains("more than once"));
        let unknown = parse(&argv(&["capture", "--audio", "on"])).unwrap_err();
        assert!(unknown.message.contains("unknown flag"));
    }

    #[test]
    fn rejects_flag_without_value_and_unknown_subcommand() {
        let no_value = parse(&argv(&["capture", "--hwnd"])).unwrap_err();
        assert!(no_value.message.contains("needs a value"));
        let subcommand = parse(&argv(&["record"])).unwrap_err();
        assert!(subcommand.message.contains("unknown subcommand"));
        assert!(parse(&[]).is_err());
    }
}
