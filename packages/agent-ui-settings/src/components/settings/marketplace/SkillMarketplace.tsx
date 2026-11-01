import { EmptyState } from "@rightkit/react-shell/react";
import { useEffect, useMemo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  toMarketplaceSkillSlug,
  type CommunitySkillDetail,
  type CommunitySkillResult,
  type RegistryHealthProjection,
} from "@rightkit/agent-ui-core/lib/daemon/marketplaces";

export type SkillMarketplaceScope = "user" | "project";

export interface SkillMarketplaceProps {
  projectCwd?: string;
  results: CommunitySkillResult[];
  popularResults?: CommunitySkillResult[];
  selectedSkill?: CommunitySkillDetail | null;
  installedSlugs?: string[] | Set<string>;
  health?: RegistryHealthProjection | null;
  searching?: boolean;
  refreshing?: boolean;
  detailLoading?: boolean;
  detailError?: string | null;
  operationLoading?: string | null;
  onSearch: (query: string) => void;
  onRefresh?: () => void;
  onSelect: (skill: CommunitySkillResult) => void;
  onClearSelection?: () => void;
  onInstall: (skill: CommunitySkillResult, scope: SkillMarketplaceScope, cwd?: string) => void | Promise<void>;
  onScopeChange?: (scope: SkillMarketplaceScope) => void;
}

