// Renderer editor-buffer hook.
//
// This ports the Aethon EditorCanvas buffer lifecycle to the host app existing
// daemon workspace-file contract. The hook owns no file system state: reads,
// writes, fingerprints, and conflict responses come from the daemon client.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getWorkspaceFile,
  putWorkspaceFile,
} from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type {
  WorkspaceFileKind,
  WorkspaceFileResponse,
  WorkspaceFileSaveOutcome,
} from "@rightkit/agent-ui-core/lib/types/workspace-files";
import {
  decideExternalChange,
  getEditorBuffer,
  registerEditorBuffer,
  useEditorBufferStore,
  type EditorViewState,
} from "@rightkit/agent-ui-core/state/editorBufferStore";

export type EditorBufferStatus = "idle" | "loading" | "ready" | "stale" | "unsupported" | "error";

export interface UseEditorBufferOptions {
  config: DaemonConfig | null;
  /** Stable editor-tab identity. A path fallback is used when omitted. */
  bufferId?: string;
  workspaceRoot: string;
  path: string;
  /** Bumped by the workspace watcher to re-stat the active file. */
  reloadToken?: number;
}

export interface UseEditorBufferResult {
  buffer: ReturnType<typeof getEditorBuffer>;
  status: EditorBufferStatus;
  response: WorkspaceFileResponse | null;
  remoteConflict: WorkspaceFileResponse | null;
  reason: string | null;
  content: string;
  dirty: boolean;
  loaded: boolean;
  loading: boolean;
  externalChanged: boolean;
  viewState: EditorViewState | null;
  setContent: (content: string) => void;
  setViewState: (viewState: EditorViewState | null) => void;
  refresh: () => Promise<void>;
  /** Re-read the daemon file and intentionally replace the local draft. */
  reloadExternal: () => Promise<void>;
  /** Keep the local draft while dismissing the current external warning. */
  keepLocal: () => void;
  /** Restore the last loaded local baseline without touching the daemon. */
  revert: () => void;
  save: (content?: string) => Promise<WorkspaceFileSaveOutcome>;
}

