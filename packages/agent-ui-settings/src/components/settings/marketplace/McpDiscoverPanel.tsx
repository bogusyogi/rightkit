import { EmptyState } from "@rightkit/react-shell/react";
import { useMemo, useState, type ChangeEvent } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type {
  ConfiguredMcpServerProjection,
  McpRegistryServer,
  RegistryHealthProjection,
} from "@rightkit/agent-ui-core/lib/daemon/marketplaces";

export type McpInstallAgent = "claude" | "codex";
export type McpInstallScope = "user" | "project" | "local";

export interface McpInstallSelection {
  agent: McpInstallAgent;
  scope: McpInstallScope;
  cwd?: string;
  environment: Record<string, string>;
  headers: Record<string, string>;
}

export interface McpDiscoverPanelProps {
  projectCwd?: string;
  query: string;
  results: McpRegistryServer[];
  popularResults?: McpRegistryServer[];
  selectedServer?: McpRegistryServer | null;
  installedServers?: ConfiguredMcpServerProjection[];
  health?: RegistryHealthProjection | null;
  searching?: boolean;
  refreshing?: boolean;
  operationLoading?: string | null;
  installAgent?: McpInstallAgent;
  installScope?: McpInstallScope;
  onQueryChange: (query: string) => void;
  onAgentChange?: (agent: McpInstallAgent) => void;
  onScopeChange?: (scope: McpInstallScope) => void;
  onRefresh?: () => void;
  onSelect: (server: McpRegistryServer) => void;
  onClearSelection?: () => void;
  onInstall: (server: McpRegistryServer, selection: McpInstallSelection) => void | Promise<void>;
  onImportClaudeDesktop?: () => void | Promise<void>;
  onImportJson?: (file: File) => void | Promise<void>;
}

function transport(server: McpRegistryServer): "stdio" | "http" {
  return server.remotes.length > 0 ? "http" : "stdio";
}

function transportLabel(server: McpRegistryServer): string {
  if (server.remotes.length > 0) return "HTTP";
  return server.packages[0]?.registryType || "stdio";
}

function codexKey(name: string): string {
  return name.replace(/[^a-zA-Z0-9_-]/g, "-").replace(/-{2,}/g, "-").replace(/^-|-$/g, "");
}

function isInstalled(
  server: McpRegistryServer,
  installedServers: ConfiguredMcpServerProjection[],
  agent: McpInstallAgent,
): boolean {
  const rows = installedServers.filter((item) => (item.agent ?? "claude") === agent);
  if (server.remotes.length > 0) {
    return rows.some((item) => item.url === server.remotes[0]?.url);
  }
  const identifier = server.packages[0]?.identifier;
  return identifier
    ? rows.some((item) => item.args?.some((arg) => arg === identifier || arg.includes(identifier)))
    : false;
}

function RegistryStatus({ health, onRefresh, refreshing }: Pick<McpDiscoverPanelProps, "health" | "onRefresh" | "refreshing">) {
  const tone = health === null || health === undefined ? "bg-text-3/40" : health.available ? "bg-accent-2" : "bg-danger";
  return (
    <div className="flex shrink-0 items-center gap-1">
      <span className="flex items-center gap-1.5 text-[10px] text-text-3" title={health?.reason ?? ""}>
        <span className={`inline-block h-2 w-2 rounded-full ${tone}`} aria-hidden="true" />
        registry
      </span>
      {onRefresh ? (
        <button
          type="button"
          className="rounded p-1 text-text-3 hover:bg-panel hover:text-text disabled:opacity-40"
          title="Refresh registry"
          aria-label="Refresh registry"
          disabled={refreshing}
          onClick={onRefresh}
        >
          <Icon name="refresh" size={13} />
        </button>
      ) : null}
    </div>
  );
}

