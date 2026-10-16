// Crates renamed in the owner naming batch (2026-10-14). The old names stay in `cargo` as published history that apps
// still pin until they migrate, but the tree has only the new directory and the old name is never staged again.
// Remove an entry here when the last app pin moves and `cargo` drops the old name.
export const RENAMED_AWAY = new Map([
  ["rightkit-shell", "rightkit-tauri-shell"],
  ["rightkit-tauri", "rightkit-device"],
  ["rightkit-control", "rightkit-qa-driver"],
  ["rightkit-jobs", "rightkit-job-queue"],
  ["rightkit-process", "rightkit-child-process"],
  ["rightkit-framed-sidecar", "rightkit-sidecar"],
  ["rightkit-secrets", "rightkit-credentials"],
  ["rightkit-settings", "rightkit-app-settings"],
  ["rightkit-ort", "rightkit-onnx"],
  ["rightkit-media", "rightkit-ffmpeg"],
  ["rightkit-watch", "rightkit-file-watch"],
  ["rightkit-scenario", "rightkit-workflow"],
  ["rightkit-qts", "rightkit-tts-qwen"],
  ["rightkit-qts-ggml-sys", "rightkit-tts-qwen-sys"],
  ["rightkit-qts-ggml", null],
]);

// npm packages renamed in the same batch. Old names stay in rightkit-versions.json (npm/stagedNpm/legacyNpm) as
// published history that apps still pin; the tree has only the successor directory.
export const RENAMED_AWAY_NPM = new Map([
  ["@rightkit/git", "@rightkit/ci"],
  ["@rightkit/updates", "@rightkit/app-update"],
  ["@rightkit/shell", "@rightkit/tauri-shell"],
  ["@rightkit/app-shell", "@rightkit/react-shell"],
]);
