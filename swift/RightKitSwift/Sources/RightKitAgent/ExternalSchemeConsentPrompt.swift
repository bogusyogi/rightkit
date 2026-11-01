import SwiftUI

/// ENG-113: the one place a non-http(s) scheme handoff or external-app open can be approved.
/// Presented from `ExternalSchemeConsentStore.pendingRequest` — never shown speculatively, and
/// never auto-dismissed with an implied "allow". Both buttons require an explicit tap; there is
/// no default action a stray tap or a timeout can trigger.
@MainActor
struct ExternalSchemeConsentPrompt: View {
    let request: ExternalSchemeConsentStore.PendingRequest
    let onDecide: (ExternalSchemeConsentStore.Decision.Choice, _ remember: Bool) -> Void
    let onDismiss: () -> Void

    @State private var remember = true

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Open outside CodeRight?")
                    .font(.headline)
                    .foregroundStyle(Theme.text)
                Text(explanation)
                    .font(.subheadline)
                    .foregroundStyle(Theme.muted)
            }

            VStack(alignment: .leading, spacing: 8) {
                LabeledContent("Scheme") {
                    Text(request.scheme)
                        .font(.system(.body, design: .monospaced))
                }
                LabeledContent("Link") {
                    Text(request.url.absoluteString)
                        .font(.system(.footnote, design: .monospaced))
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                if let origin = request.origin {
                    LabeledContent("From") {
                        Text(origin)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
            }
            .foregroundStyle(Theme.text)
            .padding(12)
            .background(Theme.raised, in: RoundedRectangle(cornerRadius: 10))

            Toggle("Remember this choice for \(request.scheme):// links", isOn: $remember)
                .font(.footnote)
                .tint(Theme.accent2)

            HStack(spacing: 12) {
                Button(role: .cancel) {
                    onDecide(.deny, remember)
                } label: {
                    Text("Don't Open")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)

                Button {
                    onDecide(.allow, remember)
                } label: {
                    Text("Open")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
            }
        }
        .padding(20)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .overlay(
            RoundedRectangle(cornerRadius: 16)
                .strokeBorder(Theme.border)
        )
        .padding(.horizontal, 24)
        // A swipe-to-dismiss or system interruption resolves as "don't open," never as
        // consent — matching `ExternalSchemeConsentStore.dismissPending()`.
        .onDisappear {
            onDismiss()
        }
    }

    private var explanation: String {
        "This link leaves CodeRight and hands off to another app or system prompt. It came from content the assistant cannot fully vouch for — nothing opens until you approve it."
    }
}

/// Convenience modifier: attaches the consent prompt as a sheet driven entirely by
/// `ExternalSchemeConsentStore.pendingRequest`, so any screen that can encounter an external
/// scheme (the browser surface, a rendered message, ...) gets the same fail-closed behavior
/// by attaching one modifier rather than re-implementing presentation.
struct ExternalSchemeConsentPresenting: ViewModifier {
    @Environment(ExternalSchemeConsentStore.self) private var consentStore

    func body(content: Content) -> some View {
        content.sheet(item: Binding(
            get: { consentStore.pendingRequest },
            set: { newValue in
                if newValue == nil {
                    consentStore.dismissPending()
                }
            }
        )) { request in
            ExternalSchemeConsentPrompt(
                request: request,
                onDecide: { choice, remember in
                    consentStore.recordDecision(choice: choice, remember: remember)
                },
                onDismiss: {
                    // The sheet's own dismissal already calls dismissPending() via the
                    // binding above when pendingRequest is cleared by recordDecision; this
                    // covers the swipe-to-dismiss path where pendingRequest is still set.
                    consentStore.dismissPending()
                }
            )
            .presentationDetents([.medium])
            .presentationBackground(Theme.background)
        }
    }
}

extension View {
    /// Attach ENG-113's consent sheet to any view whose environment has an
    /// `ExternalSchemeConsentStore`.
    func externalSchemeConsentPresenting() -> some View {
        modifier(ExternalSchemeConsentPresenting())
    }
}
