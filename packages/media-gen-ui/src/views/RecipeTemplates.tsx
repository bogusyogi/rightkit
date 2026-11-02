import { useEffect, useState } from 'react';
import {
  approveRecipeTemplateDraft,
  errorCode,
  recipeTemplateCompose,
  recipeTemplatePreflight,
  recipeTemplatesList,
  type RecipeComposeOk,
  type RecipeComposeResult,
  type RecipePreflightResult,
  type RecipeTemplate,
  type RecipeTemplatesResult
} from '../lib/api.js';
import { Card, Badge, Button, EmptyState, Input, Notice, Select, Toggle } from '@rightkit/react-shell/react';
import { Kv, usd } from '../components/ui.js';
import { EngineError, toError } from '../lib/engine-error.js';
import { RecipeCharacterBatch } from './RecipeCharacterBatch.js';
import {
  approvalFailureText,
  buildComposeInput,
  checkSlotFill,
  moderationRows,
  refusalView,
  routeKey,
  stagePlanText,
  stagedSteps,
  unservedRouteNames,
  violationsBySlot,
  withStaged
} from './recipe-templates-logic.js';
import { hostAppName } from "../appName.js";

const APPROVE_LABEL: Record<string, string> = { single: 'Approve request', draft: 'Approve draft', final: 'Approve final' };

/** The engine's message without its code: codes are not shown as copy. */
function errorText(error: unknown): string {
  const e = toError(error);
  return e instanceof EngineError ? e.detail : e.message;
}

/** The template list, each template's card, and the character batch. Nothing here submits a paid generation. */
export function RecipeTemplatesArea() {
  const [data, setData] = useState<RecipeTemplatesResult | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    recipeTemplatesList()
      .then((r) => live && setData(r))
      .catch((e) => live && setLoadError(errorText(e)));
    return () => {
      live = false;
    };
  }, []);

  const batchSpec = data?.batches.character_reference_batch;

  return (
    <div style={{ marginTop: 22 }}>
      <div className="eyebrow" style={{ marginBottom: 8 }}>Templates</div>
      <p className="lede" style={{ fontSize: 12.5 }}>
        Fill each reference slot in order with a file on this computer or a web (https) address. The Show exact request
        button displays the request {hostAppName()} would send. Approving a draft goes no further: nothing is generated from
        this view, and approved drafts are not yet passed to the composer.
      </p>
      {loadError ? <Notice tone="bad">{loadError}</Notice> : null}
      {!data && !loadError ? <EmptyState title="Loading templates…" /> : null}
      {data
        ? data.templates.map((t) => <TemplateCard key={t.template} template={t} />)
        : null}
      {data && batchSpec ? <RecipeCharacterBatch maxItems={batchSpec.max_items} /> : null}
    </div>
  );
}

type Busy = 'compose' | 'preflight' | string | null;

