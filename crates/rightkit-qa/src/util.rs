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
