# Pulse — RightKit migration handoff

Source root: /Volumes/D/claude/pulse

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/pulse.json)

## rightkit-fs — N

Pulse has active app-owned implementation overlapping cargo:rightkit-fs on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-fs at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/core/src/app_manager.rs:622) — Pulse locally implements temp-write/rename state writes instead of rightkit-fs.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lib.rs:45) — Owner exports atomic_write/private/state and lock APIs.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-hotkeys — N

Pulse has active app-owned implementation overlapping cargo:rightkit-hotkeys on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-hotkeys at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Conveniences/EventTapHub.swift:17) — Active Mac notch owns CGEventTap registration.
- Local: [active production integration](/Volumes/D/claude/pulse/windows/src/keys.rs:1) — Active Windows notch owns WH_KEYBOARD_LL/RegisterHotKey hook layer.
- Owner: [mod.rs](/Volumes/D/claude/rightkit/crates/rightkit-hotkeys/src/windows/mod.rs:104) — Owner LowLevelKeyboardHook::install path is the matching Windows hotkey API; Pulse windows/src/keys.rs owns same runtime behavior.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-http — N

Pulse has active app-owned implementation overlapping cargo:rightkit-http on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-http at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Updater/ReleaseFeed.swift:50) — Mac updater uses URLSession/URLRequest directly.
- Local: [active production integration](/Volumes/D/claude/pulse/windows/src/http.rs:1) — Windows updater defines direct WinHTTP client.
- Owner: [client.rs](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:772) — Owner Client::send path is the matching Rust transport API; Pulse windows/src/http.rs owns same runtime behavior.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-logs — N

Pulse has active app-owned implementation overlapping cargo:rightkit-logs on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-logs at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/windows/src/diag.rs:50) — Windows notch writes custom structured stderr/notch.log diagnostics.
- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/App/Log.swift:7) — Mac notch uses local Logger categories.
- Owner: [store.rs](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/store.rs:29) — Owner JsonlStore::append API is the matching Rust logging primitive; Pulse windows/src/diag.rs owns same runtime logging.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-overlay — N

Pulse has active app-owned implementation overlapping cargo:rightkit-overlay on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-overlay at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Notch/NotchPanel.swift:6) — Active Mac notch owns NSPanel overlay behavior.
- Local: [active production integration](/Volumes/D/claude/pulse/windows/src/main.rs:1) — Active Windows notch owns layered topmost panel/window interaction.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-overlay/src/lib.rs:186) — Owner make_noactivate/reassert overlay policy is the matching native window API; Pulse Windows notch owns same runtime behavior.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-process — N

Pulse has active app-owned implementation overlapping cargo:rightkit-process on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-process at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/System/ClaudeRestart.swift:107) — Mac notch launches/waits/terminates child processes directly.
- Local: [active production integration](/Volumes/D/claude/pulse/windows/src/update.rs:764) — Windows updater launches installer directly.
- Owner: [process_handle.rs](/Volumes/D/claude/rightkit/crates/rightkit-process/src/process_handle.rs:131) — Owner ProcessHandle::try_wait/wait API is the matching process primitive; Pulse Windows updater owns same runtime process control.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-send — N

Pulse has active app-owned implementation overlapping cargo:rightkit-send on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-send at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/core/src/localsend/mod.rs:1) — Pulse production hub implements LocalSend v2 Service locally.
- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Sharing/NearbySharing.swift:173) — Active Swift notch reads share-state and sends commands for local nearby sharing path.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-send/src/lib.rs:634) — Owner Service::start/send service API is the matching shared send route; Pulse core/localsend plus Mac NearbySharing remain active local production paths and have no dependency/import.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-settings — N

Pulse has active app-owned implementation overlapping cargo:rightkit-settings on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-settings at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Settings/Preferences.swift:6) — Mac notch persists settings in UserDefaults.
- Local: [active production integration](/Volumes/D/claude/pulse/windows/src/settings.rs:1) — Windows notch owns pill-settings.json parser/store.
- Owner: [store.rs](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/store.rs:299) — Owner Store::update API is the matching Rust settings primitive; Pulse Windows settings owns same runtime persistence.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## rightkit-watch — N

Pulse has active app-owned implementation overlapping cargo:rightkit-watch on applicable target(s); no RightKit production route is imported.

