// Wave 3 Info inspector — a 280px panel docked at the right edge of the content plane. It is a
// flex SIBLING of the plane (the plane shrinks), so it never overlaps the floating format bar or
// find bar. Shows only data already on LoadedDocument (path, encoding, fingerprint) or reachable
// through the live edit subscription the DocStatsPill uses — no backend calls, no invented fields.
// One global toggle owns it (App.tsx); Escape inside the panel closes it via onClose, which returns
// focus to the toolbar toggle button.
import { memo, useRef, type ReactNode } from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { Copy } from "@phosphor-icons/react/dist/icons/Copy";
import { LockSimple } from "@phosphor-icons/react/dist/icons/LockSimple";
import { X } from "@phosphor-icons/react/dist/icons/X";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { KIND_LABEL } from "../Toolbar.js";
import { copyText, isLocalFileDoc } from "../sidebar/SidebarPathUtils.js";
import { useEditText } from "@rightkit/document-core/app/useEditText";

// Same formats the DocStatsPill counts — the only surfaces with a live text buffer.
const COUNT_FORMATS = new Set<LoadedDocument["format"]>(["md", "mdx", "txt", "html", "unknown"]);

export const InfoInspector = memo(InfoInspectorImpl);

function InfoInspectorImpl({
  doc,
  onClose,
  subscribeEdits,
  getEditSnapshot,
}: {
  doc: LoadedDocument;
  onClose: () => void;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
}) {
  // Escape inside the panel closes it (bubble phase on the panel; stops propagation, as before).
  const panelRef = useRef<HTMLElement>(null);
  useDismiss(onClose, {
    escapeTarget: panelRef,
    outside: false,
    preventDefault: false,
    imeGuard: false,
  });
  // "Location" means the real filesystem path for file docs and the URL for web previews; untitled
  // drafts have only an internal pseudo-path, so the row is omitted for them.
  const location = doc.format === "web" ? doc.web?.url : isLocalFileDoc(doc) ? doc.path : null;
  const fingerprint = doc.fingerprint;
  const size = fingerprint?.size ? formatBytes(fingerprint.size) : null;
  const modified =
    fingerprint?.mtimeMs && fingerprint.mtimeMs > 0
      ? new Date(fingerprint.mtimeMs).toLocaleString(undefined, {
          dateStyle: "medium",
          timeStyle: "short",
        })
      : null;

  return (
    <aside
      ref={panelRef}
      id="vr-info-inspector"
      aria-label="Document info"
      className="no-print vr-glass-panel flex w-[280px] shrink-0 flex-col"
    >
      <div className="flex h-11 shrink-0 items-center justify-between pl-4 pr-2">
        <h2 className="font-sans text-chrome-xs font-semibold uppercase tracking-[0.14em] text-muted-2">
          Info
        </h2>
        <button
          type="button"
          aria-label="Close document info"
          title="Close"
          onClick={onClose}
          className="vr-press vr-focus flex h-7 w-7 items-center justify-center rounded-full text-muted hover:bg-paper/10 hover:text-paper"
        >
          <X size={14} weight="regular" aria-hidden />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
        <h3 className="mb-2 font-sans text-chrome-sm font-semibold text-paper">Document</h3>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-2">
          <InfoRow label="Kind">{KIND_LABEL[doc.format] ?? "Document"}</InfoRow>
          <InfoRow label="Name">{doc.displayName}</InfoRow>
          {location ? <LocationRow location={location} web={doc.format === "web"} /> : null}
          {COUNT_FORMATS.has(doc.format) ? (
            <LiveCounts
              doc={doc}
              subscribeEdits={subscribeEdits}
              getEditSnapshot={getEditSnapshot}
            />
          ) : null}
          {doc.encoding ? (
            <InfoRow label="Encoding" mono>
              {doc.encoding}
            </InfoRow>
          ) : null}
          {size ? <InfoRow label="Size">{size}</InfoRow> : null}
          {modified ? <InfoRow label="Modified">{modified}</InfoRow> : null}
        </dl>
        {doc.format !== "web" && (
          <div className="mt-4 flex gap-2.5 rounded-xl bg-paper/5 px-3 py-2.5 font-sans text-chrome-sm leading-relaxed text-muted">
            <LockSimple
              size={15}
              weight="regular"
              aria-hidden
              className="mt-0.5 shrink-0 text-copper-hi"
            />
            <p className="m-0">Opened from this computer. Nothing is uploaded.</p>
          </div>
        )}
      </div>
    </aside>
  );
}

function InfoRow({
  label,
  mono = false,
  children,
}: {
  label: string;
  mono?: boolean;
  children: ReactNode;
}) {
  return (
    <>
      <dt className="whitespace-nowrap pt-px text-right font-sans text-chrome-sm leading-5 text-muted-2">
        {label}
      </dt>
      <dd
        title={typeof children === "string" ? children : undefined}
        className={`min-w-0 truncate leading-5 text-paper ${
          mono ? "font-mono text-chrome-sm" : "font-sans text-chrome-sm"
        }`}
      >
        {children}
      </dd>
    </>
  );
}

// Full path/URL in the mono accent face, truncated with a title tooltip, plus the existing
// sidebar clipboard helper for the copy affordance.
function LocationRow({ location, web }: { location: string; web: boolean }) {
  const copyLabel = web ? "Copy URL" : "Copy path";
  return (
    <>
      <dt className="whitespace-nowrap pt-px text-right font-sans text-chrome-sm leading-5 text-muted-2">
        Location
      </dt>
      <dd className="flex min-w-0 items-center gap-1">
        <span
          title={location}
          className="min-w-0 flex-1 truncate font-mono text-chrome-xs leading-5 text-paper"
        >
          {location}
        </span>
        <button
          type="button"
          aria-label={copyLabel}
          title={copyLabel}
          onClick={() => void copyText(location)}
          className="vr-press vr-focus flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-2 hover:bg-paper/10 hover:text-paper"
        >
          <Copy size={12} weight="regular" aria-hidden />
        </button>
      </dd>
    </>
  );
}

// Live word/character counts — the same subscription DocStatsPill uses, so the inspector updates
// per keystroke without re-rendering the App shell.
function LiveCounts({
  doc,
  subscribeEdits,
  getEditSnapshot,
}: {
  doc: LoadedDocument;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
}) {
  const liveText = useEditText(subscribeEdits, getEditSnapshot, doc.path) ?? doc.content;
  const trimmed = liveText.trim();
  const words = trimmed ? trimmed.split(/\s+/).length : 0;
  const mins = words ? Math.max(1, Math.round(words / 230)) : 0;
  return (
    <>
      <InfoRow label="Words">{words.toLocaleString()}</InfoRow>
      <InfoRow label="Characters">{liveText.length.toLocaleString()}</InfoRow>
      <InfoRow label="Reading">{words ? `~${mins} min` : "—"}</InfoRow>
    </>
  );
}

function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  const kb = size / 1024;
  if (kb < 1024) return `${kb >= 100 ? Math.round(kb) : kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  return `${mb >= 100 ? Math.round(mb) : mb.toFixed(1)} MB`;
}
