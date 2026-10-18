import Foundation
import Testing
@testable import RightKitCredentials
#if canImport(CryptoKit)
import CryptoKit
#endif

#if canImport(CryptoKit)
private let magic = Data("PULSECL1".utf8)

@Test func sealedBlobRoundTripsAndBindsKeyAndMagic() throws {
    let key = SymmetricKey(size: .bits256)
    let sealed = try SealedBlob.seal(Data("hello".utf8), key: key, magic: magic)
    #expect(SealedBlob.isSealed(sealed, magic: magic))
    #expect(sealed.count == magic.count + 12 + 5 + 16)
    #expect(try SealedBlob.open(sealed, key: key, magic: magic) == Data("hello".utf8))
    #expect(throws: SealedBlob.Failure.authenticationFailed) {
        _ = try SealedBlob.open(sealed, key: SymmetricKey(size: .bits256), magic: magic)
    }
    #expect(throws: SealedBlob.Failure.notSealed) {
        _ = try SealedBlob.open(Data("plain".utf8), key: key, magic: magic)
    }
    var tampered = sealed
    tampered[tampered.count - 1] ^= 1
    #expect(throws: SealedBlob.Failure.authenticationFailed) {
        _ = try SealedBlob.open(tampered, key: key, magic: magic)
    }
}

@Test func sealedBlobKeyIsMintedOnceAndShared() throws {
    let store = try SecretsStore(namespace: "test.\(UUID().uuidString)", guardEnv: [], guardMode: .memory,
                                 environment: [:], backend: MemorySecretsBackend())
    let first = try SealedBlob.key(in: store, account: "history-key-1")
    let second = try SealedBlob.key(in: store, account: "history-key-1")
    #expect(first.withUnsafeBytes { Data($0) } == second.withUnsafeBytes { Data($0) })
    try store.setString("bad", "not base64!")
    #expect(throws: SealedBlob.Failure.badKey) { _ = try SealedBlob.key(in: store, account: "bad") }
}

@Test func ownerKeyIdIsSixteenBytesOfSha256InHex() {
    #if canImport(LocalAuthentication)
    #expect(OwnerKey.keyId(of: Data("abc".utf8)) == "ba7816bf8f01cfea414140de5dae2223")
    #expect(OwnerKey.hex(Data([0, 255, 16])) == "00ff10")
    #endif
}
#endif
