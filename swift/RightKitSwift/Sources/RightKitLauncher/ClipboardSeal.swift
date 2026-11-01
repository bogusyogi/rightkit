import CryptoKit
import Foundation
import OSLog
import RightKitAppleCredentials

// Encryption at rest for the clipboard history (docs/clipboard-spec.md section 9; Pulse #152).
//
// Every file the store writes (index, boards, colours, payloads, thumbnails, the version 0
// backup) is sealed with AES-256-GCM under one 256-bit key. The key is kept by RightKit's
// AppleCredentialStore in the Keychain, this device only (scope `device`, never synced).
// Sealed layout: "PULSECL1" (8 bytes, also the authenticated data), then the CryptoKit combined
// box (12-byte nonce, ciphertext, 16-byte tag). A file without the header is a plain file from
// before encryption: it is read as it is, and `prepare` seals it in place (the atomic replace
// removes the plaintext). The switch `clipboardEncryption` only decides how new writes are
// stored; reads take both forms.
//
// Missing or unreadable key with sealed files present: the history is cleared (a fresh start),
// the event is logged, and nothing crashes. Called on the store's background queue.

enum ClipSeal {
    static let magic = Data("PULSECL1".utf8)
    static let service = "dev.orthic.pulse.clipboard"
    static let account = "history-key-1"

    enum Prepared: Equatable {
        /// The history is readable; new writes follow the switch.
        case ready
        /// Sealed files had no key: the history was cleared and a new key was made.
        case fresh
        /// No usable key: nothing is written this session.
        case unavailable
    }

    private static let log = Logger(subsystem: "dev.orthic.pulse", category: "clipboard")
    private static let lock = NSLock()
    private static var key: SymmetricKey?
    private static var switchOn = true

    /// Set from `clipboardEncryption` (default on) whenever the settings change.
    static var enabled: Bool {
        lock.lock(); defer { lock.unlock() }
        return switchOn
    }

    static func setEnabled(_ on: Bool) {
        lock.lock(); defer { lock.unlock() }
        switchOn = on
    }

    private static func currentKey() -> SymmetricKey? {
        lock.lock(); defer { lock.unlock() }
        return key
    }

    private static func setKey(_ value: SymmetricKey) {
        lock.lock(); defer { lock.unlock() }
        key = value
    }

    static func isSealed(_ data: Data) -> Bool {
        data.starts(with: magic)
    }

    private enum KeyLoad {
        case ready(SymmetricKey)
        case missing
        case unreadable
    }

    /// Reads the key from the Keychain. `create` mints one when there is none; without it an
    /// absent key is `missing`.
    private static func loadKey(create: Bool) -> KeyLoad {
        let store = AppleCredentialStore(service: service)
        switch store.read(account, scope: .device) {
        case .success(let text?):
            guard let key = decode(text) else { return .unreadable }
            return .ready(key)
        case .success(nil):
            guard create else { return .missing }
            let fresh = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
            guard case .success = store.write(fresh.base64EncodedString(), account: account, scope: .device) else {
                return .unreadable
            }
            // Read back: the Keychain copy, not this one, is the key from now on.
            guard case .success(let text?) = store.read(account, scope: .device),
                  let key = decode(text) else { return .unreadable }
            return .ready(key)
        case .failure:
            return .unreadable
        }
    }

    private static func decode(_ text: String) -> SymmetricKey? {
        guard let raw = Data(base64Encoded: text), raw.count == 32 else { return nil }
        return SymmetricKey(data: raw)
    }

