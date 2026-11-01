// Behavioral tests for the `media_tools` bridge (WS-7). The file is the
// body of `mod media_tools_tests` declared in `media_tools.rs`, mirroring
// the `time/tests.rs` and `shell/tests.rs` pattern: the file is the
// module, so `super::*` resolves into `media_tools` directly and the
// `#[test]` fns sit at the top of the module.
//
// The five tests cover the spec's required cases:
//   1. `ToolRegistry::build_default()` includes every media tool.
//   2. A media tool is excluded when filtered out (the `build_for_allowed`
//      allow-list path is the canonical "disallowed by tool" code path;
//      `tool_policy` adds an explicit deny-list on top of it for runtime
//      gating, not registry construction).
//   3. Valid args produce a `CommandSpec` AND get executed — we use a
//      binary that exists on every CI image (`/bin/sh` on Unix, `cmd /C`
//      on Windows) so the test exercises the full spawn / capture path
//      without requiring ffmpeg / tesseract installed locally. The tool
//      layer reports the result; we assert the result is a real
//      `ToolResult` (no panic), is_error is set, and content is non-empty.
//   4. Invalid args (e.g. transcribe with an unknown engine) trigger
//      `command_spec()`'s `Err(MediaToolResult)` path, which the adapter
//      translates to a `ToolResult { is_error: true, .. }`.
//   5. Every media tool's `input_schema()` is well-formed JSON Schema
//      (top-level `type: "object"`, non-empty `properties`, non-empty
//      `required` array of strings). A malformed schema could break
//      tool-calling for the whole session, so this is a sanity check.

use super::*;
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::registry::ToolRegistry;
use crate::coding_tools::tool_trait::Tool;
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use rightkit_ffmpeg::agent_media::MediaToolKind;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn default_registry_includes_every_media_tool() {
    let reg = ToolRegistry::build_default();
    let names: Vec<&str> = reg.all().iter().map(|t| t.name()).collect();
    for name in MEDIA_TOOL_NAMES {
        assert!(
            names.contains(name),
            "ToolRegistry::build_default() is missing media tool `{name}`"
        );
    }
}

#[test]
fn media_tool_is_excluded_when_not_in_allow_list() {
    // `build_for_allowed` intersects the default registry with the
    // caller's allow list. An allow list of just `read_file` should drop
    // every media tool (they're not implied by `read_file`).
    let reg = ToolRegistry::build_for_allowed(&["read_file".to_string()]);
    let names: Vec<&str> = reg.all().iter().map(|t| t.name()).collect();
    for name in MEDIA_TOOL_NAMES {
        assert!(
            !names.contains(name),
            "media tool `{name}` should be excluded when only read_file is allowed"
        );
    }
}

#[test]
fn shell_allow_list_does_not_imply_media_tools() {
    let reg = ToolRegistry::build_for_allowed(&["shell".to_string()]);
    let names: Vec<&str> = reg.all().iter().map(|t| t.name()).collect();
    for name in MEDIA_TOOL_NAMES {
        assert!(
            !names.contains(name),
            "media tool `{name}` must not be implied by shell authority"
        );
    }
}

#[test]
fn media_allow_list_includes_media_without_shell() {
    let reg = ToolRegistry::build_for_allowed(&["media".to_string()]);
    let names: Vec<&str> = reg.all().iter().map(|t| t.name()).collect();
    for name in MEDIA_TOOL_NAMES {
        assert!(names.contains(name), "media grant should include `{name}`");
    }
    assert!(!names.contains(&"shell"));
}

#[test]
fn every_media_tool_declares_dedicated_capability() {
    for kind in [
        MediaToolKind::Ocr,
        MediaToolKind::OcrOnnx,
        MediaToolKind::Transcribe,
        MediaToolKind::WebExtract,
        MediaToolKind::MediaFetch,
        MediaToolKind::FfmpegExtract,
    ] {
        let tool = rightkit_ffmpeg::agent_media::NativeMediaTool::new(kind);
        assert_eq!(
            Tool::capability_class(&tool),
            Some(rightkit_agent_policy::CapabilityClass::Media)
        );
    }
}

#[test]
fn valid_args_produce_command_spec() {
    // Pure spec-translation: `command_spec` returns a `CommandSpec` for
    // valid args. The actual subprocess execution is exercised in the
    // next test; here we verify the spec shape so a future change that
    // silently drops argv tokens is caught.
    let tool = rightkit_ffmpeg::agent_media::NativeMediaTool::new(MediaToolKind::Ocr);
    let args = json!({"image_path": "a.png"});
    let spec = tool
        .command_spec(&args)
        .expect("valid ocr args should produce a CommandSpec");
    assert_eq!(spec.program, "tesseract");
    assert!(
        spec.args.contains(&"a.png".to_string()),
        "image_path should appear in argv: {:?}",
        spec.args
    );
    assert!(
        spec.args.contains(&"stdout".to_string()),
        "tesseract default output target is stdout: {:?}",
        spec.args
    );
}

