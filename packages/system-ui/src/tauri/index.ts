// @rightkit/system-ui/tauri: the Tauri `SystemClient`, `AppsClient` and event subscription, the one place this
// package imports Tauri. Install it once per window, before rendering:
//
//   import { configureSystemHost } from "@rightkit/system-ui/host";
//   import { tauriSystemHost } from "@rightkit/system-ui/tauri";
//   configureSystemHost(tauriSystemHost({ appName: "Your App", storagePrefix: "your-app.hub." }));
//
// The app's src-tauri must serve the commands listed in `tauriSystemClient.ts`.
export {
  tauriAppsClient,
  tauriSystemClient,
  tauriSystemHost,
  tauriSystemSubscribe,
  type ListenFn,
  type TauriSystemOptions,
} from "./tauriSystemClient.js";
