//! QA-only helpers for private pasteboards, behind the non-default `qa-probes` feature.
//!
//! Donor: ViewRight `commands/clipboard.rs` (`qa_paste_from_board`, `qa_stage_board`, `qa_png`).
//! An app registers these as `rightkit-qa` control commands (never as Tauri commands), so a scenario
//! can stage a private pasteboard and read it back through the same typed path the app uses, without
//! touching what the user copied. Every board here is `Board::Named`; a name in the
//! `Apple CFPasteboard ...` family (the system boards, including the general one) is refused before
//! any pasteboard is opened, and nothing falls back to `Board::General`. Named boards exist on macOS
//! only; elsewhere the read and the stage return the platform reader's error.

use std::path::PathBuf;

use serde_json::Value;

use super::{
    clipboard_temp_dir, named_board_refusal, take_item_from_with, write, Board, ClipboardContent,
    ClipboardOptions, ClipboardPolicy,
};
use crate::broker::PathBroker;

/// A control command handler receives the whole request (`{"name": .., "args": ".."}`), not the
/// bare argument string. Returns the `args` text, or the input unchanged when it is not that shape.
pub fn control_args(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("args").and_then(|a| a.as_str()).map(str::to_owned))
        .unwrap_or_else(|| raw.to_owned())
}

/// The typed paste result for the PRIVATE pasteboard named by the control request, as JSON (the
/// serialisation of [`ClipboardItem`](super::ClipboardItem): `{"kind": "files" | "image" | "text" |
/// "none", ...}`). `raw` is the control request or the bare board name. The temp directory is
/// `clipboard_temp_dir(app_id)`. A system or empty name is refused before any pasteboard is touched.
pub fn paste_from_board(broker: &PathBroker, app_id: &str, raw: &str) -> Result<String, String> {
    let name = control_args(raw);
    if let Some(why) = named_board_refusal(&name) {
        return Err(format!("paste_from_board refused {name:?}: {why}"));
    }
    let item = take_item_from_with(
        &Board::Named(name),
        broker,
        &clipboard_temp_dir(app_id),
        ClipboardPolicy::default(),
        &ClipboardOptions::default(),
    )
    .map_err(|e| e.message().to_string())?;
    serde_json::to_string(&item).map_err(|e| e.to_string())
}

/// What a staging request asks for, after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    pub board: String,
    pub file: Option<String>,
    pub image: bool,
}

/// Validate a staging request: JSON `{"board": "<prefix>...", "file": "/abs/path"?, "image": bool?}`
/// (bare or wrapped in the control request). The board must start with the non-empty `prefix` the app
/// reserves for QA staging boards and must not be a system pasteboard name. Pure.
pub fn plan_stage(raw: &str, prefix: &str) -> Result<StagePlan, String> {
    if prefix.trim().is_empty() {
        return Err("stage_board needs a non-empty QA board prefix".into());
    }
    let args = control_args(raw);
    let v: Value = serde_json::from_str(&args).map_err(|e| e.to_string())?;
    let board = v
        .get("board")
        .and_then(Value::as_str)
        .filter(|n| n.starts_with(prefix))
        .ok_or_else(|| {
            format!("stage_board needs a board name starting with {prefix}; got {args:.200}")
        })?;
    if let Some(why) = named_board_refusal(board) {
        return Err(format!("stage_board refused {board:?}: {why}"));
    }
    Ok(StagePlan {
        board: board.to_owned(),
        file: v.get("file").and_then(Value::as_str).map(str::to_owned),
        image: v.get("image").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Put a file URL and/or the test PNG on a private board. Refuses any board name without `prefix`
/// (so it can never address a system pasteboard) and never touches `Board::General`.
pub fn stage_board(raw: &str, prefix: &str) -> Result<String, String> {
    let plan = plan_stage(raw, prefix)?;
    let mut content = ClipboardContent::default();
    if let Some(file) = &plan.file {
        content.files.push(file.clone());
    }
    if plan.image {
        content.png = Some(test_png());
    }
    write(&Board::Named(plan.board.clone()), &content)?;
    Ok(serde_json::json!({ "staged": plan.board }).to_string())
}

/// The shell's clipboard temp directory for an app, as [`paste_from_board`] uses it.
pub fn temp_dir(app_id: &str) -> PathBuf {
    clipboard_temp_dir(app_id)
}

/// A valid 1x1 RGBA PNG built in code (stored deflate block, computed CRC32 and Adler-32), so no
/// hand-typed checksum can be wrong.
pub fn test_png() -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    // 1x1, 8-bit, colour type 6 (RGBA).
    chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    // One scanline: filter byte 0 then one opaque pixel.
    let raw = [0u8, 0x20, 0x40, 0x80, 0xFF];
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 0x01, raw.len() as u8, 0, !(raw.len() as u8), 0xFF];
    z.extend_from_slice(&raw);
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "rightkit-qa-board-";

    #[test]
    fn control_args_unwraps_the_request_or_passes_the_input_through() {
        assert_eq!(control_args(r#"{"name":"x","args":"board-1"}"#), "board-1");
        assert_eq!(control_args("bare-name"), "bare-name");
        // JSON without a string `args` is passed through unchanged.
        assert_eq!(control_args(r#"{"name":"x"}"#), r#"{"name":"x"}"#);
    }

    #[test]
    fn paste_refuses_system_and_empty_names_before_touching_a_pasteboard() {
        let broker = PathBroker::new();
        for raw in [
            "Apple CFPasteboard general",
            "  apple cfpasteboard find",
            r#"{"name":"paste_from_board","args":"Apple CFPasteboard drag"}"#,
            "",
            "   ",
        ] {
            let err = paste_from_board(&broker, "rk-qa-refusal-unused", raw).unwrap_err();
            assert!(err.contains("refused"), "{raw:?}: {err}");
        }
        assert!(!temp_dir("rk-qa-refusal-unused").exists());
    }

    #[test]
    fn staging_needs_the_prefix_and_refuses_system_names() {
        let ok = plan_stage(
            r#"{"board":"rightkit-qa-board-1","file":"/tmp/a.txt","image":true}"#,
            PREFIX,
        )
        .unwrap();
        assert_eq!(
            ok,
            StagePlan {
                board: "rightkit-qa-board-1".into(),
                file: Some("/tmp/a.txt".into()),
                image: true,
            }
        );
        let wrapped = serde_json::json!({
            "name": "stage_board",
            "args": r#"{"board":"rightkit-qa-board-2"}"#,
        })
        .to_string();
        let wrapped = plan_stage(&wrapped, PREFIX).unwrap();
        assert_eq!(wrapped.board, "rightkit-qa-board-2");
        assert!(!wrapped.image && wrapped.file.is_none());

        assert!(plan_stage(r#"{"board":"other-board"}"#, PREFIX).is_err());
        assert!(plan_stage(r#"{"board":"Apple CFPasteboard general"}"#, PREFIX).is_err());
        // A prefix that is itself a system name cannot be used to reach one.
        assert!(plan_stage(
            r#"{"board":"Apple CFPasteboard general"}"#,
            "Apple CFPasteboard"
        )
        .is_err());
        assert!(plan_stage(r#"{"board":"x"}"#, "").is_err());
        assert!(plan_stage("not json", PREFIX).is_err());
    }

    #[test]
    fn the_test_png_is_well_formed() {
        let png = test_png();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR chunk: length 13, type, 1x1 RGBA 8-bit, and the well-known CRC of that header.
        assert_eq!(&png[8..16], &[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        assert_eq!(&png[29..33], &[0x1F, 0x15, 0xC4, 0x89]);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
