// Start page — centred wordmark, one-line promise built only from FEATURES-backed formats, the
// entry actions (Open primary; New/Paste/Open URL secondary), then a "Recent" row list with path
// and last-opened time. All data is real: recents come from the library DB and open through the
// existing recent-open path (see mapFileEntry in src/state/library.ts).
import type { CSSProperties, ReactNode } from "react";
import { relativeTimeFrom } from "@rightkit/platform-ui";
import { ClipboardText } from "@phosphor-icons/react/dist/icons/ClipboardText";
import { FolderOpen } from "@phosphor-icons/react/dist/icons/FolderOpen";
import { GlobeSimple } from "@phosphor-icons/react/dist/icons/GlobeSimple";
import { NotePencil } from "@phosphor-icons/react/dist/icons/NotePencil";
import { FileTypeIcon } from "../../documents/FileTypeIcon.js";
import type { FileEntry } from "../../state/library.js";
import { isMac, shortcutLabel } from "@rightkit/document-core/platform";

export function StartPage({
  onNew,
  onOpenDialog,
  onOpenUrl,
  onOpenClipboard,
  recents,
  onOpenRecent,
}: {
  onNew: () => void;
  onOpenDialog: () => void;
  onOpenUrl: () => void;
  onOpenClipboard: () => void;
  recents?: FileEntry[];
  onOpenRecent?: (entry: FileEntry) => void;
}) {
  return (
    <div className="flex min-h-full w-full min-w-0 max-w-full flex-col items-center justify-center gap-9 px-4 py-10 text-center sm:px-6">
      <section className="vr-rise flex w-full min-w-0 max-w-full flex-col items-center">
        <h1 className="[font-family:var(--font-wordmark)] text-[42px] leading-none tracking-[0.01em] text-paper">
          View<span className="text-copper">Right</span>
        </h1>
        <p className="mt-3 max-w-[440px] text-balance font-sans text-sm leading-relaxed text-muted">
          Open Markdown, text, HTML, Word documents, images and PDFs. Files stay on this computer.
        </p>
        <div className="mt-6 flex w-full min-w-0 max-w-full flex-wrap items-center justify-center gap-2.5">
          <StartAction
            primary
            label="Open…"
            hint="Ctrl+O"
            onClick={onOpenDialog}
            icon={<FolderOpen size={15} weight="regular" aria-hidden />}
          />
          <StartAction
            label="New document"
            hint="Ctrl+N"
            // Bare call — passing the click event through would land it in the format arg.
            onClick={() => onNew()}
            icon={<NotePencil size={15} weight="regular" aria-hidden />}
          />
          <StartAction
            label="Paste"
            hint="Ctrl+Shift+V"
            onClick={onOpenClipboard}
            icon={<ClipboardText size={15} weight="regular" aria-hidden />}
          />
          <StartAction
            label="Open URL…"
            onClick={onOpenUrl}
            icon={<GlobeSimple size={15} weight="regular" aria-hidden />}
          />
        </div>
      </section>
      <section
        className="vr-rise w-full min-w-0 max-w-[640px] rounded-2xl border border-dashed border-line p-4 sm:p-5"
        style={{ "--rise-delay": "90ms" } as CSSProperties}
      >
        <div className="mb-3 flex min-w-0 flex-wrap items-baseline justify-between gap-x-3 gap-y-1 px-1 text-left">
          <h2 className="font-sans text-chrome-md font-semibold text-paper">Recent</h2>
          <span className="font-sans text-xs text-muted-2">Drop files anywhere to open them</span>
        </div>
        {recents && recents.length > 0 && onOpenRecent ? (
          <ul className="flex flex-col gap-0.5">
            {recents.slice(0, 8).map((entry) => (
              <li key={entry.token}>
                <RecentRow entry={entry} onOpen={() => onOpenRecent(entry)} />
              </li>
            ))}
          </ul>
        ) : (
          <p className="py-4 text-center font-sans text-xs text-muted-2">
            Recently opened files appear here.
          </p>
        )}
      </section>
    </div>
  );
}

// aria-keyshortcuts wants UI Events key names — "Ctrl" isn't one, and the actual modifier on
// macOS is Meta (the same mapping shortcutLabel renders as ⌘).
// Hints are authored in Windows vocabulary ("Ctrl+Shift+V"); convert to a canonical chord.
function hintChord(hint: string): string {
  return hint.toLowerCase().replace(/\bctrl\b/g, "mod");
}

function keyShortcutsAttr(hint: string): string {
  return hint
    .split("+")
    .map((part) => {
      const key = part.trim();
      if (key === "Ctrl" || key === "Control") return isMac() ? "Meta" : "Control";
      return key;
    })
    .join("+");
}

function StartAction({
  icon,
  label,
  hint,
  onClick,
  primary = false,
}: {
  icon: ReactNode;
  label: string;
  hint?: string;
  onClick: () => void;
  primary?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={hint ? `${label} (${shortcutLabel(hintChord(hint))})` : label}
      aria-keyshortcuts={hint ? keyShortcutsAttr(hint) : undefined}
      className={`vr-press vr-focus flex h-[34px] items-center gap-1.5 rounded-full border px-4 font-sans text-chrome-md font-medium ${
        primary
          ? "border-copper/40 bg-copper/15 text-copper-hi hover:bg-copper/25"
          : "border-line bg-panel text-paper hover:border-muted-2 hover:bg-panel-2"
      }`}
    >
      {icon}
      {label}
      {hint ? (
        <kbd aria-hidden className="ml-1 font-mono text-chrome-xs opacity-60">
          {shortcutLabel(hintChord(hint))}
        </kbd>
      ) : null}
    </button>
  );
}

/** Recents row time: the owner's relative text for the first week, then a short date. */
function recentTimeLabel(openedAt: number | undefined): string | null {
  if (!openedAt) return null;
  if (Date.now() - openedAt >= 7 * 86_400_000) {
    return new Date(openedAt).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  return relativeTimeFrom(openedAt);
}

// Recent row: format icon, file name, the full path (mono, truncated), last-opened time.
function RecentRow({ entry, onOpen }: { entry: FileEntry; onOpen: () => void }) {
  const time = recentTimeLabel(entry.openedAt);
  return (
    <button
      type="button"
      onClick={onOpen}
      title={entry.token}
      aria-label={`Open ${entry.displayName}`}
      className="vr-press vr-focus flex h-9 w-full items-center gap-2.5 rounded-lg px-2 text-left hover:bg-panel-2"
    >
      {/* The icon carries its own role="img"+label; the button's aria-label already names the file. */}
      <span aria-hidden className="flex w-5 shrink-0 justify-center text-muted-2">
        <FileTypeIcon format={entry.format} path={entry.token} size={16} />
      </span>
      <span className="max-w-[38%] shrink-0 truncate font-sans text-chrome-sm font-medium text-paper">
        {entry.displayName}
      </span>
      <span className="min-w-0 flex-1 truncate text-left font-mono text-chrome-xs text-muted-2">
        {entry.token}
      </span>
      {time && (
        <span className="shrink-0 font-mono text-chrome-xs tabular-nums text-muted-2">{time}</span>
      )}
    </button>
  );
}
