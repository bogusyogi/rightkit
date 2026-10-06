// swift-tools-version: 6.0
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
    ],
    targets: [
        .target(name: "RightKitLicense"),
        .target(name: "RightKitDiagnostics"),
        .testTarget(name: "RightKitDiagnosticsTests", dependencies: ["RightKitDiagnostics"]),
        // zlib comes from the platform SDK; no third-party dependency enters the registry archive.
        .target(name: "RightKitArtifacts", linkerSettings: [.linkedLibrary("z")]),
        .testTarget(name: "RightKitArtifactsTests", dependencies: ["RightKitArtifacts"]),
        .testTarget(
            name: "RightKitLicenseTests",
            dependencies: ["RightKitLicense"],
            resources: [.process("Contracts")],
        ),
        // StoreKit 2 purchases, entitlement ledger, and the bridge into RightKitLicense access.
        .target(name: "RightKitPurchases", dependencies: ["RightKitLicense"]),
        .testTarget(name: "RightKitPurchasesTests", dependencies: ["RightKitPurchases", "RightKitLicense"]),
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
    ],
)
