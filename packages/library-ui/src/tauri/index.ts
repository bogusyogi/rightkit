// @rightkit/library-ui/tauri: the Tauri `LibrarySource` for ScrapeRight's artifact library, the one place the
// list and reader contract meets Tauri. Pass it to the list and the reader:
//
//   import { LibraryScreen, LibraryReader } from "@rightkit/library-ui";
//   import { tauriLibrarySource, LIBRARY_SORT_VARIANTS } from "@rightkit/library-ui/tauri";
//   const source = tauriLibrarySource();
//   <LibraryScreen source={source} variants={LIBRARY_SORT_VARIANTS} ... />
//
// Dictation hosts take `deliveryLibrarySource`, the source over their delivery history, with their own command
// names, row mapping and wording.
export { LIBRARY_SORT_VARIANTS, tauriLibrarySource, type TauriLibraryCommands, type TauriLibraryOptions } from "./tauriLibrarySource.js";
export {
  deliveryLibrarySource,
  type DeliveryHistoryPage,
  type DeliveryHistoryQuery,
  type DeliveryLibraryCommands,
  type DeliveryLibrarySourceOptions,
  type DeliveryRecoveryStatus,
  type SaveFileFilter,
} from "./deliveryLibrarySource.js";
