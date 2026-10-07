# RightKitSwift

Shared Apple-platform infrastructure for the Right Suite iOS apps — one source of
truth for the pieces every app would otherwise reimplement and let drift.

| Target | What it does |
|---|---|
| `RightKitLicense` | Verifies Ed25519 entitlement tokens against the same `contracts/license-v2.json` vector the Rust and TypeScript verifiers use, so a token accepted on one platform is accepted on all. |
| `RightKitDiagnostics` | Bounded JSONL diagnostics with a privacy redactor (never logs keys, tokens, or device ids). |
| `RightKitArtifacts` | Checksummed artifact install with staging, atomic replace, rollback, and archive-traversal/symlink/expansion guards. |
| `RightKitUI` | Generic adaptive-glass surface primitive: regular/clear native Liquid Glass + ConcentricRectangle on iOS/macOS 26, material fallback on older OS versions, and opaque accessibility fallback. Product radii, colors, layout, motion, and behavior stay app-local. |

## Consuming it

Swift has no public package registry like npmjs.com or crates.io (Apple is
building one via the Swift Package Index, not yet live), and SwiftPM is
git-URL-native, so apps pin this repo by URL + version tag — the same
"published, pinned, reproducible" property npm/crates give, with no registry to
host.

Pins are always exact (`exact:`), never `from:` ranges: a consumer upgrade is a deliberate
edit of the pin, recorded in `rightkit-versions.json`.

```swift
// Package.swift
dependencies: [
    .package(url: "https://github.com/bogusyogi/rightkit-swift.git", exact: "0.3.1"),
],
targets: [
    .target(name: "YourApp", dependencies: [
        .product(name: "RightKitLicense", package: "rightkit-swift"),
    ]),
]
```

In an XcodeGen `project.yml`:

```yaml
packages:
  RightKitSwift:
    url: https://github.com/bogusyogi/rightkit-swift.git
    exactVersion: "0.3.1"
```

A clone of a consuming app resolves this straight from GitHub — no workspace
checkout required.

## Source of truth

This repo is the **consumption mirror**. The canonical source is
`tools/rightkit/swift/RightKitSwift` in the workspace repo; a release copies that
tree here and tags it, exactly as the npm/crates packages publish from the same
workspace. Do not edit here directly — change the workspace source, then cut a
new tag.

## Releasing a new version

From the workspace: bump `RightKitSwift/VERSION`, re-run `swift test`, then mirror
the tracked tree into this repo and tag `X.Y.Z`. Record the new version in
`tools/rightkit/packages/release/rightkit-versions.json` (`swift` section) so apps
can bump their pin.
