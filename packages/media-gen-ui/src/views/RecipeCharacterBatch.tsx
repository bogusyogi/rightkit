import { useEffect, useReducer, useRef, useState } from 'react';
import {
  approveRecipeTemplateDraft,
  errorCode,
  errorText,
  recipeBatchReview,
  recipeCharacterBatch,
  recipeDraftHash,
  type RecipeBatchItem
} from '../lib/api.js';
import { Card, Badge, Button, Input, Notice, Select } from '@rightkit/react-shell/react';
import { Kv, usd } from '../components/ui.js';
import { characters, type CharacterSummary } from './panels/character-lock.js';
import {
  approvalFailureText,
  batchRefusalText,
  countStates,
  draftIdsNeedingHash,
  INITIAL_REVIEW_STATE,
  itemActionsFor,
  reviewReducer,
  scenarioLines,
  scenarioProblem
} from './recipe-templates-logic.js';

import { logs } from '../lib/logs.js';
import { readUiSetting, writeUiSetting } from '../lib/settings-backend.js';

const BATCH_KEY = 'genright.recipes.characterBatch';

function saveBatch(id: string | null): void {
  writeUiSetting(BATCH_KEY, id).catch((e) => logs.error('ui_setting_write_failed', { key: BATCH_KEY, message: errorText(e) }));
}

const STATE_TONE: Record<RecipeBatchItem['state'], 'ok' | 'bad' | 'warn' | undefined> = {
  approved: 'ok',
  rejected: 'bad',
  refused: 'warn',
  pending: undefined
};

