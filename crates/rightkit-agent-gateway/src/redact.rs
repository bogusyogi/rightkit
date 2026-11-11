//! Secret-pattern redaction for what a remote tool returns to a caller that may be another
//! company's service. The first redaction for text sent out: a
//! small, hand-written pass over obvious tokens (no regex crate), run on every chat turn
//! `chat_read` returns. It is a net for the common cases, not a guarantee: a secret in a form it
//! does not know passes through, and so does the partial prefix of one that a length cut split.
//!
//! Redacted, each replaced by `[redacted]`:
//! A known key prefix whose run of key characters reaches the end of the text or the `…(truncated)`
//! marker is redacted at any length, so the per-turn cut cannot leave the start of a key.
//! * `sk-` keys (OpenAI, Anthropic: `sk-proj-`, `sk-ant-`): `sk-` and 20 or more key characters;
//! * GitHub tokens: `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` and 30 or more, `github_pat_` and 30 or
//!   more;
//! * Slack tokens: `xoxa-`, `xoxb-`, `xoxp-`, `xoxr-`, `xoxs-` and 10 or more;
//! * AWS access key ids: `AKIA` or `ASIA` and 16 upper-case letters or digits;
//! * Google API keys: `AIza` and 35 key characters;
//! * `Bearer ` and 20 or more token characters;
//! * PEM blocks, from the five-dash BEGIN marker to the end of the matching END marker, or to the end of
//!   the text when it has none;
//! * the value after `password`, `passwd`, `pwd`, `secret`, `token`, `api_key`, `apikey` or
//!   `access_key` followed by `=` (or `:` for the first three words), up to a space, quote,
//!   comma, semicolon or `&`.
//!
//! The leak check ([`find_secret`], [`text_has_secret`], [`assert_clean`]) is the other half:
//! it masks nothing, it reports a string in a response or an audit row that looks like a token
//! (OAuth access and refresh tokens, `Bearer `, client secrets) or equals a secret the caller
//! names, such as a pairing key. A gateway runs it as its release gate: seed fake secrets, run
//! every method, and fail if it finds one in an output or the audit log.

const MASK: &str = "[redacted]";

/// The text with secrets replaced, and how many were.
pub fn redact(text: &str) -> (String, usize) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some((start, end)) = find(bytes, i) {
            out.push_str(&text[i..start]);
            out.push_str(MASK);
            count += 1;
            i = end;
            continue;
        }
        out.push_str(&text[i..]);
        break;
    }
    (out, count)
}

/// The first secret at or after `from`: its start and end byte offsets.
fn find(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    (from..bytes.len())
        .find_map(|at| matches_at(bytes, at).map(|end| (value_start(bytes, at, end), end)))
}

/// Where the redacted part begins: a `key=value` match redacts the value only, so the key stays
/// readable; every other match is redacted whole.
fn value_start(bytes: &[u8], at: usize, end: usize) -> usize {
    match assignment(bytes, at) {
        Some(value_at) if value_at <= end => value_at,
        _ => at,
    }
}

