import Foundation
import XCTest
@testable import RightKitAppleCredentials

#if os(macOS)
import Security

/// Reference reads, raw statuses and UI-interaction rules for `ForeignGenericPassword`.
/// Nothing here touches a real Keychain: backends are fakes, and the `SystemBackend` tests inject
/// the `SecItemCopyMatching` call so the queries are inspected, not executed.
final class ForeignGenericPasswordReferenceTests: XCTestCase {
    typealias FGP = ForeignGenericPassword

    private func match(_ account: String, _ age: TimeInterval?, ref: String, service: String = "svc") -> FGP.Match {
        .init(service: service, account: account, modifiedAt: age.map { Date(timeIntervalSince1970: 1_700_000_000 + $0) }, persistentRef: Data(ref.utf8))
    }

    private struct RefBackend: FGP.Backend {
        var items: [FGP.Match] = []
        var secrets: [Data: (OSStatus, Data?)] = [:]
        var refLookup: [Data: Result<FGP.Match?, FGP.ReadFailure>] = [:]
        var enumerationStatus: FGP.ReadFailure?
        func attributes(service: String, account: String?, policy: FGP.Policy) -> Result<[FGP.Match], FGP.ReadError> {
            .success(items)
        }
        func attributesDetailed(service: String, account: String?, policy: FGP.Policy) -> Result<[FGP.Match], FGP.ReadFailure> {
            if let enumerationStatus { return .failure(enumerationStatus) }
            return .success(items)
        }
        func attributes(forPersistentRef ref: Data) -> Result<FGP.Match?, FGP.ReadFailure> {
            refLookup[ref] ?? .failure(.init(error: .notFound, status: errSecItemNotFound))
        }
        func data(forPersistentRef ref: Data, policy: FGP.Policy) -> (status: OSStatus, data: Data?) {
            secrets[ref] ?? (errSecItemNotFound, nil)
        }
    }

    // MARK: 1. Read by persistent reference

    func testReadByReferenceReadsExactlyThatItemNotTheNewest() throws {
        let old = match("old", 0, ref: "r-old"), new = match("new", 600, ref: "r-new")
        let backend = RefBackend(
            items: [old, new],
            secrets: [Data("r-old".utf8): (errSecSuccess, Data("old-secret".utf8)), Data("r-new".utf8): (errSecSuccess, Data("new-secret".utf8))],
            refLookup: [Data("r-old".utf8): .success(old), Data("r-new".utf8): .success(new)]
        )
        // The service-level read picks the newest; the reference read honours the caller's choice.
        XCTAssertEqual(try FGP.read(service: "svc", account: nil, policy: .noPrompt, backend: backend).get().string, "new-secret")
        let chosen = try FGP.read(persistentRef: Data("r-old".utf8), policy: .noPrompt, backend: backend).get()
        XCTAssertEqual(chosen.string, "old-secret")
        XCTAssertEqual(chosen.account, "old")
        XCTAssertEqual(chosen.service, "svc")
        XCTAssertEqual(chosen.modifiedAt, old.modifiedAt)
        XCTAssertEqual(chosen.persistentRef, Data("r-old".utf8))
    }

    func testReadByMatchKeepsTheMatchAttributesAndNeedsNoLookup() throws {
        let only = match("acct", 5, ref: "r1")
        // refLookup is empty: a lookup would fail with notFound, so success proves none happened.
        let backend = RefBackend(secrets: [Data("r1".utf8): (errSecSuccess, Data("s".utf8))])
        let item = try FGP.readDetailed(only, policy: .noPrompt, backend: backend).get()
        XCTAssertEqual(item.account, "acct")
        XCTAssertEqual(item.string, "s")
    }

    func testReferenceReadErrors() {
        let ref = Data("gone".utf8)
        XCTAssertEqual(FGP.read(persistentRef: Data(), policy: .noPrompt, backend: RefBackend()), .failure(.invalidArguments))
        XCTAssertEqual(FGP.read(persistentRef: ref, policy: .noPrompt, backend: RefBackend()), .failure(.notFound), "a stale reference is not found")
        XCTAssertEqual(FGP.readDetailed(.init(service: "s", account: nil, modifiedAt: nil, persistentRef: Data()), policy: .noPrompt, backend: RefBackend()),
                       .failure(.init(error: .invalidArguments, status: nil)))

        // Exists (attributes answered) but the no-prompt data read is skipped: needs approval, not absent.
        let skipped = RefBackend(secrets: [ref: (errSecItemNotFound, nil)], refLookup: [ref: .success(nil)])
        XCTAssertEqual(FGP.read(persistentRef: ref, policy: .noPrompt, backend: skipped), .failure(.interactionRequired))
        // A backend that cannot report attributes still reads by reference, with an empty service.
        let silent = RefBackend(secrets: [ref: (errSecSuccess, Data("x".utf8))], refLookup: [ref: .success(nil)])
        let item = try? FGP.read(persistentRef: ref, policy: .allowPrompt, backend: silent).get()
        XCTAssertEqual(item?.service, "")
        XCTAssertEqual(item?.string, "x")
    }

