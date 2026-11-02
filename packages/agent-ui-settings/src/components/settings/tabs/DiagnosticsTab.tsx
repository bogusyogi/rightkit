import { useEffect, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import ReadinessCard from "../../ReadinessCard.js";

import type {
  DaemonConfig,
  DiagnosticsReport,
  InstructionStatus,
  SupportBundle,
} from "@rightkit/agent-ui-core/lib/types";
import { ENABLE_READINESS_PANEL, getReadiness, type ReadinessSnapshot } from "@rightkit/agent-ui-core/lib/daemon";
import { DiagnosticsStat, diagnosticsLabel, diagnosticsStatusClass, OfflinePanel } from "./shared.js";

export default function DiagnosticsTab({
  config,
  report,
  supportBundle,
  supportBundlePending,
  instructionStatus,
  loading,
  error,
  onRefresh,
  onCreateBundle,
  onDownloadBundle,
}: {
  config: DaemonConfig | null;
  report: DiagnosticsReport | null;
  supportBundle: SupportBundle | null;
  supportBundlePending: string | null;
  instructionStatus: InstructionStatus | null;
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
  onCreateBundle: () => void;
  onDownloadBundle: () => void;
}) {
  const [readinessSnapshot, setReadinessSnapshot] = useState<ReadinessSnapshot | null>(null);

  useEffect(() => {
    if (!ENABLE_READINESS_PANEL || !config) {
      setReadinessSnapshot(null);
      return;
    }
    let cancelled = false;
    getReadiness(config)
      .then((snapshot) => {
        if (!cancelled) setReadinessSnapshot(snapshot);
      })
      .catch(() => {
        if (!cancelled) setReadinessSnapshot(null);
      });
    return () => {
      cancelled = true;
    };
  }, [config]);

  return (
    <section className="flex flex-col gap-4 p-6 max-w-[980px]">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-[16px] font-bold text-text">Diagnostics</h3>
          <p className="text-[12px] text-text-3 truncate">
            {report
              ? `Verdict: ${diagnosticsLabel(report.verdict)}`
              : "Runtime readiness has not been loaded"}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
          disabled={loading}
          onClick={onRefresh}
        >
          <Icon name="activity" />
          {loading ? "Refreshing" : "Refresh"}
        </button>
      </div>

      {error ? <OfflinePanel title="Diagnostics unavailable." action={error} /> : null}

      {readinessSnapshot ? <ReadinessCard snapshot={readinessSnapshot} /> : null}

      <div className="rounded-xl border border-line bg-panel overflow-hidden">
        <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-line">
          <div className="min-w-0">
            <h4 className="text-[13px] font-semibold text-text">Support bundle</h4>
            <p className="text-[12px] text-text-3 truncate">
              {supportBundle
                ? `${supportBundle.bundleId} · ${supportBundle.byteLength} bytes`
                : "No bundle created this session"}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
              disabled={supportBundlePending !== null}
              onClick={onCreateBundle}
            >
              <Icon name="activity" />
              {supportBundlePending === "create" ? "Creating" : "Create bundle"}
            </button>
            <button
              type="button"
              className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
              disabled={!supportBundle || supportBundlePending !== null}
              onClick={onDownloadBundle}
            >
              <Icon name="download" />
              {supportBundlePending === "download" ? "Downloading" : "Download"}
            </button>
          </div>
        </div>
        {supportBundle ? (
          <div className="flex flex-col gap-3 px-4 py-3">
            <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
              <DiagnosticsStat label="Crash reports" value={String(supportBundle.crashReports)} />
              <DiagnosticsStat
                label="Native artifacts"
                value={String(supportBundle.nativeArtifacts)}
              />
              <DiagnosticsStat label="Log events" value={String(supportBundle.logEvents)} />
            </div>
            <span className="text-[12px] text-text-3">
              Redacted ZIP of crash reports and a summary. Nothing is uploaded.
            </span>
          </div>
        ) : null}
      </div>

      <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
        {report?.checks.map((check) => (
          <div
            className="flex items-start justify-between gap-3 p-4 rounded-xl border border-line bg-panel"
            key={check.id}
          >
            <div className="flex flex-col gap-1 min-w-0">
              <div className="flex items-center gap-2 min-w-0">
                <strong className="text-[13px] text-text truncate">{check.label}</strong>
                <span
                  className={`px-2 py-0.5 rounded-full text-[11px] border ${diagnosticsStatusClass(
                    check.status,
                  )}`}
                >
                  {diagnosticsLabel(check.status)}
                </span>
              </div>
              <span className="text-[12px] text-text-2">{check.detail}</span>
              {check.fix ? <span className="text-[12px] text-text-3">{check.fix}</span> : null}
            </div>
          </div>
        ))}
      </div>

      {instructionStatus ? (
        <div className="rounded-xl border border-line bg-panel overflow-hidden">
          <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-line">
            <div className="min-w-0">
              <h4 className="text-[13px] font-semibold text-text">Instruction sources</h4>
              <p className="text-[12px] text-text-3 truncate">
                {instructionStatus.fileCount} files · {instructionStatus.composedBytes} composed
                bytes
              </p>
            </div>
          </div>
          {instructionStatus.files.length ? (
            <div className="divide-y divide-line">
              {instructionStatus.files.map((file) => {
                const filters = [
                  ...file.globs.map((glob) => `glob:${glob}`),
                  ...file.paths.map((path) => `path:${path}`),
                ];
                return (
                  <div
                    className="grid grid-cols-[86px_1fr_110px] gap-3 px-4 py-3 text-[12px]"
                    key={`${file.scope}-${file.path}`}
                  >
                    <span className="text-text-2 capitalize">{file.scope}</span>
                    <span className="text-text truncate" title={file.path}>
                      {file.path}
                    </span>
                    <span className="text-text-3 text-right">{file.bodyLines} lines</span>
                    {filters.length ? (
                      <span className="col-start-2 col-span-2 text-text-3 truncate">
                        {filters.join(" · ")}
                      </span>
                    ) : null}
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="px-4 py-5 text-[12px] text-text-3">
              No instruction files were discovered for this workspace.
            </div>
          )}
        </div>
      ) : null}

      {!report && !error ? (
        <OfflinePanel
          title="No diagnostics loaded."
          action="Refresh to load daemon readiness checks."
        />
      ) : null}
    </section>
  );
}
