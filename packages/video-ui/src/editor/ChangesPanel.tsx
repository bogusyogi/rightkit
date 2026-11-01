/**
 * Changes tab of the Finished edit side panel (docs/design/app-ui.md §3.4).
 * Reads the stored auto-edit plans via `editplan.list` (analysis/editplan-*.json
 * with every item and take mapped through the live timeline by the engine):
 * items grouped by editorial category with counts, each suggestion with its
 * why text, Apply / Dismiss, and "Use other take" for retake groups.
 *
 * Apply/Dismiss/take picks run through `autoedit.apply` reviews
 * ({accept, reject, takes}) — the engine re-resolves the plan under the review
 * and lands it as real revisions, then the host refreshes `project.show`.
 */
import { Button } from "@rightkit/react-shell/react";
import { useEffect, useMemo, useState } from "react";
import { errorMessage } from "../lib/engine";
import {
  autoeditApply,
  editplanList,
  type PlanItem,
  type PlanTake,
  type StoredPlan,
  type CutPreset,
} from "../lib/media";
import { formatSeconds } from "../lib/project";

import type { EditSession } from "./adapter";

/** Item kind (snake_case) → the spec's Change groups. */
function groupOf(item: PlanItem): "Retakes" | "Pacing" | "Structure" {
  const k = item.kind ?? "";
  if (k.includes("retake")) return "Retakes";
  if (["silence", "long_pause", "breath", "low_energy", "off_mic"].includes(k)) return "Pacing";
  return "Structure";
}

