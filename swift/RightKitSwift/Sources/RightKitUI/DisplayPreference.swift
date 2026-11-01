import Foundation

/// Whether the notch follows keyboard focus or stays on one physical display.
enum DisplayPreference: Hashable {
    case followActiveWindow
    case display(String)
}
