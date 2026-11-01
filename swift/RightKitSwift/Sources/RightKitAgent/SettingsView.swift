import Foundation
import SwiftUI

/// Settings entry point reached from `SessionListView`'s toolbar: active daemon, a path to
/// `DaemonPickerView` (Storage/Pairing module) for managing paired daemons, and a push into
/// the License module's own `LicenseView` for activation/management.
struct SettingsView: View {
    @Environment(LocalWorkspaceStore.self) private var workspaces
    @Environment(DaemonStore.self) private var daemonStore
    @Environment(LicenseStore.self) private var licenseStore
    @Environment(\.dismiss) private var dismiss
    @State private var showDaemonPicker = false
    @State private var showCloudPicker = false
    @State private var showDiagnostics = false
    @State private var showDebugLog = false
    @State private var showWhatsNew = false
    @State private var showFeedback = false
    @State private var showMemoryPanel = false
    @State private var promptTemplateSyncOn = PromptTemplateSyncSettings.isOn(
        persisted: UserDefaults.standard.bool(forKey: PromptTemplateCloudSync.enabledKey))

    var body: some View {
        NavigationStack {
            Form {
                Section("Preferences") {
                    NavigationLink { AppearanceSettingsView() } label: {
                        Label("Appearance", systemImage: "circle.lefthalf.filled")
                    }
                    NavigationLink { PrivacySettingsView() } label: {
                        Label("Privacy & permissions", systemImage: "hand.raised")
                    }
                }
                Section {
                    // Disabled until the iCloud entitlement ships. Enabling later: set
                    // `PromptTemplateSyncSettings.isEntitlementReady`, then have this setter
                    // also call `PromptTemplateCloudSync.setEnabled` on the shared instance.
                    Toggle("Sync prompt templates", isOn: Binding(
                        get: { promptTemplateSyncOn },
                        set: { promptTemplateSyncOn = $0 }
                    ))
                    .disabled(!PromptTemplateSyncSettings.isToggleEnabled())
                    .accessibilityLabel("Sync prompt templates with iCloud")
                    .accessibilityIdentifier("prompt-template-sync-toggle")
                } header: {
                    Text("iCloud")
                } footer: {
                    Text(PromptTemplateSyncSettings.footer)
                }
                Section("Connections") {
                    NavigationLink { LocalSetupView() } label: { Label("Providers & models", systemImage: "sparkles") }
                    Button { showDaemonPicker = true } label: { Label("Connected computers", systemImage: "laptopcomputer").foregroundStyle(Theme.text) }
                    Button { showCloudPicker = true } label: { Label("Cloud workspace", systemImage: "cloud").foregroundStyle(Theme.text) }
                    NavigationLink { CloudflareProjectsView() } label: { Label("Cloudflare projects", systemImage: "cloud").foregroundStyle(Theme.text) }
                    NavigationLink { LocalFlyioSetupView() } label: { Label("Fly.io account", systemImage: "server.rack") }
                }
                Section("Advanced") {
                    NavigationLink { LocalMcpConnectorsView() } label: { Label("MCP connectors", systemImage: "link") }
                    NavigationLink {
                        SkillsCatalogView(store: SkillsCatalogStore(source: LocalSkillCatalogSource(
                            roots: workspaces.workspaces.map { workspaces.url(for: $0).appendingPathComponent(".coderight/skills").path })))
                    } label: { Label("Skills catalog", systemImage: "book") }
                    Button { showMemoryPanel = true } label: { Label("Memory", systemImage: "clock.arrow.circlepath").foregroundStyle(Theme.text) }
                    NavigationLink { SessionSecretsEditorView() } label: { Label("Session secrets", systemImage: "key") }
                    NavigationLink { UsagePanelView(ledger: .shared) } label: { Label("Usage", systemImage: "chart.bar") }
                    NavigationLink { MobileCapabilityStatusView() } label: { Label("Mobile capabilities", systemImage: "iphone") }
                    NavigationLink { OnDemandResourcePacksView() } label: { Label("Resource packs", systemImage: "shippingbox") }
                    NavigationLink { LocalTeamFleetView() } label: { Label("Peer fleet", systemImage: "person.2") }
                }
                Section("Support") {
                    NavigationLink { AboutCodeRightView() } label: { Label("About CodeRight", systemImage: "info.circle") }
                    Button { showDiagnostics = true } label: { Label("Diagnostics", systemImage: "stethoscope").foregroundStyle(Theme.text) }
                    Button { showDebugLog = true } label: { Label("Debug log", systemImage: "text.alignleft").foregroundStyle(Theme.text) }
                    Button { showFeedback = true } label: { Label("Send feedback", systemImage: "envelope").foregroundStyle(Theme.text) }
                    NavigationLink { AccountView() } label: { Label("Account", systemImage: "person.crop.circle") }
                    NavigationLink { LicenseView() } label: { Label("License", systemImage: "lock.shield") }
                    NavigationLink { AccountRecoveryView() } label: { Label("Account recovery", systemImage: "arrow.clockwise") }
                    Button { showWhatsNew = true } label: { Label("What’s new", systemImage: "sparkle").foregroundStyle(Theme.text) }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }.foregroundStyle(Theme.text).accessibilityIdentifier("settings-done")
                }
            }
            .sheet(isPresented: $showDaemonPicker) {
                NavigationStack {
                    DaemonPickerView().toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showDaemonPicker = false } } }
                }
            }
            .sheet(isPresented: $showCloudPicker) {
                NavigationStack {
                    DaemonPickerView(targetKind: .cloud).toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showCloudPicker = false } } }
                }
            }
            .sheet(isPresented: $showDiagnostics) {
                DiagnosticsView()
            }
            .sheet(isPresented: $showDebugLog) {
                DebugLogView()
            }
            .sheet(isPresented: $showWhatsNew) {
                WhatsNewView()
            }
            .sheet(isPresented: $showFeedback) {
                FeedbackView()
            }
            .sheet(isPresented: $showMemoryPanel) {
                MemoryPanelView()
            }
            .task { licenseStore.load() }
        }
    }
}

