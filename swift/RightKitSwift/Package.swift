// swift-tools-version: 6.1
import PackageDescription

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
        .target(name: "RightKitCredentials"),
        .testTarget(name: "RightKitCredentialsTests", dependencies: ["RightKitCredentials"]),
        .target(name: "RightKitLicense", dependencies: ["RightKitCredentials"]),
        .target(name: "RightKitDiagnostics"),
        .testTarget(name: "RightKitDiagnosticsTests", dependencies: ["RightKitDiagnostics"]),
        // zlib comes from the platform SDK; no third-party dependency enters the registry archive.
        .target(name: "RightKitArtifacts", linkerSettings: [.linkedLibrary("z")]),
        .testTarget(name: "RightKitArtifactsTests", dependencies: ["RightKitArtifacts"]),
        .testTarget(
            name: "RightKitLicenseTests",
            dependencies: ["RightKitLicense", "RightKitCredentials"],
            resources: [.process("Contracts")],
        ),
        // StoreKit 2 purchases, entitlement ledger, and the bridge into RightKitLicense access.
        .target(name: "RightKitPurchases", dependencies: ["RightKitLicense"]),
        .testTarget(
            name: "RightKitPurchasesTests",
            dependencies: ["RightKitPurchases", "RightKitLicense"],
            resources: [.copy("StoreKit/RightKitPurchasesTests.storekit")],
        ),
        // Generic CloudKit record sync; Foundation/CloudKit/Network only.
        .target(name: "RightKitCloudSync"),
        .testTarget(name: "RightKitCloudSyncTests", dependencies: ["RightKitCloudSync"]),
        // App Group primitives, share-extension intake queue, Live Activity lifecycle, WatchConnectivity
        // delivery ledger. Foundation-only (ActivityKit/WatchConnectivity guarded by canImport).
        .target(name: "RightKitStore"),
        .testTarget(name: "RightKitStoreTests", dependencies: ["RightKitStore"]),
        .target(name: "RightKitImportQueue", dependencies: ["RightKitStore"]),
        .testTarget(name: "RightKitImportQueueTests", dependencies: ["RightKitImportQueue"]),
        .target(name: "RightKitLiveActivity"),
        .testTarget(name: "RightKitLiveActivityTests", dependencies: ["RightKitLiveActivity"]),
        .target(name: "RightKitWatchBridge"),
        .testTarget(name: "RightKitWatchBridgeTests", dependencies: ["RightKitWatchBridge"]),
        .target(name: "RightKitSpeech", resources: [.copy("Resources/whisper_mel_128.bin")]),
        .testTarget(name: "RightKitSpeechTests", dependencies: ["RightKitSpeech"]),
        .target(name: "RightKitIntents"),
        .testTarget(name: "RightKitIntentsTests", dependencies: ["RightKitIntents"]),
        .target(name: "RightKitAppleCredentials"),
        .testTarget(name: "RightKitAppleCredentialsTests", dependencies: ["RightKitAppleCredentials"]),
        .target(name: "RightKitOverlay"),
        .testTarget(name: "RightKitOverlayTests", dependencies: ["RightKitOverlay"]),
        // FSEvents file watcher (macOS only; the target is empty on iOS). CoreServices from the SDK.
        .target(name: "RightKitWatch"),
        .testTarget(name: "RightKitWatchTests", dependencies: ["RightKitWatch"]),
        .target(name: "RightKitUI"),
        .testTarget(name: "RightKitUITests", dependencies: ["RightKitUI"]),
        // Pulse facade over the proposed rightkit-jobs C ABI seam (design selection, not a qualified adoption).
        .target(name: "RightKitPulse"),
        .testTarget(name: "RightKitPulseTests", dependencies: ["RightKitPulse"]),
        // Read-only reader for rightkit-db databases (Rust is the only writer); system SQLite3.
        .target(name: "RightKitDB", linkerSettings: [.linkedLibrary("sqlite3")]),
        .testTarget(name: "RightKitDBTests", dependencies: ["RightKitDB"]),
        // Data root (Orthic Labs/<App>), escape-safe dataDir, atomic write, verified copy, append. Foundation/CryptoKit only.
        .target(name: "RightKitFS"),
        .testTarget(name: "RightKitFSTests", dependencies: ["RightKitFS"]),
    ],
)
