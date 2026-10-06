import type { RootContent } from "mdast";
import { CONFIDENCE_THRESHOLD, type Detector, type DetectorHit, type SemanticKind } from "./types";
import { calloutDetector } from "./callout.js";
import { frontmatterDetector } from "./frontmatter.js";
import { stepperDetector } from "./stepper.js";
import { comparisonDetector } from "./comparison.js";
import { faqDetector } from "./faq.js";
import { richTableDetector } from "./richTable.js";
import { descriptionListDetector } from "./descriptionList.js";

export * from "./types";

// Priority order: most specific / least ambiguous first.
const DETECTORS: Detector[] = [
  frontmatterDetector,
  calloutDetector,
  richTableDetector,
  faqDetector,
  descriptionListDetector,
  comparisonDetector,
  stepperDetector,
];

export type Block =
  | { kind: "semantic"; hit: DetectorHit; position: { start: number; end: number } }
  | { kind: "plain"; nodes: RootContent[] };

const HINT_RE =
  /<!--\s*(?:doc|viewright):\s*(frontmatter|callout|stepper|comparison|faq|richTable|descriptionList)\s*-->/i;

function parseHint(node: RootContent): SemanticKind | null {
  if ((node.type as string) !== "html") return null;
  const m = HINT_RE.exec((node as unknown as { value: string }).value ?? "");
  return m ? (m[1] as SemanticKind) : null;
}

/**
 * The confidence gate. Walks top-level nodes, runs detectors, and decides per block:
 *   - hit at/above threshold (or forced by a hint) → semantic component
 *   - hit below threshold                          → plain markdown (silent fallback)
 *   - no hit                                       → plain markdown
 * Returns a flat block list the renderer maps to React.
 */
export function buildBlocks(nodes: RootContent[]): Block[] {
  const blocks: Block[] = [];
  let pendingHint: SemanticKind | null = null;
  let plainRun: RootContent[] = [];

  const flushPlain = () => {
    if (plainRun.length) {
      blocks.push({ kind: "plain", nodes: plainRun });
      plainRun = [];
    }
  };

  let i = 0;
  while (i < nodes.length) {
    const node = nodes[i];

    const hint = parseHint(node);
    if (hint) {
      pendingHint = hint;
      i += 1; // the hint comment itself never renders
      continue;
    }

    let chosen: DetectorHit | null = null;
    for (const detect of DETECTORS) {
      const hit = detect(nodes, i, pendingHint);
      if (hit && (!chosen || hit.confidence > chosen.confidence || hit.forced)) {
        chosen = hit;
        if (hit.forced) break;
      }
    }

    if (chosen && (chosen.confidence >= CONFIDENCE_THRESHOLD || chosen.forced)) {
      flushPlain();
      const startNode = nodes[i];
      const endNode = nodes[i + chosen.consumed - 1];
      const start = startNode.position?.start.offset ?? 0;
      const end = endNode.position?.end.offset ?? startNode.position?.start.offset ?? 0;
      blocks.push({ kind: "semantic", hit: chosen, position: { start, end } });
      i += chosen.consumed;
    } else if (chosen) {
      // Below threshold: render the nodes the detector eyed as ordinary markdown.
      for (let k = 0; k < chosen.consumed; k++) plainRun.push(nodes[i + k]);
      i += chosen.consumed;
    } else {
      plainRun.push(node);
      i += 1;
    }
    pendingHint = null;
  }

  flushPlain();
  return blocks;
}
