import { useEffect, useState } from 'react';
import {
  recipeCatalogueList,
  recipeCatalogueValidate,
  type RecipeCatalogueError,
  type RecipeCatalogueGraph,
  type RecipeCatalogueResult,
  type RecipeCatalogueValidation
} from '../lib/api.js';
import { Card, Badge, Button, EmptyState, Notice } from '@rightkit/react-shell/react';
import { Kv } from '../components/ui.js';
import { EngineError, toError } from '../lib/engine-error.js';
import motionTransfer from '../studio-data/recipes/catalogue/motion_transfer.json' with { type: "json" };
import objectReplacement from '../studio-data/recipes/catalogue/object_replacement.json' with { type: "json" };
import voxDirector from '../studio-data/recipes/catalogue/vox_director.json' with { type: "json" };
import {
  errorsByNode,
  missingRoutesText,
  nodeDepths,
  orderNodes,
  paramsText,
  routeLabel,
  SERVED_DETAIL,
  servedText,
  servedTone,
  unbuiltPlanned,
  validationSummary
} from './recipe-catalogue-logic.js';

interface DefinitionNode {
  id: string;
  depends_on?: string[];
}

// The engine's listing reports each node's status but not its depends_on. The seeded files here are the
// same ones the engine embeds at build time, so the dependency edges are read from them.
const DEFINITION_NODES: Record<string, DefinitionNode[]> = {
  motion_transfer: (motionTransfer as unknown as { nodes: DefinitionNode[] }).nodes,
  object_replacement: (objectReplacement as unknown as { nodes: DefinitionNode[] }).nodes,
  vox_director: (voxDirector as unknown as { nodes: DefinitionNode[] }).nodes
};

type CheckState =
  | { phase: 'busy' }
  | { phase: 'done'; result: RecipeCatalogueValidation }
  | { phase: 'failed'; message: string };

/** The engine's message without its code: codes are shown only as a small secondary line. */
function errorText(error: unknown): string {
  const e = toError(error);
  return e instanceof EngineError ? e.detail : e.message;
}

/** The seeded recipe graphs (definitions, served status, validation) and the plan's not-yet-built list. */
export function RecipeCatalogueArea() {
  const [data, setData] = useState<RecipeCatalogueResult | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [checks, setChecks] = useState<Record<string, CheckState>>({});

  useEffect(() => {
    let live = true;
    recipeCatalogueList()
      .then((r) => live && setData(r))
      .catch((e) => live && setLoadError(errorText(e)));
    return () => {
      live = false;
    };
  }, []);

  const validate = async (id: string) => {
    setChecks((c) => ({ ...c, [id]: { phase: 'busy' } }));
    try {
      const result = await recipeCatalogueValidate(id);
      setChecks((c) => ({ ...c, [id]: { phase: 'done', result } }));
    } catch (e) {
      setChecks((c) => ({ ...c, [id]: { phase: 'failed', message: errorText(e) } }));
    }
  };

  const planned = data ? unbuiltPlanned(data.plan_named) : [];

  return (
    <div style={{ marginTop: 22 }}>
      <div className="eyebrow" style={{ marginBottom: 8 }}>Recipes</div>
      <p className="lede" style={{ fontSize: 12.5 }}>
        Each recipe is a set of steps. This list shows the definitions only: running a recipe still goes through the
        Pipelines panel, and nothing here runs a recipe or spends money.
      </p>
      {loadError ? <Notice tone="bad">{loadError}</Notice> : null}
      {!data && !loadError ? <EmptyState title="Loading recipes…" /> : null}

      {data ? (
        <>
          {data.graphs.map((g) => (
            <GraphCard key={g.id} graph={g} check={checks[g.id]} onValidate={() => validate(g.id)} />
          ))}

          <div className="eyebrow" style={{ marginTop: 22, marginBottom: 8 }}>Not available yet</div>
          <Card flush className="group-box gb-mt0 gb-dashed">
            <div className="setting" style={{ borderTop: 0 }}>
              <b style={{ fontSize: 12.5 }}>Named in the plan, not yet built</b>
              <Badge>{planned.length}</Badge>
            </div>
            <p style={{ padding: '0 12px 8px', fontSize: 11.5, color: 'var(--ink-3)' }}>
              The plan names these recipes, but they are not built yet, so they cannot be run or checked here.
            </p>
            <ul style={{ margin: 0, padding: '0 12px 12px 30px', fontSize: 12 }}>
              {planned.map((p) => (
                <li key={p.name} style={{ marginBottom: 6 }}>
                  <b>{p.name}</b>: <span style={{ color: 'var(--ink-2)' }}>{p.reason}</span>
                </li>
              ))}
            </ul>
          </Card>
        </>
      ) : null}
    </div>
  );
}

