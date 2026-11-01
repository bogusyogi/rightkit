import type { MediaKind, ModelEntry } from './api';

/** What a caller needs from the engine's model list: a kind, and optionally a preferred provider and model id. */
export interface ModelRequest {
  kind: MediaKind;
  provider?: string;
  model?: string;
}

/**
 * Chooses one model from the engine's `models` list (held in app state). The exact provider/model is used
 * when the engine still lists it with the requested kind. Otherwise the first model of that kind is used,
 * preferring the requested provider. Null means the engine lists no model of this kind.
 */
export function pickModel(models: readonly ModelEntry[], request: ModelRequest): ModelEntry | null {
  const ofKind = models.filter((m) => m.kind === request.kind);
  if (!ofKind.length) return null;
  const exact = request.model
    ? ofKind.find((m) => m.model === request.model && (!request.provider || m.provider === request.provider))
    : undefined;
  if (exact) return exact;
  const sameProvider = request.provider ? ofKind.find((m) => m.provider === request.provider) : undefined;
  return sameProvider ?? ofKind[0];
}

/**
 * First model of a kind in the engine's list order whose provider is not `excludeProvider`: the route a
 * composer offers when one provider refuses a job. Only models the engine lists are ever returned; null
 * means the engine lists no other route for this kind.
 */
export function pickOtherProviderModel(
  models: readonly ModelEntry[],
  request: { kind: MediaKind; excludeProvider: string }
): ModelEntry | null {
  return models.find((m) => m.kind === request.kind && m.provider !== request.excludeProvider) ?? null;
}

/** Empty-state line for a kind the engine lists no model for. */
export const noModelMessage = (kind: MediaKind): string => `No ${kind} model available`;

/**
 * Price line for a cloud model. The engine's `est_usd` is its estimate for one default-sized output, so it is
 * always shown as `est.` with the context it was made for: the default duration when the model has one, else
 * the unit it is priced per. The engine marks no figure as a starting price, so none is labelled "from".
 * The engine reports 0 when it has no price for a model, which is shown as unknown rather than free.
 */
export function priceCaption(entry: Pick<ModelEntry, 'est_usd' | 'default_duration_s'>, unit: string): string {
  if (!Number.isFinite(entry.est_usd) || entry.est_usd <= 0) return 'price unknown';
  const amount = `est. ${money(entry.est_usd)}`;
  return entry.default_duration_s != null ? `${amount} · ${entry.default_duration_s} s` : `${amount} / ${unit}`;
}

function money(value: number): string {
  return `$${value.toFixed(value < 0.1 ? 3 : 2)}`;
}
