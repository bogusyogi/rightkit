// Native RightKit ownership signer. This executable is a review UI, not an
// unattended signing command: only its visible buttons begin enrollment/signing.
import AppKit
import CryptoKit
import CoreFoundation
import Foundation
import LocalAuthentication
import Security
import UniformTypeIdentifiers
import Darwin

private let keychainService = "com.orthiclabs.rightkit.ownership"
private let keychainAccount = "approval-key"
private let publicKeychainService = "com.orthiclabs.rightkit.ownership.public"
private let publicKeychainAccount = "approval-public-key"
private let ownershipBundleIdentifier = "com.orthiclabs.rightkit.ownership-approval"
private let ownershipTeamIdentifier = "6KLGD3LLKF"
private let payloadKeys = [
  "repositoryIdentity", "fingerprint", "policyDigest", "minimumScannerVersion",
  "minimumCatalogVersion", "occurrences", "reason", "owner", "issued", "expires", "requestId", "trackingRef"
]

private enum ApprovalError: LocalizedError {
  case usage, unsupported, malformed(String), alreadyEnrolled, notEnrolled
  case declined, keychain(OSStatus), keychainPreflight(String)
  case invalidOutput, outputExists, io(String), invalidKeyLog(String)

  var errorDescription: String? {
    switch self {
    case .usage: return "Usage: ownership-approval --enroll | --review <unsigned-record.json>"
    case .unsupported: return "This Mac does not provide Secure Enclave signing."
    case .malformed(let detail): return "The draft is invalid: \(detail)"
    case .alreadyEnrolled: return "An approval key is already enrolled on this Mac."
    case .notEnrolled: return "No approval key is enrolled on this Mac."
    case .declined: return "The operation was cancelled."
    case .keychain(let status): return "Keychain operation failed (OSStatus \(status))."
    case .keychainPreflight(let detail): return "Keychain preflight failed: \(detail)"
    case .invalidOutput: return "Output path must be absolute."
    case .outputExists: return "Output already exists; refusing to replace it."
    case .io(let detail): return "File operation failed: \(detail)"
    case .invalidKeyLog(let detail): return "The key-log draft is invalid: \(detail)"
    }
  }
}

private func hex<S: Sequence>(_ data: S) -> String where S.Element == UInt8 { data.map { String(format: "%02x", $0) }.joined() }

// Same-directory exclusive temp + exclusive rename prevents both partial output
// and replacement of a file another process created concurrently.
private func writeExclusiveAtomic(_ data: Data, to url: URL) throws {
  guard url.isFileURL, url.path.hasPrefix("/") else { throw ApprovalError.invalidOutput }
  let temp = url.deletingLastPathComponent().appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString).tmp")
  let fd = open(temp.path, O_WRONLY | O_CREAT | O_EXCL, S_IRUSR | S_IWUSR)
  guard fd >= 0 else { throw ApprovalError.io(String(cString: strerror(errno))) }
  var ok = true
  data.withUnsafeBytes { raw in
    var offset = 0
    while offset < raw.count {
      let n = Darwin.write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
      if n <= 0 { ok = false; break }
      offset += n
    }
  }
  if ok && fsync(fd) != 0 { ok = false }
  _ = close(fd)
  guard ok else { _ = unlink(temp.path); throw ApprovalError.io(String(cString: strerror(errno))) }
  // renamex_np(RENAME_EXCL) is atomic and fails if destination exists.
  if renamex_np(temp.path, url.path, UInt32(RENAME_EXCL)) != 0 {
    let code = errno; _ = unlink(temp.path)
    if code == EEXIST { throw ApprovalError.outputExists }
    throw ApprovalError.io(String(cString: strerror(code)))
  }
}

private func localKeyRecord(_ key: P256.Signing.PublicKey, deviceName: String) -> [String: Any] {
  ["keyId": hex(SHA256.hash(data: key.derRepresentation)), "publicKeyPem": publicPEM(key), "platform": "macos", "deviceName": deviceName]
}

private func validDeviceName(_ name: String) -> Bool {
  !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && name.utf16.count <= 128
    && !name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
}

private func derTLV(_ bytes: inout [UInt8], tag: UInt8) throws -> [UInt8] {
  guard bytes.count >= 2, bytes.removeFirst() == tag else { throw ApprovalError.invalidKeyLog("invalid RSA SubjectPublicKeyInfo DER") }
  let first = bytes.removeFirst()
  let length: Int
  if first & 0x80 == 0 { length = Int(first) }
  else {
    let width = Int(first & 0x7f)
    guard width > 0, width <= 4, bytes.count >= width, bytes[0] != 0 else { throw ApprovalError.invalidKeyLog("invalid DER length") }
    var value = 0
    for _ in 0..<width { value = (value << 8) | Int(bytes.removeFirst()) }
    guard value >= 128 else { throw ApprovalError.invalidKeyLog("noncanonical DER length") }
    length = value
  }
  guard length <= bytes.count else { throw ApprovalError.invalidKeyLog("truncated DER") }
  let value = Array(bytes.prefix(length)); bytes.removeFirst(length)
  return value
}

private func derLength(_ length: Int) -> [UInt8] {
  if length < 128 { return [UInt8(length)] }
  var bytes: [UInt8] = []; var value = length
  while value > 0 { bytes.insert(UInt8(value & 0xff), at: 0); value >>= 8 }
  return [0x80 | UInt8(bytes.count)] + bytes
}

private func derWrap(_ tag: UInt8, _ bytes: [UInt8]) -> [UInt8] { [tag] + derLength(bytes.count) + bytes }

private func publicDER(_ pem: String) -> Data? {
  let header = "-----BEGIN PUBLIC KEY-----", footer = "-----END PUBLIC KEY-----"
  guard pem.hasPrefix(header), let end = pem.range(of: footer, options: .backwards),
        end.upperBound == pem.endIndex || ["\n", "\r\n"].contains(String(pem[end.upperBound...])),
        end.lowerBound >= pem.index(pem.startIndex, offsetBy: header.count) else { return nil }
  let bodyRange = pem.index(pem.startIndex, offsetBy: header.count)..<end.lowerBound
  var body = String(pem[bodyRange])
  if body.hasPrefix("\r\n") { body.removeFirst(2) } else if body.hasPrefix("\n") { body.removeFirst() } else { return nil }
  body = body.replacingOccurrences(of: "\r\n", with: "").replacingOccurrences(of: "\n", with: "")
  guard !body.isEmpty, body.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 43 || $0 == 47 || $0 == 61 }) else { return nil }
  return Data(base64Encoded: body)
}

