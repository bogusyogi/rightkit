import { create } from "zustand";

import {
  createDiagnosticsBundle,
  downloadDiagnosticsBundle,
  getCoreSandboxSettings,
  getDiagnostics,
  getInstructions,
} from "../lib/daemon";
import type {
  DaemonConfig,
  DiagnosticsReport,
  InstructionStatus,
  SupportBundle,
  SupportBundleDownload,
} from "../lib/types";
import { messageOf, type SetCoreSandboxSettings, type SetStatus } from "../lib/utils";

/** Diagnostics settings: the diagnostics report, support-bundle lifecycle, and
 * instruction status. `coreSandboxSettings` lives in the shell (shared with the
 * execution slice) and is threaded into `load`; the bundle download uses a
 * `saveBundle` callback so the shell keeps its file-write helpers. */
export type DiagnosticsStoreState = {
  diagnostics: DiagnosticsReport | null;
  supportBundle: SupportBundle | null;
  supportBundlePending: string | null;
  instructionStatus: InstructionStatus | null;
  error: string | null;
  loading: boolean;
  load: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    setStatus: SetStatus,
    setCoreSandboxSettings: SetCoreSandboxSettings,
  ) => Promise<void>;
  createBundle: (
    config: DaemonConfig | null,
    selectedId: string | null,
    setStatus: SetStatus,
  ) => Promise<void>;
  downloadBundle: (
    config: DaemonConfig | null,
    setStatus: SetStatus,
    saveBundle: (bundle: SupportBundleDownload) => void,
  ) => Promise<void>;
};

export const useDiagnosticsStore = create<DiagnosticsStoreState>((set, get) => ({
  diagnostics: null,
  supportBundle: null,
  supportBundlePending: null,
  instructionStatus: null,
  error: null,
  loading: false,

  async load(config, workspaceRoot, setStatus, setCoreSandboxSettings) {
    if (!config) return;
    set({ loading: true, error: null });
    try {
      const [next, instructions, sandboxSettings] = await Promise.all([
        getDiagnostics(config, workspaceRoot.trim()),
        getInstructions(config, workspaceRoot.trim()),
        getCoreSandboxSettings(config, workspaceRoot.trim()),
      ]);
      set({ diagnostics: next, instructionStatus: instructions });
      setCoreSandboxSettings(sandboxSettings);
    } catch (err) {
      const message = messageOf(err, "Diagnostics failed");
      set({ error: message });
      setStatus(message);
    } finally {
      set({ loading: false });
    }
  },

  async createBundle(config, _selectedId, setStatus) {
    if (!config || get().supportBundlePending) return;
    set({ supportBundlePending: "create" });
    try {
      const bundle = await createDiagnosticsBundle(config);
      set({ supportBundle: bundle });
      setStatus("Diagnostics bundle created");
    } catch (err) {
      setStatus(messageOf(err, "Diagnostics bundle failed"));
    } finally {
      set({ supportBundlePending: null });
    }
  },

  async downloadBundle(config, setStatus, saveBundle) {
    const current = get().supportBundle;
    if (!config || !current || get().supportBundlePending) return;
    set({ supportBundlePending: "download" });
    try {
      const bundle = await downloadDiagnosticsBundle(config, current.bundleId);
      saveBundle(bundle);
      setStatus("Diagnostics bundle downloaded");
    } catch (err) {
      setStatus(messageOf(err, "Diagnostics bundle download failed"));
    } finally {
      set({ supportBundlePending: null });
    }
  },
}));
