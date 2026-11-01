import Foundation

/// What the notch face asks of nearby sharing: the Send ring's short pulse after a chat
/// message. The Mac's `NearbySharing` conforms (Notch/MacNotchAdapters.swift).
@MainActor
protocol NearbyActivitySource {
    func chatActivity() -> ActivitySummary?
}

/// The platform seam, like `TextMeasuringRegistry`: the Mac registers its source at launch
/// (`MacNotchAdapters.register()`); nil until then and on a platform with no nearby sharing.
@MainActor
enum NearbyActivityHost {
    static var source: NearbyActivitySource?
}
