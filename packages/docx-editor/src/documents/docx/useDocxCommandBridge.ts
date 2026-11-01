import type { DocxHostToolbarContext } from "./eigenpalReact.js";

type FormatAction = Parameters<DocxHostToolbarContext["format"]>[0];

/** Maps the host app toolbar/menu intents onto Eigenpal's bridged command handlers.
 *  Eigenpal still owns the deep DOCX/ProseMirror execution + selection restoration;
 *  this hook is purely the host command map. Returned values are plain command
 *  functions — the ui.tsx primitives wrap them in focus-safe onMouseDown handlers. */
export function useDocxCommandBridge(ctx: DocxHostToolbarContext) {
  const clampZoom = (z: number) => Math.min(2, Math.max(0.5, Number(z.toFixed(2))));
  return {
    /** A format action (toggle, list, alignment, font, size, color, style…). */
    fmt: (action: FormatAction) => () => ctx.format(action),
    undo: () => ctx.undo(),
    redo: () => ctx.redo(),
    save: () => ctx.save(),
    print: () => ctx.print(),
    pageSetup: () => ctx.pageSetup(),
    zoomIn: () => ctx.setZoom(clampZoom(ctx.zoom + 0.1)),
    zoomOut: () => ctx.setZoom(clampZoom(ctx.zoom - 0.1)),
    resetZoom: () => ctx.setZoom(1),
    insertTable: (rows: number, cols: number) => () => ctx.insertTable(rows, cols),
    insertImage: () => ctx.insertImage(),
    insertPageBreak: () => ctx.insertPageBreak(),
    insertTOC: () => ctx.insertTOC(),
  };
}