function installCount(value: number): string {
  if (value < 1_000) return String(value);
  if (value < 1_000_000) return `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}k`;
  return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}m`;
}

function hasInstalledSkill(installedSlugs: string[] | Set<string>, skillId: string): boolean {
  const slug = toMarketplaceSkillSlug(skillId);
  if (!slug) return false;
  return installedSlugs instanceof Set ? installedSlugs.has(slug) : installedSlugs.includes(slug);
}

function RegistryStatus({ health, refreshing, onRefresh }: Pick<SkillMarketplaceProps, "health" | "refreshing" | "onRefresh">) {
  const tone = health === null || health === undefined ? "bg-text-3/40" : health.available ? "bg-accent-2" : "bg-danger";
  return (
    <div className="flex shrink-0 items-center gap-1">
      <span className="flex items-center gap-1.5 text-[10px] text-text-3" title={health?.reason ?? ""}>
        <span className={`inline-block h-2 w-2 rounded-full ${tone}`} aria-hidden="true" />
        skills marketplace
      </span>
      {onRefresh ? <button type="button" className="rounded p-1 text-text-3 hover:bg-panel hover:text-text disabled:opacity-40" aria-label="Refresh skills marketplace" title="Refresh skills marketplace" disabled={refreshing} onClick={onRefresh}><Icon name="refresh" size={13} /></button> : null}
    </div>
  );
}

export default function SkillMarketplace({
  projectCwd = "",
  results,
  popularResults = [],
  selectedSkill = null,
  installedSlugs = [],
  health = null,
  searching = false,
  refreshing = false,
  detailLoading = false,
  detailError = null,
  operationLoading = null,
  onSearch,
  onRefresh,
  onSelect,
  onClearSelection,
  onInstall,
  onScopeChange,
}: SkillMarketplaceProps) {
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState<SkillMarketplaceScope>("user");
  const displayResults = query.trim().length >= 2 ? results : popularResults;
  const selectedResult = selectedSkill
    ? displayResults.find((skill) => skill.id === selectedSkill.id) ?? results.find((skill) => skill.id === selectedSkill.id)
    : null;
  const selectedIsInstalled = selectedResult ? hasInstalledSkill(installedSlugs, selectedResult.skillId) : false;

  useEffect(() => {
    const normalized = query.trim();
    const timer = window.setTimeout(() => onSearch(normalized.length >= 2 ? normalized : ""), normalized.length >= 2 ? 300 : 0);
    return () => window.clearTimeout(timer);
  }, [onSearch, query]);

  const quickFilters = useMemo(() => ["react", "python", "security", "testing", "devops", "best practices"], []);

  function changeScope(next: SkillMarketplaceScope) {
    setScope(next);
    onScopeChange?.(next);
  }

  function install(skill: CommunitySkillResult) {
    if (toMarketplaceSkillSlug(skill.skillId)) onInstall(skill, scope, projectCwd || undefined);
  }

  return (
    <section className="flex min-h-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <RegistryStatus health={health} refreshing={refreshing} onRefresh={onRefresh} />
        <label className="flex min-w-[220px] flex-1 items-center gap-2 rounded-lg border border-line bg-panel px-3 text-text-3">
          <Icon name="search" size={13} />
          <input type="search" aria-label="Search community skills" className="h-9 min-w-0 flex-1 border-0 bg-transparent text-[13px] text-text outline-none placeholder:text-text-3" placeholder="Search community skills" value={query} onChange={(event) => setQuery(event.target.value)} />
        </label>
        <div className="inline-flex rounded-lg border border-line bg-panel p-1">
          {(["user", "project"] as const).map((nextScope) => (
            <button type="button" key={nextScope} className={`rounded-md px-3 py-1.5 text-[12px] capitalize ${scope === nextScope ? "bg-panel-2 text-text" : "text-text-3 hover:text-text"}`} disabled={nextScope === "project" && !projectCwd} aria-pressed={scope === nextScope} onClick={() => changeScope(nextScope)}>{nextScope}</button>
          ))}
        </div>
      </div>

      <div className="flex flex-wrap gap-2">
        {quickFilters.map((filter) => <button type="button" key={filter} className={`rounded-full border px-3 py-1 text-[11px] ${query === filter ? "border-accent/50 bg-accent/10 text-text" : "border-line text-text-3 hover:border-line-2 hover:text-text"}`} onClick={() => setQuery(filter)}>{filter}</button>)}
      </div>

      {searching ? (
        <div className="flex items-center justify-center gap-2 py-10 text-[12px] text-text-3" role="status"><Icon name="refresh" size={14} /> Searching skills marketplace…</div>
      ) : displayResults.length === 0 ? (
        <EmptyState icon={<Icon name="skill" size={18} />} title={query.trim().length >= 2 ? "No community skills found." : "No popular skills available."} action={onRefresh && query.trim().length < 2 ? <button type="button" className="rounded-lg border border-line px-3 py-1.5 text-[12px] text-text-2 hover:bg-panel" onClick={onRefresh}>Retry</button> : undefined}>{query.trim().length >= 2 ? "Try a broader search term." : "Refresh the marketplace or search by capability."}</EmptyState>
      ) : (
        <div className="flex min-h-[300px] min-w-0 gap-3">
          <div className="w-[280px] shrink-0 space-y-2 overflow-y-auto pr-1">
            <div className="px-1 text-[11px] text-text-3">{query.trim().length >= 2 ? `${results.length} results` : "Popular skills"}</div>
            {displayResults.map((skill) => {
              const installed = hasInstalledSkill(installedSlugs, skill.skillId);
              const valid = toMarketplaceSkillSlug(skill.skillId) !== null;
              const pending = operationLoading === skill.id;
              return (
                <article key={skill.id} className={`rounded-lg border px-3 py-2 ${selectedSkill?.id === skill.id ? "border-accent/50 bg-accent/5" : "border-line bg-panel hover:bg-panel-2"}`}>
                  <div className="flex items-start gap-2">
                    <button type="button" className="min-w-0 flex-1 text-left" onClick={() => onSelect(skill)}>
                      <strong className="block truncate text-[13px] font-semibold text-text">{skill.name}</strong>
                      <span className="mt-1 block truncate text-[11px] text-text-3">{skill.installs > 0 ? `${installCount(skill.installs)} installs · ` : ""}{skill.source}</span>
                    </button>
                    <button type="button" className={`shrink-0 rounded-md px-2 py-1 text-[11px] font-semibold ${installed ? "bg-accent-2/10 text-accent-2" : "bg-accent text-on-accent"}`} disabled={installed || !valid || pending} title={!valid ? "The marketplace id cannot be converted to a safe local slug." : undefined} onClick={() => install(skill)}>{pending ? "…" : installed ? "Installed" : valid ? "Install" : "Unavailable"}</button>
                  </div>
                </article>
              );
            })}
          </div>

          <div className="min-w-0 flex-1 overflow-y-auto">
            {detailLoading ? (
              <div className="flex h-full items-center justify-center gap-2 rounded-lg border border-line bg-panel text-[12px] text-text-3" role="status"><Icon name="refresh" size={14} /> Loading preview…</div>
            ) : detailError ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 rounded-lg border border-danger/40 bg-danger/5 p-6 text-center"><Icon name="alert" size={20} /><strong className="text-[13px] text-text">Skill preview unavailable.</strong><span className="max-w-sm text-[12px] text-text-3">{detailError}</span></div>
            ) : selectedSkill ? (
              <div className="space-y-4 rounded-lg border border-line bg-panel p-4">
                <div className="flex items-start gap-3">
                  <div className="min-w-0 flex-1">
                    <h3 className="truncate text-[14px] font-semibold text-text">{selectedSkill.name}</h3>
                    <div className="mt-1 flex flex-wrap gap-2 text-[11px] text-text-3"><span>{selectedSkill.source}</span>{selectedSkill.installs > 0 ? <span>{installCount(selectedSkill.installs)} installs</span> : null}{selectedSkill.githubUrl ? <a className="underline hover:text-text" href={selectedSkill.githubUrl} target="_blank" rel="noreferrer">GitHub</a> : null}{selectedSkill.skillsShUrl ? <a className="underline hover:text-text" href={selectedSkill.skillsShUrl} target="_blank" rel="noreferrer">skills.sh</a> : null}</div>
                    <p className="mt-2 text-[12px] leading-5 text-text-2">{selectedSkill.description || "No description provided."}</p>
                  </div>
                  <button type="button" aria-label="Close skill preview" className="text-text-3 hover:text-text" onClick={() => onClearSelection?.()}><Icon name="close" size={14} /></button>
                </div>
                <div className="border-t border-line pt-3">
                  <h4 className="mb-2 text-[11px] uppercase text-text-3">SKILL.md preview</h4>
                  {selectedSkill.content ? <pre className="max-h-[420px] overflow-auto whitespace-pre-wrap break-words rounded-lg bg-bg p-3 font-mono text-[12px] leading-5 text-text-2">{selectedSkill.content}</pre> : <p className="text-[12px] italic text-text-3">No content preview supplied.</p>}
                </div>
                <div className="flex items-center justify-between gap-3 border-t border-line pt-3">
                  <span className="text-[11px] text-text-3">Install scope: {scope}</span>
                  <button
                    type="button"
                    className={`inline-flex h-9 items-center gap-2 rounded-lg px-4 text-[12px] font-semibold ${selectedIsInstalled ? "border border-line bg-transparent text-text-3" : "bg-accent text-on-accent hover:opacity-90"}`}
                    disabled={selectedIsInstalled || !selectedResult || toMarketplaceSkillSlug(selectedResult.skillId) === null || operationLoading === selectedResult?.id}
                    onClick={() => selectedResult && install(selectedResult)}
                  >
                    <Icon name="download" size={13} />
                    {operationLoading === selectedResult?.id ? "Installing…" : selectedIsInstalled ? "Installed" : "Install skill"}
                  </button>
                </div>
              </div>
            ) : <EmptyState icon={<Icon name="skill" size={18} />} title="Select a community skill.">{"Choose a result to inspect its source, content, and install scope."}</EmptyState>}
          </div>
        </div>
      )}
    </section>
  );
}
