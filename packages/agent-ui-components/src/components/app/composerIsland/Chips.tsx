import { Popover } from "@rightkit/react-shell/react";
import { useRef } from "react";
import { projectName } from "@rightkit/agent-ui-core/lib/utils";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export function WorkspaceStrip(props: {
  workspaceRoot: string;
  workspaces: string[];
  showWorkspaceMenu: boolean;
  showBranchMenu: boolean;
  gitBranch: string | null;
  gitBranches: string[];
  branchSwitchPending: string | null;
  workspaceSearch: string;
  shownWorkspaces: string[];
  onToggleWorkspaceMenu: () => void;
  onToggleBranchMenu: () => void;
  onSetWorkspace: (path: string) => void;
  onSwitchBranch: (branch: string) => void;
  onOpenWorkspace: () => void;
  onWorkspaceSearch: (value: string) => void;
  onOpenEnvironments?: () => void;
  environmentLabel: string;
}) {
  const workspaceButtonRef = useRef<HTMLButtonElement | null>(null);
  const branchButtonRef = useRef<HTMLButtonElement | null>(null);
  return (
    <div className="flex h-8 flex-wrap items-center gap-1.5 bg-transparent px-0 text-[12px] text-text-3">
      <button
        type="button"
        className="inline-flex h-6 shrink-0 items-center gap-1.5 rounded-full border border-line px-2"
        title="Choose the execution environment"
        onClick={props.onOpenEnvironments}
        disabled={!props.onOpenEnvironments}
      >
        <Icon name="terminal" size={12} />
        {props.environmentLabel}
      </button>

      <div className="relative min-w-0 max-w-[45%] @[420px]:max-w-[220px]" data-composer-menu>
        <button
          type="button"
          className="cr-control-pill inline-flex h-6 w-full min-w-0 cursor-pointer items-center gap-1.5 rounded-md border border-line bg-panel px-2 text-left text-text-3 hover:border-line-2 hover:text-text-2"
          ref={workspaceButtonRef}
          onClick={props.onToggleWorkspaceMenu}
          title={props.workspaceRoot}
          aria-label="Choose repository"
          aria-haspopup="menu"
          aria-expanded={props.showWorkspaceMenu}
        >
          <Icon name="folder" size={12} />
          <span className="min-w-0 flex-1 truncate">{projectName(props.workspaceRoot)}</span>
          <Icon name="chevron" size={10} />
        </button>
        {props.showWorkspaceMenu ? (
          <Popover
            anchor={workspaceButtonRef.current}
            onClose={props.onToggleWorkspaceMenu}
            label="Choose repository"
            placement="top-start"
            width={300}
          >
            <label className="flex items-center gap-2 border-b border-line px-3 py-2 text-[12px] text-text-3">
              <Icon name="search" size={12} />
              <input
                aria-label="Chip input"
                value={props.workspaceSearch}
                onChange={(event) => props.onWorkspaceSearch(event.currentTarget.value)}
                placeholder="Search repositories…"
                // Opening the menu must focus this field: without it a user who
                // opens the picker and types immediately loses every keystroke.
                // biome-ignore lint/a11y/noAutofocus: focus belongs in the field the menu exists to fill.
                autoFocus
                // Repository names are identifiers, not prose. macOS autocorrect
                // rewrote "sellright" to "Sell right" here, so the search could
                // never match the repo the user was typing.
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                className="min-w-0 flex-1 bg-transparent text-[12px] text-text outline-none placeholder:text-text-3"
              />
            </label>
            <button
              type="button"
              role="menuitem"
              className="flex w-full cursor-pointer items-center gap-2 px-3 py-2 text-left text-[13px] text-text-2 hover:bg-panel-2 hover:text-text"
              onClick={props.onOpenWorkspace}
            >
              <Icon name="folder" size={13} />
              Open folder…
              <span className="ml-auto text-[10px] text-text-3">new session</span>
            </button>
            {props.shownWorkspaces.map((workspace) => (
              <button
                key={workspace}
                type="button"
                role="menuitem"
                className="flex w-full cursor-pointer items-center gap-2 px-3 py-2 text-left text-[13px] text-text-2 hover:bg-panel-2 hover:text-text"
                onClick={() => props.onSetWorkspace(workspace)}
              >
                <Icon name="folder" size={13} />
                <span className="truncate">{projectName(workspace)}</span>
                <span className="ml-auto text-[10px] text-text-3">new session</span>
              </button>
            ))}
          </Popover>
        ) : null}
      </div>

      <div className="relative min-w-0 max-w-[45%] @[420px]:max-w-[200px]" data-composer-menu>
        <button
          type="button"
          className="cr-control-pill inline-flex h-6 w-full min-w-0 cursor-pointer items-center gap-1.5 rounded-md border border-line bg-panel px-2 text-left text-text-3 hover:border-line-2 hover:text-text-2"
          ref={branchButtonRef}
          onClick={props.onToggleBranchMenu}
          title={props.gitBranch ?? "No branch"}
          aria-label="Choose Git branch"
          aria-haspopup="menu"
          aria-expanded={props.showBranchMenu}
        >
          <Icon name="branch" size={12} />
          <span className="min-w-0 flex-1 truncate">{props.gitBranch ?? "No branch"}</span>
          <Icon name="chevron" size={10} />
        </button>
        {props.showBranchMenu ? (
          <Popover
            anchor={branchButtonRef.current}
            onClose={props.onToggleBranchMenu}
            label="Choose Git branch"
            placement="top-start"
            width={280}
            className="max-h-[260px] overflow-y-auto"
          >
            {props.gitBranches.length === 0 ? (
              <div className="px-3 py-2 text-[12px] text-text-3">No branches found.</div>
            ) : (
              props.gitBranches.map((branch) => (
                <button
                  key={branch}
                  type="button"
                  role="menuitem"
                  disabled={props.branchSwitchPending === branch}
                  className="flex w-full cursor-pointer items-center gap-2 px-3 py-2 text-left text-[13px] text-text-2 hover:bg-panel-2 hover:text-text disabled:cursor-wait disabled:opacity-50"
                  onClick={() => props.onSwitchBranch(branch)}
                >
                  <Icon name="branch" size={12} />
                  <span className="truncate">{branch}</span>
                  {branch === props.gitBranch ? (
                    <span className="ml-auto text-accent">✓</span>
                  ) : null}
                </button>
              ))
            )}
          </Popover>
        ) : null}
      </div>
    </div>
  );
}
