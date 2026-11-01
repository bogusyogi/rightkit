import Foundation

/// The rule that turns a remembered order into a real one.
///
/// The awkward part is not applying an order, it is that the set of providers is
/// not fixed: Claude Code contributes one per `~/.claude-<slug>` found at launch,
/// so an id can appear on a Mac that has never seen it and vanish from one that
/// has. A stored order is a preference to reconcile, never an authority.
enum ProviderOrder {
    /// The notch cells for a set of snapshots, in their given order.
    static func cells(from snapshots: [ProviderSnapshot],
                      keeping previous: [ProviderSnapshot]) -> [ProviderSnapshot] {
        snapshots.flatMap(\.notchSnapshots)
    }

    /// `items` in the user's order, then everything the order has never seen, in
    /// the order it arrived in.
    ///
    /// Idempotent, which is what lets the store re-apply it to an already-sorted
    /// `snapshots` without the rings shuffling.
    static func arrange<T>(_ items: [T], by order: [String], id: (T) -> String) -> [T] {
        guard !order.isEmpty else { return items }

        var remaining = items
        var arranged: [T] = []
        for providerID in order {
            // A stored id no provider claims is the ordinary case, not
            // corruption: a `~/.claude-work` that is not on this Mac today.
            guard let index = remaining.firstIndex(where: { id($0) == providerID })
            else { continue }
            arranged.append(remaining.remove(at: index))
        }
        // Appended rather than dropped: a provider added by a new version has to
        // turn up somewhere, and the end is the one position that is never a lie
        // about what the user chose.
        return arranged + remaining
    }

    /// The order to remember, given the order now on screen.
    ///
    /// Ids that are remembered but not visible are kept, at the end: a Claude
    /// profile whose directory is not on this Mac today must not lose its place
    /// because an unrelated row moved.
    static func remember(_ visible: [String], keeping remembered: [String]) -> [String] {
        visible + remembered.filter { !visible.contains($0) }
    }
}