/** Item kind → readable name (title case, no jargon). */
function kindName(kind: string): string {
  return kind
    .split("_")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

type PlanExt = StoredPlan & { config?: { cuts?: { preset?: CutPreset } } };

export function ChangesPanel({
  dir,
  plans,
  loadError,
  session,
  onCommitted,
  onSeek,
}: {
  dir: string;
  plans: StoredPlan[];
  /** Why the stored plans could not be read, when they could not. */
  loadError?: string | null;
  session: EditSession;
  /** Re-read `project.show` + cues after a review lands (or fails). */
  onCommitted: () => void;
  /** Seek the timeline playhead to a timeline-mapped second. */
  onSeek: (timeS: number) => void;
}) {
  const [local, setLocal] = useState<StoredPlan[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [altFor, setAltFor] = useState<string | null>(null);

  const data = local ?? plans;
  useEffect(() => setLocal(null), [plans]);

  const review = useMemo(
    () =>
      async (plan: PlanExt, rv: { reject?: string[]; accept?: string[]; takes?: Record<string, number> }) => {
        setError(null);
        try {
          await autoeditApply(dir, plan.config?.cuts?.preset ?? "natural", {
            source: plan.source ?? plan.source_blake3,
            review: rv,
          });
          const r = await editplanList(dir);
          setLocal(r.plans);
          onCommitted();
        } catch (e) {
          setError(errorMessage(e));
          onCommitted();
        }
      },
    [dir, onCommitted],
  );

  const items = data.flatMap((pl) => pl.items ?? []);
  const pending = items.filter((i) => i.tier === "suggest").length;

  if (loadError) {
    return (
      <p className="cr-muted" role="alert" style={{ margin: 0, fontSize: 12 }}>
        The auto-edit plan could not be read — {loadError}
      </p>
    );
  }
  if (items.length === 0 && data.every((pl) => (pl.take_groups ?? []).length === 0)) {
    return (
      <p className="cr-muted" style={{ margin: 0, fontSize: 12 }}>
        No auto-edit plan on this project yet — changes will appear here after an auto-edit run.
      </p>
    );
  }

  const groups = (["Retakes", "Pacing", "Structure"] as const)
    .map((label) => ({
      label,
      items: data.flatMap((pl) =>
        (pl.items ?? [])
          .filter((i) => groupOf(i) === label)
          .map((item) => ({ plan: pl as PlanExt, item })),
      ),
    }))
    .filter((g) => g.items.length > 0);

  return (
    <>
      <div className="fe-change-group">
        <div className="fe-change-group__head">
          <h4>Auto-edit</h4>
          <span className="rk-badge">{items.length}</span>
          {pending > 0 && <span className="rk-badge">{pending} suggested</span>}
        </div>
      </div>

      {error && (
        <p role="alert" className="cr-muted" style={{ margin: 0, fontSize: 12 }}>
          {error}
        </p>
      )}

      {groups.map((g) => (
        <section key={g.label} className="fe-change-group">
          <div className="fe-change-group__head">
            <h4>{g.label}</h4>
            <span className="rk-badge">{g.items.length}</span>
          </div>
          {g.items.map(({ plan, item }) => (
            <ChangeRow
              key={item.id}
              plan={plan}
              item={item}
              busy={session.busy}
              altOpen={altFor === item.id}
              onToggleAlt={() => setAltFor(altFor === item.id ? null : item.id)}
              onSeek={onSeek}
              onReview={(rv) => void review(plan, rv)}
            />
          ))}
        </section>
      ))}
    </>
  );
}

function ChangeRow({
  plan,
  item,
  busy,
  altOpen,
  onToggleAlt,
  onSeek,
  onReview,
}: {
  plan: PlanExt;
  item: PlanItem;
  busy: boolean;
  altOpen: boolean;
  onToggleAlt: () => void;
  onSeek: (timeS: number) => void;
  onReview: (rv: { reject?: string[]; accept?: string[]; takes?: Record<string, number> }) => void;
}) {
  const suggested = item.tier === "suggest";
  const placed = (item.out?.length ?? 0) > 0;
  const at = item.at_ms ?? item.start_ms;
  const takeGroup = item.take
    ? plan.take_groups?.find((g) => g.id === item.take?.group)
    : undefined;
  const covered = item.covered_by != null;

  const stateChip = suggested ? (
    <span className="rk-badge">Suggested</span>
  ) : placed ? (
    <span className="rk-badge">On timeline</span>
  ) : (
    <span className="rk-badge">Applied</span>
  );

  return (
    <div className="fe-change" data-state={suggested ? "suggested" : "applied"}>
      <div className="fe-change__head">
        <button
          type="button"
          className="fe-change__label"
          title={`${kindName(item.kind)} · ${formatSeconds(at / 1000)} on the timeline`}
          onClick={() => onSeek(at / 1000)}
        >
          {item.text ? `\u201C${item.text}\u201D` : kindName(item.kind)}
        </button>
        {stateChip}
      </div>
      {item.reason && <p className="fe-change__reason">{item.reason}</p>}
      {covered && (
        <p className="fe-change__reason">Already covered by a higher-priority change.</p>
      )}
      <div className="fe-change__actions">
        {suggested && (
          <>
            <Button size="sm" disabled={busy} onClick={() => onReview({ accept: [item.id] })}>
              Apply
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => onReview({ reject: [item.id] })}
            >
              Dismiss
            </Button>
          </>
        )}
        {takeGroup && takeGroup.takes.length > 1 && (
          <Button size="sm" variant="ghost" disabled={busy} onClick={onToggleAlt}>
            {altOpen ? "Hide takes" : "Use other take"}
          </Button>
        )}
      </div>
      {altOpen && takeGroup && (
        <div className="fe-change__alts">
          {takeGroup.takes.map((alt: PlanTake) => {
            const isCurrent = alt.chosen === true || alt.index === takeGroup.chosen;
            return (
              <div key={alt.index} className={`fe-take${isCurrent ? " fe-take--current" : ""}`}>
                <span className="fe-take__label" title={alt.text}>
                  Take {alt.index + 1}
                  {alt.at_ms != null && ` · ${formatSeconds(alt.at_ms / 1000)}`}
                  {alt.rank != null && ` · rank ${alt.rank}`}
                </span>
                {isCurrent ? (
                  <span className="rk-badge">Current</span>
                ) : (
                  <Button
                    size="sm"
                    disabled={busy}
                    onClick={() => onReview({ takes: { [takeGroup.id]: alt.index } })}
                  >
                    Use this take
                  </Button>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
