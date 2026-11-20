// Local speech-model lanes: per-backend install status, the download / prepare state machine for the
// lanes the settings screen owns, a second "alternate" lane (a larger multi-language model that must be
// installed, prepared, then pinned to a language), and the selection rules. Commands, event names, setting
// keys, backend ids and all wording are inputs; this file holds no product name and no copy.
// Moved from HeardRight `components/settings/speech/SpeechRecognitionSettingsImpl.tsx`.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

export type ModelPhase = "" | "downloading" | "extracting" | "preparing";

export interface ModelDownloadState {
  active: boolean;
  phase: ModelPhase;
  pct: number;
  error: string;
}

export interface AltModelDownloadState {
  active: boolean;
  phase: string;
  pct: number;
  error: string;
}

export interface LocalModelStatusLike {
  installed: boolean;
  replacement_required?: boolean;
}

export interface AltModelStatus {
  installed: boolean;
  prepared: boolean;
}

export interface LocalModelEvent<B extends string> {
  backend: B;
  phase: ModelPhase | "installed";
  received: number;
  total: number;
}

export interface AltModelEvent {
  phase: string;
  received: number;
  total: number;
}

/** The commands and events the host binds. Every member is a thin wrapper over one host call. */
export interface SpeechModelsPort<B extends string, S extends LocalModelStatusLike> {
  localStatus(backend: B): Promise<S>;
  altStatus(): Promise<AltModelStatus>;
  downloadLocal(backend: B): Promise<S>;
  downloadAlt(): Promise<void>;
  /** Runs after a verified local install (HeardRight: `register_hotkeys_now`). Failures are ignored. */
  afterLocalInstall?(): Promise<void>;
  /** Warms the selected engine (HeardRight: `prewarm_engine`). */
  prewarm(): Promise<void>;
  onLocalProgress(cb: (event: LocalModelEvent<B>) => void): Promise<() => void>;
  onAltProgress(cb: (event: AltModelEvent) => void): Promise<() => void>;
}

/** Progress of the one download another store owns (HeardRight: the shared model-update store). */
export interface SharedModelProgress {
  busy: boolean;
  /** `verifying` shows as extracting, `preparing` as preparing, anything else as downloading. */
  phase: string;
  pct: number;
}

export interface SpeechModelsOptions<B extends string> {
  /** Every local backend that has a status row, in display order. */
  backends: readonly B[];
  /** The only local lane that can be active (HeardRight: `parakeet-tdt`). */
  primaryBackend: B;
  /** A retired backend id: while it is the saved backend no local card is active and the replacement is offered. */
  retiredBackend?: B;
  /** The backend whose download another store owns; its events only refresh status. */
  sharedBackend: B;
  shared: SharedModelProgress;
  startSharedDownload(): void;
  /** Saved values. */
  asrBackend: string;
  language: string;
  loaded: boolean;
  pro: boolean;
  patch(key: string, value: unknown): Promise<unknown> | unknown;
  /** Setting keys and values the machine writes. */
  keys: { backend: string; language: string };
  values: { altBackend: string; autoLanguage: string; altAutoLanguage: string };
}

const IDLE: ModelDownloadState = { active: false, phase: "", pct: 0, error: "" };
const ALT_IDLE: AltModelDownloadState = { active: false, phase: "", pct: 0, error: "" };

export interface SpeechModels<B extends string, S extends LocalModelStatusLike> {
  localStatuses: Partial<Record<B, S>>;
  /** Per-backend download state; the shared backend's entry mirrors the shared store. */
  localDl: Record<B, ModelDownloadState>;
  altDl: AltModelDownloadState;
  altStatus: AltModelStatus;
  /** Seconds since any download or prepare began (drives a synthetic prepare percentage). */
  elapsed: number;
  /** The active local lane, or null on a retired install that has not migrated. */
  activeLocal: B | null;
  /** True while a retired backend is saved or the shared backend reports a required replacement. */
  replacementPending: boolean;
  sharedInstalled: boolean;
  downloadLocal(backend: B): Promise<void>;
  selectLocal(backend: B): Promise<void>;
  dismissLocalError(backend: B): void;
  downloadAlt(): Promise<void>;
  pickAltTarget(code: string): void;
  dismissAltError(): void;
}

