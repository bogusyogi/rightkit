import { useEffect, useMemo, useState } from 'react';
import { errorMessage, studio } from '../lib/api';
import { Card, Badge, Button, EmptyState, Notice, SegmentedControl, Select } from '@rightkit/react-shell/react';
import { Kv } from '../components/ui';
import brandCatalog from '../studio-data/brands/brands.json';
import { RecipeTemplatesArea } from './RecipeTemplates';
import { RecipeCatalogueArea } from './RecipeCatalogue';

interface LibraryItem {
  key: string;
  name?: string | null;
  use?: string | null;
  fragment?: string | null;
  tags?: string[] | null;
}

interface Library {
  id: string;
  slot: string;
  subcategory?: string | null;
  principle?: string | null;
  brand_defaults?: Record<string, string | null> | null;
  items: LibraryItem[];
}

interface ModelRow {
  slug: string;
  backend?: string | null;
  negative_compile_strategy?: string | null;
  reference_max?: number | null;
  max_prompt_length?: number | null;
  has_contract?: boolean;
}

interface LibrariesResult {
  libraries: Library[];
  models: ModelRow[];
  model_brand_defaults?: Record<string, string | null> | null;
}

interface ComposeResult {
  ok: boolean;
  prompt?: string;
  params?: Record<string, unknown>;
  meta?: Record<string, unknown>;
  message?: string;
  violations?: Array<{ rule?: string; detail?: string } | string>;
}

const ASPECTS = ['1:1', '16:9', '9:16', '4:3', '3:4', '3:2', '2:3'];
const BRANDS: string[] = (brandCatalog.brands as Array<{ code: string }>).map((b) => b.code);
const COMPOSE_SLOTS = ['aesthetic', 'composition', 'lens', 'lighting', 'depth', 'color', 'skin'];