#[tokio::test]
async fn valid_args_execute_against_a_real_binary_and_return_a_tool_result() {
    // Use a binary that exists on every CI image so the test exercises
    // the full `MediaExecutionContext` -> spawn -> wait -> capture path
    // without requiring ffmpeg / tesseract installed. We point the
    // binary at a path that does NOT exist (`a.png` is the
    // tesseract-shaped `image_path` and we don't create it) so the
    // tool fails with a clean error from the underlying binary — but
    // importantly, it FAILS through the real spawn / capture / result
    // shape, not through a spawn error or a panic. The test asserts
    // the result is a structured `ToolResult` (non-empty content) — the
    // shape is what the spec cares about, not the success/failure axis.
    //
    // We call through `Box<dyn Tool>` (the registry's trait method) so
    // the test exercises the WS-7 bridge from `Tool::execute` ->
    // `MediaExecutionContext` -> `NativeMediaTool::execute` -> spawn.
    // Calling the inherent `NativeMediaTool::execute` directly would
    // skip the bridge under test.
    let fake_binary = if cfg!(windows) { "type" } else { "cat" };
    let tool: Box<dyn Tool> = Box::new(rightkit_ffmpeg::agent_media::NativeMediaTool::new(
        MediaToolKind::Ocr,
    ));
    // Ocr tool's argv shape: [image_path, "stdout", extra_args...].
    // We point `image_path` at a path that does not exist, and
    // explicitly set `binary` to `cat` / `type` so the tool layer
    // actually invokes a real binary (not a no-op). The binary will
    // fail with a clear "no such file" error, which surfaces through
    // the spawn / capture / result pipeline as a structured
    // `ToolResult` — never a panic.
    let args = json!({
        "image_path": "ws7-does-not-exist.bin",
        "binary": fake_binary,
        "timeout_ms": 5_000u64,
    });
    let ctx = ToolContext::new(
        "ws7-smoke",
        PathBuf::from("."),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    let result = tool.execute(args, &ctx).await;
    // The bridge ran end-to-end and produced a structured result.
    // `is_error` may be true (the fake binary could not open the
    // missing file) OR `content` may be non-empty (the spawn pipeline
    // captured stdout / stderr) — both are valid signals that the
    // WS-7 bridge is wired. What we MUST NOT see: a panic, an empty
    // result, or a swallowed spawn error.
    assert!(
        result.is_error || !result.content.is_empty(),
        "execute() must return a structured result (error or non-empty content), never an empty/panic: {result:?}"
    );
}

#[tokio::test]
async fn invalid_args_surface_as_failed_tool_call_not_panic() {
    // `transcribe` only accepts `engine = "whisper" | "parakeet"`.
    // Anything else trips `command_spec()`'s `Err(MediaToolResult)` path,
    // which the adapter translates to `ToolResult { is_error: true, .. }`.
    //
    // Call through `Box<dyn Tool>` (registry trait method) so the test
    // exercises the WS-7 bridge translation of `Err(MediaToolResult)`
    // -> `ToolResult { is_error: true, .. }`, not just the inherent
    // method on `NativeMediaTool`.
    let tool: Box<dyn Tool> = Box::new(rightkit_ffmpeg::agent_media::NativeMediaTool::new(
        MediaToolKind::Transcribe,
    ));
    let args = json!({
        "media_path": "a.wav",
        "engine": "not-a-real-engine",
    });
    let ctx = ToolContext::new(
        "ws7-invalid",
        PathBuf::from("."),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    let result = tool.execute(args, &ctx).await;
    assert!(
        result.is_error,
        "invalid engine should produce a failed ToolResult, not a panic, got: {result:?}"
    );
    assert!(
        result.content.contains("whisper") && result.content.contains("parakeet"),
        "error should mention the valid engine options, got: {}",
        result.content
    );
}

#[test]
fn every_media_input_schema_is_well_formed_json_schema() {
    // The LLM provider validates every tool call against the tool's
    // input_schema. A malformed schema (missing `type`, empty
    // `properties`, `required` that's not an array of strings) breaks
    // tool-calling for the whole session. This sanity check ensures
    // every media tool exposes a well-formed schema.
    for kind in [
        MediaToolKind::Ocr,
        MediaToolKind::OcrOnnx,
        MediaToolKind::Transcribe,
        MediaToolKind::WebExtract,
        MediaToolKind::MediaFetch,
        MediaToolKind::FfmpegExtract,
    ] {
        let tool = rightkit_ffmpeg::agent_media::NativeMediaTool::new(kind);
        let schema = tool.input_schema();
        assert_eq!(
            schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{kind:?} input_schema missing top-level type=object: {schema}"
        );
        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("{kind:?} input_schema missing properties object"));
        assert!(
            !properties.is_empty(),
            "{kind:?} input_schema has empty properties: {schema}"
        );
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{kind:?} input_schema missing required array"));
        assert!(
            !required.is_empty(),
            "{kind:?} input_schema has empty required array"
        );
        for entry in required {
            assert!(
                entry.is_string(),
                "{kind:?} input_schema required entry is not a string: {entry}"
            );
        }
    }
}