export function useSpeechModels<B extends string, S extends LocalModelStatusLike>(
  port: SpeechModelsPort<B, S>,
  options: SpeechModelsOptions<B>,
): SpeechModels<B, S> {
  const {
    backends,
    primaryBackend,
    retiredBackend,
    sharedBackend,
    shared,
    startSharedDownload,
    asrBackend,
    language,
    loaded,
    pro,
    patch,
    keys,
    values,
  } = options;
  const value = language || values.autoLanguage;
  const portRef = useRef(port);
  portRef.current = port;

  // A retired backend still saved (until the replacement finishes downloading) has NO active local card:
  // the primary card offers its Download button instead, and the backend flips itself on verified install.
  const replacementFromRetired = retiredBackend !== undefined && asrBackend === retiredBackend;
  const activeLocal: B | null = replacementFromRetired ? null : primaryBackend;

  const [altStatus, setAltStatus] = useState<AltModelStatus>({ installed: false, prepared: false });
  const [localStatuses, setLocalStatuses] = useState<Partial<Record<B, S>>>({});
  const [ownDl, setOwnDl] = useState<Record<string, ModelDownloadState>>(() =>
    Object.fromEntries(backends.map((b) => [b, IDLE])),
  );
  const [altDl, setAltDl] = useState<AltModelDownloadState>(ALT_IDLE);
  const [elapsed, setElapsed] = useState(0);

  const sharedDl: ModelDownloadState = {
    active: shared.busy,
    phase:
      shared.phase === "verifying"
        ? "extracting"
        : shared.phase === "preparing"
          ? "preparing"
          : "downloading",
    pct: shared.pct,
    error: "",
  };
  const localDl = useMemo(() => {
    const merged: Record<string, ModelDownloadState> = { ...ownDl };
    merged[sharedBackend] = sharedDl;
    return merged as Record<B, ModelDownloadState>;
    // biome-ignore lint/correctness/useExhaustiveDependencies: sharedDl is rebuilt from its three scalar inputs
  }, [ownDl, sharedBackend, shared.busy, shared.phase, shared.pct]);

  const localBusy = useMemo(() => Object.values<ModelDownloadState>(ownDl).some((d) => d.active), [ownDl]);

  const setOne = useCallback((backend: string, next: ModelDownloadState) => {
    setOwnDl((d) => ({ ...d, [backend]: next }));
  }, []);

  const refreshLocalStatuses = useCallback(async () => {
    const entries = await Promise.all(
      backends.map(async (backend) => [backend, await portRef.current.localStatus(backend)] as const),
    );
    setLocalStatuses(Object.fromEntries(entries) as Partial<Record<B, S>>);
  }, [backends]);

  useEffect(() => {
    void portRef.current
      .altStatus()
      .then(setAltStatus)
      .catch(() => {});
    void refreshLocalStatuses().catch(() => {});
  }, [refreshLocalStatuses]);

  useEffect(() => {
    if (!localBusy && !altDl.active) {
      setElapsed(0);
      return;
    }
    const start = Date.now();
    const id = setInterval(() => setElapsed(Math.floor((Date.now() - start) / 1000)), 1000);
    return () => clearInterval(id);
  }, [localBusy, altDl.active]);

  useEffect(() => {
    const un = portRef.current.onLocalProgress((event) => {
      const { backend, phase, received, total } = event;
      if (!backends.includes(backend)) return;
      // The shared store owns every download of the shared backend.
      if (backend === sharedBackend) {
        if (phase === "installed") void refreshLocalStatuses().catch(() => {});
        return;
      }
      if (phase === "installed") {
        setOne(backend, IDLE);
        void refreshLocalStatuses().catch(() => {});
        return;
      }
      const pct = phase === "downloading" && total > 0 ? Math.round((received / total) * 100) : 0;
      setOne(backend, { active: true, phase, pct, error: "" });
    });
    return () => void un.then((f) => f()).catch(() => undefined);
  }, [backends, sharedBackend, refreshLocalStatuses, setOne]);

  useEffect(() => {
    const un = portRef.current.onAltProgress((e) => {
      const { phase, received, total } = e;
      if (phase === "downloading") {
        const pct = total > 0 ? Math.round((received / total) * 100) : 0;
        setAltDl((d) => ({ ...d, active: true, phase, pct }));
      } else if (phase === "extracting" || phase === "preparing") {
        setAltDl((d) => ({ ...d, active: true, phase }));
      } else if (phase === "installed") {
        setAltStatus({ installed: true, prepared: true });
        setAltDl(ALT_IDLE);
      }
    });
    return () => void un.then((f) => f()).catch(() => undefined);
  }, []);

  const downloadLocal = useCallback(
    async (backend: B) => {
      if (backend === sharedBackend) {
        // One shared flow (and one download) for the modal and Settings.
        startSharedDownload();
        return;
      }
      if (ownDl[backend]?.active) return;
      setOne(backend, { active: true, phase: "downloading", pct: 0, error: "" });
      try {
        const status = await portRef.current.downloadLocal(backend);
        setLocalStatuses((s) => ({ ...s, [backend]: status }));
        if (status.installed) {
          await portRef.current.afterLocalInstall?.().catch(() => undefined);
        }
        setOne(backend, IDLE);
      } catch (e) {
        setOne(backend, { active: false, phase: "", pct: 0, error: String(e) });
      }
    },
    [ownDl, setOne, sharedBackend, startSharedDownload],
  );

  const selectLocal = useCallback(
    async (backend: B) => {
      const status = localStatuses[backend];
      if (!loaded || !status?.installed || localDl[backend]?.active) return;
      await patch(keys.backend, backend);
      if (value !== values.autoLanguage) await patch(keys.language, values.autoLanguage);
      setOne(backend, { active: true, phase: "preparing", pct: 0, error: "" });
      try {
        await portRef.current.prewarm();
        setOne(backend, IDLE);
      } catch (e) {
        // A bare try/finally with no catch would clear the "preparing" spinner (so the UI looks done)
        // while a prewarm failure vanishes as a silent unhandled promise rejection, leaving the user
        // unaware the model never actually finished preparing.
        setOne(backend, { active: false, phase: "", pct: 0, error: String(e) });
      }
    },
    [keys.backend, keys.language, loaded, localDl, localStatuses, patch, setOne, value, values.autoLanguage],
  );

  const pickAltTarget = useCallback(
    (code: string) => {
      if (!pro || !altStatus.prepared) return;
      if (asrBackend !== values.altBackend) void patch(keys.backend, values.altBackend);
      void patch(keys.language, code);
    },
    [altStatus.prepared, asrBackend, keys.backend, keys.language, patch, pro, values.altBackend],
  );

  const downloadAlt = useCallback(async () => {
    if (altDl.active) return;
    setAltDl({ active: true, phase: "downloading", pct: 0, error: "" });
    try {
      await portRef.current.downloadAlt();
      setAltStatus({ installed: true, prepared: true });
      setAltDl(ALT_IDLE);
      // Switching to the alternate lane is valid from any prior lane, including a retired install that
      // never migrated (activeLocal === null).
      if (asrBackend !== values.altBackend) {
        void patch(keys.backend, values.altBackend);
        void patch(keys.language, values.altAutoLanguage);
      }
    } catch (e) {
      setAltDl({ active: false, phase: "", pct: 0, error: String(e) });
    }
  }, [altDl.active, asrBackend, keys.backend, keys.language, patch, values.altAutoLanguage, values.altBackend]);

  const sharedStatus = localStatuses[sharedBackend];
  const sharedInstalled = sharedStatus?.installed === true;
  const replacementPending = replacementFromRetired || sharedStatus?.replacement_required === true;

  // Rust persists the replacement after a verified install, but this mount may still hold the
  // pre-download snapshot. Reconcile it as soon as the status refresh proves the shared backend exists,
  // including users already stuck on the old "Ready" state from an earlier attempt.
  useEffect(() => {
    if (loaded && sharedInstalled && replacementFromRetired) {
      void patch(keys.backend, sharedBackend);
    }
  }, [keys.backend, loaded, patch, replacementFromRetired, sharedBackend, sharedInstalled]);

  // When the shared download ends (installed or failed), re-read model status.
  const wasBusy = useRef(false);
  useEffect(() => {
    if (wasBusy.current && !shared.busy) void refreshLocalStatuses().catch(() => {});
    wasBusy.current = shared.busy;
  }, [shared.busy, refreshLocalStatuses]);

  return {
    localStatuses,
    localDl,
    altDl,
    altStatus,
    elapsed,
    activeLocal,
    replacementPending,
    sharedInstalled,
    downloadLocal,
    selectLocal,
    dismissLocalError: (backend) => setOwnDl((d) => ({ ...d, [backend]: { ...d[backend], error: "" } })),
    downloadAlt,
    pickAltTarget,
    dismissAltError: () => setAltDl((d) => ({ ...d, error: "" })),
  };
}