/** One character, one scenario line per item, one shared lock. Approval per item; rejection is final. */
export function RecipeCharacterBatch({ maxItems }: { maxItems: number }) {
  const [list, setList] = useState<CharacterSummary[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [characterId, setCharacterId] = useState('');
  const [provider, setProvider] = useState('');
  const [model, setModel] = useState('');
  const [scenarioText, setScenarioText] = useState('');
  const [referenceText, setReferenceText] = useState('');
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [state, dispatch] = useReducer(reviewReducer, INITIAL_REVIEW_STATE);
  const hashRequests = useRef(new Set<string>());

  useEffect(() => {
    let live = true;
    characters
      .list()
      .then((r) => {
        if (!live) return;
        setList(r.characters);
        setCharacterId((cur) => cur || r.characters[0]?.id || '');
      })
      .catch((e) => live && setListError(errorText(e)));
    // The last batch id, so the review can be read back from the engine after a reload.
    readUiSetting<string | null>(BATCH_KEY, null).catch((e) => {
      if (live) setListError(`Could not read saved batch: ${errorText(e)}`);
      return null;
    }).then((saved) => {
      if (!live || !saved) return;
      recipeBatchReview(saved)
        .then((v) => dispatch({ type: 'view', view: v }))
        .catch((e) => {
          if (errorCode(e) === 'batch_not_found') saveBatch(null);
          else dispatch({ type: 'loadError', message: `Could not read the saved batch: ${errorText(e)}` });
        });
    });
    return () => {
      live = false;
    };
  }, []);

  const view = state.view;

  // The approve button needs each pending item's request hash as the user sees it. A creation response
  // carries it; after a reload it comes from a fresh challenge. Requests are not repeated while in flight.
  useEffect(() => {
    for (const draftId of draftIdsNeedingHash(view, state.hashes)) {
      if (hashRequests.current.has(draftId)) continue;
      hashRequests.current.add(draftId);
      recipeDraftHash(draftId)
        .then((hash) => {
          if (hash) dispatch({ type: 'hash', draftId, hash });
        })
        .catch((e) => dispatch({ type: 'loadError', message: `Could not read a request hash: ${errorText(e)}` }))
        .finally(() => hashRequests.current.delete(draftId));
    }
  }, [view, state.hashes]);

  const scenarios = scenarioLines(scenarioText);
  const problem = scenarioProblem(scenarios, maxItems);
  const canCreate = !creating && characterId !== '' && provider.trim() !== '' && model.trim() !== '' && problem === null;

  const create = async () => {
    setCreating(true);
    setCreateError(null);
    try {
      const references = scenarioLines(referenceText);
      const created = await recipeCharacterBatch({
        character_id: characterId,
        provider: provider.trim(),
        model: model.trim(),
        scenarios,
        ...(references.length > 0 ? { references } : {})
      });
      saveBatch(created.batch_id);
      dispatch({ type: 'view', view: created });
      for (const row of created.items) {
        const hash = row.approval_challenge?.legacy_hash;
        if (row.draft_id && hash) dispatch({ type: 'hash', draftId: row.draft_id, hash });
      }
    } catch (e) {
      setCreateError(errorText(e));
    } finally {
      setCreating(false);
    }
  };

  const approve = async (item: RecipeBatchItem) => {
    if (!view || !item.draft_id) return;
    const hash = state.hashes[item.draft_id];
    if (!hash) return;
    dispatch({ type: 'busy', index: item.index });
    try {
      await approveRecipeTemplateDraft(item.draft_id, hash);
      dispatch({ type: 'view', view: await recipeBatchReview(view.batch_id) });
    } catch (e) {
      dispatch({ type: 'itemError', index: item.index, message: approvalFailureText(errorCode(e), errorText(e)) });
    }
  };

  const reject = async (item: RecipeBatchItem) => {
    if (!view) return;
    dispatch({ type: 'busy', index: item.index });
    try {
      dispatch({ type: 'view', view: await recipeBatchReview(view.batch_id, [{ item: item.index, decision: 'reject' }]) });
    } catch (e) {
      dispatch({ type: 'itemError', index: item.index, message: approvalFailureText(errorCode(e), errorText(e)) });
    }
  };

  const forget = () => {
    saveBatch(null);
    dispatch({ type: 'reset' });
  };

  const counts = view ? countStates(view.items) : null;

  return (
    <Card flush className="group-box gb-mt14">
      <div className="setting" style={{ borderTop: 0 }}>
        <b style={{ fontSize: 12.5 }}>Character reference batch</b>
        <Badge>{`up to ${maxItems} lines`}</Badge>
      </div>
      <div style={{ padding: '0 12px 12px', display: 'grid', gap: 8 }}>
        {listError ? <Notice tone="bad">{listError}</Notice> : null}
        {list && list.length === 0 ? <div className="foot-note">No characters yet. Create one first, then come back here.</div> : null}
        <div className="fgrid">
          <div>
            <label>Character</label>
            <Select label="Character" value={characterId} onChange={setCharacterId} disabled={!list || list.length === 0} options={(list ?? []).map((c) => ({ value: c.id, label: c.character_id || c.id }))} />
          </div>
          <div>
            <label htmlFor="rt-batch-provider">Provider</label>
            <Input id="rt-batch-provider" value={provider} placeholder="e.g. fal" onChange={(e) => setProvider(e.target.value)} />
          </div>
          <div>
            <label htmlFor="rt-batch-model">Model</label>
            <Input id="rt-batch-model" value={model} placeholder="model id on that provider" onChange={(e) => setModel(e.target.value)} />
          </div>
        </div>
        <div>
          <label htmlFor="rt-batch-scenarios">Scenario lines, one per line</label>
          <textarea className="rk-input"
            id="rt-batch-scenarios"
            aria-label="Scenario lines"
            value={scenarioText}
            placeholder={'a ground-level shot in a kitchen\na close-up at a cafe table'}
            onChange={(e) => setScenarioText(e.target.value)}
          />
          <div className="foot-note">
            {scenarios.length} of {maxItems} lines.{' '}
            {scenarioText.trim() !== '' && problem ? <span style={{ color: "var(--bad)" }}>{problem}</span> : null}
          </div>
        </div>
        <div>
          <label htmlFor="rt-batch-refs">Reference images, https URLs, one per line (optional)</label>
          <textarea className="rk-input"
            id="rt-batch-refs"
            aria-label="Reference image URLs"
            value={referenceText}
            placeholder="https://…/face.png"
            onChange={(e) => setReferenceText(e.target.value)}
          />
          <div className="foot-note">Local files are not uploaded by the batch. Without https references an item cannot be approved.</div>
        </div>
        <div>
          <Button type="button" variant="primary" disabled={!canCreate} onClick={create}>
            {creating ? 'Creating…' : 'Create batch review'}
          </Button>
        </div>
        {createError ? <Notice tone="bad">{createError}</Notice> : null}
      </div>

      {state.loadError ? <div style={{ margin: '0 12px 12px' }}><Notice tone="bad">{state.loadError}</Notice></div> : null}

      {view && counts ? (
        <div style={{ padding: '0 12px 12px', display: 'grid', gap: 8 }}>
          <Kv
            rows={[
              ['Batch', <span className="mono" key="batch">{view.batch_id}</span>],
              ['Items', `${counts.items}: ${counts.pending} pending, ${counts.approved} approved, ${counts.rejected} rejected, ${counts.refused} refused`]
            ]}
          />
          <div className="foot-note">
            Contact sheet images are not built yet: this review is a list of the batch items, with no composited image.
          </div>
          {view.items.map((item) => {
            const hash = item.draft_id ? state.hashes[item.draft_id] : undefined;
            const actions = itemActionsFor(item, Boolean(hash));
            const busy = state.busyIndex !== null;
            return (
              <div key={item.index} style={{ fontSize: 12 }}><Notice tone="info">
                <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginBottom: 4 }}>
                  <span className="mono" style={{ fontSize: 11 }}>#{item.index}</span>
                  <b style={{ fontSize: 12.5 }}>{item.scenario}</b>
                  <Badge tone={STATE_TONE[item.state]}>{item.state}</Badge>
                  {item.state !== 'refused' ? (
                    <Badge tone={item.identity_attached ? 'ok' : 'warn'}>{item.identity_attached ? 'identity attached' : 'no identity attached'}</Badge>
                  ) : null}
                </div>
                {item.refusal ? <div style={{ marginBottom: 4 }}><Notice tone="bad">{batchRefusalText(item.refusal)}</Notice></div> : null}
                {item.prompt ? <div style={{ whiteSpace: 'pre-wrap', marginBottom: 4 }}>{item.prompt}</div> : null}
                {item.state !== 'refused' ? (
                  <div className="foot-note" style={{ marginBottom: 6 }}>
                    Estimate {usd(item.est_usd ?? null, 2)}
                    {hash ? <span className="mono" title={hash}> · hash {hash.slice(0, 12)}</span> : null}
                  </div>
                ) : null}
                <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                  <Button type="button" variant="primary" disabled={!actions.approve || busy} onClick={() => approve(item)}>
                    {state.busyIndex === item.index ? 'Working…' : 'Approve'}
                  </Button>
                  <Button type="button" variant="ghost" disabled={!actions.reject || busy} onClick={() => reject(item)}>
                    Reject
                  </Button>
                </div>
                {actions.note ? <div className="foot-note" style={{ marginTop: 4 }}>{actions.note}</div> : null}
                {state.itemErrors[item.index] ? <div style={{ marginTop: 4 }}><Notice tone="bad">{state.itemErrors[item.index]}</Notice></div> : null}
              </Notice></div>
            );
          })}
          <div className="foot-note">Approval stops at approved drafts. Nothing is submitted to a provider from here.</div>
          <div>
            <Button type="button" variant="ghost" onClick={forget}>Forget this batch</Button>
          </div>
        </div>
      ) : null}
    </Card>
  );
}