function mapKind(kind: WorkspaceFileKind): EditorBufferStatus {
  switch (kind) {
    case "file":
      return "ready";
    case "directory":
    case "binary":
    case "tooLarge":
      return "unsupported";
    case "missing":
      return "error";
    default:
      return "idle";
  }
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

export function useEditorBuffer({
  config,
  bufferId,
  workspaceRoot,
  path,
  reloadToken = 0,
}: UseEditorBufferOptions): UseEditorBufferResult {
  const stableBufferId = useMemo(
    () => bufferId?.trim() || `${workspaceRoot}\u0000${path}`,
    [bufferId, path, workspaceRoot],
  );
  const buffer = useEditorBufferStore((state) => state.buffers[stableBufferId]);
  const ensureBuffer = useEditorBufferStore((state) => state.ensureBuffer);
  const updateContent = useEditorBufferStore((state) => state.updateContent);
  const setViewStateInStore = useEditorBufferStore((state) => state.setViewState);
  const setLoading = useEditorBufferStore((state) => state.setLoading);
  const hydrate = useEditorBufferStore((state) => state.hydrate);
  const preserveDirtyRemote = useEditorBufferStore((state) => state.preserveDirtyRemote);
  const markSaved = useEditorBufferStore((state) => state.markSaved);
  const restoreBaseline = useEditorBufferStore((state) => state.restoreBaseline);
  const clearExternalChange = useEditorBufferStore((state) => state.clearExternalChange);

  const [status, setStatus] = useState<EditorBufferStatus>("idle");
  const [reason, setReason] = useState<string | null>(null);
  const requestRef = useRef<{ controller: AbortController; serial: number } | null>(null);
  const serialRef = useRef(0);

  // Register before the load effect runs. Existing buffers are deliberately
  // not reset: that is what preserves unsaved content and view state across
  // an editor canvas unmount/remount and across tab switches.
  useEffect(() => {
    if (!stableBufferId) return;
    ensureBuffer(stableBufferId, workspaceRoot, path);
    setStatus("idle");
    setReason(null);
  }, [ensureBuffer, path, stableBufferId, workspaceRoot]);

  const load = useCallback(
    async (force: boolean): Promise<void> => {
      void reloadToken;
      if (!config || !workspaceRoot.trim() || !path.trim()) {
        requestRef.current?.controller.abort();
        setLoading(stableBufferId, false);
        setStatus("idle");
        setReason(null);
        return;
      }

      requestRef.current?.controller.abort();
      setLoading(stableBufferId, true);
      const controller = new AbortController();
      const serial = ++serialRef.current;
      requestRef.current = { controller, serial };
      setStatus("loading");
      setReason(null);

      try {
        const next = await getWorkspaceFile(config, workspaceRoot, path, {
          signal: controller.signal,
        });
        const current = getEditorBuffer(stableBufferId);
        if (controller.signal.aborted || serial !== serialRef.current || !current) return;

        if (!force && current.dirty) {
          const outcome = decideExternalChange(
            current.externalBaselineFingerprint ?? current.fingerprint,
            next.fingerprint,
            true,
          );
          if (
            outcome === "flag" ||
            !current.loaded
          ) {
            preserveDirtyRemote(stableBufferId, next);
            setStatus("stale");
          } else {
            setLoading(stableBufferId, false);
            setStatus("ready");
          }
          return;
        }

        hydrate(stableBufferId, next);
        setStatus(mapKind(next.kind));
      } catch (error) {
        if (controller.signal.aborted || serial !== serialRef.current) return;
        setLoading(stableBufferId, false);
        setStatus("error");
        setReason(errorMessage(error, "file read failed"));
      }
    },
    [config, hydrate, path, preserveDirtyRemote, reloadToken, setLoading, stableBufferId, workspaceRoot],
  );

  const refresh = useCallback(() => load(false), [load]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(
    () => () => {
      requestRef.current?.controller.abort();
      serialRef.current += 1;
    },
    [],
  );

  const reloadExternal = useCallback(async () => {
    const current = getEditorBuffer(stableBufferId);
    if (current?.remoteConflict) {
      hydrate(stableBufferId, current.remoteConflict);
      setStatus(mapKind(current.remoteConflict.kind));
      return;
    }
    await load(true);
  }, [hydrate, load, stableBufferId]);

  const setContent = useCallback(
    (content: string) => updateContent(stableBufferId, content),
    [stableBufferId, updateContent],
  );

  const setViewState = useCallback(
    (viewState: EditorViewState | null) => setViewStateInStore(stableBufferId, viewState),
    [setViewStateInStore, stableBufferId],
  );

  const keepLocal = useCallback(
    () => clearExternalChange(stableBufferId),
    [clearExternalChange, stableBufferId],
  );

  const revert = useCallback(
    () => restoreBaseline(stableBufferId),
    [restoreBaseline, stableBufferId],
  );

  const save = useCallback(
    async (nextContent?: string): Promise<WorkspaceFileSaveOutcome> => {
      if (nextContent !== undefined) updateContent(stableBufferId, nextContent);
      const current = getEditorBuffer(stableBufferId);
      if (!config) return { kind: "unsupported", reason: "daemon offline" };
      if (!current?.response || current.response.kind !== "file" || !current.fingerprint) {
        return { kind: "unsupported", reason: "file not loaded" };
      }

      try {
        const saved = await putWorkspaceFile(
          config,
          {
            workspaceRoot,
            path,
            content: current.content,
            expectedFingerprint: current.fingerprint,
            baseRevision: null,
          },
          {},
        );
        try {
          const refreshed = await getWorkspaceFile(config, workspaceRoot, path);
          hydrate(stableBufferId, refreshed);
          setStatus(mapKind(refreshed.kind));
        } catch (error) {
          // The write succeeded. Keep the new fingerprint/baseline even when
          // the follow-up read is temporarily unavailable.
          markSaved(stableBufferId, saved);
          setStatus("error");
          setReason(errorMessage(error, "post-save refresh failed"));
        }
        return { kind: "saved", response: saved };
      } catch (error) {
        const statusCode = (error as { status?: number }).status ?? 0;
        if (statusCode === 409) {
          useEditorBufferStore.getState().preserveDirtyRemote(stableBufferId, current.response);
          setStatus("stale");
          return { kind: "stale" };
        }
        if (statusCode === 403) {
          return { kind: "forbidden", reason: errorMessage(error, "forbidden") };
        }
        if (statusCode === 404) return { kind: "missing" };
        return { kind: "unsupported", reason: errorMessage(error, "file save failed") };
      }
    },
    [config, hydrate, markSaved, path, stableBufferId, updateContent, workspaceRoot],
  );

  return {
    buffer,
    status,
    response: buffer?.response ?? null,
    remoteConflict: buffer?.remoteConflict ?? null,
    reason,
    content: buffer?.content ?? "",
    dirty: buffer?.dirty ?? false,
    loaded: buffer?.loaded ?? false,
    loading: buffer?.loading ?? false,
    externalChanged: buffer?.externalChanged ?? false,
    viewState: buffer?.viewState ?? null,
    setContent,
    setViewState,
    refresh,
    reloadExternal,
    keepLocal,
    revert,
    save,
  };
}

// Keep the registration helper import live in this leaf's public seam: hub
// integrations that create a tab before mounting the hook can use the same
// donor-compatible lifecycle operation without importing Zustand internals.
export { registerEditorBuffer };
