# HeardRight transcript delivery: read-back in the shipped builds, and a design for delivery inside `rightkit-automation`

Read-only study. Nothing was compiled, run, committed or pushed. "Verified" below always means verified by reading the code at the cited lines, never by execution. "Suspicion" means inferred and not traced to the end.

Trees (S = `<scratch>`):

- WIN-LIVE = `S/survey/heardright-win-live/tauri-app-next` (0.1.162, shipped on Windows)
- MAC-LIVE = `S/survey/heardright-live/tauri-app-next` (0.1.213, shipped on macOS)
- MAIN = `S/survey/heardright-main/tauri-app-next` (0.1.231, unreleased)
- RK = `/home/user/rightkit/crates`

The delivery spec the code comments cite (`docs/DESKTOP_PASTE_AND_MEDIA_SPEC.md`) is not inside any of the extracted `tauri-app-next` trees, so "spec section N" references are quoted from code comments only.

---

## Part 1. Read-back

### 1.0 Headline

The two shipped builds do **not** share one read-back rule, so "take the rule from the shipped builds" has two answers:

- **Windows 0.1.162**: the clipboard receipt is the proof. UIA read-back is computed and thrown away (telemetry only). It gates nothing.
- **macOS 0.1.213**: receipt **and** exact AX read-back are both required. A target that cannot be read is never reported as pasted and never gets Enter.
- **MAIN** is receipt-first on both systems (read-back is only a second chance when no receipt came). For success this equals shipped Windows and is looser than shipped macOS. The change is commented as deliberate.

### 1.1 Table

| Build | Delivery reported as success when | Enter sent when | Read-back unavailable |
|---|---|---|---|
| **Windows 0.1.162 (live)** | Ctrl+V sent and a `WM_RENDERFORMAT` receipt arrives within 1,500 ms. Read-back plays no part. (Exception: an empty, non-Chromium, non-terminal Value-pattern field is written natively first, and that path does require read-back.) | After the receipt: same window still foreground, field positively not a password (`Some(false)`), UIA identity not seen to move. No read-back, no settle wait. | No effect; read-back is not consulted. With no receipt: "Copied" fallback, transcript rewritten to the clipboard. No allowlist. |
| **macOS 0.1.213 (live)** | Cmd+V sent, AppKit promise receipt within 1,500 ms, **and** exact AX read-back of the pinned editor confirmed within 650 ms. | Only with confirmed read-back, and the proven editor still current and positively non-secure. | Never success. Outcome "Paste sent, unconfirmed" (`paste_readback_unavailable`), clipboard recovery card shown, transcript left staged on the clipboard, no Enter. No allowlist, no fallback rule. |
| **MAIN, macOS** | Cmd+V sent and (receipt, or target key event seen and read-back confirmed). | After success: wait up to 400 ms for read-back to show the text (60 ms if unreadable), then Return if the proven editor is still current and positively non-secure. Read-back is not required. | With a receipt: "Pasted". Without a receipt: "Paste sent, unconfirmed" plus recovery card. |
| **MAIN, Windows** | Ctrl+V sent and (receipt or attributable read-back), raced against one deadline (1,500 ms, 2,500 ms for terminals). | After success: same settle wait (400 ms / 60 ms), then Enter if window still current, positively not a password, identity not moved. Never for a terminal reported as "unknown". | With a receipt: "Pasted". Terminal with no receipt and no read-back channel: reported "Pasted" after 300 ms, no Enter, clipboard not restored. Otherwise "Paste sent, unconfirmed". |

### 1.2 Windows 0.1.162 (live), file `WIN-LIVE/src-tauri/src/delivery_sections/section01.rs`

All verified.

a. **Read-back performed.**
- Paste path: a UIA text snapshot is taken before the chord (`:1255-1257`) and after the receipt (`:1315-1322`), compared with `insertion_attributable`, and the result is bound to an unused variable `_uia_readback_confirmed`. The comments say so: "UIA readback is retained as non-authoritative telemetry for diagnostics; it never gates Ctrl+V, Enter, or clipboard restoration" (`:1251-1254`) and "accessibility read-back is advisory only" (`:193-194`).
- Native path, tried first: `try_native_insertion_windows` (`:644-758`) runs only when the focused element has a writable, **empty** `ValuePattern`, is not a password, its framework is not `Chrome`/`InternetExplorer`, and the process is not on the terminal list (`:766-794`, `:864-908`). It calls `SetValue` and then requires a before/after read-back (`:715-725`). An unconfirmed native write returns `native_insertion_unconfirmed` and is **not** followed by a paste (`:1189-1192`).

b. **Success.** `insert_transcript` returns `Ok` and the caller maps that to `DeliveryOutcome::Pasted` (`:136-139`). For the paste path that needs: password probe `Some(false)` (`:1158-1166`, `:1264-1272`; an unknown state refuses with `secure_target_unverified`), clipboard snapshot and promise staged (`:1198-1223`), window-level target check (`:1224`, `:1276`), UIA identity not moved (`:1280`), chord sent (`:1286`), and `promised.wait_for_receipt(1_500 ms)` observed (`:1298-1313`, constant `:196`).

