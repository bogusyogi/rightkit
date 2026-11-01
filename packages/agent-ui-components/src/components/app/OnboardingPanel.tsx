import { useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { projectName } from "@rightkit/agent-ui-core/lib/utils";
import type { RunTarget } from "../../hooks/useShellUiState.js";
import { hostAppName } from "../../appName.js";
// a11y-test: role="group" aria-label="Start point options" — keep as single substring for matrix test
export default function OnboardingPanel({
  status,
  workspaceRoot,
  runTarget,
  showAdvancedConnect,
  urlDraft,
  tokenDraft,
  onOpenWorkspace,
  onRunTarget,
  onRetryEngine,
  onToggleAdvanced,
  onUrlDraft,
  onTokenDraft,
  onConnect,
}: {
  status: string;
  workspaceRoot: string;
  runTarget: RunTarget;
  showAdvancedConnect: boolean;
  urlDraft: string;
  tokenDraft: string;
  onOpenWorkspace: () => void;
  onRunTarget: (target: RunTarget) => void;
  onRetryEngine: () => void;
  onToggleAdvanced: () => void;
  onUrlDraft: (value: string) => void;
  onTokenDraft: (value: string) => void;
  onConnect: () => void;
}) {
  const [showToken, setShowToken] = useState(false);
  const previewMode = status === "Preview mode";
  const runTargetCardClass = (target: RunTarget) =>
    `group flex min-h-[148px] flex-col items-start gap-3 rounded-xl border p-[18px] text-left cursor-pointer transition-[border-color,background-color,box-shadow,transform] duration-150 active:translate-y-px ${
      runTarget === target
        ? "border-line-2 bg-panel-2 text-text [box-shadow:inset_3px_0_0_var(--color-accent),0_0_0_1px_color-mix(in_srgb,var(--color-accent)_10%,transparent)]"
        : "border-line text-text-2 hover:border-line-2 hover:bg-panel hover:text-text"
    }`;

  return (
    <section
      className="flex-1 min-h-0 overflow-y-auto scroll-pb-12 grid place-items-center p-8 pb-12"
      aria-labelledby="onboarding-title"
      aria-describedby="onboarding-desc"
    >
      <div className="flex w-full max-w-[720px] flex-col gap-5">
        <div className="flex flex-col gap-2">
          <p className="text-[12px] uppercase text-text-3 font-semibold">Start point</p>
          <h2 id="onboarding-title" className="text-[22px] font-bold text-text">
            {workspaceRoot ? projectName(workspaceRoot) : `Choose where ${hostAppName()} should work`}
          </h2>
          <p id="onboarding-desc" className="text-[15px] text-text-2 leading-[1.55]">
            {workspaceRoot
              ? workspaceRoot
              : "Open a local repo or choose a branch-aware worktree before starting a session."}
          </p>
        </div>
        {/* Live region announces engine status changes and workspace selection. */}
        <p className="sr-only" aria-live="polite" aria-atomic="true">
          {status}
          {workspaceRoot ? ` — selected workspace ${workspaceRoot}` : ""}
        </p>
        <fieldset
          className="grid grid-cols-2 gap-3 border-0 p-0 m-0"
          aria-label="Start point options"
        >
          <button
            type="button"
            aria-label="Open local folder"
            aria-pressed={runTarget === "local"}
            title="Open local folder"
            className={runTargetCardClass("local")}
            onClick={() => {
              onRunTarget("local");
              onOpenWorkspace();
            }}
          >
            <span className="flex w-full items-start gap-2">
              <span className="grid h-8 w-8 place-items-center rounded-lg border border-line bg-panel-2 text-text-2">
                <Icon name="folder" size={15} />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-[10px] font-semibold uppercase tracking-wide text-text-3">
                  Recommended
                </span>
                <strong className="block text-[15px] font-semibold text-text">Local folder</strong>
              </span>
            </span>
            <span className="text-[13px] leading-[1.55]">
              Open an existing repo or project directory.
            </span>
          </button>
          <button
            type="button"
            aria-label="Open repo or worktree"
            aria-pressed={runTarget === "worktree"}
            title="Open repo or worktree"
            className={runTargetCardClass("worktree")}
            onClick={() => onRunTarget("worktree")}
          >
            <span className="grid h-8 w-8 place-items-center rounded-lg border border-line bg-panel text-text-3 group-hover:text-text">
              <Icon name="git" size={15} />
            </span>
            <strong className="text-[15px] font-semibold">Repo / worktree</strong>
            <span className="text-[13px] leading-[1.55]">
              Work from a branch-aware git workspace.
            </span>
          </button>
        </fieldset>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            aria-label={previewMode ? "Reload preview" : "Retry local engine"}
            title={previewMode ? "Reload preview" : "Retry local engine"}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel hover:text-text"
            onClick={onRetryEngine}
          >
            <Icon name="refresh" size={13} />
            {previewMode ? "Reload preview" : "Retry local engine"}
          </button>
          <button
            type="button"
            aria-label={
              showAdvancedConnect ? "Hide advanced connection" : "Show advanced connection"
            }
            title={showAdvancedConnect ? "Hide advanced connection" : "Show advanced connection"}
            className="inline-flex h-8 items-center rounded-lg border border-transparent bg-transparent px-2 text-[12px] text-text-3 cursor-pointer hover:text-text hover:bg-panel"
            onClick={onToggleAdvanced}
          >
            {showAdvancedConnect ? "Hide advanced connection" : "Advanced connection"}
          </button>
        </div>
        {showAdvancedConnect ? (
          <div className="cr-panel-reveal flex flex-col gap-3 p-5 rounded-xl border border-line bg-panel">
            <label className="flex flex-col gap-1 text-[13px] text-text-2">
              Local engine URL
              <input
                id="onboarding-url"
                aria-label="Local engine URL"
                aria-describedby="onboarding-url-hint"
                placeholder="http://127.0.0.1:3210"
                className="h-9 px-3 rounded-lg border border-line bg-transparent text-text text-[13px] outline-none focus:border-focus"
                value={urlDraft}
                onChange={(event) => onUrlDraft(event.target.value)}
              />
              <span id="onboarding-url-hint" className="sr-only">
                Enter the daemon URL. Defaults to local engine.
              </span>
            </label>
            <label className="flex flex-col gap-1 text-[13px] text-text-2">
              Token
              <span className="relative">
                <input
                  id="onboarding-token"
                  aria-label="Daemon bearer token"
                  type={showToken ? "text" : "password"}
                  className="h-9 w-full px-3 pr-16 rounded-lg border border-line bg-transparent text-text text-[13px] outline-none focus:border-focus"
                  value={tokenDraft}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(event) => onTokenDraft(event.target.value)}
                />
                <button
                  type="button"
                  aria-label={showToken ? "Hide token" : "Show token"}
                  aria-pressed={showToken}
                  className="absolute right-1 top-1 inline-flex h-7 items-center rounded-md border border-line bg-panel-2 px-2 text-[11px] font-semibold text-text-3 hover:text-text"
                  onClick={() => setShowToken((visible) => !visible)}
                >
                  {showToken ? "Hide" : "Show"}
                </button>
              </span>
            </label>
            <button
              type="button"
              aria-label="Connect to custom engine"
              className="inline-flex items-center h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text self-start"
              onClick={onConnect}
            >
              Connect manually
            </button>
          </div>
        ) : null}
      </div>
    </section>
  );
}
