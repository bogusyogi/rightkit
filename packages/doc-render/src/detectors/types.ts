import type { RootContent } from "mdast";

/** Confidence threshold above which a semantic component renders instead of plain markdown. */
export const CONFIDENCE_THRESHOLD = 0.6;

export type SemanticKind =
  | "frontmatter"
  | "callout"
  | "stepper"
  | "comparison"
  | "faq"
  | "richTable"
  | "descriptionList";

/**
 * A detector inspects a slice of top-level mdast nodes and either claims them
 * (returning a hit) or declines (returning null → silent fallback to plain markdown).
 * Detectors are pure: no I/O, no side effects. Fixture-testable in isolation.
 */
export interface DetectorHit {
  kind: SemanticKind;
  /** 0..1. The gate renders a component only at or above CONFIDENCE_THRESHOLD. */
  confidence: number;
  /** How many consecutive top-level nodes (starting at the offered index) this hit consumes. */
  consumed: number;
  /** Parsed payload the matching component renders from. */
  data: unknown;
  /** True when an explicit `<!-- doc: KIND --> (legacy `viewright:` accepted)` hint forced this hit. */
  forced?: boolean;
}

/**
 * @param nodes  full list of top-level nodes
 * @param index  current position to attempt a match at
 * @param hint   explicit component hint active at this position, if any
 */
export type Detector = (
  nodes: RootContent[],
  index: number,
  hint: SemanticKind | null,
) => DetectorHit | null;
