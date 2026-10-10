import Foundation
import XCTest
@testable import RightKitAppleCredentials

#if os(macOS)
import Security

final class ForeignGenericPasswordTests: XCTestCase {
    // MARK: Selection and error mapping (no Keychain)

    private func match(_ account: String, _ age: TimeInterval?, ref: String) -> ForeignGenericPassword.Match {
        .init(service: "svc", account: account, modifiedAt: age.map { Date(timeIntervalSince1970: 1_700_000_000 + $0) }, persistentRef: Data(ref.utf8))
    }

    func testNewestByModificationDateWinsRegardlessOfOrder() {
        let items = [match("old", 0, ref: "r-old"), match("new", 600, ref: "r-new"), match("mid", 300, ref: "r-mid")]
        XCTAssertEqual(ForeignGenericPassword.newestFirst(items).map(\.account), ["new", "mid", "old"])
        XCTAssertEqual(ForeignGenericPassword.newestFirst(items.reversed()).first?.account, "new")
    }

    func testUndatedItemRanksLastButIsKept() {
        let ordered = ForeignGenericPassword.newestFirst([match("none", nil, ref: "a"), match("dated", 1, ref: "b")])
        XCTAssertEqual(ordered.map(\.account), ["dated", "none"])
        XCTAssertEqual(ForeignGenericPassword.newestFirst([match("none", nil, ref: "a")]).count, 1)
    }

    func testAttributeDictionariesWithoutPersistentRefAreDropped() {
        let date = Date(timeIntervalSince1970: 5)
        let found = ForeignGenericPassword.matches(fromAttributes: [
            [kSecAttrService as String: "svc", kSecAttrAccount as String: "a", kSecAttrModificationDate as String: date, kSecValuePersistentRef as String: Data("ref".utf8)],
            [kSecAttrService as String: "svc", kSecAttrAccount as String: "no-ref"],
        ])
        XCTAssertEqual(found, [.init(service: "svc", account: "a", modifiedAt: date, persistentRef: Data("ref".utf8))])
    }

    private struct FakeBackend: ForeignGenericPassword.Backend {
        var items: Result<[ForeignGenericPassword.Match], ForeignGenericPassword.ReadError> = .success([])
        var secrets: [Data: (OSStatus, Data?)] = [:]
        func attributes(service: String, account: String?, policy: ForeignGenericPassword.Policy) -> Result<[ForeignGenericPassword.Match], ForeignGenericPassword.ReadError> { items }
        func data(forPersistentRef ref: Data, policy: ForeignGenericPassword.Policy) -> (status: OSStatus, data: Data?) {
            secrets[ref] ?? (errSecItemNotFound, nil)
        }
    }

    func testReadsTheNewestItemByItsPersistentRef() throws {
        let backend = FakeBackend(
            items: .success([match("old", 0, ref: "r-old"), match("new", 60, ref: "r-new")]),
            secrets: [Data("r-old".utf8): (errSecSuccess, Data("old-secret".utf8)), Data("r-new".utf8): (errSecSuccess, Data("new-secret".utf8))]
        )
        let item = try ForeignGenericPassword.read(service: "svc", account: nil, policy: .noPrompt, backend: backend).get()
        XCTAssertEqual(item.string, "new-secret")
        XCTAssertEqual(item.account, "new")
        XCTAssertEqual(item.persistentRef, Data("r-new".utf8))
    }

    func testNoMatchIsNotFound() {
        XCTAssertEqual(ForeignGenericPassword.read(service: "svc", account: nil, policy: .noPrompt, backend: FakeBackend()), .failure(.notFound))
    }

    func testNoPromptTurnsSkippedOrRefusedDataIntoInteractionRequired() {
        let ref = Data("r".utf8)
        for status in [errSecItemNotFound, errSecInteractionNotAllowed, errSecAuthFailed] {
            let backend = FakeBackend(items: .success([match("a", 1, ref: "r")]), secrets: [ref: (status, nil)])
            XCTAssertEqual(
                ForeignGenericPassword.read(service: "svc", account: nil, policy: .noPrompt, backend: backend),
                .failure(.interactionRequired), "status \(status)"
            )
        }
    }

    func testAllowPromptDistinguishesDenialFromAbsence() {
        let ref = Data("r".utf8)
        func read(_ status: OSStatus) -> Result<ForeignGenericPassword, ForeignGenericPassword.ReadError> {
            ForeignGenericPassword.read(service: "svc", account: nil, policy: .allowPrompt,
                backend: FakeBackend(items: .success([match("a", 1, ref: "r")]), secrets: [ref: (status, nil)]))
        }
        XCTAssertEqual(read(errSecUserCanceled), .failure(.accessDenied))
        XCTAssertEqual(read(errSecAuthFailed), .failure(.accessDenied))
        XCTAssertEqual(read(errSecItemNotFound), .failure(.notFound))
        XCTAssertEqual(read(-12345), .failure(.keychainStatus(-12345)))
    }

