// Product name shown in user-facing copy. Hosts call setHostAppName() once at startup.
let current = "App";

export function setHostAppName(name: string): void {
  current = name;
}

export function hostAppName(): string {
  return current;
}

/**
 * Host-owned wording and names the packages would otherwise hard-code. Every default is the neutral
 * value the packages have always shipped; a host overrides only what it needs, once at startup, through
 * `setHostOptions()`. The other agent-ui packages re-export this module from core, so one call configures
 * all of them.
 */
export interface HostOptions {
  /** Settings > Appearance label of the default (`:root`) palette, and the fallback label. */
  defaultPaletteLabel: string;
  /** Attention panel summary text when quiet hours are off and durable history is kept. */
  attentionHistoryLabel: string;
  /** Pre-filled environment-variable name for a new webhook trigger's secret binding. */
  webhookSecretEnv: string;
  /** Heading of an assistant turn in an exported session transcript. */
  assistantExportLabel: string;
  /** Render the collapsible "Session options" panel (effort, thinking budget, execution backend, tool lists) in the sidebar. */
  newSessionOptionsPanel: boolean;
  /** Name of the operator sandbox-tier environment variable shown in the sandbox tier panel. */
  sandboxTierEnv: string;
}

const DEFAULT_HOST_OPTIONS: Readonly<HostOptions> = {
  defaultPaletteLabel: "Default",
  attentionHistoryLabel: "history preserved",
  webhookSecretEnv: "RIGHTKIT_WEBHOOK_SECRET",
  assistantExportLabel: "Assistant",
  newSessionOptionsPanel: false,
  sandboxTierEnv: "RIGHTKIT_SANDBOX_TIER",
};

let options: HostOptions = { ...DEFAULT_HOST_OPTIONS };

export function setHostOptions(next: Partial<HostOptions>): void {
  const merged: Record<string, unknown> = { ...options };
  for (const key of Object.keys(DEFAULT_HOST_OPTIONS) as (keyof HostOptions)[]) {
    const value = next[key];
    if (typeof value === typeof DEFAULT_HOST_OPTIONS[key]) merged[key] = value;
  }
  options = merged as unknown as HostOptions;
}

export function hostOption<K extends keyof HostOptions>(key: K): HostOptions[K] {
  return options[key];
}
