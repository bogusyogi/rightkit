// Minimal ambient shape of `@tauri-apps/plugin-dialog` 2.x `open`, used only to type-check dialog.ts.
// The plugin is an optional peer: this repository does not install it, and nothing here is emitted
// into dist or referenced by a published declaration file (the public types live in dialog-types.ts).
declare module '@tauri-apps/plugin-dialog' {
  export interface DialogFilter {
    name: string;
    extensions: string[];
  }
  export interface OpenDialogOptions {
    title?: string;
    filters?: DialogFilter[];
    defaultPath?: string;
    multiple?: boolean;
    directory?: boolean;
    recursive?: boolean;
    canCreateDirectories?: boolean;
    [option: string]: unknown;
  }
  export function open(options?: OpenDialogOptions): Promise<string | string[] | null>;
}