c. **Enter.** `:1327-1353`: `send_enter` set, `target_ready_for_input` (same pid and stable window, `heardright_core/src/delivery.rs:288-300`), `windows_focused_field_is_password() == Some(false)`, and `!windows_identity_moved(..)`. `windows_identity_moved` returns false when either probe is `None` (`:929-951`), so Chromium and terminal targets pass on the window check alone. No read-back and no wait between receipt and Enter.

d. **Read-back unavailable.** Irrelevant to the outcome. The only failure signal is a missing receipt: `paste_receipt_unconfirmed` (`:1307-1313`) becomes `CopiedFallback` and the caller rewrites the transcript to the clipboard (`:140-152`). There is no `PasteSentUnconfirmed` outcome in this build (`heardright_core/src/delivery.rs:72-95`). No allowlist of apps exists; the terminal process/class/title list (`:1391-1419`) is used only to keep terminals off the native-write path and to classify the focused field as text input. The comment at `:1228-1240` records why UIA identity may not gate the paste: 0.1.157 treated a missing identity as focus change and "pasted nothing on Windows at all".

Other facts that matter for the design (verified):
- The destination is the **live** foreground target at delivery time (`:84`), not a stop-time pin. `execute_targeted_delivery` (`heardright_core/src/delivery.rs:313-358`) only restores the recording-start window when the stop came from the app's own pill, and that restore calls `SetForegroundWindow` (`delivery_sections/section02.rs:48-80`). So this build does activate a target in that one case.
- Clipboard restore is inline, 200 ms after the last receipt (`:1360-1373`).
- No WezTerm binding, no persistent UIA worker, no paste probe in this tree (searched). Every UIA probe spawns its own thread with a 600 ms cap.

### 1.3 macOS 0.1.213 (live), file `MAC-LIVE/src-tauri/src/delivery/backend.rs`

All verified unless marked.

a. **Read-back performed.** Before the chord a `FocusedFieldProof` baseline is taken from the retained pin (`:661-671`). After the receipt the code polls `insertion_confirmation_for_pin` (or `insertion_confirmed_for_pid` without a pin) every 5 ms for up to 650 ms (`:810-850`, constant `:335`). A CGEventTap probe records whether the tagged Cmd+V reached the target pid (`:692-712`, `:738-772`); in this build it is telemetry only ("Target event/clipboard receipt prove dispatch and consumption only. Only exact editor readback may clear recovery or restore the clipboard", `:855-857`).

b. **Success.** Receipt observed (`:802-809`) **and** `readback_confirmed` (`:857`, `:872-884`). Preconditions: retained pin current (`:546-556`), Accessibility trusted (`:559-565`), not positively secure (`:574-584`, `:633-648`; unknown does not refuse), target ready checks around every yield (`:625-631`, `:649-655`, `:687-690`, `:715-718`).

c. **Enter.** `if send_enter && readback_confirmed` (`:898`), then `proof_destination_is_current` and secure state `Some(false)` (`:919-922`). The comment: "A receipt on its own is never sufficient to submit" (`:893-897`).

d. **Read-back unavailable.** A blind proof (`is_blind`: no AX value and no text-marker value, `heardright_platform/src/macos/focus/proof.rs:89-91`) makes `readback_confirmed` false, so the function returns `paste_readback_unavailable` (`:872-884`). The caller maps `paste_readback_unconfirmed`, `paste_receipt_unconfirmed`, `paste_readback_unavailable` and `native_insertion_unconfirmed` to `DeliveryOutcome::PasteSentUnconfirmed` (`:261-271`). The pill shows "Paste sent, unconfirmed" (`src-tauri/src/dictation_lifecycle.rs:716-718`) and the clipboard recovery card is raised (`:669-686`). The promise is returned without `restore_if_unchanged`, so the transcript stays on the clipboard. The keystroke has already gone out, so the text is normally in the app while the pill says unconfirmed. No allowlist, no receipt-only path.
- Suspicion: the code comment names "terminal grids and opaque web editors" as the blind cases (`:657-660`), but `proof.rs` has no terminal special case (searched `focus/*.rs`; the terminal bundle list at `focus/context.rs:624-636` is used for context and spacing only). A terminal that exposes its scrollback as `AXValue` would be treated as readable. Not traced further.

Other facts (verified):
- No native AX write before the paste ("Single dispatch-only path ... No native AX write precedes it", `:594-598`); the `keystroke` method is retired (`:586-593`).
- The backend never activates anything. The session layer may refocus the recording-start editor when the transcript is bound to it and its content is provably unchanged: `restore_focus_pin` (`src-tauri/src/runtime/session.rs:976-981`), which calls `restore_pid` and sets `AXMain`/`AXFocused` (`heardright_platform/src/macos/frontmost.rs:175-213`). So "no target activation" is true of the paste transaction, not of the whole delivery.
- The same tree's Windows branch (never shipped on Windows) was also strict: 40 ms receipt wait (`:332`, `:1109-1111`), then read-back required (`:1130-1166`), Enter only with read-back (`:1178`). So the history is: Windows receipt-only (0.1.162), then strict on both (0.1.213), then receipt-first on both (MAIN).

