import { useEffect, useState, type ReactNode } from 'react';
import { errorCode, errorDetails, errorText, fetchModelPriceFacts, fetchRoutePrefs, routeMedia, saveRoutePrefs, type MediaKind, type ModelPriceFacts, type RouteAlternative, type RoutePreferences, type RoutePrivacy, type RouteResult, type RouteSpend } from '../lib/api.js';
import { ASSUMED_ROUNDING_SENTENCE, UNCONFIRMED_PRICE_LABEL, UNCONFIRMED_PRICE_SENTENCE, isAssumedRounding, isUnconfirmedPrice } from '../lib/gen-params.js';
import type { SettingsSectionId, ViewId } from '../lib/nav.js';
import { Badge, Button, Notice, Select } from '@rightkit/react-shell/react';
import {
  PRIVACY_CHOICES,
  SPEND_CHOICES,
  estimateLabel,
  fixHints,
  isChoosable,
  preferenceNote,
  routeFromError,
  routeFromResponse,
  routeId,
  routeInput,
  settingsSectionFor,
  stateLabels,
  type RouteLoad,
  type RouteView
} from './route-panel-logic.js';

export interface RoutePanelProps {
  kind: MediaKind;
  /** The provider and model the user picked in the composer, or null when they picked none. */
  pick: { provider: string; model: string } | null;
  /** The ids of the model picker's options, so a route is pickable only when the picker can show it. */
  optionIds: readonly string[];
  /** Sets the composer's provider and model. It never submits. */
  onChoose: (id: string) => void;
  /** Opens a view; a fix button passes the Settings section that clears its blocker. */
  onNavigate: (view: ViewId, section?: SettingsSectionId) => void;
}

/**
 * "Why this route" for the composer's kind. Collapsed by the composer until opened, so it calls the engine
 * only on demand. It reads routes and saves preferences; it never selects a paid route and never submits.
 */
export function RoutePanel({ kind, pick, optionIds, onChoose, onNavigate }: RoutePanelProps) {
  const pickProvider = pick?.provider ?? null;
  const pickModel = pick?.model ?? null;
  const [load, setLoad] = useState<RouteLoad | null>(null);
  const [busy, setBusy] = useState(true);
  const [version, setVersion] = useState(0);
  const [prefs, setPrefs] = useState<RoutePreferences | null>(null);
  const [prefsError, setPrefsError] = useState<string | null>(null);
  // Price facts from media modalities: the route answer does not carry them. A failed read shows no caveat.
  const [priceFacts, setPriceFacts] = useState<Record<string, ModelPriceFacts>>({});

  useEffect(() => {
    let live = true;
    fetchModelPriceFacts()
      .then((facts) => {
        if (live) setPriceFacts(facts);
      })
      .catch(() => {
        if (live) setPriceFacts({});
      });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    let live = true;
    setBusy(true);
    const input = routeInput(kind, pickProvider !== null && pickModel !== null ? { provider: pickProvider, model: pickModel } : null);
    routeMedia(input)
      .then((raw) => {
        if (live) setLoad(routeFromResponse(raw));
      })
      .catch((error: unknown) => {
        if (live) setLoad(routeFromError(errorCode(error), errorDetails(error), errorText(error)));
      })
      .finally(() => {
        if (live) setBusy(false);
      });
    return () => {
      live = false;
    };
  }, [kind, pickProvider, pickModel, version]);

  useEffect(() => {
    let live = true;
    fetchRoutePrefs()
      .then((saved) => {
        if (live) setPrefs(saved.preferences);
      })
      .catch((error: unknown) => {
        if (live) setPrefsError(errorText(error));
      });
    return () => {
      live = false;
    };
  }, []);

  async function savePref(patch: Partial<Pick<RoutePreferences, 'privacy' | 'spend'>>) {
    setPrefsError(null);
    try {
      const saved = await saveRoutePrefs(patch);
      setPrefs(saved.preferences);
      setVersion((v) => v + 1);
    } catch (error) {
      setPrefsError(errorText(error));
    }
  }

  const view: RouteView | null = load?.ok ? load.view : null;
  const result: RouteResult | null = view?.result ?? null;
  const loadError = load && !load.ok ? load.message : null;
  const alternatives = result?.alternatives ?? [];
  const verdict = view && result ? verdictOf(view, result) : null;

  return (
    <section aria-label="Why this route" className="route-panel">
      <div className="route-panel__head">
        <span className="route-panel__outcome">
          {verdict ? verdict.headline : loadError ? 'Routes could not be checked' : 'Checking routes…'}
        </span>
        {verdict?.estimate ? <span className="route-panel__est">{verdict.estimate}</span> : null}
        <span className="eyebrow route-panel__kind">Why this route · {kind}</span>
      </div>
      {verdict ? <p className="route-panel__explain">{verdict.detail}</p> : null}
      {verdict?.extra ? <Notice tone="info" className="route-panel__note">{verdict.extra}</Notice> : null}
      {loadError ? <Notice tone="bad" className="route-panel__err">{loadError}</Notice> : null}
      {alternatives.length ? (
        <div className="route-panel__list">
          {alternatives.map((alt) => (
            <AlternativeRow
              key={routeId(alt)}
              alt={alt}
              choosable={view ? isChoosable(view, alt) : false}
              pickable={optionIds.includes(routeId(alt))}
              facts={priceFacts[routeId(alt)] ?? null}
              onChoose={onChoose}
              onNavigate={onNavigate}
            />
          ))}
        </div>
      ) : null}
      <PreferenceBlock prefs={prefs} error={prefsError} note={result ? preferenceNote(result) : null} onSave={savePref} />
    </section>
  );
}