Adopt cargo:rightkit-watch at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/hub/src-tauri/src/watch.rs:89) — Hub directly creates FSEventStream for live storage refresh.
- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Conveniences/ClaudeChatMirror.swift:174) — Mac notch directly creates FSEventStream for session mirror.
- Owner: [typed.rs](/Volumes/D/claude/rightkit/crates/rightkit-watch/src/typed.rs:104) — Owner TypedWatcher::watch API is the matching Rust filesystem watcher; Pulse hub/src-tauri/src/watch.rs owns same runtime FSEvents/filesystem watch path.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## @rightkit/settings-ui — N

Pulse has active app-owned implementation overlapping npm:@rightkit/settings-ui on applicable target(s); no RightKit production route is imported.

Adopt npm:@rightkit/settings-ui at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/hub/src/views/Settings.tsx:1) — Hub implements settings page and controls locally.
- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Settings/Preferences.swift:6) — Mac notch owns native settings model.
- Owner: [SettingsPanel.tsx](/Volumes/D/claude/rightkit/packages/settings-ui/src/SettingsPanel.tsx:110) — Owner SettingsPanel component is the matching React settings surface; Pulse hub/src/views/Settings.tsx owns same runtime UI.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## @rightkit/theme — N

Pulse has active app-owned implementation overlapping npm:@rightkit/theme on applicable target(s); no RightKit production route is imported.

Adopt npm:@rightkit/theme at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/hub/src/styles.css:1) — Hub defines app-owned CSS tokens/theme.
- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/DesignSystem/Palette.swift:1) — Mac notch defines app-owned palette/design system.
- Owner: [runtime.ts](/Volumes/D/claude/rightkit/packages/theme/src/runtime.ts:22) — Owner applyBrand runtime theme API is the matching React theme primitive; Pulse hub/src/styles.css owns same runtime token surface.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## RightKitDiagnostics — N

Pulse has active app-owned implementation overlapping swift:RightKitDiagnostics on applicable target(s); no RightKit production route is imported.

Adopt swift:RightKitDiagnostics at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/App/Log.swift:7) — Active Mac notch uses local Logger instead of RightKitDiagnostics.
- Owner: [JSONLDiagnosticStore.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:128) — Owner JSONLDiagnosticStore::append API is the matching Swift diagnostics path; Pulse Mac Logger owns same runtime logging.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## RightKitAppleCredentials — N

Pulse has active app-owned implementation overlapping swift:RightKitAppleCredentials on applicable target(s); no RightKit production route is imported.

Adopt swift:RightKitAppleCredentials at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Providers/KeychainItem.swift:55) — Active Mac notch reads credentials through Security SecItemCopyMatching.
- Owner: [RightKitAppleCredentials.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:103) — Owner AppleCredentialStore::read API is the matching Security-backed credential path; Pulse KeychainItem owns same runtime credentials.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## RightKitOverlay — N

Pulse has active app-owned implementation overlapping swift:RightKitOverlay on applicable target(s); no RightKit production route is imported.

Adopt swift:RightKitOverlay at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Notch/NotchPanel.swift:6) — Active Mac notch owns NSPanel overlay.
- Owner: [Overlay.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitOverlay/Overlay.swift:179) — Owner provides RightKitOverlayPanel/placement/hit regions.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## RightKitWatch — N

Pulse has active app-owned implementation overlapping swift:RightKitWatch on applicable target(s); no RightKit production route is imported.

Adopt swift:RightKitWatch at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Conveniences/ClaudeChatMirror.swift:174) — Active Mac notch owns FSEventStream session mirror.
- Owner: [RightKitWatch.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatch/RightKitWatch.swift:192) — Owner RightKitWatcher class over FSEvents is the matching Swift watch API; Pulse ClaudeChatMirror owns same runtime watcher.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

## RightKitUI — N

Pulse has active app-owned implementation overlapping swift:RightKitUI on applicable target(s); no RightKit production route is imported.

Adopt swift:RightKitUI at owning runtime boundary, preserve Pulse policy/UI, route callers through owner API, then remove duplicate local implementation after focused parity evidence.

- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/DesignSystem/Design.swift:1) — Active Mac notch owns SwiftUI design system.
- Local: [active production integration](/Volumes/D/claude/pulse/mac/Notch/Sources/Notch/NotchRootView.swift:1) — Active Mac notch owns SwiftUI notch UI.
- Owner: [AdaptiveGlassSurface.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitUI/AdaptiveGlassSurface.swift:37) — Owner rightKitAdaptiveGlass SwiftUI modifier is the matching UI primitive; Pulse Design.swift/NotchRootView own same runtime surface.

Delivery: No direct RightKit dependency/import for this capability in active Pulse manifests.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
