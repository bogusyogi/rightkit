import { useState } from "react";
import {
  clearAllDiagnostics,
  collectDiagnosticsText,
  getDiagnostics,
} from "@rightkit/document-core/state/diagnostics";
import { APP_VERSION } from "../../state/updateLicense.js";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../../appName.js";

export function DiagnosticsSection() {
  const { settings } = useSettings();
  const [log, setLog] = useState(() => getDiagnostics());
  const [copied, setCopied] = useState<"" | "copied" | "failed">("");

  const refresh = () => setLog(getDiagnostics());
  const copyLog = async () => {
    try {
      await writeClipboardText(await collectDiagnosticsText());
      setCopied("copied");
    } catch {
      setCopied("failed");
    }
    setTimeout(() => setCopied(""), 1800);
  };
  const emailLog = async () => {
    await copyLog();
    const subject = encodeURIComponent(`${hostAppName()} ${APP_VERSION} diagnostics`);
    const body = encodeURIComponent(
      "The diagnostics log has been copied to your clipboard — please paste it below this line, then send.\n\n----\n",
    );
    window.location.href = `mailto:hello@viewright.cc?subject=${subject}&body=${body}`;
  };
  const clear = async () => {
    try {
      await clearAllDiagnostics();
    } catch {
      // The log stays on screen when erasure failed, so the failure is visible to the user.
    }
    refresh();
  };

  if (settings.diagnostics === false) {
    return (
      <div className="space-y-4">
        <div className="space-y-3 text-sm text-muted">
          <p>
            Diagnostics logging is currently off (Privacy → diagnostics). Turn it on to capture
            loads, document timings, and errors here.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-4 text-sm text-muted">
      <p>
        A local log of loads, document timings, and errors — stays on your machine. Use it to report
        a problem: it never sends on its own; “Email to support” copies the log and opens a
        prefilled message you send yourself.
      </p>
      <div className="flex flex-wrap gap-2 font-mono text-xs">
        <button
          type="button"
          onClick={emailLog}
          className="vr-press vr-focus rounded bg-copper/20 px-3 py-1.5 text-copper-hi hover:bg-copper/30"
        >
          Email to support
        </button>
        <button
          type="button"
          onClick={copyLog}
          className="vr-press vr-focus rounded border border-line px-3 py-1.5 text-muted hover:border-copper hover:text-copper-hi"
        >
          {copied === "copied" ? "Copied ✓" : copied === "failed" ? "Copy failed" : "Copy log"}
        </button>
        <button
          type="button"
          onClick={refresh}
          className="vr-press vr-focus rounded border border-line px-3 py-1.5 text-muted hover:border-copper hover:text-copper-hi"
        >
          Refresh
        </button>
        <button
          type="button"
          onClick={clear}
          className="vr-press vr-focus rounded border border-line px-3 py-1.5 text-muted hover:border-red-500/60 hover:text-paper"
        >
          Clear log
        </button>
      </div>
      <div className="max-h-72 overflow-auto rounded border border-line bg-ink/40 p-2 font-mono text-chrome-xs leading-relaxed text-muted-2 select-text">
        {log.length === 0 ? (
          <p className="text-muted-2">No diagnostics recorded yet.</p>
        ) : (
          [...log].reverse().map((e, i) => (
            <p key={`${e.ts_ms}-${i}`} className="whitespace-pre-wrap break-words">
              <span className="text-muted">{new Date(e.ts_ms).toISOString().slice(11, 19)}</span>{" "}
              <span className="text-copper-hi">{e.kind}</span> {e.event}
              {e.duration_ms === undefined ? "" : ` — ${e.duration_ms}ms`}
            </p>
          ))
        )}
      </div>
    </div>
  );
}
