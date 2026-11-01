import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { SettingsConfigStatus } from "@rightkit/agent-ui-core/lib/types";
import { hostAppName } from "../../../appName.js";

export default function WorkspaceTab({
  workspaceRoot,
  settingsConfigStatus,
  onOpenWorkspace,
}: {
  workspaceRoot: string;
  settingsConfigStatus: SettingsConfigStatus | null;
  onOpenWorkspace: () => void;
}) {
  const hasWorkspace = workspaceRoot.trim().length > 0;

  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex flex-col gap-1">
        <h3 className="text-[16px] font-bold text-text">Workspace</h3>
        <p className="text-[13px] text-text-2">
          {hostAppName()} should always make the active folder, repo mode, and state storage obvious.
        </p>
      </div>
      {!hasWorkspace ? (
        <div className="cr-panel-reveal max-w-[720px] rounded-xl border border-[color-mix(in_srgb,var(--color-accent)_28%,var(--color-line))] bg-panel px-4 py-4">
          <div className="flex items-start gap-3">
            <span className="grid h-9 w-9 flex-shrink-0 place-items-center rounded-lg border border-[color-mix(in_srgb,var(--color-accent)_35%,var(--color-line))] bg-[color-mix(in_srgb,var(--color-accent)_8%,transparent)] text-accent">
              <Icon name="folder" size={15} />
            </span>
            <div className="min-w-0 flex-1">
              <strong className="block text-[14px] font-semibold text-text">
                Open a workspace to activate {hostAppName()}
              </strong>
              <p className="mt-1 max-w-[560px] text-[13px] leading-[1.5] text-text-2">
                Settings, model roles, skills, permissions, and transcript state attach to the
                active repo. Start by choosing the local folder {hostAppName()} should work in.
              </p>
              <button
                type="button"
                className="cr-control-pill mt-3 inline-flex h-9 items-center gap-2 rounded-lg border-0 bg-accent px-4 text-[13px] font-semibold text-on-accent hover:opacity-90"
                onClick={onOpenWorkspace}
              >
                <Icon name="folder" size={14} /> Open local folder
              </button>
            </div>
          </div>
        </div>
      ) : null}
      <div className="flex flex-col gap-3 max-w-[560px]">
        <div className="flex items-center gap-4 px-4 py-3 rounded-xl border border-line bg-panel">
          <span className="text-[12px] text-text-3 w-24 flex-shrink-0">Active folder</span>
          <strong className="text-[13px] font-medium text-text truncate">
            {workspaceRoot || "None selected"}
          </strong>
        </div>
        <div className="flex items-center gap-4 px-4 py-3 rounded-xl border border-line bg-panel">
          <span className="text-[12px] text-text-3 w-24 flex-shrink-0">State file</span>
          <strong className="text-[13px] font-medium text-text truncate">
            {workspaceRoot ? `${workspaceRoot}/.coderight/config.toml` : "Select a workspace first"}
          </strong>
        </div>
        {hasWorkspace ? (
          <button
            type="button"
            className="cr-control-pill inline-flex h-9 items-center gap-2 rounded-lg border border-line bg-transparent px-4 text-[13px] text-text-2 cursor-pointer hover:bg-panel hover:text-text self-start"
            onClick={onOpenWorkspace}
          >
            <Icon name="folder" /> Change workspace
          </button>
        ) : null}
      </div>
      <div className="flex flex-col gap-3 max-w-[720px]">
        <div className="flex items-center justify-between gap-3">
          <h4 className="text-[13px] font-semibold text-text">Config files</h4>
          {settingsConfigStatus ? (
            <span className="text-[11px] text-text-3 truncate">
              {settingsConfigStatus.fingerprint.slice(0, 19)}
            </span>
          ) : null}
        </div>
        <div className="rounded-xl border border-line bg-panel overflow-hidden">
          {(settingsConfigStatus?.files ?? []).map((file) => (
            <div
              className="grid grid-cols-[88px_72px_1fr] gap-3 px-4 py-3 border-b border-line last:border-b-0 items-center"
              key={`${file.scope}:${file.path}`}
            >
              <span className="text-[12px] text-text-3 capitalize">{file.scope}</span>
              <span
                className={`justify-self-start px-2 py-0.5 rounded-full text-[11px] border ${
                  file.exists
                    ? "text-accent-2 border-accent-2/40 bg-accent-2/10"
                    : "text-text-3 border-line bg-panel-2"
                }`}
              >
                {file.exists ? "Loaded" : "Missing"}
              </span>
              <span className="text-[12px] text-text-2 truncate">{file.path}</span>
            </div>
          ))}
          {!settingsConfigStatus ? (
            <div className="px-4 py-3 text-[12px] text-text-3">
              Config metadata loads with settings.
            </div>
          ) : null}
        </div>
      </div>
    </section>
  );
}
