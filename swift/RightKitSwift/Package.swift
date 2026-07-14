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
    ],
    targets: [
        .target(name: "RightKitLicense"),
        .target(name: "RightKitDiagnostics"),
        .testTarget(name: "RightKitDiagnosticsTests", dependencies: ["RightKitDiagnostics"]),
        .testTarget(
            name: "RightKitLicenseTests",
            dependencies: ["RightKitLicense"],
            resources: [.process("Contracts")],
        ),
    ],
)