private func isRSA2048Exponent65537SPKI(_ der: Data) -> Bool {
  var outerBytes = Array(der)
  guard let outer = try? derTLV(&outerBytes, tag: 0x30), outerBytes.isEmpty else { return false }
  var fields = outer
  let algorithm = try? derTLV(&fields, tag: 0x30)
  let expectedAlgorithm: [UInt8] = [0x30,0x0d,0x06,0x09,0x2a,0x86,0x48,0x86,0xf7,0x0d,0x01,0x01,0x01,0x05,0x00]
  guard algorithm == Array(expectedAlgorithm.dropFirst(2)), let bits = try? derTLV(&fields, tag: 0x03), fields.isEmpty,
        bits.first == 0 else { return false }
  let rsaDER = Array(bits.dropFirst())
  var rsaBytes = rsaDER
  guard let rsa = try? derTLV(&rsaBytes, tag: 0x30), rsaBytes.isEmpty else { return false }
  var ints = rsa
  guard var modulus = try? derTLV(&ints, tag: 0x02), let exponent = try? derTLV(&ints, tag: 0x02), ints.isEmpty,
        !modulus.isEmpty, !exponent.isEmpty else { return false }
  guard modulus[0] == 0, modulus.count > 1, modulus[1] & 0x80 != 0 else { return false }
  modulus.removeFirst()
  guard modulus.count == 256, modulus[0] & 0x80 != 0, exponent == [0x01,0x00,0x01],
        let key = SecKeyCreateWithData(Data(rsaDER) as CFData,
          [kSecAttrKeyType: kSecAttrKeyTypeRSA, kSecAttrKeyClass: kSecAttrKeyClassPublic, kSecAttrKeySizeInBits: 2048] as CFDictionary, nil),
        let attrs = SecKeyCopyAttributes(key) as? [String: Any], (attrs[kSecAttrKeySizeInBits as String] as? NSNumber)?.intValue == 2048 else { return false }
  return true
}

private func validateKeyRecord(_ value: Any) throws -> [String: Any] {
  guard let record = value as? [String: Any], Set(record.keys) == Set(["keyId", "publicKeyPem", "platform", "deviceName"]),
        let keyId = record["keyId"] as? String, keyId.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil,
        let pem = record["publicKeyPem"] as? String, pem.utf8.count <= 4096, pem.hasPrefix("-----BEGIN PUBLIC KEY-----"),
        let platform = record["platform"] as? String, ["macos", "windows"].contains(platform),
        let device = record["deviceName"] as? String, validDeviceName(device),
        let der = publicDER(pem),
        hex(SHA256.hash(data: der)) == keyId else {
    throw ApprovalError.invalidKeyLog("invalid key record or SPKI fingerprint")
  }
  let p256 = (try? P256.Signing.PublicKey(derRepresentation: der)) != nil
  guard p256 || (platform == "windows" && isRSA2048Exponent65537SPKI(der)) else {
    throw ApprovalError.invalidKeyLog("only P-256 or Windows RSA-2048/e=65537 SPKI keys are accepted")
  }
  return record
}

private func keyLogPayload(_ value: Any, expectedSigner: String) throws -> [String: Any] {
  guard let p = value as? [String: Any], Set(p.keys) == Set(["domain", "rootFingerprint", "sequence", "previousHash", "operation", "subject", "signerKeyId"]),
        p["domain"] as? String == "rightkit.ownership-key-log.v1",
        let root = p["rootFingerprint"] as? String, root.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil,
        let seq = p["sequence"] as? NSNumber, CFGetTypeID(seq) != CFBooleanGetTypeID(), seq.doubleValue >= 0, seq.doubleValue <= 9007199254740991, seq.doubleValue.rounded(.towardZero) == seq.doubleValue,
        let op = p["operation"] as? String, ["root", "add", "revoke"].contains(op),
        let signer = p["signerKeyId"] as? String, signer == expectedSigner else { throw ApprovalError.invalidKeyLog("invalid fields, domain, or signerKeyId") }
  if op == "root" {
    guard seq.intValue == 0, p["previousHash"] is NSNull,
          let s = try? validateKeyRecord(p["subject"] as Any),
          root == signer, s["platform"] as? String == "macos", s["keyId"] as? String == signer else { throw ApprovalError.invalidKeyLog("invalid root genesis") }
  } else {
    guard seq.int64Value > 0, let prev = p["previousHash"] as? String, prev.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil,
          let s = p["subject"] as? [String: Any] else { throw ApprovalError.invalidKeyLog("invalid chained event") }
    if op == "add" {
      guard (try? validateKeyRecord(s)) != nil else { throw ApprovalError.invalidKeyLog("invalid added key record") }
    } else if Set(s.keys) != Set(["keyId"]) || (s["keyId"] as? String)?.range(of: "^[a-f0-9]{64}$", options: .regularExpression) == nil { throw ApprovalError.invalidKeyLog("invalid revoked key subject") }
  }
  return p
}

private func signedKeyLog(_ payload: [String: Any], with key: SecureEnclave.P256.Signing.PrivateKey) throws -> [String: Any] {
  let signer = hex(SHA256.hash(data: key.publicKey.derRepresentation))
  guard payload["signerKeyId"] as? String == signer else { throw ApprovalError.invalidKeyLog("signer does not match the Mac key") }
  let signature = try key.signature(for: signingInput(payload, allowSafeIntegers: true).digest)
  return ["schema": 1, "payload": payload,
    "signature": ["keyId": signer, "host": "macos", "value": signature.derRepresentation.base64EncodedString()]]
}

