// @rightkit/library-ui/tauri: the Tauri `LibrarySource` for ScrapeRight's artifact library, the one place the
// list and reader contract meets Tauri. Pass it to the list and the reader:
//
//   import { LibraryScreen, LibraryReader } from "@rightkit/library-ui";
//   import { tauriLibrarySource, LIBRARY_SORT_VARIANTS } from "@rightkit/library-ui/tauri";
//   const source = tauriLibrarySource();
//   <LibraryScreen source={source} variants={LIBRARY_SORT_VARIANTS} ... />
//
// HeardRight's history is the `history` source of `@rightkit/dictation-ui/tauri`; it is not repeated here.
export { LIBRARY_SORT_VARIANTS, tauriLibrarySource, type TauriLibraryCommands, type TauriLibraryOptions } from "./tauriLibrarySource.js";
