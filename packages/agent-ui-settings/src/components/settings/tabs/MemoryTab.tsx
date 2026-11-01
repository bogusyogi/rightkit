import { useEffect, useRef, useState } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type { WorkspaceFileSaveOutcome } from "@rightkit/agent-ui-core/lib/types/workspace-files";
import { useMemoryFiles } from "@rightkit/agent-ui-hooks/hooks/useMemoryFiles";

interface MemoryTabProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
}

function saveOutcomeMessage(outcome: WorkspaceFileSaveOutcome): string {
  switch (outcome.kind) {
    case "saved":
      return "Saved";
    case "stale":
      return "File changed remotely. Re-read or keep your draft.";
    case "forbidden":
    case "unsupported":
      return outcome.reason;
    case "missing":
      return "CLAUDE.md no longer exists.";
  }
}

export default function MemoryTab({ config, workspaceRoot }: MemoryTabProps) {
  const memory = useMemoryFiles(config, workspaceRoot);
  const [confirmLeave, setConfirmLeave] = useState(false);
  const [saveState, setSaveState] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const previousWorkspace = useRef(workspaceRoot);

  useEffect(() => {
    const beforeUnload = (event: BeforeUnloadEvent) => {
      if (!memory.dirty) return;
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [memory.dirty]);

  useEffect(() => {
    if (previousWorkspace.current !== workspaceRoot && memory.dirty) {
      setConfirmLeave(true);
      setSaveState("Workspace changed; local draft is preserved until you choose.");
    }
    previousWorkspace.current = workspaceRoot;
  }, [workspaceRoot, memory.dirty]);

  const save = async () => {
    setSaving(true);
    try {
      setSaveState(saveOutcomeMessage(await memory.save()));
    } finally {
      setSaving(false);
    }
  };

  if (!config || !workspaceRoot.trim()) {
    return (
      <section className="flex max-w-[720px] flex-col gap-2 p-6">
        <h3 className="text-[16px] font-bold text-text">Project memory</h3>
        <p className="text-[12px] text-text-3">
          Open a connected workspace to edit its existing CLAUDE.md.
        </p>
      </section>
    );
  }
  if (memory.status === "loading" || memory.status === "idle") {
    return <section className="p-6 text-sm text-text-3">Loading CLAUDE.md…</section>;
  }
  if (!memory.response) {
    return (
      <section className="flex max-w-[720px] flex-col gap-2 p-6">
        <h3 className="text-[16px] font-bold text-text">Project memory</h3>
        <p role="alert" className="text-[12px] text-danger">
          {memory.reason ?? "CLAUDE.md is unavailable."}
        </p>
      </section>
    );
  }
  if (memory.response.kind !== "file") {
    const unavailableReason =
      memory.response.kind === "missing"
        ? "Project-root CLAUDE.md does not exist. Create it in the workspace before editing."
        : (memory.response.reason ?? "Existing project-root CLAUDE.md is not readable.");
    return (
      <section className="flex max-w-[720px] flex-col gap-2 p-6">
        <h3 className="text-[16px] font-bold text-text">Project memory</h3>
        <p role="status" className="text-[12px] text-text-3">
          {unavailableReason}
        </p>
      </section>
    );
  }

  return (
    <section className="flex min-h-0 max-w-[900px] flex-1 flex-col gap-3 p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-[16px] font-bold text-text">Project memory</h3>
          <p className="text-[12px] text-text-3">
            {memory.path} · edits existing project-root file only
          </p>
        </div>
        {memory.dirty ? <span className="text-[11px] text-warning">Unsaved changes</span> : null}
      </div>
      {memory.status === "error" ? (
        <p role="alert" className="text-[12px] text-danger">
          {memory.reason ?? "Latest disk read failed; local content is preserved."}
        </p>
      ) : null}
      {memory.workspaceMismatch ? (
        <p role="alert" className="text-[12px] text-warning">
          Draft belongs to {memory.draftWorkspaceRoot}. Switch back or Re-read to discard before
          saving in this workspace.
        </p>
      ) : null}
      {memory.remoteConflict ? (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-2 rounded-md border border-warning/40 bg-warning/10 p-3 text-[12px] text-text-2"
        >
          <span>CLAUDE.md changed on disk.</span>
          <button
            type="button"
            className="rounded border border-line px-2 py-1"
            onClick={() => void memory.reRead()}
          >
            Re-read
          </button>
          <button
            type="button"
            className="rounded border border-line px-2 py-1"
            onClick={() => void memory.keepLocal()}
          >
            Keep local
          </button>
        </div>
      ) : null}
      <textarea
        aria-label="Project CLAUDE.md"
        className="min-h-[360px] flex-1 resize-y rounded-md border border-line bg-panel p-3 font-mono text-[12px] text-text outline-none focus:border-accent"
        value={memory.content}
        onChange={(event) => memory.edit(event.currentTarget.value)}
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          className="rounded-md bg-accent px-3 py-2 text-[12px] font-semibold text-on-accent disabled:opacity-40"
          disabled={!memory.dirty || memory.workspaceMismatch || saving}
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save"}
        </button>
        <button
          type="button"
          className="rounded-md border border-line px-3 py-2 text-[12px]"
          disabled={!memory.dirty}
          onClick={() => setConfirmLeave(true)}
        >
          Re-read
        </button>
        {saveState ? (
          <span role="status" className="text-[11px] text-text-3">
            {saveState}
          </span>
        ) : null}
      </div>
      {confirmLeave ? (
        <div
          role="dialog"
          aria-label="Discard draft confirmation"
          className="rounded-md border border-warning/40 bg-warning/10 p-3 text-[12px] text-text-2"
        >
          <p>Re-read and discard local changes?</p>
          <div className="mt-2 flex gap-2">
            <button
              type="button"
              className="rounded border border-line px-2 py-1"
              onClick={() => {
                setConfirmLeave(false);
                void memory.reRead();
              }}
            >
              Re-read
            </button>
            <button
              type="button"
              className="rounded border border-line px-2 py-1"
              onClick={() => setConfirmLeave(false)}
            >
              Keep local
            </button>
          </div>
        </div>
      ) : null}
    </section>
  );
}
