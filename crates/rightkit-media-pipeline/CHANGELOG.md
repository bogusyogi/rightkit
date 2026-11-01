# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-studio` (genright).
- Feature `motion` (off by default): `motion`, the HyperFrames HTML-composition render the resident engine owns (`motion.status`, `motion.render`, `motion.cancel`; loopback `rightkit_motion::project_server`, managed headless Chrome through `rightkit-browser`, encode through `rightkit-motion`; `motion://progress` topic events), and `motion::encode` (ffmpeg resolution, encoder pick and the shared `h264_encoder_unavailable` shape), moved from `genright-engine` `motion_service.rs` and `motion_encode.rs`. GSAP is not bundled: `motion::set_gsap` registers the build to serve.
- `brief`: the engine side of `brief run` (RightKit content-brief in, validated delivery envelope out; keyless local-image stills and carousels only, human-eyes review gate through `rightkit-approvals`, project-local job state in `<project>/.<namespace>/`, caller registration in the user's library), and `caller_tag` (tagging `job run`/`generate` outputs with the named caller), moved from `genright-engine` `brief.rs` and `caller_tag.rs`. The `Engine::Genright` envelope value and the `genright.export-manifest.v1` manifest schema id are kept: they are contract and on-disk ids.
