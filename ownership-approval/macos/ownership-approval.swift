// Native RightKit ownership signer. This executable is a review UI, not an
// unattended signing command: only its visible buttons begin enrollment/signing.
import AppKit
import CryptoKit
import CoreFoundation
import Foundation
import LocalAuthentication
import Security
import UniformTypeIdentifiers

private let keychainService = "com.orthiclabs.rightkit.ownership"
private let keychainAccount = "approval-key"
private let payloadKeys = [
  "repositoryIdentity", "catalogDigest", "fingerprint", "paths",
  "occurrenceHashes", "reason", "owner", "expires", "requestId", "trackingRef"
]

private enum ApprovalError: LocalizedError {
  case usage, unsupported, malformed(String), alreadyEnrolled, notEnrolled
  case declined, keychain(OSStatus)

  var errorDescription: String? {
    switch self {
    case .usage: return "Usage: ownership-approval --enroll | --review <unsigned-record.json>"
    case .unsupported: return "This Mac does not provide Secure Enclave signing."
    case .malformed(let detail): return "The draft is invalid: \(detail)"
    case .alreadyEnrolled: return "An approval key is already enrolled on this Mac."
    case .notEnrolled: return "No approval key is enrolled on this Mac."
    case .declined: return "The operation was cancelled."
    case .keychain(let status): return "Keychain operation failed (OSStatus \(status))."
    }
  }
}

// This deliberately supports only the v1 verifier's value types. Its output
// matches JavaScript JSON.stringify for these values, including UTF-8 text,
// JS UTF-16 key ordering, compact separators, and control-character escapes.
private func jsQuote(_ string: String) throws -> String {
  var result = "\""
  for scalar in string.unicodeScalars {
    switch scalar.value {
    case 0x22: result += "\\\""
    case 0x5C: result += "\\\\"
    case 0x08: result += "\\b"
    case 0x09: result += "\\t"
    case 0x0A: result += "\\n"
    case 0x0C: result += "\\f"
    case 0x0D: result += "\\r"
    case 0x00...0x1F: result += String(format: "\\u%04x", scalar.value)
    default: result.unicodeScalars.append(scalar)
    }
  }
  result += "\""
  return result
}

private func jsKeyLess(_ left: String, _ right: String) -> Bool {
  left.utf16.lexicographicallyPrecedes(right.utf16)
}

private func canonicalPayload(_ value: Any) throws -> String {
  if let string = value as? String { return try jsQuote(string) }
  if let number = value as? NSNumber {
    if CFGetTypeID(number) == CFBooleanGetTypeID() { return number.boolValue ? "true" : "false" }
    throw ApprovalError.malformed("numbers are not permitted in approval payload v1")
  }
  if value is NSNull { return "null" }
  if let values = value as? [Any] {
    return "[" + (try values.map(canonicalPayload)).joined(separator: ",") + "]"
  }
  if let object = value as? [String: Any] {
    let keys = object.keys.sorted(by: jsKeyLess)
    let entries = try keys.map { key in
      "\(try jsQuote(key)):\(try canonicalPayload(object[key]!))"
    }
    return "{" + entries.joined(separator: ",") + "}"
  }
  throw ApprovalError.malformed("unsupported JSON value")
}

private func signingInput(_ payload: Any) throws -> (canonical: String, bytes: Data, digest: SHA256.Digest) {
  let canonical = try canonicalPayload(payload)
  let bytes = Data(canonical.utf8)
  return (canonical, bytes, SHA256.hash(data: bytes))
}

