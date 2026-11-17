// Batch delivery — one project's timelines x presets as a resumable batch (batch.* engine methods).
import { Badge, Button, Checkbox, Field, Input, Table, TableBody, TableCell, TableHead, TableHeaderCell, TableRow, useToast } from "@rightkit/react-shell/react";
import type { BatchPreset, BatchState } from "../../lib/media.js";
import { useVideoBatchDelivery } from "../hooks/useVideoBatchDelivery.js";
import { Icon } from "../ui/index.js";

const PRESETS: { value: BatchPreset; label: string; shape: "wide" | "tall" | "square" }[] = [
  { value: "youtube", label: "YouTube 16:9", shape: "wide" },
  { value: "reels", label: "Reels 9:16", shape: "tall" },
  { value: "tiktok", label: "TikTok 9:16", shape: "tall" },
  { value: "square", label: "Square 1:1", shape: "square" },
];
const PRESET_BY_ID: Record<string, (typeof PRESETS)[number]> = Object.fromEntries(
  PRESETS.map((p) => [p.value, p] as const),
);

const STATE_LABEL: Record<BatchState, string> = {
  pending: "Pending",
  rendered: "Rendered",
  qualified: "Qualified",
  failed: "Failed",
};
const STATE_TONE: Record<BatchState, "neutral" | "accent" | "ok" | "bad"> = {
  pending: "neutral",
  rendered: "accent",
  qualified: "ok",
  failed: "bad",
};

/** Timeline key for the main timeline; saved variants use their own names. */
const MAIN = "";

/** Ties the changed-settings notice to the Run button that a run will be refused by. */
const DRIFT_NOTICE_ID = "batch-drift-notice";
/** Ties the all-delivered reason to the Run button that is disabled by it. */
const DELIVERED_REASON_ID = "batch-delivered-reason";
const DELIVERED_REASON = "Every output is delivered and the finishing settings are unchanged, so there is nothing to run.";

/** Engine names that do not read as words once their underscores are spaces. */
const SETTING_WORDS: Record<string, string> = {
  lut_blake3: "LUT file contents",
  lut_path: "LUT file",
  caption_style_in_force: "caption style in use",
  burn_in_force: "burned captions",
  grade_by_source: "per-source grade",
};

const settingWords = (key: string) => SETTING_WORDS[key] ?? key.replace(/_/g, " ");

