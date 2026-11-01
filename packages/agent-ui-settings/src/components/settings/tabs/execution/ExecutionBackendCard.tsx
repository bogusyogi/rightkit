import { Select } from "@rightkit/react-shell/react";
import { useState } from "react";
import type {
  BackendDoctorReport,
  ExecutionBackendConfig,
  ExecutionNetworkProfile,
} from "@rightkit/agent-ui-core/lib/types";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { ExecutionBackendLimitsForm } from "./ExecutionBackendLimitsForm.js";
import type { NetworkProfileChange, ResourceLimitsChange, ResourceLimitsDraft } from "./types.js";

interface ExecutionBackendCardProps {
  backend: ExecutionBackendConfig;
  isDefault: boolean;
  doctorReport?: BackendDoctorReport;
  doctorPending: boolean;
  defaultPending: boolean;
  enabledPending: boolean;
  limitsPending: boolean;
  networkPending: boolean;
  networkProfiles: ExecutionNetworkProfile[];
  onDoctor: () => void;
  onSetDefault: () => void;
  onSetEnabled: () => void;
  onSaveResourceLimits: ResourceLimitsChange;
  onSaveNetworkProfile: NetworkProfileChange;
}

export function ExecutionBackendCard({
  backend,
  isDefault,
  doctorReport,
  doctorPending,
  defaultPending,
  enabledPending,
  limitsPending,
  networkPending,
  networkProfiles,
  onDoctor,
  onSetDefault,
  onSetEnabled,
  onSaveResourceLimits,
  onSaveNetworkProfile,
}: ExecutionBackendCardProps) {
  const [resourceDraft, setResourceDraft] = useState<ResourceLimitsDraft>(() =>
    initialResourceDraft(backend),
  );
  const [networkDraft, setNetworkDraft] = useState(backend.networkProfile ?? "");
  const verdict = doctorReport?.verdict ?? (backend.enabled ? "warn" : "block");
  const networkDirty = networkDraft !== (backend.networkProfile ?? "");
  return (
    <article className="grid grid-cols-[minmax(0,1.1fr)_minmax(0,1.4fr)_minmax(0,1.2fr)_auto] gap-4 px-4 py-4">
      <div className="flex flex-col gap-2 min-w-0">
        <BackendIdentity backend={backend} isDefault={isDefault} />
        <span className="text-[12px] text-text-3 break-all">{backend.workspaceLayout.root}</span>
      </div>

      <ExecutionBackendLimitsForm
        backend={backend}
        draft={resourceDraft}
        pending={limitsPending}
        onDraftChange={(update) => setResourceDraft((current) => update(current))}
        onSave={onSaveResourceLimits}
      />

      <DoctorPanel
        backend={backend}
        doctorReport={doctorReport}
        verdict={verdict}
        networkDraft={networkDraft}
        networkProfiles={networkProfiles}
        networkDirty={networkDirty}
        networkPending={networkPending}
        onNetworkDraftChange={setNetworkDraft}
        onSaveNetworkProfile={onSaveNetworkProfile}
      />

      <BackendActions
        backend={backend}
        isDefault={isDefault}
        doctorPending={doctorPending}
        defaultPending={defaultPending}
        enabledPending={enabledPending}
        onDoctor={onDoctor}
        onSetDefault={onSetDefault}
        onSetEnabled={onSetEnabled}
      />
    </article>
  );
}

function BackendIdentity({
  backend,
  isDefault,
}: {
  backend: ExecutionBackendConfig;
  isDefault: boolean;
}) {
  return (
    <>
      <div className="flex items-center gap-2 min-w-0">
        <strong className="text-[13px] text-text truncate">{backend.name}</strong>
        {isDefault ? (
          <span className="px-2 py-0.5 rounded-full border border-accent-2/40 text-accent-2 text-[11px]">
            default
          </span>
        ) : null}
        <span
          className={`px-2 py-0.5 rounded-full border text-[11px] ${
            backend.enabled ? "border-accent-2/40 text-accent-2" : "border-danger/40 text-danger"
          }`}
        >
          {backend.enabled ? "enabled" : "disabled"}
        </span>
      </div>
      <span className="text-[12px] text-text-3 truncate">
        {backend.kind} · {backend.id}
      </span>
    </>
  );
}

