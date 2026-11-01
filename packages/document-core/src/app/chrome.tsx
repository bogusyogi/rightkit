import { useState } from "react";
import { normalizeLivePreviewUrl } from "../documents/web/webUrl.js";
import { getRecentUrls } from "../state/recentUrls.js";

function looksLikeLocalPath(input: string) {
  return (
    /^file:\/\//i.test(input) ||
    /^[a-z]:[\\/]/i.test(input) ||
    /^\\\\/.test(input) ||
    /^\/[^/]/.test(input)
  );
}
import { ActionDialog, Button, IconButton, Input, type DialogAction } from "@rightkit/react-shell/react";
import { hostAppName } from "../appName.js";

// Presentational shell chrome for App: confirmation dialogs and the Open-URL dialog. Pure leaf
// components — no app state, no file semantics. Extracted from App.tsx so the shell composition
// stays readable. (The Start page moved to src/app/shell/StartPage.tsx in Wave 3.)

export const DIALOG_SLOTS = {
  overlay: "z-[60] bg-black/50 p-4 transition-opacity duration-[80ms] ease-out",
  root: "vr-pop vr-pop-modal max-w-[calc(100vw-2rem)] rounded-xl border border-line bg-panel-2 p-6 shadow-2xl",
  header: "mb-1 font-serif text-lg font-semibold text-paper",
  footer: "flex justify-end gap-2 font-sans text-xs",
} as const;

const BTN = "vr-press vr-focus rounded px-3 py-1.5 text-xs font-normal";
const CANCEL_CLASS = `${BTN} border border-line bg-transparent text-muted hover:text-paper`;
const DANGER_CLASS = `${BTN} border border-line bg-transparent text-muted hover:border-danger/60 hover:text-paper`;
const PRIMARY_CLASS = `${BTN} border-0 bg-copper/20 text-copper-hi hover:bg-copper/30`;
const SECONDARY_CLASS = `${BTN} border border-line bg-transparent text-muted hover:border-copper hover:text-copper-hi`;

export function cancelAction(onClick: () => void, label = "Cancel"): DialogAction {
  return { id: "cancel", label, onClick, variant: "ghost", className: CANCEL_CLASS };
}
export function dangerAction(label: string, onClick: () => void): DialogAction {
  return { id: "danger", label, onClick, variant: "ghost", className: DANGER_CLASS };
}
export function primaryAction(label: string, onClick: () => void): DialogAction {
  return { id: "primary", label, onClick, variant: "ghost", className: PRIMARY_CLASS };
}

export function QuitConfirm({
  dirtyNames,
  onQuit,
  onQuitWithoutSaving,
  onSaveAll,
  onCancel,
}: {
  dirtyNames: string[];
  onQuit: () => void;
  onQuitWithoutSaving?: () => void;
  onSaveAll: () => void;
  onCancel: () => void;
}) {
  return (
    <ActionDialog
      title="Unsaved changes"
      ariaLabel="Unsaved changes"
      description="You have file-backed or Word documents with unsaved changes that will be lost if you quit. (Untitled text drafts are kept and restored automatically.)"
      onClose={onCancel}
      width={480}
      role="alertdialog"
      slots={DIALOG_SLOTS}
      actions={[
        cancelAction(onCancel),
        dangerAction("Quit without saving", onQuitWithoutSaving ?? onQuit),
        primaryAction("Save All", onSaveAll),
      ]}
    >
      {dirtyNames.length > 0 && (
        <ul className="mb-5 max-h-40 overflow-y-auto rounded border border-line bg-ink/20 px-3 py-2 font-mono text-xs text-paper">
          {dirtyNames.map((name) => (
            <li key={name} className="truncate py-0.5">
              {name}
            </li>
          ))}
        </ul>
      )}
    </ActionDialog>
  );
}

