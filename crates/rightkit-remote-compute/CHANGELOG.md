# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-studio` (genright).
- `models`: the local model catalogue and `studio.models.*` commands, hardware fit, the memory-aware governor for heavy local models (`register_runtime`, `LoadedRuntime`), installs through `rightkit-models` (sd.cpp profiles, QTS), the Blender and Wan2GP doctors and the rented-worker actions, moved from `genright-studio` `model_management/{mod,governor,hardware,install,sdcpp,wan2gp,blender}.rs`; the local-image profile list ships as `data/local-image-profiles.json`. The mapping named `rightkit-models` (EXTEND) for these; they depend on `rightkit-media-gen` and `rightkit-local-gen`, so they live here instead of in the lower-level store crate.
