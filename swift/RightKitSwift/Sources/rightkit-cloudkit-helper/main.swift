#if os(macOS)
// rightkit-cloudkit-helper — long-lived stdin/stdout helper that gives rightkit-sync (Rust) access
// to the user's private CloudKit database. Protocol: see Session.swift and the rightkit-sync API.md.
//
//   rightkit-cloudkit-helper --container iCloud.com.example.app --zone Items --subscription items-sub
//   rightkit-cloudkit-helper --backend memory      # in-process zone, no iCloud (protocol E2E tests)
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
case "memory":
    // Deterministic in-process zone. Account identity and page size can be pinned for tests.
    let zone = MemoryZone(account: argument("--account") ?? "in-memory-account",
                          pageSize: Int(argument("--page-size") ?? "") ?? 100)
    session = HelperSession(transport: zone, debugWriter: zone)
case "cloudkit":
    #if canImport(CloudKit)
    guard let container = argument("--container"), let zone = argument("--zone"),
          let subscription = argument("--subscription")
    else { fail("--container, --zone and --subscription are required") }
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