private func requireEnrollmentPresence(_ key: SecureEnclave.P256.Signing.PrivateKey) throws {
  // Signing fixed non-exported proof bytes forces the one native presence check
  // before a newly-created public key is stored/exported.
  let proof = Data("rightkit.ownership-key-enrollment-presence.v1".utf8)
  _ = try key.signature(for: SHA256.hash(data: proof))
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

private func canonicalPayload(_ value: Any, allowSafeIntegers: Bool = false) throws -> String {
  if let string = value as? String { return try jsQuote(string) }
  if let number = value as? NSNumber {
    if CFGetTypeID(number) == CFBooleanGetTypeID() { return number.boolValue ? "true" : "false" }
    if allowSafeIntegers, number.doubleValue.isFinite, number.doubleValue.rounded(.towardZero) == number.doubleValue,
       abs(number.doubleValue) <= 9007199254740991 { return String(number.int64Value) }
    throw ApprovalError.malformed("numbers are not permitted in approval payload v1")
  }
  if value is NSNull { return "null" }
  if let values = value as? [Any] {
    return "[" + (try values.map { try canonicalPayload($0, allowSafeIntegers: allowSafeIntegers) }).joined(separator: ",") + "]"
  }
  if let object = value as? [String: Any] {
    let keys = object.keys.sorted(by: jsKeyLess)
    let entries = try keys.map { key in
      "\(try jsQuote(key)):\(try canonicalPayload(object[key]!, allowSafeIntegers: allowSafeIntegers))"
    }
    return "{" + entries.joined(separator: ",") + "}"
  }
  throw ApprovalError.malformed("unsupported JSON value")
}

private func signingInput(_ payload: Any, allowSafeIntegers: Bool = false) throws -> (canonical: String, bytes: Data, digest: SHA256.Digest) {
  let canonical = try canonicalPayload(payload, allowSafeIntegers: allowSafeIntegers)
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

private func keyLogSelfTest() throws {
  // Test-only software keys. No Secure Enclave, Keychain, LAContext, or trust path.
  let macKey = P256.Signing.PrivateKey()
  let macSigner = hex(SHA256.hash(data: macKey.publicKey.derRepresentation))
  let rootSubject = localKeyRecord(macKey.publicKey, deviceName: "test-mac")
  let root: [String: Any] = ["domain": "rightkit.ownership-key-log.v1", "rootFingerprint": macSigner,
    "sequence": 0, "previousHash": NSNull(), "operation": "root", "subject": rootSubject, "signerKeyId": macSigner]
  _ = try keyLogPayload(root, expectedSigner: macSigner)
  let rootInput = try signingInput(root, allowSafeIntegers: true)
  let rootSignature = try macKey.signature(for: rootInput.digest)
  let rootEvent: [String: Any] = ["schema": 1, "payload": root,
    "signature": ["keyId": macSigner, "host": "macos", "value": rootSignature.derRepresentation.base64EncodedString()]]

  let rsaAttrs: [String: Any] = [kSecAttrKeyType as String: kSecAttrKeyTypeRSA,
    kSecAttrKeySizeInBits as String: 2048, kSecAttrIsPermanent as String: false]
  var rsaError: Unmanaged<CFError>?
  guard let rsaPrivate = SecKeyCreateRandomKey(rsaAttrs as CFDictionary, &rsaError),
        let rsaPublic = SecKeyCopyPublicKey(rsaPrivate),
        let rsaData = SecKeyCopyExternalRepresentation(rsaPublic, &rsaError) as Data? else {
    if let rsaError { throw rsaError.takeRetainedValue() }
    throw ApprovalError.invalidKeyLog("could not make ephemeral RSA test key")
  }
  let algorithm: [UInt8] = [0x30,0x0d,0x06,0x09,0x2a,0x86,0x48,0x86,0xf7,0x0d,0x01,0x01,0x01,0x05,0x00]
  let bitString = derWrap(0x03, [0] + Array(rsaData))
  let spki = Data(derWrap(0x30, algorithm + bitString))
  let pemBody = spki.base64EncodedString(options: [.lineLength64Characters])
  let rsaRecord: [String: Any] = ["keyId": hex(SHA256.hash(data: spki)),
    "publicKeyPem": "-----BEGIN PUBLIC KEY-----\n\(pemBody)\n-----END PUBLIC KEY-----\n",
    "platform": "windows", "deviceName": "test-windows"]
  let previousHash = hex(SHA256.hash(data: rootInput.bytes))
  let add: [String: Any] = ["domain": "rightkit.ownership-key-log.v1", "rootFingerprint": macSigner,
    "sequence": 1, "previousHash": previousHash, "operation": "add", "subject": rsaRecord, "signerKeyId": macSigner]
  let accepted = try keyLogPayload(add, expectedSigner: macSigner)
  let input = try signingInput(accepted, allowSafeIntegers: true)
  let signature = try macKey.signature(for: input.digest)
  let signed: [String: Any] = ["schema": 1, "payload": accepted,
    "signature": ["keyId": macSigner, "host": "macos", "value": signature.derRepresentation.base64EncodedString()]]
  let result: [String: Any] = ["signedEventLog": ["schema": 1, "events": [rootEvent, signed]],
    "signerPublicKeySpkiBase64": macKey.publicKey.derRepresentation.base64EncodedString(),
    "rootNullGenesisAccepted": true, "windowsRsa2048AddAccepted": true,
    "rootFingerprint": macSigner, "rootCanonicalSha256": previousHash,
    "addCanonicalUtf8Base64": input.bytes.base64EncodedString(), "rootCanonicalUtf8Base64": rootInput.bytes.base64EncodedString()]
  FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .withoutEscapingSlashes]))
  FileHandle.standardOutput.write(Data([10]))
}

private func requireString(_ object: [String: Any], _ key: String, max: Int = 4096) throws -> String {
  guard let value = object[key] as? String, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, value.utf16.count <= max else {
    throw ApprovalError.malformed("invalid \(key)")
  }
  return value
}

