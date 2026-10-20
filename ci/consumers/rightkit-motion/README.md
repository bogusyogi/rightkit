# rightkit-motion 0.2.0 registry consumer (draft, PK19)

Standalone consumer, outside the workspace, pinning `rightkit-motion =0.2.0` exactly with the
`encode` feature. It exercises the beat snap, the storyboard sheet plan, the bitrate map, the
encoder pick from a listed ffmpeg inventory, and the ffmpeg argument vector. This is the surface
GenRight's local encoder code (`motion_encode.rs`) would be replaced by.

State: no files, no processes, no network. ffmpeg is never run, so the recipe needs no ffmpeg
asset and no skip marker.

Not covered: the `FrameSource` render driver, PNG decode (`png_to_ppm`, `png_to_pam`), stills,
posters, and the `assemble` feature. The render driver runs ffmpeg and remains under the crate's
asset-gated `encode_e2e` entry in `ci/asset-gated.json`.