    func testEnumerationFailureIsPropagatedAndEmptyArgumentsRejected() {
        XCTAssertEqual(
            ForeignGenericPassword.read(service: "svc", account: nil, policy: .noPrompt, backend: FakeBackend(items: .failure(.keychainStatus(-25308)))),
            .failure(.keychainStatus(-25308))
        )
        XCTAssertEqual(ForeignGenericPassword.read(service: "", account: nil, policy: .noPrompt, backend: FakeBackend()), .failure(.invalidArguments))
        XCTAssertEqual(ForeignGenericPassword.read(service: "svc", account: "", policy: .noPrompt, backend: FakeBackend()), .failure(.invalidArguments))
    }

    // MARK: Real legacy Keychain (opt-in)

    /// Touches the user's login keychain, so it runs only when asked:
    /// `RIGHTKIT_INTERACTIVE_KEYCHAIN_TESTS=1 swift test --filter ForeignGenericPasswordTests`.
    private func requireInteractive() throws {
        try XCTSkipUnless(
            ProcessInfo.processInfo.environment["RIGHTKIT_INTERACTIVE_KEYCHAIN_TESTS"] == "1",
            "Set RIGHTKIT_INTERACTIVE_KEYCHAIN_TESTS=1 to exercise the real legacy Keychain."
        )
    }

    private static func legacyQuery(_ service: String, _ account: String? = nil) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecUseDataProtectionKeychain as String: false,
        ]
        if let account { query[kSecAttrAccount as String] = account }
        return query
    }

    private func addLegacyItem(service: String, account: String, secret: String) throws {
        var item = Self.legacyQuery(service, account)
        item[kSecValueData as String] = Data(secret.utf8)
        XCTAssertEqual(SecItemAdd(item as CFDictionary, nil), errSecSuccess)
    }

    func testReadsNewestOfSeveralLegacyItemsAndStaysReadOnly() throws {
        try requireInteractive()
        let service = "org.rightkit.tests.foreign.\(UUID().uuidString)"
        addTeardownBlock { Self.deleteAll(service) }

        XCTAssertEqual(ForeignGenericPassword.read(service: service), .failure(.notFound))
        try addLegacyItem(service: service, account: "first", secret: "first-secret")
        try await_(1.2)
        try addLegacyItem(service: service, account: "second", secret: "second-secret")

        let newest = try ForeignGenericPassword.read(service: service).get()
        XCTAssertEqual(newest.account, "second")
        XCTAssertEqual(newest.string, "second-secret")
        XCTAssertEqual(try ForeignGenericPassword.read(service: service, account: "first").get().string, "first-secret")
        XCTAssertEqual(try ForeignGenericPassword.matches(service: service).get().map(\.account), ["second", "first"])

        // Touching the older item makes it the newest: selection is by modification date, not order.
        try await_(1.2)
        let update = SecItemUpdate(Self.legacyQuery(service, "first") as CFDictionary, [kSecValueData as String: Data("first-v2".utf8)] as CFDictionary)
        XCTAssertEqual(update, errSecSuccess)
        let after = try ForeignGenericPassword.read(service: service).get()
        XCTAssertEqual(after.account, "first")
        XCTAssertEqual(after.string, "first-v2")

        // The reader never mutated anything: both items are still present.
        XCTAssertEqual(try ForeignGenericPassword.matches(service: service).get().count, 2)
    }

    /// A foreign item whose ACL trusts no application would normally raise the system access dialog.
    /// Under `.noPrompt` the read must come back with `.interactionRequired` and no dialog.
    func testNoPromptReadOfAclProtectedForeignItemReturnsTypedErrorWithoutDialog() throws {
        try requireInteractive()
        let service = "org.rightkit.tests.foreign-acl.\(UUID().uuidString)"
        let account = "acl"
        addTeardownBlock { Self.deleteAll(service) }
        let tool = Process()
        tool.executableURL = URL(fileURLWithPath: "/usr/bin/security")
        tool.arguments = ["add-generic-password", "-s", service, "-a", account, "-w", "acl-secret", "-T", ""]
        tool.standardOutput = FileHandle.nullDevice
        tool.standardError = FileHandle.nullDevice
        try tool.run(); tool.waitUntilExit()
        try XCTSkipUnless(tool.terminationStatus == 0, "could not create the ACL-protected fixture item")

        let started = Date()
        let result = ForeignGenericPassword.read(service: service, account: account, policy: .noPrompt)
        XCTAssertEqual(result, .failure(.interactionRequired))
        XCTAssertLessThan(Date().timeIntervalSince(started), 5, "a prompt would have blocked here")
    }

    /// `SecItemDelete` removes one match per call on the legacy Keychain; loop until none remain.
    private static func deleteAll(_ service: String) {
        while SecItemDelete(legacyQuery(service) as CFDictionary) == errSecSuccess {}
    }

    private func await_(_ seconds: TimeInterval) throws { Thread.sleep(forTimeInterval: seconds) }
}
#endif
