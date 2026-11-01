import { diagnosticsCardStatusClass } from "../shared.js";
import type { RemoteStatusCardProps } from "./types.js";

export function RemoteStatusCard({ title, value, status, detail }: RemoteStatusCardProps) {
  return (
    <div
      className={`flex flex-col gap-1 rounded-lg border px-3 py-2 ${diagnosticsCardStatusClass(status)}`}
    >
      <span className="text-[11px] uppercase text-text-3">{title}</span>
      <strong className="text-[14px] font-semibold text-text">{value}</strong>
      {detail ? <span className="text-[12px] text-text-3 break-all">{detail}</span> : null}
    </div>
  );
}
