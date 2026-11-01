import { create } from "zustand";

import {
  createDirectConnection,
  deleteDirectConnection,
  listInferenceCatalog,
  listInferenceConnections,
  probeInferenceConnection,
  setDirectConnectionSecret,
  updateDirectConnection,
} from "../lib/daemon/inference";
import type { DaemonConfig } from "../lib/types";
import type {
  CatalogGroup,
  CatalogModel,
  CatalogState,
  DirectConnection,
  InferenceAuthority,
  InferenceProtocol,
  InferenceSelection,
} from "../lib/types/inference";

export type ConnectionProfileKind = "local" | "remote";
export type DirectConnectionInput = Parameters<typeof createDirectConnection>[1];
export type EditableConnectionInput = DirectConnectionInput & { id?: string };

export interface ConnectionProfile {
  id: string;
  kind: ConnectionProfileKind;
  label: string;
  authority: InferenceAuthority;
  connectionId: string;
  protocol: InferenceProtocol;
  state: CatalogState | "unknown";
  models: CatalogModel[];
  credentialConfigured: boolean;
  authKind: DirectConnection["authKind"];
  authHeaderName: string | null;
  url: string | null;
}

export interface ConnectionProfilesState {
  profiles: ConnectionProfile[];
  catalogGroups: CatalogGroup[];
  activeProfileId: string | null;
  activeSelection: InferenceSelection | null;
  loading: boolean;
  pendingAction: string | null;
  error: string | null;
  reset: () => void;
  load: (config: DaemonConfig | null, selection?: InferenceSelection | null) => Promise<void>;
  selectProfile: (profileId: string) => InferenceSelection | null;
  selectModel: (profileId: string, modelId: string) => InferenceSelection | null;
  save: (
    config: DaemonConfig | null,
    input: EditableConnectionInput,
    secret?: string,
  ) => Promise<boolean>;
  remove: (config: DaemonConfig | null, profileId: string) => Promise<boolean>;
  probe: (config: DaemonConfig | null, profileId: string) => Promise<boolean>;
}

let loadGeneration = 0;

function isLocalUrl(url: string): boolean {
  try {
    const hostname = new URL(url).hostname.toLowerCase();
    return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]" || hostname === "::1";
  } catch {
    return false;
  }
}

function kindForConnection(connection: DirectConnection): ConnectionProfileKind {
  return connection.id === "local" || isLocalUrl(connection.normalizedBaseUrl) ? "local" : "remote";
}

function profileFromDirect(connection: DirectConnection, group?: CatalogGroup): ConnectionProfile {
  return {
    id: `direct:${connection.id}`,
    kind: kindForConnection(connection),
    label: connection.name,
    authority: "direct",
    connectionId: connection.id,
    protocol: group?.protocol ?? connection.protocol,
    state: group?.state ?? (connection.credentialConfigured ? "unknown" : "needs_secret"),
    models: group?.models ?? [],
    credentialConfigured: connection.credentialConfigured,
    authKind: connection.authKind,
    authHeaderName: connection.authHeaderName,
    url: connection.normalizedBaseUrl,
  };
}

export function buildConnectionProfiles(
  connections: DirectConnection[],
  groups: CatalogGroup[],
): ConnectionProfile[] {
  const directGroups = new Map(
    groups
      .filter((group) => group.authority === "direct")
      .map((group) => [group.connectionId, group]),
  );
  const profiles = connections.map((connection) =>
    profileFromDirect(connection, directGroups.get(connection.id)),
  );
  const knownDirectIds = new Set(connections.map((connection) => connection.id));
  for (const group of groups) {
    if (group.authority === "direct" && !knownDirectIds.has(group.connectionId)) {
      profiles.push({
        id: `direct:${group.connectionId}`,
        kind: group.connectionId === "local" || isLocalUrl(group.name) ? "local" : "remote",
        label: group.name,
        authority: group.authority,
        connectionId: group.connectionId,
        protocol: group.protocol,
        state: group.state,
        models: group.models,
        credentialConfigured: group.state === "ready",
        authKind: "none",
        authHeaderName: null,
        url: null,
      });
    }
  }
  return profiles;
}

function selectionForProfile(profile: ConnectionProfile, modelId: string): InferenceSelection {
  return {
    schemaVersion: 1,
    authority: profile.authority,
    connectionId: profile.connectionId,
    protocol: profile.protocol,
    modelId,
    routePolicyId: null,
  };
}

function firstSelectableModel(profile: ConnectionProfile): CatalogModel | undefined {
  return (
    profile.models.find((model) => model.selectable && !model.id.startsWith("auto/")) ??
    profile.models.find((model) => model.selectable)
  );
}

