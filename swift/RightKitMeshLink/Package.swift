// swift-tools-version: 6.0
import PackageDescription

// MeshLink: a phone's side of the Orthic device mesh (pair with a computer by scanning its code,
// then send one request per stream). App-neutral. Handed over from Pulse (rightkit#786).
//
// It is its own package, not a RightKitSwift product, because RightKitSwift is source only and this
// one links a Rust static library: the binary target is built from crates/rightkit-mesh-ffi (see
// BUILD.md) and is not committed (see .gitignore).
let package = Package(
    name: "RightKitMeshLink",
    platforms: [.iOS(.v18)],
    products: [
        .library(name: "MeshLink", targets: ["MeshLink"]),
    ],
    dependencies: [
        // Only RightKitCredentials is used. In this repository it is the sibling package.
        .package(path: "../RightKitSwift"),
    ],
    targets: [
        .binaryTarget(name: "RightKitMeshFFI", path: "RightKitMeshFFI.xcframework"),
        .target(
            name: "MeshLink",
            dependencies: [
                "RightKitMeshFFI",
                .product(name: "RightKitCredentials", package: "RightKitSwift"),
            ],
            // The Rust library reads the system DNS configuration (iroh's resolver) and uses the keychain.
            linkerSettings: [.linkedFramework("SystemConfiguration"), .linkedFramework("Security")]
        ),
    ]
)