private func cryptoSelfTest() throws {
  // This mode is deliberately isolated from Secure Enclave and Keychain APIs.
  // Its output is test material only and is never an ownership record.
  let controls = "tab\tline\nreturn\rbackspace" + String(UnicodeScalar(8)!) + "formfeed" + String(UnicodeScalar(12)!)
  let sample: [String: Any] = [
    "unicode": "café 雪 🧪",
    "quote": "say \"yes\"",
    "slashes": "https://example.invalid/a/b\\c",
    "controls": controls
  ]
  let input = try signingInput(sample)
  let key = P256.Signing.PrivateKey()
  let signature = try key.signature(for: input.digest)
  let output: [String: Any] = [
    "canonicalUtf8Base64": input.bytes.base64EncodedString(),
    "publicKeySpkiBase64": key.publicKey.derRepresentation.base64EncodedString(),
    "signatureDerBase64": signature.derRepresentation.base64EncodedString()
  ]
  let data = try JSONSerialization.data(withJSONObject: output, options: [.sortedKeys, .withoutEscapingSlashes])
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data([10]))
}

private func requireString(_ object: [String: Any], _ key: String, max: Int = 4096) throws -> String {
  guard let value = object[key] as? String, !value.isEmpty, value.utf8.count <= max else {
    throw ApprovalError.malformed("invalid \(key)")
  }
  return value
}

private func validateDraft(_ data: Data) throws -> ([String: Any], [String: Any]) {
  guard let outer = try JSONSerialization.jsonObject(with: data) as? [String: Any],
        Set(outer.keys) == Set(["schema", "payload", "signature"]),
        let schema = outer["schema"] as? NSNumber, schema.intValue == 1,
        CFGetTypeID(schema) != CFBooleanGetTypeID(), outer["signature"] is NSNull,
        let payload = outer["payload"] as? [String: Any], Set(payload.keys) == Set(payloadKeys)
  else { throw ApprovalError.malformed("expected an unsigned schema-1 record with the complete v1 payload") }

  _ = try requireString(payload, "repositoryIdentity", max: 512)
  for key in ["catalogDigest", "fingerprint"] {
    let value = try requireString(payload, key)
    guard value.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil else {
      throw ApprovalError.malformed("invalid \(key)")
    }
  }
  for key in ["reason", "owner", "expires", "requestId"] { _ = try requireString(payload, key) }
  let expiry = try requireString(payload, "expires")
  guard expiry.range(of: "^\\d{4}-\\d{2}-\\d{2}$", options: .regularExpression) != nil,
        let date = ISO8601DateFormatter().date(from: expiry + "T00:00:00Z"),
        ISO8601DateFormatter().string(from: date).hasPrefix(expiry) else {
    throw ApprovalError.malformed("invalid expires date")
  }
  let request = try requireString(payload, "requestId")
  guard request.range(of: "^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$", options: .regularExpression) != nil else {
    throw ApprovalError.malformed("invalid requestId")
  }
  guard let paths = payload["paths"] as? [String], !paths.isEmpty else { throw ApprovalError.malformed("paths must be a nonempty string array") }
  for path in paths {
    let parts = path.split(separator: "/", omittingEmptySubsequences: false)
    guard !path.isEmpty, !path.hasPrefix("/"), !path.contains("\\"),
          !path.contains(where: { "*?[]{}".contains($0) }),
          parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
      throw ApprovalError.malformed("paths must be exact repository-relative paths")
    }
  }
  guard Set(paths).count == paths.count else { throw ApprovalError.malformed("paths contain duplicates") }
  guard let hashes = payload["occurrenceHashes"] as? [String], !hashes.isEmpty,
        hashes.allSatisfy({ $0.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil }),
        Set(hashes).count == hashes.count else { throw ApprovalError.malformed("invalid occurrenceHashes") }
  guard let tracking = payload["trackingRef"] as? [String: Any],
        Set(tracking.keys) == Set(["url", "id", "verified"]),
        let url = tracking["url"] as? String, url.range(of: "^https://.+", options: .regularExpression) != nil,
        let identifier = tracking["id"] as? String, !identifier.isEmpty,
        let verified = tracking["verified"] as? NSNumber, CFGetTypeID(verified) == CFBooleanGetTypeID(), verified.boolValue else {
    throw ApprovalError.malformed("trackingRef must be verified and https-backed")
  }
  return (outer, payload)
}

