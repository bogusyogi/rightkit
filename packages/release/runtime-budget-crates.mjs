// The continuous owners that must carry a runtime budget (docs/RUNTIME-BUDGETS.md). Kept free of
// dependencies so the secretless publish plan job can check the runtime proof table against it.
export const RUNTIME_BUDGET_CRATES = Object.freeze([
  "rightkit-fsindex",
  "rightkit-file-watch",
  "rightkit-job-queue",
  "rightkit-daemon",
  "rightkit-cache",
  "rightkit-sync",
  "rightkit-logs",
  "rightkit-tauri-shell",
  "rightkit-qa",
  "rightkit-send",
]);
