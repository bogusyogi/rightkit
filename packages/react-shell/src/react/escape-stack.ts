import { useEffect, useRef } from "react";

// Open layers that take Escape, bottom to top. Only the top entry may handle Escape, so stacked dialogs and panels
// close one at a time. A higher priority always sits above a lower one regardless of open order, so a modal dialog is
// above every non-modal panel; within one priority the most recently opened is on top.
interface Entry {
  owner: object;
  priority: number;
}
const stack: Entry[] = [];

/** Priority a modal dialog registers with. Non-modal panels register at the default 0, below any open modal. */
export const MODAL_ESCAPE_PRIORITY = 1;

/** Register an owner on the stack; the returned function removes it. */
export function pushEscapeOwner(owner: object, priority = 0): () => void {
  let at = stack.length;
  while (at > 0 && stack[at - 1].priority > priority) at -= 1;
  const entry: Entry = { owner, priority };
  stack.splice(at, 0, entry);
  return () => {
    const i = stack.indexOf(entry);
    if (i >= 0) stack.splice(i, 1);
  };
}

/** True when `owner` is the topmost open layer. */
export function isTopEscapeOwner(owner: object): boolean {
  return stack.length > 0 && stack[stack.length - 1].owner === owner;
}

export interface EscapeOwnerOptions {
  /** Stack priority. Default 0 (below every open modal dialog). */
  priority?: number;
}

/**
 * Make a non-modal panel (find bar, tool panel, approval card, question card) an Escape owner while `active`. The
 * handler runs only for a plain Escape (not during IME composition) and only when this owner is the topmost; the
 * event is then consumed (default prevented, propagation stopped) unless the handler returns `false` to leave it
 * unconsumed. Owners opened later sit above earlier ones; an open `ModalShell` dialog is always above all of them.
 * It shares one ordered stack with `ModalShell`.
 */
export function useEscapeOwner(active: boolean, handler: (event: KeyboardEvent) => boolean | void, options: EscapeOwnerOptions = {}): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;
  const priority = options.priority ?? 0;
  useEffect(() => {
    if (!active) return;
    const owner = {};
    const remove = pushEscapeOwner(owner, priority);
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing || !isTopEscapeOwner(owner)) return;
      if (handlerRef.current(e) === false) return;
      e.stopPropagation();
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      remove();
    };
  }, [active, priority]);
}
