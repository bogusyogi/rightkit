// Shared types for the update manifest and check results.

export const SCHEMA_VERSION = 1 as const;

export const POLICIES = ["auto-land", "agent-pr", "held"] as const;
export type Policy = (typeof POLICIES)[number];

/** RightKit platform ids used as keys of a tool's `assets` map. */
export const PLATFORMS = [
  "macos-arm64",
  "macos-x86_64",
  "windows-x86_64",
  "windows-arm64",
  "linux-x86_64",
] as const;
export type Platform = (typeof PLATFORMS)[number];

/**
 * A hold with its justification (rightkit#1267): why the dependency stays where it is, the evidence for that, and what would
 * end the hold. Only a hold with a reason excuses a row `check` cannot judge; a bare name (the 0.2 form) still holds but
 * excuses nothing.
 */
export interface HoldEntry {
  name: string;
  reason: string;
  evidence: string;
  exitCondition: string;
}

export interface PolicyConfig {
  default: Policy;
  /** Dependencies that are always held, regardless of kind: a justified hold, or a bare name (0.2 form, excuses no unknown). */
  held?: Array<string | HoldEntry>;
  tier?: {
    /** Exact names, or `/regex/` strings, that default to held. */
    held?: string[];
  };
  /** What `land` does after committing: `commit` (default) keeps the commit local, `push` also pushes. */
  land?: LandMode;
}

export const LAND_MODES = ["commit", "push"] as const;
export type LandMode = (typeof LAND_MODES)[number];

/** A repository gate: a command that must exit 0 for `gate` to pass. */
export interface GateDef {
  name: string;
  /** Argument vector. Spawned directly, never through a shell. */
  command: string[];
  /** Working directory relative to the repository root. Defaults to the root. */
  cwd?: string;
  /** Timeout in seconds. Defaults to 1800. */
  timeoutSec?: number;
}

export interface CargoSource {
  kind: "cargo";
  /** One manifest path. Exactly one of `manifest` and `glob` is set. */
  manifest?: string;
  /** Glob over manifests (`*` within one path segment), whose last segment is `Cargo.toml`. */
  glob?: string;
  policy?: Policy;
}

export interface PnpmSource {
  kind: "pnpm";
  /** One manifest path. Exactly one of `manifest` and `glob` is set. */
  manifest?: string;
  /** Glob over manifests (`*` within one path segment), whose last segment is `package.json`. */
  glob?: string;
  policy?: Policy;
}

export interface SwiftpmSource {
  kind: "swiftpm";
  /** `Package.swift` or `project.yml`, relative to the repository root. */
  manifest: string;
  policy?: Policy;
}

export interface RightkitSource {
  kind: "rightkit";
  /** Path to the RightKit versions file; defaults to packages/release/rightkit-versions.json. */
  versions?: string;
  policy?: Policy;
}

export interface ToolExtract {
  /** Path of the binary inside the downloaded archive. */
  member: string;
}

export interface ToolAsset {
  /** Asset name as published upstream. */
  asset: string;
  /** `sha256:<hex>` digest of the downloaded asset exactly as published. */
  checksum: string;
  extract?: ToolExtract;
  /** Download size bound in bytes. Defaults to 512 MiB. */
  maxBytes?: number;
}

export type ToolUpstream =
  | { type: "github-release"; repo: string }
  | { type: "url"; latest: string; download: string };

export interface Journey {
  /** Argument vector; `{candidate}` and `{out}` are placeholders for gate. */
  command: string[];
  /** Report path relative to the output directory, typically `{out}/report.json`. */
  report: string;
  /** Free-form inputs, passed as JSON on stdin when gate runs the journey. */
  inputs?: Record<string, unknown>;
  /** Model id substituted for `{model}`; empty when omitted. */
  model?: string;
  /** Journey timeout in seconds. Defaults to 1800. */
  timeoutSec?: number;
}

export interface ToolSource {
  kind: "tool";
  name: string;
  pin: string;
  assets: Partial<Record<Platform, ToolAsset>>;
  upstream: ToolUpstream;
  journey: Journey;
  policy?: Policy;
}

export type Source = CargoSource | PnpmSource | SwiftpmSource | RightkitSource | ToolSource;

export interface UpdateManifest {
  schemaVersion: typeof SCHEMA_VERSION;
  sources: Source[];
  policy: PolicyConfig;
  /** Repository gates run by `right-update gate`. */
  gates?: GateDef[];
}

export type CheckStatus = "current" | "outdated" | "unknown";

export interface PlatformReport {
  platform: Platform;
  asset: string;
  checksum: string;
  extract?: ToolExtract;
}

export interface CheckEntry {
  name: string;
  kind: Source["kind"];
  /** The manifest or source the entry came from. */
  source: string;
  pinned: string | null;
  latest: string | null;
  status: CheckStatus;
  policy: Policy;
  /** Why the status is unknown, when it is not an error. */
  reason?: string;
  /** Set when the lookup failed. Any error makes `check` exit 2. */
  error?: string;
  platforms?: PlatformReport[];
}

export interface CheckReport {
  schemaVersion: typeof SCHEMA_VERSION;
  repo: string;
  exitCode: 0 | 1 | 2;
  errors: string[];
  entries: CheckEntry[];
}
