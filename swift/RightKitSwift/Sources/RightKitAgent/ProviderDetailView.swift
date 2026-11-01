import SwiftUI

struct ProviderDetailView: View {
    let accountID: String
    @Environment(ProviderAccountStore.self) private var providers
    @Environment(ModelRoutingStore.self) private var routing
    @Environment(AppLockGate.self) private var gate
    @Environment(\.dismiss) private var dismiss
    @State private var checking = false
    @State private var status: String?
    @State private var failed = false
    @State private var removing = false
    @State private var editing = false
    private var account: ProviderAccount? { providers.accounts.first { $0.id == accountID } }
    var body: some View {
        Group {
            if let account {
                Form {
                    Section {
                        VStack(alignment: .leading, spacing: 8) {
                            Label(account.label, systemImage: "sparkles")
                                .font(Theme.body(size: 26, weight: .bold, relativeTo: .title2))
                            Text(account.openCodeService == .go ? "Your Go subscription" : account.openCodeService == .zen ? "Free models & usage-based billing" : "Direct provider connection")
                                .foregroundStyle(Theme.muted)
                        }.padding(.vertical, 12)
                    }
                    if let service = account.openCodeService {
                        Section {
                            Picker("Billing route", selection: Binding(get: { service }, set: {
                                guard $0 != service else { return }
                                routing.removeAccount(account.id)
                                status = nil
                                providers.setOpenCodeService($0, for: account)
                            })) {
                                Text("Go subscription").tag(OpenCodeService.go)
                                Text("Zen balance & free").tag(OpenCodeService.zen)
                            }.pickerStyle(.segmented).accessibilityIdentifier("provider-billing-route")
                        } footer: {
                            Text(service == .go ? "Uses your OpenCode Go allowance. Zen balance is separate." : "Uses Zen’s free models or pay-as-you-go balance, separate from Go allowance.")
                        }
                    }
                    Section("Connection") {
                        LabeledContent("Endpoint", value: account.baseURL).font(Theme.body(size: 14))
                        LabeledContent("Protocol", value: account.wireProtocol.displayName)
                        LabeledContent("Authentication", value: account.authMode.displayName)
                        if account.authMode == .apiKey {
                            Button("Edit connection") { editing = true }
                                .accessibilityIdentifier("provider-edit")
                        }
                        NavigationLink("Models & visibility") { ProviderModelInventoryView(account: account) }
                    }
                    Section {
                        Button {
                            Task { await test(account) }
                        } label: {
                            HStack { Label("Test connection", systemImage: "arrow.clockwise"); Spacer(); if checking { ProgressView() } }
                        }.disabled(checking || account.authMode != .apiKey).accessibilityIdentifier("provider-test")
                        if account.authMode != .apiKey {
                            Text("This account authenticates when you start a chat. Catalog connection tests support API keys.")
                                .font(.footnote).foregroundStyle(Theme.muted)
                        }
                        if let status { Text(status).foregroundStyle(failed ? Theme.danger : Theme.accent2).font(.footnote) }
                    }
                    Section {
                        Button("Remove provider", role: .destructive) { removing = true }
                    } footer: { Text("Saved chats remain on this device. Their provider must be configured to continue them.") }
                }.scrollContentBackground(.hidden).background(Theme.background)
            } else { ContentUnavailableView("Provider removed", systemImage: "sparkles") }
        }.navigationTitle(account?.label ?? "Provider").navigationBarTitleDisplayMode(.inline)
            .sheet(isPresented: $editing) {
                if let account { AddProviderView(account: account) }
            }
            .onChange(of: account) { _, _ in status = nil }
            .confirmationDialog("Remove this provider?", isPresented: $removing, titleVisibility: .visible) {
                Button("Remove provider", role: .destructive) {
                    if let account { routing.removeAccount(account.id); providers.remove(account) }; dismiss()
                }
            }
    }
    private func test(_ account: ProviderAccount) async {
        guard account.authMode == .apiKey else { return }
        checking = true; status = nil
        defer { checking = false }
        do {
            let authentication = try await providers.sessionAuthentication(for: account, gate: gate)
            let models = try await ModelCatalog.fetch(wireProtocol: account.wireProtocol, baseURL: account.baseURL, apiKey: authentication.runtimeBootstrapCredential)
            status = "Connected · \(models.count) models available"; failed = false
        } catch { status = error.localizedDescription; failed = true }
    }
}
