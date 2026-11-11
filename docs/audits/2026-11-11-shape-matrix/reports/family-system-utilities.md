# Family review: System utilities

Read-only review, 11 Nov 2026. Reference app for this family is Pulse (`S/survey/pulse`, surveyed about 1 Nov). RightKit is `/home/user/rightkit` at `d17fd41`.

Paths: `P/` = `S/survey/pulse/`, `RK/` = `/home/user/rightkit/crates/`. "Planned lines" are summed from `S/coverage-rows.json`. Each finding is marked **[V]** verified by reading or diffing, or **[S]** suspicion.

CodeRight source is not available (`S/survey/coderight` does not exist). Legion is not available.

## Findings that apply to the whole family

1. **Every Pulse-fed crate here is a near-verbatim lift of Pulse files, and Pulse does not depend on any of them yet.** [V] Pulse's `Cargo.toml` files name `rightkit-mesh` (its own in-tree `pulse-mesh` package), `rightkit-send`, `rightkit-fs`, `rightkit-daemon` and others, but none of `rightkit-launcher`, `-clipboard-history`, `-disk-cleanup`, `-app-manager`, `-system-monitor`, `-notch-ui`, `-win-ui`, `-desktop-tweaks`, `-app-lock`, `-privileged-helper`. Until Pulse switches, each of these is duplicate code in two repositories.
2. **Pulse implements several capabilities twice itself: Swift on Mac, Rust on Windows.** [V] Launcher (`P/mac/Notch/Sources/Launcher/*.swift`, 12,919 lines, against `P/windows/src/launcher*.rs`), clipboard history (the `Clipboard*.swift` files in the same folder against `P/windows/src/clip*.rs`), notch rendering, desktop tweaks. RightKit took only the Windows Rust half. The plan lists these as one cargo crate "(Win32 + Swift)", which cannot hold the Swift half. "No duplicate code" therefore needs a decision per capability: Mac calls the Rust core through FFI, or the Swift twin becomes a RightKitSwift module and is accepted as a second implementation.
3. **Lifted crates still carry Pulse product decisions.** [V] Strings and pages in `rightkit-launcher/src/panel.rs`, `Cell::Claude`/`Cell::Codex` in `rightkit-notch-ui/src/layout.rs:132-175`, 36 "Pulse" strings in `@rightkit/system-ui`.
4. **The path-based plan mis-mapped several large files** (listed per package below). About 10,000 planned lines in this family belong to other packages.
5. **Identical files in two RightKit crates.** [V] `rightkit-launcher/src/json.rs` = `rightkit-clipboard-history/src/json.rs` (166 lines, `cmp` identical). `rightkit-disk-cleanup/src/snapshot.rs` = `rightkit-system-monitor/src/snapshot.rs` (67 lines, identical).

---

## rightkit-mesh

1. **Exists:** `crates/rightkit-mesh` 0.1.2 (13,084 lines of Rust, 35 files).
2. **Feeds (plan):** Pulse 33,474; HeardRight 873; CodeRight 605.
3. **Basis:** Pulse. [V] Every file of `P/rightkit-mesh/src` is in the crate with 0 to 7 changed lines, except `trust_pulse.rs`, which was dropped. The `devicelink` module (behind a feature) is `P/core/src/devicelink/{circle,identity,join_token,link,pairing,registry,relay_admit,wire,transport}` with small changes; the record types moved out to `rightkit-device-link-types`. RightKit added `mailbox/registry.rs`, `devicelink/policy.rs` and the `rightkit-mesh-relay-admission` binary.
4. **Side by side:**
   - The earlier "about half" figure is right, and the missing half is the host. [V] `P/engine/src/devicelink_host.rs` (7,337 lines: controller thread, `PulseTrust`, pairing sessions over `pulse/pair/1`, the `pulse/link/1` dispatcher, circle sync) and `P/engine/src/share_mesh.rs` (4,831 lines: Send over the mesh) have 127 and 89 matching lines in the crate. No RightKit crate depends on `rightkit-mesh` except `rightkit-mesh-ffi`, and the only `impl PeerTrust` in RightKit is the phone one at `rightkit-mesh-ffi/src/store.rs:198`. A desktop app adopting the crate today must rewrite those 12,000 lines.
   - The host is entangled with Pulse: it imports `pulse_core::agent::*`, `crate::journal::{Journal, LinkSubmit}`, `pulse_core::chat::mesh`, `pulse_core::remote::unlock` (`devicelink_host.rs:65-91`).
   - HeardRight's three files are not mesh. [V] `heardright-main/ios-native/HeardRight/DevicePairingStore.swift` reports Apple Watch pairing through `WCSession` ("HeardRight never creates a second account, QR pairing flow, or server-side device identity"). `AccountSessionStore.swift` is a bearer session token for the account's device list. They belong to `RightKitWatch`/`RightKitWatchBridge` and `RightKitAccount`.
   - CodeRight's five files (`Pairing/PairingView`, `DaemonPickerView`, `NearbyDiscoveryService`, `QRScannerView`, `ConnectionTrustView`) could not be read. [S] The names suggest QR pairing with a daemon, which is `rightkit-daemon` territory, not this mesh.
