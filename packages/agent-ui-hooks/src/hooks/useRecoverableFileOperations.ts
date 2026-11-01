import { useCallback, useMemo, useRef, useState } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type { WorkspaceEntry } from "@rightkit/agent-ui-core/lib/types/workspace-files";
import { hostAppName } from "../appName";

/** The daemon's recoverable-trash receipt for one workspace entry. */
export interface DeletedWorkspaceEntry {
  name: string;
  path: string;
  originalPath: string;
  trashPath: string;
  kind: WorkspaceEntry["kind"];
}

export interface RecoverableFileOperationsClient {
  createFile: (
    workspaceRoot: string,
    parentPath: string | null,
    name: string,
  ) => Promise<WorkspaceEntry>;
  createDirectory: (
    workspaceRoot: string,
    parentPath: string | null,
    name: string,
  ) => Promise<WorkspaceEntry>;
  renameEntry: (
    workspaceRoot: string,
    path: string,
    name: string,
  ) => Promise<WorkspaceEntry>;
  trashEntry: (
    workspaceRoot: string,
    path: string,
  ) => Promise<DeletedWorkspaceEntry>;
  restoreDeletedEntries: (
    workspaceRoot: string,
    entries: DeletedWorkspaceEntry[],
  ) => Promise<WorkspaceEntry[]>;
  revealEntry: (workspaceRoot: string, path: string) => Promise<void>;
}

export interface UseRecoverableFileOperationsOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  /** Injected in tests or by the eventual typed daemon connector. */
  client?: RecoverableFileOperationsClient;
  onChanged?: () => void;
}

export interface UseRecoverableFileOperationsResult {
  createFile: (parentPath: string | null, name: string) => Promise<WorkspaceEntry | null>;
  createDirectory: (parentPath: string | null, name: string) => Promise<WorkspaceEntry | null>;
  renameEntry: (path: string, name: string) => Promise<WorkspaceEntry | null>;
  trashEntries: (entries: WorkspaceEntry[]) => Promise<void>;
  restoreLastDeletedEntries: () => Promise<WorkspaceEntry[]>;
  revealEntry: (path: string) => Promise<void>;
  undo: () => Promise<WorkspaceEntry[]>;
  undoCount: number;
  canUndo: boolean;
  pending: boolean;
  error: string | null;
  notice: string | null;
  clearMessage: () => void;
}

const MAX_DELETED_BATCHES = 20;

type Mutation = <T>(work: () => Promise<T>) => Promise<T | null>;

/**
 * Sinew's FileTree keeps the last twenty delete batches, rather than only one
 * undo slot. The receipt is retained until restore succeeds, so a failed
 * restore remains recoverable instead of silently losing the undo action.
 */
