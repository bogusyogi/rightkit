/**
 * High-level helpers for the desktop UI's multi-provider CLI runtime
 * surface (PRs 1–12 of the architecture plan).
 *
 * These helpers close over the wire schema for `cli_account_profiles`,
 * `cli_harness_attestations`, and `cli_delegation_policy` so the
 * components don't have to re-implement the parsing. They are
 * deliberately small: no caching, no provider-specific dispatch, no
 * retry logic. The components they call already own their own state.
 *
 * Two non-obvious rules from the architecture plan show up here:
 *
 * 1. **No API-key surface.** This file must never grow a `saveApiKey`
 *    helper. `cli_account_profiles` is the only allowed store for
 *    authenticated-CLI provider credentials, and its profiles are
 *    managed by the vendor CLI itself. the host app never reads the
 *    credential bytes.
 *
 * 2. **No Kiro.** `CLI_PROVIDER_IDS` (re-exported from
 *    `lib/types/cliProviders`) is the canonical list. Helpers that
 *    iterate "every provider" must use it directly; tests assert
 *    that no provider array literal anywhere ships `kiro`.
 */
import {
  authenticatedCliDefaultProfileId,
  CLI_PROVIDER_IDS,
  type CliProviderId,
  type CliProviderEligibility,
  type CliProviderCardSnapshot,
  type CliAccountProfile,
  type CliAuthMode,
  type CliTransportKind,
  type HarnessAttestation,
  type HarnessAttestationRow,
  type HarnessIsolationMode,
  type CliDelegationPolicy,
} from "./types/cliProviders";
import type { CliAccountProfileView } from "./daemon/config-host";
import type { DaemonCliProvider } from "./daemon/cli-providers";

export {
  CLI_PROVIDER_IDS,
  authenticatedCliDefaultProfileId,
  type CliProviderId,
  type CliProviderCardSnapshot,
  type CliAccountProfileView,
};

/**
 * Static display data for the six built-in providers. Order matches
 * `BUILT_IN_PROVIDER_IDS` on the engine side. Kiro is intentionally
 * absent — the architecture plan scope lock forbids it. Anything that
 * looks like a provider list must read this constant rather than a
 * hand-rolled array.
 */
export interface CliProviderDisplay {
  id: CliProviderId;
  displayName: string;
  shortLabel: string;
  /** Native login command the user can run from a terminal. */
  loginCommand: string;
  /** Default transport the adapter drives. Surfaced in advanced views. */
  transport: CliTransportKind;
  /** Default isolation mode the adapter aims for. */
  isolationMode: HarnessIsolationMode;
}

const DISPLAY: Record<string, CliProviderDisplay> = {
  claude: {
    id: "claude",
    displayName: "Claude Code",
    shortLabel: "Claude",
    loginCommand: "claude login",
    transport: "claude_stream_json",
    isolationMode: "exclusive_coderight_gateway",
  },
  codex: {
    id: "codex",
    displayName: "Codex CLI",
    shortLabel: "Codex",
    loginCommand: "codex login",
    transport: "codex_app_server",
    isolationMode: "exclusive_coderight_gateway",
  },
  grok: {
    id: "grok",
    displayName: "Grok Build CLI",
    shortLabel: "Grok",
    loginCommand: "grok login",
    transport: "acp",
    isolationMode: "exclusive_coderight_gateway",
  },
  cursor: {
    id: "cursor",
    displayName: "Cursor Agent CLI",
    shortLabel: "Cursor",
    loginCommand: "cursor agent login",
    transport: "acp",
    isolationMode: "exclusive_coderight_gateway",
  },
  cline: {
    id: "cline",
    displayName: "Cline CLI",
    shortLabel: "Cline",
    loginCommand: "cline auth",
    transport: "acp",
    isolationMode: "exclusive_coderight_gateway",
  },
  commandcode: {
    id: "commandcode",
    displayName: "Command Code CLI",
    shortLabel: "Command Code",
    loginCommand: "commandcode login",
    transport: "resumable_ndjson",
    isolationMode: "native_read_only_compatibility",
  },
};

export function getProviderDisplay(id: CliProviderId): CliProviderDisplay {
  return (
    DISPLAY[id] ?? {
      id,
      displayName: id,
      shortLabel: id,
      loginCommand: `${id} login`,
      transport: "acp",
      isolationMode: "exclusive_coderight_gateway",
    }
  );
}

export function listProviderDisplays(): CliProviderDisplay[] {
  return CLI_PROVIDER_IDS.map((id) => DISPLAY[id]);
}

// ---------- Activation status of the canonical six ----------

/**
 * Non-secret auth-mode judgement the UI surfaces. The actual mode
 * tag is preserved for diagnostics; the explicit variant is what the
 * agent rail / status row need to render.
 */
export type ProviderActivationStatus =
  | "native_cli_account"
  | "byok_or_external_api"
  | "unknown"
  | "not_signed_in";

