// D14 — WorkspaceFileViewer.
//
// The editor surface is deliberately thin: Aethon's durable editor-buffer
// lifecycle is carried by the host app useEditorBuffer hook, while the daemon
// remains authoritative for reads, fingerprints, and writes. This component
// only presents the buffer and the explicit conflict choices.

import { useCallback, useMemo } from "react";
import type { ChangeEvent } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import { useEditorBuffer } from "@rightkit/agent-ui-hooks/hooks/useEditorBuffer";
import type {
  WorkspaceFileResponse,
  WorkspaceFileSaveOutcome,
} from "@rightkit/agent-ui-core/lib/types/workspace-files";

export interface WorkspaceFileViewerProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
  path: string;
  /** Stable editor-tab identity. The hook uses root + NUL + path when omitted. */
  bufferId?: string;
  /** Optional: a remote-edit reload token (bumped by the workspace watcher). */
  reloadToken?: number;
  /** Triggered after a save attempt. */
  onSaved?: (outcome: WorkspaceFileSaveOutcome) => void;
  /** Triggered when the user resolves a stale-buffer conflict. */
  onResolvedConflict?: (choice: "re-read" | "kept-local") => void;
}

export default function WorkspaceFileViewer({
  config,
  workspaceRoot,
  path,
  bufferId,
  reloadToken = 0,
  onSaved,
  onResolvedConflict,
}: WorkspaceFileViewerProps) {
  const {
    status,
    response,
    remoteConflict,
    reason,
    content,
    dirty,
    loading,
    setContent,
    save,
    revert,
    reloadExternal,
    keepLocal,
  } = useEditorBuffer({
    config,
    bufferId,
    workspaceRoot,
    path,
    reloadToken,
  });

  const handleChange = useCallback(
    (event: ChangeEvent<HTMLTextAreaElement>) => setContent(event.target.value),
    [setContent],
  );

  const handleSave = useCallback(async () => {
    const outcome = await save();
    onSaved?.(outcome);
  }, [onSaved, save]);

  const handleReread = useCallback(async () => {
    // A conflict is already held by the buffer registry. reloadExternal
    // atomically hydrates that remote response; if the remote response is no
    // longer available it leaves the local buffer untouched and the user can
    // retry from this same surface.
    const hadConflict = Boolean(remoteConflict);
    await reloadExternal();
    if (hadConflict) onResolvedConflict?.("re-read");
  }, [onResolvedConflict, reloadExternal, remoteConflict]);

  const handleKeepLocal = useCallback(() => {
    keepLocal();
    onResolvedConflict?.("kept-local");
  }, [keepLocal, onResolvedConflict]);

  const statusLabel = useMemo(() => {
    switch (status) {
      case "idle":
        return "Not loaded";
      case "loading":
        return "Loading…";
      case "ready":
        return dirty ? "Modified" : "Saved";
      case "stale":
        return "Stale buffer — choice required";
      case "unsupported":
        return response?.kind === "binary"
          ? "Binary file (preview disabled)"
          : response?.kind === "tooLarge"
            ? "File too large to preview"
            : response?.kind === "directory"
              ? "Directory"
              : "Unsupported";
      case "error":
        return reason ?? "Failed to read";
      default:
        return "—";
    }
  }, [dirty, reason, response?.kind, status]);

  const showConflict = Boolean(remoteConflict) || status === "stale";

  return (
    <section
      aria-label="File viewer"
      data-testid="workspace-file-viewer"
      className="flex h-full min-h-0 flex-col"
    >
      <header className="flex flex-none items-center gap-2 border-b border-line px-3 py-2 text-[12px] text-text-2">
        <span className="truncate font-mono" data-testid="workspace-file-viewer-path">
          {path}
        </span>
        <span
          className="ml-auto text-[10px] text-text-3"
          data-testid="workspace-file-viewer-status"
          role="status"
          aria-live="polite"
        >
          {statusLabel}
        </span>
        {response?.permission ? (
          <span className="text-[10px] text-text-3" data-testid="workspace-file-viewer-mode">
            {response.permission}
          </span>
        ) : null}
        <button
          type="button"
          onClick={revert}
          disabled={!dirty || loading}
          data-testid="workspace-file-viewer-revert"
          className="cr-control-pill ml-1 inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:cursor-not-allowed disabled:opacity-50"
        >
          Revert
        </button>
        <button
          type="button"
          onClick={() => void handleSave()}
          disabled={!dirty || loading || status !== "ready"}
          data-testid="workspace-file-viewer-save"
          className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-accent/60 bg-accent px-2 text-[11px] font-semibold text-on-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:cursor-not-allowed disabled:opacity-50"
        >
          Save
        </button>
      </header>

      {status === "unsupported" && response?.kind === "binary" ? (
        <BinaryNotice response={response} />
      ) : status === "unsupported" && response?.kind === "tooLarge" ? (
        <TooLargeNotice response={response} />
      ) : showConflict ? (
        <div
          className="flex h-full min-h-0 flex-col"
          data-testid="workspace-file-viewer-resolver-shell"
        >
          <StaleConflictNotice
            stash={remoteConflict}
            onReread={handleReread}
            onKeepLocal={handleKeepLocal}
            isReReading={loading}
            readError={status === "error" ? reason : null}
          />
          <textarea
            aria-label={`Edit ${path} (with conflict)`}
            data-testid="workspace-file-viewer-textarea-conflict"
            value={content}
            onChange={handleChange}
            spellCheck={false}
            className="block h-full w-full flex-1 resize-none bg-panel px-3 py-2 font-mono text-[12px] leading-relaxed text-text focus:outline-none"
          />
        </div>
      ) : (
        <textarea
          aria-label={`Edit ${path}`}
          data-testid="workspace-file-viewer-textarea"
          value={content}
          onChange={handleChange}
          spellCheck={false}
          className="block h-full w-full flex-1 resize-none bg-panel px-3 py-2 font-mono text-[12px] leading-relaxed text-text focus:outline-none"
        />
      )}
    </section>
  );
}

