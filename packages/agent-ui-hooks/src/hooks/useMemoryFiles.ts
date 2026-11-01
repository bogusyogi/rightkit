import { useCallback, useEffect, useRef, useState } from "react";
import { useWorkspaceFile } from "./useWorkspaceFile";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

const MEMORY_PATH = "CLAUDE.md";

export function useMemoryFiles(config: DaemonConfig | null, workspaceRoot: string) {
  const [draft, setDraft] = useState<string | null>(null);
  const [draftWorkspaceRoot, setDraftWorkspaceRoot] = useState<string | null>(null);
  const dirtyRef = useRef(false);
  const [reloadToken, setReloadToken] = useState(0);
  const file = useWorkspaceFile({
    config,
    workspaceRoot,
    path: MEMORY_PATH,
    reloadToken,
    isDirty: () => dirtyRef.current,
  });
  const content = draft ?? file.response?.content ?? "";
  const workspaceMismatch =
    draft !== null && draftWorkspaceRoot !== null && draftWorkspaceRoot !== workspaceRoot;
  const dirty = draft !== null && (workspaceMismatch || draft !== (file.response?.content ?? ""));

  useEffect(() => {
    dirtyRef.current = dirty;
  }, [dirty]);

  const edit = useCallback(
    (value: string) => {
      dirtyRef.current = true;
      setDraftWorkspaceRoot((current) => current ?? workspaceRoot);
      setDraft(value);
    },
    [workspaceRoot],
  );
  const reRead = useCallback(async () => {
    dirtyRef.current = false;
    setDraft(null);
    setDraftWorkspaceRoot(null);
    await file.refresh();
  }, [file.refresh]);
  const keepLocal = useCallback(async () => {
    setDraft((current) => current ?? file.response?.content ?? "");
    dirtyRef.current = true;
    if (workspaceMismatch) return;
    // Refresh the authoritative response/fingerprint while leaving the local
    // draft intact. A later Save now compares against the latest disk state
    // instead of repeating the same stale-fingerprint conflict forever.
    await file.refresh();
  }, [file.refresh, file.response?.content, workspaceMismatch]);
  const save = useCallback(async () => {
    if (workspaceMismatch) {
      return {
        kind: "unsupported" as const,
        reason: "Draft belongs to another workspace. Switch back or re-read before saving.",
      };
    }
    const outcome = await file.save(content);
    if (outcome.kind === "saved") {
      dirtyRef.current = false;
      setDraft(null);
      setDraftWorkspaceRoot(null);
    }
    return outcome;
  }, [content, file.save, workspaceMismatch]);

  const reload = useCallback(() => setReloadToken((value) => value + 1), []);

  return {
    ...file,
    path: MEMORY_PATH,
    content,
    dirty,
    draftWorkspaceRoot,
    workspaceMismatch,
    edit,
    reRead,
    keepLocal,
    save,
    reload,
  };
}

export { MEMORY_PATH };