function TemplateCard({ template }: { template: RecipeTemplate }) {
  const [values, setValues] = useState<string[]>(() => template.slots.map(() => ''));
  const [routeChoice, setRouteChoice] = useState('');
  const [staged, setStaged] = useState(false);
  const [resolution, setResolution] = useState('');
  const [attempted, setAttempted] = useState(false);
  const [compose, setCompose] = useState<RecipeComposeResult | null>(null);
  const [preflight, setPreflight] = useState<RecipePreflightResult | null>(null);
  const [approved, setApproved] = useState<string[]>([]);
  const [approveErrors, setApproveErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);

  const served = template.served;
  const moderation = moderationRows(template.moderation);
  const servedRoutes = template.routes.filter((r) => r.served);
  const chosenRoute = servedRoutes.find((r) => routeKey(r) === routeChoice) ?? null;
  const check = checkSlotFill(template.slots, values);
  const refusal = compose && compose.ok === false ? compose : preflight && preflight.ok === false ? preflight : null;
  const engineSlots = refusal ? violationsBySlot(refusal.violations, template.slots) : { bySlot: {}, general: [] as string[] };
  const bySlot: Record<number, string[]> = engineSlots.bySlot;
  const canRun = served && check.ready && busy === null;

  const clearResults = () => {
    setCompose(null);
    setPreflight(null);
    setApproved([]);
    setApproveErrors({});
    setError(null);
  };

  const setValue = (index: number, value: string) => {
    setValues((cur) => cur.map((v, i) => (i === index ? value : v)));
    clearResults();
  };

  const runCompose = async () => {
    setAttempted(true);
    setBusy('compose');
    clearResults();
    try {
      setCompose(await recipeTemplateCompose(buildComposeInput(template, values, { route: chosenRoute, staged, resolution })));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  const runPreflight = async () => {
    setAttempted(true);
    setBusy('preflight');
    clearResults();
    try {
      const input = withStaged(buildComposeInput(template, values, { route: chosenRoute, staged, resolution }), staged);
      setPreflight(await recipeTemplatePreflight(input));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  const approve = async (draftId: string, requestHash: string) => {
    setBusy(`approve:${draftId}`);
    try {
      await approveRecipeTemplateDraft(draftId, requestHash);
      setApproved((cur) => (cur.includes(draftId) ? cur : [...cur, draftId]));
      setApproveErrors((cur) => {
        const next = { ...cur };
        delete next[draftId];
        return next;
      });
    } catch (e) {
      setApproveErrors((cur) => ({ ...cur, [draftId]: approvalFailureText(errorCode(e), errorText(e)) }));
    } finally {
      setBusy(null);
    }
  };

  const slotProblems = (position: number): string[] => {
    const texts = [...(bySlot[position] ?? [])];
    for (const p of check.problems) {
      if (p.position !== position) continue;
      if (p.code === 'slot_missing' && !attempted) continue;
      texts.push(p.text);
    }
    return texts;
  };

  return (
    <Card flush className="group-box gb-mt14">
      <div className="setting" style={{ borderTop: 0 }}>
        <b style={{ fontSize: 12.5 }}>{template.title}</b>
        {served ? <Badge tone="ok">{`${servedRoutes.length} route${servedRoutes.length === 1 ? '' : 's'} available`}</Badge> : <Badge tone="warn">unavailable</Badge>}
      </div>

      {!served ? (
        <div style={{ padding: '0 12px 8px', fontSize: 12, color: 'var(--ink-2)' }}>
          Unavailable: none of its provider and model routes is in the app's catalogue ({unservedRouteNames(template).join(', ')}).
          The template is shown, not hidden, and nothing is composed or guessed.
        </div>
      ) : null}

      <div style={{ padding: '0 12px 8px' }}>
        <div style={{ fontSize: 11.5, color: 'var(--ink-3)', marginBottom: 4 }}>Content notes: read before you submit</div>
        <Notice tone="info">
          {moderation.map((row, i) => (
            <div key={i} style={{ marginBottom: 6 }} title={row.source ?? undefined}>
              <Badge tone={row.generic ? 'warn' : undefined}>{row.label}</Badge>{' '}
              {row.text}
            </div>
          ))}
          {moderation.some((row) => row.source) ? (
            <details style={{ marginTop: 4 }}>
              <summary style={{ fontSize: 11.5, color: 'var(--ink-3)', cursor: 'pointer' }}>Details</summary>
              {moderation.filter((row) => row.source).map((row, i) => (
                <div key={i} className="mono" style={{ fontSize: 10.5, color: 'var(--ink-3)', marginTop: 2 }}>
                  Source: {row.source}
                </div>
              ))}
            </details>
          ) : null}
        </Notice>
      </div>

      <div style={{ borderTop: '1px solid var(--line, rgba(0,0,0,0.08))' }}>
        {template.slots.map((slot, i) => {
          const problems = slotProblems(slot.position);
          return (
            <div key={slot.position} style={{ padding: '8px 12px', display: 'grid', gap: 4 }}>
              <div style={{ display: 'flex', gap: 6, alignItems: 'baseline', flexWrap: 'wrap' }}>
                <span className="mono" style={{ fontSize: 11 }}>{slot.position}</span>
                <b style={{ fontSize: 12.5 }}>{slot.name}</b>
                <Badge>{slot.type}</Badge>
                <Badge>{slot.required ? 'required' : 'optional'}</Badge>
              </div>
              <div style={{ fontSize: 11.5, color: 'var(--ink-3)' }}>{slot.role}</div>
              <Input
                aria-label={`Slot ${slot.position} ${slot.name} path or URL`}
                placeholder={slot.type === 'video' ? 'A video file, or an https:// link' : 'An image file, or an https:// link'}
                value={values[i]}
                disabled={!served || busy !== null}
                onChange={(e) => setValue(i, e.target.value)}
                spellCheck={false}
              />
              {problems.map((text, j) => (
                <Notice key={j} tone="bad">{text}</Notice>
              ))}
            </div>
          );
        })}
        {engineSlots.general.map((text) => (
          <div key={text} style={{ margin: '0 12px 8px' }}><Notice tone="bad">{text}</Notice></div>
        ))}
      </div>

      <div style={{ padding: '8px 12px 12px', display: 'flex', flexWrap: 'wrap', gap: 10, alignItems: 'center' }}>
        <div>
          <label style={{ fontSize: 11.5 }}>Route</label>
          <Select
            label="Route"
            value={routeChoice}
            disabled={!served || busy !== null}
            onChange={(v) => {
              setRouteChoice(v);
              clearResults();
            }}
            options={[{ value: '', label: 'First available route (default)' }, ...servedRoutes.map((r) => ({ value: routeKey(r), label: routeKey(r) }))]}
          />
        </div>
        {template.staged_resolution && served ? (
          <label style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12 }}>
            <Toggle checked={staged} label="Draft first" onChange={(v) => { setStaged(v); clearResults(); }} />
            Draft first: a draft at the lowest resolution, then the final at the resolution set below
          </label>
        ) : null}
        {staged && served ? (
          <div>
            <label htmlFor={`rt-res-${template.template}`} style={{ fontSize: 11.5 }} title="Sent as the resolution setting">Final resolution</label>
            <Input
              id={`rt-res-${template.template}`}
              value={resolution}
              placeholder="e.g. 1080p"
              onChange={(e) => {
                setResolution(e.target.value);
                clearResults();
              }}
            />
          </div>
        ) : null}
      </div>

      <div style={{ padding: '0 12px 12px', display: 'flex', flexWrap: 'wrap', gap: 8, alignItems: 'center' }}>
        <Button type="button" variant="primary" disabled={!canRun} onClick={runCompose}>
          {busy === 'compose' ? 'Building request…' : 'Show exact request'}
        </Button>
        <Button type="button" variant="ghost" disabled={!canRun} onClick={runPreflight}>
          {busy === 'preflight' ? 'Preparing…' : staged ? 'Prepare drafts: draft, then final' : 'Prepare draft for approval'}
        </Button>
        {!check.ready && served ? <span className="foot-note">Fill every required slot, in order, before continuing.</span> : null}
      </div>

      {error ? <div style={{ margin: '0 12px 12px' }}><Notice tone="bad">{error}</Notice></div> : null}

      {compose && compose.ok === true ? <ComposeView result={compose} showStage={staged} /> : null}

      {refusal ? (
        <div style={{ padding: '0 12px 12px' }}>
          <Notice tone="bad">{refusalView(refusal).headline}</Notice>
          {refusalView(refusal).details.map((d) => (
            <p key={d} className="foot-note" style={{ margin: '4px 0 0' }}>{d}</p>
          ))}
        </div>
      ) : null}

      {preflight && preflight.ok === true ? (
        <div style={{ padding: '4px 12px 12px' }}>
          <div style={{ fontSize: 11.5, color: 'var(--ink-3)', marginBottom: 6 }}>{stagePlanText(preflight.stage_plan)}</div>
          {preflight.note ? <div className="foot-note">{preflight.note}</div> : null}
          {stagedSteps(preflight.drafts, approved).map((step) => {
            const draft = preflight.drafts.find((d) => d.draft_id === step.draft_id);
            if (!draft) return null;
            const id = step.draft_id;
            return (
              <div key={id} style={{ marginBottom: 8, fontSize: 12 }}><Notice tone="info">
                <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginBottom: 4 }}>
                  <b style={{ fontSize: 12.5 }}>{step.label}</b>
                  <Badge tone={step.approved ? 'ok' : undefined}>{step.approved ? 'approved' : 'awaiting approval'}</Badge>
                  <span>{usd(draft.est_usd, 2)}</span>
                  <span className="mono" title={draft.request_hash} style={{ fontSize: 10.5, color: 'var(--ink-3)' }}>
                    fingerprint {draft.request_hash.slice(0, 12)}
                  </span>
                </div>
                <div style={{ whiteSpace: 'pre-wrap', marginBottom: 4 }}>{draft.request.prompt}</div>
                <div style={{ fontSize: 10.5, color: 'var(--ink-2)', marginBottom: 6 }}>
                  {draft.request.provider} · {draft.request.model}
                </div>
                <details style={{ marginBottom: 6 }}>
                  <summary style={{ fontSize: 10.5, color: 'var(--ink-3)', cursor: 'pointer' }}>Details</summary>
                  <div className="mono" style={{ fontSize: 10.5, color: 'var(--ink-2)', marginTop: 2 }}>
                    Settings sent: {JSON.stringify(draft.request.params)}
                  </div>
                </details>
                <Button
                  type="button"
                  variant={step.approved ? 'ghost' : 'primary'}
                  disabled={!step.canApprove || busy !== null}
                  onClick={() => approve(id, draft.request_hash)}
                >
                  {step.approved ? 'Approved' : APPROVE_LABEL[draft.stage] ?? 'Approve'}
                </Button>
                {step.blockedBy ? <div className="foot-note" style={{ marginTop: 4 }}>{step.blockedBy}</div> : null}
                {approveErrors[id] ? <div style={{ marginTop: 4 }}><Notice tone="bad">{approveErrors[id]}</Notice></div> : null}
              </Notice></div>
            );
          })}
          <div className="foot-note">Approving goes no further. Nothing is submitted to a provider from here.</div>
        </div>
      ) : null}
    </Card>
  );
}

