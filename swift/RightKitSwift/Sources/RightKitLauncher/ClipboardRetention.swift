import Foundation

// Retention (docs/clipboard-spec.md 4) as a pure plan: which unprotected items go. The store
// runs it on its background queue and applies the answer on the main thread.

enum ClipRetention {
    /// The fixed cap of the screenshot lane (4).
    static let screenshotLaneBytes = 100 * 1024 * 1024

    struct Plan: Sendable {
        /// Ids to delete: unprotected only.
        var evict: Set<String> = []
        /// Bytes used by pinned and board items when they alone exceed the byte cap, else nil.
        var protectedOverCap: Int?
    }

    /// Caps are checked together and the strictest wins; eviction is oldest `lastUsedAt` first
    /// among items that are not protected. Protected items count toward the totals and are
    /// never evicted (a user who pins more than the cap stays over it).
    static func plan(items: [ClipItem], thumbs: [String: Int], settings: ClipSettings, now: Double) -> Plan {
        // The ranges (50 to 5000 and so on) are the settings layer's; the store takes any positive value.
        let maxItems = max(1, settings.maxItems)
        let maxBytes = max(1, settings.maxMB) * 1024 * 1024
        let days = max(0, settings.maxAgeDays)
        let hours = max(0, settings.screenshotHours)

        func bytes(_ item: ClipItem) -> Int { item.size + (thumbs[item.id] ?? 0) }

        var evict = Set<String>()
        // Age: main lane by days since last use, screenshot lane by hours.
        for item in items where !item.isProtected {
            switch item.lane {
            case .main:
                if days > 0, now - item.lastUsedAt > Double(days) * 86_400 { evict.insert(item.id) }
            case .screenshot:
                if hours > 0, now - item.lastUsedAt > Double(hours) * 3_600 { evict.insert(item.id) }
            }
        }

        // Oldest first, among what is still here and unprotected.
        func oldest(_ lane: ClipLane?) -> [ClipItem] {
            items.filter { !evict.contains($0.id) && !$0.isProtected && (lane == nil || $0.lane == lane) }
                .sorted { $0.lastUsedAt < $1.lastUsedAt }
        }

        // Count: the main lane only; screenshots never count against it.
        let mainCount = items.filter { $0.lane == .main && !$0.isProtected && !evict.contains($0.id) }.count
        if mainCount > maxItems {
            for item in oldest(.main).prefix(mainCount - maxItems) { evict.insert(item.id) }
        }

        // Screenshot lane bytes.
        var laneBytes = items.filter { $0.lane == .screenshot && !evict.contains($0.id) }
            .reduce(0) { $0 + bytes($1) }
        if laneBytes > screenshotLaneBytes {
            for item in oldest(.screenshot) {
                guard laneBytes > screenshotLaneBytes else { break }
                evict.insert(item.id)
                laneBytes -= bytes(item)
            }
        }

        // Total bytes, thumbnails included.
        var total = items.filter { !evict.contains($0.id) }.reduce(0) { $0 + bytes($1) }
        if total > maxBytes {
            for item in oldest(nil) {
                guard total > maxBytes else { break }
                evict.insert(item.id)
                total -= bytes(item)
            }
        }

        let kept = items.filter { $0.isProtected }.reduce(0) { $0 + bytes($1) }
        return Plan(evict: evict, protectedOverCap: kept > maxBytes ? kept : nil)
    }
}
