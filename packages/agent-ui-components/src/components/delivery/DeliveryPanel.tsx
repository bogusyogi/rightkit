// D4 — DeliveryPanel: branch / worktree / stage / commit / push / PR /
// checks / failing logs with visible commands and errors.
//
// Component contract:
//   - props: workspaceRoot + config, optional initial host kind
//   - state from `useDeliveryStore` (action + history + snapshot)
//   - commands are surfaced as visible, raw strings (per Wave 5 rules)
//   - PR + check + thread data flow through the typed client, never faked
//   - honest "unavailable" surface when the host kind is typed but not
//     runtime-supported, or when the gh binary is missing
//
// This component owns only D4 surface area. It does NOT edit AppBody,
// WorkbenchContent, TitleBar, or Settings.

import { useEffect, useMemo, useState } from "react";

import { ActionDialog, Select } from "@rightkit/react-shell/react";
import { useDeliveryStore } from "@rightkit/agent-ui-core/state/deliveryStore";
import type { DaemonConfig, DeliveryHostKind } from "@rightkit/agent-ui-core/lib/types";

interface DeliveryPanelProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
  initialHost?: DeliveryHostKind;
}

const HOST_OPTIONS: Array<{ value: DeliveryHostKind; label: string }> = [
  { value: "local", label: "Local" },
  { value: "worktree", label: "Worktree" },
  { value: "ssh", label: "SSH" },
  { value: "wsl", label: "WSL" },
  { value: "configuredRemote", label: "Configured remote" },
];

function statusLabel(status: string): string {
  switch (status) {
    case "exited":
      return "exited";
    case "killed":
      return "killed";
    case "spawnFailed":
      return "spawn failed";
    case "cancelled":
      return "cancelled";
    case "authorityDenied":
      return "authority denied";
    default:
      return status;
  }
}

