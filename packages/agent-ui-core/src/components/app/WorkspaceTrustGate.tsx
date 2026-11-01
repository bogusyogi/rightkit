import Icon from "../Icon";
import type { WorkspaceTrustLevel, WorkspaceTrustStatus } from "../../lib/types";
import { hostAppName } from "../../appName";
export default function WorkspaceTrustGate({
  workspaceRoot,
  trust,
  pending,
  onSetWorkspaceTrust,
}: {
  workspaceRoot: string;
  trust: WorkspaceTrustStatus;
  pending: WorkspaceTrustLevel | null;
  onSetWorkspaceTrust: (trust: WorkspaceTrustLevel) => void;
}) {
  const actionClass = (level: WorkspaceTrustLevel) => {
    const selected = !trust.requiresTrust && trust.trust === level;
    return selected
      ? "inline-flex h-8 items-center gap-1 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
      : "inline-flex h-8 items-center gap-1 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 hover:bg-bg hover:text-text disabled:opacity-40";
  };
  const label = (level: WorkspaceTrustLevel, idle: string) =>
    pending === level ? "Saving…" : idle;

  return (
    <section
      data-testid="workspace-trust-gate"
      className="grid gap-3 border-b border-line bg-panel px-6 py-4 md:grid-cols-[1fr_auto] md:items-center"
    >
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <span className="inline-flex items-center gap-1 rounded-full border border-line bg-bg px-2 py-0.5 text-[11px] font-semibold uppercase text-text-2">
            <Icon name="key" /> Workspace trust
          </span>
          <span className="text-[12px] text-text-3 truncate">{workspaceRoot}</span>
        </div>
        <p className="mt-2 text-[13px] leading-relaxed text-text-2">
          This project is currently {trust.trust}. Choose how {hostAppName()} should treat file edits,
          shell commands, and unattended work in this workspace.
        </p>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          className={actionClass("trusted")}
          aria-pressed={!trust.requiresTrust && trust.trust === "trusted"}
          disabled={pending !== null}
          onClick={() => onSetWorkspaceTrust("trusted")}
        >
          <Icon name="check" /> {label("trusted", "Trust")}
        </button>
        <button
          type="button"
          className={actionClass("sandboxed")}
          aria-pressed={!trust.requiresTrust && trust.trust === "sandboxed"}
          disabled={pending !== null}
          onClick={() => onSetWorkspaceTrust("sandboxed")}
        >
          <Icon name="panel" /> {label("sandboxed", "Sandbox")}
        </button>
        <button
          type="button"
          className={actionClass("locked")}
          aria-pressed={!trust.requiresTrust && trust.trust === "locked"}
          disabled={pending !== null}
          onClick={() => onSetWorkspaceTrust("locked")}
        >
          <Icon name="lock" /> {label("locked", "Lock")}
        </button>
      </div>
    </section>
  );
}