function ComposeView({ result, showStage }: { result: RecipeComposeOk; showStage: boolean }) {
  return (
    <div style={{ padding: '0 12px 12px' }}>
      <Kv
        rows={[
          ['Provider', result.request.provider],
          ['Model', <span className="mono" key="model">{result.request.model}</span>],
          ['Estimated cost (nothing is sent)', usd(result.est_usd, 2)],
        ]}
      />
      <div style={{ whiteSpace: 'pre-wrap', marginTop: 8, fontSize: 12 }}><Notice tone="info">{result.request.prompt}</Notice></div>
      {showStage ? <div className="foot-note" style={{ marginTop: 4 }}>{stagePlanText(result.staged)}</div> : null}
      {result.warnings.map((w) => (
        <div key={w} className="foot-note" style={{ marginTop: 4 }}>{w}</div>
      ))}
      <div className="foot-note" style={{ marginTop: 4 }}>Test run only: nothing was sent to a provider.</div>
      <details style={{ marginTop: 6 }}>
        <summary className="foot-note" style={{ cursor: 'pointer' }}>Details</summary>
        <div className="mono" style={{ fontSize: 10.5, color: 'var(--ink-2)', marginTop: 4 }}>
          Settings sent: {JSON.stringify(result.request.params)}
        </div>
        {result.slots.map((row) => (
          <div key={row.position} className="mono" style={{ fontSize: 10.5, color: 'var(--ink-2)', marginTop: 2 }}>
            Slot {row.position} {row.name} is sent as {row.param}
          </div>
        ))}
      </details>
    </div>
  );
}
