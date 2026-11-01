#if os(macOS)
import AppKit

extension NSScreen: ScreenDescribing {
    var frameValue: CGRect { frame }
    var visibleFrameValue: CGRect { visibleFrame }

    /// Unlike `CGDirectDisplayID`, this UUID survives display reconfiguration
    /// and restarts, so a saved choice still names the same physical monitor.
    var displayIdentifier: String? {
        let screenNumber = NSDeviceDescriptionKey("NSScreenNumber")
        guard let number = deviceDescription[screenNumber] as? NSNumber,
              let unmanaged = CGDisplayCreateUUIDFromDisplayID(number.uint32Value)
        else { return nil }
        let uuid = unmanaged.takeRetainedValue()
        return CFUUIDCreateString(nil, uuid) as String
    }

    /// Measured from the two menu-bar strips *either side* of the notch, which
    /// is the only thing AppKit describes directly. A display without a notch
    /// reports no auxiliary areas.
    var hardwareNotch: HardwareNotch? {
        guard let left = auxiliaryTopLeftArea, let right = auxiliaryTopRightArea else {
            return nil
        }
        let width = frame.width - left.width - right.width
        let height = HardwareNotch.height(safeAreaTop: safeAreaInsets.top,
                                          beside: [left.height, right.height])
        guard width > 0, height > 0 else { return nil }
        return HardwareNotch(width: width, height: height)
    }
}

extension NotchGeometry {
    static func preferredScreen(
        from screens: [NSScreen],
        preference: DisplayPreference = .followActiveWindow
    ) -> NSScreen? {
        preferredScreen(from: screens, preference: preference, activeScreen: NSScreen.main)
    }
}
#endif
