// pdf.js publishes `AnnotationEditorUIManager`'s two viewer-component dependencies (EventBus,
// GenericL10n) only from "pdfjs-dist/web/pdf_viewer.mjs" — and that module's own top-level code
// destructures `globalThis.pdfjsLib` (Mozilla's own viewer bootstrap sets this global before
// loading it as a second <script>; a pure-ESM consumer has no such bootstrap). Importing
// pdf_viewer.mjs without this throws at *module load*, not at use — "Cannot destructure property
// 'AbortException' of 'globalThis.pdfjsLib' as it is undefined."
//
// Import this module (for its side effect only, before any "pdfjs-dist/web/pdf_viewer.mjs"
// import) to wire that global. ES module evaluation is depth-first in source order, so as long as
// this import line comes first, the global is set before pdf_viewer.mjs's body runs.
import * as pdfjs from "pdfjs-dist";

(globalThis as unknown as { pdfjsLib?: typeof pdfjs }).pdfjsLib = pdfjs;
