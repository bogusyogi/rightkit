import { create } from "zustand";

import {
  deleteMcpServer,
  getMcpServerModeStatus,
  listMcpServers,
  probeMcpServer,
  saveMcpServer,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { McpDraft } from "../components/settings/types.js";
import type {
  DaemonConfig,
  McpProbeResult,
  McpServerConfig,
  McpServerModeStatus,
  McpServerRow,
} from "@rightkit/agent-ui-core/lib/types";
import { messageOf, type SetStatus } from "@rightkit/agent-ui-core/lib/utils";

export function mcpConfigFromDraft(draft: McpDraft): McpServerConfig | null {
  if (draft.type === "http") {
    const url = draft.url.trim();
    return url ? { type: "http", url } : null;
  }
  if (draft.type === "sse") {
    const url = draft.url.trim();
    return url ? { type: "sse", url } : null;
  }
  const command = draft.command.trim();
  if (!command) return null;
  return {
    type: "stdio",
    command,
    args: draft.args.trim() ? draft.args.trim().split(/\s+/) : [],
    env: {},
  };
}

export function parseMcpChannels(raw: string): string[] {
  return Array.from(
    new Set(
      raw
        .split(/[\s,]+/)
        .map((part) => part.trim().toLowerCase())
        .filter(Boolean),
    ),
  );
}

function mcpServerKey(server: Pick<McpServerRow, "scope" | "name">) {
  return `${server.scope}:${server.name}`;
}

const DEFAULT_MCP_DRAFT: McpDraft = {
  name: "",
  scope: "workspace",
  type: "stdio",
  trustPreset: "local_process",
  channelAllowlist: "desktop, cli",
  allowMutating: false,
  url: "",
  command: "",
  args: "",
};

/** MCP-server settings: configured servers, probe results, server-mode status,
 * config paths, and the add-server draft form. */
export type McpStoreState = {
  servers: McpServerRow[];
  probeResults: Record<string, McpProbeResult>;
  serverModeStatus: McpServerModeStatus | null;
  userConfigPath: string;
  workspaceConfigPath: string | null;
  actionPending: string | null;
  error: string | null;
  draft: McpDraft;
  setDraft: (draft: McpDraft) => void;
  load: (config: DaemonConfig | null, workspaceRoot: string, setStatus: SetStatus) => Promise<void>;
  submit: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  remove: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    server: McpServerRow,
    setStatus: SetStatus,
  ) => Promise<void>;
  probe: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    server: McpServerRow,
    setStatus: SetStatus,
  ) => Promise<void>;
};

export const useMcpStore = create<McpStoreState>((set, get) => ({
  servers: [],
  probeResults: {},
  serverModeStatus: null,
  userConfigPath: "",
  workspaceConfigPath: null,
  actionPending: null,
  error: null,
  draft: DEFAULT_MCP_DRAFT,

  setDraft(draft) {
    set({ draft });
  },

  async load(config, workspaceRoot, setStatus) {
    if (!config) return;
    try {
      const [next, serverMode] = await Promise.all([
        listMcpServers(config, workspaceRoot.trim()),
        getMcpServerModeStatus(config),
      ]);
      set({
        servers: next.servers,
        serverModeStatus: serverMode,
        userConfigPath: next.userConfigPath,
        workspaceConfigPath: next.workspaceConfigPath ?? null,
        error: null,
      });
    } catch (err) {
      const message = messageOf(err, "MCP failed");
      set({ error: message });
      setStatus(message);
    }
  },

  async submit(config, workspaceRoot, setStatus) {
    if (!config) return;
    const draft = get().draft;
    const name = draft.name.trim();
    const serverConfig = mcpConfigFromDraft(draft);
    if (!name || !serverConfig) {
      setStatus("MCP server needs a name and transport details");
      return;
    }
    set({ actionPending: name });
    try {
      await saveMcpServer(config, {
        scope: draft.scope,
        workspaceRoot: workspaceRoot.trim(),
        name,
        config: serverConfig,
        trustPreset: draft.trustPreset,
        approvedBy: "settings-ui",
        channelPermissions: {
          allowedChannels: parseMcpChannels(draft.channelAllowlist),
          allowMutating: draft.allowMutating,
        },
      });
      await get().load(config, workspaceRoot, setStatus);
      set({ draft: { ...get().draft, name: "", url: "", command: "", args: "" } });
      setStatus("MCP server saved");
    } catch (err) {
      setStatus(messageOf(err, "MCP save failed"));
    } finally {
      set({ actionPending: null });
    }
  },

  async remove(config, workspaceRoot, server, setStatus) {
    if (!config) return;
    set({ actionPending: `${server.scope}:${server.name}` });
    try {
      await deleteMcpServer(config, server.name, server.scope, workspaceRoot.trim());
      await get().load(config, workspaceRoot, setStatus);
      setStatus("MCP server removed");
    } catch (err) {
      setStatus(messageOf(err, "MCP remove failed"));
    } finally {
      set({ actionPending: null });
    }
  },

  async probe(config, workspaceRoot, server, setStatus) {
    if (!config) return;
    const key = mcpServerKey(server);
    set({ actionPending: `probe:${key}` });
    try {
      const result = await probeMcpServer(config, server.name, server.scope, workspaceRoot.trim());
      set((state) => ({ probeResults: { ...state.probeResults, [key]: result } }));
      setStatus(
        result.ok
          ? `MCP ${server.name}: ${result.toolCount} tools`
          : `MCP ${server.name}: probe failed`,
      );
    } catch (err) {
      setStatus(messageOf(err, "MCP probe failed"));
    } finally {
      set({ actionPending: null });
    }
  },
}));