function profileForSelection(
  profiles: ConnectionProfile[],
  selection: InferenceSelection | null | undefined,
): ConnectionProfile | undefined {
  if (!selection) return undefined;
  return profiles.find(
    (profile) =>
      profile.authority === selection.authority && profile.connectionId === selection.connectionId,
  );
}

export const useConnectionProfilesStore = create<ConnectionProfilesState>((set, get) => ({
  profiles: [],
  catalogGroups: [],
  activeProfileId: null,
  activeSelection: null,
  loading: false,
  pendingAction: null,
  error: null,

  reset() {
    loadGeneration += 1;
    set({
      profiles: [],
      catalogGroups: [],
      activeProfileId: null,
      activeSelection: null,
      loading: false,
      pendingAction: null,
      error: null,
    });
  },

  async load(config, selection) {
    if (!config) {
      get().reset();
      return;
    }
    const generation = ++loadGeneration;
    set({ loading: true, error: null });
    try {
      const [connections, groups] = await Promise.all([
        listInferenceConnections(config),
        listInferenceCatalog(config),
      ]);
      if (generation !== loadGeneration) return;
      const profiles = buildConnectionProfiles(connections, groups);
      const selectedProfile = profileForSelection(profiles, selection);
      set({
        profiles,
        catalogGroups: groups,
        activeProfileId: selectedProfile?.id ?? profiles[0]?.id ?? null,
        activeSelection: selection ?? null,
        loading: false,
      });
    } catch (error) {
      if (generation !== loadGeneration) return;
      set({ loading: false, error: error instanceof Error ? error.message : String(error) });
    }
  },

  selectProfile(profileId) {
    const profile = get().profiles.find((item) => item.id === profileId);
    if (!profile) return null;
    const model = firstSelectableModel(profile);
    const selection = model ? selectionForProfile(profile, model.id) : null;
    set({ activeProfileId: profile.id, activeSelection: selection });
    return selection;
  },

  selectModel(profileId, modelId) {
    const profile = get().profiles.find((item) => item.id === profileId);
    const model = profile?.models.find((item) => item.id === modelId && item.selectable);
    if (!profile || !model) return null;
    const selection = selectionForProfile(profile, model.id);
    set({ activeProfileId: profile.id, activeSelection: selection });
    return selection;
  },

  async save(config, input, secret) {
    if (!config || !input.name.trim() || !input.normalizedBaseUrl.trim()) return false;
    const action = input.id ? `update:${input.id}` : "create";
    set({ pendingAction: action, error: null });
    try {
      const { id, ...body } = input;
      const saved = id
        ? await updateDirectConnection(config, id, body)
        : await createDirectConnection(config, body);
      if (secret?.trim()) await setDirectConnectionSecret(config, saved.id, secret.trim());
      await get().load(config, get().activeSelection);
      return true;
    } catch (error) {
      set({ error: error instanceof Error ? error.message : String(error) });
      return false;
    } finally {
      set({ pendingAction: null });
    }
  },

  async remove(config, profileId) {
    const profile = get().profiles.find((item) => item.id === profileId);
    if (!config || !profile || profile.authority !== "direct") return false;
    set({ pendingAction: `remove:${profileId}`, error: null });
    try {
      await deleteDirectConnection(config, profile.connectionId);
      const selection = get().activeProfileId === profileId ? null : get().activeSelection;
      await get().load(config, selection);
      return true;
    } catch (error) {
      set({ error: error instanceof Error ? error.message : String(error) });
      return false;
    } finally {
      set({ pendingAction: null });
    }
  },

  async probe(config, profileId) {
    const profile = get().profiles.find((item) => item.id === profileId);
    if (!config || !profile) return false;
    set({ pendingAction: `probe:${profileId}`, error: null });
    try {
      const group = await probeInferenceConnection(config, profile.authority, profile.connectionId);
      const groups = [
        ...get().catalogGroups.filter(
          (item) =>
            item.authority !== group.authority || item.connectionId !== group.connectionId,
        ),
        group,
      ];
      set({
        catalogGroups: groups,
        profiles: get().profiles.map((item) =>
          item.authority === group.authority && item.connectionId === group.connectionId
            ? { ...item, state: group.state, models: group.models, protocol: group.protocol }
            : item,
        ),
      });
      return true;
    } catch (error) {
      set({ error: error instanceof Error ? error.message : String(error) });
      return false;
    } finally {
      set({ pendingAction: null });
    }
  },
}));

export function activeProfileForSelection(
  profiles: ConnectionProfile[],
  selection: InferenceSelection | null | undefined,
): ConnectionProfile | null {
  return profileForSelection(profiles, selection) ?? null;
}
