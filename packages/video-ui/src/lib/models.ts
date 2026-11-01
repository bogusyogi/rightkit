// `genright.models` as the engine returns it (crates/cutright-engine/src/genright_cmd.rs `models`):
//   {models:[{provider, model, kind, label, est_usd, durations_s?, min_duration_s?, max_duration_s?,
//             default_duration_s?}], structured_durations}
// `est_usd` is the host app own per-job price estimate (null when unknown).

export type ModelKind = "image" | "video" | "voice" | "avatar";

/** Every the host app media kind a catalog row can carry (the eight generate kinds). */
export type CatalogKind = ModelKind | "music" | "sfx" | "upscale" | "lipsync";

export type ModelRow = {
  provider: string;
  model: string;
  kind: string;
  label: string;
  estUsd: number | null;
  /** A closed set of allowed durations (seconds). */
  durationsS?: number[];
  minDurationS?: number;
  maxDurationS?: number;
  defaultDurationS?: number;
};

export type ModelCatalog = { models: ModelRow[]; structuredDurations: boolean };

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
const str = (v: unknown) => (typeof v === "string" ? v : undefined);
const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : undefined);

export function parseCatalog(result: unknown): ModelCatalog {
  const r = isObj(result) ? result : {};
  const rows = Array.isArray(r.models) ? r.models : [];
  const models: ModelRow[] = [];
  for (const raw of rows) {
    if (!isObj(raw)) continue;
    const provider = str(raw.provider);
    const model = str(raw.model);
    if (!provider || !model) continue;
    const set = Array.isArray(raw.durations_s)
      ? raw.durations_s.map(num).filter((n): n is number => n !== undefined)
      : [];
    models.push({
      provider,
      model,
      kind: str(raw.kind) ?? "",
      label: str(raw.label) ?? model,
      estUsd: num(raw.est_usd) ?? null,
      durationsS: set.length > 0 ? [...set].sort((a, b) => a - b) : undefined,
      minDurationS: num(raw.min_duration_s),
      maxDurationS: num(raw.max_duration_s),
      defaultDurationS: num(raw.default_duration_s),
    });
  }
  return { models, structuredDurations: r.structured_durations === true };
}

export const ofKind = (c: ModelCatalog, kind: CatalogKind) => c.models.filter((m) => m.kind === kind);

export const providersOf = (c: ModelCatalog, kind: CatalogKind) => [...new Set(ofKind(c, kind).map((m) => m.provider))];

export const findModel = (c: ModelCatalog | null, provider: string, model: string) =>
  c?.models.find((m) => m.provider === provider && m.model === model);

export type DurationControl =
  | { type: "set"; options: number[]; initial: number }
  | { type: "range"; min: number; max: number; initial: number }
  | { type: "free"; initial: number };

/** What the duration control offers for `m`; the model's default is preselected. */
export function durationControl(m: ModelRow | undefined, fallback = 4): DurationControl {
  if (m?.durationsS) {
    const d = m.defaultDurationS;
    return { type: "set", options: m.durationsS, initial: d !== undefined && m.durationsS.includes(d) ? d : m.durationsS[0] };
  }
  if (m?.minDurationS !== undefined && m.maxDurationS !== undefined) {
    const d = m.defaultDurationS ?? m.minDurationS;
    return { type: "range", min: m.minDurationS, max: m.maxDurationS, initial: Math.min(m.maxDurationS, Math.max(m.minDurationS, d)) };
  }
  return { type: "free", initial: m?.defaultDurationS ?? fallback };
}

/** Preferred model of a kind: the wanted one when listed, else the cheapest priced, else the first. */
export function pickModel(c: ModelCatalog, kind: CatalogKind, want?: { provider: string; model: string }): ModelRow | undefined {
  const rows = ofKind(c, kind);
  const exact = want && rows.find((m) => m.provider === want.provider && m.model === want.model);
  if (exact) return exact;
  const priced = rows.filter((m) => m.estUsd !== null).sort((a, b) => (a.estUsd ?? 0) - (b.estUsd ?? 0));
  return priced[0] ?? rows[0];
}

export type SpendLine = { label: string; estUsd: number | null };

/** Sum of the known per-item estimates; `unpriced` counts items the host app has no estimate for. */
export function spendTotal(lines: SpendLine[]): { total: number; unpriced: number } {
  let total = 0;
  let unpriced = 0;
  for (const l of lines) {
    if (l.estUsd === null) unpriced += 1;
    else total += l.estUsd;
  }
  return { total, unpriced };
}

export const formatUsd = (n: number) => `$${n.toFixed(n < 1 ? 3 : 2)}`;
