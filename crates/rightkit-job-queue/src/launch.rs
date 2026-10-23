//! Engine side of [`SupervisorConfig::with_launch_frame`](crate::SupervisorConfig): read the
//! launch frame the supervisor writes as the engine's first stdin line. Store-free: compiled
//! with the `protocol` feature.

use std::fmt;
use std::io::{BufRead, Read};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Why [`read_launch_frame`] did not return params. No variant carries the frame's text: the
/// params may hold secrets, so errors name the failing field only.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LaunchError {
    /// Stdin ended before a whole line (no frame, or a line cut off before its newline).
    Eof,
    /// The line is longer than the limit.
    Oversize,
    /// Reading failed (the `io::ErrorKind`, never data).
    Io(std::io::ErrorKind),
    /// The line is not JSON, or `version` / `params` is missing or does not fit `T`. Names the
    /// field or the JSON error category only.
    Malformed(&'static str),
    /// The first line is not a `{"type":"launch"}` frame.
    WrongType,
    /// The frame is for another schema.
    WrongSchema,
    /// The frame's version is above what this engine reads.
    VersionTooNew {
        /// Version in the frame.
        found: u64,
        /// Newest version the engine accepts.
        max: u32,
    },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::Eof => f.write_str("launch frame: stdin ended before a full line"),
            LaunchError::Oversize => f.write_str("launch frame: line over the limit"),
            LaunchError::Io(kind) => write!(f, "launch frame: read failed ({kind:?})"),
            LaunchError::Malformed(what) => write!(f, "launch frame: malformed ({what})"),
            LaunchError::WrongType => f.write_str("launch frame: first line is not a launch frame"),
            LaunchError::WrongSchema => f.write_str("launch frame: unexpected schema"),
            LaunchError::VersionTooNew { found, max } => {
                write!(f, "launch frame: version {found} is newer than {max}")
            }
        }
    }
}

impl std::error::Error for LaunchError {}

/// Read the launch frame from `reader` (the engine's stdin, before it says hello) and return its
/// `params`. Reads exactly one line, at most `limit` bytes excluding the newline, and nothing
/// after it. The frame must be `{"type":"launch","schema":schema,"version":v,"params":..}` with
/// `v <= max_version`. The supervisor sends at most `MAX_LAUNCH_FRAME_BYTES` (4096).
///
/// Pass the process's shared stdin lock (`std::io::stdin().lock()`), not a `BufReader` you
/// create and drop: a private buffer may read past the frame, and those bytes (the first
/// snapshot or request) would be lost when it is dropped.
pub fn read_launch_frame<T: DeserializeOwned>(
    reader: &mut impl BufRead,
    schema: &str,
    max_version: u32,
    limit: usize,
) -> Result<T, LaunchError> {
    let mut line = Vec::new();
    // One byte past the limit for the newline, one more to detect an overlong line.
    let cap = (limit as u64).saturating_add(2);
    let read = reader
        .by_ref()
        .take(cap)
        .read_until(b'\n', &mut line)
        .map_err(|e| LaunchError::Io(e.kind()))?;
    if read == 0 {
        return Err(LaunchError::Eof);
    }
    if line.last() != Some(&b'\n') {
        return Err(if (line.len() as u64) >= cap {
            LaunchError::Oversize
        } else {
            LaunchError::Eof
        });
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    if line.len() > limit {
        return Err(LaunchError::Oversize);
    }
    let frame: Value = serde_json::from_slice(&line).map_err(|e| {
        LaunchError::Malformed(match e.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        })
    })?;
    if frame.get("type").and_then(Value::as_str) != Some("launch") {
        return Err(LaunchError::WrongType);
    }
    if frame.get("schema").and_then(Value::as_str) != Some(schema) {
        return Err(LaunchError::WrongSchema);
    }
    let version = frame
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(LaunchError::Malformed("version"))?;
    if version > u64::from(max_version) {
        return Err(LaunchError::VersionTooNew {
            found: version,
            max: max_version,
        });
    }
    let params = frame
        .get("params")
        .cloned()
        .ok_or(LaunchError::Malformed("params"))?;
    serde_json::from_value(params).map_err(|_| LaunchError::Malformed("params"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::io::Cursor;

    #[derive(Debug, Deserialize, PartialEq)]
    struct P {
        backend: String,
        flags: Vec<u8>,
    }

    fn read(input: &str, limit: usize) -> Result<P, LaunchError> {
        read_launch_frame(
            &mut Cursor::new(input.as_bytes().to_vec()),
            "hr.engine",
            2,
            limit,
        )
    }
    const GOOD: &str =
        "{\"type\":\"launch\",\"schema\":\"hr.engine\",\"version\":2,\"params\":{\"backend\":\"x\",\"flags\":[1]}}\n";

    #[test]
    fn reads_params_and_only_one_line() {
        let mut r = Cursor::new(format!("{GOOD}{{\"id\":1}}\n").into_bytes());
        let p: P = read_launch_frame(&mut r, "hr.engine", 2, 4096).unwrap();
        assert_eq!(
            p,
            P {
                backend: "x".into(),
                flags: vec![1]
            }
        );
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
        assert_eq!(rest, "{\"id\":1}\n");
        assert!(read(&GOOD.replace('\n', "\r\n"), 4096).is_ok());
    }

    #[test]
    fn typed_errors() {
        assert_eq!(read("", 4096), Err(LaunchError::Eof));
        assert_eq!(read(&GOOD[..20], 4096), Err(LaunchError::Eof));
        assert_eq!(
            read("{\"type\":\"request\"}\n", 4096),
            Err(LaunchError::WrongType)
        );
        assert_eq!(read("[1]\n", 4096), Err(LaunchError::WrongType));
        assert_eq!(
            read(&GOOD.replace("hr.engine", "other"), 4096),
            Err(LaunchError::WrongSchema)
        );
        assert_eq!(
            read(&GOOD.replace("\"version\":2", "\"version\":3"), 4096),
            Err(LaunchError::VersionTooNew { found: 3, max: 2 })
        );
        assert_eq!(
            read(&GOOD.replace("\"version\":2,", ""), 4096),
            Err(LaunchError::Malformed("version"))
        );
        assert_eq!(
            read("not json\n", 4096),
            Err(LaunchError::Malformed("syntax"))
        );
        assert_eq!(
            read(&GOOD.replace("\"flags\":[1]", "\"flags\":\"SECRET\""), 4096),
            Err(LaunchError::Malformed("params"))
        );
        let shown = LaunchError::Malformed("params").to_string();
        assert!(!shown.contains("SECRET"));
    }

    #[test]
    fn limit_is_on_the_line_without_its_newline() {
        let n = GOOD.len() - 1;
        assert!(read(GOOD, n).is_ok());
        assert_eq!(read(GOOD, n - 1), Err(LaunchError::Oversize));
        // No newline at all within the cap is oversize, not EOF.
        let long = "x".repeat(100);
        assert_eq!(read(&long, 10), Err(LaunchError::Oversize));
        assert_eq!(read(&format!("{long}\n"), 10), Err(LaunchError::Oversize));
    }
}