export default function McpDiscoverPanel({
  projectCwd = "",
  query,
  results,
  popularResults = [],
  selectedServer = null,
  installedServers = [],
  health = null,
  searching = false,
  refreshing = false,
  operationLoading = null,
  installAgent = "claude",
  installScope = "user",
  onQueryChange,
  onAgentChange,
  onScopeChange,
  onRefresh,
  onSelect,
  onClearSelection,
  onInstall,
  onImportClaudeDesktop,
  onImportJson,
}: McpDiscoverPanelProps) {
  const [environment, setEnvironment] = useState<Record<string, string>>({});
  const [headers, setHeaders] = useState<Record<string, string>>({});
  const displayResults = query.trim().length >= 2 ? results : popularResults;
  const selectedInstalled = selectedServer
    ? isInstalled(selectedServer, installedServers, installAgent)
    : false;
  const canUseProjectScope = Boolean(projectCwd) && installAgent !== "codex";

  const selectedPackage = selectedServer?.packages[0];
  const selectedRemote = selectedServer?.remotes[0];
  const environmentVariables = useMemo(
    () => selectedPackage?.environmentVariables ?? [],
    [selectedPackage],
  );
  const headerDefinitions = useMemo(() => selectedRemote?.headers ?? [], [selectedRemote]);

  function selectServer(server: McpRegistryServer) {
    setEnvironment(
      Object.fromEntries((server.packages[0]?.environmentVariables ?? []).map((item) => [item.name, ""])),
    );
    setHeaders(
      Object.fromEntries((server.remotes[0]?.headers ?? []).map((item) => [item.name, item.value ?? ""])),
    );
    onSelect(server);
  }

  function install(server: McpRegistryServer) {
    onInstall(server, {
      agent: installAgent,
      scope: installScope,
      cwd: projectCwd || undefined,
      environment,
      headers,
    });
  }

  return (
    <section className="flex min-h-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <RegistryStatus health={health} onRefresh={onRefresh} refreshing={refreshing} />
        <label className="flex min-w-[220px] flex-1 items-center gap-2 rounded-lg border border-line bg-panel px-3 text-text-3">
          <Icon name="search" size={13} />
          <input
            type="search"
            aria-label="Search MCP registry"
            className="h-9 min-w-0 flex-1 border-0 bg-transparent text-[13px] text-text outline-none placeholder:text-text-3"
            placeholder="Search MCP registry"
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
          />
        </label>
        <div className="inline-flex rounded-lg border border-line bg-panel p-1">
          {(["claude", "codex"] as const).map((agent) => (
            <button
              type="button"
              key={agent}
              className={`rounded-md px-3 py-1.5 text-[12px] ${installAgent === agent ? "bg-panel-2 text-text" : "text-text-3 hover:text-text"}`}
              aria-pressed={installAgent === agent}
              onClick={() => onAgentChange?.(agent)}
            >
              {agent === "claude" ? "Claude" : "Codex"}
            </button>
          ))}
        </div>
        <div className="inline-flex rounded-lg border border-line bg-panel p-1">
          {(["user", "project", "local"] as const).map((scope) => (
            <button
              type="button"
              key={scope}
              className={`rounded-md px-3 py-1.5 text-[12px] capitalize ${installScope === scope ? "bg-panel-2 text-text" : "text-text-3 hover:text-text"}`}
              disabled={(scope !== "user" && !canUseProjectScope) || (scope === "local" && installAgent === "codex")}
              aria-pressed={installScope === scope}
              onClick={() => onScopeChange?.(scope)}
            >
              {scope}
            </button>
          ))}
        </div>
      </div>

      <div className="flex flex-wrap gap-2">
        {["filesystem", "database", "github", "api", "web", "docker"].map((filter) => (
          <button
            type="button"
            key={filter}
            className={`rounded-full border px-3 py-1 text-[11px] ${query === filter ? "border-accent/50 bg-accent/10 text-text" : "border-line text-text-3 hover:border-line-2 hover:text-text"}`}
            onClick={() => onQueryChange(filter)}
          >
            {filter}
          </button>
        ))}
      </div>

      {onImportClaudeDesktop || onImportJson ? (
        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2">
          <span className="mr-auto text-[12px] text-text-2">Import existing MCP configuration</span>
          {onImportClaudeDesktop ? (
            <button type="button" className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-line px-3 text-[12px] text-text-2 hover:bg-panel-2" onClick={onImportClaudeDesktop}>
              <Icon name="download" size={13} /> Claude Desktop
            </button>
          ) : null}
          {onImportJson ? (
            <label className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border border-line px-3 text-[12px] text-text-2 hover:bg-panel-2">
              <Icon name="document" size={13} /> JSON
              <input type="file" accept=".json,application/json" className="sr-only" onChange={(event: ChangeEvent<HTMLInputElement>) => {
                const file = event.currentTarget.files?.[0];
                if (file) void onImportJson(file);
                event.currentTarget.value = "";
              }} />
            </label>
          ) : null}
        </div>
      ) : null}

      {searching ? (
        <div className="flex items-center justify-center gap-2 py-10 text-[12px] text-text-3" role="status">
          <Icon name="refresh" size={14} /> Searching registry…
        </div>
      ) : displayResults.length === 0 ? (
        <EmptyState icon={<Icon name="server" size={18} />} title={query.trim().length >= 2 ? "No MCP servers found." : "No popular MCP servers available."} action={onRefresh && query.trim().length < 2 ? <button type="button" className="rounded-lg border border-line px-3 py-1.5 text-[12px] text-text-2 hover:bg-panel" onClick={onRefresh}>Retry</button> : undefined}>{query.trim().length >= 2 ? "Try a broader search term." : "Refresh the registry or search by capability."}</EmptyState>
      ) : (
        <div className="flex min-h-[300px] min-w-0 gap-3">
          <div className="w-[280px] shrink-0 space-y-2 overflow-y-auto pr-1">
            <div className="px-1 text-[11px] text-text-3">{query.trim().length >= 2 ? `${results.length} results` : "Popular servers"}</div>
            {displayResults.map((server) => {
              const installed = isInstalled(server, installedServers, installAgent);
              const pending = operationLoading === server.name;
              return (
                <article key={`${server.name}:${server.version ?? ""}`} className={`rounded-lg border px-3 py-2 ${selectedServer?.name === server.name ? "border-accent/50 bg-accent/5" : "border-line bg-panel hover:bg-panel-2"}`}>
                  <div className="flex items-start gap-2">
                    <button type="button" className="min-w-0 flex-1 text-left" onClick={() => selectServer(server)}>
                      <strong className="block truncate text-[13px] font-semibold text-text">{server.title || server.name}</strong>
                      <span className="mt-1 block text-[11px] text-text-3">{server.version ? `v${server.version} · ` : ""}{transportLabel(server)}</span>
                    </button>
                    <button type="button" className={`shrink-0 rounded-md px-2 py-1 text-[11px] font-semibold ${installed ? "bg-accent-2/10 text-accent-2" : "bg-accent text-on-accent"}`} disabled={installed || pending} onClick={() => install(server)}>
                      {pending ? "…" : installed ? "Installed" : "Install"}
                    </button>
                  </div>
                </article>
              );
            })}
          </div>

          <div className="min-w-0 flex-1 overflow-y-auto">
            {selectedServer ? (
              <div className="space-y-4 rounded-lg border border-line bg-panel p-4">
                <div className="flex items-start gap-3">
                  <div className="min-w-0 flex-1">
                    <h3 className="truncate text-[14px] font-semibold text-text">{selectedServer.title || selectedServer.name}</h3>
                    <div className="mt-1 flex flex-wrap gap-2 text-[11px] text-text-3">
                      {selectedServer.version ? <span>v{selectedServer.version}</span> : null}
                      <span>{transportLabel(selectedServer)}</span>
                      {selectedServer.repository?.url ? <a className="underline hover:text-text" href={selectedServer.repository.url} target="_blank" rel="noreferrer">Repository</a> : null}
                    </div>
                    <p className="mt-2 text-[12px] leading-5 text-text-2">{selectedServer.description || "No description provided."}</p>
                  </div>
                  <button type="button" aria-label="Close MCP server preview" className="text-text-3 hover:text-text" onClick={() => onClearSelection?.()}><Icon name="close" size={14} /></button>
                </div>

                {selectedPackage ? <div className="border-t border-line pt-3"><h4 className="mb-2 text-[11px] uppercase text-text-3">Package</h4><div className="rounded-lg bg-bg px-3 py-2 text-[12px] text-text"><code>{selectedPackage.identifier}</code><span className="ml-2 text-text-3">{selectedPackage.registryType}</span></div></div> : null}
                {selectedRemote ? <div className="border-t border-line pt-3"><h4 className="mb-2 text-[11px] uppercase text-text-3">Remote endpoint</h4><div className="rounded-lg bg-bg px-3 py-2 text-[12px] text-text"><code className="break-all">{selectedRemote.url}</code><span className="ml-2 text-text-3">{selectedRemote.type}</span></div></div> : null}

                {environmentVariables.length > 0 ? <div className="space-y-2 border-t border-line pt-3"><h4 className="text-[11px] uppercase text-text-3">Environment variables</h4>{environmentVariables.map((item) => <label key={item.name} className="flex items-center gap-2 text-[11px] text-text-2"><span className="w-36 shrink-0 truncate font-mono" title={item.description}>{item.name}{item.isRequired ? " *" : ""}</span><input aria-label={`${item.name} environment variable`} type={item.isSecret ? "password" : "text"} className="h-8 min-w-0 flex-1 rounded-lg border border-line bg-bg px-2 text-[12px] text-text outline-none" value={environment[item.name] ?? ""} onChange={(event) => setEnvironment((current) => ({ ...current, [item.name]: event.target.value }))} placeholder={item.description} /></label>)}</div> : null}
                {headerDefinitions.length > 0 ? <div className="space-y-2 border-t border-line pt-3"><h4 className="text-[11px] uppercase text-text-3">HTTP headers</h4>{headerDefinitions.map((item) => <label key={item.name} className="flex items-center gap-2 text-[11px] text-text-2"><span className="w-36 shrink-0 truncate font-mono" title={item.description}>{item.name}{item.isRequired ? " *" : ""}</span><input aria-label={`${item.name} HTTP header`} type={item.isSecret ? "password" : "text"} className="h-8 min-w-0 flex-1 rounded-lg border border-line bg-bg px-2 text-[12px] text-text outline-none" value={headers[item.name] ?? ""} onChange={(event) => setHeaders((current) => ({ ...current, [item.name]: event.target.value }))} placeholder={item.description} /></label>)}</div> : null}

                <div className="flex items-center justify-between gap-3 border-t border-line pt-3">
                  <span className="text-[11px] text-text-3">{transport(selectedServer) === "http" ? "HTTP" : `${installAgent === "codex" ? codexKey(selectedServer.name) : selectedServer.name} · ${installScope}`} </span>
                  <button type="button" className={`inline-flex h-9 items-center gap-2 rounded-lg px-4 text-[12px] font-semibold ${selectedInstalled ? "border border-line bg-transparent text-text-3" : "bg-accent text-on-accent hover:opacity-90"}`} disabled={selectedInstalled || operationLoading === selectedServer.name} onClick={() => install(selectedServer)}>
                    <Icon name="download" size={13} /> {operationLoading === selectedServer.name ? "Installing…" : selectedInstalled ? "Installed" : "Install server"}
                  </button>
                </div>
              </div>
            ) : <EmptyState icon={<Icon name="server" size={18} />} title="Select an MCP server.">{"Choose a registry result to inspect its transport and installation settings."}</EmptyState>}
          </div>
        </div>
      )}
    </section>
  );
}
