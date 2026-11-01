// D1 — useWorkspaceFile hook.
//
// Loads a single file's content + metadata and exposes a typed `save`
// function that performs the fingerprint-guarded PUT. The hook tracks the
// last-seen fingerprint so callers can render a "stale buffer" warning
// when a watcher rescan invalidates the in-memory text.
//
// Draft-preserve contract (D1 fix):
//   When the owning viewer marks the file as dirty, the hook does NOT
//   overwrite the local content on a background reload (a watcher event
//   that bumps `reloadToken`). Instead the response is stored as the
//   `remoteConflict` source so the viewer can offer "Re-read" / "Keep
//   my draft" without ever silently clobbering the user's work.

import { useCallback, useEffect, useRef, useState } from "react";
import { getWorkspaceFile, putWorkspaceFile } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type {
  WorkspaceFileKind,
  WorkspaceFileResponse,
  WorkspaceFileSaveOutcome,
} from "@rightkit/agent-ui-core/lib/types/workspace-files";

export type WorkspaceFileStatus = "idle" | "loading" | "ready" | "stale" | "unsupported" | "error";

export interface UseWorkspaceFileOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  path: string;
  /** Bumped when an external change is observed; forces a reload. */
  reloadToken?: number;
  /**
   * Provided by the owning viewer. While true, the hook will not let a
   * background reload overwrite the local content; the reload is kept
   * as a "remote conflict" source for the conflict resolver.
   */
  isDirty?: () => boolean;
}

export interface UseWorkspaceFileResult {
  status: WorkspaceFileStatus;
  response: WorkspaceFileResponse | null;
  reason: string | null;
  remoteConflict: WorkspaceFileResponse | null;
  refresh: () => Promise<void>;
  save: (
    content: string,
    options?: { baseRevision?: string | null },
  ) => Promise<WorkspaceFileSaveOutcome>;
}

export function useWorkspaceFile({
  config,
  workspaceRoot,
  path,
  reloadToken = 0,
  isDirty,
}: UseWorkspaceFileOptions): UseWorkspaceFileResult {
  const [status, setStatus] = useState<WorkspaceFileStatus>("idle");
  const [response, setResponse] = useState<WorkspaceFileResponse | null>(null);
  const [reason, setReason] = useState<string | null>(null);
  const [remoteConflict, setRemoteConflict] = useState<WorkspaceFileResponse | null>(null);
  const inFlight = useRef<AbortController | null>(null);
  const responseRef = useRef<WorkspaceFileResponse | null>(null);
  const remoteConflictRef = useRef<WorkspaceFileResponse | null>(null);
  const isDirtyRef = useRef<(() => boolean) | undefined>(isDirty);
  responseRef.current = response;
  remoteConflictRef.current = remoteConflict;
  isDirtyRef.current = isDirty;

  // A watcher reload preserves a dirty draft. An explicit refresh (the
  // viewer's Re-read action) is authoritative and may replace that draft;
  // keeping those paths separate prevents Re-read from merely creating a
  // second conflict instead of resolving the first one.
  const load = useCallback(
    async (preserveDirty: boolean) => {
      if (!config || !workspaceRoot.trim() || !path.trim()) {
        setStatus("idle");
        setResponse(null);
        setRemoteConflict(null);
        setReason(null);
        return;
      }
      inFlight.current?.abort();
      const controller = new AbortController();
      inFlight.current = controller;
      setStatus("loading");
      setReason(null);
      try {
        const next = await getWorkspaceFile(config, workspaceRoot, path, {
          signal: controller.signal,
        });
        if (controller.signal.aborted) return;
        // Draft-preserve: watcher responses are held as the conflict source
        // rather than overwriting the local response. The live ref avoids
        // capturing a stale dirty value in the reload callback.
        if (preserveDirty && isDirtyRef.current?.()) {
          const prev = responseRef.current;
          const fingerprintChanged =
            prev?.fingerprint != null &&
            next.fingerprint != null &&
            prev.fingerprint !== next.fingerprint;
          if (fingerprintChanged) {
            setRemoteConflict(next);
            setStatus("stale");
          } else {
            setStatus(remoteConflictRef.current ? "stale" : mapKind(prev?.kind ?? next.kind));
          }
          return;
        }
        setRemoteConflict(null);
        setResponse(next);
        setReason(null);
        setStatus(mapKind(next.kind));
      } catch (error) {
        if (controller.signal.aborted) return;
        const message = (error as Error).message || "read failed";
        // Never erase an already loaded response on a transient read error;
        // the viewer can still preserve the draft and retry the read.
        setStatus("error");
        setReason(message);
      }
    },
    [config, workspaceRoot, path],
  );

  const refresh = useCallback(() => load(false), [load]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reloadToken is external refresh signal.
  useEffect(() => {
    void load(true);
    // reloadToken is intentionally a dependency: watcher events request a
    // fresh read while the preserveDirty branch protects an active draft.
  }, [load, reloadToken]);

  useEffect(
    () => () => {
      inFlight.current?.abort();
    },
    [],
  );

  const save = useCallback(
    async (
      content: string,
      options: { baseRevision?: string | null } = {},
    ): Promise<WorkspaceFileSaveOutcome> => {
      if (!config) return { kind: "unsupported", reason: "daemon offline" };
      const current = responseRef.current;
      if (current?.kind !== "file" || !current.fingerprint) {
        return { kind: "unsupported", reason: "file not loaded" };
      }
      try {
        const next = await putWorkspaceFile(
          config,
          {
            workspaceRoot,
            path,
            content,
            expectedFingerprint: current.fingerprint,
            baseRevision: options.baseRevision ?? null,
          },
          { signal: undefined },
        );
        // Successful save → re-read so the response fingerprint and metadata
        // are the daemon's post-write values, not an optimistic local copy.
        try {
          const refreshed = await getWorkspaceFile(config, workspaceRoot, path);
          setRemoteConflict(null);
          setResponse(refreshed);
          setReason(null);
          setStatus(mapKind(refreshed.kind));
        } catch (error) {
          const message = (error as Error).message || "post-save refresh failed";
          setStatus("error");
          setReason(message);
        }
        return { kind: "saved", response: next };
      } catch (error) {
        const statusCode = (error as { status?: number }).status ?? 0;
        if (statusCode === 409) {
          setStatus("stale");
          // Fetch the authoritative remote version so the conflict UI can
          // offer a real re-read/keep-local choice after a rejected write.
          try {
            const remote = await getWorkspaceFile(config, workspaceRoot, path);
            if (remote.fingerprint !== current.fingerprint) setRemoteConflict(remote);
          } catch {
            // The stale outcome remains actionable even if the follow-up read
            // is temporarily unavailable; the next explicit refresh retries.
          }
          return { kind: "stale" };
        }
        if (statusCode === 403) {
          return { kind: "forbidden", reason: (error as Error).message || "forbidden" };
        }
        if (statusCode === 404) {
          return { kind: "missing" };
        }
        return {
          kind: "unsupported",
          reason: (error as Error).message || "save failed",
        };
      }
    },
    [config, workspaceRoot, path],
  );

  return { status, response, reason, remoteConflict, refresh, save };
}

function mapKind(kind: WorkspaceFileKind): WorkspaceFileStatus {
  switch (kind) {
    case "file":
      return "ready";
    case "directory":
      return "unsupported";
    case "binary":
    case "tooLarge":
      return "unsupported";
    case "missing":
      return "error";
    default:
      return "idle";
  }
}