    // MARK: 2. Raw status

    func testDetailedFailureKeepsTheStatusesAccessDeniedMerges() {
        let ref = Data("r".utf8)
        for status in [errSecAuthFailed, errSecUserCanceled, errSecNotAvailable] {
            let backend = RefBackend(items: [match("a", 1, ref: "r")], secrets: [ref: (status, nil)])
            XCTAssertEqual(FGP.read(service: "svc", account: nil, policy: .allowPrompt, backend: backend), .failure(.accessDenied), "plain read is unchanged")
            XCTAssertEqual(FGP.readDetailed(service: "svc", account: nil, policy: .allowPrompt, backend: backend),
                           .failure(.init(error: .accessDenied, status: status)), "status \(status)")
        }
        let by = RefBackend(secrets: [ref: (errSecUserCanceled, nil)], refLookup: [ref: .success(nil)])
        XCTAssertEqual(FGP.readDetailed(persistentRef: ref, policy: .allowPrompt, backend: by),
                       .failure(.init(error: .accessDenied, status: errSecUserCanceled)))
    }

    func testDetailedKeepsStatusForNoPromptMappingsAndEnumerationFailures() {
        let ref = Data("r".utf8)
        for status in [errSecItemNotFound, errSecInteractionNotAllowed, errSecAuthFailed] {
            let backend = RefBackend(items: [match("a", 1, ref: "r")], secrets: [ref: (status, nil)])
            XCTAssertEqual(FGP.readDetailed(service: "svc", account: nil, policy: .noPrompt, backend: backend),
                           .failure(.init(error: .interactionRequired, status: status)))
        }
        let enumerationFailure = RefBackend(enumerationStatus: .init(error: .accessDenied, status: errSecNotAvailable))
        XCTAssertEqual(FGP.readDetailed(service: "svc", account: nil, policy: .noPrompt, backend: enumerationFailure),
                       .failure(.init(error: .accessDenied, status: errSecNotAvailable)))
        XCTAssertEqual(FGP.readDetailed(service: "svc", account: nil, policy: .noPrompt, backend: RefBackend()),
                       .failure(.init(error: .notFound, status: nil)))
        XCTAssertEqual(FGP.readDetailed(service: "", account: nil, policy: .noPrompt, backend: RefBackend()),
                       .failure(.init(error: .invalidArguments, status: nil)))
    }

    func testDefaultBackendAttributesDetailedDerivesStatusFromTheErrorCase() {
        struct Plain: FGP.Backend {
            var result: Result<[FGP.Match], FGP.ReadError>
            func attributes(service: String, account: String?, policy: FGP.Policy) -> Result<[FGP.Match], FGP.ReadError> { result }
            func data(forPersistentRef ref: Data, policy: FGP.Policy) -> (status: OSStatus, data: Data?) { (errSecItemNotFound, nil) }
        }
        func status(_ error: FGP.ReadError) -> Int32?? {
            if case let .failure(f) = Plain(result: .failure(error)).attributesDetailed(service: "s", account: nil, policy: .noPrompt) { return .some(f.status) }
            return nil
        }
        XCTAssertEqual(status(.keychainStatus(-25308)), .some(-25308))
        XCTAssertEqual(status(.notFound), .some(errSecItemNotFound))
        XCTAssertEqual(status(.accessDenied), .some(nil))
    }

    // MARK: 3. UI interaction

    private final class Capture: @unchecked Sendable {
        var queries: [[String: Any]] = []
        var interactionDuring: [Bool?] = []
    }

    private func backend(_ capture: Capture, status: OSStatus = errSecItemNotFound) -> FGP.SystemBackend {
        FGP.SystemBackend(copyMatching: { query, _ in
            capture.queries.append(query as? [String: Any] ?? [:])
            capture.interactionDuring.append(FGP.processInteractionAllowed())
            return status
        })
    }

    func testMetadataQueriesNeverFlipTheSwitchNorAskForSecretData() throws {
        let outside = try XCTUnwrap(FGP.processInteractionAllowed(), "interaction getter unavailable")
        let capture = Capture()
        let system = backend(capture)

        _ = system.attributesDetailed(service: "svc", account: "a", policy: .noPrompt)
        _ = system.attributesDetailed(service: "svc", account: nil, policy: .allowPrompt)
        _ = system.attributes(forPersistentRef: Data("r".utf8))

        XCTAssertEqual(capture.queries.count, 3)
        for (query, during) in zip(capture.queries, capture.interactionDuring) {
            XCTAssertNil(query[kSecReturnData as String], "metadata queries must not request the secret")
            XCTAssertEqual(query[kSecReturnAttributes as String] as? Bool, true)
            XCTAssertNotNil(query[kSecUseAuthenticationContext as String], "per-query no-UI context")
            XCTAssertEqual(query[kSecUseAuthenticationUI as String] as? String, kSecUseAuthenticationUISkip as String)
            XCTAssertEqual(during, outside, "the process-wide switch was flipped during a metadata query")
        }
        XCTAssertEqual(FGP.processInteractionAllowed(), outside)
    }

