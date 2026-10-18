import SwiftUI

private struct HubServicesKey: EnvironmentKey {
    static let defaultValue: any HubPlatformServices = UnavailablePlatformServices()
}

public extension EnvironmentValues {
    /// The platform services of the hub this view is in. `HubShell` sets it from its `HubContext`, so
    /// a view that needs one thing from the system (a folder chooser, image decoding) reads it here
    /// instead of being handed the whole context. Outside a shell it is `UnavailablePlatformServices`:
    /// every action throws "unsupported" and no image decodes, which previews can live with.
    var hubServices: any HubPlatformServices {
        get { self[HubServicesKey.self] }
        set { self[HubServicesKey.self] = newValue }
    }
}
