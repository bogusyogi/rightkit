# Render tools RightKit cutover matrix

Fresh source audit at `0cc72e7ae028bdb4cf53690814ed1e033a1b96e6`; 97/97 catalog rows covered: **Y 0, M 0, N 0, NA 97**.

Scope is tracked Node/Python CAD, Blender, FreeCAD, KeyShot, & FFmpeg command tooling. No Cargo, package, Swift, UI, or service target exists; no production `@rightkit/*` import, Rust FFI, framed-sidecar, or HTTP bridge was found.

`cargo:rightkit-media`/`cargo:rightkit-playback` are NA: `encode.mjs` directly runs PATH `ffmpeg`, while owner APIs are Rust libraries with verified-artifact/typed contracts, & Render has no compatible host or bridge. `cargo:rightkit-compose` is NA: Blender bpy animation/compositor mutations are not its Rust RK9 serializable evaluator. `cargo:rightkit-process`/`cargo:rightkit-sandbox` are NA: one-shot Node/Python child processes do not implement their lifecycle APIs.

No cutover task exists unless Render gains a target-compatible RightKit bridge & matching component feature. No builds, installs, network calls, or product changes were performed.