5. **Best shape:** three layers.
   - `rightkit-mesh`: transport (endpoint, hello, mailbox, seal, sync, transfer, caps), as now.
   - Device link (pairing, registry, circle, authorization): today a feature inside the mesh crate. Keep it there or make it its own crate, but resolve the name clash with `rightkit-device-link` (below).
   - A new desktop host layer: the controller, a registry-backed `PeerTrust`, the pairing session runner and an ALPN dispatcher that takes handlers from the app. Pulse supplies its journal, chat and Send handlers.
   Today's package is the first two layers only.
6. **Fit:** Pulse **gaps** (host layer; delete the in-tree `pulse-mesh` and `core/src/devicelink` copies). HeardRight **n/a** (mis-mapped). CodeRight **not verified**.
7. **Boundary:**
   - `devicelink/policy.rs` decides "whether the paired computer may use chat here": a Pulse chat rule inside the mesh crate. [V]
   - `DEFAULT_VAULT_SERVICE = "pulse.device-link"` (`devicelink/identity.rs:16`); ALPN `pulse/relay-enrol/1` (`relay_admit.rs:32`); about 30 signing-domain strings `pulse-link/...`. [V] These are wire and keychain constants. Renaming breaks every paired Pulse device, so either keep them and document them as the protocol's names, or bump the schema version on purpose.
   - Relay caps are constants (`caps.rs:20-30`: 25 MB per transfer, 250 MB per circle per month). They are Pulse's relay budget; a second app cannot change them. [V]
   - `lib.rs` header still says "built in-tree in Pulse", "iroh 1.3.0", "Nothing here has been compiled or run".
   - Pulse's repo still holds `rightkit-mesh/` and `rightkit-mesh-ffi/` (package names `pulse-mesh`, `pulse-mesh-ffi`): confirmed duplicates. [V]
