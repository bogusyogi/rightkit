// Central app store. Field setters are generated from the field object and accept
// a value OR updater function. Views subscribe to only the slices they read via
// selectors, so a change to (say) calendar state doesn't re-render the inbox list.
//
// The store is assembled from one field builder + three domain action slices:
//   - fields.ts        — every data field; setter names are derived from keys
//                         (preference-backed initializers run once)
//   - actions/ui.ts    — undo toast, busy runner, theme/font, hidden labels
//   - actions/compose.ts — signature/alias warm-up + opening the compose drawer
//   - actions/mail.ts  — list loading, thread open, and thread mutations
// Keeping fields in one place guarantees each is declared exactly once; the
// StoreActionKey + Pick/Omit return types make tsc fail if an action is dropped
// or duplicated wrong.
import { create } from "zustand";
import type { ComposeEditorHandle } from "../components/ComposeEditor";
import type { AppState } from "./types";
import { createFields } from "./fields";
import { uiActions } from "./actions/ui";
import { composeActions } from "./actions/compose";
import { mailActions } from "./actions/mail";

export { APP_OF, UNDO_TOAST_MS } from "./types";
export type { AppState, BulkAction, View } from "./types";

// Shared handle to the live TipTap compose editor (was an App-local `useRef`).
// A plain `{current}` object is a valid React ref; lifted here so ComposeView
// owns the editor while ReaderView / DriveView can insert into it. Mutating
// `.current` does not trigger re-renders — exactly ref semantics.
export const composeEditorRef: { current: ComposeEditorHandle | null } = { current: null };

export const useAppStore = create<AppState>((set, get) => ({
  ...createFields(set),
  ...uiActions(set, get),
  ...composeActions(get),
  ...mailActions(get),
}));
