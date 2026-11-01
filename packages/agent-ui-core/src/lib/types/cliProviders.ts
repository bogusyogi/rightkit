// TypeScript mirror of the multi-provider CLI runtime types from
// `engine/crates/driver_host/src/cli_runtime` and the storage layer
// in `engine/crates/storage/src/cli_attestation.rs`.
//
// PR 1–11 of the multi-provider CLI runtime architecture land these
// types on the Rust side. PR 12 (desktop completion) wires the
// corresponding UI in `apps/coderight-tauri/src/components`. Until
// the daemon exposes these via generated clients, the renderer reads
// them through dedicated helpers (`lib/cliProviders.ts`,
// `lib/daemon/cliProviders.ts`) so the wire schema can evolve without
// the desktop components re-rendering every field.

/**
 * Provider identifiers shipped in the built-in registry. The order
 * matches `BUILT_IN_PROVIDER_IDS` in the engine and is the priority
 * order used by the picker, repair, and migration paths.
 *
 * NOTE: `kiro` is intentionally absent. Per the architecture plan
 * scope lock the desktop UI does not surface it as an option.
 */
/** Open registry identifier; six built-ins are presentation defaults, not a runtime ceiling. */
export type CliProviderId = string;

export const CLI_PROVIDER_IDS: CliProviderId[] = [
  "claude",
  "codex",
  "grok",
  "cursor",
  "cline",
  "commandcode",
];

/**
 * Auth mode a CLI provider's account profile currently has. Mirrors
 * `CliAuthMode` from the engine. `byok_or_external_api` and
 * `unknown` are launch refusals; the UI surfaces them as warnings
 * with a clear repair action, never a "tap to launch" path.
 */
export type CliAuthMode =
  | "native_cli_account"
  | "byok_or_external_api"
  | "unknown";

/** Canonical source & state of authentication and launch authority. */
export type CliProviderAuthSource = "ambient_cli" | "managed_cli";
export type CliProviderAuthState =
  | "unavailable"
  | "needs_auth"
  | "ambient_authenticated"
  | "configured"
  | "unqualified";

/**
 * Lifecycle state for a `CliAgentBinding`. Mirrors
 * `CliBindingStatus`. The renderer treats every value in
 * `CliBindingStatus.Active` set as "currently owning a process".
 */
export type CliBindingStatus =
  | "unconfigured"
  | "login_required"
  | "account_ready"
  | "harness_verification_required"
  | "ready"
  | "starting"
  | "connected"
  | "running"
  | "waiting_for_approval"
  | "waiting_for_child"
  | "resuming"
  | "recovery_required"
  | "interrupted"
  | "completed"
  | "failed";

/**
 * Transport kind the adapter drives. Mirrors `CliTransportKind`.
 * Surfaced in the UI only when the user expands an advanced
 * disclosure; the picker itself never shows transport.
 */
export type CliTransportKind =
  | "claude_stream_json"
  | "codex_app_server"
  | "acp"
  | "resumable_ndjson";

/**
 * Isolation mode a binding is currently configured for. Mirrors
 * `HarnessIsolationMode`. `exclusive_coderight_gateway` is the
 * target; `native_read_only_compatibility` is a documented transition
 * state and must be visible in the UI.
 */
export type HarnessIsolationMode = "exclusive_coderight_gateway" | "native_read_only_compatibility";

/**
 * Resume strategy a provider uses when its process restarts. Mirrors
 * `ResumeStrategy`. Surfaced only in diagnostics / advanced views;
 * the picker does not display this.
 */
export type ResumeStrategy =
  | "in_process_session"
  | "protocol_load_by_id"
  | "process_restart_with_resume_flag";

/**
 * Eligibility state for a provider, as derived by the engine
 * registry. Only `ready` is launchable as a root or child.
 * `compatibility_read_only` is launchable as a labeled transition.
 * Every other variant produces a disabled row with the concrete
 * reason. There is no warning-then-launch path.
 */
export type CliProviderEligibility =
  | { kind: "ready" }
  | { kind: "compatibility_read_only" }
  | { kind: "login_required" }
  | { kind: "harness_verification_required" }
  | { kind: "version_unsupported" }
  | { kind: "unqualified"; reasons: string[] };

/**
 * One vendor account profile. the host app stores only non-secret
 * metadata; credentials live inside the vendor-managed profile
 * directory. Mirrors `CliAccountProfile`.
 */
export interface CliAccountProfile {
  id: string;
  providerId: CliProviderId;
  label: string;
  /** Vendor-owned profile location is intentionally never exposed to UI. */
  managedHome?: string;
  authMode: CliAuthMode;
  signedIn: boolean;
  accountFingerprint: string | null;
  lastVerifiedAt: string | null;
}

/**
 * Result of a qualification suite against a real installed binary.
 * Attestations are content-addressed by every input that can affect
 * provider behavior; changing any input invalidates them. Mirrors
 * `HarnessAttestation`.
 */
export interface HarnessAttestation {
  id: string;
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

export interface HarnessAttestationRow {
  attestation: HarnessAttestation;
  /** Display-friendly relative age (e.g. "12 minutes ago"). */
  ageLabel: string | null;
  /** True when the attestation has zero failing probes. */
  qualified: boolean;
}

/**
 * Session-scoped delegation policy for cross-provider child routing.
 * Mirrors `CliDelegationPolicy`. Per-session rather than global so
 * the user can opt each session into a different mix of providers.
 */
export interface CliDelegationPolicy {
  allowedChildProviders: CliProviderId[];
  defaultChildProvider: CliProviderId | null;
  permitModelProviderHints: boolean;
  maxDepth: number;
  maxParallelChildren: number;
  maxParallelPerProvider: Record<string, number>;
}

/**
 * Static model catalogue entry for a provider. Mirrors
 * `CliModelCatalog.models` (string-typed model ids surfaced by the
 * adapter). The renderer merges per-provider lists into the picker.
 */
export interface CliModelEntry {
  id: string;
  label: string;
  description: string;
}

/**
 * Snapshot of one provider the desktop UI shows in the providers
 * tab. Composes account profile + latest attestation + static
 * capabilities into a single render-ready record.
 */
export interface CliProviderCardSnapshot {
  providerId: CliProviderId;
  displayName: string;
  transportKind: CliTransportKind;
  binaryInstalled: boolean;
  binaryVersion: string | null;
  auth: {
    detectedSignedIn: boolean;
    launchReady: boolean;
    source: CliProviderAuthSource;
    state: CliProviderAuthState;
  };
  profile: CliAccountProfile | null;
  attestation: HarnessAttestationRow | null;
  eligibility: CliProviderEligibility;
  models: CliModelEntry[];
}

/**
 * Selector for the runtime that should host a sub-agent. Mirrors
 * `SpawnRuntimeKind` from `engine/crates/tools/src/context.rs`.
 * `HostNative` keeps the legacy single-runtime behaviour;
 * `AuthenticatedCli` carries the provider/model/account profile id
 * the spawner needs to wire the matching adapter.
 */
export type SpawnRuntimeKind =
  | { kind: "coderight_native"; modelId: string }
  | {
      kind: "authenticated_cli";
      providerId: CliProviderId;
      modelId: string | null;
      accountProfileId: string;
    };

export function isAuthenticatedCli(
  runtime: SpawnRuntimeKind,
): runtime is Extract<SpawnRuntimeKind, { kind: "authenticated_cli" }> {
  return runtime.kind === "authenticated_cli";
}

export function authenticatedCliDefaultProfileId(providerId: CliProviderId): string {
  return `${providerId}/default`;
}
