# Changelog

## 0.1.0 - 2026-10-11

Publishable: `publish = false` is removed and the manifest gains `readme`, `keywords` and `categories` (wave 2.8).

Breaking. The crate is unpublished, so the time model changes outright. Legacy `rightkit.composition.v1` documents still read through `Composition::from_json_str`.

### Time domain (RK9a)

- `Ticks` is the integer time type, at `TICKS_PER_SECOND` = 705,600,000 (the CutRight timeline clock). `TICKS_PER_MS` = 705,600 converts exactly.
- `Ticks::from_ms` (saturating), `Ticks::from_seconds_rational(num, den)` and `Ticks::from_frames(n, fps)` (both `Option`, floored), `to_ms_f64`, `to_seconds_f64`, checked and saturating arithmetic.
- `Rational { num, den }` is the frame rate. NTSC rates have whole-tick frames: 30000/1001 is 23,543,520 ticks per frame (`Rational::frame_ticks`).
- Time fields are `Ticks` and serialise as integers under `*_ticks` names: `Timing.start_ticks`/`end_ticks`, `Keyframe.at_ticks`, `CursorSample.at_ticks`, `duration_ticks`, `onset_ticks`, `source_start_ticks`, asset `duration_ticks`.
- `Composition` carries `fps: Rational` and `duration: Ticks` (`duration_ticks`). `Canvas` keeps only width and height.
- `evaluate(composition, t: Ticks)` is the primary API. `evaluate_ms(composition, t_ms)` is a thin wrapper that converts exactly.
- `FrameDescription.t` is `Ticks`. `Payload::Video` and `Payload::Webcam` carry `source: Ticks`. `MotionBlurFrame.offsets_ticks` is `Vec<i64>`, with each offset rounded to the nearest tick from the frame's exact duration.
- `CompositionError::OutOfRange` carries `t` and `duration` as `Ticks`. New `FrameRate`, `Json` and `Legacy` variants.
- Schemas: `rightkit.composition.v2` and `rightkit.frame.v2`.

### Retime (RK9b)

- `Content::Video` drops `source_start_ms` and `speed`. It gains `retime: Vec<RetimeSegment>`.
- `RetimeSegment::Play { source_in, source_out, speed }` and `Reverse { ... }` last `ceil((source_out - source_in) / speed)` output ticks. `Hold { source_at, duration }` freezes one source time.
- `source_time_at(segments, output) -> Option<Ticks>` evaluates the map. `Play` reads `source_in + floor(o * speed)`, `Reverse` reads `source_out - floor(o * speed)`.
- `RetimeSegment::constant(speed)` is an open-ended forward play from source zero, the old constant-speed behaviour without a source offset.
- An empty `retime` is identity (source time equals layer-local time).
- Validation rejects a speed that is zero, negative, NaN or infinite; empty play, reverse or hold segments; and a retime whose output span does not cover the layer. Output ranges are sequential by construction, so overlap cannot be expressed.

### Easing (RK9c)

- `Easing::Cosine`: `0.5 - 0.5 * cos(pi * u)`.
- `Easing::CameraSpring { bounce }`: ScreenRight's camera curve, from `springProgress` in `src/postprocess.mjs`. `bounce` is clamped to `0..=0.3`. Zero is critically damped. Above zero it is underdamped with `zeta = 1 - bounce`, `omega = 6.9 / zeta`. The endpoint is pinned to exactly 1 at `u >= 1`, as the source does.
- Both are span-independent. `Easing::apply_over` now takes a `Ticks` span. `Easing::Spring` is unchanged apart from that.
- Overshoot: bounce above zero overshoots by design, up to about 0.6% at 0.15 and 4.6% at 0.3. "No overshoot for bounce in [0, 0.3]" does not hold for this formula, so the tests assert the envelope `1 + exp(-pi zeta / sqrt(1 - zeta^2))` and monotonicity only at bounce 0.

### Legacy read

- `Composition::from_json_str` reads `rightkit.composition.v1` and converts it to v2. Every `*_ms` field becomes `*_ticks` at 705,600 ticks per millisecond, which is exact. `canvas.frame_rate_num`/`frame_rate_den` move to `fps`.
- A video's `source_start_ms` and `speed` become one retime segment spanning the layer: `Play` from `source_start` for forward speed, and `Reverse` ending at `source_start` for negative speed. A reverse that would read before source zero is refused.
- Direct serde `Deserialize` accepts only v2. Writing always emits v2.
- `serde_json` is now a runtime dependency, used only by `from_json_str`.

### Conformance

- `tests/camera_conformance.rs` checks `Easing::Cosine`, `CameraSpring { bounce: 0 }` and `CameraSpring { bounce: 0.15 }` against `tests/fixtures/camera_conformance.json`, the 101-sample `cameraConformanceVectors()` recorded from ScreenRight b4e2104. Values are rounded to six decimals as the source does and compared within 1e-6.
- These vectors come from ScreenRight's own source. They are not an independent reference, and CutRight's own vector set is still outstanding. Exactness against that set is unverified until it is supplied.
