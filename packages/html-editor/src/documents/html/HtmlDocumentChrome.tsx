// The shared preview chrome (address bar / reload / viewport / trailing button) now lives in
// `@rightkit/document-core/documents/PreviewChrome` and is used by BOTH the local-file HTML preview and the localhost URL preview.
// This file keeps only the file-preview-specific blocked-link prompt.

export function BlockedLinkPrompt({
  href,
  reason,
  onLocate,
  onCopy,
  onDismiss,
}: {
  href: string;
  reason: string;
  onLocate: () => void;
  onCopy: () => void;
  onDismiss: () => void;
}) {
  return (
    <div className="absolute bottom-4 left-1/2 z-30 w-[min(560px,calc(100%-2rem))] -translate-x-1/2 rounded-lg border border-line bg-panel-2 p-4 shadow-2xl">
      <p className="font-serif text-base font-semibold text-paper">Linked file not found</p>
      <p className="mt-1 text-sm text-muted">
        {reason} Locate it manually, copy the link, or dismiss this prompt.
      </p>
      <p className="mt-2 truncate font-mono text-xs text-muted-2">{href}</p>
      <div className="mt-3 flex justify-end gap-2 text-xs">
        <button
          type="button"
          onClick={onLocate}
          className="rounded border border-line px-3 py-1.5 text-muted hover:text-paper"
        >
          Locate file…
        </button>
        <button
          type="button"
          onClick={onCopy}
          className="rounded border border-line px-3 py-1.5 text-muted hover:text-paper"
        >
          Copy link
        </button>
        <button
          type="button"
          onClick={onDismiss}
          className="rounded bg-copper/20 px-3 py-1.5 text-copper-hi hover:bg-copper/30"
        >
          Dismiss
        </button>
      </div>
    </div>
  );
}
