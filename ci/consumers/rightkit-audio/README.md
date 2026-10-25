# rightkit-audio 0.3.1 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-audio =0.3.1` exactly with default
features off. That is the feature set CutRight's engine and project crates, ScrapeRight's speech
crate, GenRight's studio crate and HeardRight's inference crate use: the typed filter graphs,
measurement maths, WAV and room-tone surface, with no cpal. It also depends on `serde_json = "1"` to
read and write the preset and chain JSON. The crate's pure surface has no platform-specific code, so
the same recipe runs on Windows and macOS in hosted qualification.

What it proves, in plain sentences, through the public API only:

- `Filter` and its text: named, positional and quoted options, `render`, `render_chain`, the number
  formatting of `filter::num` (trailing zeros dropped, no negative zero), `filter::quote` with an
  inner quote, and `filter::db_to_linear`.
- `DialogueChain` and its stages (`HighPass`, `Denoise` in all four forms, `Expander`, `Compressor`,
  `DeEsser`, `Limiter`): the exact `-af` text of the bundled podcast chain stage by stage, the gate
  that leads the talking-head dynamics, a chain with only a limiter, the limiter's 0.0625 floor,
  validation of each stage with the range in the error text (including NaN), the JSON defaults for
  omitted fields, the RNNoise model file being required to exist, and its path rendered with forward
  slashes.
- `dialogue_bus_fragment`: the plain fragment, the fragment that mixes a room-tone layer before the
  limiter, the `anull` pass-through when nothing precedes the mix, and refusal of an invalid chain.
- `Ducking` in both forms: the sidechain fragment (and its refusal without a key label),
  `sidechain_split_fragment`, `spans_volume_filter` and the span fragment, with spans merged when
  their ramps would overlap, sorted, and dropped when empty or inverted, and the range checks of
  both forms.
- `Preset`: `builtin_names`, `builtin` for all three presets, a JSON save and reload that is equal,
  and the refusals (unknown name, wrong `schema_version`, malformed JSON, a stage out of range,
  inconsistent room-tone parameters); `RoomTonePolicy` defaults.
- `AudioGate` and `LoudnessGateResult`: the defaults, pass, inclusive tolerances, each failure text,
  and the unmeasured case; `measure::rms_db`, `peak_db` and `CLIP_THRESHOLD`, and the plain
  `LevelMeasurement`, `AStats` and `RoomToneStep` records.
- `wav`: `write_wav_mono_i16` (clamped, scaled by 32767, rounded), `read_wav_mono_f32` for PCM16,
  IEEE float (from `encode_mono_f32_wav`, whose 44-byte header is checked) and multichannel files
  averaged to mono, `read_wav_16k_mono` and its refusals, and missing and malformed files as
  `AudioError::Wav`.
- `align_mic` and `AlignParams`: a positive lag, a negative lag, no lag, a zero search range, and the
  refusals (unrelated audio, a silent camera, short input, a zero rate, parameters out of range).
- Room tone: `RoomToneParams` defaults and validation, `find_room_tone` with VAD spans and with the
  envelope test, the length cap, the shorter fallback window and its `NoRoomTone` refusals,
  `expand_gaps`, and the exact graph text of `room_tone_fill_fragment` for one gap, the loop offset
  of a second gap, and its refusals.
- `room_tone_fill`, `rms` and `room_tone_bed`: output level equal to the tone's RMS plus the gain
  for gaps shorter than, equal to and longer than the tone, no step at loop seams, silence in and
  silence out, a bed that is silent outside its fills, clipped and skipped fills, and refusals.
- The `Display` text of each `AudioError` variant that can be built without FFmpeg.

What it deliberately does not prove: anything that starts FFmpeg (`AudioTools` loudness, `astats`,
renders and `capture_room_tone`, which need an FFmpeg binary), the `capture` feature (cpal capture,
`list_input_devices`, the CoreAudio probe and the Windows endpoint and power lifecycle) and the
`playback` feature (`OutputSession`, `play_once`), because they need an audio device. Those features
are off in this recipe and are covered by the crate's own device-gated tests. Numbers that go through
`pow` or `log10` are compared to a tolerance, or are text whose digits are far from a rounding
boundary, so the result does not depend on a platform math library.

State: a per-process directory under the OS temp directory (a few small WAV files and one placeholder
model file), removed at the end. No network, no process spawn, no audio device, no user data. Runs in
a few seconds. Run only by the CI consumer lane.
