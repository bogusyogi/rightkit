// swift-tools-version: 6.1
import PackageDescription

// ROOT MANIFEST for URL consumption: SwiftPM resolves a remote package only from a Package.swift at the
// repository root, so apps pin `https://github.com/Orthic-Labs/rightkit` by exact tag and get this file.
// It mirrors swift/RightKitSwift/Package.swift (the manifest CI builds and tests in place) with every
// target given an explicit `path:` under swift/RightKitSwift. Keep the two in step: change both in one commit;
// scripts/swift-root-manifest.test.mjs fails on products, targets, dependencies or paths that drift.
// Resource paths (.process/.copy) and linker settings are relative to the target path, so they are unchanged.

// Shared platform infrastructure for the Right Suite's native Apple apps. It owns licensing,
// diagnostics, artifact install, and QA seams — never app UI, product policy, or brand behavior.
// Foundation/CryptoKit only: no third-party dependency, so the registry archive stays portable.
let package = Package(
    name: "RightKitSwift",
    platforms: [.iOS(.v17), .macOS(.v14)],
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
        .library(name: "RightKitPulse", targets: ["RightKitPulse"]),
        .library(name: "RightKitDB", targets: ["RightKitDB"]),
        .library(name: "RightKitFS", targets: ["RightKitFS"]),
        .library(name: "RightKitCredentials", targets: ["RightKitCredentials"]),
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
        .target(name: "RightKitSpeech", path: "swift/RightKitSwift/Sources/RightKitSpeech", resources: [.copy("Resources/whisper_mel_128.bin")]),
        .testTarget(name: "RightKitSpeechTests", dependencies: ["RightKitSpeech"], path: "swift/RightKitSwift/Tests/RightKitSpeechTests"),
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
        .target(name: "RightKitPulse", path: "swift/RightKitSwift/Sources/RightKitPulse"),
        .testTarget(name: "RightKitPulseTests", dependencies: ["RightKitPulse"], path: "swift/RightKitSwift/Tests/RightKitPulseTests"),
        // Read-only reader for rightkit-db databases (Rust is the only writer); system SQLite3.
        .target(name: "RightKitDB", path: "swift/RightKitSwift/Sources/RightKitDB", linkerSettings: [.linkedLibrary("sqlite3")]),
        .testTarget(name: "RightKitDBTests", dependencies: ["RightKitDB"], path: "swift/RightKitSwift/Tests/RightKitDBTests"),
        // Data root (Orthic Labs/<App>), escape-safe dataDir, atomic write, verified copy, append. Foundation/CryptoKit only.
        .target(name: "RightKitFS", path: "swift/RightKitSwift/Sources/RightKitFS"),
        .testTarget(name: "RightKitFSTests", dependencies: ["RightKitFS"], path: "swift/RightKitSwift/Tests/RightKitFSTests"),
    ],
)
