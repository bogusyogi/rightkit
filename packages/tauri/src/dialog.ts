// Production entry of `@rightkit/tauri/dialog`: thin wrappers over `@tauri-apps/plugin-dialog`, an
// optional peer of this package that apps using this entry install. This module has no test hooks
// and no alternative answer source.
import { open } from '@tauri-apps/plugin-dialog';
import { pickFileWith, pickFilesWith, pickFolderWith, type OpenFn } from './dialog-core.js';
import type { PickOptions } from './dialog-types.js';

export type { DialogFilter, DialogPickers, PickOptions } from './dialog-types.js';

const pluginOpen: OpenFn = options => open(options);

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
