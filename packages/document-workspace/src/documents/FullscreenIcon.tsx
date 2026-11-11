import { ArrowsIn } from "@phosphor-icons/react/dist/icons/ArrowsIn";
import { ArrowsOut } from "@phosphor-icons/react/dist/icons/ArrowsOut";

/** Fullscreen enter/exit glyph — shared by the HTML, Markdown, and MDX source-drawer toggles. */
export function FullscreenIcon({ restore }: { restore: boolean }) {
  return restore ? (
    <ArrowsIn size={14} weight="regular" aria-hidden="true" />
  ) : (
    <ArrowsOut size={14} weight="regular" aria-hidden="true" />
  );
}