    /// True when the file starts with the sealed header.
    private static func hasHeader(_ url: URL) -> Bool {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return false }
        defer { try? handle.close() }
        return (try? handle.read(upToCount: magic.count)) == magic
    }

    /// Every file that holds history, plain or sealed.
    private static func historyFiles(_ dir: URL) -> [URL] {
        var urls = [ClipDisk.indexURL(dir), ClipDisk.boardsURL(dir), ClipDisk.coloursURL(dir)]
        for folder in [ClipDisk.itemsDir(dir), ClipDisk.thumbsDir(dir)] {
            let names = (try? FileManager.default.contentsOfDirectory(atPath: folder.path)) ?? []
            urls += names.filter { !$0.hasPrefix(".") }.map { folder.appendingPathComponent($0) }
        }
        return urls
    }

    private static func anySealed(_ dir: URL) -> Bool {
        historyFiles(dir).contains { hasHeader($0) }
    }

    /// Removes the index, boards, payloads and thumbnails: the history that can no longer be read.
    private static func clear(_ dir: URL) {
        let manager = FileManager.default
        try? manager.removeItem(at: ClipDisk.indexURL(dir))
        try? manager.removeItem(at: ClipDisk.boardsURL(dir))
        for folder in [ClipDisk.itemsDir(dir), ClipDisk.thumbsDir(dir)] {
            for name in (try? manager.contentsOfDirectory(atPath: folder.path)) ?? [] {
                try? manager.removeItem(at: folder.appendingPathComponent(name))
            }
        }
    }

    /// Seals every plain history file in place.
    private static func migrate(_ dir: URL) {
        for url in historyFiles(dir) {
            guard let bytes = try? Data(contentsOf: url), !isSealed(bytes), let sealed = seal(bytes) else {
                continue
            }
            try? atomicWrite(sealed, to: url)
        }
    }

    /// Atomic replace (temp file, then rename), then 0600: the write the store has always made.
    static func atomicWrite(_ data: Data, to url: URL) throws {
        try data.write(to: url, options: .atomic)
        chmod(url.path, 0o600)
    }

    /// Runs once per load, before anything is read. `owner` is false when another process holds
    /// the folder: then only the key is read, and nothing is migrated, cleared or written.
    static func prepare(dir: URL, owner: Bool) -> Prepared {
        let sealedPresent = anySealed(dir)
        if !sealedPresent && !enabled { return .ready }
        if !owner {
            if case .ready(let loaded) = loadKey(create: false) {
                setKey(loaded)
                return .ready
            }
            return .unavailable
        }
        switch loadKey(create: !sealedPresent) {
        case .ready(let loaded):
            setKey(loaded)
            if enabled { migrate(dir) }
            return .ready
        case .missing, .unreadable:
            // Sealed files are never deleted here: a locked Keychain, a missing entitlement or a
            // build signed differently can fail to read a key that still opens them. The history
            // is left as it is and not used this session.
            log.error("clipboard history key not readable; history left on disk and not used this session")
            return .unavailable
        }
    }

    /// The sealed form of `plain`, or nil when no key is available.
    static func seal(_ plain: Data) -> Data? {
        guard let key = currentKey(),
              let box = try? AES.GCM.seal(plain, using: key, authenticating: magic),
              let combined = box.combined else { return nil }
        return magic + combined
    }

    /// The plain form of a sealed file, or nil when the key is missing or the tag does not verify.
    static func open(_ sealed: Data) -> Data? {
        guard sealed.starts(with: magic), let key = currentKey() else { return nil }
        guard let box = try? AES.GCM.SealedBox(combined: sealed.dropFirst(magic.count)) else { return nil }
        return try? AES.GCM.open(box, using: key, authenticating: magic)
    }

    /// The bytes of a history file: opened when sealed, as they are when plain. Nil when missing or
    /// unreadable.
    static func read(_ url: URL) -> Data? {
        guard let bytes = try? Data(contentsOf: url) else { return nil }
        return isSealed(bytes) ? open(bytes) : bytes
    }

    /// Writes a history file atomically, sealed when the switch is on. Throws when the file must be
    /// sealed and no key is available.
    static func write(_ plain: Data, to url: URL) throws {
        if enabled {
            guard let sealed = seal(plain) else { throw CocoaError(.fileWriteUnknown) }
            try atomicWrite(sealed, to: url)
        } else {
            try atomicWrite(plain, to: url)
        }
    }
}