fn matches_at(bytes: &[u8], at: usize) -> Option<usize> {
    let boundary = at == 0 || !is_word(bytes[at - 1]);
    let rest = &bytes[at..];
    if boundary {
        if let Some(end) = prefixed(rest, b"sk-", 20, is_key) {
            return Some(at + end);
        }
        for prefix in [&b"ghp_"[..], b"gho_", b"ghu_", b"ghs_", b"ghr_"] {
            if let Some(end) = prefixed(rest, prefix, 30, is_alnum) {
                return Some(at + end);
            }
        }
        if let Some(end) = prefixed(rest, b"github_pat_", 30, is_key) {
            return Some(at + end);
        }
        for prefix in [&b"xoxa-"[..], b"xoxb-", b"xoxp-", b"xoxr-", b"xoxs-"] {
            if let Some(end) =
                prefixed(rest, prefix, 10, |b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Some(at + end);
            }
        }
        for prefix in [&b"AKIA"[..], b"ASIA"] {
            if let Some(end) = exactly(rest, prefix, 16, |b| {
                b.is_ascii_uppercase() || b.is_ascii_digit()
            }) {
                return Some(at + end);
            }
        }
        if let Some(end) = exactly(rest, b"AIza", 35, is_key) {
            return Some(at + end);
        }
        if let Some(end) = prefixed(rest, b"Bearer ", 20, is_bearer) {
            return Some(at + end);
        }
    }
    if rest.starts_with(concat!("-----", "BEGIN ").as_bytes()) {
        return Some(at + pem_end(rest));
    }
    assignment(bytes, at).map(|value_at| value_at + value_len(&bytes[value_at..]))
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_key(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_bearer(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// The marker a length cut leaves at the end of a turn (`chat::said`, `chat::read`).
const CUT_MARKER: &[u8] = "…(truncated)".as_bytes();

/// Whether a run of key characters that ends at `at` in `body` was cut off by the end of the text
/// or by the cut marker, so the key may continue past what is left.
fn cut_off(body: &[u8], at: usize) -> bool {
    at == body.len() || body[at..].starts_with(CUT_MARKER)
}

/// `prefix` then at least `min` bytes satisfying `ok`; the end of the whole run. A shorter run
/// that the end of the text or the cut marker interrupts counts too: the cut must not leave the
/// start of a key behind.
fn prefixed(rest: &[u8], prefix: &[u8], min: usize, ok: impl Fn(u8) -> bool) -> Option<usize> {
    let body = rest.strip_prefix(prefix)?;
    let run = body.iter().take_while(|b| ok(**b)).count();
    (run >= min || cut_off(body, run)).then_some(prefix.len() + run)
}

/// `prefix` then exactly `len` bytes satisfying `ok`, or a shorter run cut off as for `prefixed`.
fn exactly(rest: &[u8], prefix: &[u8], len: usize, ok: impl Fn(u8) -> bool) -> Option<usize> {
    let body = rest.strip_prefix(prefix)?;
    let run = body.iter().take_while(|b| ok(**b)).count();
    if run >= len {
        Some(prefix.len() + len)
    } else {
        cut_off(body, run).then_some(prefix.len() + run)
    }
}

/// The end of a PEM block that starts at `rest[0]`.
fn pem_end(rest: &[u8]) -> usize {
    const END: &[u8] = concat!("-----", "END ").as_bytes();
    let Some(end_at) = rest.windows(END.len()).position(|w| w == END) else {
        return rest.len();
    };
    // The END line closes with its own `-----`.
    let after = end_at + END.len();
    match rest[after..].windows(5).position(|w| w == b"-----") {
        Some(close) => after + close + 5,
        None => rest.len(),
    }
}

/// When a key word that names a secret starts at `at` (not inside a longer word) and is followed
/// by its separator, the offset of the value.
fn assignment(bytes: &[u8], at: usize) -> Option<usize> {
    // `DB_PASSWORD=` and `my-token=` count; `swordpassword=` (a longer word) does not.
    if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
        return None;
    }
    const WORDS: [(&[u8], bool); 8] = [
        (b"password", true),
        (b"passwd", true),
        (b"pwd", true),
        (b"secret", false),
        (b"token", false),
        (b"api_key", false),
        (b"apikey", false),
        (b"access_key", false),
    ];
    let rest = &bytes[at..];
    for (word, colon_ok) in WORDS {
        if rest.len() < word.len() || !rest[..word.len()].eq_ignore_ascii_case(word) {
            continue;
        }
        let mut k = word.len();
        // A quoted key (`"password": "x"`) and spaces around the separator are allowed.
        while rest.get(k).is_some_and(|b| *b == b'"' || *b == b'\'') {
            k += 1;
        }
        while rest.get(k) == Some(&b' ') {
            k += 1;
        }
        let sep = *rest.get(k)?;
        if !(sep == b'=' || (sep == b':' && colon_ok)) {
            continue;
        }
        k += 1;
        while rest
            .get(k)
            .is_some_and(|b| *b == b' ' || *b == b'"' || *b == b'\'')
        {
            k += 1;
        }
        // An empty value, or the next key, is nothing to hide.
        if value_len(&rest[k..]) == 0 {
            continue;
        }
        return Some(at + k);
    }
    None
}

/// The length of a value: up to a space, quote, comma, semicolon, `&`, or end of line.
fn value_len(rest: &[u8]) -> usize {
    rest.iter()
        .take_while(|b| {
            !matches!(
                **b,
                b' ' | b'"' | b'\'' | b',' | b';' | b'&' | b'\n' | b'\r'
            )
        })
        .count()
}

/// Markers the leak check treats as a token or secret in a response or audit row.
const LEAK_MARKERS: [&str; 5] = ["ya29.", "1//", "Bearer ", "refresh_token", "client_secret"];

/// First leaked string found in `v` (keys and values), if any. `extra` adds exact secrets to
/// look for, such as a pairing key or its hash.
pub fn find_secret(v: &serde_json::Value, extra: &[&str]) -> Option<String> {
    use serde_json::Value;
    let bad = |s: &str| -> bool {
        LEAK_MARKERS.iter().any(|m| s.contains(m))
            || extra.iter().any(|e| !e.is_empty() && s.contains(e))
    };
    match v {
        Value::String(s) => bad(s).then(|| s.clone()),
        Value::Array(a) => a.iter().find_map(|x| find_secret(x, extra)),
        Value::Object(o) => o.iter().find_map(|(k, x)| {
            if bad(k) {
                Some(k.clone())
            } else {
                find_secret(x, extra)
            }
        }),
        _ => None,
    }
}

/// [`find_secret`] for a plain string such as an audit column.
pub fn text_has_secret(s: &str, extra: &[&str]) -> bool {
    find_secret(&serde_json::Value::String(s.to_string()), extra).is_some()
}

/// Panics (without echoing the match) when `v` contains a secret-looking string.
pub fn assert_clean(v: &serde_json::Value, extra: &[&str]) {
    assert!(
        find_secret(v, extra).is_none(),
        "response contains a token or secret-looking string"
    );
}
