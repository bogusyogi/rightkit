import SwiftUI

/// Asks before a destructive action: a confirmation dialog with a destructive confirm button and a cancel.
/// Wraps `.confirmationDialog` (proven in SwiftRight); use `hubConfirm` rather than the raw modifier.
public struct HubConfirm: ViewModifier {
    private let title: String
    private let message: String?
    private let confirmLabel: String
    private let isPresented: Binding<Bool>
    private let action: @MainActor () -> Void

    public init(
        title: String,
        message: String? = nil,
        confirmLabel: String,
        isPresented: Binding<Bool>,
        action: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.message = message
        self.confirmLabel = confirmLabel
        self.isPresented = isPresented
        self.action = action
    }

    @ViewBuilder
    public func body(content: Content) -> some View {
        if let message {
            content.confirmationDialog(title, isPresented: isPresented) {
                Button(confirmLabel, role: .destructive) { action() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(verbatim: message)
            }
        } else {
            content.confirmationDialog(title, isPresented: isPresented) {
                Button(confirmLabel, role: .destructive) { action() }
                Button("Cancel", role: .cancel) {}
            }
        }
    }
}

public extension View {
    /// Shows a destructive confirmation dialog while `isPresented` is true. `action` runs only on confirm.
    func hubConfirm(
        _ title: String,
        message: String? = nil,
        confirmLabel: String = "Delete",
        isPresented: Binding<Bool>,
        action: @escaping @MainActor () -> Void
    ) -> some View {
        modifier(HubConfirm(
            title: title,
            message: message,
            confirmLabel: confirmLabel,
            isPresented: isPresented,
            action: action
        ))
    }
}
