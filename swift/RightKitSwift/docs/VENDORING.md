# Vendoring RightKitSwift products for an open-source consumer

Decision (#969, recorded 2026-11-03): RightKitSwift stays in this private repository, pinned by exact git tag for the suite's own apps. An open-source consumer (an open-source RightPass, whose iOS build cannot resolve a private git URL) vendors the products it needs under the package licence, MIT OR Apache-2.0, using the procedure below. Making a public mirror repository is a publication action and stays Adrian's call; nothing here depends on it, and the procedure still works if a mirror is made later (the consumer then replaces the vendored folder with the mirror's tag).

Evidence for the choice: the six products an open-source RightPass needs have no dependency on each other or on any third-party package (`swift/RightKitSwift/Package.swift`: `RightKitAppleCredentials`, `RightKitCredentials`, `RightKitStore`, `RightKitFS`, `RightKitDiagnostics` and `RightKitOverlay` are plain `.target(name:)` entries with no `dependencies:`), so each is a self-contained folder of Swift sources; the licence note (`LICENSE-NOTE.txt`) is `MIT OR Apache-2.0`; and 62 of 69 crates and every published npm package carry the same licence.

## Procedure

1. Pick the exact tag the suite ships (`swift` in `packages/release/rightkit-versions.json`; `stagedSwift` until it is tagged). Never vendor an untagged commit.
2. In the consumer repository create `Vendor/RightKitSwift/` and copy, from the tag's tree:
   - `swift/RightKitSwift/Sources/<Target>/` for each product needed (`RightKitAppleCredentials`, `RightKitCredentials`, `RightKitStore`, `RightKitFS`, `RightKitDiagnostics`, `RightKitOverlay`);
   - `swift/RightKitSwift/LICENSE-NOTE.txt` and the MIT and Apache-2.0 texts the sibling packages ship (`crates/rightkit-tauri-shell/LICENSE-MIT` and `LICENSE-APACHE`), unchanged, into `Vendor/RightKitSwift/` (the Swift package itself has no licence text files, only the note: when a tag adds them, copy those instead);
   - nothing else: not the tests, not `native/`, not another product.
3. Add `Vendor/RightKitSwift/PROVENANCE.txt` with the source URL, the exact tag, the commit SHA and the list of copied targets, so the copy can be diffed against the tag later.
4. Write `Vendor/RightKitSwift/Package.swift` with `platforms: [.iOS(.v17), .macOS(.v14), .watchOS("10.2")]`, `swift-tools-version: 6.1`, one `.target(name: "<Target>", path: "Sources/<Target>")` and one `.library` per vendored target, and no `dependencies:`. Targets that link a system library keep their `linkerSettings` from the upstream manifest (`RightKitArtifacts` links `z`; none of the six does).
5. In the consumer's app manifest add `.package(path: "Vendor/RightKitSwift")` and reference the products with `.product(name: "RightKitCredentials", package: "RightKitSwift")`.
6. Keep the licence header lines in the copied files; add the licence texts to the consumer's third-party notices.
7. Upgrading is a deliberate repeat of steps 1 to 3 at the new tag. Do not edit vendored files: a fix goes to this repository and arrives with the next tag, so the vendored copy never diverges.

## What the consumer must not take

The suite-only products (`RightKitLicense` activation endpoints, `RightKitUpdate`, `RightKitHub`, `RightKitEngine`) carry suite behaviour and, for `RightKitEngine`, a native archive; they are outside the open-source list and stay out of a vendored copy.
