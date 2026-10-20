import {
  createHeardRightTauriClient,
  createRightKitTauriClient,
  createViewRightTauriClient,
  rightSuiteTauriAppConfigs,
} from '../src/index.js';
import type { DialogPickers, PickOptions } from '../src/dialog-types.js';

type Device = { id: string };
type LogBundle = { events: unknown[] };
type LogEvent = { event: string };
type Diagnostics = string;

const client = createRightKitTauriClient<Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});

const logs: Promise<LogBundle> = client.logs.collect();
const diagnostics: Promise<Diagnostics> = client.diagnostics.collect();
const viewRight = createViewRightTauriClient<Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});
const heardRight = createHeardRightTauriClient<Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});
const heardRightDevice: Promise<Device> = heardRight.device.getOrCreate();
// @ts-expect-error Native identity has no public renderer get/set command contract.
rightSuiteTauriAppConfigs.viewright.commands.device;

void logs;
void diagnostics;
void heardRightDevice;

createRightKitTauriClient({
  invoke: async <T>() => undefined as T,
  // @ts-expect-error Fallback-taking transitional native commands are not canonical identity.
  deviceGetOrCreate: { command: 'secure_device_id' },
});

// @ts-expect-error Licence storage is native; renderer clients expose no raw licence path.
client.license;
// @ts-expect-error App-specific clients also expose no licence storage path.
viewRight.license;
// @ts-expect-error App command maps cannot export raw licence commands.
rightSuiteTauriAppConfigs.viewright.commands.license;

createRightKitTauriClient({
  invoke: async <T>() => undefined as T,
  argumentNames: {
    // @ts-expect-error Raw licence setters have no renderer argument configuration.
    licenseSet: 'value',
  },
});

createRightKitTauriClient({
  invoke: async <T>() => undefined as T,
  commands: {
    logs: { write: 'write', collect: 'collect', clear: 'clear' },
    diagnostics: { collect: 'diagnostics', clear: 'clear-diagnostics' },
    // @ts-expect-error Raw licence command maps are outside this adapter contract.
    license: { get: 'get', set: 'set', delete: 'delete' },
  },
});

// `@rightkit/tauri/dialog` exports three pickers; cancel is `null`.
const pickers: DialogPickers = {
  pickFolder: async () => null,
  pickFile: async () => null,
  pickFiles: async () => null,
};
const folder: Promise<string | null> = pickers.pickFolder({ title: 'Open', filters: [{ name: 'Media', extensions: ['mp4'] }] });
const file: Promise<string | null> = pickers.pickFile();
const files: Promise<string[] | null> = pickers.pickFiles({ defaultPath: '/work' });
const options: PickOptions = { title: 'x', canCreateDirectories: false, somePluginOption: 1 };
// @ts-expect-error Titles are strings.
const badOptions: PickOptions = { title: 3 };

void folder;
void file;
void files;
void options;
void badOptions;
