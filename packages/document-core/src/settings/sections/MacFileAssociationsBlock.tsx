import { useEffect, useMemo, useState } from "react";
import { documentFormatEntries } from "../../documents/documentFormats.js";
import {
  fileAssociationStatus,
  inTauri,
  openFileAssociationSettings,
  setFileAssociationDefaults,
} from "../../state/tauriBridge.js";
import type {
  FileAssociationFormatStatus,
  FileAssociationStatus,
  MarkdownUtiHandlerStatus,
} from "../../state/tauriBridge.js";
import { isMac, isWindows } from "../../platform.js";
import { hostAppName } from "../../appName.js";

export const SUPPORTED_ASSOCIATION_FORMATS = [
  { id: "md", label: "Markdown" },
  { id: "mdx", label: "MDX" },
  { id: "txt", label: "Plain text & code" },
  { id: "pdf", label: "PDF" },
  { id: "html", label: "HTML" },
  { id: "docx", label: "DOCX" },
  { id: "image", label: "Images" },
] as const;

const supportedFormatIds = SUPPORTED_ASSOCIATION_FORMATS.map(({ id }) => id);

/** Extensions come exclusively from shared/document_formats.tsv via canonical entries. */
export function extensionsForFormat(formatId: string): string[] {
  return documentFormatEntries
    .filter((entry) => entry.format === formatId)
    .map((entry) => `.${entry.extension}`);
}

export function formatExtensionPreview(formatId: string): string {
  return extensionsForFormat(formatId).join(", ");
}

export function toggleFormatSelection(
  selected: string[],
  formatId: string,
  checked: boolean,
): string[] {
  const next = new Set(selected);
  if (checked) next.add(formatId);
  else next.delete(formatId);
  return supportedFormatIds.filter((id) => next.has(id));
}

export function allFormatsSelected(selected: string[]): boolean {
  return supportedFormatIds.every((id) => selected.includes(id));
}

export function selectedFormatsVerified(
  status: FileAssociationStatus | null,
  selected: string[],
): boolean {
  if (!status || selected.length === 0) return false;
  const byId = new Map(status.formats.map((format) => [format.formatId, format]));
  return selected.every((id) => byId.get(id)?.allSet === true);
}

/** One UTI row: identifier → current preferred handler (verified from LaunchServices). */
export function describeHandler(handler: MarkdownUtiHandlerStatus): string {
  const target = handler.hostIsPreferred
    ? hostAppName()
    : (handler.preferredHandler ?? "no handler");
  return `${handler.uti} → ${target}`;
}

function formatStatus(
  status: FileAssociationStatus | null,
  formatId: string,
): FileAssociationFormatStatus | undefined {
  return status?.formats.find((format) => format.formatId === formatId);
}

export function formatOwnership(status: FileAssociationFormatStatus): string {
  if (status.allSet) return hostAppName();
  const preferred = status.handlers.filter((handler) => handler.hostIsPreferred).length;
  if (preferred > 0) return `${preferred}/${status.handlers.length} ${hostAppName()}`;
  return "Other app";
}

