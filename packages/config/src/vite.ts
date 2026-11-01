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

/** The Vite settings the desktop apps share. Spread the result into `defineConfig`. */
export function rightkitViteConfig(options: RightkitViteOptions): UserConfig {
  const qa = options.qa || undefined;
  return {
    plugins: options.plugins ?? [],
    clearScreen: false,
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
