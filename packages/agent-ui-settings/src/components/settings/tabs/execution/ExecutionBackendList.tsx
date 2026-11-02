import type { ExecutionBackendConfig, ExecutionNetworkProfile } from "@rightkit/agent-ui-core/lib/types";
import { ExecutionBackendCard } from "./ExecutionBackendCard.js";

interface ExecutionBackendListProps {
  backends: ExecutionBackendConfig[];
  defaultBackend: string;
  doctorReports: Record<string, import("@rightkit/agent-ui-core/lib/types").BackendDoctorReport>;
  pendingId: string | null;
  networkProfiles: ExecutionNetworkProfile[];
  onDoctor: (backendId: string) => void;
  onSetDefault: (backendId: string) => void;
  onSetEnabled: (backendId: string, enabled: boolean) => void;
  onSaveResourceLimits: (
    backendId: string,
    limits: { cpu?: number | null; memoryMb?: number | null; diskMb?: number | null },
  ) => void;
  onSaveNetworkProfile: (backendId: string, networkProfile: string | null) => void;
}

export function ExecutionBackendList({
  backends,
  defaultBackend,
  doctorReports,
  pendingId,
  networkProfiles,
  onDoctor,
  onSetDefault,
  onSetEnabled,
  onSaveResourceLimits,
  onSaveNetworkProfile,
}: ExecutionBackendListProps) {
  if (backends.length === 0) {
    return (
      <p className="px-4 py-3 text-[13px] text-text-3">No execution backends are registered.</p>
    );
  }
  return (
    <div className="flex flex-col divide-y divide-line border border-line rounded-xl overflow-hidden max-w-[1100px]">
      {backends.map((backend) => (
        <ExecutionBackendCard
          key={backend.id}
          backend={backend}
          isDefault={backend.id === defaultBackend}
          doctorReport={doctorReports[backend.id]}
          doctorPending={pendingId === `doctor:${backend.id}`}
          defaultPending={pendingId === `default:${backend.id}`}
          enabledPending={pendingId === `enabled:${backend.id}`}
          limitsPending={pendingId === `limits:${backend.id}`}
          networkPending={pendingId === `network:${backend.id}`}
          networkProfiles={networkProfiles}
          onDoctor={() => onDoctor(backend.id)}
          onSetDefault={() => onSetDefault(backend.id)}
          onSetEnabled={() => onSetEnabled(backend.id, !backend.enabled)}
          onSaveResourceLimits={(limits) => onSaveResourceLimits(backend.id, limits)}
          onSaveNetworkProfile={(networkProfile) =>
            onSaveNetworkProfile(backend.id, networkProfile)
          }
        />
      ))}
    </div>
  );
}
