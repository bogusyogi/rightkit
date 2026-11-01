# Dialogue finish & render integration

## Where it goes in the render graph

The render path (cutright-project `render.rs` / cutright-timeline ffmpeg builder) owns the
mix and the final `loudnorm`. Insert the dialogue bus between "dialogue assembled" and
"mixed with music":

```
dialogue (cuts applied) --> [dialogue_bus_fragment] --> dbus --> (music mix) --> loudnorm
music ---------------------> [Ducking::fragment]  ----> ducked -^
```

```rust
let chain = preset.chain;                       // Preset::builtin("talking-head")
// optional room tone (see below): `bed` is the label of a full-length room tone input
let frag = rightkit_audio::bus::dialogue_bus_fragment(&chain, "dlg", Some("bed"), "dbus", "db")?;
// join `frag` into -filter_complex with ';'
```

Fragment: `[dlg] highpass, denoise, acompressor, deesser [db_d]; [db_d][bed] amix(normalize=0) [db_m];
[db_m] alimiter [dbus]` (no `amix` without a room-tone layer). The limiter runs with
`level=0` (alimiter's default auto-level would raise gain) and `latency=1` (no lookahead
delay, so audio stays in sync). The limiter ceiling is sample-peak; the final `loudnorm`
pass still owns integrated loudness and true peak.

## Room tone

1. Capture: `rightkit_audio::AudioTools::capture_room_tone(source, speech_spans_ms, &params)` (or
   `find_room_tone` on decoded samples). Uses VAD/transcript speech spans when given, else an
   envelope test. Picks the quietest stationary window outside speech.
2. Layer: `rightkit_audio::AudioTools::render_room_tone_layer(source, &sample, &chain, &gaps, gain_db, overlap_ms,
   total_ms, out.wav)` writes a full-length mono 48 kHz f32 WAV: room tone under each gap
   (extended `overlap_ms` under neighbouring speech so J/L edges never hit digital silence),
   silence elsewhere. The sample is cut from the source after the chain's high-pass/denoise
   so its residual floor matches the processed dialogue. Add that WAV as an extra ffmpeg
   input and pass its label as `room_tone_layer_in`. For in-graph use, call
   `room_tone_fill_fragment` directly (it needs the conditioned sample on a label).
3. `gaps` are output-timeline spans from the cut engine (removed silences, retake joins,
   J/L overlaps), in ms.

## Music ducking

`Ducking::Sidechain` (`sidechaincompress` keyed on dialogue; use `sidechain_split_fragment`
to make the key copy) or `Ducking::Spans` (smooth `volume` automation from speech spans;
deterministic, follows VAD/transcript). Both are typed and validated; presets pick one.

## Presets

JSON in `rightkit-audio` (`podcast`, `talking-head`, `noisy-room`), embedded in that crate;
`Preset::from_json` loads project-supplied presets. Schema version 1. `gate` is the
loudness/true-peak/clipping gate (`AudioGate::evaluate`), ported from the old audio profile.

## Filters and licences

All filters used exist in the editing ffmpeg (`tests/audio_processing_real.rs` asserts this): highpass,
lowpass, afftdn, anlmdn, arnndn, acompressor, deesser, alimiter, sidechaincompress, volume,
amix, aloop, areverse, concat, adelay, afade, atrim, asetpts, apad, aformat, asplit, ebur128,
astats, anull. `arnndn` is supported by `Denoise::Arnndn { model }` but NO model is shipped:
RNNoise model files have their own licence terms that were not verified offline, and
`afftdn` already gives the needed reduction. Ship a model only through the shared model
store after a licence check.

`deesser` is a built-in ffmpeg filter (frequency is ffmpeg-normalised `0..1`), so no
sidechain-EQ fallback is needed.
