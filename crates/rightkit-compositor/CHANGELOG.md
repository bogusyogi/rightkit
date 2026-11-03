# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.0 (unreleased)

- wgpu 30.0.1 (from 29.0.1; naga 30.0.1 for the shader tests), for CutRight's move to wgpu 30. Breaking for callers, since wgpu's types are part of this crate's API.
- Follows wgpu 30's two breaking changes that touch this crate: `VertexState::buffers` now takes `Option<VertexBufferLayout>` (seven pipelines wrap theirs in `Some`), and `SurfaceTexture::present()` became `Queue::present(surface_texture)` (`Compositor` and `GpuContext` surface presentation). The shaders need no change: their only integers are uniforms, not shader inputs or outputs, so naga's new rule that integer I/O must state `@interpolate(flat)` doesn't apply.

## 0.1.0 (unreleased)

- First RightKit release: CutRight's four compositor crates as one crate (`gpu`, `effects`, `masks` modules).
- CPU-side tests: blend-mode codes and names, the frame descriptor wire shape, every shipped WGSL shader parsed and validated with naga.