### 1.4 MAIN, file `MAIN/src-tauri/src/delivery/backend.rs`

All verified unless marked.

**macOS** (`insert_transcript`, `:645-1188`)

a. Baseline proof as before (`:787-799`). Read-back after the chord runs **only** when no receipt arrived and the paste probe saw the key event (`:950-989`). After success, a second bounded read-back wait runs only to time Enter (`:1054-1095`).

b. Success = `paste_outcome(true, receipt, target_event, readback)` (`:1008-1025`), defined in `RK/rightkit-transcript-library/src/delivery.rs:943-959`: keystroke sent and (receipt, or target event seen and read-back confirmed).

c. Enter (`:1040-1131`): if read-back has not already confirmed, wait up to `ENTER_INSERTION_SETTLE_MS` = 400 ms for it (`:378`), or sleep `ENTER_BLIND_SETTLE_MS` = 60 ms when there is no readable baseline (`:382`, `:1083-1086`). Then Return is sent when `proof_destination_is_current` and the secure state is `Some(false)` (`:1109-1113`), whether or not the wait saw the text.
- Suspicion worth an owner look: the comment at `:1038-1039` says "blind targets paste without Return", but nothing in the code checks `is_blind` before Return. `proof_destination_is_current` (`MAIN/heardright_platform/src/macos/focus/proof.rs:363-411`) needs a resolved focused element, the same window and a known non-secure role; a blind proof still carries an owner element (`:203-248`) and the role table counts `AXTextArea`, `AXWebArea`, `AXGroup`, `AXWindow` as known non-secure (`focus/context.rs:318-346`). So an unreadable editor that still resolves as the focused element appears to get Return after 60 ms. Read, not run.

d. Unavailable read-back no longer matters when a receipt exists. With no receipt: `paste_receipt_unconfirmed` becomes `PasteSentUnconfirmed` (`:271-278`).

**Windows** (`insert_transcript`, `:1191-1719`)

