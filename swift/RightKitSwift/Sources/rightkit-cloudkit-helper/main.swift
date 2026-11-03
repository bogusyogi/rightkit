#if os(macOS)
// rightkit-cloudkit-helper — long-lived stdin/stdout helper that gives rightkit-sync (Rust) access
// to the user's private CloudKit database. Protocol: see Session.swift and the rightkit-sync API.md.
//
//   rightkit-cloudkit-helper --container iCloud.com.example.app --zone Items --subscription items-sub
//
// CloudKit refuses to run unless this binary is packaged as its own signed .app bundle with the
// iCloud container entitlement and an embedded provisioning profile (see docs/cloudkit-helper/README.md); a bare
// Mach-O is killed at launch. Logs go to stderr only: stdout is protocol-only.

import Foundation
import RightKitCloudSync
#if canImport(CloudKit)
import CloudKit
#endif

func argument(_ name: String) -> String? {
    let args = CommandLine.arguments
    guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
    return args[index + 1]
}

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("rightkit-cloudkit-helper: \(message)\n".utf8))
    exit(2)
}

let backend = argument("--backend") ?? "cloudkit"
let session: HelperSession
switch backend {
case "cloudkit":
    #if canImport(CloudKit)
    guard let container = argument("--container"), let zone = argument("--zone"),
          let subscription = argument("--subscription")
    else { fail("--container, --zone and --subscription are required") }
    // CloudKit TRAPS, it does not throw, when the signature lacks the container or service entitlement
    // (#1307): refuse with a typed failure before any CKContainer is created.
    let entitlement = CloudKitEntitlement.current(containerIdentifier: container)
    guard entitlement.allowsCloudKit else {
        fail("not entitled to CloudKit container \(container): \(entitlement)")
    }
    session = HelperSession(transport: CloudKitZone(containerIdentifier: container, zoneName: zone, subscriptionID: subscription))
    #else
    fail("CloudKit is unavailable on this platform")
    #endif
default:
    fail("unknown --backend \(backend)")
}

while let line = readLine(strippingNewline: true) {
    if line.trimmingCharacters(in: .whitespaces).isEmpty { continue }
    let (response, stop) = await session.handle(line: line)
    print(response)
    fflush(stdout)
    if stop { break }
}

#endif
