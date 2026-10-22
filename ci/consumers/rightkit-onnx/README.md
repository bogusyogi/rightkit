# rightkit-onnx 0.2.7 registry consumer (PK18)

New PK18 publication qualification supplement, reviewed 2026-10-23. Original
PK01–17 did not cover this crate; this recipe adds new evidence now.

Standalone `[workspace]` consumer pins registry `rightkit-onnx =0.2.7` with
default features. It checks public API resolution in caller order, relative-path
refusal, bundle verification & same-size SHA-256 mismatch refusal. KleidiAI
policy checks cover affected hosts with old or unknown versions (refusal, or
forced disable with opt-out), safe versions & unaffected hosts. It also checks
binding opt-out defaults to false & session option defaults/toggles.

Qualification covers standalone public API, resolution, integrity & guard policy.
Synthetic library bytes are never loaded; no environment binding, native runtime,
model download or inference occurs. Existing native qualification retains its
authorized missing-assets treatment; PK18 supplies no native-runtime proof.

CI supplies `RK_CONSUMER_RUN_ROOT` through `freshRunRoot`. Unique private fixture
directory is removed explicitly on success & by `tempfile` on error or panic.
Extra dependencies: `serde_json = "1"`, `tempfile = "3"`.