a. Baseline immediately before the chord from the stop target (UIA retained element, or WezTerm pane through a 150 ms probe) or the live identity (`:1332-1359`), filtered by `windows_text_readback_is_usable` (`:1773-1790`, rejects the VS Code terminal's static label). After the chord, receipt and read-back are raced in one loop (`:1403-1465`).

b. Success = `paste_outcome(true, receipt, true, readback)` (`:1504`); "Windows has no key-event probe; the keystroke send itself is the event evidence" (`:1501-1502`). Extra case: a terminal with no baseline (`terminal_paste_has_no_confirm_channel`, `:1421-1422`, rule in `RK/rightkit-transcript-library/src/delivery.rs:970-975`) waits 300 ms for a receipt and is then reported as pasted with `paste_unknown = true` (`:1505-1521`).

c. Enter (`:1546-1631`): never when `paste_unknown` (`:1592-1597`); otherwise the same 400 ms / 60 ms settle (`:1551-1590`), then window still current, password state `Some(false)`, identity not moved (`:1599-1627`).

d. Covered by the receipt. Lossy clipboard snapshot refuses the whole transaction and does not write the fallback copy (`:1227-1249`, `:279-287`). No allowlist.

### 1.5 Differences

**Between the two live builds** (all verified)

| Point | Windows 0.1.162 | macOS 0.1.213 |
|---|---|---|
| Success proof | Receipt only | Receipt and read-back |
| Read-back | Telemetry, discarded | Required |
| Enter | After receipt, no read-back | After read-back |
| Unreadable target | Normal success | "Paste sent, unconfirmed", recovery card |
| No receipt | "Copied" fallback, clipboard rewritten | "Paste sent, unconfirmed", promise left staged |
| Native write first | Yes, for an empty native field | No, forbidden by comment |
| Unknown secure state | Refuses (`secure_target_unverified`) | Does not refuse (positive evidence only) |
| Destination | Live foreground at delivery time | Stop-time retained pin, revalidated |
| Paste event probe | None | CGEventTap, telemetry only |

**MAIN against the live builds**, with whether the code explains the change

| # | MAIN difference | Against | Explained as deliberate in code? |
|---|---|---|---|
| 1 | Success is receipt-first; read-back is a second chance only | macOS live (looser); Windows live (same, plus read-back as an alternative) | Yes. `backend.rs:943-946`; `paste_outcome` doc: "No shipping dictation app gates success on AX/UIA readback" (`RK/rightkit-transcript-library/src/delivery.rs:938-942`); Windows race comment `:1403-1409` |
| 2 | Enter no longer needs read-back; bounded settle wait instead | macOS live (looser); Windows live (same gate, adds the wait) | Yes. `:1051-1053`, `:1547-1550`, constants `:375-382` |
| 3 | Return may reach an unreadable macOS editor | macOS live (never) | **No, and the comment says the opposite** (`:1038-1039`). Suspicion, see 1.4 |
| 4 | Clipboard restore on a background thread with a quiet window; next delivery waits for it | Both (inline) | Yes. `:384-397`, `:1132-1136`, `:1633-1638` |
| 5 | Final ready-and-not-secure gate just before the chord | Both | Yes. `paste_dispatch_gate`, "issue #148" (`:530-546`) |
| 6 | No native UIA `SetValue` first | Windows live | Yes. `:1225-1226` ("Single dispatch-only path (spec section 4)") |
| 7 | Unknown password state no longer refuses a paste | Windows live | Yes. `:1219-1220` ("spec 3.3") |
| 8 | Lossy clipboard snapshot refuses the transaction | Windows live | Yes. `:1227-1230`, `:279-287` |
| 9 | Stop-time destination (`WindowsStopTarget`); no pin means clipboard fallback | Windows live (live target) | Partly. The type and its comments are explicit (`delivery/targets.rs:59-72`); the fallback is at `runtime/session.rs:861-869` with no note about the behaviour change |
| 10 | Terminal with no confirm channel is reported pasted after 300 ms | Windows live (1.5 s then "Copied") | Yes. `:363-367`, `:1419-1420`, `:1506-1509` |
| 11 | WezTerm pane binding and pre-paste probe; secure probe skipped for WezTerm | Windows live (none) | Yes. `:357-361`, `:1210-1212`, `:1337-1341` |
| 12 | Paste chord waits for the hotkey's physical modifiers to lift | Windows live | Yes. `:1382-1383` |
| 13 | Missing receipt is "Paste sent, unconfirmed", promise left staged | Windows live ("Copied", plain rewrite) | Outcome mapping is explicit (`:271-278`); no note on the change |

One more inconsistency inside MAIN (verified at the cited lines, the Windows half of that file not read in full): the second delivery path, `heardright-command-host/src/delivery/insert.rs`, used for repaste and by the engine process (`heardright-runtime/src/runtime/delivery_history.rs:12`), still applies the strict macOS rule: receipt required (`:512-518`), then read-back required (`:521-570`). So MAIN has two different success rules depending on which path delivers.

---

## Part 2. Delivery inside `rightkit-automation`, no duplicate code

### 2a. Inventory of MAIN files that take part in delivery

"RK equivalent" paths are under `RK/rightkit-automation/src` unless another crate is named. Line counts are from `wc -l`.

| MAIN file | Lines | RK equivalent today | Kind |
|---|---|---|---|
| `src-tauri/src/delivery/backend.rs` | 1,906 | None for the transaction. `delivery.rs` (675) is a different algorithm. Pure rules it calls are in `rightkit-transcript-library/src/delivery.rs` | Mechanism, with the rule choices listed in 2b |
| `src-tauri/src/delivery/targets.rs` | 1,258 | None (`desktop` has the primitives it calls) | Mechanism (capture, validate, restore) plus stop-target policy |
| `src-tauri/src/delivery.rs` | 10 | n/a | Module glue |
| `src-tauri/src/windows_uia.rs`, `windows_uia/{api,protocol,worker}.rs` | 14 + 227 + 209 + 703 | Elements and text rules: `win/uia.rs`, `uia_text.rs`, `win/bounded.rs`. The persistent MTA worker and retained stop targets: none (listed as a gap in `docs/MOVED_FROM.md:17`) | Mechanism |
| `src-tauri/src/windows_wezterm.rs` | 702 | None | Mechanism (a terminal read-back channel); which terminals to bind is a declaration |
| `src-tauri/src/paste_event_probe.rs` | 292 | None | Mechanism (macOS listen-only event tap) |
| `src-tauri/src/macos_input.rs` | 158 | `desktop/macos/keyboard.rs`, `macos/ax.rs` | Mechanism |
| `tsf-tip/` (`lib.rs`, 3 sections, `bin/tsf-register.rs`) | 953 | None | Dormant. `tsf-tip/src/lib.rs:3-8` and `src-tauri/Cargo.toml:73-76` say it has no dependent; it takes no part in delivery today |
| `src-tauri/src/media_control.rs`, `media_control/{backend,mac,win,worker}.rs` | 33 + 214 + 227 + 130 + 108 | None. `media_keys.rs` is a blind toggle; `rightkit-now-playing` publishes the host's own playback | Mechanism (observe, pause, play an identified player) plus one policy sentence (pause while recording) |
| `src-tauri/src/onboarding_calibration/media_guard.rs` | 77 | None | Product (onboarding) |
| `src-tauri/src/streaming_insertion.rs` | 235 | Typing primitives in `desktop` | Product feature (Developer mode) on crate primitives |
| `src-tauri/src/pill_native_win/clipboard.rs` | 519 | n/a | Product UI (the clip card), not delivery mechanism |
| `src-tauri/src/runtime/session.rs` (delivery block about `:840-1010`) | 1,429 total | Hook exists: `rightkit-dictation-tauri/src/host.rs:113` `DictationHost::deliver_final` | Policy (start or stop destination) wrapped around mechanism (`restore_focus_pin`, fingerprint compare) |
| `heardright_platform/src/lib.rs` | 138 | `desktop.rs` (147), 13 differing lines | Mechanism, **duplicate** |
| `heardright_platform/src/macos/clipboard.rs` | 447 | `desktop/macos/clipboard.rs`, 8 | Duplicate |
| `heardright_platform/src/macos/ffi.rs` | 321 | `desktop/macos/ffi.rs`, 10 | Duplicate |
| `heardright_platform/src/macos/focus/{context,harvest,mod,native_write,pin,polish,proof}.rs` | 664, 122, 28, 210, 563, 181, 617 | `desktop/macos/focus/*`, 38, 0, 2, 8, 40, 6, 37 | Duplicate |
| `heardright_platform/src/macos/{frontmost,keyboard,mod,observation,qos,window_query}.rs` | 188, 113, 25, 616, 11, 303 | `desktop/macos/*`, 6, 0, 16, 10, 0, 2 | Duplicate |
| `heardright_platform/src/macos_focus_continuity.rs` (focus observer) | 1,193 | `desktop/macos_focus_continuity.rs`, 20 | Duplicate |
| `heardright_platform/src/windows.rs` (clipboard, chords, typing) | 1,408 | `desktop/windows.rs`, 18 | Duplicate |
| `heardright_platform/src/windows_focus_continuity.rs` | 466 | `desktop/windows_focus_continuity.rs`, 6 | Duplicate |
| `heardright-command-host/src/delivery.rs`, `delivery/{clipboard,deliver,insert,keystrokes,targets}.rs` | 31 + 197 + 182 + 644 + 77 + 109 | `rightkit-dictation-command-host/src/delivery/*` (differing lines 49, 0, 123, 35, 15) | Duplicate, and already drifting; a second paste transaction with the strict rule |
| `heardright-command-host/src/focus.rs` | 789 | `rightkit-dictation-command-host/src/focus.rs`, 55 differing | Duplicate |
| `heardright_core::delivery` | re-export | `rightkit-transcript-library/src/delivery.rs` | Already RightKit |

Verified by `diff -w` line counts: the whole `heardright_platform` crate (7,614 source lines) equals `rightkit_automation::desktop` apart from path prefixes, brand strings and one behaviour (own-window prefix, see 2d). MAIN does not use it: `heardright_platform/Cargo.toml:13,33` pins `rightkit-automation` with `default-features = false`.

Duplicates that already exist **inside RightKit** and must be folded in the same work (verified): `collapse_line_break_runs` is defined in both `proof.rs:34` and `desktop.rs`; `continuity_window_valid` in both `proof.rs:158` and `desktop.rs`; `replacement_editor_may_receive` in both `proof.rs:141` and `rightkit-transcript-library/src/delivery.rs:914`.

### 2b. Product policy expressed as values the app passes in

`docs/LOCKED_RULES.md:19-24` lists four things that "stay in HeardRight". Under the ruling, the mechanism for each moves into the crate and the app keeps one declaration. Proposed module: `rightkit_automation::desktop::delivery` (feature `desktop-context`). The crate must not depend on `rightkit-transcript-library`, so it defines its own report type and the dictation crates map it to `DeliveryRecord`.

```rust
// rightkit_automation::desktop::delivery
pub struct DeliveryRules {
    pub success: SuccessRule,
    pub enter: EnterRule,
    pub secure: SecureRule,
    pub clipboard: ClipboardRule,
    pub destination: DestinationRule,
    pub terminals: TerminalRule,
    pub chord: ChordRule,
    pub timeouts: Timeouts,
    pub own_pid: u32,
}
pub enum SuccessRule { Receipt, ReceiptAndReadback, ReceiptOrReadback }
pub enum EnterRule {
    Never,
    ReadbackRequired,
    AfterSuccess { settle_ms: u64, blind_settle_ms: u64, allow_unreadable: bool },
}
pub enum SecureRule { PositiveEvidenceOnly, KnownNonSecureRequired }
pub struct ClipboardRule { pub refuse_lossy_snapshot: bool, pub restore_quiet_ms: u64,
                           pub restore_cap_ms: u64, pub restore_in_background: bool,
                           pub copy_on_failure: bool }
pub struct Timeouts { pub receipt_ms: u64, pub readback_ms: u64,
                      pub terminal_readback_ms: u64, pub terminal_no_channel_grace_ms: u64 }
```

| Locked item | Declaration the app keeps | Mechanism in the crate |
|---|---|---|
| Recording-delivery policy | `const DICTATION: DeliveryRules = ...` with `SuccessRule`, `EnterRule`, `SecureRule`, `ClipboardRule`, `Timeouts` | `delivery::deliver(dest: &Destination, text: &str, req: DeliveryRequest, rules: &DeliveryRules, obs: &mut dyn DeliveryObserver) -> DeliveryReport`. `DeliveryRequest { send_enter: bool, require_editable: bool }`. `DeliveryReport { outcome: PasteOutcome, enter_sent: bool, steps: Vec<Step>, target: TargetIdentity }` with `PasteOutcome::{Pasted, PastedUnknown, SentUnconfirmed(Reason), NotDelivered(Reason), Copied(Reason)}` |
| Stop-target selection | `DestinationRule { stop: StopBinding, start: StartBinding, own_window: OwnWindowRule }` where `StopBinding::{PinRequired, LiveTarget}`, `StartBinding::{Never, WhenBoundAndUnchanged}`, `OwnWindowRule::{Refuse, RestoreStart}` | `delivery::capture_destination(moment: CaptureMoment, rules) -> Captured` (today `capture_stop_target_{macos,windows}`, `capture_start_destination_*`); `Destination::is_current()`; `delivery::choose_destination(start, stop, binding: StartBindingEvidence, rules) -> Choice` returning `Stop`, `Start` (after `restore_focus_pin`), `Replacement` (rule 3) or `Recovery(reason)`; this is the block now in `session.rs` |
| Start-capture gating by hotkey proof | `StartCaptureRule { hotkey_anchor_required: bool, anchor_max_age_ms: u64 }` inside `DestinationRule` | Already in the crate: `desktop::capture_hotkey_focus_anchor`, `HotkeyFocusAnchor`, `capture_focus_continuity_with_editor`. `capture_destination(CaptureMoment::Start { anchor }, ..)` applies the rule |
| WezTerm pane binding | `TerminalRule { table: &'static [TerminalMatch], channels: &'static [TerminalChannelKind], no_channel: NoChannelRule }` with `TerminalChannelKind::WezTermCli`, `NoChannelRule::{ReportPastedUnknown, ReportUnconfirmed}` | `trait TerminalChannel { fn bind(&self, hwnd, pid, creation) -> Option<Box<dyn BoundPane>>; }` and `trait BoundPane { fn preflight(&self, budget) -> PanePreflight; fn read(&self, deadline) -> Option<TextProof>; }`; `desktop::windows::wezterm` implements it (moved `windows_wezterm.rs`). The process/class/title table from `windows_terminal_target` becomes the default `TerminalMatch` data |
| PTT chord windows | `ChordRule { wait_for_physical_modifiers: bool, max_wait_ms: u64, force_release: bool }` | Already in the crate: `desktop::windows::send_paste_chord_after_hotkey`, `settle_physical_modifiers`. The chord windows themselves belong to `rightkit-hotkeys`, not to delivery |
| No TSF machine registration | Nothing to declare. The crate gets no TSF code | None. See question 5 |
| One detokenization entry point | Not delivery (ASR). Unaffected | None |
| Pill window z-order `TOPMOST` | Not delivery (overlay). The delivery-side need is only "never treat my own window as the destination": `rules.own_pid` plus `set_own_window_owner_prefix` | `desktop::macos::set_own_window_owner_prefix` (exists) |

Telemetry and progress leave the mechanism through one trait, so the crate has no logging dependency:

```rust
pub trait DeliveryObserver {
    fn step(&mut self, name: &'static str, ms: u64);
    fn note(&mut self, event: &'static str, detail: &dyn std::fmt::Display);
    fn paste_issued(&mut self);
    fn inserted(&mut self);
}
```

Media pause and resume, if it moves (question 4): `media_session::{Backend, Lifecycle, begin_for_session, restore_for_session, restore_any}` moved as they are from `media_control/`; the app's declaration is the call at recording start and stop.

### 2c. What the entry point must do to match live dictation

Recommendation: a **new entry point**, `desktop::delivery::deliver`, not a mode of `Automation::deliver_text`. `Automation` is generic over the `Platform` trait and built around the allowlist, leases and the tier ladder; the dictation transaction needs the concrete promised-clipboard, pin and UIA-worker types that live in `desktop`. To end with one paste implementation, `Automation::deliver_text` tier 4 (`delivery.rs:580-661`) is then rewritten to call the same transaction core.

Required behaviour, each point against today's `Automation::deliver_text` (all verified in `RK/rightkit-automation/src/delivery.rs`):

| Requirement | Live source | `deliver_text` today | New entry point |
|---|---|---|---|
| Paste first, one dispatch | `MAIN backend.rs:716-719`, `:1225-1226` | Paste is tier 4, off by default (`:50-58`, `:514-521`) | Only path |
| No native AX/UIA write first | same | Tier 1 `native_write` runs first (`:405-442`) | Never called |
| No typed text events | `MAIN backend.rs:708-714` | Tiers 2 and 3 post `InputEvent::Text` (`:444-512`) | Never |
| Clipboard receipt | `desktop/macos/clipboard.rs`, `desktop/windows.rs` | Plain `clip_write`, verdict on read-back only (`:598`, `:626`) | Promised clipboard, `mark_posted`, `wait_for_receipt` |
| No target activation | Backend never activates | `restore_front(pin.pid)` before, `give_back_front` after (`:604`, `:627`) | Refuse on focus change. The only activation is `choose_destination` returning `Start`, under `StartBinding::WhenBoundAndUnchanged` |
| Secure field | Positive evidence only; Enter needs positive non-secure | Same idea (`:386-390`) | `SecureRule` value |
| Lossy clipboard | Refuse, do not overwrite | `Why::ClipboardLossy` (`:591-597`) | `ClipboardRule.refuse_lossy_snapshot` |
| Read-back rule | Part 1 | Read-back always required (`Verdict`, `:560-578`) | `SuccessRule` and `EnterRule` values; all three shipped/MAIN behaviours are expressible |
| Restore | Quiet window, only if the clipboard generation is still ours | Immediate (`:631`) | `ClipboardRule` |
| Credential-shaped payload | Not checked by HeardRight | Refused by `looks_secret` (`:287-294`) | Not applied to dictation (a user may dictate anything); the kill switch `policy::killed()` is applied |

**Allowlist.** `Policy::check` refuses every app that is not on the allowlist ("Empty denies all", `policy.rs:56`, `:113-122`), and `ALWAYS_DENIED` contains `com.anthropic.claudefordesktop` (`policy.rs:28-39`), so the engine can never deliver into Claude desktop. The live builds have no allowlist and name Claude as a common target (`WIN-LIVE section01.rs:1232`). The list mixes two ideas: surfaces nothing should ever type into (system settings, the security agent, UAC and credential prompts) and "an agent must not drive the agent that governs it". Dictation is the user typing into the field they focused, so the second idea does not apply. Proposal: the new entry point does not call `Policy::check`. It takes `rules.own_pid` (never deliver into the host itself) and applies a narrower built-in list of credential and consent surfaces only (`com.apple.securityagent`, `consent.exe`, `credentialuibroker.exe`, and whatever else the owner keeps from the list). `policy.rs` exposes the two lists separately, for example `never_typed_into(app_id)` and `never_driven_by_agent(app_id)`, with `always_denied` as their union for the engine. This is question 2.

### 2d. What HeardRight deletes, and the startup calls

Deleted from HeardRight once it calls the crate (line counts per file are in 2a):

- The whole `heardright_platform` crate (19 source files, 7,614 lines, and its examples); every `heardright_platform::` path becomes `rightkit_automation::desktop::`. Dependents to repoint: `src-tauri`, `heardright-runtime`, `heardright-engine`, `heardright-text-pipeline`, `heardright-command-host` (each names it in `Cargo.toml`).
- `src-tauri/src/delivery/backend.rs`, `delivery/targets.rs`. `delivery.rs` shrinks to: the `DeliveryRules` constant, a `DeliveryObserver` that forwards to telemetry, and the map from `DeliveryReport` to `DeliveryRecord`.
- `src-tauri/src/windows_uia.rs` and `windows_uia/{api,protocol,worker}.rs`.
- `src-tauri/src/windows_wezterm.rs`.
- `src-tauri/src/paste_event_probe.rs`.
- The key-post and AX helpers in `src-tauri/src/macos_input.rs` that duplicate `desktop/macos/keyboard.rs`.
- `heardright-command-host/src/delivery.rs`, `delivery/*`, `focus.rs`, replaced by `rightkit-dictation-command-host` (whose own `delivery/insert.rs` must first be reduced to a call into the new entry point, or the duplicate just moves).
- The destination-choice block in `src-tauri/src/runtime/session.rs`, replaced by `choose_destination`.
- `src-tauri/src/media_control/*` if question 4 is answered "move".
- `tsf-tip/` per question 5.

Stays in HeardRight: the rules constant, the observer, `streaming_insertion.rs` (product feature on crate primitives), the clip card UI, onboarding media guard.

One-time startup calls (today's sites in `MAIN/src-tauri/src/app_run.rs` and `main.rs`):

1. First thing in `main()`: `desktop::macos::run_window_query_helper()` (today `main.rs:35`). The helper flag is now `--rightkit-window-query-helper` (`desktop/macos/window_query.rs:26`); the crate both passes and checks it, so nothing else changes.
2. **New, required**: `desktop::macos::set_own_window_owner_prefix("heardright")` before any frontmost query. MAIN hard-codes the prefix (`heardright_platform/src/macos/frontmost.rs:108`); the crate excludes **no** window when it is unset (`desktop/macos/frontmost.rs:106`), so without this call the pill can become a delivery destination. Verified.
3. `desktop::macos::set_window_query_diagnostic_hook(..)` and `enable_isolated_window_queries()` (`app_run.rs:1058-1064`).
4. `desktop::set_focus_observer_diagnostic_hook(..)` (`app_run.rs:515`).
5. `desktop::macos::start_chromium_prewake_watcher()` (`app_run.rs:531`).
6. `desktop::start_focus_observer()` on both systems (`app_run.rs:637`); coverage is still opened per recording with `begin_focus_coverage` / `end_focus_coverage`.
7. Windows: nothing new if the UIA worker keeps starting lazily on first request, as `windows_uia/api.rs` does today (suspicion: lazy start not traced line by line).
8. If media moves: no startup call; the worker is spawned on first `begin_for_session`.

### 2e. Ordered implementation plan

Each step is one change that type-checks alone for `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`. Steps marked RK publish a crate version; HeardRight consumes published versions only.

1. **Owner answers questions 1 to 3** (the rule values). No code.
2. **RK**: make `proof.rs` the single home of the pure rules. Move in `paste_outcome`, `insertion_attributable`, `terminal_grid_insertion_attributable`, `terminal_paste_has_no_confirm_channel`, `paste_restore_quiet_remaining`, the terminal match table. Delete the second copies in `desktop.rs` and re-export from `proof`. Pure code, both targets.
3. **HR**: turn on `desktop-context`, add the `set_own_window_owner_prefix` call, and reduce `heardright_platform/src/lib.rs` to `pub use rightkit_automation::desktop::*;`. Delete its other 18 files. Behaviour should be unchanged because the two trees are equal (2a).
4. **HR**: rename `heardright_platform::` to `rightkit_automation::desktop::` in the five dependents and remove the crate from the workspace. Mechanical.
5. **RK**: add `desktop::windows::uia_worker` (moved `windows_uia/{api,protocol,worker}.rs`; closes the "single persistent MTA worker" gap). Windows-only code with a stub module on macOS.
6. **RK**: add `desktop::windows::wezterm` behind `TerminalChannel` / `BoundPane`.
7. **RK**: add `desktop::macos::paste_probe` (moved `paste_event_probe.rs`).
8. **HR**: repoint `src-tauri` at steps 5 to 7 and delete `windows_uia*`, `windows_wezterm.rs`, `paste_event_probe.rs`.
9. **RK**: add the types only: `DeliveryRules` and its enums, `Destination`, `DeliveryReport`, `DeliveryObserver`. No behaviour.
10. **RK**: `capture_destination`, `Destination::is_current`, `choose_destination` (moved from `targets.rs` and `session.rs`).
11. **RK**: `deliver` for macOS (moved from `backend.rs:645-1188`), reading every choice from `DeliveryRules`.
12. **RK**: `deliver` for Windows (moved from `backend.rs:1191-1719`). **Riskiest step.**
13. **HR**: `delivery.rs` becomes rules, observer and record mapping; delete `backend.rs` and `targets.rs`; `session.rs` calls `choose_destination`.
14. **RK**: `rightkit-dictation-command-host/src/delivery/insert.rs` calls `deliver`; **HR** drops `heardright-command-host`'s delivery and focus files for the RightKit crate. This also removes the second success rule noted in 1.5.
15. **RK**: `Automation::deliver_text` tier 4 calls the same transaction core, keeping its allowlist and its opt-in activation as its own options.
16. **RK/HR**: media session move, if approved.
17. **RK**: rewrite `docs/LOCKED_RULES.md:19-24` and `docs/MOVED_FROM.md:13,17` to say delivery is in the crate and the product keeps declarations.

Why step 12 is the riskiest: it is the largest branch set (three stop-target kinds, the receipt/read-back race, the WezTerm probe, the "terminal unknown" outcome, the late-receipt restore thread, modifier settling); it is Windows-only code, so a macOS type-check says nothing about it; its behaviour is timing-dependent; the crate has no unit tests by owner decision (`CHANGELOG.md` 0.6.5), so only an end-to-end run on the installed Windows build can prove it; and it is the path where the shipped rule and MAIN's rule differ least visibly, so a slip is easy to miss. Step 3 is the largest deletion but low risk because the code is already equal.

### 2f. Questions that need the owner

1. **Which success rule ships?** The shipped builds disagree (Windows: receipt only; macOS: receipt and read-back). MAIN uses receipt, or key event plus read-back, on both. *Recommendation*: `SuccessRule::ReceiptOrReadback` on both systems. It is what shipped Windows does plus a fallback, MAIN's comments record it as a deliberate decision, and the strict macOS rule is what leaves unreadable apps permanently on the recovery card. The crate can express the strict rule if he prefers it for macOS.

2. **Enter on macOS without read-back?** Shipped macOS never sends Return without confirmed read-back. MAIN sends it after a receipt and a wait of at most 400 ms, and by my reading also to editors that cannot be read at all, although its own comment says it does not (1.4). Shipped Windows has sent Enter on the receipt alone since September. *Recommendation*: `EnterRule::AfterSuccess { settle_ms: 400, blind_settle_ms: 60, allow_unreadable: false }` on macOS, so a readable editor gets the bounded wait and an unreadable one gets the paste without Return, which is what the MAIN comment promises; `allow_unreadable: true` on Windows, which is the shipped behaviour there. Needs his call because it decides whether "send" works in unreadable Mac apps.

3. **Allowlist for dictation, and Claude desktop.** *Recommendation*: dictation delivery does not use the allowlist; it refuses only the host itself and credential or consent surfaces, and Claude desktop stays denied for agent-driven automation only (2c). Without this, moving delivery into the crate breaks dictation into Claude desktop.

4. **Does media pause/resume move into `rightkit-automation`?** His stated reason for automation is "the layer that uses uia and ax"; media control uses neither (GSMTC on Windows, MediaRemote on macOS). The earlier inventory proposed a new `rightkit-media-control` crate. *Recommendation*: a small separate crate, or `rightkit-now-playing` gaining a "control other players" half, rather than `rightkit-automation`. Either way HeardRight's copy is deleted.

5. **TSF tip: delete or park?** It is dormant, has no dependent, and takes no part in delivery (2a). *Recommendation*: leave it out of `rightkit-automation` and delete it from HeardRight; it remains in git history if TSF is ever revisited. Parking it in RightKit would move 953 lines that nothing compiles against.

6. **Windows with no stop-time pin.** Shipped Windows pastes into whatever is focused at delivery time; MAIN sends the transcript to the clipboard when no stop pin was captured (`session.rs:861-869`). *Recommendation*: keep MAIN (`StopBinding::PinRequired`), since the stop-time binding is what stops a transcript landing in a window the user switched to, but confirm, because it is a visible change from the installed Windows build and the code carries no note about it.