function DoctorPanel({
  backend,
  doctorReport,
  verdict,
  networkDraft,
  networkProfiles,
  networkDirty,
  networkPending,
  onNetworkDraftChange,
  onSaveNetworkProfile,
}: {
  backend: ExecutionBackendConfig;
  doctorReport?: BackendDoctorReport;
  verdict: BackendDoctorReport["verdict"];
  networkDraft: string;
  networkProfiles: ExecutionNetworkProfile[];
  networkDirty: boolean;
  networkPending: boolean;
  onNetworkDraftChange: (value: string) => void;
  onSaveNetworkProfile: NetworkProfileChange;
}) {
  return (
    <div className="flex flex-col gap-2 min-w-0">
      <div className="flex items-center gap-2">
        <span className="text-[11px] uppercase tracking-[0.08em] text-text-3">Doctor</span>
        <span
          className={`px-2 py-0.5 rounded-full border text-[11px] ${executionVerdictClass(verdict)}`}
        >
          {doctorReport ? verdictLabel(doctorReport.verdict) : "Not run"}
        </span>
      </div>
      <span className="text-[12px] text-text-3 truncate">
        Network: {backend.networkProfile || "none"}
      </span>
      <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
        <label className="flex flex-col gap-1 min-w-0">
          <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">Profile</span>
          <Select value={networkDraft} options={[{ value: "", label: "None" }, ...networkProfiles.map((profile) => ({ value: profile.id, label: profile.name }))]} onChange={(value) => onNetworkDraftChange(value)} label="Profile" className="h-8 min-w-0 rounded-lg border border-line bg-panel px-2 text-[12px] text-text outline-none focus:border-accent/60" />
        </label>
        <button
          type="button"
          className="self-end inline-flex items-center justify-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40 whitespace-nowrap"
          disabled={!networkDirty || networkPending}
          onClick={() => onSaveNetworkProfile(networkDraft.trim() || null)}
        >
          <Icon name="check" />
          {networkPending ? "Saving" : "Apply"}
        </button>
      </div>
      {doctorReport ? (
        <div className="flex flex-col gap-1">
          {doctorReport.checks.slice(0, 3).map((check) => (
            <span className="text-[12px] text-text-2 truncate" key={check.id}>
              {verdictLabel(check.verdict)} · {check.label}
            </span>
          ))}
        </div>
      ) : (
        <span className="text-[12px] text-text-3">
          Run doctor before selecting this for real work.
        </span>
      )}
    </div>
  );
}

function BackendActions({
  backend,
  isDefault,
  doctorPending,
  defaultPending,
  enabledPending,
  onDoctor,
  onSetDefault,
  onSetEnabled,
}: {
  backend: ExecutionBackendConfig;
  isDefault: boolean;
  doctorPending: boolean;
  defaultPending: boolean;
  enabledPending: boolean;
  onDoctor: () => void;
  onSetDefault: () => void;
  onSetEnabled: () => void;
}) {
  return (
    <div className="flex flex-col items-stretch gap-2">
      <ActionButton
        disabled={isDefault || enabledPending}
        onClick={onSetEnabled}
        icon={backend.enabled ? "pause" : "play"}
        pending={enabledPending}
        savingLabel="Saving"
        idleLabel={backend.enabled ? "Disable" : "Enable"}
      />
      <ActionButton
        disabled={isDefault || !backend.enabled || defaultPending}
        onClick={onSetDefault}
        icon="check"
        pending={defaultPending}
        savingLabel="Setting"
        idleLabel="Set default"
      />
      <ActionButton
        disabled={doctorPending}
        onClick={onDoctor}
        icon="activity"
        pending={doctorPending}
        savingLabel="Checking"
        idleLabel="Run doctor"
      />
    </div>
  );
}

function ActionButton({
  disabled,
  onClick,
  icon,
  pending,
  savingLabel,
  idleLabel,
}: {
  disabled: boolean;
  onClick: () => void;
  icon: "check" | "pause" | "play" | "activity";
  pending: boolean;
  savingLabel: string;
  idleLabel: string;
}) {
  return (
    <button
      type="button"
      className="inline-flex items-center justify-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40 whitespace-nowrap"
      disabled={disabled}
      onClick={onClick}
    >
      <Icon name={icon} />
      {pending ? savingLabel : idleLabel}
    </button>
  );
}

function initialResourceDraft(backend: ExecutionBackendConfig): ResourceLimitsDraft {
  return {
    cpu: formatLimitInput(backend.resourceLimits.cpu),
    memoryMb: formatLimitInput(backend.resourceLimits.memoryMb),
    diskMb: formatLimitInput(backend.resourceLimits.diskMb),
  };
}

function formatLimitInput(value?: number | null) {
  return value ? String(value) : "";
}

function executionVerdictClass(status: BackendDoctorReport["verdict"]) {
  if (status === "pass") return "border-accent-2/40 text-accent-2";
  if (status === "warn") return "border-accent/40 text-accent";
  return "border-danger/40 text-danger";
}

function verdictLabel(verdict: BackendDoctorReport["verdict"]) {
  if (verdict === "pass") return "Pass";
  if (verdict === "warn") return "Warn";
  return "Block";
}
