# @rightkit/dictation-ui

Dictation behavior for React hosts. The host renders each window, supplies its own copy, assets & styles, and binds native commands to its client.

## Entry points

| Import | Host integration |
| --- | --- |
| `@rightkit/dictation-ui` | Configure dictation clients per window, subscribe to events, and use microphone, history and settings hooks from host-rendered views. |
| `@rightkit/dictation-ui/pill` | Use the recording and popup controllers directly or pass a render function as a child of their components. The host supplies markup, accessibility labels and styles. |
| `@rightkit/dictation-ui/onboarding` | Use microphone recording and speech-recognition setup in the host's chosen steps. |
| `@rightkit/dictation-ui/settings` | Implement the speech settings client used by the host's settings sections. |

The main entry also exports pure model-preparation and feature-reel state helpers, onboarding model-progress helpers, paged history query hooks/types, `createTauriDictationClient`, and the host-configured single-flight model-update store. The Tauri client takes host-provided `invoke`/`listen` functions plus command/event maps; it imports no native transport.

## Wiring the recording pill

Attach `wrapRef` to the overlay wrapper, `pillRef` to the pill & optional `compactAnchorRef` to its compact measurement target. Apply `focusProps` to the focus container. Mark the interactive surface with `data-pill-surface` & its actions with `data-pill-action` so pointer and keyboard interaction reach the controller.

Attach each popup's `cardRef` to its rendered content & the clipboard editor's `fieldRef` to its textarea. Supply window styles, accessible labels & animation rendering in the host. Call `setIntro(false)` when the host's intro finishes.

## Migrating from 0.2.7

The 0.3 API removes the packaged home, history, vocabulary, polish and transcription screens; settings panels; first-run steps; shell components; and stylesheets. Compose the app shell, settings and onboarding directly with their mandatory RightKit packages. Keep the product's page order, layout, labels and theme in the app.

Replace the former Tauri entry point with host-provided command and event bindings. Consumers of the pill supply their own rendering over the recording and popup behavior. Keep the operating-system permission checks and native window policy with the appropriate host bindings.

Existing releases remain independently versioned. Upgrade a consumer only after it supplies its presentation and verifies the affected user journeys.
