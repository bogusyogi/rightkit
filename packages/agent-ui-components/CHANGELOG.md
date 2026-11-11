# Changelog

## 0.1.1 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/dictation-library-ui-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

- The voice conversation loop moved here from `@rightkit/dictation-ui` 0.1.0 (`src/voice`), its only user: `VoiceConversation` (`components/voice`), `useVoiceConversation` and `useMicRecorder` (`hooks/voice`), exported from the root. Its copy of the icon set is dropped; it uses `@rightkit/agent-ui-core`'s `Icon`.
- The dependency on `@rightkit/dictation-ui` is removed.

## 0.1.0

- First release. Coding-agent UI components: app frame, chat, composer, review, panels, delivery, remote and usage.
