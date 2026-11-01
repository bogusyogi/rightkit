import { CLI_PROVIDER_IDS, getProviderDisplay, type CliProviderId } from "./cliProviders";
import type { DaemonCliProvider } from "./daemon/cli-providers";
import type { DriverBindingStatus, DriverKind, ExternalDriverKind } from "./types";
import { hostAppName } from "../appName";

/** One selectable model inside a conductor's nested models submenu. */
export interface ConductorModelOption {
  id: string;
  label: string;
  description?: string;
  isDefault?: boolean;
}

export interface ConductorOption {
  /** External CLI provider id. Native is the worker-model list, not a row. */
  kind: ExternalDriverKind;
  label: string;
  detail: string;
  available: boolean;
  unavailableReason?: string;
  models?: ConductorModelOption[];
  selectedModelId?: string;
}

/** Canonical built-in CLI providers, in engine registry order. */
export const EXTERNAL_CONDUCTORS: CliProviderId[] = CLI_PROVIDER_IDS;

function unavailable(
  kind: CliProviderId,
  detail: string,
  unavailableReason: string,
): ConductorOption {
  return {
    kind,
    label: getProviderDisplay(kind).shortLabel,
    detail,
    available: false,
    unavailableReason,
  };
}

function providerOption(
  kind: CliProviderId,
  provider: DaemonCliProvider | undefined,
  selectedModelIds?: Partial<Record<CliProviderId, string>>,
): ConductorOption {
  const display = getProviderDisplay(kind);
  if (!provider) {
    return unavailable(kind, "Status unavailable", `${display.displayName} status unavailable.`);
  }
  if (!provider.installed) {
    return unavailable(kind, "Not installed", `${display.displayName} is not installed.`);
  }
  if (!provider.auth.launchReady) {
    const detected = provider.auth.detectedSignedIn
      ? "Ambient vendor sign-in detected"
      : "Vendor sign-in not detected";
    const routing = `Run ${display.loginCommand}, then reopen this menu.`;
    return unavailable(
      kind,
      detected,
      `${routing} ${provider.binaryVersion ?? "Installed CLI"}.`,
    );
  }
  if (provider.harness.eligibility !== "ready") {
    return unavailable(
      kind,
      provider.binaryVersion ?? "installed",
      `Harness is ${provider.harness.eligibility.replaceAll("_", " ")}. Complete provider verification before selecting it.`,
    );
  }
  if (!provider.models.available) {
    return unavailable(
      kind,
      provider.binaryVersion ?? "installed",
      "Live model catalog unavailable.",
    );
  }
  if (provider.models.items.length === 0) {
    return unavailable(
      kind,
      provider.binaryVersion ?? "installed",
      "Live model catalog has no selectable models.",
    );
  }
  const models = provider.models.items.map((id, index) => ({
    id,
    label: id,
    isDefault: index === 0,
  }));
  return {
    kind,
    label: display.shortLabel,
    detail: `${provider.binaryVersion ?? "installed"} · ${provider.models.source ?? "live"} catalog`,
    available: true,
    models,
    selectedModelId: selectedModelIds?.[kind] ?? models.find((model) => model.isDefault)?.id,
  };
}

/**
 * Composer conductor rows from one live daemon CLI-provider snapshot.
 *
 * Models are never inferred: an unavailable or empty daemon catalog leaves
 * its provider typed but disabled, preventing a stale local model id from
 * launching a root session.
 */
export function buildConductorOptions(params: {
  providers: DaemonCliProvider[];
  loading: boolean;
  selectedModelIds?: Partial<Record<CliProviderId, string>>;
}): ConductorOption[] {
  if (params.loading) {
    return EXTERNAL_CONDUCTORS.map((kind) =>
      unavailable(kind, "Checking…", "Still checking this CLI."),
    );
  }
  const providers = new Map(params.providers.map((provider) => [provider.providerId, provider]));
  return EXTERNAL_CONDUCTORS.map((kind) =>
    providerOption(kind, providers.get(kind), params.selectedModelIds),
  );
}

function conductorLabel(kind: DriverKind): string {
  return kind === "native" ? `Native ${hostAppName()}` : getProviderDisplay(kind).shortLabel;
}

/** What the composer pill says about its live conductor binding. */
export function conductorPillLabel(
  kind: DriverKind,
  binding: DriverBindingStatus | null,
): string | null {
  if (kind === "native") return null;
  const name = conductorLabel(kind);
  if (!binding) return `${name} · not started`;
  switch (binding.lifecycle) {
    case "ready":
    case "running":
    case "waiting_for_approval":
      return `${name} via ${hostAppName()}`;
    case "starting":
      return `${name} · starting…`;
    case "leased":
      return `${name} · not started`;
    case "interrupted":
      return `${name} · interrupted`;
    case "recovery_required":
      return `${name} · needs recovery`;
    case "completed":
    case "stopped":
      return `${name} · stopped`;
    default:
      return `${name} · ${binding.lifecycle}`;
  }
}
