#if os(macOS)
import Foundation

/// The one process flag the shared notch face needs, without `Runtime`'s account-home lookups.
public enum RuntimeFlags {
    public static let isUnderTest: Bool = {
        #if PULSE_QA
        let environment = ProcessInfo.processInfo.environment
        return environment["XCTestConfigurationFilePath"] != nil
            || environment["XCTestBundlePath"] != nil
            || environment["XCTestSessionIdentifier"] != nil
            || NSClassFromString("XCTestCase") != nil
            || Bundle.allBundles.contains { $0.bundleURL.pathExtension == "xctest" }
            || environment["PULSE_RENDER_VIEWS"] == "1"
        #else
        return false
        #endif
    }()
}
#endif