function BinaryNotice({ response }: { response: WorkspaceFileResponse }) {
  return (
    <div className="px-3 py-4 text-[12px] text-text-2" data-testid="workspace-file-viewer-binary">
      <p className="font-medium text-text">Binary file</p>
      <p className="mt-1 text-text-3">{response.reason ?? "Binary content; preview disabled."}</p>
      <p className="mt-2 text-[10px] text-text-3">
        SHA-256: <code>{response.fingerprint ?? "unavailable"}</code>
      </p>
    </div>
  );
}

function TooLargeNotice({ response }: { response: WorkspaceFileResponse }) {
  return (
    <div
      className="px-3 py-4 text-[12px] text-text-2"
      data-testid="workspace-file-viewer-too-large"
    >
      <p className="font-medium text-text">File too large to preview</p>
      <p className="mt-1 text-text-3">
        {response.reason ?? "Use the shell to inspect this file directly."}
      </p>
    </div>
  );
}

function StaleConflictNotice({
  stash,
  onReread,
  onKeepLocal,
  isReReading,
  readError,
}: {
  stash: WorkspaceFileResponse | null;
  onReread: () => void;
  onKeepLocal: () => void;
  isReReading: boolean;
  readError: string | null;
}) {
  return (
    <div
      role="alert"
      data-testid="workspace-file-viewer-stale"
      className="mx-3 mt-3 rounded-md border border-warning/40 bg-warning/10 p-3 text-[12px] text-text"
    >
      <p className="font-medium">Stale buffer</p>
      <p className="mt-1 text-text-2">
        The file on disk changed since you opened it. Re-read to replace your draft, or keep your
        local draft and save again after reviewing the remote change.
      </p>
      {stash?.fingerprint ? (
        <p
          className="mt-1 font-mono text-[10px] text-text-3"
          data-testid="workspace-file-viewer-conflict-fingerprint"
        >
          Remote fingerprint: {stash.fingerprint}
        </p>
      ) : null}
      {readError ? (
        <p
          className="mt-2 rounded border border-warning/40 bg-panel-2 px-2 py-1 text-[11px] text-warning"
          role="status"
          data-testid="workspace-file-viewer-reread-error"
        >
          Re-read failed: {readError}
        </p>
      ) : null}
      <div className="mt-2 flex gap-2">
        <button
          type="button"
          onClick={() => void onReread()}
          disabled={isReReading}
          data-testid="workspace-file-viewer-reread"
          className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {isReReading ? "Re-reading…" : "Re-read file"}
        </button>
        <button
          type="button"
          onClick={onKeepLocal}
          data-testid="workspace-file-viewer-keep-local"
          className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          Keep my draft
        </button>
      </div>
    </div>
  );
}
