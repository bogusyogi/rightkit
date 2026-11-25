// Offline file transcription state: pick or drop a file, run one transcription at a time (a newer run
// supersedes an older one), show the text or the subtitles, copy with a short flash, save as a file, and track
// a native drag-over highlight. The commands, the dialogs and the drag-drop event are the host's port.
// Imports only `react`. Moved from HeardRight `pages/Transcribe.tsx`.
import { useCallback, useEffect, useRef, useState } from "react";

export interface FileTranscriptLike {
  text: string;
  srt: string;
  vtt: string;
}

export type NativeDragDrop =
  | { type: "enter"; paths: string[] }
  | { type: "over" }
  | { type: "drop"; paths: string[] }
  | { type: "leave" };

export interface FileTranscriptionPort<R extends FileTranscriptLike> {
  transcribe(path: string): Promise<R>;
  /** Subscribes to native drag-drop; resolves to the unsubscribe. */
  onDragDrop(cb: (event: NativeDragDrop) => void): Promise<() => void>;
  copyText(text: string): Promise<void>;
  /** Opens the file picker; resolves the chosen path or a falsy value when cancelled. */
  pickFile(): Promise<string | null | undefined>;
  /** Opens the save dialog for a default file name and extension; resolves the path or a falsy value. */
  pickSavePath(defaultName: string, ext: string): Promise<string | null | undefined>;
  writeText(path: string, contents: string): Promise<void>;
  /**
   * Opens the system's file-access settings, for a host that recovers from a permission error
   * (`UseFileTranscriptionOptions.permissionErrorMarker`). `removable` is true when the file is on a
   * removable volume (`removablePathPrefixes`). Optional: without it `openPermissionSettings` does nothing.
   */
  openPermissionSettings?(removable: boolean): Promise<void>;
}

/** One entry of a save dialog's file-type filter. */
export interface SaveDialogFilter {
  name: string;
  extensions: string[];
}

/**
 * The save dialog filter for one extension: its upper-case name and the extension, for example
 * `[{ name: "TXT", extensions: ["txt"] }]`. For a host's `pickSavePath` port.
 */
export function saveDialogFilters(ext: string): SaveDialogFilter[] {
  return [{ name: ext.toUpperCase(), extensions: [ext] }];
}

export interface UseFileTranscriptionOptions {
  /** How long a "copied" flash lasts. Default 1500 ms. */
  copiedMs?: number;
  /** The error shown when a copy fails; receives the failure text. */
  copyFailedMessage(detail: string): string;
  /** The default stem of a saved file when no file was chosen. Default `transcript`. */
  fallbackStem?: string;
  /**
   * Text an error contains when the system denied access to the file (for example `E_PERMISSION`). When set,
   * `permissionDenied` is true while the current error contains it. Default unset: `permissionDenied` stays false.
   */
  permissionErrorMarker?: string;
  /** Whether the permission recovery applies on this platform (for example a macOS check). Default always. */
  permissionRecoveryApplies?: () => boolean;
  /** Path prefixes that mean a removable volume (for example `/Volumes/`), passed on as `removable`. Default none. */
  removablePathPrefixes?: string[];
}

export type TranscriptView = "original" | "segmented";

export interface FileTranscription<R extends FileTranscriptLike> {
  path: string;
  busy: boolean;
  error: string;
  setError(message: string): void;
  result: R | null;
  view: TranscriptView;
  setView(view: TranscriptView): void;
  dragging: boolean;
  /** The label of the last copy, until the flash ends. */
  copied: string;
  selectedName: string;
  hasSrt: boolean;
  /** The error is a permission denial the host marked and recovery applies here: show a button for `openPermissionSettings`. */
  permissionDenied: boolean;
  /** Opens the system's file-access settings through the port; errors are swallowed. No-op without the port method. */
  openPermissionSettings(): Promise<void>;
  /** A transcript exists and nothing is running: the drop zone shrinks to a slim bar. */
  collapsed: boolean;
  run(filePath: string): Promise<void>;
  choose(): Promise<void>;
  copy(label: string, text: string): Promise<void>;
  saveAs(ext: string, contents: string): Promise<void>;
}

export function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function useFileTranscription<R extends FileTranscriptLike>(
  port: FileTranscriptionPort<R>,
  options: UseFileTranscriptionOptions,
): FileTranscription<R> {
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<R | null>(null);
  const [view, setView] = useState<TranscriptView>("original");
  const [dragging, setDragging] = useState(false);
  const [copied, setCopied] = useState("");
  const runId = useRef(0);
  const copiedTimer = useRef<number | undefined>(undefined);
  const portRef = useRef(port);
  portRef.current = port;
  const optionsRef = useRef(options);
  optionsRef.current = options;

  // Clear the pending copied-flash timer on unmount so it can't write state on an unmounted component.
  useEffect(
    () => () => {
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
    },
    [],
  );

  const run = useCallback(async (filePath: string) => {
    if (!filePath) return;
    const id = ++runId.current;
    setPath(filePath);
    setResult(null);
    setError("");
    setBusy(true);
    try {
      const out = await portRef.current.transcribe(filePath);
      if (runId.current === id) {
        setResult(out);
        setView("original");
      }
    } catch (e) {
      if (runId.current === id) setError(String(e));
    } finally {
      if (runId.current === id) setBusy(false);
    }
  }, []);

  const choose = useCallback(async () => {
    const picked = await portRef.current.pickFile();
    if (picked) void run(picked);
  }, [run]);

  // Native file drag-drop (paths come from the webview, not an <input>).
  useEffect(() => {
    const unlistenPromise = portRef.current.onDragDrop((p) => {
      if (p.type === "over" || p.type === "enter") setDragging(true);
      else if (p.type === "leave") setDragging(false);
      else if (p.type === "drop") {
        setDragging(false);
        const first = p.paths?.[0];
        if (first) void run(first);
      }
    });
    return () => {
      void unlistenPromise.then((stop) => stop()).catch(() => undefined);
    };
  }, [run]);

  const copy = useCallback(async (label: string, text: string) => {
    try {
      await portRef.current.copyText(text);
      setCopied(label);
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(""), optionsRef.current.copiedMs ?? 1500);
    } catch (nextError) {
      setError(optionsRef.current.copyFailedMessage(String(nextError)));
    }
  }, []);

  const saveAs = useCallback(
    async (ext: string, contents: string) => {
      const stem = path ? baseName(path).replace(/\.[^.]+$/, "") : (optionsRef.current.fallbackStem ?? "transcript");
      const dest = await portRef.current.pickSavePath(`${stem}.${ext}`, ext);
      if (dest) await portRef.current.writeText(dest, contents);
    },
    [path],
  );

  const marker = options.permissionErrorMarker;
  const permissionDenied =
    !!marker && error.includes(marker) && (options.permissionRecoveryApplies?.() ?? true);

  const openPermissionSettings = useCallback(async () => {
    const open = portRef.current.openPermissionSettings;
    if (!open) return;
    const prefixes = optionsRef.current.removablePathPrefixes ?? [];
    try {
      await open.call(portRef.current, prefixes.some((prefix) => path.startsWith(prefix)));
    } catch {
      // The settings pane is a convenience; a failure leaves the error text in place.
    }
  }, [path]);

  return {
    path,
    busy,
    error,
    setError,
    result,
    view,
    setView,
    dragging,
    copied,
    selectedName: path ? baseName(path) : "",
    hasSrt: !!result?.srt?.trim(),
    permissionDenied,
    openPermissionSettings,
    collapsed: !!result && !busy,
    run,
    choose,
    copy,
    saveAs,
  };
}