interface Verdict {
  headline: ReactNode;
  /** The estimate of the chosen route, shown at the header's right. */
  estimate: string | null;
  /** One short explanatory line. */
  detail: string;
  /** The engine's own note, when it adds one. */
  extra: string | null;
}

function verdictOf(view: RouteView, result: RouteResult): Verdict {
  if (view.status === 'chosen' && result.chosen) {
    const chosen = result.chosen;
    return {
      headline: (
        <>
          {result.basis === 'request' ? 'Your pick is sent as named' : 'Routing would pick'}: {chosen.provider} · <span className="mono">{chosen.model}</span>
        </>
      ),
      estimate: estimateLabel(chosen),
      detail: chosen.reason,
      extra: result.selection_note ?? null
    };
  }
  if (view.status === 'choice') {
    return {
      headline: 'A paid route needs your choice',
      estimate: null,
      detail: 'No free route is usable. Pick one below to fill the model picker; nothing is sent until you press Generate.',
      extra: result.note ?? null
    };
  }
  if (view.status === 'ambiguous') {
    return { headline: 'More than one provider offers this model', estimate: null, detail: result.note ?? '', extra: null };
  }
  return { headline: `No usable route for ${result.kind}`, estimate: null, detail: result.note ?? '', extra: null };
}

function AlternativeRow({
  alt,
  choosable,
  pickable,
  facts,
  onChoose,
  onNavigate
}: {
  alt: RouteAlternative;
  choosable: boolean;
  pickable: boolean;
  facts: ModelPriceFacts | null;
  onChoose: (id: string) => void;
  onNavigate: (view: ViewId, section?: SettingsSectionId) => void;
}) {
  const id = routeId(alt);
  const fixes = fixHints(alt);
  return (
    <div className="route-row">
      <div className="route-row__model">
        {alt.provider} · <span className="mono">{alt.model}</span>
      </div>
      <div className="route-row__est">
        {estimateLabel(alt)}
        {isAssumedRounding(facts) ? ` · ${ASSUMED_ROUNDING_SENTENCE}` : ''}
      </div>
      {isUnconfirmedPrice(facts) ? (
        <Notice tone="info" className="route-panel__note">
          <Badge>{UNCONFIRMED_PRICE_LABEL}</Badge> {UNCONFIRMED_PRICE_SENTENCE}
        </Notice>
      ) : null}
      <div className="route-row__chips" aria-label="Route states">
        {stateLabels(alt.states).map((state) => (
          <Badge key={state.key} tone={state.tone}>
            {state.label}: {state.value}
          </Badge>
        ))}
      </div>
      <div className="route-row__actions">
        {choosable ? (
          <Button type="button" variant="primary" size="sm" disabled={!pickable} title={pickable ? undefined : 'This route is not in the model picker'} onClick={() => onChoose(id)}>
            Use {alt.model}
          </Button>
        ) : null}
        {fixes.map((fix) => (
          <Button key={fix.section} type="button" variant="ghost" onClick={() => onNavigate('settings', settingsSectionFor(fix.section))}>
            {fix.label}
          </Button>
        ))}
      </div>
      <div className="route-row__reason">{alt.reason}</div>
    </div>
  );
}

function PreferenceBlock({
  prefs,
  error,
  note,
  onSave
}: {
  prefs: RoutePreferences | null;
  error: string | null;
  note: string | null;
  onSave: (patch: Partial<Pick<RoutePreferences, 'privacy' | 'spend'>>) => void;
}) {
  return (
    <div className="route-prefs">
      <div className="route-prefs__row">
        <span className="eyebrow">Routing preferences</span>
        <label>
          Privacy{' '}
          <Select label="Routing privacy" disabled={!prefs} value={prefs?.privacy ?? 'any'} onChange={(v) => onSave({ privacy: v as RoutePrivacy })} options={PRIVACY_CHOICES.map((c) => ({ value: c.value, label: c.label }))} />
        </label>
        <label>
          Spend{' '}
          <Select label="Routing spend" disabled={!prefs} value={prefs?.spend ?? 'ask'} onChange={(v) => onSave({ spend: v as RouteSpend })} options={SPEND_CHOICES.map((c) => ({ value: c.value, label: c.label }))} />
        </label>
      </div>
      {error ? <Notice tone="bad" className="route-panel__err">{error}</Notice> : null}
      {note ? <Notice tone="info" className="route-panel__note">{note}</Notice> : null}
      <Notice tone="info" className="route-panel__note">These apply only to requests that name no provider or model. This composer always names its own, so it sends the route you picked.</Notice>
    </div>
  );
}
