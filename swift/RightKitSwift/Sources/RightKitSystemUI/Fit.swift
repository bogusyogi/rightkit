import SwiftUI

// Layout priorities that differ on Windows.
//
// SwiftRight's stack layout measures a Text offered zero width as its full single-line width (the
// renderer reads a zero bound as "no bound"), so a Text never gives space back to a sibling of higher
// priority: every Text's whole width stays in reserve for the lower groups, and the group served
// first is the one that is squeezed. Two things follow, and neither happens on the Mac, where
// SwiftUI lets text shrink first:
//
// - In a row whose texts fit only just, a stack offers each child in a priority group an equal share
//   of what is left, so the first long label beside a short one is cut ("Unused 90+…") although the
//   row has room for both. Distinct priorities, longest first, serve each child what it needs.
// - In a row that is too tight, the highest priority child takes the cut. Put the text that may
//   shrink or truncate (a title column, a note) at the highest priority, and keep values, buttons
//   and badges below it.
//
// The Mac keeps its own priorities: these helpers return the Mac value there.

enum Fit {
    /// The priority of a label among the choices of a segmented control: longer labels first.
    /// Zero on the Mac.
    static func segment(_ label: String) -> Double {
        ACPlatform.isWindows ? Double(label.count) : 0
    }

    /// A priority on Windows, and `other` on the Mac.
    static func priority(windows: Double, other: Double = 0) -> Double {
        ACPlatform.isWindows ? windows : other
    }

    /// A length on Windows, and `other` on the Mac: a column widened for Segoe UI.
    static func length(windows: CGFloat, other: CGFloat) -> CGFloat {
        ACPlatform.isWindows ? windows : other
    }
}
