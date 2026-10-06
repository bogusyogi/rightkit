import type { Heading, List, ListItem } from "mdast";
import type { Detector, DetectorHit } from "./types";
import { nodeText } from "./nodeText.js";

export interface StepperData {
  title: string | null;
  steps: string[];
}

// Process-keyword vocabulary — one signal among several.
const PROCESS_WORDS =
  /\b(steps?|how\s*to|guide|recipe|instructions?|setup|set\s*up|process|walkthrough|tutorial|procedure|getting\s+started)\b/i;

// Imperative verbs that step instructions commonly start with — a strong structural signal
// that an ordered list is procedural (vs. e.g. a ranked list of nouns).
const STEP_VERB =
  /^(add|allow|apply|build|call|change|check|choose|click|clone|close|configure|connect|copy|create|define|delete|deploy|disable|download|edit|enable|enter|export|extract|fill|find|fix|fold|get|go|import|init|insert|install|launch|load|lock|make|mark|merge|move|navigate|open|paste|pick|place|press|pull|push|read|register|reload|remove|rename|repeat|replace|restart|run|save|select|set|ship|start|stop|switch|tap|test|toggle|turn|type|update|upload|use|verify|visit|wait|write)\b/i;

/**
 * Stepper: an ordered list, optionally introduced by a process-keyword heading.
 * - heading + ordered list with a process word → high confidence
 * - bare ordered list                          → low confidence (gate falls back to plain)
 * - explicit hint                              → forced
 */
export const stepperDetector: Detector = (nodes, index, hint): DetectorHit | null => {
  let listIndex = index;
  let heading: Heading | null = null;

  if (nodes[index]?.type === "heading" && nodes[index + 1]?.type === "list") {
    heading = nodes[index] as Heading;
    listIndex = index + 1;
  } else if (nodes[index]?.type !== "list") {
    return null;
  }

  const list = nodes[listIndex] as List;
  if (list.type !== "list" || !list.ordered) return null;

  const steps = list.children
    .filter((c): c is ListItem => c.type === "listItem")
    .map((li) => nodeText(li).trim())
    .filter(Boolean);
  if (steps.length < 2) return null;

  const title = heading ? nodeText(heading).trim() : null;
  const consumed = heading ? 2 : 1;

  if (hint === "stepper") {
    return {
      kind: "stepper",
      confidence: 1,
      consumed,
      data: { title, steps } satisfies StepperData,
      forced: true,
    };
  }

  // Multi-signal score: combine independent weak signals rather than a single keyword.
  const verbRatio = steps.filter((s) => STEP_VERB.test(s)).length / steps.length;
  let confidence = 0.2; // bare ordered list — below threshold, renders as plain
  if (title && PROCESS_WORDS.test(title))
    confidence += 0.45; // process-keyword heading
  else if (title) confidence += 0.12; // some heading
  if (verbRatio >= 0.6)
    confidence += 0.4; // most items start with an imperative verb
  else if (verbRatio >= 0.3) confidence += 0.2;
  if (steps.length >= 3) confidence += 0.1; // a real procedure tends to have ≥3 steps
  confidence = Math.min(confidence, 0.95);

  return { kind: "stepper", confidence, consumed, data: { title, steps } satisfies StepperData };
};
