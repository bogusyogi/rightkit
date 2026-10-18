import SwiftUI

/// A small helper for the app's `App.body`. The app owns its `App`; this only returns the window.
///
///     @main struct MyApp: App {
///         var body: some Scene {
///             HubScene.windowGroup("My App", sections: sections, context: context)
///         }
///     }
public enum HubScene {
    /// A `WindowGroup` hosting a `HubShell` with its own selection state.
    ///
    /// - Parameters:
    ///   - title: The window title.
    ///   - id: The window group id, for opening it by id.
    ///   - sections: The hub's sections.
    ///   - context: What every section's view is given.
    ///   - initialSelection: The section id selected when the window opens.
    @MainActor
    public static func windowGroup(
        _ title: String,
        id: String = "hub",
        sections: [any HubSection],
        context: HubContext,
        initialSelection: String? = nil
    ) -> some Scene {
        WindowGroup(title, id: id) {
            HubSceneRoot(sections: sections, context: context, initialSelection: initialSelection)
        }
    }
}

/// The view `HubScene` puts in its window: a `HubShell` over a selection it owns.
public struct HubSceneRoot: View {
    private let sections: [any HubSection]
    private let context: HubContext
    @State private var selection: String?

    public init(sections: [any HubSection], context: HubContext, initialSelection: String? = nil) {
        self.sections = sections
        self.context = context
        _selection = State(initialValue: initialSelection)
    }

    public var body: some View {
        HubShell(sections: sections, selection: $selection, context: context)
            .frame(minWidth: 760, minHeight: 480)
    }
}
