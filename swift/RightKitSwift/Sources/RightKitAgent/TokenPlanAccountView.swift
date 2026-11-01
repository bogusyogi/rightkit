import SwiftUI

/// Isolated setup sheet for prepaid token-plan accounts. Hosting settings
/// choose when to present it; this view never falls back to API-key storage.
struct TokenPlanAccountView: View {
    @Environment(ProviderAccountStore.self) private var providers
    @Environment(\.dismiss) private var dismiss

    @State private var label = ""
    @State private var baseURL = ""
    @State private var model = ""
    @State private var planID = ""
    @State private var remainingQuota = ""
    @State private var refreshEndpoint = ""
    @State private var refreshToken = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Provider") {
                    TextField("Name", text: $label)
                    TextField("Base URL", text: $baseURL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    TextField("Model", text: $model)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }

                Section {
                    TextField("Plan ID", text: $planID)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Remaining quota", text: $remainingQuota)
                        .keyboardType(.numberPad)
                    TextField("Refresh endpoint", text: $refreshEndpoint)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    SecureField("Refresh credential", text: $refreshToken)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("Token plan")
                } footer: {
                    Text(
                        "Plan ID, endpoint, & quota identify this plan. Refresh credential is stored only in "
                            + "Keychain; it is never written to account settings."
                    )
                }

                if let error {
                    Section { Text(error).foregroundStyle(Theme.danger) }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("Add Token Plan")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", action: save).disabled(!isValid)
                }
            }
        }
    }

    private var quota: UInt64? {
        UInt64(remainingQuota.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private var isValid: Bool {
        quota != nil && !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && baseURL.hasPrefix("https://")
            && TokenPlanConfiguration(
                planID: planID,
                remainingQuota: quota ?? 0,
                refreshEndpoint: refreshEndpoint
            ).isValid
            && !refreshToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func save() {
        guard let quota else { return }
        let account = ProviderAccount(
            label: label.trimmingCharacters(in: .whitespacesAndNewlines),
            wireProtocol: .openai,
            baseURL: baseURL.trimmingCharacters(in: .whitespacesAndNewlines),
            model: model.trimmingCharacters(in: .whitespacesAndNewlines),
            authMode: .tokenPlan,
            tokenPlan: TokenPlanConfiguration(
                planID: planID.trimmingCharacters(in: .whitespacesAndNewlines),
                remainingQuota: quota,
                refreshEndpoint: refreshEndpoint.trimmingCharacters(in: .whitespacesAndNewlines)
            )
        )
        do {
            try providers.saveTokenPlan(account, refreshToken: refreshToken)
            dismiss()
        } catch {
            self.error = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}