private func accessControl(privateKey: Bool) throws -> SecAccessControl {
  var error: Unmanaged<CFError>?
  var flags: SecAccessControlCreateFlags = [.userPresence]
  if privateKey { flags.insert(.privateKeyUsage) }
  guard let control = SecAccessControlCreateWithFlags(
    kCFAllocatorDefault, kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
    flags, &error
  ) else { throw error!.takeRetainedValue() as Error }
  return control
}

private func authContext(reason: String) -> LAContext {
  let context = LAContext()
  context.localizedReason = reason
  context.localizedFallbackTitle = "Use Mac password"
  return context
}

private func keyIdentifier(_ key: P256.Signing.PublicKey) -> String {
  "mac-" + SHA256.hash(data: key.derRepresentation).map { String(format: "%02x", $0) }.joined()
}

private func publicPEM(_ key: P256.Signing.PublicKey) -> String {
  "-----BEGIN PUBLIC KEY-----\n\(key.derRepresentation.base64EncodedString(options: Data.Base64EncodingOptions.lineLength64Characters))\n-----END PUBLIC KEY-----\n"
}

private func keychainQuery(returnData: Bool = false, context: LAContext? = nil) -> [String: Any] {
  var query: [String: Any] = [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: keychainService,
    kSecAttrAccount as String: keychainAccount,
    kSecMatchLimit as String: kSecMatchLimitOne
  ]
  if returnData { query[kSecReturnData as String] = true }
  if let context {
    query[kSecUseAuthenticationContext as String] = context
  }
  return query
}

@MainActor
private final class ApprovalWindow: NSObject, NSApplicationDelegate {
  private let mode: String
  private let draftURL: URL?
  private var window: NSWindow!
  private var content = NSTextView()
  private var actionButton: NSButton!
  private var status = NSTextField(wrappingLabelWithString: "")
  private var loaded: [String: Any]?

  init(mode: String, draftURL: URL?) { self.mode = mode; self.draftURL = draftURL }

  func applicationDidFinishLaunching(_ notification: Notification) {
    NSApp.setActivationPolicy(.regular)
    makeWindow()
    NSApp.activate(ignoringOtherApps: true)
    if mode == "--review", let draftURL { loadDraft(draftURL) }
  }

  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

