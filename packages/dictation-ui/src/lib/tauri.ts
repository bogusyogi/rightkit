// tauri — the stable import path every window/component uses for IPC: a re-export of
// `./tauriBridge`, so no consumer's import path (`"./lib/tauri"` / `"./tauri"` / ...) changes.
export { invoke, listen } from "./tauriBridge";
