import Foundation
import XCTest
@testable import RightKitAppleCredentials

final class SyncedEnvelopeStoreTests: XCTestCase {
    private let subject = SyncedEnvelopeStore.Subject(account: "api.anthropic.v1", provider: "anthropic")

    private func defaultsSuite() -> UserDefaults { UserDefaults(suiteName: "rk.env.\(UUID().uuidString)")! }

    /// A device over `store`, with its own defaults (applied revisions are device-local).
    private func device(_ store: AppleCredentialStore, enabled: Bool = true) -> SyncedEnvelopeStore {
        let defaults = defaultsSuite()
        let envelopes = SyncedEnvelopeStore(store: store, defaults: defaults, appID: "coderight")
        if enabled { defaults.set(true, forKey: envelopes.enabledDefaultsKey) }
        return envelopes
    }

    private func makeStore() -> AppleCredentialStore {
        AppleCredentialStore(service: "test.\(UUID().uuidString)", backend: InMemoryAppleCredentialBackend())
    }

    private func raw(_ store: AppleCredentialStore) throws -> SyncedCredentialEnvelope {
        let text = try XCTUnwrap(try store.read(subject.account, scope: .synced).get())
        return try JSONDecoder().decode(SyncedCredentialEnvelope.self, from: Data(text.utf8))
    }

    func testDefaultKeysMatchCodeRightsLocalNames() {
        let e = device(makeStore())
        XCTAssertEqual(e.enabledDefaultsKey, "coderight.sync.apple.enabled")
        XCTAssertEqual(e.appliedRevisionKeyPrefix, "coderight.sync.appliedRevision.")
    }

    func testSyncOffTouchesNothing() throws {
        let store = makeStore()
        let e = device(store, enabled: false)
        XCTAssertEqual(try e.publish("k", subject: subject), .syncOff)
        XCTAssertFalse(e.hasRemoteItem(subject))
        XCTAssertEqual(e.pull(subject, local: nil), .nothing)
        XCTAssertNil(try store.read(subject.account, scope: .synced).get())
    }

    func testPublishWritesRevisionedEnvelopeAndTracksAppliedRevision() throws {
        let store = makeStore()
        let e = device(store)
        XCTAssertEqual(try e.publish("  sk-one \n", subject: subject), .written(revision: 1))
        let first = try raw(store)
        XCTAssertEqual(first.secret, "sk-one")
        XCTAssertEqual(first.state, .active)
        XCTAssertEqual(first.app, "coderight")
        XCTAssertEqual(e.appliedRevision(subject), 1)
        XCTAssertEqual(try e.publish("sk-one", subject: subject, onlyIfChanged: true), .unchanged)
        XCTAssertEqual(try e.publish("sk-two", subject: subject, onlyIfChanged: true), .written(revision: 2))
        XCTAssertEqual(try raw(store).credentialId, first.credentialId)
        XCTAssertThrowsError(try e.publish("   ", subject: subject)) {
            XCTAssertEqual($0 as? SyncedEnvelopeError, .emptySecret)
        }
    }

    func testSecondDeviceAdoptsThenSeesNothingNew() throws {
        let store = makeStore()
        try device(store).publish("sk-remote", subject: subject)
        let b = device(store)
        let pull = b.pull(subject, local: nil)
        XCTAssertEqual(pull, .adopt(SyncedSecret(value: "sk-remote"), revision: 1))
        XCTAssertEqual(SyncedEnvelopeStore.outcome(for: pull), .adoptedRemote)
        b.markApplied(subject, revision: 1)
        XCTAssertEqual(b.pull(subject, local: "sk-remote"), .nothing)
    }

    func testIdenticalLocalMarksAppliedAndDifferingLocalIsAConflict() throws {
        let store = makeStore()
        try device(store).publish("sk-remote", subject: subject)
        let b = device(store)
        XCTAssertEqual(b.pull(subject, local: "sk-other"), .conflict)
        XCTAssertEqual(b.appliedRevision(subject), 0)
        XCTAssertEqual(b.pull(subject, local: " sk-remote "), .nothing)
        XCTAssertEqual(b.appliedRevision(subject), 1)
    }

