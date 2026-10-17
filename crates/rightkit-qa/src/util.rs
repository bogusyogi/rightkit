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
        // padding optional, whitespace ignored
        assert_eq!(base64_decode("Zm8").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v\nYmFy ").unwrap(), b"foobar");
    }
}

// ---------------------------------------------------------------------------------- secret forms

/// One encoded form a leaked secret could take: `name` is the form, `text` the needle to search for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretForm {
    pub name: &'static str,
    pub text: String,
}

/// Standard base64 of `bytes`, with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    b64(bytes, false, true)
}

/// URL-safe base64 (RFC 4648 section 5) of `bytes`, no padding: the JWT / webhook-signature form.
pub fn base64url_encode(bytes: &[u8]) -> String {
    b64(bytes, true, false)
}

/// Padded base64 of `bytes` in the standard (`+/`) or URL-safe (`-_`) alphabet.
fn b64_padded(bytes: &[u8], url: bool) -> String {
    const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let alpha = if url { URL } else { STD };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(alpha[((n >> 18) & 63) as usize] as char);
        out.push(alpha[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            alpha[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            alpha[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The four spellings of base64 (standard or URL-safe alphabet, padded or not).
fn b64(bytes: &[u8], url: bool, pad: bool) -> String {
    let mut out = b64_padded(bytes, url);
    if !pad {
        out.truncate(out.trim_end_matches('=').len());
    }
    out
}

const STREAM_NAMES: [(&str, &str); 3] = [
    ("base64_at0", "base64url_at0"),
    ("base64_at1", "base64url_at1"),
    ("base64_at2", "base64url_at2"),
];

/// The part of base64(`prefix`-byte-offset `k` ++ `bytes` ++ anything) that depends on `bytes` alone, no
/// padding: the leading chars that mix in the `k` preceding bytes and the trailing char that mixes in the
/// following byte are dropped. `None` when under 4 chars remain (too short to search for).
fn b64_stream(bytes: &[u8], k: usize, url: bool) -> Option<String> {
    let mut buf = vec![0u8; k];
    buf.extend_from_slice(bytes);
    let full = b64(&buf, url, false);
    let lead = [0, 2, 3][k];
    let trail = usize::from(!buf.len().is_multiple_of(3));
    let end = full.len().checked_sub(trail)?;
    let text = full.get(lead..end)?;
    (text.len() >= 4).then(|| text.to_string())
}

/// HMAC-SHA256 of `message` under `key` (RFC 2104), the raw 32-byte tag, for mocks that verify or
/// sign webhook-style payloads. Built on `rightkit_fs::digest::Sha256Hasher`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let mut h = rightkit_fs::digest::Sha256Hasher::new();
        h.update(key);
        k[..32].copy_from_slice(&h.finalize_bytes());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = rightkit_fs::digest::Sha256Hasher::new();
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.update(&ipad);
    inner.update(message);
    let inner_digest = inner.finalize_bytes();
    let mut outer = rightkit_fs::digest::Sha256Hasher::new();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.update(&opad);
    outer.update(&inner_digest);
    outer.finalize_bytes()
}

/// Lowercase hex of [`hmac_sha256`].
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    rightkit_fs::digest::hex(&hmac_sha256(key, message))
}

/// Percent-encode every byte but the RFC 3986 unreserved set. `form` encodes a space as `+` (form
/// encoding); otherwise it is `%20`. `upper` selects the hex-digit case of each escape.
fn pct(text: &str, upper: bool, form: bool) -> String {
    let mut out = String::new();
    for &b in text.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else if form && b == b' ' {
            out.push('+');
        } else if upper {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out
}

/// Every encoded form a leak of `secret` could take, for secret-absence checks. Names and forms:
/// `raw`; `hex_lower`, `hex_upper`; `base64` (standard alphabet, padded and unpadded); `base64url` (URL-safe
/// alphabet, padded and unpadded); `percent_lower`, `percent_upper` (every non-unreserved byte escaped, a
/// space as `%20`); `form_lower`, `form_upper` (as percent, a space as `+`). A form identical to one already
/// listed is omitted. `base64_at0`, `base64_at1`, `base64_at2` (and `base64url_at*`) are the unpadded base64 of the
/// secret as it appears inside a longer base64 stream at byte offset 0, 1 or 2 (mod 3), with the edge chars
/// that depend on neighbouring bytes trimmed (none at offset 0 when the length is a multiple of 3); one shorter
/// than 4 chars is omitted. A form identical to one already
/// listed is omitted, so a secret with no special characters yields no percent or form copy. An empty secret
/// yields nothing. Split or re-wrapped copies are not covered.
pub fn secret_forms(secret: &str) -> Vec<SecretForm> {
    let mut out: Vec<SecretForm> = Vec::new();
    if secret.is_empty() {
        return out;
    }
    let bytes = secret.as_bytes();
    let hex = rightkit_fs::digest::hex(bytes);
    let mut add = |name: &'static str, text: String| {
        if !out.iter().any(|f| f.text == text) {
            out.push(SecretForm { name, text });
        }
    };
    add("raw", secret.to_string());
    add("hex_lower", hex.to_ascii_lowercase());
    add("hex_upper", hex.to_ascii_uppercase());
    add("base64", b64(bytes, false, true));
    add("base64", b64(bytes, false, false));
    add("base64url", b64(bytes, true, true));
    add("base64url", b64(bytes, true, false));
    // Base64 of the secret inside a larger stream: at byte offset 0, 1 or 2 mod 3 the encoder's
    // grouping differs, and the chars that mix in neighbouring bytes cannot be matched.
    for (k, names) in STREAM_NAMES.iter().enumerate() {
        for (url, name) in [(false, names.0), (true, names.1)] {
            if let Some(t) = b64_stream(bytes, k, url) {
                add(name, t);
            }
        }
    }
    add("percent_lower", pct(secret, false, false));
    add("percent_upper", pct(secret, true, false));
    add("form_lower", pct(secret, false, true));
    add("form_upper", pct(secret, true, true));
    out
}

/// Names of the forms of `secret` found in `haystack` (empty means the secret did not leak). Names only, never
/// the secret. A name repeats when several of its spellings match.
pub fn secret_leaks(haystack: &[u8], secret: &str) -> Vec<&'static str> {
    secret_forms(secret)
        .into_iter()
        .filter(|f| {
            haystack
                .windows(f.text.len())
                .any(|w| w == f.text.as_bytes())
        })
        .map(|f| f.name)
        .collect()
}

#[cfg(test)]
mod base64_encode_tests {
    use super::*;

    #[test]
    fn public_encoders_match_rfc4648_and_round_trip() {
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64url_encode(b"fo"), "Zm8");
        assert_eq!(base64url_encode(&[0xfb, 0xff, 0xfe]), "-__-");
        assert_eq!(base64_encode(&[0xfb, 0xff, 0xfe]), "+//+");
        assert_eq!(base64url_encode(b""), "");
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
    }
}

#[cfg(test)]
mod secret_form_tests {
    use super::*;

    fn text(forms: &[SecretForm], name: &str) -> Vec<String> {
        forms
            .iter()
            .filter(|f| f.name == name)
            .map(|f| f.text.clone())
            .collect()
    }

    #[test]
    fn plain_secret_yields_raw_hex_and_base64_only() {
        let f = secret_forms("foobar");
        assert_eq!(text(&f, "raw"), vec!["foobar"]);
        assert_eq!(text(&f, "hex_lower"), vec!["666f6f626172"]);
        assert_eq!(text(&f, "hex_upper"), vec!["666F6F626172"]);
        assert_eq!(text(&f, "base64"), vec!["Zm9vYmFy"]);
        assert!(text(&f, "percent_lower").is_empty() && text(&f, "form_upper").is_empty());
    }

    #[test]
    fn base64_matches_rfc4648_padding_and_url_alphabet() {
        let f = secret_forms("fo");
        assert_eq!(text(&f, "base64"), vec!["Zm8=", "Zm8"]);
        assert!(text(&f, "base64url").is_empty());
        let f = secret_forms("a?>b~");
        assert_eq!(base64_decode(&text(&f, "base64")[0]).unwrap(), b"a?>b~");
        assert!(text(&f, "base64")[0].contains(['+', '/']));
        assert!(text(&f, "base64url")[0].contains(['-', '_']));
        assert!(!text(&f, "base64url")[0].contains(['+', '/']));
        assert_eq!(text(&f, "base64url").len(), 2);
    }

    #[test]
    fn percent_and_form_cases_and_space() {
        let f = secret_forms("a b/c+é");
        assert_eq!(text(&f, "percent_lower"), vec!["a%20b%2fc%2b%c3%a9"]);
        assert_eq!(text(&f, "percent_upper"), vec!["a%20b%2Fc%2B%C3%A9"]);
        assert_eq!(text(&f, "form_lower"), vec!["a+b%2fc%2b%c3%a9"]);
        assert_eq!(text(&f, "form_upper"), vec!["a+b%2Fc%2B%C3%A9"]);
        let f = secret_forms("a/b");
        assert_eq!(text(&f, "percent_upper"), vec!["a%2Fb"]);
        assert!(text(&f, "form_upper").is_empty());
    }

    #[test]
    fn empty_secret_yields_nothing_and_leaks_are_named() {
        assert!(secret_forms("").is_empty());
        let secret = "tok en/1";
        let hay = format!("x={} y", pct(secret, true, true));
        assert_eq!(secret_leaks(hay.as_bytes(), secret), vec!["form_upper"]);
        assert!(secret_leaks(b"clean", secret).is_empty());
        assert_eq!(secret_leaks(b"has tok en/1 inside", secret), vec!["raw"]);
    }

    #[test]
    fn base64_stream_forms_cover_all_three_alignments() {
        // The secret carries sextets 62 and 63 in every alignment, so the url-safe copies differ from the
        // standard copies and survive de-duplication under their `base64url*` names.
        let secret = "sk_live_abc~def>123?";
        let forms = secret_forms(secret);
        for (pre, post) in [
            ("", ""),
            ("A", "Z"),
            ("AB", "YZ"),
            ("ABC", "XYZ"),
            ("x", ""),
            ("xy", "!"),
        ] {
            let mut stream = pre.as_bytes().to_vec();
            stream.extend_from_slice(secret.as_bytes());
            stream.extend_from_slice(post.as_bytes());
            let enc = b64(&stream, false, true);
            assert!(
                forms
                    .iter()
                    .any(|f| enc.contains(&f.text) && f.name.starts_with("base64")),
                "no form found the secret at offset {} in {enc}",
                pre.len()
            );
            let url = b64(&stream, true, false);
            assert!(forms
                .iter()
                .any(|f| f.name.starts_with("base64url") && url.contains(&f.text)));
            assert!(
                !secret_leaks(enc.as_bytes(), secret).is_empty(),
                "offset {}",
                pre.len()
            );
        }
    }

    #[test]
    fn base64_stream_forms_trim_the_edges() {
        // "foobar" (6 bytes): offset 0 is the whole, unpadded form; offset 1 drops 2 leading chars
        // and the last; offset 2 drops 3 leading chars and the last.
        assert_eq!(b64_stream(b"foobar", 0, false).unwrap(), "Zm9vYmFy");
        assert_eq!(
            b64_stream(b"foobar", 1, false).unwrap(),
            b64(b"\0foobar", false, false)[2..9]
        );
        assert_eq!(
            b64_stream(b"foobar", 2, false).unwrap(),
            b64(b"\0\0foobar", false, false)[3..10]
        );
        assert!(b64_stream(b"fo", 1, false).is_none());
        // Offset forms are real, distinct search terms.
        let f = secret_forms("foobar-secret");
        assert!(
            f.iter().any(|x| x.name == "base64_at1") && f.iter().any(|x| x.name == "base64_at2")
        );
    }

    #[test]
    fn hmac_sha256_matches_rfc4231() {
        let long = [0xaau8; 131];
        assert_eq!(
            hmac_sha256_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_sha256_hex(&[0xaa; 20], &[0xdd; 50]),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        assert_eq!(
            hmac_sha256_hex(
                &long,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(hmac_sha256(b"k", b"m").len(), 32);
    }
}
