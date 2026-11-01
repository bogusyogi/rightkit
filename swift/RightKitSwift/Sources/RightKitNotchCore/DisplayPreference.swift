import Foundation

/// Whether the notch follows keyboard focus or stays on one physical display.
public enum DisplayPreference: Hashable {
    case followActiveWindow
    case display(String)
}
