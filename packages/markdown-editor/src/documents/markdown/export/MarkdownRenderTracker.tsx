import { createContext } from "react";

export class MarkdownRenderTracker {
  private readonly pending = new Map<number, string>();
  private readonly errors: string[] = [];
  private nextId = 1;
  private revision = 0;

  begin(label: string): () => void {
    const id = this.nextId++;
    this.pending.set(id, label);
    this.revision++;
    let finished = false;
    return () => {
      if (finished) return;
      finished = true;
      this.pending.delete(id);
      this.revision++;
    };
  }

  fail(label: string, error: unknown): void {
    this.errors.push(`${label}: ${error instanceof Error ? error.message : String(error)}`);
    this.revision++;
  }

  snapshot() {
    return {
      pending: this.pending.size,
      pendingLabels: [...this.pending.values()],
      revision: this.revision,
      errors: [...this.errors],
    };
  }
}

export const MarkdownRenderTrackerContext = createContext<MarkdownRenderTracker | null>(null);

const nextFrame = () =>
  new Promise<void>((resolve) => {
    requestAnimationFrame(() => resolve());
  });

export async function waitForStableMarkdownRender(
  tracker: MarkdownRenderTracker,
  timeoutMs = 10_000,
): Promise<readonly string[]> {
  const started = performance.now();
  let stableRevision = -1;
  let stableFrames = 0;
  while (performance.now() - started < timeoutMs) {
    await nextFrame();
    const state = tracker.snapshot();
    if (state.pending === 0 && state.revision === stableRevision) {
      stableFrames++;
      if (stableFrames >= 2) return state.errors;
    } else {
      stableRevision = state.revision;
      stableFrames = state.pending === 0 ? 1 : 0;
    }
  }
  const pending = tracker.snapshot().pendingLabels.join(", ") || "unknown work";
  throw new Error(`Markdown export timed out waiting for: ${pending}`);
}