export interface ProviderActivationView {
  id: CliProviderId;
  signedIn: boolean;
  status: ProviderActivationStatus;
  authMode: CliAuthMode;
  profileLabel: string | null;
  accountFingerprint: string | null;
  lastVerifiedAt: string | null;
}

export function activationStatusFor(
  profile: CliAccountProfileView | undefined,
): ProviderActivationStatus {
  if (!profile?.signedIn) return "not_signed_in";
  if (profile.authMode === "native_cli_account") return "native_cli_account";
  if (profile.authMode === "byok_or_external_api") return "byok_or_external_api";
  return "unknown";
}

export function buildProviderActivationViews(
  accounts: CliAccountProfileView[],
): Map<CliProviderId, ProviderActivationView> {
  const map = new Map<CliProviderId, ProviderActivationView>();
  for (const id of CLI_PROVIDER_IDS) {
    const profile = accounts.find((p) => p.providerId === id);
    map.set(id, {
      id,
      signedIn: profile?.signedIn ?? false,
      status: activationStatusFor(profile),
      authMode: (profile?.authMode ?? "unknown") as CliAuthMode,
      profileLabel: profile?.label ?? null,
      accountFingerprint: profile?.accountFingerprint ?? null,
      lastVerifiedAt: profile?.lastVerifiedAt ?? null,
    });
  }
  return map;
}

// ---------- Harness attestation surface ----------

/** Wire shape for daemon-exported attestation history. */
export interface CliHarnessAttestationRowWire {
  attestationId: string;
  providerId: CliProviderId;
  executableSha256: string;
  binaryVersion: string;
  protocolVersion: string;
  profileHash: string;
  policyHash: string;
  toolSchemaHash: string;
  operatingSystem: string;
  architecture: string;
  isolationMode: HarnessIsolationMode;
  passedProbes: string[];
  failedProbes: string[];
  verifiedAt: string;
}

/**
 * Parse the wire attestation into the render-ready row. The Rust
 * side stores `verifiedAt` as an ISO 8601 timestamp; the helper
 * computes a friendly "X minutes ago" label and the
 * qualified/failed boolean.
 */
export function renderHarnessAttestation(
  row: CliHarnessAttestationRowWire,
  now: Date = new Date(),
): HarnessAttestationRow {
  const ageMs = Date.parse(row.verifiedAt) ? now.getTime() - Date.parse(row.verifiedAt) : 0;
  return {
    attestation: row as unknown as HarnessAttestation,
    ageLabel: formatAgeLabel(ageMs),
    qualified: row.failedProbes.length === 0,
  };
}