/// State for the prompt-template iCloud row. The row stays disabled while `isEntitlementReady`
/// is false, and nothing here constructs `PromptTemplateCloudSync`, its engine, or the CloudKit
/// transport. Reading the consent key is a plain UserDefaults read.
enum PromptTemplateSyncSettings {
    static let isEntitlementReady = false
    static let footer =
        "Keeps your prompt templates in step across your Apple devices. Available once iCloud is set up for CodeRight."

    static func isToggleEnabled(entitlementReady: Bool = isEntitlementReady) -> Bool {
        entitlementReady
    }

    /// The row never shows "on" without the entitlement, even if a stale consent flag is stored.
    static func isOn(persisted: Bool, entitlementReady: Bool = isEntitlementReady) -> Bool {
        entitlementReady && persisted
    }
}

@MainActor
private struct OnDemandResourcePacksView: View {
    @State private var manager = OnDemandResourceManager()

    var body: some View {
        List(manager.catalog) { pack in
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text(pack.displayName)
                    Spacer()
                    Text(stateLabel(manager.state(for: pack.tag)))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.muted)
                }
                Text(pack.tag)
                    .font(.caption.monospaced())
                    .foregroundStyle(Theme.muted)
                controls(for: pack)
            }
            .padding(.vertical, 4)
        }
        .navigationTitle("On-demand packs")
        .navigationBarTitleDisplayMode(.inline)
        .task { await manager.refreshFetchedPacks() }
    }

    @ViewBuilder
    private func controls(for pack: OnDemandResourcePack) -> some View {
        switch manager.state(for: pack.tag) {
        case .notFetched, .failed:
            Button("Download") {
                Task { try? await manager.fetch(tag: pack.tag) }
            }
        case .fetching(let progress):
            ProgressView(value: progress) {
                Text("Downloading")
            }
            Button("Cancel", role: .cancel) { manager.cancelFetch(tag: pack.tag) }
        case .fetched:
            HStack {
                Button("Pin") { manager.pin(tag: pack.tag) }
                Button("Release", role: .destructive) { manager.release(tag: pack.tag) }
            }
        case .pinned:
            HStack {
                Button("Unpin") { manager.unpin(tag: pack.tag) }
                Button("Release", role: .destructive) { manager.release(tag: pack.tag) }
            }
        }
    }

    private func stateLabel(_ state: OnDemandResourceState) -> String {
        switch state {
        case .notFetched: return "Not downloaded"
        case .fetching(let progress): return "\(Int(progress * 100))%"
        case .fetched: return "Downloaded"
        case .pinned: return "Pinned"
        case .failed(let error): return error.localizedDescription
        }
    }
}

