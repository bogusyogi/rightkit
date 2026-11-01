import LocalAuthentication
import SwiftUI

/// Manage model/search credentials plus on-device workspaces.
struct LocalSetupView: View {
    @Environment(ProviderAccountStore.self) private var providers
    @Environment(ModelRoutingStore.self) private var modelRouting
    @Environment(LocalWorkspaceStore.self) private var workspaces
    @Environment(AppLockGate.self) private var lockGate
    @Environment(WebSearchCredentialStore.self) private var webSearch

    @State private var showAddProvider = false
    @State private var showAddTokenPlan = false
    @State private var newWorkspaceName = ""
    @State private var pendingDelete: LocalWorkspace?
    @State private var tinyFishKey = ""
    @State private var tinyFishError: String?

    private let availability = SecretStore.availability()

    var body: some View {
        Form {
            providersSection
            webSearchSection
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Providers & models")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(isPresented: $showAddProvider) {
            AddProviderView()
        }
        .sheet(isPresented: $showAddTokenPlan) {
            TokenPlanAccountView()
        }
        .alert("Delete workspace?", isPresented: deleteBinding) {
            Button("Cancel", role: .cancel) { pendingDelete = nil }
            Button("Delete", role: .destructive) {
                if let pendingDelete { workspaces.delete(pendingDelete) }
                pendingDelete = nil
            }
        } message: {
            Text(
                "This permanently removes \(pendingDelete?.name ?? "the workspace") and everything in it "
                    + "from this device. Unpushed commits cannot be recovered."
            )
        }
    }

    // MARK: - Sections

    @ViewBuilder
    private var biometricsSection: some View {
        Section {
            if availability.canEvaluate {
                @Bindable var lockGate = lockGate
                Toggle("Require \(biometryName)", isOn: $lockGate.requireBiometrics)
            } else {
                LabeledContent("Protection", value: "Unavailable")
                Text(availability.reason ?? "Authentication is unavailable.")
                    .font(.footnote)
                    .foregroundStyle(Theme.danger)
            }
        } header: {
            Text("Credentials")
        } footer: {
            Text(credentialsDetail)
        }
    }

    @ViewBuilder
    private var providersSection: some View {
        Section {
            if providers.accounts.isEmpty {
                Text("No provider added")
                    .foregroundStyle(Theme.muted)
            }
            ForEach(providers.accounts) { account in
                NavigationLink {
                    ProviderDetailView(accountID: account.id)
                } label: {
                    DesignRow(title: account.label,
                        subtitle: account.openCodeService == .go ? "Go subscription" : account.openCodeService == .zen ? "Zen balance & free models" : account.wireProtocol.displayName,
                        icon: "sparkles", showsChevron: false)
                }
            }
            .onDelete { offsets in
                offsets.map { providers.accounts[$0] }.forEach { account in
                    modelRouting.removeAccount(account.id)
                    providers.remove(account)
                }
            }
            NavigationLink("Model preferences") {
                ModelRoutingSettingsView()
            }
            Button("Add provider") { showAddProvider = true }
            Button("Add token plan") { showAddTokenPlan = true }
        } header: {
            Text("Model providers")
        } footer: {
            Text("You pay the provider directly. CodeRight never proxies your requests.")
        }
    }

    @ViewBuilder
    private var webSearchSection: some View {
        Section {
            if webSearch.tinyFishConfigured {
                LabeledContent("TinyFish", value: "Configured")
                Button("Remove TinyFish key", role: .destructive) {
                    webSearch.removeTinyFish()
                }
            } else {
                SecureField("TinyFish API key", text: $tinyFishKey)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Save TinyFish key") {
                    do {
                        try webSearch.saveTinyFish(tinyFishKey)
                        tinyFishKey = ""
                        tinyFishError = nil
                    } catch {
                        tinyFishError = error.localizedDescription
                    }
                }
                .disabled(tinyFishKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            if let tinyFishError {
                Text(tinyFishError)
                    .font(.footnote)
                    .foregroundStyle(Theme.danger)
            }
        } header: {
            Text("Web search")
        } footer: {
            Text("TinyFish is preferred when configured. DuckDuckGo works without a key until then.")
        }
    }

    @ViewBuilder
    private var workspacesSection: some View {
        Section {
            if workspaces.workspaces.isEmpty {
                Text("No workspace yet")
                    .foregroundStyle(Theme.muted)
            }
            ForEach(workspaces.workspaces) { workspace in
                VStack(alignment: .leading, spacing: 2) {
                    Text(workspace.name)
                        .foregroundStyle(Theme.text)
                    Text(sizeLabel(workspace))
                        .font(.footnote)
                        .foregroundStyle(Theme.muted)
                }
                .swipeActions {
                    Button("Delete", role: .destructive) { pendingDelete = workspace }
                }
            }

            HStack {
                TextField("New workspace name", text: $newWorkspaceName)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Create") {
                    if workspaces.create(named: newWorkspaceName) != nil {
                        newWorkspaceName = ""
                    }
                }
                .disabled(newWorkspaceName.trimmingCharacters(in: .whitespaces).isEmpty)
            }

            if let error = workspaces.lastError {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(Theme.danger)
            }
        } header: {
            Text("Workspaces")
        } footer: {
            Text("Workspaces live in this app's private storage and are excluded from iCloud backup.")
        }
    }

