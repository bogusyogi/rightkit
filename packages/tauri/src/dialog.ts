// Production entry of `@rightkit/tauri/dialog`: thin wrappers over `@tauri-apps/plugin-dialog`, an
// optional peer of this package that apps using this entry install. This module has no test hooks
// and no alternative answer source.
import { ask as pluginAsk, open, save } from '@tauri-apps/plugin-dialog';
import { askWith, pickFileWith, pickFilesWith, pickFolderWith, pickSavePathWith, type AskFn, type OpenFn, type SaveFn } from './dialog-core.js';
import type { AskOptions, PickOptions, SaveOptions } from './dialog-types.js';

export type { AskOptions, DialogFilter, DialogPickers, PickOptions, SaveOptions } from './dialog-types.js';

const pluginOpen: OpenFn = options => open(options);
const pluginSave: SaveFn = options => save(options);
const pluginAskFn: AskFn = (message, options) => pluginAsk(message, options);

/** Pick one folder. Resolves `null` when the person cancels. Options pass to the plugin unchanged. */
export function pickFolder(options?: PickOptions): Promise<string | null> {
  return pickFolderWith(pluginOpen, options);
}

/** Pick one file. Resolves `null` when the person cancels. */
export function pickFile(options?: PickOptions): Promise<string | null> {
  return pickFileWith(pluginOpen, options);
}

/** Pick one or more files. Resolves `null` when the person cancels. */
export function pickFiles(options?: PickOptions): Promise<string[] | null> {
  return pickFilesWith(pluginOpen, options);
}

/** Choose a path to save to. Resolves `null` when the person cancels. The app writes the file itself. */
export function pickSavePath(options?: SaveOptions): Promise<string | null> {
  return pickSavePathWith(pluginSave, options);
}

/** Ask a yes/no question. Resolves `true` only when the person confirms; cancel or dismiss is `false`. */
export function ask(message: string, options?: AskOptions): Promise<boolean> {
  return askWith(pluginAskFn, message, options);
}