private func validateDraft(_ data: Data) throws -> ([String: Any], [String: Any]) {
  guard let outer = try JSONSerialization.jsonObject(with: data) as? [String: Any],
        Set(outer.keys) == Set(["schema", "payload", "signature"]),
        let schema = outer["schema"] as? NSNumber, schema.doubleValue == 2,
        CFGetTypeID(schema) != CFBooleanGetTypeID(), outer["signature"] is NSNull,
        let payload = outer["payload"] as? [String: Any], Set(payload.keys) == Set(payloadKeys)
  else { throw ApprovalError.malformed("expected an unsigned schema-2 record with the complete v2 payload") }

  _ = try requireString(payload, "repositoryIdentity", max: 512)
  let fingerprint = try requireString(payload, "fingerprint", max: 128)
  guard fingerprint.range(of: "^[a-z0-9][a-z0-9._-]{1,127}$", options: .regularExpression) == fingerprint.startIndex..<fingerprint.endIndex,
        fingerprint.range(of: "^[a-f0-9]{64}$", options: .regularExpression) == nil else {
    throw ApprovalError.malformed("invalid fingerprint")
  }
  let policyDigest = try requireString(payload, "policyDigest", max: 64)
  guard policyDigest.range(of: "^[a-f0-9]{64}$", options: .regularExpression) == policyDigest.startIndex..<policyDigest.endIndex else {
    throw ApprovalError.malformed("invalid policyDigest")
  }
  for key in ["minimumScannerVersion", "minimumCatalogVersion"] {
    let value = try requireString(payload, key, max: Int.max)
    guard value.range(of: "^[0-9]+(?:\\.[0-9]+)*$", options: .regularExpression) == value.startIndex..<value.endIndex else {
      throw ApprovalError.malformed("invalid \(key)")
    }
  }
  _ = try requireString(payload, "reason", max: 4000)
  _ = try requireString(payload, "owner", max: 256)
  let issued = try requireString(payload, "issued")
  let expiry = try requireString(payload, "expires")
  guard issued.range(of: "^[0-9]{4}-[0-9]{2}-[0-9]{2}$", options: .regularExpression) == issued.startIndex..<issued.endIndex,
        expiry.range(of: "^[0-9]{4}-[0-9]{2}-[0-9]{2}$", options: .regularExpression) == expiry.startIndex..<expiry.endIndex,
        let issuedDate = ISO8601DateFormatter().date(from: issued + "T00:00:00Z"),
        let date = ISO8601DateFormatter().date(from: expiry + "T00:00:00Z"),
        ISO8601DateFormatter().string(from: issuedDate).hasPrefix(issued),
        ISO8601DateFormatter().string(from: date).hasPrefix(expiry) else {
    throw ApprovalError.malformed("invalid issued or expires date")
  }
  guard issuedDate <= Date(), date > issuedDate, date.timeIntervalSince(issuedDate) <= 90 * 86400 else {
    throw ApprovalError.malformed("issued must not be in the future; expiry must follow issued by at most 90 days")
  }
  let request = try requireString(payload, "requestId")
  guard request.range(of: "^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$", options: .regularExpression) == request.startIndex..<request.endIndex else {
    throw ApprovalError.malformed("invalid requestId")
  }
  guard let occurrences = payload["occurrences"] as? [[String: Any]], (1...256).contains(occurrences.count) else {
    throw ApprovalError.malformed("occurrences must contain 1 to 256 objects")
  }
  var pairs = Set<[String]>()
  for occurrence in occurrences {
    guard Set(occurrence.keys) == Set(["path", "kind", "hash"]),
          let path = occurrence["path"] as? String,
          let kind = occurrence["kind"] as? String, ["source", "dependency"].contains(kind),
          let hash = occurrence["hash"] as? String,
          hash.range(of: "^[a-f0-9]{64}$", options: .regularExpression) == hash.startIndex..<hash.endIndex else {
      throw ApprovalError.malformed("each occurrence must contain exactly path, source/dependency kind, and a SHA-256 hash")
    }
    let parts = path.split(separator: "/", omittingEmptySubsequences: false)
    guard !path.isEmpty, !path.hasPrefix("/"), !path.contains("\\"),
          path.range(of: "^[A-Za-z]:", options: .regularExpression) == nil,
          !path.contains(where: { "*?[]{}".contains($0) }),
          parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
      throw ApprovalError.malformed("occurrence paths must be exact repository-relative paths")
    }
    guard pairs.insert([path, hash]).inserted else { throw ApprovalError.malformed("occurrences contain duplicate path/hash pairs") }
  }
  guard let tracking = payload["trackingRef"] as? [String: Any],
        Set(tracking.keys) == Set(["url", "id", "verified"]),
        let url = tracking["url"] as? String, let components = URLComponents(string: url),
        components.scheme == "https", let host = components.host, !host.isEmpty,
        components.user == nil, components.password == nil,
        let identifier = tracking["id"] as? String, !identifier.isEmpty,
        let verified = tracking["verified"] as? NSNumber, CFGetTypeID(verified) == CFBooleanGetTypeID(), verified.boolValue else {
    throw ApprovalError.malformed("trackingRef must be verified and HTTPS-backed without userinfo")
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

// Storage ACL trusts only this currently running, correctly signed helper.
private func signedHelperAccess() throws -> SecAccess {
  var trusted: SecTrustedApplication?
  guard SecTrustedApplicationCreateFromPath(nil, &trusted) == errSecSuccess, let trusted else {
    throw ApprovalError.keychainPreflight("could not identify current signed helper for Keychain ACL")
  }
  var access: SecAccess?
  let status = SecAccessCreate("RightKit ownership approval helper" as CFString, [trusted] as CFArray, &access)
  guard status == errSecSuccess, let access else { throw ApprovalError.keychain(status) }
  return access
}

private func requireSignedHelperIdentity() throws {
  guard Bundle.main.bundleIdentifier == ownershipBundleIdentifier else {
    throw ApprovalError.keychainPreflight("expected app bundle identifier \(ownershipBundleIdentifier)")
  }
  var code: SecStaticCode?
  let path = Bundle.main.bundleURL as CFURL
  guard SecStaticCodeCreateWithPath(path, SecCSFlags(), &code) == errSecSuccess, let code else {
    throw ApprovalError.keychainPreflight("could not inspect signed helper code")
  }
  let requirementText = "anchor apple generic and identifier \"\(ownershipBundleIdentifier)\" and certificate leaf[subject.OU] = \"\(ownershipTeamIdentifier)\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate 1[field.1.2.840.113635.100.6.2.6] exists"
  var requirement: SecRequirement?
  guard SecRequirementCreateWithString(requirementText as CFString, SecCSFlags(), &requirement) == errSecSuccess,
        let requirement,
        SecStaticCodeCheckValidity(code, SecCSFlags(rawValue: kSecCSStrictValidate), requirement) == errSecSuccess else {
    throw ApprovalError.keychainPreflight("helper signature must be valid Developer ID Team ID \(ownershipTeamIdentifier) with the expected designated identifier")
  }
}

private func defaultUserKeychain() throws -> SecKeychain {
  var keychain: SecKeychain?
  let copied = SecKeychainCopyDefault(&keychain)
  guard copied == errSecSuccess, let keychain else {
    throw ApprovalError.keychainPreflight("default user Keychain is unavailable (OSStatus \(copied))")
  }
  var pathLength = UInt32(PATH_MAX)
  var pathBytes = [Int8](repeating: 0, count: Int(pathLength))
  let pathStatus = SecKeychainGetPath(keychain, &pathLength, &pathBytes)
  guard pathStatus == errSecSuccess else { throw ApprovalError.keychainPreflight("cannot inspect default user Keychain (OSStatus \(pathStatus))") }
  let path = String(cString: pathBytes)
  guard path != "/Library/Keychains/System.keychain" else {
    throw ApprovalError.keychainPreflight("default Keychain resolves to system Keychain; a user Keychain is required")
  }
  var state = SecKeychainStatus()
  let status = SecKeychainGetStatus(keychain, &state)
  guard status == errSecSuccess else { throw ApprovalError.keychainPreflight("default user Keychain unavailable (OSStatus \(status))") }
  guard state & UInt32(kSecUnlockStateStatus) != 0 else { throw ApprovalError.keychainPreflight("default user Keychain is locked; unlock it in Keychain Access") }
  return keychain
}

private func withKeychainInteractionDisabled<T>(_ operation: () throws -> T) throws -> T {
  var previous = DarwinBoolean(false)
  let readStatus = SecKeychainGetUserInteractionAllowed(&previous)
  guard readStatus == errSecSuccess else { throw ApprovalError.keychainPreflight("cannot read Keychain interaction state (OSStatus \(readStatus))") }
  _ = SecKeychainSetUserInteractionAllowed(false)
  var current = DarwinBoolean(true)
  let verifyStatus = SecKeychainGetUserInteractionAllowed(&current)
  guard verifyStatus == errSecSuccess, !current.boolValue else {
    _ = SecKeychainSetUserInteractionAllowed(previous.boolValue)
    var restored = DarwinBoolean(!previous.boolValue)
    let restoreStatus = SecKeychainGetUserInteractionAllowed(&restored)
    guard restoreStatus == errSecSuccess, restored.boolValue == previous.boolValue else {
      throw ApprovalError.keychainPreflight("could not restore Keychain interaction after disable failure (OSStatus \(restoreStatus))")
    }
    throw ApprovalError.keychainPreflight("could not disable Keychain interaction (OSStatus \(verifyStatus))")
  }
  let outcome = Result { try operation() }
  _ = SecKeychainSetUserInteractionAllowed(previous.boolValue)
  var restored = DarwinBoolean(!previous.boolValue)
  let restoreStatus = SecKeychainGetUserInteractionAllowed(&restored)
  guard restoreStatus == errSecSuccess, restored.boolValue == previous.boolValue else {
    throw ApprovalError.keychainPreflight("could not restore Keychain interaction state (OSStatus \(restoreStatus))")
  }
  return try outcome.get()
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

private func keychainQuery(returnData: Bool = false) throws -> [String: Any] {
  var query: [String: Any] = [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: keychainService,
    kSecAttrAccount as String: keychainAccount,
    kSecMatchLimit as String: kSecMatchLimitOne,
    kSecMatchSearchList as String: [try defaultUserKeychain()]
  ]
  if returnData { query[kSecReturnData as String] = true }
  return query
}

// This query checks signing access without prompting or performing authentication.
private func keychainPreflight(printPass: Bool = true) throws {
  try requireSignedHelperIdentity()
  let keychain = try defaultUserKeychain()
  let query: [String: Any] = [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: keychainService,
    kSecAttrAccount as String: keychainAccount,
    kSecMatchLimit as String: kSecMatchLimitOne,
    kSecReturnData as String: true,
    kSecMatchSearchList as String: [keychain]
  ]
  var result: CFTypeRef?
  let status = try withKeychainInteractionDisabled { SecItemCopyMatching(query as CFDictionary, &result) }
  switch status {
  case errSecSuccess, errSecItemNotFound:
    if printPass { print("Keychain preflight: PASS (signed helper ACL + unlocked default user Keychain; no authentication performed).") }
  default:
    throw ApprovalError.keychainPreflight("SecItemCopyMatching returned OSStatus \(status)")
  }
}

private func runStorageProbe(_ executable: String, arguments: [String]) throws -> (Int32, String) {
  let process = Process()
  process.executableURL = URL(fileURLWithPath: executable)
  process.arguments = arguments
  let output = Pipe()
  process.standardOutput = output
  process.standardError = output
  let finished = DispatchSemaphore(value: 0)
  process.terminationHandler = { _ in finished.signal() }
  try process.run()
  if finished.wait(timeout: .now() + 10) != .success {
    process.terminate()
    _ = finished.wait(timeout: .now() + 2)
    throw ApprovalError.keychainPreflight("Keychain ACL child probe exceeded 10-second limit")
  }
  return (process.terminationStatus, String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self))
}

private func keychainStorageReadProbe(keychainPath: String, service: String, account: String) throws {
  try requireSignedHelperIdentity()
  guard service.range(of: "^com\\.orthiclabs\\.rightkit\\.ownership\\.storage-probe\\.[0-9A-Fa-f-]{36}$", options: .regularExpression) != nil,
        account.range(of: "^[0-9A-Fa-f-]{36}$", options: .regularExpression) != nil else {
    throw ApprovalError.keychainPreflight("storage read probe accepts only UUID sentinel coordinates")
  }
  var keychain: SecKeychain?
  let opened = SecKeychainOpen(keychainPath, &keychain)
  guard opened == errSecSuccess, let keychain else { throw ApprovalError.keychain(opened) }
  let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: service, kSecAttrAccount as String: account,
    kSecMatchLimit as String: kSecMatchLimitOne, kSecReturnData as String: true,
    kSecMatchSearchList as String: [keychain]]
  var result: CFTypeRef?
  let status = try withKeychainInteractionDisabled { SecItemCopyMatching(query as CFDictionary, &result) }
  guard status == errSecSuccess, result is Data else { throw ApprovalError.keychain(status) }
  print("READ_ALLOWED")
}

