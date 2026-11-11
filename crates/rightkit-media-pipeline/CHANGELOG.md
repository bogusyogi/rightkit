# Changelog

## 0.2.0

- Requires `rightkit-media-gen` 0.2.0 (breaking: the media-gen types this crate exposes come from the new major). The H.264 rule is `rightkit_ffmpeg::h264_rule` (compose, motion encode, media capabilities); `rightkit_media_gen::host::h264` was a copy and is gone. Same rule, same behaviour.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).

## Unreleased

- Breaking (feature `motion`): requires `rightkit-motion` 0.4.0, whose `Encoder` `motion::encode::pick_encoder` and `pick_alpha_encoder` return. Version 0.2.0. Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.
- The human-eyes gate (`eyes::review_gate`) comes from `rightkit-media-gen-approvals` 0.1.0, where the GenRight approvals moved when `rightkit-approvals` 0.2.0 became the one approvals core (decision 3 of the 2026-11-11 shape review, taken on the recommended option, pending the owner's confirmation). Same code, same behaviour.
- Follows rightkit-motion 0.3.0 (#1144): dependency requirement only; `ProjectServer::start` is unchanged. Its `ChromeSource` can move to `rightkit_motion::browser_source::BrowserFrameSource`.
- Follows rightkit-browser 0.3.9 (#921): the omit-background screenshot literal uses `..Default::default()` because `ScreenshotOptions` gained fields. Capture behaviour unchanged.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-studio` (genright).
- Feature `motion` (off by default): `motion`, the HyperFrames HTML-composition render the resident engine owns (`motion.status`, `motion.render`, `motion.cancel`; loopback `rightkit_motion::project_server`, managed headless Chrome through `rightkit-browser`, encode through `rightkit-motion`; `motion://progress` topic events), and `motion::encode` (ffmpeg resolution, encoder pick and the shared `h264_encoder_unavailable` shape), moved from `genright-engine` `motion_service.rs` and `motion_encode.rs`. GSAP is not bundled: `motion::set_gsap` registers the build to serve.
- `brief`: the engine side of `brief run` (RightKit content-brief in, validated delivery envelope out; keyless local-image stills and carousels only, human-eyes review gate through `rightkit-approvals`, project-local job state in `<project>/.<namespace>/`, caller registration in the user's library), and `caller_tag` (tagging `job run`/`generate` outputs with the named caller), moved from `genright-engine` `brief.rs` and `caller_tag.rs`. The `Engine::Genright` envelope value and the `genright.export-manifest.v1` manifest schema id are kept: they are contract and on-disk ids.
