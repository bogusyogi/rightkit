// swift-tools-version: 6.1
import PackageDescription

// ROOT MANIFEST for URL consumption: SwiftPM resolves a remote package only from a Package.swift at the
// repository root, so apps pin `https://github.com/bogusyogi/rightkit` by exact tag and get this file.
// It mirrors swift/RightKitSwift/Package.swift (the manifest CI builds and tests in place) with every
// target given an explicit `path:` under swift/RightKitSwift. Keep the two in step: change both in one commit;
// scripts/swift-root-manifest.test.mjs fails on products, targets, dependencies or paths that drift.
// Resource paths (.process/.copy) and linker settings are relative to the target path, so they are unchanged.

// Shared platform infrastructure for the Right Suite's native Apple apps. It owns licensing,
// diagnostics, artifact install, and QA seams — never app UI, product policy, or brand behavior.
// Foundation/CryptoKit only: no third-party dependency, so the registry archive stays portable.
let package = Package(
    name: "RightKitSwift",
    platforms: [.iOS(.v17), .macOS(.v14), .watchOS("10.2")],
    products: [
        .library(name: "RightKitLicense", targets: ["RightKitLicense"]),
        .library(name: "RightKitDiagnostics", targets: ["RightKitDiagnostics"]),
        .library(name: "RightKitArtifacts", targets: ["RightKitArtifacts"]),
        .library(name: "RightKitPurchases", targets: ["RightKitPurchases"]),
        .library(name: "RightKitCloudSync", targets: ["RightKitCloudSync"]),
        .library(name: "RightKitStore", targets: ["RightKitStore"]),
        .library(name: "RightKitImportQueue", targets: ["RightKitImportQueue"]),
        .library(name: "RightKitLiveActivity", targets: ["RightKitLiveActivity"]),
        .library(name: "RightKitWatchBridge", targets: ["RightKitWatchBridge"]),
        .library(name: "RightKitSpeech", targets: ["RightKitSpeech"]),
        .library(name: "RightKitIntents", targets: ["RightKitIntents"]),
        .library(name: "RightKitAppleCredentials", targets: ["RightKitAppleCredentials"]),
        .library(name: "RightKitOverlay", targets: ["RightKitOverlay"]),
        .library(name: "RightKitWatch", targets: ["RightKitWatch"]),
        .library(name: "RightKitUI", targets: ["RightKitUI"]),
        .library(name: "RightKitDB", targets: ["RightKitDB"]),
        .library(name: "RightKitFS", targets: ["RightKitFS"]),
        .library(name: "RightKitCredentials", targets: ["RightKitCredentials"]),
        .library(name: "RightKitAccessibility", targets: ["RightKitAccessibility"]),
        .library(name: "RightKitHotkeys", targets: ["RightKitHotkeys"]),
        .library(name: "RightKitAudio", targets: ["RightKitAudio"]),
        .library(name: "RightKitTTS", targets: ["RightKitTTS"]),
        .library(name: "RightKitHTTP", targets: ["RightKitHTTP"]),
        .library(name: "RightKitLLM", targets: ["RightKitLLM"]),
        .library(name: "RightKitUpdate", targets: ["RightKitUpdate"]),
        .library(name: "RightKitOAuth", targets: ["RightKitOAuth"]),
        .library(name: "RightKitAccount", targets: ["RightKitAccount"]),
        .library(name: "RightKitHelperProtocol", targets: ["RightKitHelperProtocol"]),
        .library(name: "RightKitSystem", targets: ["RightKitSystem"]),
        .library(name: "RightKitEngineClient", targets: ["RightKitEngineClient"]),
        .library(name: "RightKitEngine", targets: ["RightKitEngine"]),
        .library(name: "RightKitHub", targets: ["RightKitHub"]),
        .library(name: "RightKitHubQA", targets: ["RightKitHubQA"]),
        // macOS CloudKit helper for rightkit-sync (rightkit#1302). Apps build it with
        // `swift build --product rightkit-cloudkit-helper`; the body is compiled on macOS only.
        .executable(name: "rightkit-cloudkit-helper", targets: ["rightkit-cloudkit-helper"]),
    ],
    targets: [
        // Keychain-backed secret store (namespace = service, key = account); RightKitLicense persists through it.
        .target(name: "RightKitCredentials", path: "swift/RightKitSwift/Sources/RightKitCredentials"),
        .testTarget(name: "RightKitCredentialsTests", dependencies: ["RightKitCredentials"], path: "swift/RightKitSwift/Tests/RightKitCredentialsTests"),
        .target(name: "RightKitLicense", dependencies: ["RightKitCredentials"], path: "swift/RightKitSwift/Sources/RightKitLicense"),
        .target(name: "RightKitDiagnostics", path: "swift/RightKitSwift/Sources/RightKitDiagnostics"),
        .testTarget(name: "RightKitDiagnosticsTests", dependencies: ["RightKitDiagnostics"], path: "swift/RightKitSwift/Tests/RightKitDiagnosticsTests"),
        // zlib comes from the platform SDK; no third-party dependency enters the registry archive.
        .target(name: "RightKitArtifacts", path: "swift/RightKitSwift/Sources/RightKitArtifacts", linkerSettings: [.linkedLibrary("z")]),
        .testTarget(name: "RightKitArtifactsTests", dependencies: ["RightKitArtifacts"], path: "swift/RightKitSwift/Tests/RightKitArtifactsTests"),
        .testTarget(
            name: "RightKitLicenseTests",
            dependencies: ["RightKitLicense", "RightKitCredentials"],
            path: "swift/RightKitSwift/Tests/RightKitLicenseTests",
            resources: [.process("Contracts")],
        ),
        // StoreKit 2 purchases, entitlement ledger, and the bridge into RightKitLicense access.
        .target(name: "RightKitPurchases", dependencies: ["RightKitLicense"], path: "swift/RightKitSwift/Sources/RightKitPurchases"),
        .testTarget(
            name: "RightKitPurchasesTests",
            dependencies: ["RightKitPurchases", "RightKitLicense"],
            path: "swift/RightKitSwift/Tests/RightKitPurchasesTests",
            resources: [.copy("StoreKit/RightKitPurchasesTests.storekit")],
        ),
        // Generic CloudKit record sync; Foundation/CloudKit/Network only.
        .target(name: "RightKitCloudSync", path: "swift/RightKitSwift/Sources/RightKitCloudSync"),
        .testTarget(name: "RightKitCloudSyncTests", dependencies: ["RightKitCloudSync"], path: "swift/RightKitSwift/Tests/RightKitCloudSyncTests"),
        // App Group primitives, share-extension intake queue, Live Activity lifecycle, WatchConnectivity
        // delivery ledger. Foundation-only (ActivityKit/WatchConnectivity guarded by canImport).
        .target(name: "RightKitStore", path: "swift/RightKitSwift/Sources/RightKitStore"),
        .testTarget(name: "RightKitStoreTests", dependencies: ["RightKitStore"], path: "swift/RightKitSwift/Tests/RightKitStoreTests"),
        .target(name: "RightKitImportQueue", dependencies: ["RightKitStore"], path: "swift/RightKitSwift/Sources/RightKitImportQueue"),
        .testTarget(name: "RightKitImportQueueTests", dependencies: ["RightKitImportQueue"], path: "swift/RightKitSwift/Tests/RightKitImportQueueTests"),
        .target(name: "RightKitLiveActivity", path: "swift/RightKitSwift/Sources/RightKitLiveActivity"),
        .testTarget(name: "RightKitLiveActivityTests", dependencies: ["RightKitLiveActivity"], path: "swift/RightKitSwift/Tests/RightKitLiveActivityTests"),
        .target(name: "RightKitWatchBridge", path: "swift/RightKitSwift/Sources/RightKitWatchBridge"),
        .testTarget(name: "RightKitWatchBridgeTests", dependencies: ["RightKitWatchBridge"], path: "swift/RightKitSwift/Tests/RightKitWatchBridgeTests"),
        .target(name: "RightKitSpeech", dependencies: ["RightKitAudio", "RightKitArtifacts"], path: "swift/RightKitSwift/Sources/RightKitSpeech", resources: [.copy("Resources/whisper_mel_128.bin")]),
        .testTarget(name: "RightKitSpeechTests", dependencies: ["RightKitSpeech", "RightKitAudio", "RightKitArtifacts"], path: "swift/RightKitSwift/Tests/RightKitSpeechTests"),
        .target(name: "RightKitIntents", path: "swift/RightKitSwift/Sources/RightKitIntents"),
        .testTarget(name: "RightKitIntentsTests", dependencies: ["RightKitIntents"], path: "swift/RightKitSwift/Tests/RightKitIntentsTests"),
        .target(name: "RightKitAppleCredentials", path: "swift/RightKitSwift/Sources/RightKitAppleCredentials"),
        .testTarget(name: "RightKitAppleCredentialsTests", dependencies: ["RightKitAppleCredentials"], path: "swift/RightKitSwift/Tests/RightKitAppleCredentialsTests"),
        .target(name: "RightKitOverlay", path: "swift/RightKitSwift/Sources/RightKitOverlay"),
        .testTarget(name: "RightKitOverlayTests", dependencies: ["RightKitOverlay"], path: "swift/RightKitSwift/Tests/RightKitOverlayTests"),
        // FSEvents file watcher (macOS only; the target is empty on iOS). CoreServices from the SDK.
        .target(name: "RightKitWatch", path: "swift/RightKitSwift/Sources/RightKitWatch"),
        .testTarget(name: "RightKitWatchTests", dependencies: ["RightKitWatch"], path: "swift/RightKitSwift/Tests/RightKitWatchTests"),
        .target(name: "RightKitUI", path: "swift/RightKitSwift/Sources/RightKitUI"),
        .testTarget(name: "RightKitUITests", dependencies: ["RightKitUI"], path: "swift/RightKitSwift/Tests/RightKitUITests"),
        // Pulse facade over the proposed rightkit-job-queue C ABI seam (design selection, not a qualified adoption).
        // Read-only reader for rightkit-db databases (Rust is the only writer); system SQLite3.
        .target(name: "RightKitDB", path: "swift/RightKitSwift/Sources/RightKitDB", linkerSettings: [.linkedLibrary("sqlite3")]),
        .testTarget(name: "RightKitDBTests", dependencies: ["RightKitDB"], path: "swift/RightKitSwift/Tests/RightKitDBTests"),
        // Data root (Orthic Labs/<App>), escape-safe dataDir, atomic write, verified copy, append. Foundation/CryptoKit only.
        .target(name: "RightKitFS", path: "swift/RightKitSwift/Sources/RightKitFS"),
        .testTarget(name: "RightKitFSTests", dependencies: ["RightKitFS"], path: "swift/RightKitSwift/Tests/RightKitFSTests"),
        // Accessibility (AXUIElement reads/writes, window actions, AXObserver) and Carbon hot keys (macOS only; both targets are empty on iOS).
        .target(name: "RightKitAccessibility", path: "swift/RightKitSwift/Sources/RightKitAccessibility"),
        .testTarget(name: "RightKitAccessibilityTests", dependencies: ["RightKitAccessibility"], path: "swift/RightKitSwift/Tests/RightKitAccessibilityTests"),
        .target(name: "RightKitHotkeys", path: "swift/RightKitSwift/Sources/RightKitHotkeys"),
        .testTarget(name: "RightKitHotkeysTests", dependencies: ["RightKitHotkeys"], path: "swift/RightKitSwift/Tests/RightKitHotkeysTests"),
        .target(name: "RightKitAudio", path: "swift/RightKitSwift/Sources/RightKitAudio"),
        .testTarget(name: "RightKitAudioTests", dependencies: ["RightKitAudio"], path: "swift/RightKitSwift/Tests/RightKitAudioTests"),
        .target(name: "RightKitTTS", dependencies: ["RightKitAudio"], path: "swift/RightKitSwift/Sources/RightKitTTS"),
        .testTarget(name: "RightKitTTSTests", dependencies: ["RightKitTTS", "RightKitAudio"], path: "swift/RightKitSwift/Tests/RightKitTTSTests"),
        .target(name: "RightKitHTTP", path: "swift/RightKitSwift/Sources/RightKitHTTP"),
        .testTarget(name: "RightKitHTTPTests", dependencies: ["RightKitHTTP"], path: "swift/RightKitSwift/Tests/RightKitHTTPTests"),
        .target(name: "RightKitLLM", dependencies: ["RightKitHTTP", "RightKitCredentials"], path: "swift/RightKitSwift/Sources/RightKitLLM"),
        .testTarget(name: "RightKitLLMTests", dependencies: ["RightKitLLM", "RightKitHTTP"], path: "swift/RightKitSwift/Tests/RightKitLLMTests"),
        .target(name: "RightKitUpdate", dependencies: ["RightKitHTTP"], path: "swift/RightKitSwift/Sources/RightKitUpdate"),
        .testTarget(name: "RightKitUpdateTests", dependencies: ["RightKitUpdate", "RightKitHTTP"], path: "swift/RightKitSwift/Tests/RightKitUpdateTests"),
        .target(name: "RightKitOAuth", dependencies: ["RightKitHTTP", "RightKitCredentials"], path: "swift/RightKitSwift/Sources/RightKitOAuth"),
        .testTarget(name: "RightKitOAuthTests", dependencies: ["RightKitOAuth", "RightKitHTTP", "RightKitCredentials"], path: "swift/RightKitSwift/Tests/RightKitOAuthTests"),
        .target(name: "RightKitAccount", dependencies: ["RightKitHTTP", "RightKitCredentials", "RightKitLicense"], path: "swift/RightKitSwift/Sources/RightKitAccount"),
        .testTarget(name: "RightKitAccountTests", dependencies: ["RightKitAccount", "RightKitHTTP", "RightKitCredentials", "RightKitLicense"], path: "swift/RightKitSwift/Tests/RightKitAccountTests"),
        .target(name: "RightKitHelperProtocol", path: "swift/RightKitSwift/Sources/RightKitHelperProtocol"),
        .testTarget(name: "RightKitHelperProtocolTests", dependencies: ["RightKitHelperProtocol"], path: "swift/RightKitSwift/Tests/RightKitHelperProtocolTests"),
        .target(name: "RightKitSystem", path: "swift/RightKitSwift/Sources/RightKitSystem"),
        .testTarget(name: "RightKitSystemTests", dependencies: ["RightKitSystem"], path: "swift/RightKitSwift/Tests/RightKitSystemTests"),
        .target(name: "CRightKitEngine", path: "swift/RightKitSwift/Sources/CRightKitEngine"),
        .target(name: "RightKitEngineClient", path: "swift/RightKitSwift/Sources/RightKitEngineClient"),
        .target(name: "RightKitEngine", dependencies: ["CRightKitEngine", "RightKitEngineClient"], path: "swift/RightKitSwift/Sources/RightKitEngine"),
        .target(name: "RightKitHub", dependencies: ["RightKitEngineClient"], path: "swift/RightKitSwift/Sources/RightKitHub"),
        .target(name: "RightKitHubQA", dependencies: ["RightKitHub"], path: "swift/RightKitSwift/Sources/RightKitHubQA"),
        .testTarget(name: "RightKitEngineClientTests", dependencies: ["RightKitEngineClient"], path: "swift/RightKitSwift/Tests/RightKitEngineClientTests"),
        .testTarget(name: "RightKitHubTests", dependencies: ["RightKitHub", "RightKitHubQA"], path: "swift/RightKitSwift/Tests/RightKitHubTests"),
        // Line-JSON `rightkit-sync-helper/1` CloudKit helper over RightKitCloudSync (macOS only; empty elsewhere).
        .executableTarget(name: "rightkit-cloudkit-helper", dependencies: ["RightKitCloudSync"], path: "swift/RightKitSwift/Sources/rightkit-cloudkit-helper"),
        .testTarget(name: "RightKitCloudKitHelperTests", dependencies: ["rightkit-cloudkit-helper"], path: "swift/RightKitSwift/Tests/RightKitCloudKitHelperTests"),
    ],
)
