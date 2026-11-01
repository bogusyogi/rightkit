import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";

import { logs } from "./logs.js";

/// Restart after an update installs.
///
/// NOT `@tauri-apps/plugin-process`'s `relaunch()`. That calls Tauri's
/// `restart`, which spawns the replacement process before exiting this one —
/// and the overlap makes `tauri-plugin-single-instance` kill the newcomer,
/// leaving the user with no running app at all. The native command sequences
/// it correctly: detached helper waits for this PID to disappear, then
/// relaunches through LaunchServices.
///
/// The plugin call stays only as a fallback for a build without the command
/// registered; it is strictly better than doing nothing if that ever happens.
export async function relaunchForUpdate(): Promise<void> {
  try {
    await invoke("relaunch_for_update");
  } catch (error) {
    logs.info("relaunch_for_update", {
      status: "fallback",
      message: String(error),
    });
    await relaunch();
  }
}
