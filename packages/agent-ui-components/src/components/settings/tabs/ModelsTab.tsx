import { Checkbox, Select } from "@rightkit/react-shell/react";
import { useEffect, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { ModelsTabProps } from "./ModelsTabProps.js";
import { OfflinePanel, DiagnosticsStat } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import RootModelPicker from "@rightkit/agent-ui-settings/components/settings/tabs/RootModelPicker";
import { listInferenceCatalog } from "@rightkit/agent-ui-core/lib/daemon/inference";
import {
  LocalModelCookbookEmpty,
  ModelSettingsEmptyState,
  localGpuMemoryLabel,
  localModelFitClass,
  localModelFitLabel,
  localModelSmokeCandidate,
  localModelVramLabel,
} from "@rightkit/agent-ui-settings/components/settings/tabs/models/ModelEmptyStates";

// UI contract anchors preserved across decomposition: Model settings attach to a workspace Open local folder Workspace required Choose workspace Load roles [grid-template-columns:repeat(auto-fit,minmax(190px,1fr))] Inspect local model fit probe before save
export default function ModelsTab({
  settings,
  settingsDraft,
  catalogGroups: catalogGroupsProp,
  config,
  workspaceRoot,
  localModelCatalog,
  localModelProbeBaseUrl,
  localModelProbeResult,
  localModelChatSmokeResult,
  localProviderSuggestion,
  localModelActionPending,
  localModelError,
  onSettingsDraft,
  onRefreshLocalModels,
  onLocalModelProbeBaseUrl,
  onProbeLocalModelEndpoint,
  onSmokeLocalModelEndpoint,
  onSuggestLocalProvider,
  onOpenWorkspace,
}: ModelsTabProps) {
  const [catalogGroups, setCatalogGroups] = useState(catalogGroupsProp ?? []);
  useEffect(() => {
    if (catalogGroupsProp) {
      setCatalogGroups(catalogGroupsProp);
      return;
    }
    if (!config) return;
    void listInferenceCatalog(config)
      .then(setCatalogGroups)
      .catch(() => setCatalogGroups([]));
  }, [catalogGroupsProp, config]);
  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex flex-col gap-1">
        <h3 className="text-[16px] font-bold text-text">Models</h3>
        <p className="text-[13px] text-text-2">
          Pick the default orchestration model and keep a visible check on enabled models.
        </p>
      </div>
      {settings && settingsDraft ? (
        <>
          <div className="max-w-[640px]">
            <h4 className="mb-2 text-[13px] font-semibold text-text">Conductor</h4>
            <RootModelPicker
              catalogGroups={catalogGroups}
              value={settingsDraft.primarySelection}
              onSelect={(primarySelection) =>
                onSettingsDraft({ ...settingsDraft, primarySelection })
              }
            />
          </div>
          <div className="flex flex-col gap-3 max-w-[640px] rounded-xl border border-line bg-panel p-4">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <h4 className="text-[13px] font-semibold text-text">Role model policy</h4>
                  {!settingsDraft.roleModelSplitAvailable ? (
                    <span className="rounded-full border border-line px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-text-3">
                      Pro
                    </span>
                  ) : null}
                </div>
                <p className="mt-1 text-[12px] text-text-3">
                  {settingsDraft.roleModelSplitAvailable
                    ? "Choose one conductor model for every role, or assign models per role."
                    : "Every role uses the conductor model. Per-role models are available with Pro."}
                </p>
              </div>
              {settingsDraft.roleModelSplitAvailable ? (
                <Select value={settingsDraft.roleModelMode} options={[{ value: "single", label: "One model" }, { value: "split", label: "Per-role models" }]} onChange={(value) => onSettingsDraft({
                      ...settingsDraft,
                      roleModelMode: value as typeof settingsDraft.roleModelMode,
                    })} label="Role model policy" className="h-8 shrink-0 rounded-lg border border-line bg-panel-2 px-2 text-[12px] text-text outline-none" />
              ) : null}
            </div>
            {settingsDraft.roleModelSplitAvailable && settingsDraft.roleModelMode === "split" ? (
              <div className="flex flex-col border-t border-line/60 pt-2">
                <h5 className="px-1 pb-1 text-[12px] font-semibold text-text-2">Per-role models</h5>
                {settings.agents.map((agent) => {
                  const value =
                    settingsDraft.roleSelections?.[agent.id] ?? settingsDraft.primarySelection;
                  return (
                    <div
                      className="grid min-h-9 grid-cols-[minmax(0,1fr)_240px] items-center gap-3 border-b border-line/60 px-1"
                      key={agent.id}
                    >
                      <div className="min-w-0">
                        <strong className="text-[13px] font-medium text-text truncate">
                          {agent.name}
                        </strong>
                        <span className="text-[12px] text-text-3"> · {agent.role}</span>
                      </div>
                      <div className="max-w-[360px]">
                        <RootModelPicker
                          compact
                          catalogGroups={catalogGroups}
                          value={value}
                          onSelect={(selection) =>
                            onSettingsDraft({
                              ...settingsDraft,
                              roleSelections: {
                                ...(settingsDraft.roleSelections ?? {}),
                                [agent.id]: selection,
                              },
                            })
                          }
                        />
                      </div>
                    </div>
                  );
                })}
              </div>
            ) : null}
          </div>
          <div className="flex flex-col">
            {(() => {
              // The model catalog is flattened from per-role assignments, so the
              // same underlying model (e.g. moonshotai/kimi-k2.6) appears once per
              // role that uses it. Collapse to one row per unique provider+slug,
              // preferring the entry that is the current primary orchestrator.
              const primary = settingsDraft.primaryOrchestratorModelId;
              const seen = new Set<string>();
              return [...settings.models]
                .sort((a, b) => Number(b.id === primary) - Number(a.id === primary))
                .filter((m) => {
                  const k = `${m.provider}/${m.modelSlug}`;
                  if (seen.has(k)) return false;
                  seen.add(k);
                  return true;
                });
            })().map((m) => (
              <div
                className="grid grid-cols-[minmax(0,1fr)_240px_auto] items-center gap-3 px-4 h-9 border-b border-line/60"
                key={m.id}
              >
                <strong className="text-[13px] font-semibold text-text truncate">
                  {m.displayName || m.id}
                </strong>
                <span className="text-[12px] text-text-3 truncate">
                  {m.provider} · {m.modelSlug}
                </span>
                <code
                  className={`text-[11px] font-mono ${
                    m.id === settingsDraft.primaryOrchestratorModelId
                      ? "text-accent-2"
                      : "text-text-3"
                  }`}
                >
                  {m.id === settingsDraft.primaryOrchestratorModelId
                    ? "current"
                    : m.supportsTools
                      ? "tools"
                      : "text"}
                </code>
              </div>
            ))}
          </div>
          <div className="flex flex-col gap-2 max-w-[640px] rounded-xl border border-line bg-panel p-4">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <h4 className="text-[13px] font-semibold text-text">Capability router</h4>
                <p className="text-[12px] text-text-3">
                  Routes tasks by capability inside the selected role model policy.
                </p>
              </div>
              <Checkbox
                checked={settingsDraft.routing.capabilityRouter}
                onChange={(checked) =>
                  onSettingsDraft({
                    ...settingsDraft,
                    routing: {
                      ...settingsDraft.routing,
                      capabilityRouter: checked,
                    },
                  })
                }
                label={
                  <>
                    <span className="text-[13px] text-text-2">Enabled</span>
                    {settingsDraft.routing.capabilityRouterSource ? (
                      <span
                        className="rounded-full border border-line bg-panel-2 px-2 py-0.5 text-[10px] font-medium text-text-3"
                        title={`Source: ${settingsDraft.routing.capabilityRouterSource}`}
                      >
                        {settingsDraft.routing.capabilityRouterSource}
                      </span>
                    ) : null}
                  </>
                }
                className="inline-flex items-center gap-2 shrink-0 cursor-pointer"
              />
            </div>
            <label className="flex flex-col gap-1 text-[13px] text-text-2 max-w-[280px]">
              Aggressiveness
              <Select value={settingsDraft.routing.aggressiveness} options={[{ value: "conservative", label: "Conservative" }, { value: "balanced", label: "Balanced" }, { value: "aggressive", label: "Aggressive" }]} onChange={(value) => onSettingsDraft({
                    ...settingsDraft,
                    routing: {
                      ...settingsDraft.routing,
                      aggressiveness: value as typeof settingsDraft.routing.aggressiveness,
                    },
                  })} label="Aggressiveness" className="h-9 px-3 rounded-lg border border-line bg-panel-2 text-text text-[13px] outline-none disabled:opacity-40" disabled={!settingsDraft.routing.capabilityRouter} />
            </label>
          </div>
        </>
      ) : !workspaceRoot.trim() ? (
        <ModelSettingsEmptyState onOpenWorkspace={onOpenWorkspace} />
      ) : (
        <OfflinePanel
          title="Model settings will load after the local engine connects."
          action="Retry engine from the workspace screen."
        />
      )}
      <div className="rounded-xl border border-line bg-panel overflow-hidden">
        <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-line">
          <div className="min-w-0">
            <h4 className="text-[13px] font-semibold text-text">Local model cookbook</h4>
            <p className="text-[12px] text-text-3 truncate">
              Hardware fit, runtime readiness, endpoint probing, and workspace route setup.
            </p>
          </div>
          <button
            type="button"
            className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
            disabled={localModelActionPending !== null}
            onClick={onRefreshLocalModels}
          >
            <Icon name="refresh" />
            {localModelActionPending === "refresh" ? "Refreshing" : "Refresh"}
          </button>
        </div>
        {localModelError ? (
          <div className="px-4 py-3">
            <OfflinePanel title="Local model cookbook unavailable." action={localModelError} />
          </div>
        ) : null}
        {localModelCatalog ? (
          <div className="flex flex-col gap-4 px-4 py-3">
            <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
              <DiagnosticsStat
                label="CPU threads"
                value={String(localModelCatalog.hardware.cpuThreads)}
              />
              <DiagnosticsStat
                label="Memory"
                value={
                  localModelCatalog.hardware.totalMemoryMb
                    ? `${localModelCatalog.hardware.totalMemoryMb} MB`
                    : "unknown"
                }
              />
              <DiagnosticsStat
                label="Detected runtimes"
                value={String(
                  localModelCatalog.hardware.runtimes.filter((runtime) => runtime.detected).length,
                )}
                detail={`${localModelCatalog.hardware.runtimes.length} checked`}
              />
              <DiagnosticsStat
                label="GPU / VRAM"
                value={
                  localModelCatalog.hardware.gpuDevices.length > 0
                    ? `${localModelCatalog.hardware.gpuDevices.length} GPU`
                    : "not detected"
                }
                detail={localGpuMemoryLabel(localModelCatalog.hardware.gpuDevices)}
              />
            </div>
            {localModelCatalog.hardware.gpuDevices.length > 0 ||
            localModelCatalog.hardware.gpuNotes.length > 0 ? (
              <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
                {localModelCatalog.hardware.gpuDevices.map((gpu) => (
                  <div
                    className="rounded-lg border border-line bg-panel-2 px-3 py-2 min-w-0"
                    key={`${gpu.name}-${gpu.source}`}
                  >
                    <div className="flex items-center justify-between gap-3">
                      <strong className="text-[13px] text-text truncate">{gpu.name}</strong>
                      <span className="px-2 py-0.5 rounded-full border border-accent-2/40 text-accent-2 text-[11px]">
                        {gpu.memoryMb ? `${gpu.memoryMb} MB` : "VRAM unknown"}
                      </span>
                    </div>
                    <p className="text-[12px] text-text-3 truncate">
                      {[gpu.vendor, gpu.source].filter(Boolean).join(" · ")}
                    </p>
                  </div>
                ))}
                {localModelCatalog.hardware.gpuDevices.length === 0
                  ? localModelCatalog.hardware.gpuNotes.slice(0, 2).map((note) => (
                      <div
                        className="rounded-lg border border-line bg-panel-2 px-3 py-2 text-[12px] text-text-3"
                        key={note}
                      >
                        {note}
                      </div>
                    ))
                  : null}
              </div>
            ) : null}
            <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
              {localModelCatalog.hardware.runtimes.map((runtime) => (
                <div
                  className="rounded-lg border border-line bg-panel-2 px-3 py-2 min-w-0"
                  key={runtime.id}
                >
                  <div className="flex items-center justify-between gap-3">
                    <strong className="text-[13px] text-text truncate">{runtime.name}</strong>
                    <span
                      className={`px-2 py-0.5 rounded-full border text-[11px] ${
                        runtime.detected
                          ? "border-accent-2/40 text-accent-2"
                          : "border-line text-text-3"
                      }`}
                    >
                      {runtime.detected ? "detected" : "not found"}
                    </span>
                  </div>
                  <p className="text-[12px] text-text-3 truncate">
                    {runtime.version || runtime.installHint}
                  </p>
                </div>
              ))}
            </div>
            <div className="rounded-lg border border-line bg-panel-2 p-3">
              <div className="grid grid-cols-1 sm:grid-cols-[minmax(0,1fr)_auto_auto] gap-2">
                <input
                  aria-label="Local model base URL"
                  className="h-9 rounded-lg border border-line bg-bg px-3 text-[13px] text-text outline-none"
                  value={localModelProbeBaseUrl}
                  onChange={(event) => onLocalModelProbeBaseUrl(event.target.value)}
                />
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-9 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
                  disabled={localModelActionPending !== null || !localModelProbeBaseUrl.trim()}
                  onClick={onProbeLocalModelEndpoint}
                >
                  <Icon name="activity" />
                  {localModelActionPending === "probe" ? "Probing" : "Probe"}
                </button>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-9 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
                  disabled={
                    localModelActionPending !== null ||
                    !localModelProbeBaseUrl.trim() ||
                    !localModelSmokeCandidate(localModelProbeResult, localModelCatalog)
                  }
                  onClick={() =>
                    onSmokeLocalModelEndpoint(
                      localModelSmokeCandidate(localModelProbeResult, localModelCatalog),
                    )
                  }
                >
                  <Icon name="spark" />
                  {localModelActionPending === "chat-smoke" ? "Checking" : "Chat smoke"}
                </button>
              </div>
              {localModelProbeResult ? (
                <div className="mt-3 text-[12px] text-text-2">
                  {localModelProbeResult.ok
                    ? `${localModelProbeResult.modelCount} model(s): ${localModelProbeResult.models.slice(0, 4).join(", ") || "none returned"}`
                    : localModelProbeResult.error || "Probe failed"}
                </div>
              ) : null}
              {localModelChatSmokeResult ? (
                <div className="mt-2 text-[12px] text-text-2">
                  {localModelChatSmokeResult.ok
                    ? `Chat OK (${localModelChatSmokeResult.model}): ${localModelChatSmokeResult.responsePreview || "empty response"}`
                    : localModelChatSmokeResult.error || "Chat smoke failed"}
                </div>
              ) : null}
            </div>
            <div className="flex flex-col divide-y divide-line border border-line rounded-lg overflow-hidden">
              {localModelCatalog.recommendations.map((model) => (
                <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-3 py-3" key={model.id}>
                  <div className="min-w-0">
                    <div className="flex items-center gap-2 min-w-0">
                      <strong className="text-[13px] text-text truncate">{model.name}</strong>
                      <span
                        className={`px-2 py-0.5 rounded-full border text-[11px] ${localModelFitClass(
                          model.fit,
                        )}`}
                      >
                        {localModelFitLabel(model.fit)}
                      </span>
                    </div>
                    <p className="text-[12px] text-text-3 truncate">
                      {model.providerRuntime} · {model.modelSlug} · {model.fitReason}
                    </p>
                    <p className="text-[12px] text-text-3 truncate">
                      {localModelVramLabel(model)} · {model.fitConfidence} confidence
                    </p>
                  </div>
                  <button
                    type="button"
                    className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
                    disabled={localModelActionPending !== null}
                    onClick={() => onSuggestLocalProvider(model.endpointHint, model.modelSlug)}
                  >
                    <Icon name="plus" size={13} />
                    Save default
                  </button>
                </div>
              ))}
            </div>
            {localProviderSuggestion ? (
              <div className="rounded-lg border border-accent-2/30 bg-bg px-3 py-2 text-[12px] text-text-2">
                <strong className="text-text">
                  {localProviderSuggestion.applied ? "Saved " : ""}
                  {localProviderSuggestion.displayName}
                </strong>
                <div className="mt-1 grid grid-cols-1 md:grid-cols-3 gap-2">
                  <code>{localProviderSuggestion.providerId}</code>
                  <code>{localProviderSuggestion.baseUrl}</code>
                  <code>{localProviderSuggestion.model}</code>
                </div>
                <p className="mt-1 text-text-3">
                  {localProviderSuggestion.setDefault
                    ? `Workspace default saved in ${localProviderSuggestion.configPath}`
                    : localProviderSuggestion.note}
                </p>
              </div>
            ) : null}
          </div>
        ) : (
          <LocalModelCookbookEmpty onRefreshLocalModels={onRefreshLocalModels} />
        )}
      </div>
    </section>
  );
}
