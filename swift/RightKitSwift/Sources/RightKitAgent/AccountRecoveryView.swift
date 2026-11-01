import SwiftUI

/// APP-15: entry point for secret-key backup, guided restore (manual / QR / lost-access),
/// and optional phone-number linking.
public struct AccountRecoveryView: View {
    private let store: AccountRecoveryStore
    @State private var backup: RecoveryBackup?
    @State private var errorMessage: String?
    @State private var showRestore = false
    @State private var showLostAccess = false

    public init(store: AccountRecoveryStore = AccountRecoveryStore()) {
        self.store = store
    }

    public var body: some View {
        List {
            Section("Back Up Your Key") {
                if let backup {
                    Text(backup.displayText)
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                    ShareLink(item: backup.displayText) {
                        Label("Share backup", systemImage: "square.and.arrow.up")
                    }
                } else {
                    Button("Reveal & back up recovery key") { runBackup() }
                }
                if let errorMessage {
                    Text(errorMessage).font(.footnote).foregroundStyle(Theme.danger)
                }
            }

            Section("Restore") {
                Button("Restore from backup") { showRestore = true }
                Button("Lost access") { showLostAccess = true }
            }

            Section("Additional Recovery") {
                Text("Phone recovery is not available yet. Keep a secure copy of your recovery key.")
                    .font(.footnote).foregroundStyle(Theme.muted)
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Account Recovery")
        .sheet(isPresented: $showRestore) { RecoveryRestoreView(store: store) }
        .sheet(isPresented: $showLostAccess) { LostAccessView() }
    }

    private func runBackup() {
        do {
            backup = try store.makeBackup()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// Guided restore: manual entry or QR scan, both funneled through `AccountRecoveryStore.restore`.
struct RecoveryRestoreView: View {
    let store: AccountRecoveryStore
    @Environment(\.dismiss) private var dismiss
    @State private var manualEntry = ""
    @State private var errorMessage: String?
    @State private var showScanner = false
    @State private var didRestore = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Manual Entry") {
                    TextField("XXXX-XXXX-XXXX-XXXX-XXXX", text: $manualEntry)
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                    Button("Restore") { restore(manualEntry) }
                }
                Section("Or Scan a QR Code") {
                    Button("Scan QR code") { showScanner = true }
                }
                if let errorMessage {
                    Text(errorMessage).foregroundStyle(Theme.danger)
                }
                if didRestore {
                    Text("Recovery key restored.").foregroundStyle(Theme.accent2)
                }
            }
            .navigationTitle("Restore Key")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .sheet(isPresented: $showScanner) {
                // Reuses the same `QRScannerView` used by daemon pairing (Pairing/). A
                // recovery-key QR is expected to encode the raw key text as its payload,
                // parsed as a URL query here for consistency with the pairing scanner's
                // `onScan: (URL) -> Void` shape; a plain-text QR falls back to its `absoluteString`.
                QRScannerView { url in
                    showScanner = false
                    let text = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                        .queryItems?.first(where: { $0.name == "key" })?.value ?? url.absoluteString
                    restore(text)
                }
            }
        }
    }

    private func restore(_ text: String) {
        do {
            try store.restore(from: text)
            didRestore = true
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// Lost-access path: no recovery key, no linked phone. Points at a support/feedback channel
/// rather than pretending to recover anything automatically.
struct LostAccessView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var showFeedback = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                Text("If you've lost your recovery key and don't have a linked phone number, "
                    + "we can't restore access automatically. Contact support with your install ID "
                    + "so we can verify ownership another way.")
                    .foregroundStyle(Theme.muted)
                Button("Contact support") { showFeedback = true }
                Spacer()
            }
            .padding()
            .navigationTitle("Lost Access")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .sheet(isPresented: $showFeedback) { FeedbackView() }
        }
    }
}

/// Optional phone-number linking. Backend verification is not wired up yet — see
/// `PhoneRecoveryLink`'s TODO(backend) — so this surfaces that state honestly rather than
/// pretending success.
struct PhoneRecoveryLinkView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var phoneNumber = ""
    @State private var errorMessage: String?
    private let link = PhoneRecoveryLink()

    var body: some View {
        NavigationStack {
            Form {
                Section("Phone Number") {
                    TextField("+1 555 0100", text: $phoneNumber)
                        .keyboardType(.phonePad)
                }
                Button("Send verification code") { requestCode() }
                if let errorMessage {
                    Text(errorMessage).foregroundStyle(Theme.muted).font(.footnote)
                }
            }
            .navigationTitle("Link Phone")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func requestCode() {
        Task {
            do {
                _ = try await link.requestVerificationCode(for: phoneNumber)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }
}
