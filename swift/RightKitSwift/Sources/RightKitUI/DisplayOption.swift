#if os(macOS)
import AppKit

/// A connected display as it appears in Settings.
struct DisplayOption: Identifiable, Equatable {
    let id: String
    let name: String

    @MainActor
    static var connected: [DisplayOption] {
        NSScreen.screens.compactMap { screen in
            screen.displayIdentifier.map { DisplayOption(id: $0, name: screen.localizedName) }
        }
    }
}
#endif
