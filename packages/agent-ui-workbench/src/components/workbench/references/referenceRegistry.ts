import { createElement, type ReactNode } from "react";

import ReferenceLink from "./ReferenceLink.js";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Module-level registry for inline-reference resolution. `MarkdownText.tsx`
// resolves a URL/destination string to a `ReferenceObject` (if any) by calling
// `resolveInlineReference`. The renderer returns either a `ReactNode`
// (ReferenceLink) for known references or `undefined` to signal "fall through
// to legacy regex path".
//
// The registry is intentionally a plain module object — Zustand-aware lookups
// are deferred to the call site, which subscribes to the store. This keeps
// the inline path synchronous (the regex match in `MarkdownText` is on a
// hot path) and lets the resolver stay pure (no React rendering inside).

export interface InlineReferenceResolution {
  reference: ReferenceObject;
  /** Optional ReactNode override; when omitted, ReferenceLink is used with
   * `label = match`. */
  node?: ReactNode;
}

export interface InlineReferenceResolver {
  /**
   * Resolve a URL / path / destination string from the MarkdownText regex
   * match to a typed reference. Returns `undefined` to signal "no known
   * reference — use legacy regex rendering".
   *
   * Implementations should be cheap (linear scan of an in-memory store) since
   * this is on the hot path of every rendered span.
   */
  resolveByUrl(url: string): InlineReferenceResolution | undefined;
  /**
   * Same as `resolveByUrl`, but keyed by ReferenceObject.id (used by
   * reference-aware markdown extensions that want to render the same
   * reference from multiple text positions).
   */
  resolveById(id: string): InlineReferenceResolution | undefined;
}

const noopResolver: InlineReferenceResolver = {
  resolveByUrl: () => undefined,
  resolveById: () => undefined,
};

let activeResolver: InlineReferenceResolver = noopResolver;

/** Replace the active resolver. Caller is responsible for restoring the
 * previous resolver when unmounting (test isolation). */
export function setInlineReferenceResolver(resolver: InlineReferenceResolver | null): void {
  activeResolver = resolver ?? noopResolver;
}

/** Read the active resolver. `MarkdownText.tsx` calls this on every match. */
export function getInlineReferenceResolver(): InlineReferenceResolver {
  return activeResolver;
}

/** Convenience renderer: returns a ReactNode for a known reference, or
 * `undefined` for a fallback. Used by `MarkdownText` so the call site stays
 * a single-line expression. */
export function renderInlineReference(url: string, label?: string): ReactNode | undefined {
  const resolution = activeResolver.resolveByUrl(url);
  if (!resolution) return undefined;
  if (resolution.node) return resolution.node;
  return createElement(ReferenceLink, {
    reference: resolution.reference,
    ...(label === undefined ? {} : { label }),
    onAction: (id, action) => {
      const hostAction = hostActionHandler;
      if (hostAction) hostAction(id, action);
    },
  });
}

/** A second registry: a top-level "host action" handler. ReferenceLink
 * dispatches to this when no per-call onAction is provided. Tests stub it. */
type HostAction = (id: string, action: "open" | "reveal" | "copy_path" | "preview") => void;
let hostActionHandler: HostAction | null = null;

export function setInlineReferenceHostAction(handler: HostAction | null): void {
  hostActionHandler = handler;
}

/** Build a resolver from a list of in-memory references (cheap; linear scan).
 * Suitable for tests and the composer store wiring. */
export function buildInlineReferenceResolver(
  references: readonly ReferenceObject[],
): InlineReferenceResolver {
  const byUrl = new Map<string, ReferenceObject>();
  const byId = new Map<string, ReferenceObject>();
  for (const ref of references) {
    byId.set(ref.id, ref);
    byUrl.set(ref.destination, ref);
    if (ref.filename) byUrl.set(ref.filename, ref);
  }
  return {
    resolveByUrl(url) {
      const reference = byUrl.get(url);
      return reference ? { reference } : undefined;
    },
    resolveById(id) {
      const reference = byId.get(id);
      return reference ? { reference } : undefined;
    },
  };
}
