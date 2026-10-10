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

export interface PolicyConfig {
  default: Policy;
  /** Dependency names that are always held, regardless of kind. */
  held?: string[];
  tier?: {
    /** Exact names, or `/regex/` strings, that default to held. */
    held?: string[];
  };
}

export interface CargoSource {
  kind: "cargo";
  manifest: string;
  policy?: Policy;
}

export interface PnpmSource {
  kind: "pnpm";
  manifest: string;
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
