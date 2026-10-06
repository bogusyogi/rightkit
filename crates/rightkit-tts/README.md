# rightkit-tts

Shared local TTS for Right Suite apps. `SystemTts` renders to an isolated WAV
file through macOS `say` + `afconvert` or Windows SAPI; it never plays audio.
Enable `engine` for the Qwen3-TTS QTS worker (rightkit-qts), with `metal` on macOS or
`vulkan` on Windows. QTS model files are selected with
`RIGHTKIT_TTS_MODEL_DIR`; `RIGHTKIT_TTS_WORKER` and `ORT_DYLIB_PATH` select
runtime assets. Tests use `/private/tmp/gre2e-qts` as a read-only fixture root.
`QtsClient` owns worker lifetime and idle unload policy.
