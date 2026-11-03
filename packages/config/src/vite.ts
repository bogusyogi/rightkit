import type { PluginOption, UserConfig } from "vite";

export interface RightkitViteOptions {
  /** Dev-server port. */
  port: number;
  /** Fail instead of sliding to the next port (Tauri dev pins the port). Default true. */
  strictPort?: boolean;
  /** Dev-server host. Default "127.0.0.1". */
  host?: string;
  /** Host plugins, e.g. `[react(), tailwindcss()]`. */
  plugins?: PluginOption[];
  /** Extra watcher ignores on top of the audit/cache/target/dist defaults. */
  watchIgnored?: string[];
  /** Build target. Default "es2022". */
  buildTarget?: string;
  /**
   * Browser-QA mode: pre-bundle only these entries and skip dependency discovery, so the dev
   * server never re-optimises (and reloads) mid-journey.
   */
  qa?: { include: string[] } | false;
}

/** Watcher ignores every desktop app shares: QA output, caches, Rust targets and build output. */
export const DEFAULT_WATCH_IGNORED: readonly string[] = [
  "**/.audit/**",
  "**/.cache/**",
  "**/target/**",
  "**/src-tauri/target/**",
  "**/dist/**",
];

/**
 * The one web QA switch (rightkit#1480): `__RIGHTKIT_QA__`, a boolean Vite `define`. It is `true` only when the development lane
 * produced the bundle (`right-release dev` sets RIGHTKIT_BUILD_MODE=dev in the environment of every command it runs) and `false`
 * otherwise, so a release bundle constant-folds every `if (__RIGHTKIT_QA__)` branch away. QA code inside such a branch carries the
 * fixed string `RIGHTKIT-QA-WEB-COMPILED`; `right-release` refuses a release bundle that contains the string, the define or a retired
 * per-app flag, and checks in the dev lane that the web and native (`qa-native`) halves are produced together. Declare it for
 * TypeScript with `declare const __RIGHTKIT_QA__: boolean;`.
 */
export function rightkitQaDefine(env: Record<string, string | undefined> = process.env): { __RIGHTKIT_QA__: string } {
  return { __RIGHTKIT_QA__: JSON.stringify(env.RIGHTKIT_BUILD_MODE === "dev") };
}

/** The Vite settings the desktop apps share. Spread the result into `defineConfig`. */
export function rightkitViteConfig(options: RightkitViteOptions): UserConfig {
  const qa = options.qa || undefined;
  return {
    plugins: options.plugins ?? [],
    clearScreen: false,
    define: rightkitQaDefine(),
    ...(qa
      ? {
          optimizeDeps: {
            noDiscovery: true,
            include: qa.include,
            entries: [],
            holdUntilCrawlEnd: false,
          },
        }
      : {}),
    build: { target: options.buildTarget ?? "es2022" },
    server: {
      host: options.host ?? "127.0.0.1",
      port: options.port,
      strictPort: options.strictPort ?? true,
      watch: { ignored: [...DEFAULT_WATCH_IGNORED, ...(options.watchIgnored ?? [])] },
    },
  };
}
