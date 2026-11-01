import { useEffect } from "react";
import { createShortcutRegistry } from "@rightkit/react-shell";

/**
 * Escape closes the slash menu and returns focus to the editor. It is read on the document in the
 * capture phase (so a modal that takes it in the window capture phase wins, and it runs above element
 * handlers): the capture listener delegates to the public `registry.handle`, which matches Escape with
 * any modifiers, in a text field, during IME composition and when already prevented. A handled Escape
 * stops propagation, so only this top layer closes (a link popover underneath keeps its own Escape).
 */
export function useSlashMenuEscape(open: boolean, close: (refocus: boolean) => void): void {
  useEffect(() => {
    if (!open) return;
    const shortcuts = createShortcutRegistry();
    shortcuts.register({
      id: "markdown-slash.close",
      label: "Close slash menu",
      chord: "escape",
      hidden: true,
      ignoreExtraModifiers: true,
      allowInInput: true,
      allowComposing: true,
      allowDefaultPrevented: true,
      preventDefault: false,
      run: () => {
        close(true);
      },
    });
    const onKeyDown = (event: KeyboardEvent) => {
      if (shortcuts.handle(event)) event.stopPropagation();
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [open, close]);
}
