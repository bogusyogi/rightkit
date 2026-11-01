import SwiftUI

/// Settings for provider-backed model inventory, chat picker visibility, &
/// default model choices. This is intentionally separate from credentials:
/// catalog IDs are harmless metadata, while keys stay in Keychain.
struct ModelRoutingSettingsView: View {
    @Environment(ProviderAccountStore.self) private var providers
    @Environment(ModelRoutingStore.self) private var routing

    var body: some View {
        Form {
            Section {
                if providers.accounts.isEmpty {
                    Text("Add a provider first")
                        .foregroundStyle(Theme.muted)
                }
                ForEach(providers.accounts) { account in
                    NavigationLink {
                        ProviderModelInventoryView(account: account)
                    } label: {
                        LabeledContent(account.label, value: inventorySummary(for: account))
                    }
                }
            } header: {
                Text("Provider models")
            } footer: {
                Text("Refresh each provider, then enable only models you want in chat picker.")
            }

            Section {
                NavigationLink {
                    ModelReferencePicker(
                        title: "Orchestrator model",
                        selection: routing.orchestrator(),
                        allowsInheritance: false
                    ) { routing.setOrchestrator($0) }
                } label: {
                    LabeledContent("Default model", value: referenceLabel(routing.orchestrator()))
                }
                .accessibilityIdentifier("routing-default-model")
            } header: {
                Text("Orchestrator")
            } footer: {
                Text("Chat starts with this enabled model. You can still switch models from chat picker.")
            }


        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Model preferences")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func inventorySummary(for account: ProviderAccount) -> String {
        let all = routing.catalog(for: account.id).count
        let enabled = routing.enabledModels(for: account.id).count
        guard all > 0 else { return "Refresh" }
        return "\(enabled) of \(all) enabled"
    }

    private func referenceLabel(_ reference: ModelReference?, inherited: Bool = false) -> String {
        guard let reference else { return inherited ? "Inherit orchestrator" : "Choose model" }
        let provider = providers.accounts.first { $0.id == reference.accountID }?.label ?? "Removed provider"
        return "\(provider) · \(shortModelName(reference.modelID))"
    }
}

struct ProviderModelInventoryView: View {
    let account: ProviderAccount

    @Environment(ProviderAccountStore.self) private var providers
    @Environment(AppLockGate.self) private var lockGate
    @Environment(ModelRoutingStore.self) private var routing
    @State private var isRefreshing = false
    @State private var error: String?

    var body: some View {
        List {
            if let refreshed = routing.refreshedAt(for: account.id) {
                Text("Updated \(refreshed.formatted(.relative(presentation: .named)))")
                    .font(.footnote)
                    .foregroundStyle(Theme.muted)
            }

            let models = routing.catalog(for: account.id)
            if models.isEmpty, !isRefreshing {
                ContentUnavailableView(
                    "No models loaded",
                    systemImage: "arrow.triangle.2.circlepath",
                    description: Text("Refresh to pull models available to this provider account.")
                )
                .listRowBackground(Color.clear)
            }

            if !models.isEmpty {
                Section {
                    ForEach(models, id: \.self) { modelID in
                        Toggle(modelID, isOn: Binding(
                            get: { routing.isVisible(modelID, for: account.id) },
                            set: { routing.setVisible($0, modelID: modelID, accountID: account.id) }
                        ))
                        .accessibilityIdentifier("model-visibility-\(modelID)")
                        .tint(Theme.accent)
                    }
                } header: {
                    Text("Shown in chat picker")
                }
            }

            if let error {
                Section {
                    Text(error)
                        .foregroundStyle(Theme.danger)
                }
            }
        }
        .overlay {
            if isRefreshing { ProgressView("Refreshing models…") }
        }
        .navigationTitle(account.label)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Refresh") { Task { await refresh() } }
                    .disabled(isRefreshing)
            }
        }
        .task {
            if routing.catalog(for: account.id).isEmpty { await refresh() }
        }
    }

    private func refresh() async {
        guard let latest = providers.accounts.first(where: { $0.id == account.id }) else {
            error = "This provider was removed."
            return
        }
        guard latest.authMode == .apiKey else {
            error = "Model refresh is not available for \(latest.authMode.displayName) accounts yet."
            return
        }
        isRefreshing = true
        error = nil
        defer { isRefreshing = false }
        do {
            let authentication = try await providers.sessionAuthentication(for: latest, gate: lockGate)
            let models = try await ModelCatalog.fetch(
                wireProtocol: latest.wireProtocol,
                baseURL: latest.baseURL,
                apiKey: authentication.runtimeBootstrapCredential
            )
            routing.setCatalog(models, for: latest.id)
        } catch {
            self.error = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}

private struct ModelReferencePicker: View {
    let title: String
    let selection: ModelReference?
    let allowsInheritance: Bool
    let onSelect: (ModelReference?) -> Void

    @Environment(ProviderAccountStore.self) private var providers
    @Environment(ModelRoutingStore.self) private var routing
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        List {
            if allowsInheritance {
                Button {
                    onSelect(nil)
                    dismiss()
                } label: {
                    Label("Inherit orchestrator", systemImage: selection == nil ? "checkmark" : "arrow.turn.down.right")
                }
            }

            let references = routing.availableReferences(accounts: providers.accounts)
            if references.isEmpty {
                ContentUnavailableView(
                    "No enabled models",
                    systemImage: "slider.horizontal.3",
                    description: Text("Refresh a provider, then enable at least one model.")
                )
                .listRowBackground(Color.clear)
            } else {
                ForEach(references) { reference in
                    Button {
                        onSelect(reference)
                        dismiss()
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(shortModelName(reference.modelID))
                                Text(providers.accounts.first { $0.id == reference.accountID }?.label ?? "Removed provider")
                                    .font(.footnote)
                                    .foregroundStyle(Theme.muted)
                            }
                            Spacer()
                            if selection == reference {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(Theme.accent)
                            }
                        }
                    }
                }
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}

private func shortModelName(_ modelID: String) -> String {
    modelID.split(separator: "/").last.map(String.init) ?? modelID
}
