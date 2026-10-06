# rightkit-audio

Shared audio primitives for Right Suite applications. The crate provides typed
FFmpeg filter graphs, loudness measurement and gates, room tone, ducking, WAV
I/O, and optional microphone capture with resampling, downmixing, and recovery.

The default `capture` feature enables `cpal`, `rubato`, and `ringbuf`; disable
it for filter-graph and file-processing consumers. Presets are bundled from
the `presets` directory.

Publish through the RightKit release workflow after platform gates pass.
