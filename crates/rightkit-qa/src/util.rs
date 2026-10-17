//! Shared error type and small helpers.
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, Error>;

/// One flat error: every failure in a QA run is a message a human reads.
#[derive(Debug, Clone)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(format!("json: {e}"))
    }
}
impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error(format!("toml: {e}"))
    }
}

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

/// Current UTC time as RFC 3339 (`2026-10-10T12:34:56.789Z`; the fractional part is
/// omitted when zero and has trailing zeros trimmed).
pub fn now_iso() -> String {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_rfc3339_utc(since_epoch.as_secs(), since_epoch.subsec_nanos())
}

fn format_rfc3339_utc(unix_secs: u64, nanos: u32) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    // Civil-from-days (proleptic Gregorian), after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let mut out = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    );
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

/// A random RFC 4122 version-4 identifier in hyphenated lowercase form (36 characters).
pub fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("operating system random source unavailable");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..16)
    )
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Last `max` bytes of `s`, on a char boundary.
pub fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

pub fn require_text(value: &str, name: &str) -> Result<String> {
    let v = value.trim();
    if v.is_empty() {
        return err(format!("{name} is required"));
    }
    Ok(v.to_string())
}

/// `darwin` or `windows`; QA runs on those two desktops only.
pub fn platform() -> Result<&'static str> {
    if cfg!(target_os = "macos") {
        Ok("darwin")
    } else if cfg!(target_os = "windows") {
        Ok("windows")
    } else {
        err(format!(
            "rightkit-qa supports macOS and Windows, received {}",
            std::env::consts::OS
        ))
    }
}

/// Longest ordinary path text this crate hands out: under MAX_PATH (260) with room for a file
/// name the app or test appends.
const PLAIN_PATH_LIMIT: usize = 200;

fn is_reserved_device_name(component: &str) -> bool {
    // Windows reserves the device name with or without an extension (`NUL.txt`).
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// The ordinary form of a Windows verbatim path text: `\\?\C:\dir` becomes `C:\dir` and
/// `\\?\UNC\server\share` becomes `\\server\share`. Text that is not verbatim is returned
/// unchanged. A verbatim path is also returned unchanged when the ordinary form would mean
/// something else or not work: longer than the ordinary limit, a component that is a reserved
/// device name (`CON`, `NUL.txt`, `COM1`), ends in a dot or a space, holds a character Windows
/// rejects (`<>:"|?*`, control characters), or is empty (`a\\b`). Std only; pure text.
pub fn plain_path_text(text: &str) -> String {
    let (plain, rest) = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        (format!(r"\\{unc}"), unc)
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        if !drive {
            return text.to_owned();
        }
        (rest.to_owned(), &rest[3..])
    } else {
        return text.to_owned();
    };
    if plain.chars().count() > PLAIN_PATH_LIMIT {
        return text.to_owned();
    }
    let mut components = rest.split('\\').peekable();
    while let Some(c) = components.next() {
        if c.is_empty() {
            // The drive root itself (`C:\`) has an empty last component; nothing else may.
            if components.peek().is_none() && rest.len() <= 3 {
                continue;
            }
            return text.to_owned();
        }
        if c.ends_with('.') && c != "."
            || c.ends_with(' ')
            || c == "."
            || c == ".."
            || is_reserved_device_name(c)
            || c.chars()
                .any(|ch| ch.is_control() || "<>:\"|?*/".contains(ch))
        {
            return text.to_owned();
        }
    }
    plain
}