export function Recipes() {
  const [libs, setLibs] = useState<LibrariesResult | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [subject, setSubject] = useState('');
  const [model, setModel] = useState('');
  const [brand, setBrand] = useState('');
  const [aspect, setAspect] = useState('1:1');
  const [hasHumans, setHasHumans] = useState(true);
  const [picks, setPicks] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ComposeResult | null>(null);

  useEffect(() => {
    let live = true;
    studio<LibrariesResult>('recipes.libraries')
      .then((r) => {
        if (!live) return;
        setLibs(r);
        const def = r.models.find((m) => m.has_contract)?.slug ?? r.models[0]?.slug ?? '';
        setModel(def);
      })
      .catch((e) => live && setLoadError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, []);

  const slotLibraries = useMemo(
    () => (libs?.libraries ?? []).filter((l) => COMPOSE_SLOTS.includes(l.slot)),
    [libs]
  );
  const editingLibraries = useMemo(
    () => (libs?.libraries ?? []).filter((l) => !COMPOSE_SLOTS.includes(l.slot)),
    [libs]
  );
  const activeModel = libs?.models.find((m) => m.slug === model);

  const pick = (slot: string, key: string) => setPicks((p) => ({ ...p, [slot]: key }));

  const compose = async () => {
    setBusy(true);
    setError(null);
    setResult(null);
    const opts: Record<string, unknown> = {
      subject: subject.trim(),
      aspect,
      has_humans: hasHumans
    };
    if (model) opts.model = model;
    if (brand) opts.brand = brand;
    for (const slot of COMPOSE_SLOTS) {
      if (picks[slot]) opts[slot] = picks[slot];
    }
    try {
      const r = await studio<ComposeResult>('recipes.compose', { opts });
      if (r.ok === false) setError(r.message ?? 'compose failed');
      else setResult(r);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const canCompose = subject.trim() !== '' && (model !== '' || brand !== '') && !busy;

  return (
    <div className="content view">
      <section className="work">
        <div className="eyebrow">Prompt recipes</div>
        <h1>Recipes</h1>
        <p className="lede">
          Compose a structured brief into a per-model prompt. Pickers come from the recipe libraries bundled with the
          engine; leaving a slot on Auto uses the brand default, then the library default.
        </p>

        {loadError ? <Notice tone="bad">{loadError}</Notice> : null}
        {!libs && !loadError ? <EmptyState title="Loading recipe libraries…" /> : null}

        {libs ? (
          <>
            <Card className="form gb-mb18">
              <div>
                <label htmlFor="rc-subject">Subject</label>
                <textarea className="rk-input"
                  id="rc-subject"
                  value={subject}
                  placeholder="a copper folding knife on warm linen"
                  onChange={(e) => setSubject(e.target.value)}
                />
              </div>
              <div className="fgrid">
                <div>
                  <label>Model</label>
                  <Select label="Model" value={model} onChange={setModel} options={[{ value: '', label: 'Brand default' }, ...libs.models.map((m) => ({ value: m.slug, label: m.slug }))]} />
                </div>
                <div>
                  <label>Brand</label>
                  <Select label="Brand" value={brand} onChange={setBrand} options={[{ value: '', label: 'None' }, ...BRANDS.map((b) => ({ value: b, label: b }))]} />
                </div>
                <div>
                  <label>Aspect</label>
                  <Select label="Aspect" value={aspect} onChange={setAspect} options={ASPECTS.map((a) => ({ value: a, label: a }))} />
                </div>
                <div>
                  <label>Humans in frame</label>
                  <SegmentedControl
                    label="Humans in frame"
                    value={hasHumans ? 'yes' : 'no'}
                    options={[{ value: 'yes', label: 'Yes' }, { value: 'no', label: 'No' }]}
                    onChange={(v) => setHasHumans(v === 'yes')}
                  />
                </div>
              </div>
            </Card>

            <div className="eyebrow" style={{ marginBottom: 8 }}>Recipe pickers</div>
            <div className="templates" style={{ gridTemplateColumns: 'repeat(auto-fill,minmax(230px,1fr))' }}>
              {slotLibraries.map((lib) => (
                <Card flush className="group-box gb-mt0" key={lib.id}>
                  <div className="setting" style={{ borderTop: 0 }}>
                    <b style={{ fontSize: 12.5 }}>{lib.subcategory ?? lib.id}</b>
                    <Badge>{lib.items.length}</Badge>
                  </div>
                  {lib.principle ? (
                    <div style={{ padding: '0 12px 8px', fontSize: 11.5, color: 'var(--ink-3)' }}>{lib.principle}</div>
                  ) : null}
                  <div style={{ padding: '0 12px 12px' }}>
                    <Select
                      label={`${lib.id} picker`}
                      value={picks[lib.slot] ?? ''}
                      onChange={(v) => pick(lib.slot, v)}
                      options={[{ value: '', label: 'Auto' }, ...lib.items.map((it) => ({ value: it.key, label: it.name ?? it.key }))]}
                    />
                    {picks[lib.slot] ? (
                      <div style={{ marginTop: 6, fontSize: 11.5, color: 'var(--ink-2)' }}>
                        {lib.items.find((i) => i.key === picks[lib.slot])?.use ?? ''}
                      </div>
                    ) : null}
                  </div>
                </Card>
              ))}
            </div>
            {editingLibraries.length ? (
              <p className="lede" style={{ marginTop: 14, fontSize: 12 }}>
                Edit-mode libraries ({editingLibraries.map((l) => l.id).join(', ')}) apply to image edits and are composed
                through the Image studio edit flow.
              </p>
            ) : null}
          </>
        ) : null}

        <RecipeTemplatesArea />
        <RecipeCatalogueArea />
      </section>

      <aside className="run">
        <div className="run-head">
          <div className="eyebrow">Compose</div>
          <h3 style={{ margin: '2px 0 0', fontSize: 15 }}>Prompt preview</h3>
        </div>
        <div className="run-body">
          {error ? <Notice tone="bad">{error}</Notice> : null}
          {result ? (
            <>
              <div style={{ whiteSpace: 'pre-wrap' }}><Notice tone="info">{result.prompt}</Notice></div>
              {result.params && Object.keys(result.params).length ? (
                <>
                  <h6 style={{ margin: '14px 0 4px', fontSize: 12, color: 'var(--ink-3)' }}>Model params</h6>
                  <Kv rows={Object.entries(result.params).map(([k, v]) => [k, <span className="mono" key={k}>{JSON.stringify(v)}</span>])} />
                </>
              ) : null}
              {activeModel ? (
                <div style={{ marginTop: 12, display: 'flex', gap: 6, flexWrap: 'wrap' }}>
                  {activeModel.has_contract ? <Badge tone="ok">contract</Badge> : <Badge tone="warn">no contract</Badge>}
                  {activeModel.reference_max ? <Badge>{activeModel.reference_max} refs</Badge> : null}
                  {activeModel.max_prompt_length ? <Badge>{activeModel.max_prompt_length} chars</Badge> : null}
                </div>
              ) : null}
            </>
          ) : (
            !error && <EmptyState title="Nothing composed yet">The composed prompt and provider params appear here.</EmptyState>
          )}
        </div>
        <div className="run-foot">
          <Button type="button" variant="primary" disabled={!canCompose} onClick={compose}>
            {busy ? 'Composing…' : 'Compose prompt'}
          </Button>
          <div className="foot-note">Composition only — nothing is sent to a provider.</div>
        </div>
      </aside>
    </div>
  );
}
