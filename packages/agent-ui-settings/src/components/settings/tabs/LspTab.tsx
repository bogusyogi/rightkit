import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { LspStatusView, LspWorkspaceServer } from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "./shared";
import { hostAppName } from "../../../appName";
export default function LspTab({
  status,
  draft,
  pendingId,
  error,
  workspaceRoot,
  onDraft,
  onRefresh,
  onSave,
}: {
  status: LspStatusView | null;
  draft: LspWorkspaceServer[];
  pendingId: string | null;
  error: string | null;
  workspaceRoot: string;
  onDraft: (draft: LspWorkspaceServer[]) => void;
  onRefresh: () => void;
  onSave: () => void;
}) {
  function updateServer(index: number, patch: Partial<LspWorkspaceServer>) {
    onDraft(draft.map((server, i) => (i === index ? { ...server, ...patch } : server)));
  }

  function addServer() {
    onDraft([
      ...draft,
      {
        language: "rust",
        extensions: ["rs"],
        serverPath: "rust-analyzer",
        serverArgs: [],
      },
    ]);
  }

  function removeServer(index: number) {
    onDraft(draft.filter((_, i) => i !== index));
  }

  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex items-start gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">LSP</h3>
          <p className="text-[13px] text-text-2 truncate">
            {status?.configPath ||
              (workspaceRoot
                ? `${workspaceRoot}/.coderight/lsp.json`
                : "Select a workspace to configure language servers.")}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text"
          onClick={onRefresh}
        >
          <Icon name="refresh" /> Refresh
        </button>
      </div>

      {error ? <OfflinePanel title="LSP unavailable." action={error} /> : null}

      {status ? (
        <div className="flex flex-col divide-y divide-line border border-line rounded-xl overflow-hidden">
          {status.servers.map((server) => (
            <article
              className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_90px_90px] items-center gap-4 px-4 py-3"
              key={server.language}
            >
              <div className="min-w-0">
                <strong className="block text-[13px] font-semibold text-text truncate">
                  {server.language}
                </strong>
                <span className="block text-[11px] text-text-3 truncate">
                  {server.extensions.join(", ") || "no extensions"}
                </span>
              </div>
              <code className="text-[11px] font-mono text-text-3 truncate">
                {server.effectivePath || "local symbol fallback"}
              </code>
              <code
                className={`text-[11px] font-mono ${
                  server.available ? "text-accent-2" : "text-text-3"
                }`}
              >
                {server.source}
              </code>
              <code className="text-[11px] font-mono text-text-3">
                {server.available ? "ready" : "fallback"}
              </code>
            </article>
          ))}
        </div>
      ) : (
        <OfflinePanel
          title="No LSP status loaded."
          action="Open a workspace, then refresh LSP status."
        />
      )}

      <div className="flex flex-col gap-3 border border-line rounded-xl p-4">
        <div className="flex items-center gap-3">
          <div className="flex-1 min-w-0">
            <h4 className="text-[14px] font-bold text-text">Workspace servers</h4>
            <p className="text-[12px] text-text-3 truncate">
              Configured servers override PATH discovery for matching languages.
            </p>
          </div>
          <button
            type="button"
            className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text"
            onClick={addServer}
          >
            <Icon name="plus" /> Add
          </button>
          <button
            type="button"
            className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[12px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
            disabled={pendingId === "save" || !workspaceRoot.trim()}
            onClick={onSave}
          >
            <Icon name="settings" /> Save
          </button>
        </div>

        {draft.length > 0 ? (
          <div className="flex flex-col gap-3">
            {draft.map((server, index) => (
              <div
                className="grid grid-cols-[120px_150px_minmax(0,1fr)_minmax(0,1fr)_auto] gap-3 items-end"
                key={`${server.language}:${server.serverPath}:${server.extensions.join(",")}:${server.serverArgs.join(" ")}`}
              >
                <label className="flex flex-col gap-1 text-[13px] text-text-2">
                  Language
                  <input
                    className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
                    value={server.language}
                    onChange={(e) => updateServer(index, { language: e.target.value })}
                  />
                </label>
                <label className="flex flex-col gap-1 text-[13px] text-text-2">
                  Extensions
                  <input
                    className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
                    value={server.extensions.join(",")}
                    onChange={(e) =>
                      updateServer(index, {
                        extensions: e.target.value
                          .split(",")
                          .map((ext) => ext.trim())
                          .filter(Boolean),
                      })
                    }
                  />
                </label>
                <label className="flex flex-col gap-1 text-[13px] text-text-2">
                  Server path
                  <input
                    className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
                    value={server.serverPath}
                    onChange={(e) => updateServer(index, { serverPath: e.target.value })}
                  />
                </label>
                <label className="flex flex-col gap-1 text-[13px] text-text-2">
                  Args
                  <input
                    className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
                    value={server.serverArgs.join(" ")}
                    onChange={(e) =>
                      updateServer(index, {
                        serverArgs: e.target.value.trim() ? e.target.value.trim().split(/\s+/) : [],
                      })
                    }
                  />
                </label>
                <button
                  type="button"
                  className="inline-flex items-center justify-center h-9 w-9 rounded-lg border border-line bg-transparent text-text-2 cursor-pointer hover:bg-panel hover:text-danger"
                  onClick={() => removeServer(index)}
                  title="Remove server"
                >
                  <Icon name="trash" size={14} />
                </button>
              </div>
            ))}
          </div>
        ) : (
          <OfflinePanel
            title="No workspace LSP overrides."
            action={`${hostAppName()} will use PATH discovery and the local symbol fallback.`}
          />
        )}
      </div>
    </section>
  );
}