/// [`plain_path_text`] for a path: on Windows a verbatim path (what `fs::canonicalize` returns)
/// becomes the ordinary drive path (`C:\...`) when that is safe, else it stays as it is.
/// Every other path, and every path on other systems, is returned unchanged.
pub fn plain_path(path: &std::path::Path) -> std::path::PathBuf {
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    match path.to_str() {
        Some(text) if text.starts_with(r"\\?\") => std::path::PathBuf::from(plain_path_text(text)),
        _ => path.to_path_buf(),
    }
}

/// `fs::canonicalize` with the result handed out as an ordinary path (see [`plain_path`]).
pub fn canonical_plain(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::canonicalize(path).map(|p| plain_path(&p))
}

#[cfg(test)]
mod plain_path_tests {
    use super::*;

    #[test]
    fn drive_and_unc_paths_lose_the_verbatim_prefix() {
        assert_eq!(
            plain_path_text(r"\\?\C:\Users\a\run-1"),
            r"C:\Users\a\run-1"
        );
        assert_eq!(plain_path_text(r"\\?\C:\"), r"C:\");
        assert_eq!(plain_path_text(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(plain_path_text(r"C:\already\plain"), r"C:\already\plain");
        assert_eq!(plain_path_text("/usr/local"), "/usr/local");
    }

    #[test]
    fn unsafe_paths_stay_verbatim() {
        for p in [
            r"\\?\C:\work\nul",
            r"\\?\C:\work\NUL.txt",
            r"\\?\C:\work\com1\x",
            r"\\?\C:\work\LPT9.log",
            r"\\?\C:\work\trailing.",
            r"\\?\C:\work\trailing \x",
            r"\\?\C:\work\a:b",
            r"\\?\C:\work\a?b",
            r"\\?\C:\work\\double",
            r"\\?\C:\work\..\up",
            r"\\?\Volume{1234}\x",
            r"\\?\GLOBALROOT\Device\x",
        ] {
            assert_eq!(plain_path_text(p), p, "{p}");
        }
        // com0, comfort and names that merely start like a device stay plain.
        assert_eq!(plain_path_text(r"\\?\C:\w\com0"), r"C:\w\com0");
        assert_eq!(plain_path_text(r"\\?\C:\w\console"), r"C:\w\console");
    }

    #[test]
    fn a_path_over_the_ordinary_limit_stays_verbatim() {
        let long = format!(r"\\?\C:\{}", "d".repeat(PLAIN_PATH_LIMIT));
        assert_eq!(plain_path_text(&long), long);
        let fits = format!(r"\\?\C:\{}", "d".repeat(PLAIN_PATH_LIMIT - 10));
        assert_eq!(plain_path_text(&fits), fits[4..].to_string());
    }

    #[cfg(windows)]
    #[test]
    fn canonical_plain_hands_out_a_drive_path() {
        let dir = std::env::temp_dir();
        let verbatim = std::fs::canonicalize(&dir).unwrap();
        assert!(verbatim.to_string_lossy().starts_with(r"\\?\"));
        let plain = canonical_plain(&dir).unwrap();
        let text = plain.to_string_lossy();
        assert!(!text.starts_with(r"\\?\"), "{text}");
        assert!(plain.is_dir());
        // It names the same directory.
        assert_eq!(std::fs::canonicalize(&plain).unwrap(), verbatim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(format_rfc3339_utc(0, 0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339_utc(951_782_400, 0), "2000-02-29T00:00:00Z");
        assert_eq!(
            format_rfc3339_utc(1_791_635_696, 789_000_000),
            "2026-10-10T12:34:56.789Z"
        );
        assert_eq!(
            format_rfc3339_utc(86_399, 123_456_789),
            "1970-01-01T23:59:59.123456789Z"
        );
        let now = now_iso();
        assert!(now.ends_with('Z') && now.len() >= 20, "{now}");
    }

    #[test]
    fn new_id_is_a_version_4_uuid() {
        let id = new_id();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
        assert!(id.bytes().all(|c| c == b'-' || c.is_ascii_hexdigit()));
        assert_ne!(id, new_id());
    }
}

/// Decode standard base64 (padding optional, whitespace ignored); `None` on any other byte.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod base64_tests {
    use super::base64_decode;
    #[test]
    fn decodes_rfc4648_vectors_and_rejects_junk() {
        for (i, o) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(base64_decode(i).unwrap(), o.as_bytes());
        }
        assert!(base64_decode("Zm9v!").is_none());
    }
}
