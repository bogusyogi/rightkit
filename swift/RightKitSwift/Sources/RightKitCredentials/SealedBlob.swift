import Foundation
#if canImport(CryptoKit)
import CryptoKit

/// Encryption at rest for files: `magic | nonce (12) | ciphertext | tag (16)`, AES-256-GCM, with
/// the magic as authenticated data. The layout equals Pulse's `PULSECL1` clipboard files and the
/// Rust `seal_blob` with a store-held key in `rightkit-credentials`, so either side opens what the
/// other wrote when both use the same key.
public enum SealedBlob {
    public enum Failure: Error, Equatable, Sendable {
        /// The data does not start with the magic.
        case notSealed
        /// Wrong key, wrong magic, or the data was changed.
        case authenticationFailed
        /// The stored key text is not 32 bytes of base64.
        case badKey
    }

    public static func isSealed(_ data: Data, magic: Data) -> Bool {
        !magic.isEmpty && data.starts(with: magic)
    }

    public static func seal(_ plain: Data, key: SymmetricKey, magic: Data) throws -> Data {
        guard !magic.isEmpty, key.bitCount == 256 else { throw Failure.badKey }
        let box = try AES.GCM.seal(plain, using: key, authenticating: magic)
        guard let combined = box.combined else { throw Failure.authenticationFailed }
        return magic + combined
    }

    public static func open(_ sealed: Data, key: SymmetricKey, magic: Data) throws -> Data {
        guard isSealed(sealed, magic: magic) else { throw Failure.notSealed }
        guard key.bitCount == 256 else { throw Failure.badKey }
        do {
            let box = try AES.GCM.SealedBox(combined: sealed.dropFirst(magic.count))
            return try AES.GCM.open(box, using: key, authenticating: magic)
        } catch {
            throw Failure.authenticationFailed
        }
    }

    /// The 256-bit key kept in `store` under `account` as standard base64 text (the form the Rust
    /// `Store::get_or_create_key` writes), minted on first use with an add-if-absent so concurrent
    /// callers adopt one winner. A stored value that is not a valid key is `Failure.badKey`; it is
    /// never replaced, because a fresh key could not open what the old one sealed.
    public static func key(in store: SecretsStore, account: String) throws -> SymmetricKey {
        if let text = try store.getString(account), !text.isEmpty {
            return try decodeKey(text)
        }
        let fresh = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
        if try store.setIfAbsent(account, string: fresh.base64EncodedString()) {
            return SymmetricKey(data: fresh)
        }
        guard let winner = try store.getString(account), !winner.isEmpty else { throw Failure.badKey }
        return try decodeKey(winner)
    }

    static func decodeKey(_ text: String) throws -> SymmetricKey {
        guard let data = Data(base64Encoded: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              data.count == 32
        else { throw Failure.badKey }
        return SymmetricKey(data: data)
    }
}
#endif
