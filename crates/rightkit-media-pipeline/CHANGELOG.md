# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-studio` (genright).
- Feature `motion` (off by default): `motion`, the HyperFrames HTML-composition render the resident engine owns (`motion.status`, `motion.render`, `motion.cancel`; loopback `rightkit_motion::project_server`, managed headless Chrome through `rightkit-browser`, encode through `rightkit-motion`; `motion://progress` topic events), and `motion::encode` (ffmpeg resolution, encoder pick and the shared `h264_encoder_unavailable` shape), moved from `genright-engine` `motion_service.rs` and `motion_encode.rs`. GSAP is not bundled: `motion::set_gsap` registers the build to serve.
