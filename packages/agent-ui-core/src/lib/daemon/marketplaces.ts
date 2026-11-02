import type {
  DaemonConfig,
  PluginInstallResponse,
  PluginMarketplaceEntry,
  PluginMarketplaceListResponse,
  PluginMarketplaceSyncResponse,
  SkillItem,
  SkillListResponse,
  SkillSaveResponse,
} from "../types.js";
import { requestDaemonApi } from "../generated/daemon-openapi.js";

/** The daemon-owned inputs for a marketplace catalog refresh. */
export interface MarketplaceCatalogSyncRequest {
  catalogUrl: string;
  expectedSha256: string;
  trustAccepted: boolean;
  requireSigned: boolean;
  allowFileUrls: boolean;
}

/** The daemon-owned inputs for installing a catalog plugin. */
export interface MarketplacePluginInstallRequest {
  trustAccepted: boolean;
  expectedSha256?: string;
  overwrite: boolean;
  allowFileUrls: boolean;
}

/** The exact request accepted by POST /skills/install. */
export interface MarketplaceSkillInstallRequest {
  slug: string;
  content: string;
  name?: string;
  description?: string;
  trigger?: string;
  enabled?: boolean;
  category?: "tool" | "behavioral";
  priority?: number;
  tags?: string[];
}

/** The exact request accepted by PUT /skills/{slug}. */
export interface MarketplaceSkillUpdateRequest {
  content: string;
  enabled?: boolean;
}

/**
 * The registry shape used by the MCP discovery projection. These are data
 * contracts only: the current daemon does not expose registry search/install
 * operations, so the component receives them from the eventual daemon seam.
 */
export interface McpRegistrySearchResult {
  servers: McpRegistryServer[];
  nextCursor?: string | null;
  count?: number;
}

export interface McpRegistryServer {
  name: string;
  description: string;
  title?: string;
  version?: string;
  packages: McpRegistryPackage[];
  remotes: McpRegistryRemote[];
  repository?: { url?: string; source?: string };
}

export interface McpRegistryPackage {
  registryType: string;
  identifier: string;
  version?: string;
  environmentVariables: McpRegistryEnvironmentVariable[];
}

export interface McpRegistryRemote {
  type: string;
  url: string;
  headers: McpRegistryHeader[];
}

export interface McpRegistryEnvironmentVariable {
  name: string;
  description?: string;
  isRequired?: boolean;
  isSecret?: boolean;
}

export interface McpRegistryHeader {
  name: string;
  description?: string;
  value?: string;
  isRequired?: boolean;
  isSecret?: boolean;
}

export interface ConfiguredMcpServerProjection {
  name: string;
  args?: string[];
  url?: string;
  agent?: "claude" | "codex";
}

/** Community skills data returned by the planned registry endpoint. */
export interface CommunitySkillResult {
  id: string;
  name: string;
  skillId: string;
  installs: number;
  source: string;
}

export interface CommunitySkillDetail {
  id: string;
  name: string;
  description: string;
  installs: number;
  source: string;
  content: string | null;
  rawUrl: string | null;
  skillsShUrl: string | null;
  githubUrl: string | null;
}

export interface RegistryHealthProjection {
  available: boolean;
  reason?: string | null;
}

/** Search the daemon-owned plugin marketplace catalog. */
export async function getMarketplacePlugins(
  config: DaemonConfig,
  query = "",
): Promise<PluginMarketplaceListResponse> {
  return requestDaemonApi<PluginMarketplaceListResponse>(config, "getPluginsMarketplace", {
    query: { query: query.trim() || undefined },
  });
}

/** Refresh the daemon-owned plugin marketplace catalog. */
export async function syncMarketplaceCatalog(
  config: DaemonConfig,
  request: MarketplaceCatalogSyncRequest,
): Promise<PluginMarketplaceSyncResponse> {
  return requestDaemonApi<PluginMarketplaceSyncResponse, MarketplaceCatalogSyncRequest>(
    config,
    "postPluginsMarketplaceSync",
    { body: request },
  );
}

/** Install one signed/checksum-pinned plugin from the daemon catalog. */
export async function installPluginFromMarketplace(
  config: DaemonConfig,
  pluginId: string,
  request: MarketplacePluginInstallRequest,
): Promise<PluginInstallResponse> {
  return requestDaemonApi<PluginInstallResponse, MarketplacePluginInstallRequest>(
    config,
    "postPluginsMarketplaceByIdInstall",
    { pathParams: { id: pluginId }, body: request },
  );
}

/** Read the daemon-owned installed skill inventory before marketplace install. */
export async function getInstalledSkills(config: DaemonConfig): Promise<SkillListResponse> {
  return requestDaemonApi<SkillListResponse>(config, "getSkills");
}

/** Install a validated marketplace skill through the daemon's skill writer. */
export async function installSkillFromMarketplace(
  config: DaemonConfig,
  request: MarketplaceSkillInstallRequest,
): Promise<{ installed: boolean; skillDir: string; skill: SkillItem }> {
  return requestDaemonApi<
    { installed: boolean; skillDir: string; skill: SkillItem },
    MarketplaceSkillInstallRequest
  >(config, "postSkillsInstall", { body: request });
}

/** Update an installed skill through the daemon-owned skill writer. */
export async function updateMarketplaceSkill(
  config: DaemonConfig,
  slug: string,
  request: MarketplaceSkillUpdateRequest,
): Promise<SkillSaveResponse> {
  return requestDaemonApi<SkillSaveResponse, MarketplaceSkillUpdateRequest>(
    config,
    "putSkillsBySlug",
    { pathParams: { slug }, body: request },
  );
}

/** Remove an installed skill through the daemon-owned skill writer. */
export async function removeMarketplaceSkill(config: DaemonConfig, slug: string): Promise<void> {
  await requestDaemonApi(config, "deleteSkillsBySlug", { pathParams: { slug } });
}

/**
 * Convert a registry identifier to the daemon's local skill slug format.
 * Returning null is deliberate: the UI must not send traversal or empty ids to
 * POST /skills/install, and the daemon remains the authority for final checks.
 */
export function toMarketplaceSkillSlug(skillId: string): string | null {
  const base = skillId.trim().split("/").pop() ?? "";
  const slug = base
    .replace(/:/g, "-")
    .replace(/[^a-zA-Z0-9_-]/g, "")
    .toLowerCase();
  return /^[a-z0-9](?:[a-z0-9_-]{0,62})$/.test(slug) ? slug : null;
}

export type { PluginMarketplaceEntry };
