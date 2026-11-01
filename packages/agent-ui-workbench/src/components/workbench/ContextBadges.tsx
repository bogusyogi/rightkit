import Icon from "@rightkit/agent-ui-core/components/Icon";

export function ToolsetBadge({ toolset: rawToolset }: { toolset?: string | null }) {
  const toolset = rawToolset?.trim();
  if (!toolset) return null;
  return (
    <span
      className="inline-flex h-7 max-w-[180px] shrink-0 items-center gap-1.5 rounded-md border border-line bg-panel px-2.5 text-[11px] font-medium text-text-2"
      title={`Active toolset: ${toolset}`}
    >
      <Icon name="lock" size={12} />
      <span className="text-text-3">Toolset</span>
      <span className="min-w-0 truncate font-mono text-text">{toolset}</span>
    </span>
  );
}

export function ExecutionBadge({
  executionBackend: rawBackend,
  executionWorkspaceId: rawWorkspaceId,
}: {
  executionBackend?: string | null;
  executionWorkspaceId?: string | null;
}) {
  const backend = rawBackend?.trim();
  const workspaceId = rawWorkspaceId?.trim();
  if ((!backend || backend === "local") && !workspaceId) return null;
  const label = backend && backend !== "local" ? backend : (workspaceId ?? "runtime");
  const title = [
    backend ? `Execution backend: ${backend}` : null,
    workspaceId ? `Workspace: ${workspaceId}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <span
      className="inline-flex h-7 max-w-[180px] shrink-0 items-center gap-1.5 rounded-md border border-line bg-panel px-2.5 text-[11px] font-medium text-text-2"
      title={title}
    >
      <Icon name="server" size={12} />
      <span className="text-text-3">Exec</span>
      <span className="min-w-0 truncate font-mono text-text">{label}</span>
    </span>
  );
}

export function SpecBadge({ specId: rawSpecId }: { specId?: string | null }) {
  const specId = rawSpecId?.trim();
  if (!specId) return null;
  return (
    <span
      className="inline-flex h-7 max-w-[210px] shrink-0 items-center gap-1.5 rounded-md border border-line bg-panel px-2.5 text-[11px] font-medium text-text-2"
      title={`Bound spec: ${specId}`}
    >
      <Icon name="tasks" size={12} />
      <span className="text-text-3">Spec</span>
      <span className="min-w-0 truncate font-mono text-text">{specId}</span>
    </span>
  );
}

export function SafeModeBadge({ safeMode }: { safeMode?: boolean }) {
  if (!safeMode) return null;
  return (
    <span
      className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md border border-line bg-panel px-2.5 text-[11px] font-medium text-text-2"
      title="Safe mode: user plugins, hooks, and persistent permission rules are disabled for this session. Compare with a normal fork if behavior changes."
    >
      <Icon name="lock" size={12} />
      <span className="text-text-3">Safe</span>
    </span>
  );
}

export function ForkBadge({
  forkedFromSessionId: rawParentId,
}: {
  forkedFromSessionId?: string | null;
}) {
  const parentId = rawParentId?.trim();
  if (!parentId) return null;
  return (
    <span
      className="inline-flex h-7 max-w-[180px] shrink-0 items-center gap-1.5 rounded-md border border-line bg-panel px-2.5 text-[11px] font-medium text-text-2"
      title={`Forked from session: ${parentId}`}
    >
      <Icon name="branch" size={12} />
      <span className="text-text-3">Fork</span>
      <span className="min-w-0 truncate font-mono text-text">{parentId}</span>
    </span>
  );
}
