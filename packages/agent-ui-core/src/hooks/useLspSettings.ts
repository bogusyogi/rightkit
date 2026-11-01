import { useCallback, useState } from "react";
import { getLspStatus, saveLspConfig } from "../lib/daemon";
import type { DaemonConfig, LspStatusView, LspWorkspaceServer } from "../lib/types";

interface UseLspSettingsOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

function draftFromStatus(status: LspStatusView): LspWorkspaceServer[] {
  return status.servers
    .filter((server) => server.configuredPath)
    .map((server) => ({
      language: server.language,
      extensions: server.extensions,
      serverPath: server.configuredPath ?? "",
      serverArgs: server.serverArgs,
    }));
}

function normalizeDraft(draft: LspWorkspaceServer[]): LspWorkspaceServer[] {
  return draft
    .map((server) => ({
      ...server,
      language: server.language.trim(),
      serverPath: server.serverPath.trim(),
      extensions: server.extensions.map((ext) => ext.trim()).filter(Boolean),
      serverArgs: server.serverArgs.map((arg) => arg.trim()).filter(Boolean),
    }))
    .filter((server) => server.language && server.serverPath);
}

export function useLspSettings({
  config,
  workspaceRoot,
  formatError,
  onStatus,
}: UseLspSettingsOptions) {
  const [lspStatus, setLspStatus] = useState<LspStatusView | null>(null);
  const [lspDraft, setLspDraft] = useState<LspWorkspaceServer[]>([]);
  const [lspActionPending, setLspActionPending] = useState<string | null>(null);
  const [lspError, setLspError] = useState<string | null>(null);

  const loadLsp = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) {
      setLspStatus(null);
      setLspDraft([]);
      return;
    }
    setLspError(null);
    try {
      const next = await getLspStatus(config, workspaceRoot.trim());
      setLspStatus(next);
      setLspDraft(draftFromStatus(next));
    } catch (err) {
      const message = formatError(err, "LSP failed");
      setLspError(message);
      onStatus(message);
    }
  }, [config, formatError, onStatus, workspaceRoot]);

  const saveLsp = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) {
      onStatus("LSP config needs an active workspace");
      return;
    }
    const servers = normalizeDraft(lspDraft);
    setLspActionPending("save");
    setLspError(null);
    try {
      const next = await saveLspConfig(config, workspaceRoot.trim(), servers);
      setLspStatus(next);
      setLspDraft(servers);
      onStatus("LSP config saved");
    } catch (err) {
      const message = formatError(err, "LSP save failed");
      setLspError(message);
      onStatus(message);
    } finally {
      setLspActionPending(null);
    }
  }, [config, formatError, lspDraft, onStatus, workspaceRoot]);

  return {
    lspStatus,
    lspDraft,
    lspActionPending,
    lspError,
    setLspDraft,
    loadLsp,
    saveLsp,
  };
}

export type UseLspSettingsReturn = ReturnType<typeof useLspSettings>;
