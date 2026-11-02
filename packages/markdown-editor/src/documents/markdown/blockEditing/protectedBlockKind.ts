/**
 * Protected-block kind registry.
 *
 * The registry is the single owner of "what kinds of blocks are
 * editable inline." Adapters register kinds; the framework
 * dispatches editing commands to registered kinds.
 */

import type { BlockKind } from "./types.js";

/**
 * One registration.
 */
export interface ProtectedBlockRegistration {
  readonly kind: BlockKind;
  readonly displayName: string;
  readonly isDirect: boolean;
  readonly sourceSyntax:
    | { readonly kind: "fence"; readonly language?: string }
    | { readonly kind: "dollar"; readonly displayMode: boolean }
    | { readonly kind: "image"; readonly altFormat?: string }
    | { readonly kind: "link"; readonly titleFormat?: string }
    | { readonly kind: "footnote"; readonly style: "numbered" | "named" }
    | { readonly kind: "heading"; readonly style: "atx" | "setext" }
    | { readonly kind: "list"; readonly style: "bullet" | "ordered" }
    | { readonly kind: "semantic"; readonly semanticKind: string };
}

/**
 * Internal registry of protected-block kinds. Keys are
 * {@link BlockKind}; values are registrations.
 */
const registry = new Map<BlockKind, ProtectedBlockRegistration>();

/**
 * Register a protected-block kind. Re-registering replaces the
 * previous registration; this is intentional so adapters can
 * override defaults during mount.
 */
export function registerProtectedBlock(registration: ProtectedBlockRegistration): void {
  registry.set(registration.kind, registration);
}

/**
 * Look up the registration for `kind`. Returns `null` when no
 * adapter has registered.
 */
export function getProtectedBlockRegistration(kind: BlockKind): ProtectedBlockRegistration | null {
  return registry.get(kind) ?? null;
}

/**
 * Returns every registration, sorted alphabetically by
 * `displayName`. Useful for the adapter list shown in the
 * settings UI.
 */
export function listProtectedBlocks(): readonly ProtectedBlockRegistration[] {
  return Array.from(registry.values()).sort((a, b) => a.displayName.localeCompare(b.displayName));
}

/**
 * Clear the registry. Tests use this to reset between cases.
 */
export function clearProtectedBlocks(): void {
  registry.clear();
}

// Register common protected kinds used by ProtectedBlockHost routing.
// This ensures Mermaid and other kinds are discoverable via the shared
// block-kind registry without requiring adapters to pre-register.
//
// Ordinary representable fenced code projects to the native `code_fence` node
// (contract 14 CE-3). The protected `code-raw-fallback` kind is the EXPLICIT
// raw fallback for unrepresentable fenced blocks only (indented code, malformed
// or unterminated fences). Ordinary code → native code_fence; only
// unsupported → raw fallback. The generic `"code"` route has been removed so
// ordinary fences cannot be recaptured — projection intercepts those before
// rendering.
registerProtectedBlock({
  kind: "code-raw-fallback",
  displayName: "Code (raw fallback)",
  isDirect: true,
  sourceSyntax: { kind: "fence", language: "code" },
});
registerProtectedBlock({
  kind: "mermaid",
  displayName: "Mermaid diagram",
  isDirect: true,
  sourceSyntax: { kind: "fence", language: "mermaid" },
});
registerProtectedBlock({
  kind: "math",
  displayName: "Math block",
  isDirect: true,
  sourceSyntax: { kind: "dollar", displayMode: true },
});
registerProtectedBlock({
  kind: "table",
  displayName: "Table (protected)",
  isDirect: false,
  sourceSyntax: { kind: "fence", language: "table" },
});
registerProtectedBlock({
  kind: "raw",
  displayName: "Raw block",
  isDirect: false,
  sourceSyntax: { kind: "fence", language: "raw" },
});
for (const semanticKind of ["faq", "stepper", "comparison", "richTable"] as const) {
  registerProtectedBlock({
    kind: semanticKind,
    displayName: semanticKind[0].toUpperCase() + semanticKind.slice(1),
    isDirect: false,
    sourceSyntax: { kind: "semantic", semanticKind },
  });
}