function GraphCard({
  graph,
  check,
  onValidate
}: {
  graph: RecipeCatalogueGraph;
  check: CheckState | undefined;
  onValidate: () => void;
}) {
  const defs = DEFINITION_NODES[graph.id] ?? [];
  const dependsOn = new Map(defs.map((n) => [n.id, n.depends_on ?? []]));
  const withDeps = graph.nodes.map((n) => ({ ...n, depends_on: dependsOn.get(n.id) ?? [] }));
  const ordered = orderNodes(withDeps);
  const depth = nodeDepths(withDeps);
  const nodeIds = graph.nodes.map((n) => n.id);

  const result = check?.phase === 'done' ? check.result : null;
  const errors = result ? errorsByNode(result.errors, nodeIds) : null;
  const busy = check?.phase === 'busy';

  return (
    <Card flush className="group-box gb-mt0 gb-mb14">
      <div className="setting" style={{ borderTop: 0 }}>
        <b style={{ fontSize: 12.5 }}>{graph.title}</b>
        <Badge tone={graph.valid ? 'ok' : 'bad'}>{validationSummary(graph.valid, graph.errors)}</Badge>
      </div>
      <div style={{ padding: '0 12px 10px', fontSize: 11.5, color: 'var(--ink-3)' }}>
        <details>
          <summary style={{ cursor: 'pointer' }}>Details</summary>
          <div className="mono" style={{ marginTop: 4 }}>{graph.id}</div>
          {Object.entries(graph.source).map(([k, v]) => (
            <div key={k}>
              {k}: {v}
            </div>
          ))}
        </details>
      </div>

      <div style={{ padding: '0 12px 10px' }}>
        <div className="eyebrow" style={{ marginBottom: 4 }}>Inputs</div>
        {graph.inputs.length ? (
          <Kv rows={graph.inputs.map((i) => [i.name, <span className="mono" key={i.name}>{i.type}</span>])} />
        ) : (
          <div style={{ fontSize: 12 }}>No inputs.</div>
        )}
      </div>

      <div style={{ padding: '0 12px 10px' }}>
        <div className="eyebrow" style={{ marginBottom: 4 }}>Steps, each after the steps it depends on</div>
        <ol style={{ listStyle: 'none', margin: 0, padding: 0 }}>
          {ordered.map((n) => {
            const deps = n.depends_on ?? [];
            const nodeErrors = errors?.byNode[n.id] ?? [];
            return (
              <li
                key={n.id}
                style={{ padding: `6px 0 6px ${(depth[n.id] ?? 0) * 18}px`, borderTop: '1px solid rgba(127, 127, 127, 0.25)' }}
              >
                <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                  <span className="mono"><b>{n.id}</b></span>
                  <span style={{ fontSize: 12 }}>
                    {n.kind}
                    {n.output ? ` → ${n.output}` : ''}
                  </span>
                  <Badge tone={servedTone(n.status)}>{servedText(n.status)}</Badge>
                </div>
                <div style={{ fontSize: 11.5, color: 'var(--ink-2)' }}>{SERVED_DETAIL[n.status] ?? ''}</div>
                <div style={{ fontSize: 12 }}>Route: {routeLabel(n.route, n.status)}</div>
                <div style={{ fontSize: 12 }}>Depends on: {deps.length ? deps.join(', ') : 'nothing'}</div>
                {n.params_checked !== undefined ? (
                  <div style={{ fontSize: 11.5, color: 'var(--ink-3)' }}>{paramsText(n.params_checked)}</div>
                ) : null}
                {n.status === 'unserved' ? (
                  <div style={{ fontSize: 12 }}>
                    Not in the app's catalogue, so it cannot run. Routes that would work (preferred first): {missingRoutesText(n.missing_routes)}
                  </div>
                ) : null}
                {nodeErrors.map((e, i) => (
                  <ErrorLine key={`${e.code}-${i}`} error={e} />
                ))}
              </li>
            );
          })}
        </ol>
      </div>

      <div style={{ padding: '0 12px 10px', fontSize: 12 }}>
        <div>
          Outputs: {graph.outputs.map((o) => `${o.name} from ${o.from}`).join(', ') || 'none'}
        </div>
        <div>
          Review: {graph.review.required ? 'required' : 'not required'}.{' '}
          <span style={{ color: 'var(--ink-3)' }}>{graph.review.policy}</span>
        </div>
      </div>

      <div style={{ padding: '0 12px 12px' }}>
        <Button type="button" variant="ghost" onClick={onValidate} disabled={busy}>
          {busy ? 'Checking…' : 'Check recipe'}
        </Button>
        {check?.phase === 'failed' ? <div style={{ marginTop: 8 }}><Notice tone="bad">{check.message}</Notice></div> : null}
        {result && errors ? (
          <div style={{ marginTop: 8 }}>
            <Badge tone={result.ok ? 'ok' : 'bad'}>{validationSummary(result.ok, result.errors)}</Badge>
            {errors.graph.map((e, i) => (
              <ErrorLine key={`graph-${e.code}-${i}`} error={e} />
            ))}
          </div>
        ) : null}
      </div>
    </Card>
  );
}

function ErrorLine({ error }: { error: RecipeCatalogueError }) {
  return (
    <div style={{ fontSize: 12, marginTop: 4 }}>
      <Notice tone="bad">
        {error.message}
        <span className="mono" style={{ display: 'block', fontSize: 10.5, color: 'var(--ink-3)' }}>{error.code}</span>
      </Notice>
    </div>
  );
}