private func keychainStorageSelfTest(impostorPath: String, successorPath: String? = nil) throws {
  try keychainPreflight(printPass: false)
  let keychain = try defaultUserKeychain()
  var pathLength = UInt32(PATH_MAX)
  var pathBytes = [Int8](repeating: 0, count: Int(pathLength))
  let pathStatus = SecKeychainGetPath(keychain, &pathLength, &pathBytes)
  guard pathStatus == errSecSuccess else { throw ApprovalError.keychainPreflight("cannot identify default user Keychain for ACL probe") }
  let service = "com.orthiclabs.rightkit.ownership.storage-probe.\(UUID().uuidString)"
  let account = UUID().uuidString
  let sentinel = Data(UUID().uuidString.utf8)
  let base: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: service, kSecAttrAccount as String: account,
    kSecMatchSearchList as String: [keychain]]
  var sentinelAdded = false
  var sentinelRemoved = false
  defer {
    if sentinelAdded && !sentinelRemoved {
      _ = try? withKeychainInteractionDisabled { SecItemDelete(base as CFDictionary) }
    }
  }
  var add = base
  add[kSecUseKeychain as String] = keychain
  add[kSecAttrAccess as String] = try signedHelperAccess()
  add[kSecValueData as String] = sentinel
  let added = try withKeychainInteractionDisabled { SecItemAdd(add as CFDictionary, nil) }
  guard added == errSecSuccess else { throw ApprovalError.keychain(added) }
  sentinelAdded = true

  var read = base
  read[kSecMatchLimit as String] = kSecMatchLimitOne
  read[kSecReturnData as String] = true
  var ownResult: CFTypeRef?
  let ownReadStatus = try withKeychainInteractionDisabled { SecItemCopyMatching(read as CFDictionary, &ownResult) }
  guard ownReadStatus == errSecSuccess, (ownResult as? Data) == sentinel else {
    throw ApprovalError.keychain(ownReadStatus)
  }

  let childArguments = [String(cString: pathBytes), service, account]
  let (impostorStatus, foreignOutput) = try runStorageProbe(impostorPath, arguments: childArguments)
  let sentinelText = String(decoding: sentinel, as: UTF8.self)
  let acceptedDenials = [errSecInteractionNotAllowed, errSecAuthFailed].map { "DENIED:\($0)" }
  let safeForeignOutput = String(foreignOutput.replacingOccurrences(of: sentinelText, with: "[sentinel]").prefix(500))
  guard impostorStatus != 0, !foreignOutput.contains(sentinelText), !foreignOutput.contains("UNEXPECTED_READ") else {
    throw ApprovalError.keychainPreflight("unrelated executable unexpectedly read signed-helper ACL sentinel (exit \(impostorStatus), output: \(safeForeignOutput))")
  }
  let trimmedForeignOutput = foreignOutput.trimmingCharacters(in: .whitespacesAndNewlines)
  guard let recognizedDenial = acceptedDenials.first(where: { trimmedForeignOutput == "KEYCHAIN_UNLOCKED\n\($0)" }) else {
    throw ApprovalError.keychainPreflight("unrelated executable denial needs classification (exit \(impostorStatus), expected \(acceptedDenials.joined(separator: " or ")), output: \(safeForeignOutput))")
  }
  var successorStatus: Int32?
  if let successorPath {
    let (status, output) = try runStorageProbe(successorPath,
      arguments: ["--keychain-storage-read-probe"] + childArguments)
    guard status == 0, output.trimmingCharacters(in: .whitespacesAndNewlines) == "READ_ALLOWED" else {
      throw ApprovalError.keychainPreflight("same-identity updated helper did not read sentinel without interaction")
    }
    successorStatus = status
  }

  let deleted = try withKeychainInteractionDisabled { SecItemDelete(base as CFDictionary) }
  guard deleted == errSecSuccess else { throw ApprovalError.keychain(deleted) }
  sentinelRemoved = true
  var afterDelete: CFTypeRef?
  let verifyDelete = try withKeychainInteractionDisabled { SecItemCopyMatching(read as CFDictionary, &afterDelete) }
  guard verifyDelete == errSecItemNotFound else {
    throw ApprovalError.keychainPreflight("UUID sentinel remained after cleanup (OSStatus \(verifyDelete))")
  }
  if let successorStatus {
    print("Keychain storage self-test: PASS (helper read; unrelated helper exited \(impostorStatus) with \(recognizedDenial); updated same-identity helper exited \(successorStatus) with READ_ALLOWED; UUID sentinel deleted and verified absent).")
  } else {
    print("Keychain storage self-test: PASS (helper read; unrelated helper exited \(impostorStatus) with \(recognizedDenial); UUID sentinel deleted and verified absent).")
  }
}

