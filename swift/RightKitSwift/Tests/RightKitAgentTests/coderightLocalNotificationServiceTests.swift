import Foundation
import UserNotifications
import XCTest
@testable import RightKitAgent
/// `UNNotificationRequest` is not `Sendable`, so it cannot cross the actor
/// boundary. Capture the Sendable facts the assertions actually need instead of
/// storing the request objects themselves.
struct CapturedNotification: Sendable, Equatable {
    let identifier: String
    let sessionId: String?
}

private final class MockNotificationCenter: LocalNotificationCenter, @unchecked Sendable {
    // Not an actor: `UNNotificationRequest` is non-Sendable, so it cannot cross an
    // actor boundary at all. A lock gives the same serialization without forcing
    // the parameter over an isolation edge.
    private let lock = NSLock()
    private var storage: [CapturedNotification] = []
    private var statusValue: UNAuthorizationStatus

    var addedRequests: [CapturedNotification] {
        lock.withLock { storage }
    }

    var status: UNAuthorizationStatus {
        get { lock.withLock { statusValue } }
        set { lock.withLock { statusValue = newValue } }
    }

    init(status: UNAuthorizationStatus = .authorized) {
        self.statusValue = status
    }

    func requestAuthorization(options: UNAuthorizationOptions) async throws -> Bool {
        status = .authorized
        return true
    }

    func add(_ request: UNNotificationRequest) async throws {
        let captured = CapturedNotification(
            identifier: request.identifier,
            sessionId: request.content.userInfo["sessionId"] as? String
        )
        lock.withLock { storage.append(captured) }
    }

    func authorizationStatus() async -> UNAuthorizationStatus {
        status
    }
}

@MainActor
final class LocalNotificationServiceTests: XCTestCase {
    func testPostsExactlyOneNotificationPerCompletionEvent() async {
        let center = MockNotificationCenter()
        let service = LocalNotificationService(center: center)

        await service.notify(sessionId: "session-1", title: "Done", kind: .turnCompleted)

        let requests = center.addedRequests
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.sessionId, "session-1")
    }

    func testPostsExactlyOneNotificationPerNeedsInputEvent() async {
        let center = MockNotificationCenter()
        let service = LocalNotificationService(center: center)

        await service.notify(sessionId: "session-2", title: "Approve?", kind: .needsInput)
        await service.notify(sessionId: "session-2", title: "Approve?", kind: .needsInput)

        // Same session + kind is deduplicated by identifier — a caller re-notifying about the
        // same still-pending event never stacks duplicate alerts. Two distinct sessions or
        // kinds would each get their own request.
        let requests = center.addedRequests
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(Set(requests.map(\.identifier)).count, 1)
    }

    func testDoesNotNotifyWithoutAuthorization() async {
        let center = MockNotificationCenter(status: .denied)
        let service = LocalNotificationService(center: center)

        await service.notify(sessionId: "session-3", title: "Done", kind: .turnCompleted)

        let requests = center.addedRequests
        XCTAssertTrue(requests.isEmpty)
    }
}
