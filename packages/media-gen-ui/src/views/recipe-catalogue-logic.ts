// Pure helpers for the recipe graph catalogue view: dependency order and depth of a graph's nodes,
// served-status wording, the parameter-check line, and the mapping of validation errors onto the node
// each one names. The engine decides validity and served status; these helpers only order and label
// what it returned. No runtime imports from the app, so the node tests load this file directly.

import type { RecipeCatalogueError, RecipeCatalogueRoute, RecipePlannedName } from '../lib/api';

export interface OrderableNode {
  id: string;
  depends_on?: string[];
}

/**
 * Nodes in dependency order: each node follows every node it depends on. Ties keep declaration order.
 * A dependency that names no node is ignored here (the engine reports it). A node on a cycle, or
 * downstream of one, is appended in declaration order so nothing is dropped.
 */
export function orderNodes<T extends OrderableNode>(nodes: T[]): T[] {
  const ids = new Set(nodes.map((n) => n.id));
  const placed = new Set<string>();
  const out: T[] = [];
  let progressed = true;
  while (out.length < nodes.length && progressed) {
    progressed = false;
    for (const n of nodes) {
      if (placed.has(n.id)) continue;
      const waitingOn = (n.depends_on ?? []).filter((d) => ids.has(d) && d !== n.id);
      if (waitingOn.every((d) => placed.has(d))) {
        placed.add(n.id);
        out.push(n);
        progressed = true;
      }
    }
  }
  for (const n of nodes) if (!placed.has(n.id)) out.push(n);
  return out;
}

/**
 * Indent level of each node: 0 with no dependency, else one more than its deepest dependency.
 * Dependencies that name no node, and nodes on a cycle, do not add depth.
 */
export function nodeDepths(nodes: OrderableNode[]): Record<string, number> {
  const deps = new Map(nodes.map((n) => [n.id, (n.depends_on ?? []).filter((d) => d !== n.id)]));
  const depth: Record<string, number> = {};
  const visiting = new Set<string>();
  const depthOf = (id: string): number => {
    if (depth[id] !== undefined) return depth[id];
    if (visiting.has(id)) return 0;
    visiting.add(id);
    let best = -1;
    for (const d of deps.get(id) ?? []) {
      if (!deps.has(d)) continue;
      best = Math.max(best, depthOf(d));
    }
    visiting.delete(id);
    depth[id] = best + 1;
    return depth[id];
  };
  for (const n of nodes) depthOf(n.id);
  return depth;
}

export const SERVED_TEXT: Record<string, string> = {
  served: 'Available',
  unserved: 'Not available',
  ask: 'Chosen at run time',
  native: 'Built-in step',
  invalid_route: 'Invalid route'
};

export const SERVED_DETAIL: Record<string, string> = {
  served: "This provider, model and kind are in the app's catalogue.",
  unserved: "This provider and model are not in the app's catalogue, so this step cannot run until a route is available.",
  ask: 'No route is fixed here; the run asks you for one.',
  native: 'A built-in step, not a provider route.',
  invalid_route: 'The provider or model name is not valid.'
};

/** Short status text shown beside the chip. An unknown status is shown as the raw word, never hidden. */
export function servedText(status: string): string {
  return SERVED_TEXT[status] ?? status;
}

/** Chip tone for a status. Colour is never the only signal: the chip always carries the text. */
export function servedTone(status: string): 'ok' | 'bad' | undefined {
  if (status === 'served') return 'ok';
  if (status === 'unserved' || status === 'invalid_route') return 'bad';
  return undefined;
}

/** What the route column says: "provider · model", or the plain phrase for a route with no provider. */
export function routeLabel(route: RecipeCatalogueRoute | 'ask' | null | undefined, status: string): string {
  if (route && typeof route === 'object') return `${route.provider} · ${route.model}`;
  if (route === 'ask' || status === 'ask') return 'chosen at run time';
  if (status === 'native') return 'built-in step, no provider';
  return 'no route';
}

/** The settings line. `params_checked: false` reads as the spec states it, not as a failure. */
export function paramsText(checked: boolean | undefined): string {
  return checked === true
    ? 'settings checked against the recipe definition'
    : 'settings not checked against the recipe definition';
}

/** The missing-route line for an unserved node: every route it would need, primary first. */
export function missingRoutesText(routes: RecipeCatalogueRoute[]): string {
  return routes.map((r) => `${r.provider} · ${r.model}`).join('; ');
}

/** "Valid, no errors" or a count of errors, for the graph and the validate result. */
export function validationSummary(ok: boolean, errors: RecipeCatalogueError[]): string {
  if (ok && errors.length === 0) return 'Valid: no errors';
  return errors.length === 1 ? '1 error' : `${errors.length} errors`;
}

export interface GraphErrors {
  /** Errors that name a node in this graph, keyed by node id, in the order the engine returned them. */
  byNode: Record<string, RecipeCatalogueError[]>;
  /** Errors that name no node, or a node this graph does not have. Nothing is dropped. */
  graph: RecipeCatalogueError[];
}

/** Map each validation error onto the node it names. An error with no matching node stays at graph level. */
export function errorsByNode(errors: RecipeCatalogueError[], nodeIds: string[]): GraphErrors {
  const known = new Set(nodeIds);
  const byNode: Record<string, RecipeCatalogueError[]> = {};
  const graph: RecipeCatalogueError[] = [];
  for (const e of errors) {
    if (e.node && known.has(e.node)) {
      (byNode[e.node] ??= []).push(e);
    } else {
      graph.push(e);
    }
  }
  return { byNode, graph };
}

/** Recipes the plan names that have no seeded graph. Seeded ones are shown as graphs, not here. */
export function unbuiltPlanned(named: RecipePlannedName[]): RecipePlannedName[] {
  return named.filter((n) => !n.seeded);
}
