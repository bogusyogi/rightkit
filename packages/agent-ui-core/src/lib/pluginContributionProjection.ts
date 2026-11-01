import type {
  PluginCommandContribution,
  PluginManifest,
  PluginPanelComponent,
  PluginPanelContribution,
  PluginPermission,
  PluginSessionActionContribution,
  PluginSettingsSchema,
  PluginStatusBarItem,
} from "./pluginContributionTypes";
import { isPluginPermission } from "./pluginContributionTypes";
import type { PluginManifest as DaemonPluginManifest } from "./types";

export interface ProjectedCommand extends PluginCommandContribution {
  pluginId: string;
  pluginName: string;
}

export interface ProjectedPanel extends PluginPanelContribution {
  pluginId: string;
  component?: PluginPanelComponent;
}

export interface ProjectedStatusBarItem extends PluginStatusBarItem {
  pluginId: string;
  visible: boolean;
}

export interface ProjectedSessionAction extends PluginSessionActionContribution {
  pluginId: string;
  badge?: { text?: string; count?: number };
}

export interface PluginContributionProjection {
  pluginId: string;
  pluginName: string;
  version: string;
  description: string;
  commands: ProjectedCommand[];
  panels: ProjectedPanel[];
  statusBarItems: ProjectedStatusBarItem[];
  sessionActions: ProjectedSessionAction[];
  settings: PluginSettingsSchema;
  permissions: PluginPermission[];
  unsupportedPermissions: string[];
}

export interface PluginContributionProjectionOptions {
  panelComponents?: ReadonlyMap<string, PluginPanelComponent>;
  statusBarOverrides?: ReadonlyMap<
    string,
    { text?: string; tooltip?: string; visible?: boolean }
  >;
  sessionActionBadges?: ReadonlyMap<string, { text?: string; count?: number }>;
}

/**
 * Projects the Hermes contribution shape into renderer-owned rows. This is a
 * pure projection: no plugin code runs, and no daemon mutation is attempted.
 * The runtime can therefore use it for both initial render and hot reload.
 */
export function projectPluginContributions(
  manifest: PluginManifest,
  options: PluginContributionProjectionOptions = {},
): PluginContributionProjection {
  const contributions = manifest.contributes ?? {};
  const panelComponents = options.panelComponents;
  const statusBarOverrides = options.statusBarOverrides;
  const sessionActionBadges = options.sessionActionBadges;
  const declaredPermissions = [
    ...(manifest.permissions ?? []),
    // Hermes' runtime grants storage when a plugin contributes settings,
    // because those values are persisted through the plugin storage API.
    ...(Object.keys(contributions.settings ?? {}).length > 0 ? ["storage"] : []),
  ];
  const permissionProjection = projectPluginPermissions(declaredPermissions);

  return {
    pluginId: manifest.id,
    pluginName: manifest.name,
    version: manifest.version,
    description: manifest.description,
    commands: (contributions.commands ?? []).map((command) => ({
      ...command,
      pluginId: manifest.id,
      pluginName: manifest.name,
    })),
    panels: (contributions.panels ?? []).map((panel) => ({
      ...panel,
      pluginId: manifest.id,
      component: panelComponents?.get(panel.id),
    })),
    statusBarItems: (contributions.statusBarItems ?? []).map((item) => {
      const override = statusBarOverrides?.get(item.id);
      return {
        ...item,
        pluginId: manifest.id,
        text: override?.text ?? item.text,
        tooltip: override?.tooltip ?? item.tooltip,
        visible: override?.visible ?? true,
      };
    }),
    sessionActions: (contributions.sessionActions ?? []).map((action) => ({
      ...action,
      pluginId: manifest.id,
      badge: sessionActionBadges?.get(action.id),
    })),
    settings: { ...(contributions.settings ?? {}) },
    ...permissionProjection,
  };
}

/**
 * Projects all active manifests in registration order, matching Hermes'
 * contribution enumeration. A later item remains later; the host decides
 * where that order is meaningful (for example, status-bar priority).
 */
export function projectActivePluginContributions(
  manifests: readonly PluginManifest[],
  activePluginIds?: ReadonlySet<string>,
  options: PluginContributionProjectionOptions = {},
): PluginContributionProjection[] {
  return manifests
    .filter((manifest) => !activePluginIds || activePluginIds.has(manifest.id))
    .map((manifest) => projectPluginContributions(manifest, options));
}

/**
 * Permission projection is fail-closed. Known declarations are deduplicated;
 * malformed or future declarations are retained as diagnostics rather than
 * being silently granted to a plugin.
 */
export function projectPluginPermissions(
  permissions: readonly (string | PluginPermission)[],
): Pick<PluginContributionProjection, "permissions" | "unsupportedPermissions"> {
  const known = new Set<PluginPermission>();
  const unsupported: string[] = [];
  for (const permission of permissions) {
    if (isPluginPermission(permission)) {
      known.add(permission);
    } else if (!unsupported.includes(permission)) {
      unsupported.push(permission);
    }
  }
  return { permissions: [...known], unsupportedPermissions: unsupported };
}

/**
 * Adapter for the host app daemon manifest. The daemon currently exposes
 * command/tool metadata, not Hermes frontend `contributes` or permissions;
 * this keeps that absence explicit instead of fabricating panels/components.
 */
export function projectDaemonPluginManifest(
  manifest: DaemonPluginManifest,
): PluginContributionProjection {
  return {
    pluginId: manifest.id,
    pluginName: manifest.name,
    version: manifest.version,
    description: manifest.description,
    commands: manifest.providesCommands.map((command) => ({
      command,
      title: command,
      pluginId: manifest.id,
      pluginName: manifest.name,
    })),
    panels: [],
    statusBarItems: [],
    sessionActions: [],
    settings: {},
    permissions: [],
    unsupportedPermissions: [],
  };
}

// Explicit alias for callers that prefer the shorter verb used by the
// extension registry API.
export const projectPluginManifest = projectPluginContributions;
