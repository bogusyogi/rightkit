import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { errorCode, errorMessage } from "../../lib/engine.js";
import { reframeAnalyzeMissing, reframeApproveChecked, reframeShow, thumbnails, type ReframeSummary } from "../../lib/media.js";
import { assetUrl } from "../../lib/asset.js";

type AnchorView = { mid_ms: number; approved: boolean; crop: { x: number; y: number; w: number; h: number } };

function anchorViews(plan: ReframeSummary): AnchorView[] {
  const p = (plan.plan ?? {}) as {
    anchors?: { start_ms?: number; end_ms?: number; approved?: boolean }[];
    keyframes?: { t_ms?: number; x?: number; y?: number }[];
    source_width?: number;
    source_height?: number;
  };
  const anchors = p.anchors ?? [];
  const keyframes = p.keyframes ?? [];
  const [sw, sh] = [p.source_width ?? 0, p.source_height ?? 0];
  const [cw, ch] = plan.crop ?? [0, 0];
  const out: AnchorView[] = [];
  const step = Math.max(1, Math.floor(anchors.length / 6) || 1);
  for (let i = 0; i < anchors.length && out.length < 6; i += step) {
    const a = anchors[i];
    const mid = Math.round(((a.start_ms ?? 0) + (a.end_ms ?? 0)) / 2);
    let k = keyframes[0];
    for (const cand of keyframes) {
      if (Math.abs((cand.t_ms ?? 0) - mid) < Math.abs((k?.t_ms ?? 0) - mid)) k = cand;
    }
    out.push({
      mid_ms: mid,
      approved: a.approved === true,
      crop: {
        x: sw ? (k?.x ?? 0) / sw : 0,
        y: sh ? (k?.y ?? 0) / sh : 0,
        w: sw ? cw / sw : 0,
        h: sh ? ch / sh : 0,
      },
    });
  }
  return out;
}

export type FramingPlanState = ReframeSummary | "none" | "error";
export type FramingPlanView = FramingPlanState | "loading";
type FramingEvent = {
  key: string;
  dir: string;
  source: string;
  aspect: string;
  busy: string | null;
  result?: FramingPlanState;
  error?: string;
};
const mutations = new Map<string, string>();
const listeners = new Set<(event: FramingEvent) => void>();
const reads = new Map<string, Set<{ valid: boolean }>>();
const framingKey = (dir: string, source: string, aspect: string) => JSON.stringify([dir, source, aspect]);

export function framingReadFailure(error: unknown, source: string, aspect: string): "none" | "error" {
  return errorCode(error) === "bad_input"
    && errorMessage(error) === `source ${source} has no reframe plan for ${aspect}; run reframe.analyze first`
    ? "none"
    : "error";
}

export async function readCurrentFraming(
  dir: string,
  source: string,
  aspect: string,
  apply: (plan: ReframeSummary) => void,
  onError?: (error: unknown) => void,
) {
  const key = framingKey(dir, source, aspect);
  const pending = reads.get(key) ?? new Set<{ valid: boolean }>();
  reads.set(key, pending);
  const token = { valid: true };
  pending.add(token);
  try {
    let result: ReframeSummary;
    try {
      result = await reframeShow(dir, source, aspect);
    } catch (error) {
      if (!token.valid) return;
      if (onError) onError(error);
      else throw error;
      return;
    }
    // Apply in the same turn as the validity check. A later mutation queues its update last.
    if (token.valid) apply(result);
  } finally {
    pending.delete(token);
    if (!pending.size) reads.delete(key);
  }
}
function emit(event: FramingEvent) { for (const listener of listeners) listener(event); }

/** Publish a stored plan to every view of the same project, source and aspect. */
export function publishFramingPlan(dir: string, source: string, aspect: string, result: FramingPlanState, error?: string) {
  const key = framingKey(dir, source, aspect);
  for (const read of reads.get(key) ?? []) read.valid = false;
  emit({ key, dir, source, aspect, busy: mutations.get(key) ?? null, result, error });
}

export function useFramingPlanChanges(dir: string, apply: (source: string, aspect: string, plan: FramingPlanState) => void) {
  const callback = useRef(apply);
  useLayoutEffect(() => { callback.current = apply; }, [apply]);
  useLayoutEffect(() => {
    const listener = (event: FramingEvent) => {
      if (event.dir === dir && event.result) callback.current(event.source, event.aspect, event.result);
    };
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  }, [dir]);
}

