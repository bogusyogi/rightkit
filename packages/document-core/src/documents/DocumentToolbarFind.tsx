import type { ReactNode } from "react";

export type DocumentSearchMode = "find" | "replace";

/**
 * Where the find overlay sits relative to its positioned ancestor.
 *
 * `below` (default) hangs it under the toolbar. That works for every format whose toolbar is a slim
 * single row — HTML, MDX, TXT, PDF, DOCX — because "under the toolbar" and "near the top of the
 * document" are the same place.
 *
 * `beside` pins it to the top instead. Markdown's toolbar is a tall, wrapping cluster of button
 * groups, so anchoring below it pushed find far down the page (and further still whenever the
 * toolbar wrapped to another row). Markdown's toolbar is `w-fit`, leaving free space to its right,
 * so find belongs in that space — level with the toolbar, immune to its height.
 */
export type DocumentSearchAlign = "below" | "beside";

export function DocumentToolbarSearch({
  findBar,
  align = "below",
}: {
  findBar?: ReactNode;
  align?: DocumentSearchAlign;
  activeMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
}) {
  if (!findBar) return null;

  return (
    <div
      data-vr-toolbar-find=""
      data-vr-find-align={align}
      className={
        align === "beside"
          ? "no-print vr-chrome absolute right-3 top-3 z-50 min-w-0 max-w-[calc(100%-1.5rem)]"
          : "no-print vr-chrome absolute right-3 top-full z-50 mt-2 min-w-0 max-w-[calc(100%-1.5rem)]"
      }
    >
      {findBar}
    </div>
  );
}
