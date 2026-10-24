// swift-tools-version: 6.1
import PackageDescription

// Link probe (not shipped: the consumers' manifest is the package one level up from here, which never lists this fixture).
// `SpeechNoAppleProbe` imports only RightKitSpeech; `run-probe.sh` asserts its binary has no Speech.framework load command
// and no Speech symbols (HeardRight App Store policy). `AppleControlProbe` imports the fixture-local `SpeechControl` target (`import Speech`, references
// SFSpeechRecognizer) and MUST show Speech.framework: the positive control that proves the scanner can see it. This is the one
// sanctioned `import Speech` under swift/RightKitSwift; RightKitSwift itself no longer contains Apple Speech.
let package = Package(
    name: "SpeechNoApple",
    platforms: [.iOS(.v17), .macOS(.v14)],
    dependencies: [.package(path: "../..")],
    targets: [
        .executableTarget(
            name: "SpeechNoAppleProbe",
            dependencies: [.product(name: "RightKitSpeech", package: "RightKitSwift")]
        ),
        .target(name: "SpeechControl"),
        .executableTarget(
            name: "AppleControlProbe",
            dependencies: [
                .product(name: "RightKitSpeech", package: "RightKitSwift"),
                "SpeechControl",
            ]
        ),
    ],
)
