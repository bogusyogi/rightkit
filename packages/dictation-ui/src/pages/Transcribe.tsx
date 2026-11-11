// Transcribe a file - offline file transcription (free tier). Pick or drop an
// audio/video file; the host transcribes it (`DictationClient.transcribeFile`),
// returning text + SRT subtitles.
import { useCallback, useEffect, useRef, useState } from "react";
import { FileAudio, DownloadSimple } from "@phosphor-icons/react";
import { getPlatform } from "@rightkit/platform-ui";
import ErrorBanner from "../components/ErrorBanner.js";
import { dictationClient, type FileTranscript } from "../host.js";
import { cx, ui } from "../lib/ui.js";

function baseName(path: string): string {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] || path;
}

export function TranscribeSurface({
  compact = false,
  isPro = false,
}: {
  compact?: boolean;
  isPro?: boolean | null;
}) {
  const [path, setPath] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>("");
  const [result, setResult] = useState<FileTranscript | null>(null);
  const [view, setView] = useState<"original" | "segmented">("original");
  const [dragging, setDragging] = useState(false);
  const [copied, setCopied] = useState<string>("");
  const runId = useRef(0);
  const copiedTimer = useRef<number | undefined>(undefined);

  // Clear the pending copied-flash timer on unmount so it can't write state
  // on an unmounted component.
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
      const out = await dictationClient().transcribeFile(filePath);
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

  async function choose() {
    const picked = await dictationClient().pickMediaFile();
    if (picked) void run(picked);
  }

  // Native file drag-drop (paths come from the webview, not an <input>).
  useEffect(() => {
    const unlistenPromise = dictationClient().on("nativeDragDrop", (p) => {
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

  async function copy(label: string, text: string) {
    try {
      await dictationClient().copyText(text);
      setCopied(label);
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(""), 1500);
    } catch (nextError) {
      setError(`Clipboard copy failed: ${String(nextError)}`);
    }
  }

  async function saveAs(ext: "txt" | "srt", contents: string) {
    const stem = path ? baseName(path).replace(/\.[^.]+$/, "") : "transcript";
    await dictationClient().saveTextFile(`${stem}.${ext}`, contents);
  }

  const hasSrt = !!result?.srt?.trim();
  const selectedName = path ? baseName(path) : "";

  // Once a transcript exists, collapse the big dropzone to a slim bar so the
  // result gets the vertical room (and the modal never grows past the screen).
  const collapsed = !!result && !busy;

  return (
    <div className={cx(ui.pageScroll, "flex flex-col")}>
      {isPro === false && (
        <div className="mb-[var(--space-4)] flex shrink-0 flex-wrap items-center gap-[var(--space-1)] text-[length:var(--size-sm)] text-[var(--muted)]">
          <span>Can transcribe files up to 10 minutes. For longer audio,</span>
          <button
            type="button"
            className="cursor-pointer border-0 bg-transparent p-0 [font:inherit] font-semibold text-[var(--accent-cool)] underline-offset-4 hover:underline focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
            onClick={() => void dictationClient().openPricing()}
          >
            buy a Pro license
          </button>
          <span>.</span>
        </div>
      )}

      {error && <ErrorBanner message={error} onDismiss={() => setError("")} />}
      <span className="sr-only" role="status" aria-live="polite">
        {busy ? "Transcribing selected file" : result ? "Transcription complete" : ""}
      </span>
      {getPlatform() === "macos" && error.includes("E_PERMISSION") && (
        <button
          type="button"
          className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact, "mb-[var(--space-3)] self-start")}
          onClick={() =>
            void dictationClient().openFilesSettings?.(path.startsWith("/Volumes/")).catch(() => undefined)
          }
        >
          Open Privacy Settings
        </button>
      )}

      {collapsed ? (
        <button
          type="button"
          onClick={choose}
          aria-label="Choose another file to transcribe"
          className={cx(
            ui.card,
            "group flex w-full shrink-0 cursor-pointer items-center justify-between gap-[var(--space-3)] border border-[var(--surface-3)] px-[var(--space-4)] py-[var(--space-2)] text-left transition-[background-color,border-color] duration-150 ease-[var(--ease-out-strong)]",
            "hover:border-[var(--accent)] hover:bg-[var(--surface-hover)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]",
          )}
        >
          <span className="flex min-w-0 items-center gap-[var(--space-2)]">
            <span className={cx(ui.iconBadge, "h-7 w-7")}>
              <FileAudio className="h-4 w-4" aria-hidden="true" />
            </span>
            <span className="truncate text-[length:var(--size-sm)] font-semibold text-[var(--paper)]">
              {selectedName}
            </span>
          </span>
          <span
            className={cx(
              ui.button,
              ui.buttonSecondary,
              "pointer-events-none min-h-8 shrink-0 px-3 text-[length:var(--size-sm)]",
            )}
          >
            Choose another
          </span>
        </button>
      ) : (
        <button
          type="button"
          onClick={choose}
          disabled={busy}
          aria-label={
            busy ? "Transcribing selected file" : "Choose an audio or video file to transcribe"
          }
          className={cx(
            ui.card,
            "group flex w-full shrink-0 cursor-pointer flex-col items-center justify-center gap-[var(--space-3)] border-2 border-dashed text-center transition-[background-color,border-color,box-shadow] duration-150 ease-[var(--ease-out-strong)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]",
            compact ? "min-h-[124px] py-[var(--space-4)]" : "min-h-[150px] py-[var(--space-6)]",
            dragging
              ? "border-[var(--accent)] bg-[var(--surface-hover)]"
              : "border-[var(--surface-3)]",
            busy && "cursor-progress opacity-70",
          )}
        >
          <span
            className={cx(
              ui.iconBadge,
              "h-11 w-11 transition-transform duration-150 ease-[var(--ease-out-strong)] group-hover:scale-105",
            )}
          >
            <DownloadSimple className="h-5 w-5" aria-hidden="true" />
          </span>
          <span className={cx(ui.cardTitle, "mb-0")}>
            {busy ? "Transcribing…" : "Choose a file or drop it here"}
          </span>
          {!busy && (
            <span
              className={cx(
                ui.button,
                "pointer-events-none min-h-8 px-3 text-[length:var(--size-sm)]",
              )}
            >
              Choose file
            </span>
          )}
          <span className={ui.mutedSm}>
            {selectedName || "MP3, M4A, WAV, FLAC, MP4, MOV and more"}
          </span>
          {busy && (
            <span className="mt-[var(--space-1)] h-[3px] w-full max-w-[240px] overflow-hidden rounded-full bg-[rgba(255,255,255,0.08)]">
              <span className="block h-full w-1/2 rounded-full bg-[var(--accent)] [animation:hrProgress_1.1s_var(--ease-out-strong)_infinite]" />
            </span>
          )}
        </button>
      )}

      {result && (
        <section className={cx(ui.card, "mt-[var(--space-4)] flex min-h-0 flex-1 flex-col")}>
          <div className="mb-[var(--space-3)] flex shrink-0 items-center justify-between gap-[var(--space-2)]">
            <div className={ui.inlineControls}>
              <button
                type="button"
                onClick={() => setView("original")}
                className={cx(
                  ui.button,
                  ui.buttonCompact,
                  view === "original" ? "" : ui.buttonSecondary,
                )}
              >
                Original
              </button>
              <button
                type="button"
                onClick={() => setView("segmented")}
                disabled={!hasSrt}
                className={cx(
                  ui.button,
                  ui.buttonCompact,
                  view === "segmented" ? "" : ui.buttonSecondary,
                  !hasSrt && "opacity-40",
                )}
              >
                Segmented
              </button>
            </div>
            <div className={ui.inlineControls}>
              {view === "original" ? (
                <>
                  <button
                    type="button"
                    onClick={() => copy("text", result.text)}
                    className={cx(ui.button, ui.buttonCompact, ui.buttonSecondary)}
                  >
                    {copied === "text" ? "Copied" : "Copy"}
                  </button>
                  <button
                    type="button"
                    onClick={() => saveAs("txt", result.text)}
                    className={cx(ui.button, ui.buttonCompact, ui.buttonSecondary)}
                  >
                    Save .txt
                  </button>
                </>
              ) : (
                <>
                  <button
                    type="button"
                    onClick={() => copy("srt", result.srt)}
                    className={cx(ui.button, ui.buttonCompact, ui.buttonSecondary)}
                  >
                    {copied === "srt" ? "Copied" : "Copy"}
                  </button>
                  <button
                    type="button"
                    onClick={() => saveAs("srt", result.srt)}
                    className={cx(ui.button, ui.buttonCompact, ui.buttonSecondary)}
                  >
                    Save .srt
                  </button>
                </>
              )}
            </div>
          </div>
          <pre
            className={cx(
              "m-0 min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words select-text",
              view === "segmented" && ui.mono,
              ui.cardCopy,
            )}
          >
            {view === "original" ? result.text : result.srt}
          </pre>
        </section>
      )}
    </div>
  );
}
