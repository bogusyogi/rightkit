import Foundation
import RightKitHubQA

extension HubFixtures {
    /// Canned `pulse-engine` replies for the Activity section, shaped like the real commands'
    /// results. `activity_timeline` answers relative to now, so the picture always has "Today" and
    /// "Yesterday" groups, a full first page (so "Load older" shows) and a short older page.
    static func activity(_ engine: QAEngineClient) {
        engine.respond(to: "activity_timeline") { params in
            let request = (try? JSONSerialization.jsonObject(with: params)) as? [String: Any] ?? [:]
            let limit = max(1, (request["limit"] as? NSNumber)?.intValue ?? 50)
            let before = (request["before"] as? NSNumber)?.int64Value
            let now = Int64(Date().timeIntervalSince1970)
            var rows = ActivityFixtureRows.rows(now: now)
            if let before { rows = rows.filter { ($0["at"] as? Int64 ?? 0) < before } }
            let page: [[String: Any]] = Array(rows.prefix(limit))
            let body: [String: Any] = ["rows": page, "unreadable": ["Drive health"]]
            return try JSONSerialization.data(withJSONObject: body)
        }
    }
}

/// Fifty-eight invented timeline rows, newest first, spread over about two weeks.
private enum ActivityFixtureRows {
    private static let gaps: [Int64] = [900, 2400, 5400, 11000, 26000, 41000, 3600, 70000]

    /// (kind, summary, section that owns it)
    private static let templates: [(String, String, String?)] = [
        ("Cleanup", "Moved 14 items to the Trash, 1.2 GB", "cleanup"),
        ("Scan", "Scanned /Users/demo: 412,380 entries, 38,214 folders", "storage"),
        ("Send", "Sent 3 files to Studio Mac", nil),
        ("Chat", "Message received from Studio Mac", "chat"),
        ("Uninstall", "Moved Sketchbook to the Trash: 6 items, up to 480 MB on disk. Space is not freed until the Trash is emptied.", "apps"),
        ("Undo", "Restored 4 items", "cleanup"),
        ("Drive health", "Backup Drive: reallocated sector count rose from 0 to 2", "storage"),
        ("Send", "Received 12 files from Studio Mac", nil),
        ("Cleanup", "Moved 3 items to the Trash, 86 MB; 1 restored", "cleanup"),
        ("Scan", "Scanned /Users/demo/Projects: 96,412 entries, 7,830 folders", "storage"),
        ("Chat", "Message received from Dell", "chat"),
        ("Send", "Could not send 2 files to Dell", nil),
    ]

    static func rows(now: Int64) -> [[String: Any]] {
        var at = now
        var out: [[String: Any]] = []
        for index in 0..<58 {
            at -= gaps[index % gaps.count] + Int64(index) * 37
            let template = templates[index % templates.count]
            var row: [String: Any] = ["at": at, "kind": template.0, "summary": template.1]
            row["open"] = template.2 ?? NSNull()
            out.append(row)
        }
        return out
    }
}
