const SIDECAR_STDERR_LOG_LIMIT: usize = 512;

pub(crate) fn redact_sidecar_stderr_line(line: &str) -> String {
    // The embedded diagnostic JSON gets its field policy and the owner's value scrub; the text
    // around it (and a line with no JSON) gets the app floors and the owner's text scrub, so a
    // secret outside the JSON is never written.
    if let Some((before, json, after)) = redact_embedded_diagnostic_json(line) {
        return bound_sidecar_stderr(format!(
            "{}{}{}",
            floor_and_scrub_text(before),
            json,
            floor_and_scrub_text(after)
        ));
    }
    bound_sidecar_stderr(floor_and_scrub_text(line))
}

/// The app's keyword and prefix floors first (labelled content, colon-labelled values, the
/// prefixes the owner lacks), then the owner's value scrub (credential shapes, emails, IPs,
/// user-path segments); the caller bounds the result, so a credential cut by the bound cannot survive.
fn floor_and_scrub_text(text: &str) -> String {
    let mut floored = String::with_capacity(text.len());
    let mut redact_next = false;
    for token in text.split_inclusive(char::is_whitespace) {
        let (part, next_is_sensitive) = redact_log_token(token, redact_next);
        floored.push_str(&part);
        redact_next = next_is_sensitive;
    }
    rightkit_logs::scrub_text(&floored)
}

fn redact_embedded_diagnostic_json(line: &str) -> Option<(&str, String, &str)> {
    let start = line.find('{')?;
    let end = line.rfind('}')?;
    if end <= start {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_str(&line[start..=end]).ok()?;
    let mut payload = rightkit_dictation_core::redact_diagnostic_event(payload);
    rightkit_logs::scrub_value(&mut payload);
    Some((&line[..start], payload.to_string(), &line[end + 1..]))
}

fn bound_sidecar_stderr(mut line: String) -> String {
    if line.len() > SIDECAR_STDERR_LOG_LIMIT {
        let mut boundary = SIDECAR_STDERR_LOG_LIMIT;
        while !line.is_char_boundary(boundary) {
            boundary -= 1;
        }
        line.truncate(boundary);
        line.push_str("...[truncated]");
    }
    line
}

fn redact_log_token(token: &str, force: bool) -> (String, bool) {
    let trimmed_len = token.trim_end_matches(char::is_whitespace).len();
    let (body, suffix) = token.split_at(trimmed_len);
    let lower = body.to_ascii_lowercase();
    let sensitive = [
        "authorization",
        "bearer",
        "api_key",
        "apikey",
        "token",
        "transcript",
        "prompt",
        "content",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let redact_following =
        lower == "bearer" || lower.ends_with("=bearer") || lower.ends_with(":bearer");
    if force || sensitive {
        (format!("[redacted]{suffix}"), redact_following)
    } else if let Some(shaped) = redact_secret_shape(body) {
        (format!("{shaped}{suffix}"), redact_following)
    } else {
        (token.to_string(), redact_following)
    }
}

/// The app's credential-prefix floor, compared case-insensitively. It overlaps the owner scrubber
/// (`rightkit_logs::scrub_text`) on purpose: the owner splits words only on whitespace, quotes,
/// brackets and `,;=`, not on `:`, so a colon-labelled value (`key:sk-...`) is one word it does not
/// recognise. The app floor looks at the value after the label.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "gsk_",
    "csk-",
    "csk_",
    "xai-",
    concat!("gh", "p_"),
    concat!("gh", "o_"),
    concat!("gh", "s_"),
    "ghu_",
    concat!("gi", "thub_pat_"),
    "akia",
    "asia",
    "aiza",
    concat!("xo", "xb-"),
    concat!("xo", "xp-"),
    concat!("xo", "xa-"),
    "nvapi-",
    "pk_live_",
    "rk_live_",
    "hf_",
    "eyj",
];

/// Replace the secret-shaped value in a whitespace-free token, keeping any
/// `key=` / `key:` label and surrounding quote punctuation.
fn redact_secret_shape(body: &str) -> Option<String> {
    let is_wrap = |c: char| {
        matches!(
            c,
            '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '<' | '>' | '{' | '}'
        )
    };
    let lead_len = body.len() - body.trim_start_matches(is_wrap).len();
    let core_all = body.trim_matches(is_wrap);
    let (label_len, value) = match core_all.rfind(['=', ':']) {
        Some(i) => (i + 1, &core_all[i + 1..]),
        None => (0, core_all),
    };
    let value_trim = value.trim_matches(is_wrap);
    if !looks_like_secret(value_trim) {
        return None;
    }
    let value_off = lead_len + label_len + (value.len() - value.trim_start_matches(is_wrap).len());
    let value_end = value_off + value_trim.len();
    Some(format!(
        "{}[redacted]{}",
        &body[..value_off],
        &body[value_end..]
    ))
}

fn looks_like_secret(v: &str) -> bool {
    if v.len() >= 12
        && {
            let lower = v.to_ascii_lowercase();
            SECRET_PREFIXES.iter().any(|p| lower.starts_with(p))
        }
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.=+/".contains(&b))
    {
        return true;
    }
    // Long hex and base64 blobs after a label are the owner's (`scrub_text`, opaque blobs of 32 or
    // more); asked here because the owner does not split on `:`.
    rightkit_logs::scrub_text(v).contains("[redacted:credential]")
}
