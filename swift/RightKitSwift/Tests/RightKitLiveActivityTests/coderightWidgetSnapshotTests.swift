import XCTest
@testable import RightKitLiveActivity
@MainActor
final class WidgetSnapshotTests: XCTestCase {
    func testSnapshotRedactsDaemonCredentialsAndWorkspacePath() throws {
        let defaults = UserDefaults(suiteName: "WidgetSnapshotTests.redaction")!
        defaults.removePersistentDomain(forName: "WidgetSnapshotTests.redaction")
        try WidgetSnapshotStore.write(
            sessions: [WidgetSession(id: "s1", title: "Build", project: "coderight", needsAttention: true)],
            attentionSessionIDs: ["s1"],
            runtimeLabel: "Mac daemon",
            defaults: defaults,
            reloadTimelines: false
        )

        let data = try XCTUnwrap(defaults.data(forKey: WidgetSnapshotStore.snapshotKey))
        let raw = try XCTUnwrap(String(bytes: data, encoding: .utf8))
        XCTAssertFalse(raw.contains("token"))
        XCTAssertFalse(raw.contains("http://"))
        XCTAssertFalse(raw.contains("/Users/"))
        XCTAssertEqual(try WidgetSnapshotStore.load(defaults: defaults).attentionCount, 1)
    }

    func testEmptyRecentAndAttentionStatesRoundTrip() throws {
        let defaults = UserDefaults(suiteName: "WidgetSnapshotTests.states")!
        defaults.removePersistentDomain(forName: "WidgetSnapshotTests.states")
        try WidgetSnapshotStore.write(
            sessions: [
                WidgetSession(id: "recent", title: "Recent", updatedAt: "2026-08-27T10:00:00Z"),
                WidgetSession(id: "attention", title: "Needs input")
            ],
            attentionSessionIDs: ["attention"],
            defaults: defaults,
            reloadTimelines: false
        )

        let snapshot = try WidgetSnapshotStore.load(defaults: defaults)
        XCTAssertFalse(snapshot.isEmpty)
        XCTAssertEqual(snapshot.recentSessions.count, 2)
        XCTAssertEqual(snapshot.recentSessions.filter(\.needsAttention).map(\.id), ["attention"])
        XCTAssertEqual(WidgetSnapshot.empty.attentionCount, 0)
        XCTAssertTrue(WidgetSnapshot.empty.isStale(at: Date()))
    }

    func testSnapshotCapsRecentSessions() throws {
        let defaults = UserDefaults(suiteName: "WidgetSnapshotTests.cap")!
        defaults.removePersistentDomain(forName: "WidgetSnapshotTests.cap")
        let sessions = (0..<10).map { WidgetSession(id: "s\($0)", title: "Session \($0)") }
        try WidgetSnapshotStore.write(sessions: sessions, defaults: defaults, reloadTimelines: false)
        XCTAssertEqual(
            try WidgetSnapshotStore.load(defaults: defaults).recentSessions.count,
            WidgetSnapshotStore.maxSessions
        )
    }
}
