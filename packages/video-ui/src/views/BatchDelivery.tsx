// Batch delivery — one project's timelines x presets as a resumable batch (batch.* engine methods).
import { Badge, Button, Checkbox, Field, Input, Table, TableBody, TableCell, TableHead, TableHeaderCell, TableRow, useToast } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { assetUrl } from "../lib/asset.js";
import { errorCode, errorMessage, onEngineEvent } from "../lib/engine.js";
import {
  type BatchCover,
  type BatchDocument,
  type BatchOutputRow,
  type BatchPreset,
  type BatchRunResult,
  type BatchState,
  type BatchStatus,
  batchCover,
  batchCreate,
  batchList,
  batchRun,
  batchStatus,
  recipeCancel,
  SETTINGS_CHANGED,
  variantsList,
} from "../lib/media.js";
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

type BatchSummary = { id: string; counts?: Record<string, number> };

/** A batch whose finishing settings changed since it was created. */
type Drift = { id: string; differs: string[] };

/** A batch whose finishing settings the engine could not read to compare. */
type CompareError = { id: string; message: string };

/** Ties the changed-settings notice to the Run button that a run will be refused by. */
const DRIFT_NOTICE_ID = "batch-drift-notice";
/** Ties the all-delivered reason to the Run button that is disabled by it. */
const DELIVERED_REASON_ID = "batch-delivered-reason";
const DELIVERED_REASON = "Every output is delivered and the finishing settings are unchanged, so there is nothing to run.";

function isBatchState(v: unknown): v is BatchState {
  return v === "pending" || v === "rendered" || v === "qualified" || v === "failed";
}

/** Engine names that do not read as words once their underscores are spaces. */
const SETTING_WORDS: Record<string, string> = {
  lut_blake3: "LUT file contents",
  lut_path: "LUT file",
  caption_style_in_force: "caption style in use",
  burn_in_force: "burned captions",
  grade_by_source: "per-source grade",
};

const settingWords = (key: string) => SETTING_WORDS[key] ?? key.replace(/_/g, " ");

/** The drift a status reply reports; null when the batch matches or was made before pinning. */
function driftOf(status: BatchStatus): Drift | null {
  return status.finishing_matches === false ? { id: status.id, differs: status.finishing_differs } : null;
}

function asCover(v: unknown): BatchCover | null {
  if (!v || typeof v !== "object") return null;
  const c = v as Partial<BatchCover>;
  return typeof c.path === "string" && typeof c.at_ms === "number" && typeof c.blake3 === "string"
    ? { at_ms: c.at_ms, path: c.path, blake3: c.blake3 }
    : null;
}

function withItem<T>(set: Set<T>, item: T, on: boolean): Set<T> {
  const next = new Set(set);
  if (on) next.add(item);
  else next.delete(item);
  return next;
}

function patchRow(doc: BatchDocument, key: string, patch: Partial<BatchOutputRow>): BatchDocument {
  return { ...doc, outputs: doc.outputs.map((o) => (o.key === key ? { ...o, ...patch } : o)) };
}

function applyRunOutputs(doc: BatchDocument, outs: BatchRunResult["outputs"]): BatchDocument {
  return outs.reduce(
    (acc, o) => patchRow(acc, o.key, { state: o.state, output: o.output, cover: o.cover, error: o.error }),
    doc,
  );
}

/** Batch panel on the Exports screen: build the output list, run it, resume it, and pick covers. */
/** A project-relative path as an absolute one, in the project folder's own separator style. */
function projectFile(dir: string, relative: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  return `${dir.replace(/[\\/]+$/, "")}${sep}${relative.split("/").join(sep)}`;
}

