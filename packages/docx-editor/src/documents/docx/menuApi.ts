import type { DocxHostToolbarContext } from "./eigenpalReact.js";

type FormatAction = Parameters<DocxHostToolbarContext["format"]>[0];

/** Stable command surface DocxPane lifts up to the app chrome (DocActionBar) so the
 *  File/Format/Insert/Help menus can drive the editor without the host app reaching into
 *  Eigenpal. Methods are lazy — they read the live bridge context at call time. */
export interface DocxMenuApi {
  save: () => void;
  print: () => void;
  pageSetup: () => void;
  format: (action: FormatAction) => void;
  insertTable: (rows: number, cols: number) => void;
  insertImage: () => void;
  insertPageBreak: () => void;
  insertTOC: () => void;
}
