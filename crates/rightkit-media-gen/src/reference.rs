//! `reference.save_png` `{png_base64, name}` -> `{path}`: store a UI-made PNG (a painted mask)
//! under `<app data>/references/` so a generate request can name it as a local path.

use crate::{Context, GenError};
use serde_json::{json, Value};
use std::path::Path;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Decoded size cap.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> GenError {
    GenError::new("invalid_input", message)
}

/// Standard base64 (padding optional, ASCII whitespace ignored). No `data:` prefix handling beyond
/// the caller stripping it.
pub fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    let mut t: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '=')
        .collect();
    while t.len() % 4 != 0 {
        t.push('=');
    }
    let out = rightkit_crypto::base64_decode(&t)
        .map_err(|_| "png_base64 is not valid base64".to_string())?;
    if out.len() > MAX_BYTES {
        return Err("png exceeds the 20 MB limit".into());
    }
    Ok(out)
}

/// A safe single-component file name ending in `.png`: letters, digits, `-` and `_` only.
pub fn sanitise_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = base
        .strip_suffix(".png")
        .or_else(|| base.strip_suffix(".PNG"))
        .unwrap_or(base);
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    let clean = clean.trim_matches('_');
    let clean = if clean.is_empty() { "reference" } else { clean };
    format!("{clean}.png")
}

pub fn save_png_in(dir: &Path, input: &Value) -> Result<Value, GenError> {
    let b64 = input["png_base64"]
        .as_str()
        .ok_or_else(|| invalid("png_base64 (string) is required"))?;
    let b64 = b64.strip_prefix("data:image/png;base64,").unwrap_or(b64);
    if b64.len() > MAX_BYTES / 3 * 4 + 8 {
        return Err(invalid("png exceeds the 20 MB limit"));
    }
    let bytes = decode_base64(b64).map_err(invalid)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("png exceeds the 20 MB limit"));
    }
    if bytes.len() < PNG_SIGNATURE.len() || bytes[..8] != PNG_SIGNATURE {
        return Err(invalid("not a PNG (bad signature)"));
    }
    let name = sanitise_name(input["name"].as_str().unwrap_or(""));
    // A fresh prefix per save so two masks with the same name never overwrite each other.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{stamp}-{name}"));
    std::fs::create_dir_all(dir).map_err(|e| GenError::new("io_error", e.to_string()))?;
    rightkit_fs::atomic_write(&path, &bytes)
        .map_err(|e| GenError::new("io_error", e.to_string()))?;
    Ok(json!({ "path": path.to_string_lossy() }))
}

pub fn save_png(ctx: &Context, input: &Value) -> Result<Value, GenError> {
    save_png_in(&ctx.data_dir.join("references"), input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        rightkit_crypto::base64_encode(bytes)
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("gr-ref-{tag}-{}", std::process::id()))
    }

    #[test]
    fn base64_round_trips() {
        for len in 0..9u8 {
            let data: Vec<u8> = (0..len).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(decode_base64(&b64(&data)).unwrap(), data);
        }
        assert!(decode_base64("a$b").is_err());
    }

    #[test]
    fn names_are_sanitised() {
        assert_eq!(sanitise_name("mask.png"), "mask.png");
        assert_eq!(sanitise_name("../../etc/pass wd.png"), "pass_wd.png");
        assert_eq!(sanitise_name("C:\\x\\..\\.."), "reference.png");
        assert_eq!(sanitise_name(""), "reference.png");
    }

    #[test]
    fn saves_a_png_and_refuses_bad_input() {
        let dir = scratch("ok");
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(b"rest");
        let out = save_png_in(&dir, &json!({"png_base64": b64(&png), "name": "../m.png"})).unwrap();
        let path = std::path::PathBuf::from(out["path"].as_str().unwrap());
        assert_eq!(path.parent().unwrap(), dir);
        assert!(path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-m.png"));
        assert_eq!(std::fs::read(&path).unwrap(), png);

        let bad = save_png_in(
            &dir,
            &json!({"png_base64": b64(b"GIF89a-not-png"), "name": "x"}),
        );
        assert_eq!(bad.unwrap_err().code, "invalid_input");
        assert_eq!(
            save_png_in(&dir, &json!({"name": "x"})).unwrap_err().code,
            "invalid_input"
        );

        let mut big = PNG_SIGNATURE.to_vec();
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(
            save_png_in(&dir, &json!({"png_base64": b64(&big), "name": "x"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