export function useRecoverableFileOperations({
  config,
  workspaceRoot,
  client,
  onChanged,
}: UseRecoverableFileOperationsOptions): UseRecoverableFileOperationsResult {
  const stackRef = useRef<DeletedWorkspaceEntry[][]>([]);
  const [undoCount, setUndoCount] = useState(0);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const daemonClient = useMemo(
    () => client ?? (config ? createWorkspaceFileOperationsClient(config) : null),
    [client, config],
  );

  const run = useCallback<Mutation>(
    async <T,>(work: () => Promise<T>) => {
      if (!daemonClient || !workspaceRoot.trim()) {
        setError(`Workspace file operations are unavailable while ${hostAppName()} is offline.`);
        return null;
      }
      setPending(true);
      setError(null);
      setNotice(null);
      try {
        return await work();
      } catch (cause) {
        setError(errorMessage(cause));
        return null;
      } finally {
        setPending(false);
      }
    },
    [daemonClient, workspaceRoot],
  );

  const createFile = useCallback(
    (parentPath: string | null, name: string) =>
      run(async () => {
        const entry = await daemonClient!.createFile(workspaceRoot, parentPath, name);
        setNotice(`Created ${entry.name}`);
        onChanged?.();
        return entry;
      }),
    [daemonClient, onChanged, run, workspaceRoot],
  );

  const createDirectory = useCallback(
    (parentPath: string | null, name: string) =>
      run(async () => {
        const entry = await daemonClient!.createDirectory(workspaceRoot, parentPath, name);
        setNotice(`Created ${entry.name}`);
        onChanged?.();
        return entry;
      }),
    [daemonClient, onChanged, run, workspaceRoot],
  );

  const renameEntry = useCallback(
    (path: string, name: string) =>
      run(async () => {
        const entry = await daemonClient!.renameEntry(workspaceRoot, path, name);
        setNotice(`Renamed to ${entry.name}`);
        onChanged?.();
        return entry;
      }),
    [daemonClient, onChanged, run, workspaceRoot],
  );

  const trashEntries = useCallback(
    async (entries: WorkspaceEntry[]) => {
      if (!entries.length) return;
      const deleted: DeletedWorkspaceEntry[] = [];
      const result = await run(async () => {
        for (const entry of entries) {
          deleted.push(await daemonClient!.trashEntry(workspaceRoot, entry.path));
        }
      });
      // Preserve a partially completed batch too: every successful trash has a
      // daemon receipt and can be restored even if a later item failed.
      if (deleted.length) {
        pushDeletedBatch(stackRef.current, deleted);
        setUndoCount(stackRef.current.length);
        onChanged?.();
      }
      if (result !== null) setNotice(`${deleted.length} item${deleted.length === 1 ? "" : "s"} moved to trash`);
    },
    [daemonClient, onChanged, run, workspaceRoot],
  );

  const restoreLastDeletedEntries = useCallback(async () => {
    const batch = stackRef.current.pop();
    if (!batch?.length) return [];
    if (!daemonClient || !workspaceRoot.trim()) {
      pushDeletedBatch(stackRef.current, batch);
      setUndoCount(stackRef.current.length);
      setError(`Workspace file operations are unavailable while ${hostAppName()} is offline.`);
      return [];
    }
    setUndoCount(stackRef.current.length);
    setPending(true);
    setError(null);
    setNotice(null);
    try {
      const restored = await daemonClient.restoreDeletedEntries(workspaceRoot, batch);
      setNotice(`Restored ${restored.length} item${restored.length === 1 ? "" : "s"}`);
      onChanged?.();
      return restored;
    } catch (cause) {
      // Sinew's restore path puts the batch back on failure. Do the same so
      // transient daemon failures never destroy the user's recovery option.
      pushDeletedBatch(stackRef.current, batch);
      setUndoCount(stackRef.current.length);
      setError(errorMessage(cause));
      return [];
    } finally {
      setPending(false);
    }
  }, [daemonClient, onChanged, workspaceRoot]);

  const revealEntry = useCallback(
    (path: string) =>
      run(async () => {
        await daemonClient!.revealEntry(workspaceRoot, path);
        return undefined;
      }),
    [daemonClient, run, workspaceRoot],
  );

  const clearMessage = useCallback(() => {
    setError(null);
    setNotice(null);
  }, []);

  return {
    createFile,
    createDirectory,
    renameEntry,
    trashEntries,
    restoreLastDeletedEntries,
    revealEntry: async (path) => {
      await revealEntry(path);
    },
    undo: restoreLastDeletedEntries,
    undoCount,
    canUndo: undoCount > 0 && !pending,
    pending,
    error,
    notice,
    clearMessage,
  };
}

export function pushDeletedBatch(
  stack: DeletedWorkspaceEntry[][],
  batch: DeletedWorkspaceEntry[],
): void {
  if (!batch.length) return;
  stack.push(batch);
  if (stack.length > MAX_DELETED_BATCHES) stack.splice(0, stack.length - MAX_DELETED_BATCHES);
}

export function createWorkspaceFileOperationsClient(
  config: DaemonConfig,
): RecoverableFileOperationsClient {
  return {
    createFile: (workspaceRoot, parentPath, name) =>
      workspaceMutation<WorkspaceEntry>(config, "/workspace/entries", {
        workspaceRoot,
        parentPath,
        name,
        kind: "file",
      }),
    createDirectory: (workspaceRoot, parentPath, name) =>
      workspaceMutation<WorkspaceEntry>(config, "/workspace/entries", {
        workspaceRoot,
        parentPath,
        name,
        kind: "directory",
      }),
    renameEntry: (workspaceRoot, path, name) =>
      workspaceMutation<WorkspaceEntry>(config, "/workspace/entries/rename", {
        workspaceRoot,
        path,
        name,
      }),
    trashEntry: (workspaceRoot, path) =>
      workspaceMutation<DeletedWorkspaceEntry>(config, "/workspace/trash", {
        workspaceRoot,
        path,
      }),
    restoreDeletedEntries: (workspaceRoot, entries) =>
      workspaceMutation<WorkspaceEntry[]>(config, "/workspace/trash/restore", {
        workspaceRoot,
        entries,
      }),
    revealEntry: (workspaceRoot, path) =>
      workspaceMutation<void>(config, "/workspace/reveal", { workspaceRoot, path }),
  };
}

async function workspaceMutation<T>(config: DaemonConfig, path: string, body: unknown): Promise<T> {
  const response = await fetch(`${config.url.replace(/\/+$/, "")}${path}`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/json",
      ...(config.token ? { authorization: `Bearer ${config.token}` } : {}),
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(detail || `workspace request failed (${response.status})`);
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
