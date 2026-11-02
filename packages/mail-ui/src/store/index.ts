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
import { create, type StoreApi, type UseBoundStore } from "zustand";
import type { ComposeEditorHandle } from "../components/ComposeEditor.js";
import type { AppState } from "./types.js";
import { createFields } from "./fields.js";
import { uiActions } from "./actions/ui.js";
import { composeActions } from "./actions/compose.js";
import { mailActions } from "./actions/mail.js";

export { APP_OF, UNDO_TOAST_MS } from "./types.js";
export type { AppState, BulkAction, View } from "./types.js";

// Shared handle to the live TipTap compose editor (was an App-local `useRef`).
// A plain `{current}` object is a valid React ref; lifted here so ComposeView
// owns the editor while ReaderView / DriveView can insert into it. Mutating
// `.current` does not trigger re-renders — exactly ref semantics.
export const composeEditorRef: { current: ComposeEditorHandle | null } = { current: null };

// The store is created on first use, not when this module loads: its fields read preferences, which the host loads with
// initPreferences() before the first render. Importing the package (plain Node, a test, a host's module graph) therefore never
// reads preferences. The exported hook keeps zustand's whole API (call it as a hook, or getState/setState/subscribe).
type AppStore = UseBoundStore<StoreApi<AppState>>;
let store: AppStore | null = null;
const appStore = (): AppStore =>
  (store ??= create<AppState>((set, get) => ({
    ...createFields(set),
    ...uiActions(set, get),
    ...composeActions(get),
    ...mailActions(get),
  })));

export const useAppStore = new Proxy(function useAppStore() {} as unknown as AppStore, {
  apply: (_target, _this, args: Parameters<AppStore>) => (appStore() as (...a: unknown[]) => unknown)(...args),
  get: (_target, key) => Reflect.get(appStore(), key),
}) as AppStore;
