import type { EditorView } from "@codemirror/view";

const scrollRevision = new WeakMap<EditorView, number>();
const MAX_SCROLL_MEASURES = 8;

/**
 * Scroll a document position to vertical center, reliably.
 *
 * CodeMirror's own scroll (`EditorView.scrollIntoView` effect AND the `scrollIntoView` transaction
 * flag) does not move the editor in this app — find/inspect set the selection but the view stays put.
 * This drives `scrollDOM.scrollTop` directly, which always works. The wrinkle is that line heights for
 * not-yet-rendered lines are ESTIMATED, so the first scroll only lands approximately; once the target
 * region renders with real heights we recompute and correct, converging within a few measure passes.
 */
export function scrollPosIntoView(view: EditorView, pos: number): void {
  const target = Math.max(0, Math.min(pos, view.state.doc.length));
  const revision = (scrollRevision.get(view) ?? 0) + 1;
  scrollRevision.set(view, revision);
  let measures = 0;
  let stablePaints = 0;

  const isCurrent = () => view.dom.isConnected && scrollRevision.get(view) === revision;
  const schedule = () => {
    if (!isCurrent() || measures >= MAX_SCROLL_MEASURES) return;
    measures += 1;
    view.requestMeasure({
      read: () => {
        if (!isCurrent()) return null;
        const sc = view.scrollDOM;
        const block = view.lineBlockAt(target);
        const margin = Math.max(0, (sc.clientHeight - block.height) / 2);
        return Math.max(0, Math.min(block.top - margin, sc.scrollHeight - sc.clientHeight));
      },
      write: (want) => {
        if (want === null || !isCurrent()) return;
        const sc = view.scrollDOM;
        if (Math.abs(sc.scrollTop - want) > 2) {
          sc.scrollTop = want;
          stablePaints = 0;
        } else {
          stablePaints += 1;
        }

        // CodeMirror replaces offscreen line-height estimates only after the newly reached viewport
        // paints. Immediate requestMeasure recursion sees the same estimate and stops too early,
        // leaving long wrapped-source matches outside the drawer. Wait for paint, then converge.
        if (stablePaints < 2 && measures < MAX_SCROLL_MEASURES) requestAnimationFrame(schedule);
      },
    });
  };
  schedule();
}
