import { useEffect, useRef, useState } from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { acquireMarkdownResource } from "./markdownResourceLifecycle";

export interface OutlineHeading {
  depth: number;
  text: string;
  id: string;
}

/** Floating document outline with scroll-spy. Toggled; highlights the section in view. */
export function Outline({ headings }: { headings: OutlineHeading[] }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState<string | null>(null);
  const shellRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const els = headings
      .map((h) => document.getElementById(h.id))
      .filter((e): e is HTMLElement => !!e);
    if (els.length === 0) return;
    const obs = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) setActive(visible[0].target.id);
      },
      { rootMargin: "0px 0px -70% 0px" },
    );
    const release = acquireMarkdownResource("observer");
    els.forEach((el) => obs.observe(el));
    return () => {
      obs.disconnect();
      release();
    };
  }, [open, headings]);

  // A press outside the shell closes it.
  useDismiss(() => setOpen(false), {
    enabled: open,
    escape: false,
    container: shellRef,
    outside: "mousedown",
  });

  // Escape on the document in the capture phase (below a modal's window-capture Escape, above element
  // handlers): the capture listener delegates to the public `registry.handle`, which matches Escape
  // with any modifiers, in a text field, during IME composition and when already prevented. The owner's
  // `useDismiss` reads Escape only on a window capture or an element bubble, neither of which keeps this
  // ordering, so the listener stays and the matching stays the registry's.
  useEffect(() => {
    if (!open) return;
    const shortcuts = createShortcutRegistry();
    shortcuts.register({
      id: "outline.close",
      label: "Close outline",
      chord: "escape",
      hidden: true,
      ignoreExtraModifiers: true,
      allowInInput: true,
      allowComposing: true,
      allowDefaultPrevented: true,
      preventDefault: false,
      run: () => setOpen(false),
    });
    const onKeyDown = (event: KeyboardEvent) => {
      shortcuts.handle(event);
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [open]);

  if (headings.length < 2) return null;

  // Sticky inside the document scroll area (not viewport-fixed) so it sits just below the header
  // regardless of its height. Keep it on the left so it behaves like the PDF/DOCX navigation affordance.
  return (
    <div
      ref={shellRef}
      className="no-print sticky top-0 z-20 mr-auto flex w-fit flex-col items-start pl-4 pt-2"
    >
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        title="Outline"
        className="rounded border border-line bg-panel px-2 py-1 font-mono text-chrome-xs text-muted hover:border-copper hover:text-copper-hi"
      >
        ☰ outline
      </button>
      {open && (
        <nav className="mt-1 max-h-[70vh] w-60 overflow-auto rounded-lg border border-line bg-panel-2 p-3 text-sm shadow-2xl">
          {headings.map((h) => (
            <a
              key={h.id}
              href={`#${h.id}`}
              onClick={() => setOpen(false)}
              className={`block truncate py-0.5 no-underline ${active === h.id ? "text-copper-hi" : "text-muted hover:text-paper"}`}
              style={{ paddingLeft: (h.depth - 1) * 12 }}
            >
              {h.text}
            </a>
          ))}
        </nav>
      )}
    </div>
  );
}
