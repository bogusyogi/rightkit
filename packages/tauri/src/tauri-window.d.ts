// Minimal ambient shape of `@tauri-apps/api/window` 2.x `getCurrentWindow`, used only to type-check window.ts.
// `@tauri-apps/api` is an optional peer: this repository does not install it for this package, and nothing here
// is emitted into dist or referenced by a published declaration file (the public types live in window.ts).
declare module '@tauri-apps/api/window' {
  export interface Window {
    minimize(): Promise<void>;
    toggleMaximize(): Promise<void>;
    close(): Promise<void>;
  }
  export function getCurrentWindow(): Window;
}