/// A decoded capability row returned by the embedded Rust runtime.
struct MobileCapabilityStatus: Codable, Identifiable, Sendable {
    let id: String
    let name: String
    let available: Bool
    let reason: String
    let toolName: String?
}

struct MobileCapabilityReport: Codable, Sendable {
    let capabilities: [MobileCapabilityStatus]
    let resourcePacks: [String]
    let toolNames: [String]
}

private struct MobileCapabilityEnvelope: Codable, Sendable {
    let ok: Bool
    let value: MobileCapabilityReport?
    let error: String?
}

/// Shared capability discovery surface for Settings and an active local session.
/// It calls the C ABI catalog rather than mirroring readiness in Swift, so
/// missing model weights, transports, and orchestration routes remain visible.
struct MobileCapabilityStatusView: View {
    @Environment(LocalWorkspaceStore.self) private var workspaces
    @State private var report: MobileCapabilityReport?
    @State private var error: String?
    @State private var probeResult: String?
    @State private var onDemandResources = OnDemandResourceManager()

    var body: some View {
        List {
            Section {
                if let report {
                    ForEach(report.capabilities.filter { $0.id != "onDeviceInference" }) { capability in
                        let isWasmCompute = capability.id == "wasmCompute"
                        let isAvailable =
                            isWasmCompute
                            ? WasmComputeTool.isRuntimeConfigured
                            : capability.available
                        let reason =
                            isWasmCompute && WasmComputeTool.isRuntimeConfigured
                            ? "Built-in import-free interpreter is ready. Modules receive no filesystem, network, process, or extension host access."
                            : capability.reason
                        VStack(alignment: .leading, spacing: 5) {
                            HStack {
                                Text(capability.name)
                                Spacer()
                                Label(
                                    isAvailable ? "Available" : "Unavailable",
                                    systemImage: isAvailable ? "checkmark.circle" : "xmark.circle"
                                )
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(isAvailable ? Theme.accent : Theme.muted)
                            }
                            Text(reason)
                                .font(.footnote)
                                .foregroundStyle(Theme.muted)
                                .fixedSize(horizontal: false, vertical: true)
                            if isWasmCompute {
                                Text(
                                    "Bounds: \(WasmComputeTool.maxMemoryBytes / (1024 * 1024)) MiB memory · "
                                        + "\(WasmComputeTool.maxFuel) fuel · "
                                        + "\(WasmComputeTool.maxWallTimeMs) ms wall time · "
                                        + (WasmComputeTool.isRuntimeConfigured
                                            ? "Runtime configured." : "Runtime not configured.")
                                )
                                .font(.footnote)
                                .foregroundStyle(Theme.muted)
                                .fixedSize(horizontal: false, vertical: true)
                            }
                            if capability.id != "codeEditor" {
                                Button("Check") {
                                    Task {
                                        if isWasmCompute {
                                            await checkWasmCompute()
                                        } else {
                                            await probe(capability.id)
                                        }
                                    }
                                }
                                .font(.caption.weight(.semibold))
                                .disabled(probeResult == "Checking…")
                            }
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel(
                            "\(capability.name): \(isAvailable ? "Available" : "Unavailable"). \(reason)"
                        )
                    }
                } else if error == nil {
                    ProgressView("Checking mobile capabilities…")
                }
            } header: {
                Text("Embedded runtime")
            } footer: {
                Text(
                    "Unavailable means the required engine route or external artifact is not configured; it is not a failed ready state."
                )
            }

            if let report {
                Section("On-demand resources") {
                    if report.resourcePacks.isEmpty {
                        Text("No optional packs are downloaded.")
                            .foregroundStyle(Theme.muted)
                    } else {
                        ForEach(report.resourcePacks, id: \.self) { tag in
                            Label(tag, systemImage: "checkmark.circle.fill")
                                .foregroundStyle(Theme.accent2)
                        }
                    }
                }
            }

            if let report, !report.toolNames.isEmpty {
                Section("Registered portable tools") {
                    Text(report.toolNames.sorted().joined(separator: ", "))
                        .font(.footnote.monospaced())
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
            }

            if let probeResult {
                Section("Capability check") {
                    Text(probeResult)
                        .font(.footnote)
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
            }

            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(Theme.danger)
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Mobile capabilities")
        .navigationBarTitleDisplayMode(.inline)
        .task { await reload() }
        .refreshable { await reload() }
    }

    private func reload() async {
        do {
            await onDemandResources.refreshFetchedPacks()
            report = try await MobileCapabilityStatusBridge.fetch(
                resourcePacks: onDemandResources.fetchedResourcePackTags
            )
            error = nil
        } catch {
            report = nil
            self.error = error.localizedDescription
        }
    }

    private func probe(_ capability: String) async {
        probeResult = "Checking…"
        do {
            let message = try await MobileCapabilityStatusBridge.probe(
                capability: capability,
                sessionId: "mobile-capability-probe",
                workspaceRoot: (workspaces.workspaces.first.map { workspaces.url(for: $0).path })
                    ?? workspaces.root.path
            )
            probeResult = message
        } catch {
            probeResult = error.localizedDescription
        }
    }

    private func checkWasmCompute() async {
        probeResult = "Checking…"
        guard WasmComputeTool.isRuntimeConfigured,
            let pointer = coderight_mobile_wasm_executor_self_test()
        else {
            probeResult = "WASM runtime is not configured."
            return
        }
        defer { coderight_mobile_string_free(pointer) }
        guard let object = try? JSONSerialization.jsonObject(
            with: Data(String(cString: pointer).utf8)
        ) as? [String: Any] else {
            probeResult = "WASM runtime returned an invalid response."
            return
        }
        if object["ok"] as? Bool == true {
            probeResult = "Shared wasm_compute tool reached iOS WasmKit successfully."
        } else {
            probeResult = object["error"] as? String ?? "WASM compute failed."
        }
    }


}

/// Minimal Swift caller for the handle-independent Dispatch B/C catalog export.
enum MobileCapabilityStatusBridge {
    enum Failure: Error, LocalizedError {
        case unavailable(String)
        case malformed

        var errorDescription: String? {
            switch self {
            case .unavailable(let message): return message
            case .malformed: return "The mobile capability response was malformed."
            }
        }
    }

    static func fetch(resourcePacks: [String] = []) async throws -> MobileCapabilityReport {
        struct Request: Encodable {
            let fetchedResourcePacks: [String]
            let onDeviceInferenceAvailable: Bool
            let wasmComputeAvailable: Bool
        }
        let payload = try JSONEncoder().encode(
            Request(
                fetchedResourcePacks: resourcePacks,
                onDeviceInferenceAvailable: false,
                wasmComputeAvailable: WasmComputeTool.isRuntimeConfigured
            ))
        let raw: String? = await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let result = String(data: payload, encoding: .utf8)?.withCString { request -> String? in
                    guard
                        let response = coderight_mobile_runtime_dispatch_capabilities_with_resources(request)
                    else {
                        return nil
                    }
                    defer { coderight_mobile_string_free(response) }
                    return String(cString: response)
                }
                continuation.resume(returning: result)
            }
        }
        guard let raw, let data = raw.data(using: .utf8) else {
            throw Failure.malformed
        }
        let envelope = try JSONDecoder().decode(MobileCapabilityEnvelope.self, from: data)
        guard envelope.ok, let value = envelope.value else {
            throw Failure.unavailable(envelope.error ?? "The mobile capability catalog is unavailable.")
        }
        return value
    }

    static func probe(capability: String, sessionId: String, workspaceRoot: String) async throws
        -> String
    {
        struct Request: Encodable {
            let capability: String
            let sessionId: String
            let workspaceRoot: String
        }
        let payload = try JSONEncoder().encode(
            Request(
                capability: capability,
                sessionId: sessionId,
                workspaceRoot: workspaceRoot
            ))
        let raw: String? = await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let result: String?
                if let request = String(data: payload, encoding: .utf8) {
                    result = request.withCString { pointer -> String? in
                        guard let response = coderight_mobile_runtime_dispatch_capability(pointer) else {
                            return nil
                        }
                        defer { coderight_mobile_string_free(response) }
                        return String(cString: response)
                    }
                } else {
                    result = nil
                }
                continuation.resume(returning: result)
            }
        }
        guard let raw, let data = raw.data(using: .utf8) else {
            throw Failure.malformed
        }
        struct Envelope: Decodable {
            let ok: Bool
            let value: String?
            let error: String?
        }
        let envelope = try JSONDecoder().decode(Envelope.self, from: data)
        guard envelope.ok else {
            throw Failure.unavailable(envelope.error ?? "The capability check failed.")
        }
        return envelope.value ?? "Capability check completed without a result."
    }
}
