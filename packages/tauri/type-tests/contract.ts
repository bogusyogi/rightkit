import {
  createHeardRightTauriClient,
  createRightKitTauriClient,
  createViewRightTauriClient,
  rightSuiteTauriAppConfigs,
} from '../src/index.js';

type License = { key: string };
type Device = { id: string };
type LogBundle = { events: unknown[] };
type LogEvent = { event: string };
type Diagnostics = string;

const client = createRightKitTauriClient<License, Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});

const logs: Promise<LogBundle> = client.logs.collect();
const diagnostics: Promise<Diagnostics> = client.diagnostics.collect();
const viewRight = createViewRightTauriClient<License, Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});
const heardRight = createHeardRightTauriClient<License, Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});
const viewRightLicense: Promise<License | null> = viewRight.license.get();
const heardRightDevice: Promise<Device> = heardRight.device.getOrCreate();
const viewRightLicenseCommand: 'secure_update_license_get' =
  rightSuiteTauriAppConfigs.viewright.commands.license.get;
// @ts-expect-error Native identity has no public renderer get/set command contract.
rightSuiteTauriAppConfigs.viewright.commands.device;

void logs;
void diagnostics;
void viewRightLicense;
void heardRightDevice;
void viewRightLicenseCommand;

createRightKitTauriClient({
  invoke: async <T>() => undefined as T,
  // @ts-expect-error Fallback-taking transitional native commands are not canonical identity.
  deviceGetOrCreate: { command: 'secure_device_id' },
});
