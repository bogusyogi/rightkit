# Changelog

## 0.1.2

- `BackgroundTasksPanel` names the host app in the isolation badge (it printed the literal "${hostAppName()} gateway"). The attention panel's "history preserved" is the `attentionHistoryLabel` host option; the sidebar's "Session options" panel renders when `newSessionOptionsPanel` is on (defaults: "history preserved", off).
- Polls (browser pane, terminal processes and logs, orchestration settings) use `setVisibleInterval`. `useAppEffects` takes an optional `pendingQuestion` and exports `shouldInterruptOnEscape`: Escape does not interrupt a turn while a question is pending (unset keeps the old behaviour).
- `VirtualSessionList` and the shell header title use `sessionDisplayTitle`. The project picker records the open project as a recent one; the environments panel names kinds the runtime cannot run yet in one line; the transcript follows content growth while stuck to the bottom; the conductor selector shows the session's own model; the transcript reserves the minimap strip; the composer shows a model selected through a config route. `appName` re-exports `@rightkit/agent-ui-core/appName`.

## 0.1.1 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/dictation-library-ui-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

- The voice conversation loop moved here from `@rightkit/dictation-ui` 0.1.0 (`src/voice`), its only user: `VoiceConversation` (`components/voice`), `useVoiceConversation` and `useMicRecorder` (`hooks/voice`), exported from the root. Its copy of the icon set is dropped; it uses `@rightkit/agent-ui-core`'s `Icon`.
- The dependency on `@rightkit/dictation-ui` is removed.

## 0.1.0

- First release. Coding-agent UI components: app frame, chat, composer, review, panels, delivery, remote and usage.
