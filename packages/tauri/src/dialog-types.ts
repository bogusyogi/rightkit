// Types shared by both entries of `@rightkit/tauri/dialog`.
// Types only: this file emits no runtime code and imports nothing.

/** One entry of the plugin's `filters` option. */
export interface DialogFilter {
  name: string;
  extensions: string[];
}

/**
 * The options of the dialog plugin's `open()` that an app may set. `directory` and
 * `multiple` are decided by the function you call, never by the options. Options not named here are
 * passed to the plugin unchanged (the plugin's own option type is wider than this one).
 */
export interface PickOptions {
  /** Dialog title (desktop). */
  title?: string;
  filters?: DialogFilter[];
  /** Initial directory or file path. */
  defaultPath?: string;
  /** Folder picks only: the folder will be read recursively later. */
  recursive?: boolean;
  /** macOS: allow creating folders in the dialog. */
  canCreateDirectories?: boolean;
  [pluginOption: string]: unknown;
}

/** The three pick functions every entry of `@rightkit/tauri/dialog` exports. */
export interface DialogPickers {
  /** One folder, or `null` when the person cancels. */
  pickFolder(options?: PickOptions): Promise<string | null>;
  /** One file, or `null` when the person cancels. */
  pickFile(options?: PickOptions): Promise<string | null>;
  /** One or more files, or `null` when the person cancels. */
  pickFiles(options?: PickOptions): Promise<string[] | null>;
}
