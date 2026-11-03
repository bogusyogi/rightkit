// CG-A07 — headless one-shot/stdin input normalized before run admission.
//
// `main_runtime_run_input_format.rs` already implements the JSONL
// `stream-json` input channel (`InputFormat::StreamJson`,
// `drive_jsonl_script`, `pick_initial_prompt`, `summarize_script`) for
// `coderight run --input-format stream-json`. What's missing — and what
// this module adds — is the layer *above* that: normalizing whatever a
// headless one-shot invocation actually received (a positional prompt arg,
// piped stdin text, both, or neither) into one admitted prompt string
// before either the plain-text path or the stream-json path ever sees it.
// Today that normalization is implicit and scattered at CLI call sites
// (trim-or-not decisions made ad hoc where `run` reads its args). This
// module makes it one small, pure, testable admission gate.
//
// Not wired into the binary yet — see the lane's `WIRING.md` note for the
// exact call-site swap the orchestrator must make in `main_runtime_server.rs`
// / wherever `coderight run` currently reads its prompt.

use std::io::Read;

pub fn read_cli_image_attachments(paths: &[String]) -> Result<Vec<crate::api::Attachment>, String> {
    const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
    paths
        .iter()
        .map(|path| {
            let source = std::path::Path::new(path);
            let metadata = std::fs::metadata(source)
                .map_err(|error| format!("could not read image {}: {error}", source.display()))?;
            if metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
                return Err(format!(
                    "image {} must contain 1 byte to 20 MiB",
                    source.display()
                ));
            }
            let bytes = std::fs::read(source)
                .map_err(|error| format!("could not read image {}: {error}", source.display()))?;
            // Type comes from the file's bytes, never its extension.
            let mime_type = crate::tui_image_input::validate_image_bytes(&bytes)
                .map_err(|reason| format!("image {}: {reason}", source.display()))?;
            Ok(crate::api::Attachment {
                attachment_type: "image".to_string(),
                data: rightkit_crypto::base64_encode(&bytes),
                mime_type: mime_type.to_string(),
            })
        })
        .collect()
}

/// One malformed/rejected admission outcome. Distinguishes the reasons a
/// one-shot invocation can fail admission so the CLI can print an accurate
/// usage error instead of a generic "no prompt".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAdmissionError {
    /// Neither a positional prompt argument nor any piped stdin content was
    /// supplied.
    NoInput,
    /// Input was supplied but, after normalization, contained nothing but
    /// whitespace.
    BlankAfterTrim,
}

impl std::fmt::Display for InputAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputAdmissionError::NoInput => write!(
                f,
                "no prompt supplied: pass a positional prompt argument or pipe text on stdin"
            ),
            InputAdmissionError::BlankAfterTrim => {
                write!(f, "prompt was blank after trimming whitespace")
            }
        }
    }
}

impl std::error::Error for InputAdmissionError {}

/// Raw, not-yet-normalized inputs a one-shot `coderight run` invocation may
/// carry. `positional_prompt` is whatever the CLI parsed as the prompt
/// argument (may be absent); `stdin_text` is the full content read from
/// stdin when the caller determined stdin was piped (not a TTY) — callers
/// decide TTY-ness themselves since that check is environment-dependent and
/// out of scope for this pure module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawOneShotInput {
    pub positional_prompt: Option<String>,
    pub stdin_text: Option<String>,
}

/// The result of normalizing a [`RawOneShotInput`] into one prompt string
/// ready for run admission, plus which source(s) contributed to it so the
/// caller can log/echo accurately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedInput {
    pub prompt: String,
    pub source: InputSource,
}

/// Which raw input source(s) produced the normalized prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    /// Only the positional argument was present.
    PositionalOnly,
    /// Only piped stdin was present.
    StdinOnly,
    /// Both were present; combined per [`combine_positional_and_stdin`].
    Combined,
}

/// Normalizes a raw one-shot input into an admitted prompt.
///
/// Rules (in order):
/// 1. Encoding: stdin bytes are read as UTF-8 lossily upstream by the
///    caller ([`read_stdin_to_string`]); this function only ever sees
///    already-decoded `String`s, so it performs no byte-level work — its
///    job is trimming, combination, and blank rejection.
/// 2. Trimming: both the positional prompt and stdin text are trimmed of
///    leading/trailing whitespace independently before either is used or
///    combined, so trailing newlines from piped input never leak into the
///    admitted prompt.
/// 3. Combination: when both are present and non-blank after trimming, the
///    stdin text becomes a "piped context" block that is combined with the
///    positional prompt via [`combine_positional_and_stdin`] — the
///    positional prompt stays the operator-facing instruction; stdin
///    content is treated as attached data, not as a second instruction.
/// 4. Empty-input handling: when both are absent or blank after trimming,
///    returns [`InputAdmissionError::NoInput`] (nothing at all supplied) or
///    [`InputAdmissionError::BlankAfterTrim`] (something was supplied but
///    it was pure whitespace) so the CLI can report which happened.
pub fn normalize_one_shot_input(
    raw: RawOneShotInput,
) -> Result<NormalizedInput, InputAdmissionError> {
    let positional = raw
        .positional_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let stdin = raw
        .stdin_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match (positional, stdin) {
        (Some(p), Some(s)) => Ok(NormalizedInput {
            prompt: combine_positional_and_stdin(p, s),
            source: InputSource::Combined,
        }),
        (Some(p), None) => Ok(NormalizedInput {
            prompt: p.to_string(),
            source: InputSource::PositionalOnly,
        }),
        (None, Some(s)) => Ok(NormalizedInput {
            prompt: s.to_string(),
            source: InputSource::StdinOnly,
        }),
        (None, None) => {
            let something_was_supplied = raw
                .positional_prompt
                .as_deref()
                .is_some_and(|s| !s.is_empty())
                || raw.stdin_text.as_deref().is_some_and(|s| !s.is_empty());
            if something_was_supplied {
                Err(InputAdmissionError::BlankAfterTrim)
            } else {
                Err(InputAdmissionError::NoInput)
            }
        }
    }
}

