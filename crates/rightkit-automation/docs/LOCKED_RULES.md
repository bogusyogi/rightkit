# Locked rules carried from HeardRight

Source: HeardRight `7775b80d`, `docs/DESKTOP_PASTE_AND_MEDIA_SPEC.md` section 5 (AX/UIA implementation rules). Each rule came from a live failure. Where a shared decision exists it is called, never copied per platform. "Gate" names the assertion that locks it here.

| # | Rule (generic form) | Implementation | Gate |
|---|---|---|---|
| 1 | Bind only to the editor that was focused when the pin was captured. A later or different focus never becomes the destination. | `capture_pin` + `pin_is_current` (same process instance, same element) refuses delivery on any change: `Why::FocusChanged`. | `tests/e2e_mac.rs` ("a pin never follows focus to another editor"); `engine.rs::focus_change_after_pin_refuses_delivery` |
| 2 | A web-area container naming is the same place. | Not ported as a separate rule: `Platform::focused` already resolves a container to the editable editor. The bounded 40-hop / 400 ms ancestor walk (`FocusPin::editor_identity_is_self_or_ancestor`) stays a documented gap. | none |
| 3 | A replaced editor may receive text only when provably the same box (original positively gone on two reads, same process instance and window, positively non-secure, identical text fingerprint). | `proof::replacement_editor_may_receive`, `Automation::replacement_pin`, `FocusPin::fingerprint`. | `proof.rs::only_the_same_place_with_the_same_text_may_replace_the_original`; `engine.rs::a_replaced_editor_receives_text_only_when_provably_the_same_box`; e2e negative case |
| 4 | Stop-time recovery proves movement by continuity, never by a cached identity. | `proof::continuity_window_valid` (the pure decision). The native proofs stay with the observer that owns them (HeardRight). | `proof.rs` unit cases |
| 5 | Readback compares under one canonical line-break form; no other fuzzing. | `proof::collapse_line_break_runs`, applied to before, after and needle in `confirm_insertion`, on macOS and Windows alike. | `engine.rs::multi_paragraph_text_is_confirmed_under_canonical_line_breaks`; `e2e_mac.rs` multi-paragraph delivery into real TextEdit |

Mechanics (spec 5.4 / 5.5), as implemented:

* macOS: `CannotComplete` is retried inside the caller's deadline, `NoValue` is "unavailable", `InvalidUIElement` is "gone" (never reacquired as a different target), proof text is length-checked before conversion. Observers (AXObserver on a dedicated run loop) are **not** in this crate; polling reads are.
* Windows: ValuePattern/TextPattern reads are bounded (`MAX_PROOF_BYTES`); password controls are positively excluded and an unavailable `IsPassword` never becomes a blanket no-paste rule. UIA joins the MTA per thread (`ensure_com`); a single persistent MTA worker is a documented gap.
* Before-values, selection context and media titles are memory-only; receipts carry lengths and outcome codes. Credential-shaped text never reaches a receipt (`redact`).

Locked product rules that are **not generic** and therefore stay in HeardRight; a consumer that migrates must keep them:

* No TSF machine registration (Windows text services).
* One detokenization entry point.
* Pill window z-order `TOPMOST`.
* Dictation policy: start-capture gating by hotkey proof (rule 1's timing half), recording-delivery policy, stop-target selection, WezTerm pane binding, PTT chord windows.