export function BatchDelivery({ dir }: { dir: string }) {
  const toast = useToast();
  const [variants, setVariants] = useState<string[]>([]);
  const [presets, setPresets] = useState<Set<BatchPreset>>(new Set<BatchPreset>(["youtube"]));
  const [timelines, setTimelines] = useState<Set<string>>(new Set([MAIN]));
  const [batches, setBatches] = useState<BatchSummary[]>([]);
  const [batchId, setBatchId] = useState("");
  const [doc, setDoc] = useState<BatchDocument | null>(null);
  const [coverAt, setCoverAt] = useState<Record<string, string>>({});
  const [creating, setCreating] = useState(false);
  const [running, setRunning] = useState<string | null>(null); // job id
  const [summary, setSummary] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [drift, setDrift] = useState<Drift | null>(null);
  const [compareError, setCompareError] = useState<CompareError | null>(null);
  const [statusReads, setStatusReads] = useState(0);
  const runTag = useRef<string | null>(null);
  const selectedId = useRef("");
  // Each read takes a number; only the newest read may write the panel, so a slow older reply
  // cannot clear a drift or compare notice that a newer read has raised.
  const readSeq = useRef(0);

  const refreshList = useCallback(async () => {
    const [list, vars] = await Promise.all([batchList(dir), variantsList(dir)]);
    setBatches(list.batches);
    setVariants(vars.variants.filter((v) => !v.error).map((v) => v.name));
  }, [dir]);

  // Re-reads a batch's status; a reply for a batch that is no longer the selected one is dropped.
  // While a read is in flight the finishing settings are unverified, so Run waits for it. A read
  // that fails leaves them unverified too: the batch's comparison is then reported as unavailable
  // and Run stays disabled until a later read succeeds.
  const loadStatus = useCallback(
    (id: string) => {
      const seq = ++readSeq.current;
      setStatusReads((n) => n + 1);
      return batchStatus(dir, id)
        .then(
          (d) => {
            if (selectedId.current !== id || seq !== readSeq.current) return;
            setDoc(d);
            setDrift(driftOf(d));
            setCompareError(d.finishing_error ? { id: d.id, message: d.finishing_error } : null);
          },
          (e: unknown) => {
            if (selectedId.current === id && seq === readSeq.current) {
              setDrift(null);
              setCompareError({ id, message: `the batch could not be re-read (${errorMessage(e)})` });
            }
            throw e;
          },
        )
        .finally(() => setStatusReads((n) => n - 1));
    },
    [dir],
  );

  // A different project starts with no batch selected.
  useEffect(() => {
    selectedId.current = "";
    setBatchId("");
    setDoc(null);
    setSummary(null);
    setError(null);
    setDrift(null);
    setCompareError(null);
  }, [dir]);

  useEffect(() => {
    refreshList().catch((e) => setError(errorMessage(e)));
  }, [refreshList]);

  // Finishing settings can change elsewhere in the app, so the selected batch's status is re-read when the panel is visible again.
  useEffect(() => {
    const refresh = () => {
      if (selectedId.current) void loadStatus(selectedId.current).catch(() => undefined);
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") refresh();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVisibility);
    refresh(); // a batch already selected when the panel mounts
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [loadStatus]);

  // Finishing settings saved in another view (Settings, Auto edit, the colour inspector) change
  // what this batch compares against. The re-read raises statusReads, so Run stays disabled
  // until it lands. A save to a different project is ignored.
  useEffect(() => {
    const onSettingsChanged = (e: Event) => {
      const savedDir = (e as CustomEvent<{ dir?: string }>).detail?.dir;
      if (savedDir !== undefined && savedDir !== dir) return;
      if (selectedId.current) void loadStatus(selectedId.current).catch(() => undefined);
    };
    window.addEventListener(SETTINGS_CHANGED, onSettingsChanged);
    return () => window.removeEventListener(SETTINGS_CHANGED, onSettingsChanged);
  }, [dir, loadStatus]);

  // batch.output progress for this panel's own run only (frames are matched by the run's tag).
  useEffect(() => {
    let off: (() => void) | undefined;
    let dead = false;
    void onEngineEvent((frame) => {
      const tag = runTag.current;
      if (!tag || frame.tag !== tag) return;
      const ev = frame.event;
      if (ev.event !== "batch.output") return;
      const key = typeof ev.key === "string" ? ev.key : undefined;
      const state = ev.state;
      if (key === undefined || !isBatchState(state)) return;
      const patch: Partial<BatchOutputRow> = { state };
      if (ev.output !== undefined) patch.output = typeof ev.output === "string" ? ev.output : null;
      if (ev.cover !== undefined) patch.cover = asCover(ev.cover);
      if (ev.error !== undefined) patch.error = typeof ev.error === "string" ? ev.error : null;
      if (typeof ev.attempts === "number") patch.attempts = ev.attempts;
      setDoc((d) => (d ? patchRow(d, key, patch) : d));
    }).then((fn) => {
      if (dead) fn();
      else off = fn;
    });
    return () => {
      dead = true;
      off?.();
    };
  }, []);

  // The cross product of the chosen timelines and presets is the output list.
  const timelineOptions = [MAIN, ...variants];
  const planned = timelineOptions
    .filter((t) => timelines.has(t))
    .flatMap((t) =>
      PRESETS.filter((p) => presets.has(p.value)).map((p) => ({
        variant: t === MAIN ? undefined : t,
        preset: p.value,
        label: `${t === MAIN ? "Main timeline" : t} · ${p.label}`,
      })),
    );

  const create = () => {
    setCreating(true);
    setError(null);
    setSummary(null);
    void batchCreate(
      dir,
      planned.map(({ variant, preset }) => ({ variant, preset })),
    )
      .then(async (created) => {
        selectedId.current = created.id;
        setDoc(created);
        setDrift(null);
        setCompareError(null);
        setBatchId(created.id);
        toast(`Batch ${created.id} created`, { kind: "success" });
        await refreshList();
      })
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setCreating(false));
  };

  const choose = (id: string) => {
    selectedId.current = id;
    setBatchId(id);
    setDoc(null);
    setSummary(null);
    setError(null);
    setDrift(null);
    setCompareError(null);
    if (!id) return;
    void loadStatus(id).catch((e) => setError(errorMessage(e)));
  };

  const outputs = doc?.outputs ?? [];
  const driftShown = drift !== null && drift.id === doc?.id;
  const qualified = outputs.filter((o) => o.state === "qualified").length;
  const left = outputs.length - qualified;
  // A fully delivered batch whose settings changed is not "All delivered": its way forward is the re-render.
  const fullyDelivered = outputs.length > 0 && left === 0;
  const runLabel =
    doc === null
      ? "Run"
      : fullyDelivered
        ? driftShown
          ? "Run"
          : "All delivered"
        : qualified === 0
          ? "Run"
          : `Resume (${left} left)`;
  // The engine refuses a plain run while the settings differ, so Run waits for the re-render button.
  // Run also waits while a settings read is in flight, and while this batch's settings cannot be compared.
  const compareShown = compareError !== null && compareError.id === doc?.id;
  const canRun =
    doc !== null && left > 0 && running === null && !driftShown && statusReads === 0 && !compareShown;
  // Run is disabled with a stated reason when every output is delivered and the settings match.
  const deliveredReason = fullyDelivered && !driftShown && !compareShown ? DELIVERED_REASON : null;

  // `finishing: "current"` re-pins the batch to the current settings and renders every output again.
  const run = (finishing?: "current") => {
    if (!doc) return;
    const id = doc.id;
    const job = `batch-ui-${Date.now().toString(36)}`;
    const tag = `batch-${job}`;
    runTag.current = tag;
    setRunning(job);
    setError(null);
    setSummary(null);
    void batchRun(dir, id, job, tag, finishing)
      .then((result) => {
        setDoc((d) => (d && d.id === id ? applyRunOutputs(d, result.outputs) : d));
        const pin =
          result.finishing?.action === "repinned"
            ? " · re-pinned to current settings"
            : result.finishing?.action === "pinned"
              ? " · pinned to current settings"
              : "";
        setSummary(
          `Rendered ${result.ran.length}, kept ${result.skipped.length}${result.status === "partial" ? " · partial, run again to finish" : ""}${pin}`,
        );
      })
      .catch((e) => {
        const code = errorCode(e);
        setError(
          code === "cancelled" || code === "render_cancelled"
            ? "Cancelled — run again to resume where it stopped."
            : errorMessage(e),
        );
      })
      .finally(() => {
        runTag.current = null;
        setRunning(null);
        // A refused run (settings changed) leaves nothing rendered; this fresh status is what shows the notice.
        void loadStatus(id).catch(() => undefined);
        void refreshList().catch(() => undefined);
      });
  };

  const cancel = () => {
    if (running === null) return;
    void recipeCancel(running).catch((e) => setError(errorMessage(e)));
  };

  const setCover = (row: BatchOutputRow) => {
    if (!doc) return;
    const raw = (coverAt[row.key] ?? "0").trim();
    const sec = Number(raw);
    if (raw === "" || !Number.isFinite(sec) || sec < 0) {
      setError(`Enter a cover time in seconds (0 or more) for ${row.key}.`);
      return;
    }
    const id = doc.id;
    setError(null);
    void batchCover(dir, id, row.key, Math.round(sec * 1000))
      .then((res) => setDoc((d) => (d && d.id === id ? patchRow(d, row.key, res) : d)))
      .catch((e) => setError(errorMessage(e)));
  };

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
              onChange={(checked) => setPresets((s) => withItem(s, p.value, checked))}
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
              onChange={(checked) => setTimelines((s) => withItem(s, t, checked))}
              label={t === MAIN ? "Main" : t}
            />
          ))}
        </div>

        {planned.length > 0 ? (
          <ul className="batch-plan" aria-label="Outputs to create">
            {planned.map((p) => (
              <li key={p.label}>{p.label}</li>
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
            onClick={create}
            disabled={(planned.length === 0 || creating || running !== null) || (creating)}
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

        {compareError && compareError.id === doc?.id && (
          <p className="panel-note" role="status" data-testid="batch-compare-error">
            Could not compare this batch with the current finishing settings: {compareError.message}
          </p>
        )}

        {driftShown && drift && (
          <div role="alert" id={DRIFT_NOTICE_ID} className="panel-stack" data-testid="batch-drift">
            <p className="panel-error">
              Finishing settings changed since this batch was created:{" "}
              {drift.differs.length > 0
                ? drift.differs.map(settingWords).join(", ")
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
                disabled={outputs.length === 0 || running !== null}
                onClick={() => run("current")}
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
            aria-describedby={driftShown ? DRIFT_NOTICE_ID : deliveredReason ? DELIVERED_REASON_ID : undefined}
            title={
              driftShown
                ? "Finishing settings changed: re-render all with current settings instead"
                : (deliveredReason ?? undefined)
            }
            onClick={() => run()}
            disabled={(!canRun) || (running !== null)}
            aria-busy={(running !== null) || undefined}
          >{!(running !== null) && <Icon name="play" />}
            {running !== null ? "Running…" : runLabel}
          </Button>
          {running !== null && (
            <Button variant="secondary" size="sm" data-testid="batch-cancel" onClick={cancel}>
              Cancel
            </Button>
          )}
        </div>
        {deliveredReason && (
          <p className="panel-note" id={DELIVERED_REASON_ID} data-testid="batch-delivered-reason">
            {deliveredReason}
          </p>
        )}

        {summary && (
          <p className="panel-note" role="status" data-testid="batch-summary">
            {summary}
          </p>
        )}
        {error && (
          <p role="alert" className="panel-error" data-testid="batch-error">
            {error}
          </p>
        )}

        {doc === null ? (
          <p className="panel-note">
            {batchId ? "Loading batch…" : "Create a batch or choose one to see its outputs."}
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
                            src={assetUrl(projectFile(dir, row.cover.path)) + `?v=${row.cover.blake3}`}
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
                            value={coverAt[row.key] ?? "0"}
                            disabled={!row.output}
                            onChange={(e) => setCoverAt((m) => ({ ...m, [row.key]: e.target.value }))}
                          />
                          <Button
                            size="sm"
                            variant="ghost"
                            data-testid={`batch-cover-set-${row.key}`}
                            disabled={!row.output || running !== null}
                            onClick={() => setCover(row)}
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
