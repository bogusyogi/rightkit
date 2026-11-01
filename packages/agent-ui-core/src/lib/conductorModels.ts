import type { ModelCatalogEntry } from "./types";

export type ConductorRuntimeModel = {
  id: string;
  label: string;
  aliases: string[];
  supportsThinkingLevels: boolean;
  supportsThinkingBudgetTokens: boolean;
  /** Catalog provider that serves this model, when known. */
  provider?: string;
  /** Set when the model cannot be used yet — e.g. its provider has no key. */
  unavailableReason?: string;
};

export const CONDUCTOR_RUNTIME_MODELS: ConductorRuntimeModel[] = [
  {
    id: "z-ai/glm-5.2",
    label: "GLM 5.2",
    aliases: ["z-ai/glm-5.2", "glm-5.2"],
    supportsThinkingLevels: false,
    supportsThinkingBudgetTokens: false,
  },
  {
    id: "moonshotai/kimi-k2.6",
    label: "Kimi K2.6",
    aliases: ["moonshotai/kimi-k2.6", "kimi-k2.6"],
    supportsThinkingLevels: false,
    supportsThinkingBudgetTokens: false,
  },
  {
    id: "minimax/minimax-m3",
    label: "MiniMax M3",
    aliases: ["minimax/minimax-m3", "minimax-m3"],
    supportsThinkingLevels: false,
    supportsThinkingBudgetTokens: false,
  },
];

/**
 * The runtime models the composer may offer.
 *
 * `enabled` alone is not enough to make a model usable: every one of these is
 * served by a provider that needs a key. Filtering on `enabled` only is why
 * GLM and Kimi were offered as if they were connected while their provider had
 * no saved key at all — picking one produced a failure at send time instead of
 * at the moment of choosing. When the caller knows which providers hold a key,
 * a model whose provider does not is still listed (so it is discoverable) but
 * marked unavailable with the provider to configure.
 */
export function conductorRuntimeModelsFromCatalog(
  models: ModelCatalogEntry[],
  providersWithKey?: ReadonlySet<string>,
): ConductorRuntimeModel[] {
  const seen = new Set<string>();
  return models.flatMap((model) => {
    if (!model.enabled) return [];
    const runtime = CONDUCTOR_RUNTIME_MODELS.find(
      (allowed) => allowed.id === model.id || allowed.id === model.modelSlug,
    );
    if (!runtime || seen.has(runtime.id)) return [];
    seen.add(runtime.id);
    const provider = model.provider.trim();
    const unavailableReason =
      providersWithKey && provider && !providersWithKey.has(provider.toLowerCase())
        ? `${provider} has no saved key`
        : undefined;
    return [
      {
        ...runtime,
        label: model.displayName || model.modelSlug || model.id,
        provider,
        unavailableReason,
      },
    ];
  });
}

export function resolveConductorRuntimeModelId(value: string | null | undefined): string {
  const normalized = value?.trim().toLowerCase();
  if (!normalized) return "";
  return (
    CONDUCTOR_RUNTIME_MODELS.find((model) =>
      model.aliases.some((alias) => alias.toLowerCase() === normalized),
    )?.id ??
    value?.trim() ??
    ""
  );
}

export function conductorRuntimeModelLabel(id: string): string {
  const resolved = resolveConductorRuntimeModelId(id);
  return CONDUCTOR_RUNTIME_MODELS.find((model) => model.id === resolved)?.label ?? id;
}

/** A historical session model is displayable only while it remains enabled. */
export function enabledConductorRuntimeModelLabel(
  value: string | null | undefined,
  options: ConductorRuntimeModel[],
): string | null {
  const resolved = resolveConductorRuntimeModelId(value);
  return options.find((option) => option.id === resolved)?.label ?? null;
}

export function conductorSupportsThinkingLevels(id: string | null | undefined): boolean {
  const resolved = resolveConductorRuntimeModelId(id);
  return (
    CONDUCTOR_RUNTIME_MODELS.find((model) => model.id === resolved)?.supportsThinkingLevels ?? false
  );
}
