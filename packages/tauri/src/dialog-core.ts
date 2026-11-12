import type { AskOptions, PickOptions, SaveOptions } from './dialog-types.js';

/** What the dialog plugin's `open()` accepts and returns, as far as these wrappers use it. */
export type OpenFn = (options: Record<string, unknown>) => Promise<unknown>;
/** The plugin's `save()`: resolves a path, or `null` when the person cancels. */
export type SaveFn = (options: Record<string, unknown>) => Promise<unknown>;
/** The plugin's `ask()`: resolves `true` for the confirm button, `false` otherwise. */
export type AskFn = (message: string, options: Record<string, unknown>) => Promise<unknown>;

function badResult(fn: string, expected: string, got: unknown): never {
  throw new TypeError(`@rightkit/tauri/dialog ${fn}: the dialog plugin returned ${describe(got)}, expected ${expected}`);
}
function describe(value: unknown): string {
  if (Array.isArray(value)) return 'an array';
  return value === undefined ? 'undefined' : typeof value;
}

/** Pick one folder through `open`. `directory` and `multiple` are forced; every other option passes through. */
export async function pickFolderWith(open: OpenFn, options?: PickOptions): Promise<string | null> {
  const result = await open({ ...options, directory: true, multiple: false });
  if (result === null) return null;
  if (typeof result === 'string') return result;
  return badResult('pickFolder', 'a path or null', result);
}

/** Pick one file through `open`. */
export async function pickFileWith(open: OpenFn, options?: PickOptions): Promise<string | null> {
  const result = await open({ ...options, directory: false, multiple: false });
  if (result === null) return null;
  if (typeof result === 'string') return result;
  return badResult('pickFile', 'a path or null', result);
}

/** Pick one or more files through `open`. A lone string (a plugin that ignores `multiple`) is a one-item list. */
export async function pickFilesWith(open: OpenFn, options?: PickOptions): Promise<string[] | null> {
  const result = await open({ ...options, directory: false, multiple: true });
  if (result === null) return null;
  if (typeof result === 'string') return [result];
  if (Array.isArray(result) && result.every(item => typeof item === 'string')) return result as string[];
  return badResult('pickFiles', 'a list of paths or null', result);
}

/** Choose where to save through `save`. The app names the file; the plugin decides nothing else. Resolves `null` on cancel. */
export async function pickSavePathWith(save: SaveFn, options?: SaveOptions): Promise<string | null> {
  const result = await save({ ...options });
  if (result === null) return null;
  if (typeof result === 'string') return result;
  return badResult('pickSavePath', 'a path or null', result);
}

/** Ask a yes/no question through `ask`. Resolves `true` only for the confirm button. */
export async function askWith(ask: AskFn, message: string, options?: AskOptions): Promise<boolean> {
  const result = await ask(message, { ...options });
  if (typeof result === 'boolean') return result;
  return badResult('ask', 'a boolean', result);
}
