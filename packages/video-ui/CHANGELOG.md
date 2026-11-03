# Changelog

## 0.1.1

- Adds the `--caption-clip` surface token (light and dark) for the caption track clip fill. Fixes CutRight #38 (the token was referenced but defined nowhere).
- Removes the unit test (`src/lib/progress.test.ts`), its vitest config and the `test/host-fixture` fixture, and the `test` script. Adrian's rule: e2e only, no fixtures or dummy data.

## 0.1.0

- First release. Video editor UI: timeline, inspector, preview and project views.