    @ViewBuilder
    private var executionTargetsSection: some View {
        Section {
            NavigationLink("Capabilities, cost & data per target") {
                LocalTargetDisclosureView()
            }
        } header: {
            Text("Execution targets")
        } footer: {
            Text(
                "See what each place a session can run (this device, a connected cloud endpoint, or a "
                    + "paired computer) can do, who pays for it, and where your data goes."
            )
        }
    }

    // MARK: - Helpers

    private var biometryName: String {
        switch availability.biometryKind {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "your passcode"
        }
    }

    private var credentialsDetail: String {
        if lockGate.requireBiometrics {
            let minutes = Int(lockGate.graceInterval / 60)
            return "\(biometryName) is required when you use a key after \(minutes) minutes away — not on every "
                + "screen. Keys leave this device only for your chosen provider."
        }
        return "Keys stay on-device without another \(biometryName) step. Turn this on for a stronger check "
            + "before using a key."
    }

    private func sizeLabel(_ workspace: LocalWorkspace) -> String {
        guard let bytes = workspaces.sizeOnDisk(workspace) else { return "—" }
        return ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private var deleteBinding: Binding<Bool> {
        Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } })
    }
}

/// A named service preset: fills in wire protocol, endpoint, and a starting
/// model so a user pastes a key without ever typing a URL.
///
/// Deliberately separate from `ProviderAccount.WireProtocol`, which is the
/// wire FORMAT the runtime speaks (openai | anthropic) — several vendors
/// share one format. OpenRouter and OpenCode Zen are both OpenAI-compatible
/// gateways with different endpoints and completely different keys; picking
/// "OpenAI-compatible" and leaving the default `api.openai.com` URL, which is
/// what happens if a user does not know what "endpoint" means, silently
/// sends an OpenRouter/OpenCode key to the wrong host and fails opaquely.
private enum ProviderPreset: String, CaseIterable, Identifiable {
    case anthropic, openai, openRouter, openCodeGo, openCodeZen, custom
    var id: String { rawValue }

    var label: String {
        switch self {
        case .anthropic: return "Anthropic"
        case .openai: return "OpenAI"
        case .openRouter: return "OpenRouter"
        case .openCodeGo: return "OpenCode Go"
        case .openCodeZen: return "OpenCode Zen"
        case .custom: return "Custom endpoint"
        }
    }

    var wireProtocol: ProviderAccount.WireProtocol {
        switch self {
        case .anthropic: return .anthropic
        default: return .openai
        }
    }

    var baseURL: String {
        switch self {
        case .anthropic: return "https://api.anthropic.com"
        case .openai: return "https://api.openai.com/v1"
        case .openRouter: return "https://openrouter.ai/api/v1"
        // Confirmed against models.dev/providers/opencode, 2026-08-28.
        case .openCodeGo: return OpenCodeService.go.baseURL
        case .openCodeZen: return OpenCodeService.zen.baseURL
        case .custom: return ""
        }
    }

    var model: String {
        switch self {
        case .anthropic: return "claude-sonnet-4-5"
        case .openai: return "gpt-5.1"
        case .openRouter: return "z-ai/glm-5.3-flash"
        // Provider catalogs change. New accounts deliberately begin without a
        // selected model; discovery decides what this account can use.
        case .openCodeGo, .openCodeZen: return ""
        case .custom: return ""
        }
    }
}

/// Add one provider account plus its key.
struct AddProviderView: View {
    private enum Field: Hashable { case name, url, key }
    let account: ProviderAccount?
    @Environment(ModelRoutingStore.self) private var routing
    @Environment(AppLockGate.self) private var gate
    @Environment(ProviderAccountStore.self) private var providers
    @Environment(\.dismiss) private var dismiss

    @State private var checking = false
    @FocusState private var focusedField: Field?
    @State private var preset: ProviderPreset = .anthropic
    @State private var wireProtocol: ProviderAccount.WireProtocol = .anthropic
    @State private var label = ProviderPreset.anthropic.label
    @State private var baseURL = ProviderPreset.anthropic.baseURL
    @State private var model = ProviderPreset.anthropic.model
    @State private var apiKey = ""
    @State private var error: String?

