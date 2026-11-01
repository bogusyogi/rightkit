import { hostAppName } from "../appName";
export const ENABLE_PROVIDER_PRESET_SETUP = true;

export type ProviderPresetModel = {
  id: string;
  label: string;
  description: string;
};

export type ProviderPreset = {
  id: "openrouter" | "minimax" | "tinyfish" | "custom-local";
  label: string;
  kind: "hosted" | "local";
  transport: "openai-compatible";
  env?: string;
  defaultBaseUrl?: string;
  docsUrl: string;
  requiresApiKey: boolean;
  supportsLocalSmoke: boolean;
  required: boolean;
  /** Prefix new hosted keys are expected to carry. Used for instant format feedback only — never a live network check. */
  keyPrefix?: string;
  /** Curated starting models surfaced on the onboarding model-selection step. */
  models?: ProviderPresetModel[];
};

export const HOSTED_PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "openrouter",
    label: "OpenRouter",
    kind: "hosted",
    transport: "openai-compatible",
    env: "OPENROUTER_API_KEY",
    defaultBaseUrl: "https://openrouter.ai/api/v1",
    docsUrl: "https://openrouter.ai/docs",
    requiresApiKey: true,
    supportsLocalSmoke: false,
    required: false,
    keyPrefix: "sk-or-",
    models: [
      {
        id: "anthropic/claude-sonnet-4.5",
        label: "Claude Sonnet 4.5",
        description: "Balanced default — strong reasoning, fast enough for everyday sessions.",
      },
      {
        id: "openai/gpt-5.1",
        label: "GPT-5.1",
        description: "Broad general-purpose coverage across coding and planning tasks.",
      },
      {
        id: "google/gemini-3-pro",
        label: "Gemini 3 Pro",
        description: "Large context window — good for whole-repo review passes.",
      },
      {
        id: "deepseek/deepseek-v4",
        label: "DeepSeek V4",
        description: "Lower-cost option for high-volume or background agent work.",
      },
    ],
  },
  {
    id: "minimax",
    label: "MiniMax",
    kind: "hosted",
    transport: "openai-compatible",
    env: "MINIMAX_API_KEY",
    defaultBaseUrl: "https://api.minimax.io/v1",
    docsUrl: "https://platform.minimax.io/",
    requiresApiKey: true,
    supportsLocalSmoke: false,
    required: false,
    models: [
      {
        id: "minimax/minimax-m3",
        label: "MiniMax M3",
        description:
          `Subscription-backed conductor for ${hostAppName()} sessions and inherited agent roles.`,
      },
    ],
  },
];

export const AUXILIARY_PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "tinyfish",
    label: "TinyFish Search",
    kind: "hosted",
    transport: "openai-compatible",
    env: "TINYFISH_API_KEY",
    docsUrl: "https://agent.tinyfish.ai/",
    requiresApiKey: true,
    supportsLocalSmoke: false,
    required: false,
  },
];

export const SETTINGS_PROVIDER_PRESETS = [
  ...HOSTED_PROVIDER_PRESETS,
  ...AUXILIARY_PROVIDER_PRESETS,
];

export const LOCAL_PROVIDER_PRESET: ProviderPreset = {
  id: "custom-local",
  label: "Custom/local API-compatible",
  kind: "local",
  transport: "openai-compatible",
  docsUrl: "#settings-models",
  requiresApiKey: false,
  supportsLocalSmoke: true,
  required: false,
};

export type KeyFormatCheck = {
  status: "empty" | "valid" | "invalid";
  message: string;
};

/**
 * Instant, offline shape check for a hosted provider key. This is NOT a live network
 * validation (no daemon route exists for that) — it catches empty/whitespace/obviously
 * wrong-prefix/too-short input before the user submits, and is re-run after save so the
 * onboarding UI can show a concrete pass/fail state per provider.
 */
export function checkKeyFormat(preset: ProviderPreset, rawValue: string): KeyFormatCheck {
  const value = rawValue.trim();
  if (!value) {
    return { status: "empty", message: "" };
  }
  if (preset.keyPrefix && !value.startsWith(preset.keyPrefix)) {
    return {
      status: "invalid",
      message: `${preset.label} keys start with "${preset.keyPrefix}"`,
    };
  }
  if (value.length < 20) {
    return { status: "invalid", message: "Key looks too short" };
  }
  if (/\s/.test(value)) {
    return { status: "invalid", message: "Key should not contain spaces" };
  }
  return { status: "valid", message: "Looks valid" };
}