export function BatchDelivery({ dir, onChanged, captureSelection }: {
  dir: string;
  onChanged?: (dir: string) => Promise<void>;
  captureSelection?: () => () => boolean;
}) {
  const toast = useToast();
  const {
    coverUrl, presets, setPreset, timelines, setTimeline, timelineOptions, planned,
    batches, batchId, doc, outputs, coverTime, setCoverTime, coverError, creating, canCreate, canEditCover, canSetCover,
    running, runningBatchId, result, cancelled, runError, error, compareError, readError, drift,
    qualified, left, fullyDelivered, cancelling, cancelDeclined, cancelError,
    canRun, canRerender, choose, create, run, cancel, setCover,
  } = useVideoBatchDelivery({ dir, onChanged });
  const creatingBatch = () => {
    const canNotify = captureSelection?.();
    void create().then((id) => {
      if (id && (canNotify?.() ?? true)) toast("Batch " + id + " created", { kind: "success" });
    });
  };
  const runLabel = !doc || drift ? "Run" : fullyDelivered ? "All delivered" : qualified ? "Resume (" + left + " left)" : "Run";
  const deliveredReason = fullyDelivered && !drift && !compareError && !readError ? DELIVERED_REASON : null;
  const pin =
    result?.finishing?.action === "repinned"
      ? " · re-pinned to current settings"
      : result?.finishing?.action === "pinned"
        ? " · pinned to current settings"
        : "";
  const publicError = coverError
    ? "Enter a cover time in seconds (0 or more) for " + coverError + "."
    : error ?? readError ?? (cancelled ? "Cancelled — run again to resume where it stopped." : runError);

  return (
    <section className="batch-delivery" data-testid="batch-delivery" aria-label="Batch delivery">
      <h2 className="panel-title">Batch delivery</h2>

      <div className="panel-stack">
        <span className="cr-overline">Presets</span>
        <div className="batch-checks" role="group" aria-label="Presets">
          {PRESETS.map((p) => (
            <Checkbox
              key={p.value}
              checked={presets.has(p.value)}
              onChange={(checked) => setPreset(p.value, checked)}
              label={p.label}
            />
          ))}
        </div>

        <span className="cr-overline">Timelines</span>
        <div className="batch-checks" role="group" aria-label="Timelines">
          {timelineOptions.map((t) => (
            <Checkbox
              key={t || "main"}
              checked={timelines.has(t)}
              onChange={(checked) => setTimeline(t, checked)}
              label={t === MAIN ? "Main" : t}
            />
          ))}
        </div>

        {planned.length > 0 ? (
          <ul className="batch-plan" aria-label="Outputs to create">
            {planned.map(({ variant, preset }) => (
              <li key={JSON.stringify([variant, preset])}>{variant ?? "Main timeline"} · {PRESET_BY_ID[preset].label}</li>
            ))}
          </ul>
        ) : (
          <p className="panel-note">Pick at least one preset and one timeline.</p>
        )}
        <div className="panel-row">
          <Button
            variant="secondary"
            size="sm"
            data-testid="batch-create"
            onClick={creatingBatch}
            disabled={!canCreate}
            aria-busy={(creating) || undefined}
          >
            {creating ? "Creating…" : "Create batch"}
          </Button>
        </div>
      </div>

      <div className="panel-stack">
        <Field label="Batch">{(ids) => (<select {...ids}
            className="rk-input cr-select"
            data-testid="batch-select"
            value={batchId}
            onChange={(e) => choose(e.target.value)}
          >
            <option value="">{batches.length === 0 ? "No batches yet" : "Choose a batch…"}</option>
            {batches.map((b) => (
              <option key={b.id} value={b.id}>
                {b.id}
              </option>
            ))}
          </select>)}</Field>

        {compareError && (
          <p className="panel-note" role="status" data-testid="batch-compare-error">
            Could not compare this batch with the current finishing settings: {compareError}
          </p>
        )}

        {drift && (
          <div role="alert" id={DRIFT_NOTICE_ID} className="panel-stack" data-testid="batch-drift">
            <p className="panel-error">
              Finishing settings changed since this batch was created:{" "}
              {drift.length > 0
                ? drift.map(settingWords).join(", ")
                : "an unreported setting"}
              .
            </p>
            <p className="panel-note">
              Outputs already delivered would no longer match the rest. Restore the settings, or re-render every output
              with the current ones.
            </p>
            <div className="panel-row">
              <Button
                variant="secondary"
                size="sm"
                data-testid="batch-rerender"
                disabled={!canRerender}
                onClick={() => void run("current")}
              >
                Re-render all with current settings
              </Button>
            </div>
          </div>
        )}

        <div className="panel-row">
          <Button
            variant="primary"
            size="sm"
            data-testid="batch-run"
            aria-describedby={drift ? DRIFT_NOTICE_ID : deliveredReason ? DELIVERED_REASON_ID : undefined}
            title={
              drift
                ? "Finishing settings changed: re-render all with current settings instead"
                : (deliveredReason ?? undefined)
            }
            onClick={() => void run()}
            disabled={!canRun}
            aria-busy={running !== null || undefined}
          >{running === null && <Icon name="play" />}
            {running !== null ? "Running…" : runLabel}
          </Button>
          {running !== null && (
            <Button variant="secondary" size="sm" data-testid="batch-cancel" onClick={cancel} disabled={cancelling}>
              {cancelling ? "Cancelling…" : "Cancel"}
            </Button>
          )}
        </div>
        {runningBatchId && runningBatchId !== batchId && (
          <p className="panel-note" role="status">Running batch {runningBatchId}.</p>
        )}
        {cancelError && (
          <p className="panel-error" role="alert" data-testid="batch-cancel-error">Could not cancel: {cancelError}</p>
        )}
        {cancelDeclined && (
          <p className="panel-note" role="status" data-testid="batch-cancel-declined">Batch did not accept cancellation. You can retry.</p>
        )}
        {deliveredReason && (
          <p className="panel-note" id={DELIVERED_REASON_ID} data-testid="batch-delivered-reason">
            {deliveredReason}
          </p>
        )}

        {result && (
          <p className="panel-note" role="status" data-testid="batch-summary">
            Rendered {result.ran.length}, kept {result.skipped.length}{result.status === "partial" ? " · partial, run again to finish" : ""}{pin}
          </p>
        )}
        {publicError && (
          <p role="alert" className="panel-error" data-testid="batch-error">
            {publicError}
          </p>
        )}

        {doc === null ? (
          <p className="panel-note">
            {batchId ? (readError ? "Could not load batch." : "Loading batch…") : "Create a batch or choose one to see its outputs."}
          </p>
        ) : outputs.length === 0 ? (
          <p className="panel-note">This batch has no outputs.</p>
        ) : (
          <div className="batch-table-wrap">
            <Table className="batch-table" label="Batch outputs">
              <TableHead>
                <TableRow>
                  <TableHeaderCell>Output</TableHeaderCell>
                  <TableHeaderCell>Preset</TableHeaderCell>
                  <TableHeaderCell>State</TableHeaderCell>
                  <TableHeaderCell>Attempts</TableHeaderCell>
                  <TableHeaderCell>Cover</TableHeaderCell>
                  <TableHeaderCell>Choose cover</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {outputs.map((row) => {
                  const shape = PRESET_BY_ID[row.preset]?.shape ?? "wide";
                  return (
                    <TableRow key={row.key} data-testid={`batch-row-${row.key}`}>
                      <TableCell>
                        <span className="cr-mono">{row.key}</span>
                      </TableCell>
                      <TableCell>{PRESET_BY_ID[row.preset]?.label ?? row.preset}</TableCell>
                      <TableCell>
                        <span data-testid={`batch-state-${row.key}`}>
                          <Badge tone={STATE_TONE[row.state]}>{STATE_LABEL[row.state]}</Badge>
                        </span>
                        {row.error && <p className="batch-table__error">{row.error}</p>}
                      </TableCell>
                      <TableCell className="cr-mono">{row.attempts}</TableCell>
                      <TableCell>
                        {row.cover ? (
                          <img
                            className={`batch-cover batch-cover--${shape}`}
                            src={coverUrl(row.cover)}
                            alt={`Cover for ${row.key}`}
                            data-testid={`batch-cover-${row.key}`}
                            loading="lazy"
                            draggable={false}
                          />
                        ) : (
                          <span className="panel-note">None yet</span>
                        )}
                      </TableCell>
                      <TableCell>
                        <div className="batch-cover-set">
                          <Input
                            type="number"
                            className="batch-cover-input"
                            min={0}
                            step={0.1}
                            aria-label={`Cover time for ${row.key} in seconds`}
                            data-testid={`batch-cover-input-${row.key}`}
                            value={coverTime(row.key)}
                            disabled={!canEditCover(row)}
                            onChange={(e) => setCoverTime(row.key, e.target.value)}
                          />
                          <Button
                            size="sm"
                            variant="ghost"
                            data-testid={`batch-cover-set-${row.key}`}
                            disabled={!canSetCover(row)}
                            onClick={() => void setCover(row)}
                          >
                            Set cover
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </div>
        )}
      </div>
    </section>
  );
}