private func publicKeyExport(_ key: P256.Signing.PublicKey) throws -> Data {
  let export: [String: Any] = ["host": "macos", "keyId": keyIdentifier(key), "publicKeyPem": publicPEM(key)]
  return try JSONSerialization.data(withJSONObject: export, options: [.sortedKeys, .withoutEscapingSlashes])
}

private func persistPublicKey(_ key: P256.Signing.PublicKey) throws {
  var item: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: publicKeychainService, kSecAttrAccount as String: publicKeychainAccount,
    kSecAttrAccess as String: try signedHelperAccess(),
    kSecUseKeychain as String: try defaultUserKeychain(),
    kSecValueData as String: try publicKeyExport(key)]
  let result = try withKeychainInteractionDisabled { SecItemAdd(item as CFDictionary, nil) }
  guard result == errSecSuccess else { throw ApprovalError.keychain(result) }
}

private func loadExistingPublicKey() throws -> (Data, P256.Signing.PublicKey) {
  var query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: publicKeychainService, kSecAttrAccount as String: publicKeychainAccount,
    kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne,
    kSecMatchSearchList as String: [try defaultUserKeychain()]]
  var result: CFTypeRef?
  let status = try withKeychainInteractionDisabled { SecItemCopyMatching(query as CFDictionary, &result) }
  guard status == errSecSuccess, let bytes = result as? Data else {
    if status == errSecItemNotFound { throw ApprovalError.notEnrolled }
    throw ApprovalError.keychain(status)
  }
  guard let json = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
        Set(json.keys) == Set(["host", "keyId", "publicKeyPem"]), json["host"] as? String == "macos",
        let keyId = json["keyId"] as? String, keyId.range(of: "^mac-[a-f0-9]{64}$", options: .regularExpression) != nil,
        let pem = json["publicKeyPem"] as? String,
        let der = publicDER(pem),
        let key = try? P256.Signing.PublicKey(derRepresentation: der), keyIdentifier(key) == keyId else {
    throw ApprovalError.invalidKeyLog("public key metadata is malformed")
  }
  return (bytes, key)
}

private func exportExistingPublicKey(to output: URL) throws {
  let (bytes, _) = try loadExistingPublicKey()
  try writeExclusiveAtomic(bytes, to: output)
}

@MainActor
private final class ApprovalWindow: NSObject, NSApplicationDelegate {
  private let mode: String
  private let draftURL: URL?
  private let outputURL: URL?
  private let deviceName: String?
  private var window: NSWindow!
  private var content = NSTextView()
  private var actionButton: NSButton!
  private var status = NSTextField(wrappingLabelWithString: "")
  private var loaded: [String: Any]?
  private var completed = false

  init(mode: String, draftURL: URL?, outputURL: URL? = nil, deviceName: String? = nil) { self.mode = mode; self.draftURL = draftURL; self.outputURL = outputURL; self.deviceName = deviceName }

