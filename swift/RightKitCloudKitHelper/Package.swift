// swift-tools-version: 6.0
import PackageDescription

// Neutral macOS CloudKit helper for rightkit-sync (Rust). Speaks the line-JSON
// `rightkit-sync-helper/1` protocol over stdin/stdout and reuses RightKitCloudSync's
// CloudKitSyncTransport (change tags, assets, error classification, account scope).
let package = Package(
    name: "RightKitCloudKitHelper",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "rightkit-cloudkit-helper", targets: ["rightkit-cloudkit-helper"]),
    ],
    dependencies: [
        .package(path: "../RightKitSwift"),
    ],
    targets: [
        .executableTarget(
            name: "rightkit-cloudkit-helper",
            dependencies: [.product(name: "RightKitCloudSync", package: "RightKitSwift")]
        ),
        .testTarget(name: "RightKitCloudKitHelperTests", dependencies: ["rightkit-cloudkit-helper"]),
    ]
)
