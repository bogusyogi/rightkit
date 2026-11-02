export { diagnosticsLabel, diagnosticsStatusClass } from "../shared.js";
import { diagnosticsLabel, diagnosticsStatusClass } from "../shared.js";
import type { DiagnosticsReport } from "@rightkit/agent-ui-core/lib/types";

export function RemoteStatusCard({
  title,
  value,
  status,
  detail,
}: {
  title: string;
  value: string;
  status: DiagnosticsReport["verdict"];
  detail: string;
}) {
  return (
    <div className="flex items-start justify-between gap-3 p-4 rounded-xl border border-line bg-panel">
      <div className="flex flex-col gap-1 min-w-0">
        <div className="flex items-center gap-2 min-w-0">
          <strong className="text-[13px] text-text truncate">{title}</strong>
          <span
            className={`px-2 py-0.5 rounded-full text-[11px] border ${diagnosticsStatusClass(
              status,
            )}`}
          >
            {diagnosticsLabel(status)}
          </span>
        </div>
        <span className="text-[12px] text-text-2">{value}</span>
        <span className="text-[12px] text-text-3 break-all">{detail}</span>
      </div>
    </div>
  );
}
