import {
  createHeardRightTauriClient,
  createRightKitTauriClient,
  createViewRightTauriClient,
  logsOnlyRightKitTauriCommands,
  rightSuiteTauriAppConfigs,
  standardRightKitDeviceGetOrCreate,
} from '../src/index.js';
import type { RightKitTauriAppConfig } from '../src/index.js';
import type { AskOptions, DialogPickers, PickOptions, SaveOptions } from '../src/dialog-types.js';
import { ask, pickSavePath } from '../src/dialog.js';

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

// 0.4.0: the app owns its config value; the one constructor takes it.
const appConfig = {
  commands: logsOnlyRightKitTauriCommands,
  deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
} satisfies RightKitTauriAppConfig;
const neutral = createRightKitTauriClient<Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
  ...appConfig,
});
const neutralDevice: Promise<Device> = neutral.device.getOrCreate();
// The deprecated per-app tables keep their 0.3.x values.
const heardRightCommands: typeof logsOnlyRightKitTauriCommands = rightSuiteTauriAppConfigs.heardright.commands;
void neutralDevice;
void heardRightCommands;

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

// Save and yes/no dialogs: a save picks a path (cancel is `null`); a question resolves a boolean.
const savedPath: Promise<string | null> = pickSavePath({ defaultPath: 'history.json', filters: [{ name: 'JSON', extensions: ['json'] }] });
const saveOptions: SaveOptions = { title: 'Export', canCreateDirectories: true, somePluginOption: 1 };
const confirmed: Promise<boolean> = ask('Rotate the key?', { title: 'Rotate', kind: 'warning', okLabel: 'Rotate' });
const askOptions: AskOptions = { cancelLabel: 'Keep', kind: 'error' };
// @ts-expect-error A save resolves one path, not a list.
const savedList: Promise<string[] | null> = pickSavePath();
// @ts-expect-error The question comes first; options are second.
ask({ title: 'x' });
// @ts-expect-error The tone is one of three words.
ask('x', { kind: 'danger' });
// @ts-expect-error Titles are strings.
const badSave: SaveOptions = { title: 3 };

void savedPath;
void saveOptions;
void confirmed;
void askOptions;
void savedList;
void badSave;