  func applicationDidFinishLaunching(_ notification: Notification) {
    NSApp.setActivationPolicy(.regular)
    makeWindow()
    NSApp.activate(ignoringOtherApps: true)
    if (mode == "--review" || mode == "--key-operation"), let draftURL { loadDraft(draftURL) }
  }

  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
  func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
    if !completed { exit(1) }
    return .terminateNow
  }

  private func makeWindow() {
    window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 780, height: 620),
                      styleMask: [.titled, .closable, .miniaturizable, .resizable],
                      backing: .buffered, defer: false)
    window.title = "RightKit Ownership Approval"
    window.center()
    let root = NSView(frame: window.contentView!.bounds)
    root.autoresizingMask = [.width, .height]
    window.contentView = root

    let heading = NSTextField(labelWithString: mode == "--enroll" || mode == "--enroll-root" ? "Enroll this Mac's approval key" : mode == "--key-operation" ? "Review ownership key-log operation" : "Review ownership exception")
    heading.font = .boldSystemFont(ofSize: 18)
    heading.frame = NSRect(x: 24, y: 574, width: 730, height: 26)
    root.addSubview(heading)
    let instruction = NSTextField(wrappingLabelWithString: mode == "--enroll" || mode == "--enroll-root"
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

    actionButton = NSButton(title: mode == "--enroll" || mode == "--enroll-root" ? "Enroll approval key…" : "Choose draft…",
                            target: self, action: #selector(performPrimaryAction))
    actionButton.bezelStyle = .rounded
    actionButton.keyEquivalent = "\r"
    actionButton.frame = NSRect(x: 24, y: 18, width: 190, height: 32)
    root.addSubview(actionButton)
    if mode == "--review" || mode == "--key-operation" { actionButton.isEnabled = false }
    window.makeKeyAndOrderFront(nil)
    content.string = mode == "--enroll" || mode == "--enroll-root"
      ? "Enrollment has not started. Press “Enroll approval key…” to create the host key."
      : "No draft loaded. Choose a file to display its full unsigned record."
  }

  @objc private func performPrimaryAction() {
    if mode == "--enroll" { enroll() }
    else if mode == "--enroll-root" { enrollRoot() }
    else if loaded == nil { chooseDraft() }
    else { signLoadedDraft() }
  }

  private func chooseDraft() {
    let panel = NSOpenPanel()
    panel.allowedContentTypes = [.json]
    panel.canChooseDirectories = false
    panel.canChooseFiles = true
    panel.allowsMultipleSelection = false
    panel.message = "Choose an unsigned schema-2 ownership exception draft."
    guard panel.runModal() == .OK, let url = panel.url else { exit(1) }
    loadDraft(url)
  }

  private func loadDraft(_ url: URL) {
    do {
      if mode == "--key-operation" {
        let p = try loadKeyOperation(url)
        let display = try JSONSerialization.data(withJSONObject: p, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
        let input = try signingInput(p, allowSafeIntegers: true)
        loaded = p
        content.string = "DRAFT: \(url.path)\n\nFULL KEY-LOG OPERATION:\n\(String(decoding: display, as: UTF8.self))\n\nROOT FINGERPRINT: \(p["rootFingerprint"] as? String ?? "")\nCANONICAL PAYLOAD SHA-256: \(hex(Data(input.digest)))\n\nThe signature covers canonical payload UTF-8 bytes."
        status.stringValue = "Validated operation. No authentication or signature has occurred."
        actionButton.title = "Sign displayed operation…"; actionButton.isEnabled = true
        return
      }
      let (_, payload) = try validateDraft(Data(contentsOf: url))
      let input = try signingInput(payload)
      let digest = input.digest.map { String(format: "%02x", $0) }.joined()
      let display = try JSONSerialization.data(withJSONObject: payload, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
      loaded = payload
      content.string = "DRAFT: \(url.path)\n\nPAYLOAD SHOWN TO USER:\n\(String(decoding: display, as: UTF8.self))\n\nCANONICAL JSON UTF-8 SHA-256: \(digest)\n\nThe signature covers only the canonical payload bytes above."
      status.stringValue = "Validated complete schema-2 record. No authentication or signature has occurred."
      actionButton.title = "Sign displayed record…"
      actionButton.isEnabled = true
    } catch { showError(error) }
  }

  private func loadKeyOperation(_ url: URL) throws -> [String: Any] {
    let bytes = try Data(contentsOf: url)
    guard let outer = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
          Set(outer.keys) == Set(["schema", "payload", "signature"]),
          let schema = outer["schema"] as? NSNumber, schema.intValue == 1, CFGetTypeID(schema) != CFBooleanGetTypeID(),
          outer["signature"] is NSNull else { throw ApprovalError.invalidKeyLog("expected unsigned schema-1 envelope") }
    guard let p = outer["payload"] as? [String: Any], let signer = p["signerKeyId"] as? String,
          signer.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil else { throw ApprovalError.invalidKeyLog("invalid signerKeyId") }
    return try keyLogPayload(p, expectedSigner: signer)
  }

  private func signKeyLog(_ payload: [String: Any], context: LAContext) throws -> [String: Any] {
    let (_, publicKey) = try loadExistingPublicKey()
    let expectedSigner = hex(SHA256.hash(data: publicKey.derRepresentation))
    guard payload["signerKeyId"] as? String == expectedSigner else { throw ApprovalError.invalidKeyLog("signer does not match the enrolled Mac key") }
    var result: CFTypeRef?
    let code = try withKeychainInteractionDisabled { SecItemCopyMatching(try keychainQuery(returnData: true) as CFDictionary, &result) }
    guard code == errSecSuccess, let data = result as? Data else {
      if code == errSecItemNotFound { throw ApprovalError.notEnrolled }; throw ApprovalError.keychain(code)
    }
    let key = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data, authenticationContext: context)
    let signer = hex(SHA256.hash(data: key.publicKey.derRepresentation))
    guard signer == expectedSigner else { throw ApprovalError.invalidKeyLog("private key does not match the stored public key") }
    return try signedKeyLog(payload, with: key)
  }

  private func exportKey(_ key: SecureEnclave.P256.Signing.PrivateKey, out: URL) throws {
    let bytes = try publicKeyExport(key.publicKey) + Data([10])
    try writeExclusiveAtomic(bytes, to: out)
  }

  private func enrollRoot() {
    do {
      guard SecureEnclave.isAvailable, let out = outputURL, let name = deviceName else { throw ApprovalError.unsupported }
      let context = authContext(reason: "Create and sign the RightKit ownership key-log root")
      let key = try SecureEnclave.P256.Signing.PrivateKey(compactRepresentable: false, accessControl: try accessControl(privateKey: true), authenticationContext: context)
      let record = localKeyRecord(key.publicKey, deviceName: name)
      let signer = record["keyId"] as! String
      let payload: [String: Any] = ["domain": "rightkit.ownership-key-log.v1", "rootFingerprint": signer, "sequence": 0, "previousHash": NSNull(), "operation": "root", "subject": record, "signerKeyId": signer]
      // The genesis signature is the sole authenticated operation for this new key.
      let signed = try signedKeyLog(payload, with: key)
      var add: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: keychainService, kSecAttrAccount as String: keychainAccount, kSecValueData as String: key.dataRepresentation, kSecAttrAccess as String: try signedHelperAccess(), kSecUseKeychain as String: try defaultUserKeychain()]
      let added = try withKeychainInteractionDisabled { SecItemAdd(add as CFDictionary, nil) }
      if added == errSecDuplicateItem { throw ApprovalError.alreadyEnrolled }
      guard added == errSecSuccess else { throw ApprovalError.keychain(added) }
      try persistPublicKey(key.publicKey)
      let bytes = try JSONSerialization.data(withJSONObject: signed, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) + Data([10])
      try writeExclusiveAtomic(bytes, to: out)
      completed = true; exit(0)
    } catch { showError(error) }
  }

  private func enroll() {
    do {
      guard SecureEnclave.isAvailable else { throw ApprovalError.unsupported }
      let context = authContext(reason: "Enroll this Mac's RightKit ownership approval key")
      let privateKey = try SecureEnclave.P256.Signing.PrivateKey(
        compactRepresentable: false, accessControl: try accessControl(privateKey: true), authenticationContext: context
      )
      try requireEnrollmentPresence(privateKey)
      var add: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: keychainService,
        kSecAttrAccount as String: keychainAccount,
        kSecValueData as String: privateKey.dataRepresentation,
        kSecAttrAccess as String: try signedHelperAccess(),
        kSecUseKeychain as String: try defaultUserKeychain()
      ]
      let result = try withKeychainInteractionDisabled { SecItemAdd(add as CFDictionary, nil) }
      if result == errSecDuplicateItem { throw ApprovalError.alreadyEnrolled }
      guard result == errSecSuccess else { throw ApprovalError.keychain(result) }
      try persistPublicKey(privateKey.publicKey)
      if let out = outputURL {
        try exportKey(privateKey, out: out)
        completed = true; exit(0)
      }
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
    confirm.informativeText = "The full payload and its SHA-256 digest are visible in the review window. Press Sign only if you approve this repository, occurrences, policy digest, version floors, reason, owner, issued/expiry dates, request id, and tracking reference."
    confirm.addButton(withTitle: "Continue to macOS authentication")
    confirm.addButton(withTitle: "Cancel")
      guard confirm.runModal() == .alertFirstButtonReturn else { exit(1) }
      if mode == "--key-operation" {
        do {
          guard let out = outputURL else { throw ApprovalError.invalidOutput }
          let signed = try signKeyLog(loaded, context: authContext(reason: "Sign the reviewed RightKit ownership key-log operation"))
          let bytes = try JSONSerialization.data(withJSONObject: signed, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) + Data([10])
          try writeExclusiveAtomic(bytes, to: out)
          completed = true; exit(0)
        } catch { showError(error) }
        return
      }
    do {
      guard SecureEnclave.isAvailable else { throw ApprovalError.unsupported }
      let context = authContext(reason: "Sign the reviewed RightKit ownership exception")
      var result: CFTypeRef?
      let statusCode = try withKeychainInteractionDisabled { SecItemCopyMatching(try keychainQuery(returnData: true) as CFDictionary, &result) }
      guard statusCode == errSecSuccess, let protectedData = result as? Data else {
        if statusCode == errSecItemNotFound { throw ApprovalError.notEnrolled }
        throw ApprovalError.keychain(statusCode)
      }
      let privateKey = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: protectedData, authenticationContext: context)
      let input = try signingInput(loaded)
      let signature = try privateKey.signature(for: input.digest)
      let signed: [String: Any] = [
        "schema": 2,
        "payload": loaded,
        "signature": ["keyId": keyIdentifier(privateKey.publicKey), "host": "macos", "value": signature.derRepresentation.base64EncodedString()]
      ]
      let panel = NSSavePanel()
      panel.nameFieldStringValue = "ownership-exception.signed.json"
      panel.message = "Save the signed exception record. The signature is ECDSA P-256 DER over SHA-256(canonical payload UTF-8)."
      panel.allowedContentTypes = [.json]
      guard panel.runModal() == .OK, let url = panel.url else { exit(1) }
      let bytes = try JSONSerialization.data(withJSONObject: signed, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) + Data([10])
      try bytes.write(to: url, options: [.atomic, .withoutOverwriting])
      content.string += "\n\nSIGNED RECORD SAVED:\n\(url.path)"
      status.stringValue = "Signed record saved. It still requires independent external trust verification."
      completed = true
      actionButton.isEnabled = false
    } catch { showError(error) }
  }

  private func showError(_ error: Error) {
    status.stringValue = error.localizedDescription
    let alert = NSAlert(error: error)
    alert.runModal()
    exit(1)
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
    if CommandLine.arguments.count == 2, CommandLine.arguments[1] == "--key-log-self-test" {
      do { try keyLogSelfTest() } catch {
        fputs("RightKit ownership key-log self-test: \(error.localizedDescription)\n", stderr)
        exit(1)
      }
      return
    }
    let arguments = CommandLine.arguments
    if arguments.count == 2, arguments[1] == "--keychain-preflight" {
      do { try keychainPreflight() }
      catch {
        fputs("RightKit ownership keychain preflight: \(error.localizedDescription)\n", stderr)
        exit(1)
      }
      return
    }
    if arguments.count == 5, arguments[1] == "--keychain-storage-read-probe" {
      do { try keychainStorageReadProbe(keychainPath: arguments[2], service: arguments[3], account: arguments[4]) }
      catch { fputs("RightKit ownership Keychain read probe: \(error.localizedDescription)\n", stderr); exit(1) }
      return
    }
    if (arguments.count == 3 || arguments.count == 4), arguments[1] == "--keychain-storage-self-test" {
      do { try keychainStorageSelfTest(impostorPath: arguments[2], successorPath: arguments.count == 4 ? arguments[3] : nil) }
      catch { fputs("RightKit ownership Keychain storage self-test: \(error.localizedDescription)\n", stderr); exit(1) }
      return
    }
    let mode: String
    let draft: URL?
    var output: URL?
    var name: String?
    if arguments.count == 4, arguments[1] == "--public-key", arguments[2] == "--out" {
      let output = URL(fileURLWithPath: arguments[3])
      guard output.path.hasPrefix("/"), !FileManager.default.fileExists(atPath: output.path),
            FileManager.default.fileExists(atPath: output.deletingLastPathComponent().path) else {
        fputs("Output must be an absolute path in an existing directory and must not exist.\n", stderr); exit(2)
      }
      do { try keychainPreflight(); try exportExistingPublicKey(to: output); exit(0) }
      catch { fputs("RightKit public key export: \(error.localizedDescription)\n", stderr); exit(1) }
    }
    if arguments.count == 2, arguments[1] == "--enroll" { mode = "--enroll"; draft = nil }
    else if arguments.count == 4, arguments[1] == "--enroll", arguments[2] == "--out" {
      mode = "--enroll"; draft = nil; output = URL(fileURLWithPath: arguments[3])
    } else if arguments.count == 6, arguments[1] == "--enroll-root", arguments[2] == "--name", arguments[4] == "--out" {
      mode = "--enroll-root"; draft = nil; name = arguments[3]; output = URL(fileURLWithPath: arguments[5])
    } else if arguments.count == 5, arguments[1] == "--key-operation", arguments[3] == "--out" {
      mode = "--key-operation"; draft = URL(fileURLWithPath: arguments[2]).standardizedFileURL; output = URL(fileURLWithPath: arguments[4])
    } else if arguments.count == 3, arguments[1] == "--review" { mode = "--review"; draft = URL(fileURLWithPath: arguments[2]).standardizedFileURL }
    else {
      fputs("Usage: ownership-approval --enroll [--out <absolute-public.json>] | --public-key --out <absolute-public.json> | --enroll-root --name <name> --out <absolute-event.json> | --key-operation <draft.json> --out <signed.json> | --review <unsigned-record.json> | --keychain-preflight | --keychain-storage-self-test <impostor-executable> [updated-helper] | --keychain-storage-read-probe <keychain> <service> <account> | --crypto-self-test | --key-log-self-test\n", stderr)
      exit(2)
    }
    if let output, !output.path.hasPrefix("/") { fputs("Output path must be absolute.\n", stderr); exit(2) }
    if let output, FileManager.default.fileExists(atPath: output.path) { fputs("Output already exists; refusing to replace it.\n", stderr); exit(2) }
    if let output, !FileManager.default.fileExists(atPath: output.deletingLastPathComponent().path) { fputs("Output directory does not exist.\n", stderr); exit(2) }
    if mode == "--enroll-root", !(name.map(validDeviceName) ?? false) { fputs("Device name must be nonempty, at most 128 UTF-16 units, and contain no control characters.\n", stderr); exit(2) }
    do { try keychainPreflight() }
    catch {
      fputs("RightKit ownership keychain preflight: \(error.localizedDescription)\n", stderr)
      exit(1)
    }
    guard SecureEnclave.isAvailable else {
      fputs("RightKit ownership approval: Secure Enclave is unavailable on this Mac.\n", stderr)
      exit(1)
    }
    let app = NSApplication.shared
    let delegate = ApprovalWindow(mode: mode, draftURL: draft, outputURL: output, deviceName: name)
    app.delegate = delegate
    app.run()
  }
}