export function CloseConfirm({
  title,
  onSave,
  onDontSave,
  onCancel,
}: {
  title: string;
  onSave: () => void;
  onDontSave: () => void;
  onCancel: () => void;
}) {
  return (
    <ActionDialog
      title="Save changes?"
      ariaLabel="Save changes?"
      description={`“${title}” has unsaved changes. Save before closing?`}
      onClose={onCancel}
      width={420}
      role="alertdialog"
      slots={DIALOG_SLOTS}
      actions={[
        cancelAction(onCancel),
        dangerAction("Don't save", onDontSave),
        primaryAction("Save", onSave),
      ]}
    />
  );
}

const DIRTY_LIST_CLASS =
  "mb-5 max-h-40 overflow-auto rounded border border-line bg-ink/40 px-3 py-2 font-mono text-xs text-muted-2";

function DirtyList({ names }: { names: string[] }) {
  return (
    <ul className={DIRTY_LIST_CLASS}>
      {names.map((name) => (
        <li key={name} className="truncate">
          {name}
          <span className="text-copper-hi"> *</span>
        </li>
      ))}
    </ul>
  );
}

export function CloseAllConfirm({
  dirtyNames,
  onSaveAll,
  onDontSave,
  onCancel,
}: {
  dirtyNames: string[];
  onSaveAll: () => void;
  onDontSave: () => void;
  onCancel: () => void;
}) {
  const n = dirtyNames.length;
  return (
    <ActionDialog
      title="Save changes before closing?"
      ariaLabel="Save changes before closing?"
      description={`${n} document${n === 1 ? "" : "s"} ${n === 1 ? "has" : "have"} unsaved changes — choose what to save before closing all tabs.`}
      onClose={onCancel}
      width={480}
      role="alertdialog"
      slots={DIALOG_SLOTS}
      actions={[
        cancelAction(onCancel),
        dangerAction("Don't Save", onDontSave),
        primaryAction("Save All", onSaveAll),
      ]}
    >
      <DirtyList names={dirtyNames} />
    </ActionDialog>
  );
}

// HTML site session close/save chooser: a tab can have several dirty local pages, so offer
// Save current / Save all / Discard all / Cancel (listing the dirty page names) instead of the
// single-doc Save / Don't Save prompt. Reused for Ctrl+S with multiple dirty pages (mode "save",
// no close) and for closing a dirty HTML tab (mode "close").
export function HtmlCloseConfirm({
  title,
  mode,
  dirtyNames,
  onSaveCurrent,
  onSaveAll,
  onDiscardAll,
  onCancel,
}: {
  title: string;
  mode: "save" | "close";
  dirtyNames: string[];
  onSaveCurrent: () => void;
  onSaveAll: () => void;
  onDiscardAll: () => void;
  onCancel: () => void;
}) {
  const closing = mode === "close";
  const n = dirtyNames.length;
  const heading = closing ? "Save changes before closing?" : "Unsaved pages";
  return (
    <ActionDialog
      title={heading}
      ariaLabel={heading}
      description={`“${title}” has ${n} unsaved ${n === 1 ? "page" : "pages"}${closing ? " — choose what to save before closing this tab." : "."}`}
      onClose={onCancel}
      width={480}
      slots={DIALOG_SLOTS}
      actions={[
        cancelAction(onCancel),
        dangerAction("Discard all", onDiscardAll),
        {
          id: "save-current",
          label: "Save current",
          onClick: onSaveCurrent,
          variant: "ghost",
          className: SECONDARY_CLASS,
        },
        primaryAction("Save all", onSaveAll),
      ]}
    >
      <DirtyList names={dirtyNames} />
    </ActionDialog>
  );
}

export function BlockDraftViewConfirm({
  onDone,
  onDiscard,
  onStay,
  error,
}: {
  onDone: () => void;
  onDiscard: () => void;
  onStay: () => void;
  error?: string | null;
}) {
  return (
    <ActionDialog
      title="Unsaved block edit"
      ariaLabel="Unsaved block edit"
      description="You have an open block edit with unsaved changes. Save this block before leaving the view?"
      onClose={onStay}
      width={460}
      role="alertdialog"
      slots={{ ...DIALOG_SLOTS, footer: `${error ? "mt-3 " : ""}${DIALOG_SLOTS.footer}` }}
      actions={[
        cancelAction(onStay, "Stay"),
        dangerAction("Discard", onDiscard),
        primaryAction("Done", onDone),
      ]}
    >
      {error && (
        <p
          role="alert"
          className="mt-3 rounded border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-red-300"
        >
          {error}
        </p>
      )}
    </ActionDialog>
  );
}