/// Combines an explicit positional prompt with piped stdin content. The
/// positional prompt is kept as the leading instruction; the stdin content
/// is appended as a clearly delimited attached-data block so downstream
/// consumers (and any trust-labeling layer, e.g. `project_binding`'s
/// `ContentTrust`) can distinguish "what the operator typed" from "what was
/// piped in" even after the two are joined into one string.
pub fn combine_positional_and_stdin(positional: &str, stdin: &str) -> String {
    format!("{positional}\n\n--- piped stdin ---\n{stdin}")
}

/// Reads all of `reader` to a `String`, decoding as UTF-8 lossily so a
/// stray invalid byte sequence on stdin degrades to `U+FFFD` replacement
/// characters instead of aborting the whole run before admission even runs.
/// Returns `None` on an I/O error (treated the same as "no stdin" by
/// callers, matching today's implicit best-effort behavior).
pub fn read_stdin_to_string<R: Read>(mut reader: R) -> Option<String> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn cli_image_attachments_validate_by_content_not_extension() {
        let dir = std::env::temp_dir().join(format!("cr-cli-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
        std::fs::write(dir.join("ok.dat"), png).unwrap();
        std::fs::write(dir.join("fake.png"), b"definitely not an image").unwrap();
        std::fs::write(dir.join("empty.png"), b"").unwrap();

        let ok = read_cli_image_attachments(&[dir.join("ok.dat").display().to_string()]).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].attachment_type, "image");
        assert_eq!(ok[0].mime_type, "image/png");
        assert!(!ok[0].data.is_empty());

        for bad in ["fake.png", "empty.png", "missing.png"] {
            let error = read_cli_image_attachments(&[dir.join(bad).display().to_string()])
                .err()
                .expect("invalid image must be rejected");
            assert!(error.contains(bad), "{error}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn positional_only_is_trimmed_and_admitted() {
        let raw = RawOneShotInput {
            positional_prompt: Some("  do the thing  ".to_string()),
            stdin_text: None,
        };
        let normalized = normalize_one_shot_input(raw).unwrap();
        assert_eq!(normalized.prompt, "do the thing");
        assert_eq!(normalized.source, InputSource::PositionalOnly);
    }

    #[test]
    fn stdin_only_is_trimmed_and_admitted() {
        let raw = RawOneShotInput {
            positional_prompt: None,
            stdin_text: Some("piped text\n".to_string()),
        };
        let normalized = normalize_one_shot_input(raw).unwrap();
        assert_eq!(normalized.prompt, "piped text");
        assert_eq!(normalized.source, InputSource::StdinOnly);
    }

    #[test]
    fn positional_and_stdin_combine_with_positional_leading() {
        let raw = RawOneShotInput {
            positional_prompt: Some("summarize this".to_string()),
            stdin_text: Some("file contents here".to_string()),
        };
        let normalized = normalize_one_shot_input(raw).unwrap();
        assert!(normalized.prompt.starts_with("summarize this"));
        assert!(normalized.prompt.contains("file contents here"));
        assert_eq!(normalized.source, InputSource::Combined);
    }

    #[test]
    fn neither_present_is_no_input_error() {
        let raw = RawOneShotInput::default();
        assert_eq!(
            normalize_one_shot_input(raw).unwrap_err(),
            InputAdmissionError::NoInput
        );
    }

    #[test]
    fn whitespace_only_positional_is_blank_after_trim_error() {
        let raw = RawOneShotInput {
            positional_prompt: Some("   \n\t  ".to_string()),
            stdin_text: None,
        };
        assert_eq!(
            normalize_one_shot_input(raw).unwrap_err(),
            InputAdmissionError::BlankAfterTrim
        );
    }

    #[test]
    fn whitespace_only_stdin_is_blank_after_trim_error() {
        let raw = RawOneShotInput {
            positional_prompt: None,
            stdin_text: Some("   ".to_string()),
        };
        assert_eq!(
            normalize_one_shot_input(raw).unwrap_err(),
            InputAdmissionError::BlankAfterTrim
        );
    }

    #[test]
    fn empty_stdin_reader_yields_none() {
        let cursor = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_stdin_to_string(cursor), None);
    }

    #[test]
    fn stdin_reader_decodes_utf8_lossily() {
        let cursor = Cursor::new(b"hello world".to_vec());
        assert_eq!(
            read_stdin_to_string(cursor),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn invalid_utf8_on_stdin_degrades_instead_of_failing() {
        let cursor = Cursor::new(vec![0xff, 0xfe, b'a', b'b']);
        let decoded = read_stdin_to_string(cursor).unwrap();
        assert!(decoded.contains("ab"));
    }

    #[test]
    fn display_messages_are_distinct_and_actionable() {
        assert_ne!(
            InputAdmissionError::NoInput.to_string(),
            InputAdmissionError::BlankAfterTrim.to_string()
        );
    }
}
