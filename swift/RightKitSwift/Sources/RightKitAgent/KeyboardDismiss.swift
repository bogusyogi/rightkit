#if os(iOS)
import SwiftUI
import UIKit

/// Dismisses the keyboard from anywhere. `@FocusState` is scoped to the view that declares it,
/// so a parent (transcript body, form background) cannot reach the composer's binding — this
/// resigns first responder globally instead, which is the one mechanism every surface can call.
enum KeyboardDismiss {
    static func now() {
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil
        )
    }
}
#endif