// The watched file changed on disk while the user had unsaved edits. Offer an explicit choice
// instead of silently clobbering either side.
export function ConflictReload({
  name,
  onReload,
  onKeep,
}: {
  name: string;
  onReload: () => void;
  onKeep: () => void;
}) {
  return (
    <ActionDialog
      title="File changed on disk"
      ariaLabel="File changed on disk"
      description={`“${name}” changed outside ${hostAppName()} while you have unsaved edits. Reload from disk to discard your edits, or keep your version and ignore the on-disk change.`}
      onClose={onKeep}
      width={460}
      role="alertdialog"
      slots={DIALOG_SLOTS}
      actions={[
        cancelAction(onKeep, "Keep my changes"),
        dangerAction("Reload from disk", onReload),
      ]}
    />
  );
}

export function OpenUrlDialog({
  onOpen,
  onCancel,
}: {
  onOpen: (input: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState("");
  const [error, setError] = useState("");
  const recent = getRecentUrls();
  const submit = () => {
    const trimmed = value.trim();
    if (!trimmed) return;
    if (!normalizeLivePreviewUrl(trimmed) && !looksLikeLocalPath(trimmed)) {
      setError("Enter a URL, localhost preview, bare domain, or local file path.");
      return;
    }
    onOpen(trimmed);
  };
  return (
    <ActionDialog
      title="Open URL"
      ariaLabel="Open URL"
      description={`Open a localhost preview, web URL, or local file in a ${hostAppName()} tab.`}
      onClose={onCancel}
      width={460}
      slots={{ ...DIALOG_SLOTS, footer: `mt-5 ${DIALOG_SLOTS.footer}` }}
      actions={[cancelAction(onCancel), primaryAction("Open", submit)]}
    >
      <Input
        value={value}
        aria-invalid={!!error}
        aria-describedby={error ? "open-url-error" : undefined}
        onChange={(e) => {
          setValue(e.target.value);
          if (error) setError("");
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
          if (e.key === "Escape") onCancel();
        }}
        className={`vr-focus mt-4 w-full rounded border bg-ink px-3 py-2 font-mono text-sm text-paper focus:border-copper ${error ? "border-danger/70" : "border-line"}`}
        placeholder="e.g. localhost:5173 or D:\Docs\file.md"
      />
      {error && (
        <p id="open-url-error" className="mt-2 font-sans text-xs text-danger">
          {error}
        </p>
      )}
      {recent.length > 0 && (
        <div className="mt-4">
          <p className="mb-1.5 font-sans text-xs uppercase tracking-wide text-muted-2">Recent</p>
          <div className="flex flex-col gap-1">
            {recent.map((url) => (
              <Button
                key={url}
                variant="ghost"
                size="sm"
                onClick={() => onOpen(url)}
                title={url}
                className="vr-press vr-focus justify-start truncate rounded border border-line px-3 py-1 text-left font-mono text-xs font-normal text-muted hover:border-copper hover:text-paper"
              >
                {url}
              </Button>
            ))}
          </div>
        </div>
      )}
    </ActionDialog>
  );
}

export function AppErrorNotice({ message, onClose }: { message: string; onClose: () => void }) {
  return (
    <ActionDialog
      title="Couldn’t open that"
      ariaLabel="Couldn’t open that"
      description={message}
      onClose={onClose}
      width={560}
      align="top"
      slots={{ ...DIALOG_SLOTS, root: `relative ${DIALOG_SLOTS.root}`, overlay: `${DIALOG_SLOTS.overlay} pt-24`, footer: "hidden" }}
      actions={[]}
    >
      <IconButton
        label="Close error"
        onClick={onClose}
        className="vr-press vr-focus absolute right-3 top-3 flex h-8 w-8 items-center justify-center rounded font-sans text-sm text-muted hover:bg-ink/40 hover:text-paper"
      >
        ×
      </IconButton>
    </ActionDialog>
  );
}
