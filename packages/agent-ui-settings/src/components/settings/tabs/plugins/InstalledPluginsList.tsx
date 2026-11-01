import { EmptyState } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { PluginManifest } from "@rightkit/agent-ui-core/lib/types";

export default function InstalledPluginsList({
  plugins,
  pendingId,
  onToggle,
}: {
  plugins: PluginManifest[];
  pendingId: string | null;
  onToggle: (plugin: PluginManifest) => void;
}) {
  if (!plugins.length) {
    return (
      <EmptyState icon={<Icon name="attachment" size={18} />} title="No plugins discovered.">{"Reload after adding a plugin manifest under the configured plugin directory."}</EmptyState>
    );
  }

  return (
    <div className="flex flex-col divide-y divide-line border border-line rounded-xl overflow-hidden">
      {plugins.map((plugin) => {
        const contributionCount = plugin.providesTools.length + plugin.providesCommands.length;
        const describedTools = plugin.providesTools.filter(
          (name) => plugin.tools?.[name]?.description,
        ).length;
        const readOnlyTools = plugin.providesTools.filter(
          (name) => plugin.tools?.[name]?.readOnly,
        ).length;
        const describedCommands = plugin.providesCommands.filter(
          (name) => plugin.commands?.[name]?.description || plugin.commands?.[name]?.usage,
        ).length;
        const promptCount = plugin.prompts?.filter((p) => p.body?.trim()).length ?? 0;
        const isolation = plugin.sandbox?.isolation ?? "restrictedSubprocess";
        const restricted = isolation === "restrictedSubprocess";
        const metadataOnly = isolation === "metadataOnly";
        const sandboxLabel = metadataOnly
          ? "metadata only"
          : restricted
            ? "restricted subprocess"
            : plugin.sandbox?.allowAbsoluteEntry
              ? "loose entry"
              : "contained entry";
        const timeoutMs = restricted
          ? Math.min(plugin.sandbox?.timeoutMs ?? 30000, 15000)
          : (plugin.sandbox?.timeoutMs ?? 30000);
        const inheritsEnv = restricted ? false : (plugin.sandbox?.inheritEnv ?? true);
        const allowedEnvCount = plugin.sandbox?.allowedEnv?.length ?? 0;
        const deniedEnvCount = plugin.sandbox?.deniedEnv?.length ?? 0;
        const envPolicy = inheritsEnv ? "inherits env" : "scrubbed env";
        const networkPolicy =
          restricted || plugin.sandbox?.stripNetworkEnv
            ? "network env stripped"
            : "network env inherited";
        return (
          <article
            className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-4 px-4 py-3"
            key={`${plugin.id}:${plugin.version}`}
          >
            <div className="flex flex-col gap-1 min-w-0">
              <div className="flex items-center gap-2 min-w-0">
                <strong className="text-[13px] font-semibold text-text truncate">
                  {plugin.name || plugin.id}
                </strong>
                <code className="text-[11px] font-mono text-text-3 flex-shrink-0">
                  {plugin.version}
                </code>
              </div>
              <span className="text-[12px] text-text-3 truncate">
                {plugin.description || plugin.entry}
              </span>
              <span className="text-[11px] text-text-3 truncate">
                {plugin.providesTools.join(", ") || "no tools"} /{" "}
                {plugin.providesCommands.join(", ") || "no commands"}
              </span>
              <span className="text-[11px] text-text-3 truncate">
                {metadataOnly ? "execution disabled" : "subprocess"} · {sandboxLabel} ·{" "}
                {Math.round(timeoutMs / 1000)}s · {plugin.entry || "no entry"}
              </span>
              <span className="text-[11px] text-text-3 truncate">
                sandbox env · {envPolicy} · allow {allowedEnvCount} · deny {deniedEnvCount} ·{" "}
                {networkPolicy}
              </span>
              {describedTools || readOnlyTools || describedCommands ? (
                <span className="text-[11px] text-text-3 truncate">
                  metadata · {describedTools}/{plugin.providesTools.length} tools described ·{" "}
                  {readOnlyTools} read-only · {describedCommands}/{plugin.providesCommands.length}{" "}
                  commands described
                </span>
              ) : null}
              {promptCount ? (
                <span className="text-[11px] text-text-3 truncate">
                  prompts · {promptCount} instruction contribution
                  {promptCount !== 1 ? "s" : ""}
                </span>
              ) : null}
            </div>
            <code className="text-[11px] font-mono text-accent-2">
              {contributionCount} item{contributionCount !== 1 ? "s" : ""}
            </code>
            <button
              type="button"
              className={`inline-flex items-center gap-2 h-8 px-3 rounded-lg border text-[12px] cursor-pointer ${
                plugin.enabled
                  ? "border-line bg-transparent text-text-2 hover:bg-panel hover:text-text disabled:opacity-40"
                  : "border-0 bg-accent text-on-accent font-semibold hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
              }`}
              disabled={pendingId === plugin.id}
              onClick={() => onToggle(plugin)}
            >
              <Icon name={plugin.enabled ? "pause" : "play"} />
              {plugin.enabled ? "Disable" : "Enable"}
            </button>
          </article>
        );
      })}
    </div>
  );
}