    func testDataReadFlipsTheSwitchOnlyForNoPromptAndRestoresIt() throws {
        let outside = try XCTUnwrap(FGP.processInteractionAllowed(), "interaction getter unavailable")
        let capture = Capture()
        let system = backend(capture)

        _ = system.data(forPersistentRef: Data("r".utf8), policy: .noPrompt)
        _ = system.data(forPersistentRef: Data("r".utf8), policy: .allowPrompt)

        XCTAssertEqual(capture.queries.compactMap { $0[kSecReturnData as String] as? Bool }, [true, true])
        XCTAssertEqual(capture.interactionDuring[0], false, "no-prompt data read runs with interaction off")
        XCTAssertEqual(capture.interactionDuring[1], outside, "a prompting read leaves the switch alone")
        XCTAssertNotNil(capture.queries[0][kSecUseAuthenticationContext as String])
        XCTAssertNil(capture.queries[1][kSecUseAuthenticationContext as String], "a prompting read carries no no-UI controls")
        XCTAssertEqual(FGP.processInteractionAllowed(), outside, "switch restored")
    }

    func testNoPromptReadDoesNotOpenAWindowWhileAPromptingReadIsInFlight() {
        let promptStarted = DispatchSemaphore(value: 0), releasePrompt = DispatchSemaphore(value: 0), done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            _ = FGP.withPolicy(.allowPrompt) { promptStarted.signal(); releasePrompt.wait(); return errSecSuccess }
            done.signal()
        }
        XCTAssertEqual(promptStarted.wait(timeout: .now() + 5), .success)
        var ran = false
        let started = Date()
        let status = FGP.withPolicy(.noPrompt) { ran = true; return errSecSuccess }
        XCTAssertEqual(status, errSecInteractionNotAllowed)
        XCTAssertFalse(ran, "the no-prompt body must not run (and must not flip the switch) under an open dialog")
        XCTAssertLessThan(Date().timeIntervalSince(started), 1, "it must not wait for the dialog")
        XCTAssertEqual(FGP.error(forStatus: status, policy: .noPrompt), .interactionRequired)
        releasePrompt.signal()
        XCTAssertEqual(done.wait(timeout: .now() + 5), .success)
        // With the dialog gone the window opens again.
        XCTAssertEqual(FGP.withPolicy(.noPrompt) { errSecSuccess }, errSecSuccess)
    }

    func testPromptingReadWaitsForAnOpenNoPromptWindowAndWindowsAreSerialised() {
        let windowOpen = DispatchSemaphore(value: 0), releaseWindow = DispatchSemaphore(value: 0), finished = DispatchGroup()
        let order = OrderLog()
        finished.enter()
        DispatchQueue.global().async {
            _ = FGP.withPolicy(.noPrompt) { windowOpen.signal(); releaseWindow.wait(); order.add("window-end"); return errSecSuccess }
            finished.leave()
        }
        XCTAssertEqual(windowOpen.wait(timeout: .now() + 5), .success)
        finished.enter()
        DispatchQueue.global().async {
            _ = FGP.withPolicy(.allowPrompt) { order.add("prompt-start"); return errSecSuccess }
            finished.leave()
        }
        finished.enter()
        let secondStatus = StatusBox()
        DispatchQueue.global().async {
            secondStatus.set(FGP.withPolicy(.noPrompt) { order.add("second-window"); return errSecSuccess })
            finished.leave()
        }
        Thread.sleep(forTimeInterval: 0.3)
        XCTAssertEqual(order.snapshot, [], "both wait for the open window")
        releaseWindow.signal()
        XCTAssertEqual(finished.wait(timeout: .now() + 5), .success)
        let log = order.snapshot
        XCTAssertEqual(log.first, "window-end", "nothing starts inside the open window")
        XCTAssertTrue(log.contains("prompt-start"), "the prompting read runs once the window closes")
        // Once the window closes, the waiting no-prompt read races the prompting read that registered
        // meanwhile: it either opens its own window (after window-end) or, finding the prompting read
        // in flight, is refused at once without running its body. Both are correct; running early is not.
        switch secondStatus.value {
        case errSecSuccess:
            XCTAssertTrue(log.contains("second-window"))
        case errSecInteractionNotAllowed:
            XCTAssertFalse(log.contains("second-window"), "a refused no-prompt body must not run")
        default:
            XCTFail("unexpected status \(String(describing: secondStatus.value))")
        }
        XCTAssertEqual(log.count, Set(log).count, "no body ran twice")
    }

    private final class StatusBox: @unchecked Sendable {
        private let lock = NSLock(); private var status: OSStatus?
        func set(_ value: OSStatus) { lock.withLock { status = value } }
        var value: OSStatus? { lock.withLock { status } }
    }

    private final class OrderLog: @unchecked Sendable {
        private let lock = NSLock(); private var items: [String] = []
        func add(_ item: String) { lock.withLock { items.append(item) } }
        var snapshot: [String] { lock.withLock { items } }
    }
}
#endif