export function FileAssociationsBlock() {
  const [status, setStatus] = useState<FileAssociationStatus | null>(null);
  const [selected, setSelected] = useState<string[]>(() => [...supportedFormatIds]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const mac = isMac();
  const windows = isWindows();
  const available = (mac || windows) && inTauri();
  const allSelected = allFormatsSelected(selected);
  const extensionCounts = useMemo(
    () =>
      new Map(
        SUPPORTED_ASSOCIATION_FORMATS.map((format) => [
          format.id,
          extensionsForFormat(format.id).length,
        ]),
      ),
    [],
  );
  const extensionCount = useMemo(
    () => Array.from(extensionCounts.values()).reduce((sum, count) => sum + count, 0),
    [extensionCounts],
  );

  useEffect(() => {
    if (!available) return;
    let cancelled = false;
    const refresh = () => {
      void fileAssociationStatus()
        .then((next) => {
          if (!cancelled) setStatus(next);
        })
        .catch(() => {
          /* surfaced by explicit action */
        });
    };
    refresh();
    window.addEventListener("focus", refresh);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", refresh);
    };
  }, [available]);

  if (!available) return null;

  const apply = async () => {
    if (selected.length === 0) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await setFileAssociationDefaults(selected);
      // Re-query source of truth rather than trusting write result.
      const next = await fileAssociationStatus();
      setStatus(next);
      if (!selectedFormatsVerified(next, selected)) {
        setError(
          "macOS did not approve every selected content type. Confirm each system prompt, then try remaining formats.",
        );
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const openWindowsSettings = async () => {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await openFileAssociationSettings(supportedFormatIds);
      setNotice(
        `Windows Settings opened. Choose ${hostAppName()} for the file types you want it to open.`,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const verified = selectedFormatsVerified(status, selected);

  return (
    <section className="space-y-4">
      <h3 className="font-sans text-sm font-semibold text-paper">Open by default</h3>
      <p className="text-xs text-muted-2">
        {mac
          ? `Choose which supported formats ${hostAppName()} should open by default. Nothing changes until you apply your selection. macOS may ask once for each changed content type.`
          : `${hostAppName()} is registered for every supported format. Windows protects default-app choices, so review current ownership here, then confirm changes in Windows Settings.`}
      </p>
      {mac ? (
        <label className="flex items-center gap-2 text-sm text-paper">
          <input
            type="checkbox"
            checked={allSelected}
            onChange={(event) => setSelected(event.target.checked ? [...supportedFormatIds] : [])}
            data-testid="file-association-select-all"
          />
          <span>Select all formats</span>
          <span className="ml-auto text-xs text-muted-2">{extensionCount} file types</span>
        </label>
      ) : (
        <div className="flex items-center justify-between rounded-md bg-panel/50 px-3 py-2 text-xs text-muted-2">
          <span>Supported by {hostAppName()}</span>
          <span>{extensionCount} file types</span>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2 font-mono text-xs">
        {mac ? (
          <button
            type="button"
            onClick={() => void apply()}
            disabled={busy || selected.length === 0}
            className="vr-press vr-focus rounded bg-copper/20 px-3 py-1.5 text-copper-hi hover:bg-copper/30 disabled:opacity-50"
            data-testid="file-association-apply"
          >
            {busy
              ? "Applying..."
              : allSelected
                ? `Make ${hostAppName()} default`
                : "Apply selected defaults"}
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void openWindowsSettings()}
            disabled={busy}
            className="vr-press vr-focus rounded bg-copper/20 px-3 py-1.5 text-copper-hi hover:bg-copper/30 disabled:opacity-50"
            data-testid="file-association-open-windows"
          >
            {busy ? "Opening..." : "Open Windows Default Apps"}
          </button>
        )}
        {mac && status && (
          <span
            className={verified ? "text-copper-hi" : "text-red-300"}
            data-testid="file-association-result"
          >
            {verified ? "Selected formats verified" : "Selected formats are not all verified"}
          </span>
        )}
      </div>
      {notice && <p className="text-xs text-muted-2">{notice}</p>}
      {error && <p className="font-mono text-xs text-red-300">{error}</p>}
      <div
        className="divide-y divide-line overflow-hidden rounded-lg border border-line bg-ink/30"
        data-testid="file-association-formats"
      >
        {SUPPORTED_ASSOCIATION_FORMATS.map((format) => {
          const current = formatStatus(status, format.id);
          const checked = selected.includes(format.id);
          return (
            <div
              key={format.id}
              className={`flex items-center gap-3 px-3 py-2.5 ${mac ? "hover:bg-line/15" : ""}`}
            >
              {mac ? (
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={(event) =>
                    setSelected(toggleFormatSelection(selected, format.id, event.target.checked))
                  }
                  aria-label={`Set ${format.label} defaults`}
                  data-testid={`file-association-format-${format.id}`}
                />
              ) : (
                <span
                  aria-hidden="true"
                  className={`h-2 w-2 rounded-full ${current?.allSet ? "bg-copper-hi" : "bg-muted-2/50"}`}
                />
              )}
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium text-paper">{format.label}</span>
                <span className="block text-chrome-xs text-muted-2">
                  {extensionCounts.get(format.id)} file{" "}
                  {extensionCounts.get(format.id) === 1 ? "type" : "types"}
                </span>
              </span>
              {current && (
                <span className={`text-xs ${current.allSet ? "text-copper-hi" : "text-muted-2"}`}>
                  {formatOwnership(current)}
                </span>
              )}
            </div>
          );
        })}
      </div>
      <details className="rounded border border-line px-3 py-2 text-xs text-muted-2">
        <summary className="cursor-pointer select-none text-paper">Technical details</summary>
        <div className="mt-3 space-y-3 font-mono text-chrome-xs leading-relaxed">
          {SUPPORTED_ASSOCIATION_FORMATS.map((format) => {
            const current = formatStatus(status, format.id);
            return (
              <div key={format.id}>
                <div className="font-semibold text-paper">{format.label}</div>
                <div className="break-words">{formatExtensionPreview(format.id)}</div>
                {current?.handlers.map((handler) => (
                  <div key={handler.uti}>{describeHandler(handler)}</div>
                ))}
              </div>
            );
          })}
        </div>
      </details>
    </section>
  );
}
