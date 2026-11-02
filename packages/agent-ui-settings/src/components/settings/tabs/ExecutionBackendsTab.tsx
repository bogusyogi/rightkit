import Icon from "@rightkit/agent-ui-core/components/Icon";
import type {
  BackendDoctorReport,
  CoreSandboxSettings,
  CoreSandboxTier,
  DiagnosticsReport,
  ExecutionBackendRegistry,
  ExecutionNetworkDefault,
} from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "./shared.js";
import { CoreSandboxTierPanel } from "./execution/CoreSandboxTierPanel.js";
import { ExecutionBackendList } from "./execution/ExecutionBackendList.js";
import { ExecutionNetworkProfilesPanel } from "./execution/ExecutionNetworkProfilesPanel.js";
import { RemoteStatusCard } from "./execution/RemoteStatusCard.js";

export default function ExecutionBackendsTab({
  registry,
  diagnostics,
  sandboxSettings,
  doctorReports,
  pendingId,
  error,
  workspaceRoot,
  onRefresh,
  onDoctor,
  onSetDefault,
  onSetEnabled,
  onSaveResourceLimits,
  onSaveNetworkProfile,
  onSaveNetworkProfileDefinition,
  onDeleteNetworkProfileDefinition,
  onSaveSandboxTier,
}: {
  registry: ExecutionBackendRegistry | null;
  diagnostics: DiagnosticsReport | null;
  sandboxSettings: CoreSandboxSettings | null;
  doctorReports: Record<string, BackendDoctorReport>;
  pendingId: string | null;
  error: string | null;
  workspaceRoot: string;
  onRefresh: () => void;
  onDoctor: (backendId: string) => void;
  onSetDefault: (backendId: string) => void;
  onSetEnabled: (backendId: string, enabled: boolean) => void;
  onSaveResourceLimits: (
    backendId: string,
    limits: { cpu?: number | null; memoryMb?: number | null; diskMb?: number | null },
  ) => void;
  onSaveNetworkProfile: (backendId: string, networkProfile: string | null) => void;
  onSaveSandboxTier: (tier: CoreSandboxTier) => void;
  onSaveNetworkProfileDefinition: (
    profileId: string,
    body: {
      name?: string;
      default?: ExecutionNetworkDefault;
      allowHosts?: string[];
      allowPrivateNetworks?: boolean;
      denyPrivateNetworks?: boolean;
    },
  ) => void;
  onDeleteNetworkProfileDefinition: (profileId: string) => void;
}) {
  const backends = registry?.backends ?? [];
  const defaultBackend = registry?.defaultBackend ?? "local";
  const profiles = registry?.networkProfiles ?? [];
  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex items-start justify-between gap-3">
        <div className="flex flex-col gap-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">Execution</h3>
          <p className="text-[13px] text-text-2 truncate">
            Default backend {defaultBackend} · {workspaceRoot || "workspace not selected"}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
          disabled={pendingId === "list"}
          onClick={onRefresh}
        >
          <Icon name="refresh" />
          {pendingId === "list" ? "Refreshing" : "Refresh"}
        </button>
      </div>

      {error ? <OfflinePanel title="Execution backends unavailable." action={error} /> : null}

      <CoreSandboxTierPanel
        report={diagnostics}
        settings={sandboxSettings}
        pending={pendingId === "sandbox-tier"}
        onSaveTier={onSaveSandboxTier}
      />

      {registry ? (
        <>
          <div className="grid grid-cols-1 xl:grid-cols-3 gap-3 max-w-[980px]">
            <RemoteStatusCard
              title="Default backend"
              value={defaultBackend}
              status="pass"
              detail="New sessions inherit this execution target unless a request overrides it."
            />
            <RemoteStatusCard
              title="Network profiles"
              value={`${profiles.length} profile${profiles.length === 1 ? "" : "s"}`}
              status={profiles.length > 0 ? "pass" : "warn"}
              detail={profiles.map((profile) => profile.id).join(", ")}
            />
            <RemoteStatusCard
              title="Enabled backends"
              value={`${backends.filter((backend) => backend.enabled).length}/${backends.length}`}
              status={backends.some((backend) => backend.enabled) ? "pass" : "block"}
              detail="Local execution is the lean-launch path; disabled entries stay visible for diagnostics."
            />
          </div>

          <ExecutionNetworkProfilesPanel
            profiles={profiles}
            pendingId={pendingId}
            workspaceRoot={workspaceRoot}
            onSaveDefinition={onSaveNetworkProfileDefinition}
            onDeleteDefinition={onDeleteNetworkProfileDefinition}
          />

          <ExecutionBackendList
            backends={backends}
            defaultBackend={defaultBackend}
            doctorReports={doctorReports}
            pendingId={pendingId}
            networkProfiles={profiles}
            onDoctor={onDoctor}
            onSetDefault={onSetDefault}
            onSetEnabled={onSetEnabled}
            onSaveResourceLimits={onSaveResourceLimits}
            onSaveNetworkProfile={onSaveNetworkProfile}
          />
        </>
      ) : (
        <OfflinePanel
          title="Execution backend registry has not loaded."
          action="Refresh after the local engine is connected."
        />
      )}
    </section>
  );
}
