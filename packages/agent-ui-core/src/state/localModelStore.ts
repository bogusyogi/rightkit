import { create } from "zustand";

import {
  getLocalModelCatalog,
  probeLocalModelEndpoint,
  smokeLocalModelChat,
  suggestLocalModelProvider,
} from "../lib/daemon.js";
import type {
  DaemonConfig,
  LocalModelCatalog,
  LocalModelChatSmokeResult,
  LocalModelProbeResult,
  LocalProviderRouteSuggestion,
} from "../lib/types.js";
import { messageOf, type SetStatus } from "../lib/utils.js";

/** Local-model cookbook: hardware catalog, endpoint probe, chat smoke, route suggest. */
export type LocalModelStoreState = {
  catalog: LocalModelCatalog | null;
  probeBaseUrl: string;
  probeResult: LocalModelProbeResult | null;
  chatSmokeResult: LocalModelChatSmokeResult | null;
  providerSuggestion: LocalProviderRouteSuggestion | null;
  actionPending: string | null;
  error: string | null;
  setProbeBaseUrl: (url: string) => void;
  loadCatalog: (config: DaemonConfig | null, setStatus: SetStatus) => Promise<void>;
  probe: (config: DaemonConfig | null, setStatus: SetStatus) => Promise<void>;
  chatSmoke: (config: DaemonConfig | null, setStatus: SetStatus, model?: string) => Promise<void>;
  suggestRoute: (
    config: DaemonConfig | null,
    setStatus: SetStatus,
    baseUrl: string,
    model: string,
    workspaceRoot: string,
    onSaved: () => Promise<void> | void,
  ) => Promise<void>;
};

export const useLocalModelStore = create<LocalModelStoreState>((set, get) => ({
  catalog: null,
  probeBaseUrl: "http://127.0.0.1:11434/v1",
  probeResult: null,
  chatSmokeResult: null,
  providerSuggestion: null,
  actionPending: null,
  error: null,

  setProbeBaseUrl(url) {
    set({ probeBaseUrl: url });
  },

  async loadCatalog(config, setStatus) {
    if (!config) return;
    set({ actionPending: "refresh", error: null });
    try {
      const catalog = await getLocalModelCatalog(config);
      set({ catalog, chatSmokeResult: null });
    } catch (err) {
      const message = messageOf(err, "Local model cookbook failed");
      set({ error: message });
      setStatus(message);
    } finally {
      set({ actionPending: null });
    }
  },

  async probe(config, setStatus) {
    if (!config || get().actionPending) return;
    set({ actionPending: "probe", error: null });
    try {
      const result = await probeLocalModelEndpoint(config, get().probeBaseUrl);
      set({ probeResult: result, chatSmokeResult: null });
      setStatus(result.ok ? "Local endpoint responded" : "Local endpoint probe failed");
    } catch (err) {
      const message = messageOf(err, "Local model probe failed");
      set({ error: message });
      setStatus(message);
    } finally {
      set({ actionPending: null });
    }
  },

  async chatSmoke(config, setStatus, model) {
    if (!config || get().actionPending) return;
    const { probeResult, catalog, probeBaseUrl } = get();
    const selectedModel =
      model?.trim() ||
      probeResult?.models.find((candidate) => candidate.trim()) ||
      catalog?.recommendations[0]?.modelSlug ||
      "";
    if (!selectedModel) {
      set({ error: "Probe the local endpoint or choose a model before chat smoke." });
      return;
    }
    set({ actionPending: "chat-smoke", error: null });
    try {
      const result = await smokeLocalModelChat(config, {
        baseUrl: probeBaseUrl,
        model: selectedModel,
      });
      set({ chatSmokeResult: result });
      setStatus(result.ok ? "Local chat smoke passed" : "Local chat smoke failed");
    } catch (err) {
      const message = messageOf(err, "Local chat smoke failed");
      set({ error: message });
      setStatus(message);
    } finally {
      set({ actionPending: null });
    }
  },

  async suggestRoute(config, setStatus, baseUrl, model, workspaceRoot, onSaved) {
    if (!config || get().actionPending) return;
    set({ actionPending: `suggest:${model}`, error: null });
    try {
      const suggestion = await suggestLocalModelProvider(config, {
        providerId: `${model.replace(/[^a-zA-Z0-9._-]+/g, "-")}-local`,
        baseUrl,
        model,
        setDefault: true,
        workspaceRoot: workspaceRoot.trim(),
      });
      set({ providerSuggestion: suggestion });
      setStatus("Local provider route saved as workspace default");
      await onSaved();
    } catch (err) {
      const message = messageOf(err, "Local provider suggestion failed");
      set({ error: message });
      setStatus(message);
    } finally {
      set({ actionPending: null });
    }
  },
}));
