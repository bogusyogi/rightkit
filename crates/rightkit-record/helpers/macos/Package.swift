// swift-tools-version: 6.0
import PackageDescription

// rightkit-record macOS helper: a signed Swift binary owned by rightkit-record and launched
// through rightkit-child-process. It speaks the rightkit-record helper wire protocol (4-byte big-endian
// length, then one JSON object) on stdin/stdout and captures one window with ScreenCaptureKit.
// `build.sh` builds and ad-hoc signs it for tests. Production signing belongs to the release tooling.
let package = Package(
    name: "RightKitRecordMacOS",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "rightkit-record-macos-helper", targets: ["rightkit-record-macos-helper"]),
        .executable(name: "rk-fixture-window", targets: ["rk-fixture-window"]),
    ],
    targets: [
        // Pure Foundation: BLAKE3, the frame codec and the protocol messages. No ScreenCaptureKit.
        .target(
            name: "RecordWire",
            swiftSettings: [.unsafeFlags(["-Ounchecked"], .when(configuration: .release))]
        ),
        .executableTarget(
            name: "rightkit-record-macos-helper",
            dependencies: ["RecordWire"]
        ),
        // A tiny AppKit window placed fully off every display, for live tests only.
        .executableTarget(name: "rk-fixture-window"),
        .testTarget(
            name: "RecordWireTests",
            dependencies: ["RecordWire"],
            exclude: ["golden.json"]
        ),
    ],
    swiftLanguageModes: [.v5]
)
