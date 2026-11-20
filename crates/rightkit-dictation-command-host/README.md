# rightkit-dictation-command-host

Dictation command host: voice command dispatch (keys, app launch, Shortcuts, power with confirmation), text delivery and insertion, focus checks and screen capture for a dictation app.

Moved into RightKit from `heardright-command-host` (tauri-app-next) on 2026-11-01.

## Gated delivery (0.3.0)

Use `paste_delivery::RealDeliveryBackend::new(&DELIVERY_HOST)` for automatic paste & optional submit. `DeliveryHost` requires explicit probe budgets, clipboard timing, process lists, terminal-pane security policy & macOS `AppTargetPolicy`; host supplies its settings & event hooks. Stop-time capture APIs retain control identity across transcription. `paste_delivery::paste_text` takes host policy for repaste.

`streaming::StreamingHost` supplies security & control probes, budgets, terminal process list & application policy. Every chunk, erase & Return requires fresh security, control & physical-modifier evidence. Handle `ReconcileOutcome` before final delivery: only `NothingWritten` or `Reconciled` permits automatic delivery; retained or uncertain writes require manual recovery.

`streaming::StreamingHost::focused_caret` (Windows UIA TextPattern caret and selection) lets streaming prove, for the bound control, that the selection is collapsed and the streamed text sits exactly before the caret before any erase or completion, and that an erase took effect at that position. A control or platform that cannot give a caret (macOS) is never streamed into; a failed proof leaves the typed text (`StoppedWritesRemain` / `StoppedCountUncertain`).

Legacy `delivery::{deliver_text, deliver_text_with_enter, paste_text}` signatures remain available. Without host policy they refuse automatic input with `delivery_host_required`, leaving clipboard untouched. Delivery record APIs return `CopiedFallback`; `paste_text` returns `E_DELIVERY_HOST_REQUIRED`. Hosts must migrate these calls to `paste_delivery` to resume automatic delivery. Explicit `delivery::copy_text` remains available for user-requested recovery.

Clipboard restoration retains generation & OS ownership checks. A paste refused after receipt marking leaves staged text recoverable & reports clipboard custody through existing fallback events. Permit expiry is checked again after receipt marking, immediately before input is sent.

Last-paste transforms through `command_dispatch::{dispatch, dispatch_with_last_text}` refuse with `E_DELIVERY_HOST_REQUIRED` before Undo. Empty previous text remains a successful no-op; unknown transforms retain `E_BAD_TRANSFORM`. `host::configure(HostIdentity)` & `set_paste_owner_name` configure identity only. Transform support needs a host-bound replacement transaction that accepts `DeliveryHost`, retains original & transformed text for recovery before Undo, binds one control through Undo & paste, gates both effects with fresh security & modifier evidence, & reports whether original text remains, replacement landed or content is uncertain. Configuring ordinary paste alone does not enable this transaction.

For runtime repaste that needs a `DeliveryRecord`, construct `RealDeliveryBackend` with host policy & call `deliver_submitting(transcript, false)`. Inspect its record outcome & `paste_delivery::clipboard_preserved(&record)` before reporting success or copied text. For a shell action that needs `Result<(), DeliveryError>`, call `paste_delivery::paste_text(host, transcript)`; this takes explicit policy & captures current destination. Stop-bound delivery instead uses `deliver_stop_target_with_progress` on Windows or `deliver_stop_submitting_with_progress` with retained AX pin on macOS.
