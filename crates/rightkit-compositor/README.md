# rightkit-compositor

Native wgpu layer compositor for Right Suite video apps. It renders an evaluated frame (`FrameDescriptor`: layers with transforms, opacity, blend mode, effect passes and masks) onto a canvas texture.

Modules:

- `gpu`: the wgpu context, blit and the shared fullscreen vertex stage.
- `effects`: GPU effect passes (Gaussian blur and uniform-driven passes).
- `masks`: signed-distance-field mask feathering (jump-flood).
- The crate root: the `Compositor`, blend modes (17, CSS names), texture pool and store.

Moved into RightKit from CutRight (`cutright-compositor`, `-gpu`, `-effects`, `-masks`) on 2026-11-01. Adapted from OpenCut Classic (MIT); see `PROVENANCE.md` and `LICENSE-OpenCut`.
