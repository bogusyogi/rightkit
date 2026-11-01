#if os(macOS)
import CoreGraphics

/// The bulb's reach, shared with the notch's hit-testing (`NotchViewModel`) so it does not
/// depend on the `NotchBulb` view. `NotchBulb` reads these.
enum NotchBulbMetrics {
    /// Where the bulb's centre is when fully out, measured from the cap's edge.
    static let outCentre: CGFloat = 41
    static let outRadius: CGFloat = 22
    /// What a press or a click on the bulb answers to: its out size and a
    /// little over, so the pointer does not have to be exact on a moving thing.
    static let hitRadius: CGFloat = outRadius + 6
}
#endif