  private func makeWindow() {
    window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 780, height: 620),
                      styleMask: [.titled, .closable, .miniaturizable, .resizable],
                      backing: .buffered, defer: false)
    window.title = "RightKit Ownership Approval"
    window.center()
    let root = NSView(frame: window.contentView!.bounds)
    root.autoresizingMask = [.width, .height]
    window.contentView = root

    let heading = NSTextField(labelWithString: mode == "--enroll" ? "Enroll this Mac's approval key" : "Review ownership exception")
    heading.font = .boldSystemFont(ofSize: 18)
    heading.frame = NSRect(x: 24, y: 574, width: 730, height: 26)
    root.addSubview(heading)
    let instruction = NSTextField(wrappingLabelWithString: mode == "--enroll"
      ? "Creates one non-exportable Secure Enclave P-256 key. Touch ID or your Mac password is required. This button is the only action that begins enrollment."
      : "Review the complete record below. Loading a file never authenticates or signs. Signing begins only after you press the button, then confirm at the macOS authentication prompt.")
    instruction.frame = NSRect(x: 24, y: 526, width: 730, height: 42)
    root.addSubview(instruction)

    let scroll = NSScrollView(frame: NSRect(x: 24, y: 94, width: 730, height: 416))
    scroll.hasVerticalScroller = true
    scroll.borderType = .bezelBorder
    content = NSTextView(frame: scroll.bounds)
    content.isEditable = false
    content.isSelectable = true
    content.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
    content.textContainerInset = NSSize(width: 8, height: 8)
    scroll.documentView = content
    root.addSubview(scroll)

    status.frame = NSRect(x: 24, y: 57, width: 730, height: 28)
    status.lineBreakMode = .byTruncatingMiddle
    root.addSubview(status)

    actionButton = NSButton(title: mode == "--enroll" ? "Enroll approval key…" : "Choose draft…",
                            target: self, action: #selector(performPrimaryAction))
    actionButton.bezelStyle = .rounded
    actionButton.keyEquivalent = "\r"
    actionButton.frame = NSRect(x: 24, y: 18, width: 190, height: 32)
    root.addSubview(actionButton)
    if mode == "--review" { actionButton.isEnabled = false }
    window.makeKeyAndOrderFront(nil)
    content.string = mode == "--enroll"
      ? "Enrollment has not started. Press “Enroll approval key…” to create the host key."
      : "No draft loaded. Choose a file to display its full unsigned record."
  }

  @objc private func performPrimaryAction() {
    if mode == "--enroll" { enroll() }
    else if loaded == nil { chooseDraft() }
    else { signLoadedDraft() }
  }

  private func chooseDraft() {
    let panel = NSOpenPanel()
    panel.allowedContentTypes = [.json]
    panel.canChooseDirectories = false
    panel.canChooseFiles = true
    panel.allowsMultipleSelection = false
    panel.message = "Choose an unsigned schema-1 ownership exception draft."
    guard panel.runModal() == .OK, let url = panel.url else { return }
    loadDraft(url)
  }

  private func loadDraft(_ url: URL) {
    do {
      let (_, payload) = try validateDraft(Data(contentsOf: url))
      let input = try signingInput(payload)
      let digest = input.digest.map { String(format: "%02x", $0) }.joined()
      let display = try JSONSerialization.data(withJSONObject: payload, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
      loaded = payload
      content.string = "DRAFT: \(url.path)\n\nPAYLOAD SHOWN TO USER:\n\(String(decoding: display, as: UTF8.self))\n\nCANONICAL JSON UTF-8 SHA-256: \(digest)\n\nThe signature covers only the canonical payload bytes above."
      status.stringValue = "Validated complete schema-1 record. No authentication or signature has occurred."
      actionButton.title = "Sign displayed record…"
      actionButton.isEnabled = true
    } catch { showError(error) }
  }

  private func enroll() {
    do {
      guard SecureEnclave.isAvailable else { throw ApprovalError.unsupported }
      let context = authContext(reason: "Enroll this Mac's RightKit ownership approval key")
      let privateKey = try SecureEnclave.P256.Signing.PrivateKey(
        compactRepresentable: false, accessControl: try accessControl(privateKey: true), authenticationContext: context
      )
      let add: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: keychainService,
        kSecAttrAccount as String: keychainAccount,
        kSecValueData as String: privateKey.dataRepresentation,
        kSecAttrAccessControl as String: try accessControl(privateKey: false)
      ]
      let result = SecItemAdd(add as CFDictionary, nil)
      if result == errSecDuplicateItem { throw ApprovalError.alreadyEnrolled }
      guard result == errSecSuccess else { throw ApprovalError.keychain(result) }
      let keyId = keyIdentifier(privateKey.publicKey)
      let panel = NSSavePanel()
      panel.nameFieldStringValue = "rightkit-ownership-macos-public.json"
      panel.message = "Save this Mac's public approval key for administrator-managed trust enrollment."
      panel.allowedContentTypes = [.json]
      guard panel.runModal() == .OK, let url = panel.url else {
        status.stringValue = "Key enrolled. Public-key export was cancelled; the key remains in this Mac's protected Keychain."
        content.string = "Enrolled key id: \(keyId)\n\nPublic PEM:\n\(publicPEM(privateKey.publicKey))"
        return
      }
      let export: [String: Any] = ["host": "macos", "keyId": keyId, "publicKeyPem": publicPEM(privateKey.publicKey)]
      let bytes = try JSONSerialization.data(withJSONObject: export, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) + Data([10])
      try bytes.write(to: url, options: [.atomic, .withoutOverwriting])
      content.string = String(decoding: bytes, as: UTF8.self)
      status.stringValue = "Key enrolled. Public key exported to \(url.path)"
      actionButton.isEnabled = false
    } catch { showError(error) }
  }

  private func signLoadedDraft() {
    guard let loaded else { return }
    let confirm = NSAlert()
    confirm.alertStyle = .warning
    confirm.messageText = "Sign this exact ownership exception?"
    confirm.informativeText = "The full payload and its SHA-256 digest are visible in the review window. Press Sign only if you approve this repository, paths, baseline, reason, owner, expiry, request id, and tracking reference."
    confirm.addButton(withTitle: "Continue to macOS authentication")
    confirm.addButton(withTitle: "Cancel")
    guard confirm.runModal() == .alertFirstButtonReturn else { return }
    do {
      guard SecureEnclave.isAvailable else { throw ApprovalError.unsupported }
      let context = authContext(reason: "Sign the reviewed RightKit ownership exception")
      var result: CFTypeRef?
      let statusCode = SecItemCopyMatching(keychainQuery(returnData: true, context: context) as CFDictionary, &result)
      guard statusCode == errSecSuccess, let protectedData = result as? Data else {
        if statusCode == errSecItemNotFound { throw ApprovalError.notEnrolled }
        throw ApprovalError.keychain(statusCode)
      }
      let privateKey = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: protectedData, authenticationContext: context)
      let input = try signingInput(loaded)
      let signature = try privateKey.signature(for: input.digest)
      let signed: [String: Any] = [
        "schema": 1,
        "payload": loaded,
        "signature": ["keyId": keyIdentifier(privateKey.publicKey), "host": "macos", "value": signature.derRepresentation.base64EncodedString()]
      ]
      let panel = NSSavePanel()
      panel.nameFieldStringValue = "ownership-exception.signed.json"
      panel.message = "Save the signed exception record. The signature is ECDSA P-256 DER over SHA-256(canonical payload UTF-8)."
      panel.allowedContentTypes = [.json]
      guard panel.runModal() == .OK, let url = panel.url else { status.stringValue = "Signature created, but output was not saved; nothing was exported."; return }
      let bytes = try JSONSerialization.data(withJSONObject: signed, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) + Data([10])
      try bytes.write(to: url, options: [.atomic, .withoutOverwriting])
      content.string += "\n\nSIGNED RECORD SAVED:\n\(url.path)"
      status.stringValue = "Signed record saved. It still requires independent external trust verification."
      actionButton.isEnabled = false
    } catch { showError(error) }
  }

  private func showError(_ error: Error) {
    status.stringValue = error.localizedDescription
    let alert = NSAlert(error: error)
    alert.runModal()
  }
}

@main
private struct OwnershipApproval {
  @MainActor static func main() {
    if CommandLine.arguments.count == 2, CommandLine.arguments[1] == "--crypto-self-test" {
      do { try cryptoSelfTest() } catch {
        fputs("RightKit ownership crypto self-test: \(error.localizedDescription)\n", stderr)
        exit(1)
      }
      return
    }
    guard SecureEnclave.isAvailable else {
      fputs("RightKit ownership approval: Secure Enclave is unavailable on this Mac.\n", stderr)
      exit(1)
    }
    let arguments = CommandLine.arguments
    let mode: String
    let draft: URL?
    if arguments.count == 2, arguments[1] == "--enroll" {
      mode = "--enroll"; draft = nil
    } else if arguments.count == 3, arguments[1] == "--review" {
      mode = "--review"; draft = URL(fileURLWithPath: arguments[2]).standardizedFileURL
    } else {
      fputs("Usage: ownership-approval --enroll | --review <unsigned-record.json> | --crypto-self-test\n", stderr)
      exit(2)
    }
    let app = NSApplication.shared
    let delegate = ApprovalWindow(mode: mode, draftURL: draft)
    app.delegate = delegate
    app.run()
  }
}
