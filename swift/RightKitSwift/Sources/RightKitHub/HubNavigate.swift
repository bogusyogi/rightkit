import SwiftUI

/// Opens another section of the hub by id, the way a section's own link ("Open Cleanup") does.
/// `HubShell` puts one in the environment that sets its selection; outside a shell it does nothing.
///
///     @Environment(\.hubNavigate) private var navigate
///     Button("Open Cleanup") { navigate("cleanup") }
public struct HubNavigateAction: Sendable {
    private let open: @MainActor @Sendable (String) -> Void

    public init(_ open: @escaping @MainActor @Sendable (String) -> Void) {
        self.open = open
    }

    @MainActor public func callAsFunction(_ sectionID: String) {
        open(sectionID)
    }

    /// Does nothing: the default outside a `HubShell`.
    public static let none = HubNavigateAction { _ in }
}

private struct HubNavigateKey: EnvironmentKey {
    static let defaultValue = HubNavigateAction.none
}

public extension EnvironmentValues {
    /// Opens a hub section by id. Set by `HubShell`.
    var hubNavigate: HubNavigateAction {
        get { self[HubNavigateKey.self] }
        set { self[HubNavigateKey.self] = newValue }
    }
}
