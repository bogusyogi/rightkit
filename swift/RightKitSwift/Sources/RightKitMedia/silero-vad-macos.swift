// RETIRED — do not use, delete this file.
//
// This was CutRight's bundled Silero VAD CoreML worker. It duplicated
// HeardRight's local VAD inference and required a HeardRight-internal model
// path. CutRight now consumes HeardRight's file-VAD capability instead
// (hardening plan §9; see crates/video-providers/src/lib.rs). Nothing compiles
// or references this source any more. Remove it with:
//   rm sidecars/model-worker/silero-vad-macos.swift
