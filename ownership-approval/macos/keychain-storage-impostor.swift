// Negative ACL probe. Reads only caller-supplied UUID sentinel coordinates;
// never creates, signs, or authenticates an ownership key.
import Foundation
import Security

func silently<T>(_ operation: () throws -> T) throws -> T {
  var previous = DarwinBoolean(false)
  let status = SecKeychainGetUserInteractionAllowed(&previous)
  guard status == errSecSuccess else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
  _ = SecKeychainSetUserInteractionAllowed(false)
  var current = DarwinBoolean(true)
  let verifyStatus = SecKeychainGetUserInteractionAllowed(&current)
  guard verifyStatus == errSecSuccess, !current.boolValue else {
    _ = SecKeychainSetUserInteractionAllowed(previous.boolValue)
    var restored = DarwinBoolean(!previous.boolValue)
    let restoreStatus = SecKeychainGetUserInteractionAllowed(&restored)
    guard restoreStatus == errSecSuccess, restored.boolValue == previous.boolValue else {
      throw NSError(domain: NSOSStatusErrorDomain, code: Int(restoreStatus))
    }
    throw NSError(domain: NSOSStatusErrorDomain, code: Int(verifyStatus))
  }
  let outcome = Result { try operation() }
  _ = SecKeychainSetUserInteractionAllowed(previous.boolValue)
  var restored = DarwinBoolean(!previous.boolValue)
  let restoreStatus = SecKeychainGetUserInteractionAllowed(&restored)
  guard restoreStatus == errSecSuccess, restored.boolValue == previous.boolValue else {
    throw NSError(domain: NSOSStatusErrorDomain, code: Int(restoreStatus))
  }
  return try outcome.get()
}

guard CommandLine.arguments.count == 4 else { exit(2) }
let path = CommandLine.arguments[1]
let service = CommandLine.arguments[2]
let account = CommandLine.arguments[3]
var keychain: SecKeychain?
let opened = SecKeychainOpen(path, &keychain)
guard opened == errSecSuccess, let keychain else {
  fputs("ERROR:KEYCHAIN_OPEN:\(opened)\n", stderr)
  exit(1)
}
var keychainStatus = SecKeychainStatus()
let statusCode = SecKeychainGetStatus(keychain, &keychainStatus)
guard statusCode == errSecSuccess else {
  fputs("ERROR:KEYCHAIN_STATUS:\(statusCode)\n", stderr)
  exit(1)
}
guard keychainStatus & UInt32(kSecUnlockStateStatus) != 0 else {
  fputs("ERROR:KEYCHAIN_LOCKED\n", stderr)
  exit(1)
}
let query: [String: Any] = [
  kSecClass as String: kSecClassGenericPassword,
  kSecAttrService as String: service,
  kSecAttrAccount as String: account,
  kSecMatchLimit as String: kSecMatchLimitOne,
  kSecReturnData as String: true,
  kSecMatchSearchList as String: [keychain]
]
var result: CFTypeRef?
let status: OSStatus
do { status = try silently { SecItemCopyMatching(query as CFDictionary, &result) } }
catch { fputs("ERROR:INTERACTION_STATE\n", stderr); exit(1) }
if status == errSecSuccess {
  if let data = result as? Data { FileHandle.standardOutput.write(data) }
  fputs("UNEXPECTED_READ\n", stderr)
  exit(0)
}
fputs("KEYCHAIN_UNLOCKED\nDENIED:\(status)\n", stderr)
exit(1)
