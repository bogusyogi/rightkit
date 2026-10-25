# rightkit-compose 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-compose =0.2.0` exactly. The crate has
no features, so the dependency uses the default (empty) set. It also depends on `serde_json = "1"` to
write composition documents and to check the wire form of `SignedTicks`, `Opacity` and `Easing`.
The crate is pure Rust with no platform-specific code, so the same recipe runs on Windows and macOS
in hosted qualification.

What it proves, in plain sentences, through the public API only:

- The constants `SCHEMA`, `LEGACY_SCHEMA_V1`, `FRAME_SCHEMA`, `FRAME_SCHEMA_V3`, `TICKS_PER_SECOND`,
  `TICKS_PER_MS` and `CAMERA_BOUNCE_MAX`.
- Time: `Ticks` (`from_ms` including saturation, `from_seconds_rational`, `from_frames`,
  `from_frames_rounded`, the float readers, `checked_add`, `checked_sub`, `saturating_add`, `Display`),
  `Rational` (`frame_ticks` for the whole-tick NTSC rate and for 11 fps where it has none, `as_f64`,
  `frame_to_ticks`, `ticks_to_frame`, `frame_at_ticks`, invalid rates returning `None`), and
  `SignedTicks` (conversions, `MIN` and `MAX`, `is_wire_representable`, JSON bounds in both
  directions).
- Tracks: `sample` (hold at both ends, empty track, `Hold` keys), every `Easing` family
  (`Linear`, `EaseIn`, `EaseOut`, `EaseInOut`, `Cosine`, `Hold`, `CubicBezier`, `Spring`,
  `CameraSpring` with its pinned endpoints, published sample values and bounce clamp), and `Lerp` for
  `f64`, `Rect` and `Transform`.
- Retime: `Play`, `Hold`, `Reverse`, `FramePlay`, `SignedHold`, `RetimeSegment::constant`,
  `output_duration`, `output_span`, `source_time_at` and `source_time_at_signed`, including segment
  chaining, exclusive ends and the unsigned reader refusing negative pre-roll.
- Opacity: `Opacity::new`, `clamped`, `constant_track`, serde validation, both `OpacityError`
  variants, and `FrameFade::opacity_at` and `opacity_track` with both `FrameFadeError` variants.
- A full eight-layer `Composition` (background, video with keyframed transform, clip, shadow and
  motion blur, webcam, cursor, badge, caption, ripple and highlight click effects) through
  `validate`, `evaluate` and `evaluate_ms`: stack order, pixel-space geometry, source time, cursor
  tip and visibility, click animation progress and layer end exclusivity.
- Documents: the JSON write, `Composition::from_json_str` reopen, and the edit-then-reopen check that
  only the edited keyframe's frames change; the `rightkit.composition.v1` migration (exact tick
  conversion, speed to retime segment) and its `Legacy` refusal; the signed-source `evaluate_v3` and
  `evaluate_ms_v3` frame against the v2 `Invalid` refusal for negative pre-roll.
- Each `CompositionError` variant by exact value or variant: `Schema`, `Canvas`, `FrameRate`,
  `Duration`, `LayerId`, `Invalid`, `OutOfRange`, `Json`, `Legacy`.

What it deliberately does not prove: any renderer, compositor or encoder (the crate has none; the
frame description is the contract), real media assets (asset ids, sizes and hashes are fixtures), and
the CutRight consumers of the frame description. Transcendental easing values are compared with a
1e-6 tolerance so the recipe does not depend on a platform math library.

State: in-memory values only. No network, no files, no process spawn, no user data. Runs in well
under a second. Run only by the CI consumer lane.