export function useVideoFramingReview({ dir, source, aspect, plan, onChanged }: {
  dir: string; source: { id: string }; aspect: string;
  plan?: FramingPlanView;
  onChanged: (plan: ReframeSummary) => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [shots, setShots] = useState<{ mid_ms: number; src?: string; crop: AnchorView["crop"]; approved: boolean }[]>([]);
  const scope = useRef<object | null>(null);
  const callback = useRef(onChanged);
  useLayoutEffect(() => { callback.current = onChanged; }, [onChanged]);
  useLayoutEffect(() => {
    const token = {}; scope.current = token;
    setBusy(null); setMutationError(null);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir, source.id, aspect]);
  useLayoutEffect(() => {
    const token = scope.current;
    const key = framingKey(dir, source.id, aspect);
    setBusy(mutations.get(key) ?? null);
    const listener = (event: FramingEvent) => {
      if (event.key !== key || scope.current !== token) return;
      setBusy(event.busy);
      if (typeof event.result === "object") callback.current(event.result);
      setMutationError((previous) => event.error ?? (event.busy === null ? previous : null));
    };
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  }, [dir, source.id, aspect]);
  const [previewPlan, setPreviewPlan] = useState<ReframeSummary | null>(null);
  const previewRequest = useRef<object | null>(null);
  useLayoutEffect(() => {
    const request = {};
    previewRequest.current = request;
    setShots([]);
    setPreviewPlan(null);
    setPreviewError(null);
    return () => { if (previewRequest.current === request) previewRequest.current = null; };
  }, [dir, source.id, aspect, plan]);
  useEffect(() => {
    const request = previewRequest.current;
    if (!request || typeof plan !== "object") return;
    const current = () => previewRequest.current === request;
    const views = anchorViews(plan);
    thumbnails(dir, { source: source.id }, { times_ms: views.map((v) => v.mid_ms), width: 640 })
      .then((t) => {
        if (!current()) return;
        setShots(views.map((v, i) => ({ ...v, src: t.frames[i] ? assetUrl(t.frames[i].path) : undefined })));
        setPreviewPlan(plan);
      })
      .catch((e) => { if (current()) setPreviewError(errorMessage(e)); });
  }, [dir, source.id, aspect, plan]);
  const approvalInvalidReason = typeof plan !== "object" || !plan.plan_hash?.trim() ? "missing_plan_hash" : previewPlan !== plan ? "preview_not_ready" : null;
  const mutate = async (kind: "analyse" | "approve" | "read") => {
    const token = scope.current;
    const key = framingKey(dir, source.id, aspect);
    if (!token || mutations.has(key) || (kind === "approve" && approvalInvalidReason) || (kind === "analyse" && plan !== "none")) return;
    const notify = callback.current;
    const event = { key, dir, source: source.id, aspect, busy: kind };
    const publish = (result: ReframeSummary, error?: string) => {
      publishFramingPlan(dir, source.id, aspect, result, error);
      if (scope.current !== token) notify(result);
    };
    mutations.set(key, kind);
    emit(event);
    try {
      if (kind === "read") {
        await readCurrentFraming(dir, source.id, aspect, publish, (error) => {
          const state = framingReadFailure(error, source.id, aspect);
          publishFramingPlan(dir, source.id, aspect, state, state === "error" ? errorMessage(error) : undefined);
        });
        return;
      }
      const result = kind === "analyse"
        ? await reframeAnalyzeMissing(dir, source.id, aspect, `frame-${source.id.slice(0, 8)}`)
        : await reframeApproveChecked(dir, source.id, { approver: "ui", aspect, expected_plan_hash: (plan as ReframeSummary).plan_hash });
      publish(result);
    } catch (e) {
      const error = errorMessage(e);
      emit({ ...event, error });
      await readCurrentFraming(dir, source.id, aspect, (latest) => publish(latest, error), () => {});
    } finally {
      mutations.delete(key);
      emit({ ...event, busy: null });
    }
  };
  return {
    busy, error: mutationError ?? previewError, shots, approvalInvalidReason,
    analyze: () => mutate("analyse"), approve: () => mutate("approve"), reload: () => mutate("read"),
  };
}