8. **Actions:** add the host layer; make the vault service and relay caps host-supplied (defaults = Pulse's); move `policy.rs` to the chat package or generalise it; decide and document the wire names; fix the crate header; drop HeardRight from the feed list.

Depth: deep.

## rightkit-mesh-ffi

1. **Exists:** `crates/rightkit-mesh-ffi` (4,784 lines including `ios-netwatch`).
2. **Feeds:** Pulse (counted inside rightkit-mesh; about 4,900 lines).
3. **Basis:** Pulse. [V] `lib.rs`, `circle.rs`, `store.rs` identical; `pairing.rs` differs by 57 lines.
4. One implementation.
5. **Best shape:** a generic C ABI: the Joiner side of pairing and one request per stream. Mostly that already; the header says "knows no application protocol".
6. **Fit:** Pulse **fits** (after deleting its copy).
7. **Boundary:** `LINK_ALPN = "pulse/link/1"` is hard-coded at `lib.rs:959`, and `circle.rs` calls `device.circle.members` on it and uses `pulse.approval.v*` and `pulse-check-bind-v*`. [V] That is the Pulse desktop's protocol inside the "generic" FFI. It moves to the host layer's protocol once that exists.
8. **Actions:** take the link ALPN from the caller or from the host-layer crate; remove "Written, not compiled, not run" once it has been.

Depth: deep (diff), light on internals.

## RightKitMeshLink (Swift)

1. **Exists:** `swift/RightKitMeshLink` as a standalone package (542 lines), not a module of RightKitSwift. [V]
2. **Feeds:** Pulse `swift/MeshLink` 696 lines, plus `ios/Sources/Mesh` 1,693 and `android/meshlink` about 410.
3. **Basis:** Pulse. `MeshLink.swift` is identical. [V]
4. One implementation.
5. **Best shape:** a thin Swift wrapper over the FFI, as it is, plus a Kotlin twin. Pulse's `ios/Sources/Mesh/Pulse*Client.swift` speak `pulse/phone/1` and stay in the app.
6. **Fit:** Pulse **fits**.
7. **Boundary:** the Android wrapper (`P/android/meshlink`, Kotlin) has no RightKit home. [V] The Swift package sits outside RightKitSwift, so it is not covered by that package's versioning.
8. **Actions:** decide whether it stays standalone (it carries a binary dependency, which is a fair reason) and say so; add the Kotlin wrapper to `rightkit-android`.

Depth: light.

## rightkit-device-link

1. **Exists:** `crates/rightkit-device-link` (924 lines).
2. **Feeds:** none in the plan.
3. **Basis:** fresh (rightkit#1244): a sans-IO reliable ordered delivery layer over a relay (`SendQueue`, `Receiver`, frames).
4. Not the same thing as Pulse's device link. [V] It has no dependents in RightKit and none in any surveyed app.
5. **Best shape:** fine as a small sans-IO crate.
6. **Fit:** all **n/a**.
7. **Boundary:** the name collides with `rightkit_mesh::devicelink` and `rightkit-device-link-types`, which are a different concept (pairing and registry). [V]
8. **Actions:** rename (for example `rightkit-relay-stream`) or fold it in where it is used; confirm it has a consumer before publishing.

Depth: light.

## rightkit-device-link-types

1. **Exists:** `crates/rightkit-device-link-types` (764 lines).
2. **Feeds:** Pulse (inside the mesh count).
3. **Basis:** Pulse's `core/src/devicelink/registry.rs` and `circle.rs` record types plus `pulse_core::agent::{DeviceRef, EffectClass, Principal}`. [V]
4. One implementation.
5. **Best shape:** already right: types only, shared by mesh, owner-approvals, remote-gateway and chat-bridge.
6. **Fit:** Pulse **fits**.
7. **Boundary:** none found.
8. **Actions:** none.

Depth: light.

## rightkit-disk-cleanup

1. **Exists:** `crates/rightkit-disk-cleanup` (12,938 lines).
2. **Feeds:** Pulse 25,046.
3. **Basis:** Pulse `core/src` (`cleanup_scan`, `scan`, `cleanup`, `store`, `rules`, `duplicates`, `storage_browser`, `compression`, `folder_growth`, `activity`, `platform/*`). [V] Changed lines: `scan.rs` 63, `rules.rs` 104, `store.rs` 171, `cleanup_scan.rs` 209.
4. One implementation; Mac and Windows both use this Rust core.
5. **Best shape:** the scan, rules and cleanup core as it is, plus the engine-side layer that makes it usable: scan lifecycle and the folded index (`P/engine/src/scanner.rs`, 2,464 lines, 42 matching), cleanup sources, the compress worker and growth sampling. Today that layer is absent, so an adopter gets a library with no runner.
6. **Fit:** Pulse **gaps**.
7. **Boundary:**
   - `P/engine/src/journal/*` (4,877 lines) is mapped here but is the durable agent task journal (`journal/mod.rs:1` "one read-only Pulse job that survives UI close and restart", `agent_act.rs`, `link_replay.rs`). It belongs to `rightkit-job-queue` or the agent engine. [V]
   - `snapshot.rs` is duplicated in `rightkit-system-monitor` and hard-codes the schema name `"pulse-snapshot"` (`snapshot.rs:22`). [V]
   - `store.rs`, `history.rs` and `model.rs` hold snapshot and report types that system-monitor and app-manager also need. [S]
8. **Actions:** move `snapshot.rs` to one owner (`rightkit-app-settings` or a small shared crate) with the schema name supplied by the host; add the scan runner; re-map the journal.

Depth: deep (headers and diffs; rule tables not read line by line).

## RightKitNotch (Swift)

1. **Exists:** no. There is no such module in `swift/RightKitSwift/Sources`. [V]
2. **Feeds:** Pulse 17,668 (`mac/Notch/Sources/{Notch 9,448, Features 4,081, Model 3,325, Presentation, DesignSystem}`).
3. **Basis:** none.
4. The Windows counterpart is `rightkit-notch-ui` (Rust). The two are parallel ports of one design.
5. **Best shape:** a generic notch surface (window, geometry, hover card container, motion) with cells and cards supplied by the app. The AI-provider rings, session cards and features stay in Pulse or go to `rightkit-ai-accounts` UI.
6. **Fit:** Pulse **gaps** (nothing to adopt).
7. **Boundary:** `Features/` and `Model/` are Pulse product, not notch infrastructure. [S] (not read in depth)
8. **Actions:** create the module with the generic surface only; do not lift `Features`/`Model` wholesale.

Depth: light. The package is over 5,000 planned lines but there is no RightKit side to read against; the Pulse side was sampled by directory only.

## rightkit-launcher

1. **Exists:** `crates/rightkit-launcher` (11,319 lines).
2. **Feeds:** Pulse 22,237 (Windows Rust about 9,300; Mac Swift 12,919).
3. **Basis:** Pulse's Windows launcher. [V] `panel.rs` (5,426) is `P/windows/src/launcher.rs` with 617 changed lines; `calc`, `snippets`, `files`, `notes`, `rates`, `config`, `spot_takeover` are the `launcher_*.rs` files. Mac Swift coverage is 0.
4. **Side by side (Mac Swift against Windows Rust, both Pulse):**
   - Calculator: same grammar, written twice (`P/mac/Notch/Sources/Launcher/LauncherCalculator.swift:1-22` against `RK/rightkit-launcher/src/calc.rs:1-22`). The Rust one hand-writes `%.10g`.
   - Mac asks the engine (`LauncherChat.swift:138` `ResidentEngineClient.shared.call("launcher.ask")`); Windows goes through `host::Host`.
   - The plan also put the Mac clipboard strip (`ClipboardStrip*.swift`, `Clipboard*.swift`, about 6,500 lines) under launcher because of its folder. It is clipboard history. [V]
5. **Best shape:** split in two.
   - `rightkit-launcher` (portable core): `calc`, `config`, `dictionary`, `rates`, `notes`, `commands`, `files`, ranking and usage. No window, no product pages.
   - The Win32 panel as a separate crate or left in Pulse's shell.
   Today's crate is not that shape.
6. **Fit:** Pulse **wrong** as a shared package (it fits Pulse Windows only because it is Pulse Windows). A second app would inherit Pulse's product.
7. **Boundary:**
   - Product inside the package: `panel.rs:3522-3560` lists "Pulse: what fills the drives", "Pulse: settings" and so on; `Section::Pulse` (`:640`); the setting `launcherPulseCommands` (`:3563`); "Use Alt+Space for Pulse" (`:2822`); data kept in `%LOCALAPPDATA%\Pulse\launcher\` (`:28`); `chat.rs:124` "Pulse's engine is not running". [V]
   - `json.rs` is a second copy (see family finding 5). [V]
   - `response_layout.rs` (1,146 lines, the AI answer document model) is in the launcher crate although the notch also uses it; the plan had it under overlay-win. [V]
   - The crate description in `Cargo.toml` still says only "arithmetic calculator and unit conversion parsers". [V]
   - Depends on `rightkit-clipboard-history` on Windows.
8. **Actions:** split core from panel; move product pages and strings behind the host; one `json` module; move `response_layout` to a presentation crate; decide Mac: FFI to the core or a Swift module.

Depth: deep.

## RightKitSystemUI (Swift)

1. **Exists:** no. [V]
2. **Feeds:** Pulse 21,612 (`mac/Hub/Sources/PulseHub/Sections`, 63 files).
3. **Basis:** none.
4. Pulse has two hubs: SwiftUI on Mac (this) and React in `hub/src` (see `@rightkit/system-ui`). Same sections, two implementations.
5. **Best shape:** per-section SwiftUI views that take a typed client and strings, in step with the React package's sections; Chat, Membrane and Devices sections belong to their own families.
6. **Fit:** Pulse **gaps** (nothing to adopt).
7. **Boundary:** not verified.
8. **Actions:** create it, section by section, after the React package's API is settled so both speak the same contract.

Depth: light (no RightKit side; Pulse side not read).

## rightkit-app-manager

1. **Exists:** `crates/rightkit-app-manager` (6,183 lines).
2. **Feeds:** Pulse 13,783.
3. **Basis:** Pulse `core/src` (`app_manager.rs` 37 changed lines, `apps.rs`, `process_control.rs`, `apps_windows/*`). [V]
4. One implementation of the core. Disk-image install exists twice in Pulse: `mac/Notch/Sources/Conveniences/DiskImageInstaller.swift` (742) and `windows/src/installer.rs` (2,430, notch cards for `.msi`/`.msix`). Both are UI flows, not in the crate.
5. **Best shape:** today's core is right. It already takes the host's identity through `host::HostIdentity` (bundle id prefix, name prefix, display name) so it never quits or uninstalls its host. Missing: the Windows registry inventory and leftovers (`P/engine/src/apps_windows.rs` 902, `apps_windows_leftovers.rs` 634), startup items (`engine/src/startup_items.rs` 487) and auto-quit.
6. **Fit:** Pulse **gaps**.
7. **Boundary:** `subprocess.rs` is a thin detached runner over `rightkit-child-process` and says no published runner does this (`subprocess.rs:1-6`); it could move there. [S] Installer cards are product UI and should stay in the shell.
8. **Actions:** add the Windows engine half and startup items; consider moving the detached runner to `rightkit-child-process`.

Depth: deep (diff and host seam), light on internals.

## rightkit-system-monitor

1. **Exists:** `crates/rightkit-system-monitor` (4,190 lines).
2. **Feeds:** Pulse 13,212.
3. **Basis:** Pulse `core/src` (`lib.rs` became the crate's `lib.rs`, `monitor.rs` 16 changed lines, `process_inspect`, `drive_health`, `gpu_temp`, `processes`). [V]
4. One core. Mac `System/SystemProviders.swift` (436) and Windows `sensors.rs` (1,414) are separate readers for the notch rings.
5. **Best shape:** the core as it is, plus Windows sensors (GPU and network sampler), volumes and eject (`engine/src/eject_windows.rs` 707, `volumes.rs`), Windows health (`engine/src/health_windows.rs` 626) and metrics history. Today those are absent.
6. **Fit:** Pulse **gaps**.
7. **Boundary:**
   - `P/windows/src/usage.rs` (3,013 lines) is mapped here but is the Claude and Codex usage reader for the AI rings (`usage.rs:1-13`). It belongs to `rightkit-ai-accounts` or `rightkit-spend`. [V]
   - `snapshot.rs` duplicate with `"pulse-snapshot"`. [V]
   - State directory is a process-wide `OnceLock` (`lib.rs:19-25`); fine for one host per process.
   - `rightkit-sysmem` also reads physical memory on Windows; small overlap. [S]
8. **Actions:** single snapshot owner; add the Windows sensor, volume and health code; re-map `usage.rs`.

Depth: deep (diff), light on internals.

## rightkit-clipboard-history

1. **Exists:** `crates/rightkit-clipboard-history` (12,386 lines).
2. **Feeds:** Pulse 25,520 by coverage rows (12,760 in the plan table).
3. **Basis:** Pulse Windows `clip*.rs`. [V] Coverage 12,032 of 13,658 distinctive lines. `strip.rs` is `clip_strip.rs` with 230 changed lines.
4. **Side by side (Mac Swift against Windows Rust):**
   - Same spec and the same folder format (`model.rs` header: "a folder copied from the Mac opens here and the other way round").
   - Encryption differs: Mac seals with AES-256-GCM under a Keychain key, magic `PULSECL1` (`P/mac/Notch/Sources/Launcher/ClipboardSeal.swift:8-23`); Windows seals with DPAPI, magic `PULSECL2` (`RK/rightkit-clipboard-history/src/seal.rs:4-26`). A sealed folder is not portable between the two. That is by design, but the spec promise only holds for unsealed folders. [V]
   - Detection is a pure function in both (`ClipboardDetect.swift`, `detect.rs`), written twice.
5. **Best shape:** portable core (`detect`, `model`, `search`, `seal` behind a key trait) and platform capture and paste; the strip UI separate. Today the crate has the right portable modules but also holds the Win32 strip (`strip.rs`, `strip/draw.rs`, `popover.rs`, `drag.rs`: about 6,300 lines) and depends on `rightkit-notch-ui`, `rightkit-win-ui`, `rightkit-overlay`, `rightkit-hotkeys` and `rightkit-automation` on Windows.
6. **Fit:** Pulse **gaps** (Windows fits; the Mac twin has no home).
7. **Boundary:**
   - `win.rs` declares the Win32 clipboard calls by hand; `rightkit-clipboard` already owns Windows clipboard read and write. The crate does not depend on `rightkit-clipboard`. [V] Two owners of raw clipboard access.
   - `json.rs` duplicate. [V]
   - Seal magic `PULSECL2` and thread names `pulse-clip-paste`, `pulse-clip-restore` are fixed. [V] The magic is a file format constant: keep and document.
   - `shelf.rs` ("nearby shelf") is Send UI, not history. [S]
   - `Cargo.toml` description names only detection. [V]
   - The Mac `SnippetExpansion.swift` (451) is mapped here; on Windows snippets are in the launcher crate. [V]
8. **Actions:** split the strip UI from the store; use `rightkit-clipboard` for raw access or state why not; single `json`; decide the Mac twin's home; fix the description.

Depth: deep.

## rightkit-clipboard

1. **Exists:** `crates/rightkit-clipboard` (819 lines, one file).
2. **Feeds:** none from this family in the plan.
3. **Basis:** fresh, split out of `rightkit-tauri-shell` (rightkit#956).
4. Overlaps `rightkit-clipboard-history/src/win.rs` on Windows (see above).
5. **Best shape:** already right for its job: a transaction API (`read`, `write`, `formats`, named boards).
6. **Fit:** Pulse **gaps** if it is to replace `win.rs` (the history needs the sequence number, a change listener, the owner window and an own-write marker format; `win.rs:92-124`). Not verified that `rightkit-clipboard` lacks all of these.
7. **Boundary:** none of its own.
8. **Actions:** add change listening and sequence number if the history is to use it.

Depth: light.

## @rightkit/system-ui

1. **Exists:** `packages/system-ui` 0.1.0 (8,559 lines).
2. **Feeds:** Pulse 7,889.
3. **Basis:** Pulse `hub/src` verbatim. [V] `api.ts` differs by 18 lines of 1,150.
4. One React implementation; the SwiftUI hub is the twin (see RightKitSystemUI).
5. **Best shape:** views that take a typed client as a prop or context and take product name and strings from the host; one export per section; no module-level singletons. Today's package is not that.
6. **Fit:** Pulse **fits** (it is Pulse's hub). Any second app: **wrong**.
7. **Boundary:**
   - `api.ts:1` imports `invoke` from `@tauri-apps/api/core` and makes 102 direct calls with Pulse's command names; `Chat.tsx:263` calls `invoke("chat_link", { ..., pulse })`. [V]
   - `lib/rk.ts` creates logging and device clients at import time with `surface: "hub"`. [V]
   - 36 "Pulse" strings in views (`Cleanup.tsx:129`, `Activity.tsx:136`, `Storage.tsx:1523`, `NearbySettings.tsx:163`), local-storage keys `pulse.hub.*`. [V]
   - Not system UI: `Chat.tsx` (chat-bridge), `NearbySettings.tsx` (send-ui), `LauncherSettings.tsx`, `onboarding/*`, `lib/legal.ts`, `lib/qr.ts`, `membrane.css`. [V]
   - `Monitor` and `Overview` views are named in the description and plan but are not in the package. [V]
8. **Actions:** inject the transport; remove product strings; move chat, nearby, launcher settings, onboarding, legal and QR to their packages; add Monitor and Overview or drop them from the description.

Depth: deep.

## rightkit-desktop-tweaks

1. **Exists:** `crates/rightkit-desktop-tweaks` (1,047 lines), Windows only.
2. **Feeds:** Pulse 2,032, all Mac Swift (`mac/Notch/Sources/Conveniences`: Finder cut and paste, path field, window maximizer, dock-click minimize, `EventTapHub`).
3. **Basis:** Pulse `windows/src/keys.rs` (102 changed lines). [V] The plan mapped that file to `rightkit-hotkeys` (ADOPT). None of the planned Mac files is in RightKit (0 matching lines).
4. Mac and Windows tweaks are different features (Finder tweaks against Mac-style Alt shortcuts), so they are not duplicates of each other.
5. **Best shape:** two things. A keyboard-hook hub with pluggable handlers (in `rightkit-hotkeys`), and a tweaks package that registers the Mac-style editing shortcuts on it. Mac Finder tweaks are a Swift module.
6. **Fit:** Pulse **wrong**: the Mac code has no home, and the Windows crate couples unrelated features.
7. **Boundary:** `services.rs` makes the keyboard layer know about screenshots, snippet keywords, the tool wheel, the copy burst and the strip's focus (`services.rs:1-40`). [V] That is Pulse's wiring expressed as a trait.
8. **Actions:** move the hook hub to `rightkit-hotkeys` with a generic handler chain; keep only the shortcut mapping here; create the Swift side.

Depth: light on Swift, deep on the crate header and seam.

## rightkit-app-lock

1. **Exists:** `crates/rightkit-app-lock` (1,045 lines).
2. **Feeds:** MailRight 1,244; Pulse 502.
3. **Basis:** MailRight for `state`, `preference`, `startup`, `flow`, `verify`; Pulse for `hello_key.rs` and `pin_window.rs`. [V]
4. **Side by side:**
   - MailRight: lock state, fail-closed preference, startup verification and Windows Hello or Touch ID (`mailright/src-tauri/src/app_lock.rs:9-116`, `app_lock_transition.rs:27-58`). The crate reproduces these with the product name passed in (`flow.rs:17-28` `Methods { product, set_lock, ... }`; MailRight hard-codes "Unlock MailRight", `app_lock.rs:22`).
   - MailRight's serialised transitions with the generation counter and the database gate (`app_lock_transition.rs:1-18`) are not in the crate; the crate's `flow.rs` only speaks to them through `LockTransport`. [V]
   - Pulse `windows/src/unlock_auth.rs` is not an app lock. It is the PIN window the owner uses to approve a remote unlock or payment (`unlock_auth.rs:1-9`), and `hello_key.rs` has Hello sign the engine's owner challenge (`hello_key.rs:1-9`). [V]
   - Pulse `ios/Sources/Owners/AppLock.swift` is a real app lock (Face ID, 30 s grace, on by default; `AppLock.swift:5-20`), in Swift, not in RightKit.
5. **Best shape:** state, preference, startup, verify and flow as now, plus the engine half (generation-checked transitions) as a trait the app's store implements. Owner-approval proofs move out.
6. **Fit:** MailRight **gaps** (engine half stays in the app until added). Pulse **gaps** (Windows parts are in the wrong package; iOS has no Swift module).
7. **Boundary:** `hello_key.rs` and `pin_window.rs` belong with `rightkit-owner-approvals`; they are why this crate depends on `rightkit-owner-approvals` on Windows. [V]
8. **Actions:** move `hello_key` and `pin_window` to owner-approvals; add the generation-checked transition core from MailRight; add a Swift `AppLock` (RightKitUI or a new module) with the grace period as a parameter.

Depth: deep.

## rightkit-privileged-helper

1. **Exists:** `crates/rightkit-privileged-helper` (209 lines); Swift `RightKitHelperProtocol` exists for the wire framing.
2. **Feeds:** Pulse 673.
3. **Basis:** Pulse `engine/src/elevate.rs` (97 of 124 lines). [V]
4. One implementation.
5. **Best shape:** broker (Rust, present) plus the Swift helper executable template, the allowed-path policy and the UI-side broker client. Only the first exists.
6. **Fit:** Pulse **gaps**.
7. **Boundary:** `P/mac/Notch/Helper/main.swift` hard-codes `dev.orthic.pulse.helper` and `/Library/Logs/Pulse/` (`main.swift:9-18`); `TrashPolicy.swift` is the reusable part. [V]
8. **Actions:** add a Swift helper module (policy, XPC listener with audit-token check, parameterised identifiers).

Depth: light.

## rightkit-send

1. **Exists:** `crates/rightkit-send` (7,368 lines); Swift `RightKitSend` (219 lines: `PeerTransport`, `PeerTrust`).
2. **Feeds:** Pulse 9,261 (ADOPT); ScrapeRight 891 (EXTEND).
3. **Basis:** Pulse's LocalSend implementation, already extracted; Pulse depends on the crate today (`P/engine/src/share_mesh.rs:36-38`). [V]
4. **Side by side:**
   - Pulse's planned lines are host and UI: `engine/src/share.rs` (state file and command files between notch and hub), `windows/src/send.rs`, `mac/.../NearbySharing.swift`. Not package code.
   - ScrapeRight iOS has only a seam and a stub: "The real engine will be the RightKit Swift product `RightKitSend` ... Until it exists, `StubSendEngine` stands in" (`scraperight/ios-native/ScrapeRight/Send/SendEngine.swift:4-7`). [V] `RightKitSend` today has TLS pinning and trust store only, no LocalSend engine.
   - Pulse iOS sends over the mesh on `pulse/phone/1` (`P/ios/Sources/Mesh/PulseSendClient.swift:4-7`), a different route from ScrapeRight's planned LocalSend.
5. **Best shape:** the crate as is for LocalSend; a second transport for paired devices (the generic part of `share_mesh.rs`: manifest, announce, name refusal rules, accept policy) as a feature or sibling crate on `rightkit-mesh`; a Swift `RightKitSend` engine that ScrapeRight's `SendEngine` protocol can wrap.
6. **Fit:** Pulse **fits** (LocalSend) with **gaps** for the mesh route. ScrapeRight **gaps** (no Swift engine).
7. **Boundary:** `SERVICE_TYPE = "_pulse-send._tcp"` (`lib.rs:48`), the `pulse` key in prepare-upload and `PulseExtra` are protocol names, already described as "Pulse extensions"; keep. [V] `Policy::default()` is "Pulse's current behaviour" (`lib.rs:76`): acceptable because it is a parameter.
8. **Actions:** build the Swift engine (or an FFI over the crate) for ScrapeRight; extract the mesh Send route from Pulse.

Depth: deep on boundaries, light on protocol internals.

## rightkit-record

1. **Exists:** `crates/rightkit-record` (9,649 lines).
2. **Feeds:** Pulse 4,656 (EXTEND); HeardRight 439 (EXTEND).
3. **Basis:** RightKit's own per-window recorder (ScreenCaptureKit and Windows.Graphics.Capture, bundles, verify, reader). Not Pulse or HeardRight code (2 and 40 matching lines).
4. **Side by side:** the three feeding pieces are three different things, none of them recording.
   - Pulse `windows/src/shot.rs` (1,781): the user-facing screenshot tool (Alt+Shift+4 region picker, toolbar, thumbnail card; `shot.rs:1-22`). Product UI.
   - Pulse `windows/src/viewshots.rs` (2,698): a CI-only off-screen renderer of notch views (`viewshots.rs:1-8`). QA.
   - Pulse `windows/src/shot_wgc.rs` (177): one still frame of a window through WGC (`shot_wgc.rs:1-8`). Generic.
   - HeardRight `tauri-app-next/src-tauri/src/shot/capture.rs`: `PrintWindow` capture behind feature `qa-shot` (`capture.rs:4-9`). QA.
5. **Best shape:** the crate is right for recording. A single-frame "capture this window to pixels" function is the one thing it could gain.
6. **Fit:** Pulse **gaps** (still capture only). HeardRight **n/a** for this package (its code is QA).
7. **Boundary:** `viewshots.rs` and HeardRight `shot/*` belong to `rightkit-qa`; `shot.rs` stays in Pulse's shell or becomes a screenshot-tool package of its own. [V]
8. **Actions:** add a still-frame window capture on the existing backends; re-map the rest.

Depth: deep on mapping, light on the crate's internals.

## rightkit-presence

1. **Exists:** `crates/rightkit-presence` (398 lines).
2. **Feeds:** none planned.
3. **Basis:** fresh (split from the Tauri shell). [S]
4. Pulse has its own fullscreen rule for hiding the notch pill (`P/windows/src/visibility.rs:1-25`: topmost window covers the monitor and has no caption or frame). The crate's `detect(monitor, windows, own_name)` classifies meeting and fullscreen from window observations (`presence.rs:158`). Related, not identical; rules not compared line by line.
5. **Best shape:** as is.
6. **Fit:** Pulse **gaps** [S] (would need its caption and frame rule expressible). No other app in this family.
7. **Boundary:** none found.
8. **Actions:** none required; compare Pulse's rule before Pulse adopts.

Depth: light.

## rightkit-power

1. **Exists:** `crates/rightkit-power` (900 lines). 2. **Feeds:** none planned. 3. **Basis:** fresh.
4. Pulse has no keep-awake or sleep code of its own (no `SetThreadExecutionState`, `IOPMAssertion` or `caffeinate` anywhere in the tree). [V]
5. **Best shape:** as is (not reviewed in depth). 6. **Fit:** Pulse **n/a**. 7. None. 8. None.

Depth: light.

## rightkit-now-playing

1. **Exists:** `crates/rightkit-now-playing` (310 lines). 2. **Feeds:** none planned. 3. **Basis:** fresh.
4. Pulse `core/src/remote/media.rs` controls other apps' playback (osascript, SMTC session manager) for a remote caller. The crate publishes this app's own playback. Different capability. [V]
5. As is. 6. Pulse **n/a**. 7. None. 8. None.

Depth: light.

## rightkit-notch-ui

1. **Exists:** `crates/rightkit-notch-ui` (6,433 lines).
2. **Feeds:** Pulse; the plan put these files under `rightkit-overlay-win` EXTEND (`render.rs` 4,746, `layout.rs` 1,027, `motion.rs`, `card.rs`).
3. **Basis:** Pulse `windows/src/render.rs` (34 changed lines), `layout.rs`, `motion.rs`, `card.rs`. [V]
4. The Mac twin is the planned `RightKitNotch`.
5. **Best shape:** generic notch geometry, motion and a card container; cells defined by the app. Today the cell set is Pulse's.
6. **Fit:** Pulse **fits**. Any second app **wrong**.
7. **Boundary:** `layout.rs:132-175` defines `Cell::Claude`, `Cell::Codex`, `Profile`, `Account` with settings groups "accounts", "claude", "openai"; `card.rs:12-13` has `Claude` and `Codex` marks; `render.rs` keeps colour thresholds in process-wide atomics set by "the hub's Colour settings" (`render.rs:30-40`). [V]
8. **Actions:** make cells and marks host-defined; keep the AI rings in Pulse or in an AI-accounts UI crate.

Depth: deep on boundaries.

## rightkit-win-ui

1. **Exists:** `crates/rightkit-win-ui` (3,454 lines).
2. **Feeds:** Pulse (planned under overlay-win): `canvas.rs`, `glyphs.rs`, `surface.rs`, `text_layout.rs`, `raii.rs`, `fileicon.rs`.
3. **Basis:** Pulse Windows, verbatim. [V]
4. **Side by side with `rightkit-overlay-win`:** two software rasterisers for the same target. `rightkit-overlay-win/src/paint.rs:9-13` says it is "lifted in spirit from the Pulse notch canvas ... and from the HeardRight spike pill"; `rightkit-win-ui/src/canvas.rs` is that Pulse canvas itself. Both produce premultiplied `0xAARRGGBB` for `UpdateLayeredWindow`. overlay-win works in logical pixels with DPI scaling and has paths and squircles; win-ui works in raw pixels with `0xRRGGBB` colours. HeardRight main uses overlay-win (`heardright-main/tauri-app-next/src-tauri/src/pill_native_win/gfx.rs`). [V]
5. **Best shape:** one Win32 software canvas. HeardRight is the higher-priority app, so `rightkit-overlay-win`'s `PaintCtx` is the reference; win-ui keeps RAII wrappers, glyphs, text painter and file icons and drops its own canvas, or the two merge.
6. **Fit:** Pulse **fits** today; HeardRight **n/a** (uses overlay-win).
7. **Boundary:** duplicate rasteriser across two crates. [V]
8. **Actions:** choose one canvas; port `rightkit-notch-ui` and the clipboard strip to it; then publish.

Depth: deep on the overlap, light on internals.

## rightkit-autostart

1. **Exists:** `crates/rightkit-autostart` (1,046 lines). 2. **Feeds:** none planned here (Pulse `windows/src/autostart.rs`, 342 lines, was mapped to `rightkit-tauri-shell` ADOPT). 3. **Basis:** fresh, split from the Tauri shell.
4. Pulse writes one value named "Pulse" under HKCU `...\Run` with hand-declared registry calls (`P/windows/src/autostart.rs:1-12`) and reads StartupApproved (`:327-335`). The crate does the same with `AutostartSpec`, `status`, `set_enabled`, `startup_approved_is_disabled`. [V] by signatures, not line by line.
5. As is. 6. Pulse **fits** [S]. 7. Pulse's file is a duplicate to delete. 8. None.

Depth: light.

## rightkit-notifications

1. **Exists:** `crates/rightkit-notifications` (1,014 lines). 2. **Feeds:** none planned here (Pulse `windows/src/notify.rs` was under overlay-win). 3. **Basis:** fresh.
4. Pulse raises Windows toasts through a hidden PowerShell under PowerShell's own app id because it installs no Start-menu shortcut (`P/windows/src/notify.rs:5-11`), and plays stock sounds with `PlaySoundW`. The crate uses WinRT toasts with a real app id and has named system sounds. Adopting it changes Pulse's behaviour for the better but needs the installer to create the shortcut. [V] for Pulse; crate not read in depth.
5. As is. 6. Pulse **gaps** [S] (installer shortcut with the app id; a sound-only alert if the crate has none). 7. None. 8. Confirm a sound-only call exists.

Depth: light.

---

## Family summary

Counts by shape (24 rows): **best 6** (device-link-types, clipboard, presence, power, now-playing, autostart), **gaps 11** (mesh, mesh-ffi, RightKitMeshLink, device-link, disk-cleanup, app-manager, system-monitor, clipboard-history, app-lock, privileged-helper, send, record, win-ui, notifications: see JSON for the exact split), **wrong 4** (launcher, @rightkit/system-ui, desktop-tweaks, notch-ui), **missing 2** (RightKitNotch, RightKitSystemUI).

Exact counts are in the JSON: best 6, gaps 12, wrong 4, missing 2.

Most important:

1. The three known facts hold. rightkit-mesh has Pulse's transport and device-link core but not the 12,000-line host (`devicelink_host.rs`, `share_mesh.rs`). RightKitNotch and RightKitSystemUI do not exist. Pulse's repo holds `pulse-mesh` and `pulse-mesh-ffi`, file-for-file duplicates.
2. Pulse does not yet depend on any of the lifted crates, so every one of them is currently a second copy.
3. Four packages carry Pulse's product and should be redrawn before publication: launcher (Pulse pages and strings in `panel.rs`), notch-ui (`Cell::Claude`/`Codex`), @rightkit/system-ui (102 direct Tauri calls, 36 "Pulse" strings, chat and onboarding bundled), desktop-tweaks (holds a different feature from the one planned).
4. Launcher and clipboard history exist twice inside Pulse (Swift and Rust). RightKit has only the Rust half; "no duplicate code" needs an FFI-or-Swift decision.
5. Two Win32 software canvases: `rightkit-overlay-win` (HeardRight uses it) and `rightkit-win-ui` (Pulse's). One should win; HeardRight's is the reference by priority.
6. Identical files in two crates: `json.rs` (launcher, clipboard-history) and `snapshot.rs` (disk-cleanup, system-monitor).
7. Mis-mapped by the plan: `engine/src/journal/*` (4,877, agent task journal) under disk-cleanup; `windows/src/usage.rs` (3,013, AI usage) under system-monitor; `viewshots.rs` and HeardRight `shot/*` (QA) under record; HeardRight's Watch and account files under mesh; Mac clipboard Swift under launcher.
8. app-lock mixes MailRight's app lock with Pulse's owner-approval proofs (`hello_key`, `pin_window`); the latter belong in owner-approvals.
9. ScrapeRight waits on a Swift `RightKitSend` engine that does not exist.
