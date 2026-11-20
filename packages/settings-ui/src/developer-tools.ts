// Developer-tool state for a settings section: an experimental flag with optimistic toggle and rollback, a
// confirm-then-run cache rebuild, diagnostics send and export with a per-export opt-in that resets, whether
// captured audio exists on disk, a route-test open flag that closes with developer mode, and the folder-open
// notice. Commands and every message are inputs. Imports only `react`.
// Moved from HeardRight `components/settings/DeveloperSettings.tsx`.
import { useCallback, useEffect, useState } from "react";

export interface ExperimentalFlagStatus {
  enabled: boolean;
}

export interface DeveloperToolsPort<F extends ExperimentalFlagStatus> {
  /** Reads the experimental flag; a failure falls back to `fallbackFlag`. */
  readFlag(): Promise<F>;
  setFlag(enabled: boolean): Promise<void>;
  capturedAudioPresent(): Promise<boolean>;
  rebuildCache(): Promise<void>;
  /** Uploads redacted diagnostics; resolves the report id. */
  sendDiagnostics(): Promise<string>;
  /** Writes a diagnostics archive; resolves where it went. */
  exportDiagnostics(includeNativeDumps: boolean): Promise<string>;
  openCapturesFolder(): Promise<unknown>;
}

export interface DeveloperToolsMessages {
  sent(reportId: string): string;
  sendFailed(detail: string): string;
  exported(archive: string): string;
  exportFailed(detail: string): string;
}

export interface UseDeveloperToolsOptions<F extends ExperimentalFlagStatus> {
  developerMode: boolean;
  captureAudio: boolean;
  /** The flag status shown when the read fails. */
  fallbackFlag: F;
  messages: DeveloperToolsMessages;
}

export type DiagnosticsStatus = { kind: "success" | "error"; message: string };
export type CacheNotice = "" | "success" | "error";

export interface DeveloperTools<F extends ExperimentalFlagStatus> {
  flag: F | null;
  /** The toggle's error text (a failed set), cleared by the next success. */
  notice: string;
  toggleFlag(next: boolean): Promise<void>;
  cacheBusy: boolean;
  cacheConfirm: boolean;
  setCacheConfirm(open: boolean): void;
  cacheNotice: CacheNotice;
  rebuildCache(): Promise<void>;
  diagnosticsBusy: boolean;
  diagnosticsStatus: DiagnosticsStatus | undefined;
  /** Per export, never remembered: crash dumps can hold memory contents, so each export starts off. */
  includeCrashDumps: boolean;
  setIncludeCrashDumps(next: boolean): void;
  sendDiagnostics(): Promise<void>;
  exportDiagnostics(): Promise<void>;
  capturedAudioPresent: boolean;
  captureNotice: string;
  openCapturesFolder(): void;
  routeTestOpen: boolean;
  setRouteTestOpen(next: boolean | ((open: boolean) => boolean)): void;
  routeTestError: string;
  setRouteTestError(message: string): void;
}

export function useDeveloperTools<F extends ExperimentalFlagStatus>(
  port: DeveloperToolsPort<F>,
  options: UseDeveloperToolsOptions<F>,
): DeveloperTools<F> {
  const { developerMode, captureAudio, fallbackFlag, messages } = options;
  const [flag, setFlag] = useState<F | null>(null);
  const [notice, setNotice] = useState("");
  const [cacheBusy, setCacheBusy] = useState(false);
  const [cacheConfirm, setCacheConfirm] = useState(false);
  const [cacheNotice, setCacheNotice] = useState<CacheNotice>("");
  const [diagnosticsBusy, setDiagnosticsBusy] = useState(false);
  const [includeCrashDumps, setIncludeCrashDumps] = useState(false);
  const [diagnosticsStatus, setDiagnosticsStatus] = useState<DiagnosticsStatus | undefined>();
  const [captureNotice, setCaptureNotice] = useState("");
  const [capturedAudioPresent, setCapturedAudioPresent] = useState(false);
  const [routeTestOpen, setRouteTestOpen] = useState(false);
  const [routeTestError, setRouteTestError] = useState("");

  useEffect(() => {
    let alive = true;
    port
      .readFlag()
      .then((v) => alive && setFlag(v))
      .catch(() => alive && setFlag(fallbackFlag));
    return () => {
      alive = false;
    };
    // biome-ignore lint/correctness/useExhaustiveDependencies: read once on mount
  }, []);

  useEffect(() => {
    if (!developerMode || captureAudio) return;
    let alive = true;
    port
      .capturedAudioPresent()
      .then((present) => alive && setCapturedAudioPresent(present))
      .catch(() => alive && setCapturedAudioPresent(false));
    return () => {
      alive = false;
    };
    // biome-ignore lint/correctness/useExhaustiveDependencies: the port is stable for the screen's life
  }, [developerMode, captureAudio]);

  useEffect(() => {
    if (!developerMode) {
      setRouteTestOpen(false);
      setIncludeCrashDumps(false);
    }
  }, [developerMode]);

  const toggleFlag = useCallback(
    async (next: boolean) => {
      const previous = flag;
      if (!flag) return;
      setFlag({ ...flag, enabled: next });
      try {
        await port.setFlag(next);
        setNotice("");
      } catch (error) {
        setFlag(previous);
        setNotice(String(error));
      }
    },
    [flag, port],
  );

  const rebuildCache = useCallback(async () => {
    setCacheBusy(true);
    setCacheConfirm(false);
    setCacheNotice("");
    try {
      await port.rebuildCache();
      setCacheNotice("success");
    } catch {
      setCacheNotice("error");
    } finally {
      setCacheBusy(false);
    }
  }, [port]);

  const sendDiagnostics = useCallback(async () => {
    setDiagnosticsBusy(true);
    setDiagnosticsStatus(undefined);
    try {
      const reportId = await port.sendDiagnostics();
      setDiagnosticsStatus({ kind: "success", message: messages.sent(reportId) });
    } catch (error) {
      setDiagnosticsStatus({ kind: "error", message: messages.sendFailed(String(error)) });
    } finally {
      setDiagnosticsBusy(false);
    }
  }, [messages, port]);

  const exportDiagnostics = useCallback(async () => {
    setDiagnosticsBusy(true);
    setDiagnosticsStatus(undefined);
    try {
      const archive = await port.exportDiagnostics(includeCrashDumps);
      setDiagnosticsStatus({ kind: "success", message: messages.exported(archive) });
    } catch (error) {
      setDiagnosticsStatus({ kind: "error", message: messages.exportFailed(String(error)) });
    } finally {
      setDiagnosticsBusy(false);
      // The opt-in covers this one export. Whatever happened, the next export starts off again.
      setIncludeCrashDumps(false);
    }
  }, [includeCrashDumps, messages, port]);

  const openCapturesFolder = useCallback(() => {
    setCaptureNotice("");
    void port.openCapturesFolder().catch((error) => setCaptureNotice(String(error)));
  }, [port]);

  return {
    flag,
    notice,
    toggleFlag,
    cacheBusy,
    cacheConfirm,
    setCacheConfirm,
    cacheNotice,
    rebuildCache,
    diagnosticsBusy,
    diagnosticsStatus,
    includeCrashDumps,
    setIncludeCrashDumps,
    sendDiagnostics,
    exportDiagnostics,
    capturedAudioPresent,
    captureNotice,
    openCapturesFolder,
    routeTestOpen,
    setRouteTestOpen,
    routeTestError,
    setRouteTestError,
  };
}