export default function DeliveryPanel({
  config,
  workspaceRoot,
  initialHost = "local",
}: DeliveryPanelProps) {
  const snapshot = useDeliveryStore((state) => state.snapshot);
  const snapshotPending = useDeliveryStore((state) => state.snapshotPending);
  const history = useDeliveryStore((state) => state.history);
  const actionPending = useDeliveryStore((state) => state.actionPending);
  const error = useDeliveryStore((state) => state.error);
  const loadSnapshot = useDeliveryStore((state) => state.loadSnapshot);
  const loadBranches = useDeliveryStore((state) => state.loadBranches);
  const loadWorktrees = useDeliveryStore((state) => state.loadWorktrees);
  const createWorktreeAction = useDeliveryStore((state) => state.createWorktree);
  const stagePathsAction = useDeliveryStore((state) => state.stagePaths);
  const unstagePathsAction = useDeliveryStore((state) => state.unstagePaths);
  const commitChangesAction = useDeliveryStore((state) => state.commitChanges);
  const pushBranchAction = useDeliveryStore((state) => state.pushBranch);
  const pullBranchAction = useDeliveryStore((state) => state.pullBranch);
  const createPullRequestAction = useDeliveryStore((state) => state.createPullRequest);
  const loadPullRequestAction = useDeliveryStore((state) => state.loadPullRequest);
  const listChecksAction = useDeliveryStore((state) => state.listPullRequestChecks);
  const rerunCheckAction = useDeliveryStore((state) => state.rerunCheck);
  const loadCheckLogsAction = useDeliveryStore((state) => state.loadCheckLogs);
  const listThreadsAction = useDeliveryStore((state) => state.listPullRequestThreads);
  const clearHistory = useDeliveryStore((state) => state.clearHistory);

  const [host, setHost] = useState<DeliveryHostKind>(initialHost);
  const [branchInput, setBranchInput] = useState("");
  const [baseInput, setBaseInput] = useState("");
  const [stageInput, setStageInput] = useState("");
  const [commitMessage, setCommitMessage] = useState("");
  const [prTitle, setPrTitle] = useState("");
  const [prBody, setPrBody] = useState("");
  const [prId, setPrId] = useState("");
  const [checkId, setCheckId] = useState("");
  const [confirmAction, setConfirmAction] = useState<"push" | "pr-create" | null>(null);

  useEffect(() => {
    if (config && workspaceRoot.trim()) {
      void loadSnapshot(config, workspaceRoot, host);
    }
  }, [config, workspaceRoot, host, loadSnapshot]);

  const capabilities = snapshot?.capabilities ?? [];
  const hostUnavailable = useMemo(
    () =>
      snapshot
        ? !snapshot.hostAvailable
          ? (snapshot.hostUnavailableReason ?? "Host unavailable")
          : null
        : null,
    [snapshot],
  );

  const gitCapability = capabilities.find((c) => c.kind === "git");
  const prCapability = capabilities.find((c) => c.kind === "pullRequest");
  const checksCapability = capabilities.find((c) => c.kind === "checks");
  const logsCapability = capabilities.find((c) => c.kind === "failingLogs");
  const commandCapability = capabilities.find((c) => c.kind === "backgroundCommand");

  return (
    <section
      aria-label="Delivery"
      className="grid gap-3 rounded-xl border border-line bg-panel p-3"
    >
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold text-text">Delivery</h2>
          <p className="mt-0.5 text-[12px] text-text-3">
            {hostUnavailable
              ? "This workspace is not ready for delivery actions."
              : gitCapability?.available
                ? "Commit and sync changes when they are ready."
                : "Git actions are unavailable for this workspace."}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Select<DeliveryHostKind>
            value={host}
            onChange={setHost}
            label="Delivery host"
            options={HOST_OPTIONS}
          />
          <span className="rounded-full border border-line px-2 py-1 text-[11px] text-text-3">
            {snapshotPending ? "Checking" : `${snapshot?.authority.trustLabel ?? "untrusted"}`}
          </span>
        </div>
      </header>

      {hostUnavailable ? (
        <div
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-2"
          role="alert"
        >
          <p className="font-semibold">Delivery is unavailable</p>
          <p className="mt-0.5 text-text-3">{hostUnavailable}</p>
        </div>
      ) : null}

      <div className="rounded-lg border border-line bg-bg p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <h3 className="text-[12px] font-semibold text-text">Commit and sync</h3>
            <p className="mt-0.5 text-[11px] text-text-3">
              Changes are reviewed before you commit. Sync only when you are ready.
            </p>
          </div>
          <div className="flex gap-2">
            <button
              type="button"
              disabled={!config || !gitCapability?.available || actionPending === "pull"}
              onClick={() => config && void pullBranchAction(config, workspaceRoot)}
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              Pull
            </button>
            <button
              type="button"
              disabled={!config || !gitCapability?.available || actionPending === "push"}
              onClick={() => setConfirmAction("push")}
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              Push
            </button>
          </div>
        </div>
        <div className="mt-3 flex flex-wrap gap-2">
          <input
            type="text"
            aria-label="Commit message"
            placeholder="Describe the change"
            value={commitMessage}
            onChange={(event) => setCommitMessage(event.target.value)}
            className="h-9 min-w-[24ch] flex-1 rounded-lg border border-line bg-panel px-3 text-[12px] text-text"
          />
          <button
            type="button"
            disabled={
              !config ||
              !gitCapability?.available ||
              !commitMessage.trim() ||
              actionPending === "commit"
            }
            onClick={() =>
              config &&
              void commitChangesAction(config, { workspaceRoot, message: commitMessage.trim() })
            }
            className="h-9 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
          >
            Commit changes
          </button>
        </div>
      </div>

      <details
        className="rounded-lg border border-line bg-bg"
        data-testid="delivery-advanced-actions"
      >
        <summary className="cursor-pointer px-3 py-2 text-[12px] font-semibold text-text-2">
          Branch, worktree &amp; pull request tools
        </summary>
        <div className="grid gap-3 border-t border-line p-3">
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              disabled={!config || !gitCapability?.available || actionPending === "branches"}
              onClick={() => config && void loadBranches(config, workspaceRoot)}
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              List branches
            </button>
            <button
              type="button"
              disabled={!config || !gitCapability?.available || actionPending === "worktrees"}
              onClick={() => config && void loadWorktrees(config, workspaceRoot)}
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              List worktrees
            </button>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <input
              type="text"
              aria-label="New worktree branch"
              placeholder="New worktree branch"
              value={branchInput}
              onChange={(event) => setBranchInput(event.target.value)}
              className="h-8 min-w-[12ch] rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
            />
            <input
              type="text"
              aria-label="New worktree base"
              placeholder="Base branch (optional)"
              value={baseInput}
              onChange={(event) => setBaseInput(event.target.value)}
              className="h-8 min-w-[12ch] rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
            />
            <button
              type="button"
              disabled={!config || !branchInput.trim() || actionPending === "create-worktree"}
              onClick={() =>
                config &&
                void createWorktreeAction(config, {
                  workspaceRoot,
                  branch: branchInput.trim(),
                  base: baseInput.trim() || undefined,
                })
              }
              className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
            >
              Create worktree
            </button>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <input
              type="text"
              aria-label="Paths to stage"
              placeholder="Paths to stage (space-separated)"
              value={stageInput}
              onChange={(event) => setStageInput(event.target.value)}
              className="h-8 min-w-[24ch] flex-1 rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
            />
            <button
              type="button"
              disabled={!config || !stageInput.trim() || actionPending === "stage"}
              onClick={() =>
                config &&
                void stagePathsAction(
                  config,
                  workspaceRoot,
                  stageInput.trim().split(/\s+/).filter(Boolean),
                )
              }
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              Stage
            </button>
            <button
              type="button"
              disabled={!config || !stageInput.trim() || actionPending === "unstage"}
              onClick={() =>
                config &&
                void unstagePathsAction(
                  config,
                  workspaceRoot,
                  stageInput.trim().split(/\s+/).filter(Boolean),
                )
              }
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              Unstage
            </button>
          </div>
          <div className="grid gap-2 rounded-lg border border-line p-2">
            <p className="text-[11px] font-semibold text-text-2">Pull request</p>
            <div className="flex flex-wrap gap-2">
              <input
                type="text"
                aria-label="Pull request title"
                placeholder="PR title"
                value={prTitle}
                onChange={(event) => setPrTitle(event.target.value)}
                className="h-8 min-w-[16ch] rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
              />
              <input
                type="text"
                aria-label="Pull request body"
                placeholder="PR body"
                value={prBody}
                onChange={(event) => setPrBody(event.target.value)}
                className="h-8 min-w-[24ch] flex-1 rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
              />
              <button
                type="button"
                disabled={!config || !prCapability?.available || actionPending === "pr-create"}
                onClick={() => setConfirmAction("pr-create")}
                className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
              >
                Open PR
              </button>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <input
                type="text"
                aria-label="Pull request id"
                placeholder="PR id"
                value={prId}
                onChange={(event) => setPrId(event.target.value)}
                className="h-8 min-w-[10ch] rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
              />
              <button
                type="button"
                disabled={!config || !prId.trim() || actionPending === "pr-view"}
                onClick={() =>
                  config && void loadPullRequestAction(config, workspaceRoot, prId.trim())
                }
                className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
              >
                View PR
              </button>
              <button
                type="button"
                disabled={!config || !prId.trim() || actionPending === "pr-checks"}
                onClick={() => config && void listChecksAction(config, workspaceRoot, prId.trim())}
                className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
              >
                List checks
              </button>
              <button
                type="button"
                disabled={!config || !prId.trim() || actionPending === "pr-threads"}
                onClick={() => config && void listThreadsAction(config, workspaceRoot, prId.trim())}
                className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
              >
                List threads
              </button>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <input
                type="text"
                aria-label="Check id"
                placeholder="check id"
                value={checkId}
                onChange={(event) => setCheckId(event.target.value)}
                className="h-8 min-w-[12ch] rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
              />
              <button
                type="button"
                disabled={
                  !config ||
                  !prId.trim() ||
                  !checkId.trim() ||
                  !logsCapability?.available ||
                  actionPending === "pr-check-logs"
                }
                onClick={() =>
                  config &&
                  void loadCheckLogsAction(config, workspaceRoot, prId.trim(), checkId.trim())
                }
                className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
              >
                Failing logs
              </button>
              <button
                type="button"
                disabled={
                  !config ||
                  !prId.trim() ||
                  !checkId.trim() ||
                  !checksCapability?.available ||
                  actionPending === "pr-rerun"
                }
                onClick={() =>
                  config &&
                  void rerunCheckAction(config, workspaceRoot, prId.trim(), checkId.trim())
                }
                className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
              >
                Rerun
              </button>
            </div>
          </div>
        </div>
      </details>

      <details className="rounded-lg border border-line bg-bg" data-testid="delivery-capabilities">
        <summary className="cursor-pointer px-3 py-2 text-[12px] font-semibold text-text-2">
          Capability details
        </summary>
        <div className="grid gap-2 border-t border-line p-3">
          <p className="text-[11px] text-text-3">
            {snapshot?.authority.source ?? "unknown source"} · checked{" "}
            {snapshot?.checkedAt ?? "not yet"}
          </p>
          <ul className="grid grid-cols-2 gap-2">
            {capabilities.map((capability) => (
              <li
                key={capability.kind}
                className="rounded-md border border-line bg-panel px-2 py-1 text-[11px]"
              >
                <span className="font-semibold text-text-2">{capability.kind}</span>
                <span className={capability.available ? "ml-1 text-accent-2" : "ml-1 text-text-3"}>
                  {capability.available ? "ready" : (capability.reason ?? "unavailable")}
                </span>
              </li>
            ))}
          </ul>
          {commandCapability ? (
            <p className="text-[11px] text-text-3">
              Background commands:{" "}
              {commandCapability.available
                ? "routed through the shell lane"
                : (commandCapability.reason ?? "unavailable")}
            </p>
          ) : null}
        </div>
      </details>

      {error ? (
        <p
          role="alert"
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text"
        >
          {error}
        </p>
      ) : null}

      <details className="rounded-lg border border-line bg-bg">
        <summary className="cursor-pointer px-3 py-2 text-[12px] font-semibold text-text-2">
          Recent activity{" "}
          <span className="ml-1 font-normal text-text-3">
            {history.length
              ? `${history.length} command${history.length === 1 ? "" : "s"}`
              : "No commands yet"}
          </span>
        </summary>
        <div className="grid gap-2 border-t border-line p-3">
          <div className="flex items-center justify-between">
            <h3 className="text-[11px] text-text-3">Commands run for this workspace</h3>
            <button
              type="button"
              disabled={history.length === 0}
              onClick={clearHistory}
              className="h-7 rounded-md border border-line bg-bg px-2 text-[11px] text-text-3 hover:text-text disabled:opacity-40"
            >
              Clear
            </button>
          </div>
          <ul className="grid gap-2" data-testid="delivery-history">
            {history.length === 0 ? (
              <li className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-3">
                No commands yet. Run a Git or PR action above; visible commands and their exit
                status will land here.
              </li>
            ) : null}
            {history.map((row) => (
              <li
                key={row.commandId}
                className="grid gap-1 rounded-lg border border-line bg-bg p-3 text-[12px]"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-semibold uppercase text-text-2">{row.actionLabel}</span>
                  <span
                    className={
                      row.status === "exited" && row.exitCode === 0
                        ? "text-accent-2"
                        : "text-text-3"
                    }
                  >
                    {statusLabel(row.status)}
                    {row.exitCode !== null ? ` · exit ${row.exitCode}` : ""}
                  </span>
                  <span className="text-text-3">· {row.startedAt}</span>
                </div>
                <code className="block overflow-x-auto rounded-md bg-panel px-2 py-1 font-mono text-[11px] text-text">
                  {row.command} {row.args.join(" ")}
                </code>
                {row.stdoutTail ? (
                  <pre className="max-h-32 overflow-auto whitespace-pre-wrap rounded-md bg-panel px-2 py-1 text-[11px] text-text">
                    {row.stdoutTail}
                  </pre>
                ) : null}
                {row.stderrTail ? (
                  <pre className="max-h-32 overflow-auto whitespace-pre-wrap rounded-md bg-panel px-2 py-1 text-[11px] text-text">
                    {row.stderrTail}
                  </pre>
                ) : null}
              </li>
            ))}
          </ul>
        </div>
      </details>

      {confirmAction === "push" ? (
        <ActionDialog
          title="Push to remote?"
          description="This sends your local commits to the remote so they become visible to everyone with access. It is outward-facing and cannot be undone from this panel."
          role="alertdialog"
          dismissible={actionPending !== "push"}
          onClose={() => setConfirmAction(null)}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: actionPending === "push",
              onClick: () => setConfirmAction(null),
            },
            {
              id: "confirm",
              label: "Push",
              variant: "primary",
              disabled: actionPending === "push",
              onClick: () => {
                if (!config) return;
                void pushBranchAction(config, { workspaceRoot }).then(() => setConfirmAction(null));
              },
            },
          ]}
        >
          <p className="rk-modal__desc">
            Applies to:{" "}
            <strong>{`Workspace: ${workspaceRoot} · Host: ${HOST_OPTIONS.find((option) => option.value === host)?.label ?? host} · Remote: origin (default) · Branch: current branch`}</strong>
          </p>
        </ActionDialog>
      ) : null}

      {confirmAction === "pr-create" ? (
        <ActionDialog
          title="Open a pull request?"
          description="This creates a public pull request on the configured Git host. It is outward-facing and visible to collaborators once opened."
          role="alertdialog"
          dismissible={actionPending !== "pr-create"}
          onClose={() => setConfirmAction(null)}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: actionPending === "pr-create",
              onClick: () => setConfirmAction(null),
            },
            {
              id: "confirm",
              label: "Open PR",
              variant: "primary",
              disabled: actionPending === "pr-create",
              onClick: () => {
                if (!config) return;
                void createPullRequestAction(config, {
                  workspaceRoot,
                  title: prTitle.trim() || undefined,
                  body: prBody.trim() || undefined,
                }).then(() => setConfirmAction(null));
              },
            },
          ]}
        >
          <p className="rk-modal__desc">
            Applies to:{" "}
            <strong>{`Workspace: ${workspaceRoot} · Title: ${prTitle.trim() || "(untitled)"}`}</strong>
          </p>
        </ActionDialog>
      ) : null}
    </section>
  );
}