export function formatAgeLabel(ageMs: number): string {
  if (!Number.isFinite(ageMs) || ageMs < 0) return "just now";
  const seconds = Math.floor(ageMs / 1000);
  if (seconds < 60) return seconds <= 1 ? "just now" : `${seconds} seconds ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.floor(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

export function shortDigest(sha256: string, chars = 12): string {
  if (!sha256) return "—";
  if (sha256.length <= chars + 2) return sha256;
  return `${sha256.slice(0, chars)}…`;
}

// ---------- Eligibility ----------

/**
 * Compute the per-provider eligibility the picker consults. The
 * engine registry is the source of truth; this client-side version
 * collapses the providers into the same surface the Rust enums
 * express, so the picker renders the right enabled/disabled state
 * without a round-trip.
 */
export function computeEligibility(
  activation: ProviderActivationView,
  attestation: HarnessAttestationRow | null,
  isolationMode: HarnessIsolationMode,
): CliProviderEligibility {
  if (activation.status === "byok_or_external_api") {
    return {
      kind: "unqualified",
      reasons: ["BYOK / external-API configuration detected. Sign in via the vendor CLI."],
    };
  }
  if (activation.status === "unknown") {
    return {
      kind: "unqualified",
      reasons: ["Account mode cannot be proven. Refresh the profile or re-authenticate."],
    };
  }
  if (!activation.signedIn) {
    return { kind: "login_required" };
  }
  if (!attestation) {
    return { kind: "harness_verification_required" };
  }
  if (!attestation.qualified) {
    return {
      kind: "unqualified",
      reasons:
        attestation.attestation.failedProbes.length > 0
          ? [`Failed probes: ${attestation.attestation.failedProbes.join(", ")}`]
          : ["Harness attestation failed qualification."],
    };
  }
  if (isolationMode === "native_read_only_compatibility") {
    return { kind: "compatibility_read_only" };
  }
  return { kind: "ready" };
}

// ---------- Snapshot (compose card view) ----------

export interface BuildSnapshotInput {
  providers: DaemonCliProvider[];
}

/** Daemon readiness is sole authority for root & child CLI selection. */
export function isDaemonReadyProvider(provider: DaemonCliProvider | undefined): boolean {
  return provider?.auth.launchReady === true && provider.harness.eligibility === "ready";
}

export function daemonReadyProviderIds(providers: DaemonCliProvider[]): CliProviderId[] {
  return providers.filter(isDaemonReadyProvider).map((provider) => provider.providerId);
}

/** Drop stale or experimental child selections before they reach daemon policy. */
export function readyOnlyChildPolicy(
  policy: CliDelegationPolicy,
  providers: DaemonCliProvider[],
): CliDelegationPolicy {
  const ready = new Set(daemonReadyProviderIds(providers));
  const allowedChildProviders = policy.allowedChildProviders.filter((id) => ready.has(id));
  return {
    ...policy,
    allowedChildProviders,
    defaultChildProvider:
      policy.defaultChildProvider && ready.has(policy.defaultChildProvider)
        ? policy.defaultChildProvider
        : null,
  };
}

function eligibilityFromDaemon(provider: DaemonCliProvider): CliProviderEligibility {
  switch (provider.harness.eligibility) {
    case "ready":
      return provider.harness.isolationMode === "native_read_only_compatibility"
        ? { kind: "compatibility_read_only" }
        : { kind: "ready" };
    case "version_unsupported":
      return { kind: "version_unsupported" };
    case "unqualified":
      return { kind: "unqualified", reasons: ["Harness qualification failed."] };
    case "harness_verification_required":
      return provider.auth.launchReady
        ? { kind: "harness_verification_required" }
        : provider.auth.state === "unqualified"
          ? { kind: "harness_verification_required" }
          : { kind: "login_required" };
    case "login_required":
      return { kind: "login_required" };
  }
}

export function buildProviderCardSnapshots(input: BuildSnapshotInput): CliProviderCardSnapshot[] {
  const providers = new Map(input.providers.map((provider) => [provider.providerId, provider]));
  const ids = [
    ...new Set([...CLI_PROVIDER_IDS, ...input.providers.map((provider) => provider.providerId)]),
  ];
  return ids.map((id) => {
    const display = DISPLAY[id];
    const provider = providers.get(id);
    const profile: CliAccountProfile | null = provider?.auth.launchReady
      ? {
          id: `${id}/${provider.auth.accountLabel ?? "default"}`,
          providerId: id,
          label: provider.auth.accountLabel ?? "default",
          authMode: provider.auth.mode,
          signedIn: true,
          accountFingerprint: null,
          lastVerifiedAt: null,
        }
      : null;
    return {
      providerId: id,
      displayName: display.displayName,
      transportKind: provider?.transport ?? display.transport,
      binaryInstalled: provider?.installed ?? false,
      binaryVersion: provider?.binaryVersion ?? null,
      auth: {
        detectedSignedIn: provider?.auth.detectedSignedIn ?? false,
        launchReady: provider?.auth.launchReady ?? false,
        source: provider?.auth.source ?? "managed_cli",
        state: provider?.auth.state ?? "unavailable",
      },
      profile,
      attestation: null,
      eligibility: provider
        ? eligibilityFromDaemon(provider)
        : { kind: "harness_verification_required" },
      models: (provider?.models.items ?? []).map((model) => ({
        id: model,
        label: model,
        description: provider?.models.source ?? "Native CLI discovery",
      })),
    };
  });
}

// ---------- Child delegation policy ----------

/**
 * Default per-session child delegation policy. Mirrors the Rust
 * default in `CliDelegationPolicy::default` so the picker matches
 * the engine's behaviour without a round-trip.
 */
export function defaultDelegationPolicy(
  allowedChildProviders: CliProviderId[] = [],
  defaultChildProvider: CliProviderId | null = null,
): CliDelegationPolicy {
  return {
    allowedChildProviders,
    defaultChildProvider,
    permitModelProviderHints: true,
    maxDepth: 2,
    maxParallelChildren: 4,
    maxParallelPerProvider: {},
  };
}

export function isLaunchableProvider(id: CliProviderId): boolean {
  return CLI_PROVIDER_IDS.includes(id);
}

/**
 * Stable getter used by the agent rail: turn a stored provider id
 * into its display label. Falls back to the raw id for unknown
 * values so the rail never crashes when a future provider is added
 * before the renderer is updated.
 */
export function displayNameForProvider(id: string): string {
  if (CLI_PROVIDER_IDS.includes(id as CliProviderId)) {
    return DISPLAY[id as CliProviderId].displayName;
  }
  return id;
}

export function shortLabelForProvider(id: string): string {
  if (CLI_PROVIDER_IDS.includes(id as CliProviderId)) {
    return DISPLAY[id as CliProviderId].shortLabel;
  }
  return id;
}

/**
 * Convenience: a "default profile" id used by the root model picker
 * when the user has not yet attached a real account. Mirrors
 * `authenticatedCliDefaultProfileId` from the types module.
 */
export function defaultProfileId(providerId: CliProviderId): string {
  return authenticatedCliDefaultProfileId(providerId);
}
