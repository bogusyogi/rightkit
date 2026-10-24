import { createRightKitTauriClient, standardRightKitDeviceGetOrCreate } from "@rightkit/tauri";
import { invoke } from "./host";
import { logs } from "./logs";

// The device id comes from the host's one atomic command (secure_device_id_get_or_create); the renderer
// never stores it. Treat it as sensitive: it is not shown and not logged.
const native = createRightKitTauriClient<string>({ invoke, deviceGetOrCreate: standardRightKitDeviceGetOrCreate });

export function getDeviceId(): Promise<string> {
  return native.device.getOrCreate();
}

/** Create the install's device id on first launch and log only that it is ready. Never throws. */
export async function initDevice(): Promise<void> {
  try {
    await getDeviceId();
    logs.info("device_ready", { surface: "renderer" });
  } catch {
    logs.error("device_failed", { surface: "renderer" });
  }
}
