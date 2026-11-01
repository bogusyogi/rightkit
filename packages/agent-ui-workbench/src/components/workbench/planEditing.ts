export interface EditablePlanStep {
  id: string;
  text: string;
}

export interface EditablePlanDraft {
  intro: string;
  steps: EditablePlanStep[];
  outro: string;
}

const STEP_PATTERN = /^\s*(?:(?:[-*+]\s+(?:\[[ xX]\]\s+)?)|(?:\d+[.)]\s+))(.*\S)\s*$/;

function makeStepId(index: number, text: string) {
  const slug = text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);
  return `step-${index}-${slug || "item"}`;
}

export function parseEditablePlan(plan: string): EditablePlanDraft {
  const intro: string[] = [];
  const steps: EditablePlanStep[] = [];
  const outro: string[] = [];
  let sawStep = false;
  let sawOutro = false;

  for (const line of plan.split(/\r?\n/)) {
    const match = line.match(STEP_PATTERN);
    if (match && !sawOutro) {
      const text = match[1].trim();
      steps.push({ id: makeStepId(steps.length, text), text });
      sawStep = true;
      continue;
    }
    if (sawStep) {
      sawOutro = true;
      outro.push(line);
    } else {
      intro.push(line);
    }
  }

  if (steps.length === 0) {
    const fallback = plan.trim();
    return {
      intro: "",
      steps: fallback ? [{ id: makeStepId(0, fallback), text: fallback }] : [],
      outro: "",
    };
  }

  return {
    intro: intro.join("\n").trim(),
    steps,
    outro: outro.join("\n").trim(),
  };
}

export function updatePlanStep(
  draft: EditablePlanDraft,
  id: string,
  text: string,
): EditablePlanDraft {
  return {
    ...draft,
    steps: draft.steps.map((step) => (step.id === id ? { ...step, text } : step)),
  };
}

export function deletePlanStep(draft: EditablePlanDraft, id: string): EditablePlanDraft {
  return {
    ...draft,
    steps: draft.steps.filter((step) => step.id !== id),
  };
}

export function movePlanStep(
  draft: EditablePlanDraft,
  id: string,
  delta: -1 | 1,
): EditablePlanDraft {
  const index = draft.steps.findIndex((step) => step.id === id);
  const nextIndex = index + delta;
  if (index < 0 || nextIndex < 0 || nextIndex >= draft.steps.length) {
    return draft;
  }
  const steps = [...draft.steps];
  const [step] = steps.splice(index, 1);
  steps.splice(nextIndex, 0, step);
  return { ...draft, steps };
}
