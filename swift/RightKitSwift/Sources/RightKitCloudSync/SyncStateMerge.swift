import Foundation

/// Three-way merge of two `CloudSyncState` writers that started from the same `base`.
///
/// `FileSyncStateStore` uses it so a read-modify-write from a second process (app plus share or
/// keyboard extension) cannot silently discard the first writer's token, versions or tombstones.
/// Rules, per field:
/// - A side that did not change a value defers to the side that did; a deletion is a change.
/// - Both sides changed the same key differently: the later version/date wins. A modification
///   beats a concurrent deletion of the same key, so no writer loses data it just produced.
/// - Sets (pending deletions, aliases, markers) merge as `(base ∩ mine ∩ theirs) ∪ added-by-either`.
/// - Scalars (`changeToken`, `pendingLegacyAdoption`): `mine` if `mine` changed, otherwise `theirs`.
///   A stale token only costs a re-fetch, because versions are merged as a superset.
/// - Ledgers for different iCloud accounts never merge: if `accountID` differs, `mine` wins whole.
extension CloudSyncState {
    static func merged(base: CloudSyncState, mine: CloudSyncState, theirs: CloudSyncState) -> CloudSyncState {
        guard mine.accountID == theirs.accountID else { return mine }
        var out = CloudSyncState()
        out.accountID = mine.accountID
        out.changeToken = scalar(base.changeToken, mine.changeToken, theirs.changeToken)
        out.pendingLegacyAdoption = scalar(base.pendingLegacyAdoption, mine.pendingLegacyAdoption, theirs.pendingLegacyAdoption)
        out.remoteVersions = dictionary(base.remoteVersions, mine.remoteVersions, theirs.remoteVersions) {
            $0.version >= $1.version ? $0 : $1
        }
        out.tombstones = dictionary(base.tombstones, mine.tombstones, theirs.tombstones) { max($0, $1) }
        out.quarantined = dictionary(base.quarantined, mine.quarantined, theirs.quarantined) { max($0, $1) }
        out.pendingDeletions = set(base.pendingDeletions, mine.pendingDeletions, theirs.pendingDeletions)
        out.markers = set(base.markers, mine.markers, theirs.markers)
        var aliases: [String: Set<String>] = [:]
        for key in Set(base.aliases.keys).union(mine.aliases.keys).union(theirs.aliases.keys) {
            let merged = set(base.aliases[key] ?? [], mine.aliases[key] ?? [], theirs.aliases[key] ?? [])
            if !merged.isEmpty { aliases[key] = merged }
        }
        out.aliases = aliases
        return out
    }

    private static func scalar<T: Equatable>(_ base: T, _ mine: T, _ theirs: T) -> T { mine != base ? mine : theirs }

    private static func set<T: Hashable>(_ base: Set<T>, _ mine: Set<T>, _ theirs: Set<T>) -> Set<T> {
        base.intersection(mine).intersection(theirs).union(mine.subtracting(base)).union(theirs.subtracting(base))
    }

    private static func dictionary<K: Hashable, V: Equatable>(
        _ base: [K: V], _ mine: [K: V], _ theirs: [K: V], newer: (V, V) -> V
    ) -> [K: V] {
        var out: [K: V] = [:]
        for key in Set(base.keys).union(mine.keys).union(theirs.keys) {
            let b = base[key], m = mine[key], t = theirs[key]
            switch (m != b, t != b) {
            case (false, false), (false, true): if let t { out[key] = t }
            case (true, false): if let m { out[key] = m }
            case (true, true):
                switch (m, t) {
                case let (m?, t?): out[key] = m == t ? m : newer(m, t)
                case let (m?, nil): out[key] = m
                case let (nil, t?): out[key] = t
                case (nil, nil): break
                }
            }
        }
        return out
    }
}
