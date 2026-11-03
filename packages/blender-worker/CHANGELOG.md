# Changelog

## 0.1.1 (unpublished)

- Removed the unit tests under `worker/tests` (Adrian, 2026-11-03: end-to-end tests only); the worker is proven by real renders.
- Ships the job protocol JSON schemas (`schemas/job`, `result-manifest`, `shot-manifest`, `worker-manifest`), each exported by name, from GenRight `workers/blender/schemas` (GenRight, via the Orchestrator).

## 0.1.0 — unpublished staging

- First RightKit release, moved from GenRight (workers/blender). The assets override variable is now `RIGHTKIT_BLENDER_ASSETS_DIR`.
