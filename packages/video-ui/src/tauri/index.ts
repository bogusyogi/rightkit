// @rightkit/video-ui/tauri: the Tauri `VideoHost` (engine, preview session, media scope and the app part), the
// one place this package imports Tauri. Install it at startup: `installVideoHost(tauriVideoHost())`.
export { shell, tauriVideoHost } from "./tauriVideoHost.js";