    func testTombstoneErasesOnOtherDevicesAndCannotResurrect() throws {
        let store = makeStore()
        let a = device(store)
        try a.publish("sk-remote", subject: subject)
        XCTAssertTrue(try a.publishTombstone(subject))
        let tomb = try raw(store)
        XCTAssertEqual(tomb.state, .deleted)
        XCTAssertNil(tomb.secret)
        XCTAssertEqual(tomb.revision, 2)
        // Explicit JSON null, not an omitted key.
        XCTAssertTrue(try XCTUnwrap(try store.read(subject.account, scope: .synced).get()).contains("\"secret\":null"))
        XCTAssertFalse(try a.publishTombstone(subject))

        let b = device(store)
        let pull = b.pull(subject, local: "sk-remote")
        XCTAssertEqual(pull, .erase(revision: 2))
        XCTAssertEqual(SyncedEnvelopeStore.outcome(for: pull), .erased)
        b.markApplied(subject, revision: 2)
        XCTAssertEqual(b.pull(subject, local: nil), .nothing)

        // Publishing after a tombstone starts a new credential above the tombstone revision.
        XCTAssertEqual(try a.publish("sk-new", subject: subject), .written(revision: 3))
        XCTAssertNotEqual(try raw(store).credentialId, tomb.credentialId)
    }

    func testTombstoneNeverCreatesCloudDataAndAppliedRevisionOnlyMovesForward() throws {
        let store = makeStore()
        let e = device(store)
        XCTAssertFalse(try e.publishTombstone(subject))
        XCTAssertNil(try store.read(subject.account, scope: .synced).get())
        e.markApplied(subject, revision: 5)
        e.markApplied(subject, revision: 3)
        XCTAssertEqual(e.appliedRevision(subject), 5)
    }

    func testUnreadableRemoteIsReportedAndNeverOverwritten() throws {
        let store = makeStore()
        let e = device(store)
        let id = UUID().uuidString
        let bad = [
            "not json",
            #"{"v":9}"#,
            #"{"v":1,"app":"other","provider":"anthropic","credentialId":"\#(id)","revision":1,"updatedAt":"x","state":"active","secret":"s"}"#,
            #"{"v":1,"app":"coderight","provider":"openai","credentialId":"\#(id)","revision":1,"updatedAt":"x","state":"active","secret":"s"}"#,
            #"{"v":1,"app":"coderight","provider":"anthropic","credentialId":"\#(id)","revision":1,"updatedAt":"x","state":"deleted","secret":"leak"}"#,
        ]
        for text in bad {
            try store.write(text, account: subject.account, scope: .synced).get()
            XCTAssertEqual(e.pull(subject, local: "k"), .unreadable, text)
            XCTAssertThrowsError(try e.publish("new", subject: subject)) {
                XCTAssertEqual($0 as? SyncedEnvelopeError, .unreadableRemote)
            }
            XCTAssertThrowsError(try e.publishTombstone(subject))
            XCTAssertEqual(try store.read(subject.account, scope: .synced).get(), text)
        }
        XCTAssertEqual(SyncedEnvelopeStore.outcome(for: .unreadable), .unreadable)
    }

    func testSecretNeverAppearsInDescriptions() {
        XCTAssertFalse("\(SyncedSecret(value: "sk-live"))".contains("sk-live"))
        XCTAssertFalse(String(reflecting: SyncedSecret(value: "sk-live")).contains("sk-live"))
        let env = SyncedCredentialEnvelope(
            app: "a", provider: "p", credentialId: UUID().uuidString, revision: 1, updatedAt: "t", state: .active, secret: "sk-live")
        XCTAssertFalse(env.description.contains("sk-live"))
    }
}
