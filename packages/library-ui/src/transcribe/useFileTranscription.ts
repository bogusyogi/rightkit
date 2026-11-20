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
}

export interface UseFileTranscriptionOptions {
  /** How long a "copied" flash lasts. Default 1500 ms. */
  copiedMs?: number;
  /** The error shown when a copy fails; receives the failure text. */
  copyFailedMessage(detail: string): string;
  /** The default stem of a saved file when no file was chosen. Default `transcript`. */
  fallbackStem?: string;
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
    collapsed: !!result && !busy,
    run,
    choose,
    copy,
    saveAs,
  };
}
