# Platform parity review — 2026-10-11

Fresh source review only. No build, test, network, install, or product-source edit was run.

Packet coverage is partial: 198 denominator paths include 115 examined excerpts, 3 truncated paths, and 80 omitted paths. Result is therefore `complete: false`.

## Finding

`RightKitLicense.activateLease` includes `Self.platform()` in its lease payload (`swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:522-527`). `platform()` uses `canImport(UIKit)` to identify iOS/iPadOS, but its non-UIKit branch returns the iOS label (`swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:612-617`). Since this package declares macOS support, macOS activation sends `platform: "ios"`, misrepresenting host platform to the lease service. Fix with an explicit `#if os(macOS)` branch (and platform-specific tests covering macOS, iOS, and iPadOS payloads).

## Documented boundaries reviewed

- `rightkit-fsindex` documents Windows as planned/stub-only; unsupported behavior is intentional.
- `rightkit-secrets` documents Linux as fail-closed with no OS store.
- `rightkit-send` documents iOS discovery as host-supplied, with no multicast/mDNS sockets.
- `rightkit-sandbox` documents policy PTY as unsupported on Linux/Windows while unconfined PTY remains available.
- `RightKitWatch` documents macOS-only FSEvents, with an empty iOS target.
- `rightkit-control` documents macOS/Windows implementations and typed unsupported results elsewhere.

These documented boundaries were not reported as defects.
