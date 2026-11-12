import { Badge, Button, Card, EmptyState, PageShell } from "@rightkit/react-shell/react";
import { type ProgressState } from "../hooks/useProgress.js";
import { ErrorPanel, ProgressRow, timecode, Icon } from "../ui/index.js";
import { hostAppName } from "../appName.js";

function elapsedOf(progress: ProgressState): string | undefined {
  // Frames don't carry start times; show the render's out-time as elapsed.
  const r = progress.render;
  if (r?.out_time_ms !== undefined) return timecode(r.out_time_ms);
  return undefined;
}

/** Inline render bar kept for screens that embed it (Finished edit). */
export function RenderBar({ progress }: { progress: ProgressState }) {
  const r = progress.render;
  if (!r) return null;
  const pct =
    r.total_ms && r.out_time_ms !== undefined
      ? Math.max(0, Math.min(1, r.out_time_ms / r.total_ms))
      : undefined;
  return (
    <ProgressRow
      label="Render"
      status={pct === undefined ? "Rendering" : `${Math.round(pct * 100)}%`}
      tone="info"
      fraction={pct}
      elapsed={r.frame !== undefined ? `frame ${r.frame}` : undefined}
    />
  );
}

export function ProgressView({
  progress,
  jobError,
  onCancel,
  assembling,
  resumable,
  renderMissing,
  recovered,
  onResume,
}: {
  progress: ProgressState;
  jobError: string | null;
  onCancel: () => void;
  assembling: boolean;
  /** An unfinished assemble exists for this project (engine says running/failed/cancelled). */
  resumable: boolean;
  /** The recovered run's stored render file is gone; it must run again. */
  renderMissing?: boolean;
  /** Labels of the recovered assemble's jobs (shot prompts) when the engine
   *  has no seeded request rows to show. */
  recovered?: string[];
  onResume: () => void;
}) {
  const jobs = Object.values(progress.jobs);
  const error = progress.error?.message ?? jobError;
  const running = progress.running || assembling;
  // Recovered run: resumable and not currently executing (owner review: show
  // recovered jobs as ProgressRows with a `Recovered` chip, never "Nothing
  // running" + a floating Resume).
  const showRecovered = resumable && !running && !progress.done;
  const recoveredLabels = jobs.length > 0 ? jobs.map((j) => j.label) : (recovered ?? []);
  const idle =
    !running && !showRecovered && jobs.length === 0 && !progress.render && !error && !progress.done && !progress.cancelled;
  const renderPct =
    progress.render?.total_ms && progress.render.out_time_ms !== undefined
      ? Math.max(0, Math.min(1, progress.render.out_time_ms / progress.render.total_ms))
      : undefined;

  return (
    <PageShell title="Progress" actions={
        <>
          {running && (
            <Button variant="secondary" onClick={onCancel}>
              {progress.render ? "Cancel render" : "Cancel assembly"}
            </Button>
          )}
          {showRecovered && (
            <Button variant="secondary" onClick={onResume}><Icon name="rotate-ccw" />
              Resume
            </Button>
          )}
        </>
      } maxWidth={0} subtitle={running ? "Generation running" : progress.done ? "Finished" : showRecovered ? "Recovered job" : undefined}>
      <div className="shell-stack">
        {idle && (
          <EmptyState title="Nothing running" icon={<Icon name="check" width={28} height={28} />}>
            Generation and render jobs show up here while they run.
          </EmptyState>
        )}
        {showRecovered && (
          <Card>
            {recoveredLabels.length === 0 ? (
              <ProgressRow label="Assembly" status="Recovered" tone="info" fraction={null} />
            ) : (
              recoveredLabels.map((label, i) => (
                <ProgressRow key={i} label={label} status="Recovered" tone="info" fraction={null} />
              ))
            )}
          </Card>
        )}
        {renderMissing && !running && (
          <ErrorPanel
            title="This render is missing"
            detail="The export file no longer exists. Resume to render it again."
          />
        )}
        {error && (
          <ErrorPanel
            title="The job failed"
            detail={error}
            actions={
              resumable && !running ? (
                <Button variant="secondary" onClick={onResume}><Icon name="rotate-ccw" />
                  Resume
                </Button>
              ) : undefined
            }
          />
        )}
        {progress.done && (
          <div>
            <Badge tone="ok">Assembly complete</Badge>
          </div>
        )}
        {progress.cancelled && (
          <p className="m-0 text-[13px]" style={{ color: "var(--rk-ink-2)" }}>
            Cancelled. Finished {hostAppName()} jobs are kept; Resume continues with the same assemble id and
            does not submit them again.
          </p>
        )}
        {progress.resumed && (
          <p className="m-0 text-[12px]" style={{ color: "var(--rk-ink-2)" }}>
            Resumed an earlier run: existing jobs are reused.
          </p>
        )}
        {progress.stages.length > 0 && (
          <Card>
            {progress.stages.map((s, i) => (
              <ProgressRow
                key={s}
                label={s}
                status={i === progress.stages.length - 1 && running ? "Running" : "Done"}
                tone={i === progress.stages.length - 1 && running ? "info" : "ok"}
                fraction={i === progress.stages.length - 1 && running ? undefined : null}
              />
            ))}
          </Card>
        )}
        {jobs.length > 0 && !showRecovered && (
          <Card>
            {jobs.map((j) => (
              <ProgressRow
                key={j.id}
                label={j.label}
                status={j.status ?? j.request ?? "queued"}
                tone={j.status === "failed" ? "danger" : j.imported ? "ok" : "info"}
                elapsed={j.stage ?? undefined}
                error={j.status === "failed" ? (j.message ?? "failed") : undefined}
              />
            ))}
          </Card>
        )}
        {progress.render && (
          <Card>
            <ProgressRow
              label="Render"
              status={renderPct === undefined ? "Rendering" : `${Math.round(renderPct * 100)}%`}
              tone="info"
              fraction={renderPct}
              elapsed={elapsedOf(progress)}
            />
          </Card>
        )}
        {progress.timeline && (
          <p className="m-0 text-[12px]" style={{ color: "var(--rk-ink-2)" }}>
            Timeline built: {progress.timeline.video_clips ?? 0} clips,{" "}
            {progress.timeline.titles ?? 0} titles
          </p>
        )}
        {progress.imports > 0 && (
          <p className="m-0 text-[13px]" aria-live="polite">
            Imported {progress.imports} asset{progress.imports === 1 ? "" : "s"}
          </p>
        )}
      </div>
    </PageShell>
  );
}