    init(account: ProviderAccount? = nil) {
        self.account = account
        if let account {
            _preset = State(initialValue: ProviderPreset.allCases.first { $0.baseURL == account.baseURL } ?? .custom)
            _wireProtocol = State(initialValue: account.wireProtocol)
            _label = State(initialValue: account.label)
            _baseURL = State(initialValue: account.baseURL)
            _model = State(initialValue: account.model)
        }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("Service", selection: $preset) {
                        ForEach(ProviderPreset.allCases) { option in
                            Text(option.label).tag(option)
                        }
                    }
                    .accessibilityIdentifier("provider-editor-service")
                    .onChange(of: preset) { _, new in
                        if label.isEmpty || ProviderPreset.allCases.contains(where: { $0.label == label }) {
                            label = new.label
                        }
                        wireProtocol = new.wireProtocol
                        if new != .custom || ProviderPreset.allCases.contains(where: { $0.baseURL == baseURL }) {
                            baseURL = new.baseURL
                        }
                        if model.isEmpty || ProviderPreset.allCases.contains(where: { $0.model == model }) {
                            model = new.model
                        }
                    }
                } footer: {
                    Text(
                        "Picking a service fills its endpoint. \"Custom endpoint\" is for a self-hosted or "
                            + "unlisted OpenAI-compatible gateway."
                    )
                }

                Section {
                    TextField("Name", text: $label)
                        .focused($focusedField, equals: .name)
                        .accessibilityIdentifier("provider-editor-name")
                        .autocorrectionDisabled()
                    TextField("Base URL", text: $baseURL)
                        .focused($focusedField, equals: .url)
                        .accessibilityIdentifier("provider-editor-url")
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    if preset == .custom {
                        Picker("Protocol", selection: $wireProtocol) {
                            ForEach(ProviderAccount.WireProtocol.allCases, id: \.self) { Text($0.displayName).tag($0) }
                        }
                    }
                } header: {
                    Text("Provider")
                } footer: {
                    Text("Test & save checks this connection before changing your account. Available models appear automatically.")
                }

                Section {
                    SecureField(account == nil ? "API key" : "New API key (optional)", text: $apiKey)
                        .focused($focusedField, equals: .key)
                        .accessibilityIdentifier("provider-editor-key")
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("Key")
                } footer: {
                    Text(
                        account == nil
                            ? "Stored in Keychain. CodeRight never displays saved keys."
                            : "Leave blank to keep your current key. A replacement is saved only after its connection succeeds."
                    )
                }

                if checking { Section { ProgressView("Checking connection…") } }
            }
            .disabled(checking)
            .safeAreaInset(edge: .top) {
                if let error {
                    Text(error).font(.callout).foregroundStyle(Theme.danger)
                        .frame(maxWidth: .infinity, alignment: .leading).padding()
                        .background(Theme.surface).accessibilityIdentifier("provider-editor-error")
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle(account == nil ? "Add Provider" : "Edit Provider")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }.disabled(checking)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Test & save") { Task { await save() } }
                        .disabled(!isValid || checking).accessibilityIdentifier("provider-editor-save")
                }
            }
        }
    }

    private var biometryName: String {
        switch SecretStore.availability().biometryKind {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "your passcode"
        }
    }

    private var isValid: Bool {
        guard let url = URL(string: baseURL.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "https", let host = url.host, !host.isEmpty,
              url.user == nil, url.password == nil else { return false }
        return !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && (account != nil || !apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    private func save() async {
        guard isValid, !checking else { return }
        focusedField = nil
        checking = true; error = nil
        defer { checking = false }
        do {
            let replacement = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
            let key: String
            if replacement.isEmpty, let account {
                guard account.authMode == .apiKey else { return }
                key = try await providers.sessionAuthentication(for: account, gate: gate).runtimeBootstrapCredential
            } else { key = replacement }
            let endpoint = baseURL.trimmingCharacters(in: .whitespacesAndNewlines)
            let catalog = try await ModelCatalog.fetch(wireProtocol: wireProtocol, baseURL: endpoint, apiKey: key)
            guard !catalog.isEmpty else {
                error = "Connected, but this account returned no models. Check its endpoint & access."
                return
            }
            let requested = model.trimmingCharacters(in: .whitespacesAndNewlines)
            let saved = ProviderAccount(
                id: account?.id ?? UUID().uuidString,
                label: label.trimmingCharacters(in: .whitespacesAndNewlines),
                wireProtocol: wireProtocol, baseURL: endpoint,
                model: catalog.contains(requested) ? requested : catalog[0]
            )
            try providers.save(saved, apiKey: key)
            if let account, account.baseURL != endpoint || account.wireProtocol != wireProtocol {
                routing.removeAccount(account.id)
            }
            routing.setCatalog(catalog, for: saved.id)
            dismiss()
        } catch {
            self.error = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}
