// BrowserWorkspace state — owned by the D3 workspace fragment.
// Local store for the in-flight summary, console snapshot, screenshot, and
// annotation list. The desktop shell wires this into the pane; the daemon
// remains the source of truth.

import { create } from "zustand";
import type {
  BrowserWorkspaceSummary,
  ConsoleLine,
  DevServer,
  ScreenshotEnvelope,
  StoredAnnotation,
} from "../types/browserWorkspace.js";

export interface BrowserWorkspaceState {
  summary: BrowserWorkspaceSummary | null;
  console: ConsoleLine[];
  errors: ConsoleLine[];
  devServers: DevServer[];
  networkEntries: unknown[];
  actionLogLen: number;
  screenshot: ScreenshotEnvelope | null;
  annotations: StoredAnnotation[];
  loading: boolean;
  lastError: string | null;
  setSummary: (s: BrowserWorkspaceSummary) => void;
  setConsole: (c: ConsoleLine[]) => void;
  setErrors: (e: ConsoleLine[]) => void;
  setDevServers: (servers: DevServer[]) => void;
  setNetworkEntries: (entries: unknown[]) => void;
  setActionLogLen: (count: number) => void;
  setScreenshot: (s: ScreenshotEnvelope | null) => void;
  setAnnotations: (a: StoredAnnotation[]) => void;
  upsertAnnotation: (a: StoredAnnotation) => void;
  setLoading: (l: boolean) => void;
  setError: (e: string | null) => void;
  reset: () => void;
}

const initial = {
  summary: null as BrowserWorkspaceSummary | null,
  console: [] as ConsoleLine[],
  errors: [] as ConsoleLine[],
  devServers: [] as DevServer[],
  networkEntries: [] as unknown[],
  actionLogLen: 0,
  screenshot: null as ScreenshotEnvelope | null,
  annotations: [] as StoredAnnotation[],
  loading: false,
  lastError: null as string | null,
};

export const useBrowserWorkspaceStore = create<BrowserWorkspaceState>((set) => ({
  ...initial,
  setSummary: (s) => set({ summary: s }),
  setConsole: (c) => set({ console: c }),
  setErrors: (e) => set({ errors: e }),
  setDevServers: (devServers) => set({ devServers }),
  setNetworkEntries: (networkEntries) => set({ networkEntries }),
  setActionLogLen: (actionLogLen) => set({ actionLogLen }),
  setScreenshot: (s) => set({ screenshot: s }),
  setAnnotations: (a) => set({ annotations: a }),
  upsertAnnotation: (a) =>
    set((state) => {
      const idx = state.annotations.findIndex((x) => x.id === a.id);
      if (idx === -1) return { annotations: [...state.annotations, a] };
      const next = state.annotations.slice();
      next[idx] = a;
      return { annotations: next };
    }),
  setLoading: (l) => set({ loading: l }),
  setError: (e) => set({ lastError: e }),
  reset: () => set(initial),
}));
