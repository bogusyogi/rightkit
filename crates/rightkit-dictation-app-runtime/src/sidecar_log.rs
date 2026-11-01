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
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "akia",
    "asia",
    "aiza",
    "xoxb-",
    "xoxp-",
    "xoxa-",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_stderr_redacts_sensitive_tokens_and_bounds_length() {
        let line = format!(
            "Authorization=Bearer abc GROQ_API_KEY=secret transcript=hello {}",
            "word ".repeat(200)
        );
        let redacted = redact_sidecar_stderr_line(&line);

        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("hello"));
        assert!(redacted.contains("[redacted"));
        assert!(redacted.ends_with("...[truncated]"));
        assert!(redacted.len() <= SIDECAR_STDERR_LOG_LIMIT + "...[truncated]".len());
    }

    #[test]
    fn sidecar_stderr_redacts_embedded_content_fields_but_keeps_operational_details() {
        let line = concat!(
            "2026-07-17T04:00:00Z INFO command_probe: ",
            r#"{"event":"tail_probe","recognized_text":"Zephyr private quarterly plans","elapsed_ms":42,"error":"model warmup timed out","nested":{"selected_text":"customer contract clauses","prompt":"summarize confidential terms"}}"#,
        );

        let redacted = redact_sidecar_stderr_line(line);

        for private_content in [
            "Zephyr private quarterly plans",
            "customer contract clauses",
            "summarize confidential terms",
        ] {
            assert!(
                !redacted.contains(private_content),
                "leaked: {private_content}"
            );
        }
        assert!(redacted.contains("[redacted:diagnostics]"));
        assert!(redacted.contains(r#""elapsed_ms":42"#));
        assert!(redacted.contains(r#""error":"model warmup timed out""#));
        assert!(redacted.contains("INFO command_probe"));
    }

    #[test]
    fn sidecar_stderr_redacts_secret_shapes_without_labels() {
        let b64 = "Zm9vQmFyOjEyMzQ1Njc4OWFiY2RlZkdISUpLTE1OT1BRUlNU";
        let hex = "0123456789abcdef0123456789abcdef01234567";
        let lines = [
            "calling with sk-proj-AbC123dEf456GhI789jKl".to_string(),
            "key=gsk_abcdefghijklmnop1234 failed".to_string(),
            "header \"AIzaSyA1234567890abcdefghijk\", retry".to_string(),
            format!("blob {b64} end"),
            format!("digest={hex}"),
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678".to_string(),
        ];
        for line in lines {
            let out = redact_sidecar_stderr_line(&line);
            // The app floors write `[redacted]`; the owner's shapes write `[redacted:credential]`.
            assert!(out.contains("[redacted"), "not redacted: {line} -> {out}");
            for leak in ["sk-proj", "gsk_abc", "AIzaSy", b64, hex, "eyJhbG"] {
                assert!(!out.contains(leak), "leaked {leak}: {out}");
            }
        }
        // The owner masks the user-name segment of a home path and keeps the rest (a privacy gain
        // over leaving it readable).
        let keep = "loaded /Users/a/Library/Application/Support/HeardRight/models/parakeet-v3/encoder.int8.onnx in 42ms";
        assert_eq!(
            redact_sidecar_stderr_line(keep),
            "loaded /Users/<user>/Library/Application/Support/HeardRight/models/parakeet-v3/encoder.int8.onnx in 42ms"
        );
        let keep2 = "INFO model=parakeet-tdt-0.6b-v3 sha=abc123 elapsed_ms=42";
        assert_eq!(redact_sidecar_stderr_line(keep2), keep2);
    }

    #[test]
    fn sidecar_stderr_redacts_colon_labelled_secrets() {
        for (line, leak) in [
            (
                "call failed key:sk-abcdefghijklmnop1234 retry",
                "sk-abcdefghijklmnop1234",
            ),
            (
                "call failed token:ghp_abcdefghijklmnop1234567890 retry",
                "ghp_abcdefghijklmnop1234567890",
            ),
            (
                "call failed id:eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678 retry",
                "eyJhbGciOiJIUzI1NiJ9",
            ),
            (
                "call failed ref:AIZASyA1234567890abcdefghijk retry",
                "AIZASyA1234567890abcdefghijk",
            ),
            (
                "call failed k:GHP_abcdefghijklmnop1234567890 retry",
                "GHP_abcdefghijklmnop1234567890",
            ),
            (
                "call failed d:0123456789abcdef0123456789abcdef01234567 retry",
                "0123456789abcdef0123456789abcdef",
            ),
        ] {
            let out = redact_sidecar_stderr_line(line);
            assert!(!out.contains(leak), "leaked {leak}: {out}");
            assert!(out.contains("retry"), "{out}");
        }
    }

    #[test]
    fn sidecar_stderr_redacts_a_secret_outside_embedded_json() {
        let line = concat!(
            "INFO probe sk-abcdefghijklmnop1234 ",
            r#"{"event":"tail_probe","elapsed_ms":42} "#,
            "then key:ghp_abcdefghijklmnop1234567890 and mail ada@example.com"
        );
        let out = redact_sidecar_stderr_line(line);
        for leak in [
            "sk-abcdefghijklmnop1234",
            "ghp_abcdefghijklmnop1234567890",
            "ada@example.com",
        ] {
            assert!(!out.contains(leak), "leaked {leak}: {out}");
        }
        assert!(out.contains(r#""elapsed_ms":42"#), "{out}");
        assert!(out.contains("INFO probe"), "{out}");
    }

    #[test]
    fn sidecar_stderr_floor_prefixes_the_owner_lacks_are_redacted() {
        for secret in [
            "xai-ABCDEFGHIJKLMN",
            "hf_abcdefghijklmnopqrstu",
            "ASIAABCDEFGHIJKLMNOP",
            "pk_live_abcdefghijkl",
        ] {
            let out = redact_sidecar_stderr_line(&format!("using {secret} now"));
            assert!(!out.contains(secret), "leaked {secret}: {out}");
            assert!(out.contains("[redacted"), "{out}");
        }
    }

    #[test]
    fn sidecar_stderr_keeps_the_keyword_floor_for_labelled_content() {
        let out = redact_sidecar_stderr_line("transcript=hello prompt=hi done");
        assert!(
            !out.contains("hello") && !out.contains("prompt=hi"),
            "{out}"
        );
        assert!(out.ends_with("done"));
    }

    #[test]
    fn sidecar_stderr_bounds_multibyte_text_without_panicking() {
        let redacted = redact_sidecar_stderr_line(&"€".repeat(SIDECAR_STDERR_LOG_LIMIT));

        assert!(redacted.ends_with("...[truncated]"));
        assert!(redacted.is_char_boundary(redacted.len()));
    }
}
