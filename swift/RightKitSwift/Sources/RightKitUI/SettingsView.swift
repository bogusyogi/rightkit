import RightKitLicense
import RightKitPurchases
import SwiftUI
import UniformTypeIdentifiers

struct SettingsView: View {
    @EnvironmentObject private var proResolver: ProEntitlementResolver
    @EnvironmentObject private var license: LicenseController
    @EnvironmentObject private var storeKit: PurchaseController
    @EnvironmentObject private var models: ModelInstallStore
    @EnvironmentObject private var asrSettings: ASREngineSettings
    @EnvironmentObject private var sync: ICloudSyncModel
    @EnvironmentObject private var send: SendCoordinator
    @State private var showFolderImporter = false
    @State private var destinationRefresh = 0
    @State private var folderError: String?
    @State private var showModelImporter = false
    @State private var licenseKeyInput = ""
    @State private var appleSyncConflictMessage = ""
    // The Pro-continuity opt-in lives in `AppleSyncedCredentialStore`'s own
    // defaults key — deliberately separate from the CloudKit `sync` switch
    // above (that one moves jobs; this one moves the Pro bridge token).
    @AppStorage(AppleSyncedCredentialStore.syncEnabledDefaultsKey) private var appleSyncEnabled = false

    private var destinationName: String {
        switch send.destination.status {
        case .receivedFolder, .unavailable: return "Received folder"
        case .chosen(let name): return name
        }
    }

    private var modelStatus: String {
        switch models.installedSource {
        case .shared: return "Installed — shared with HeardRight"
        case .local: return "Installed"
        case nil: return "Not installed"
        }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Picker("Engine", selection: $asrSettings.engine) {
                        ForEach(ASREngine.allCases) { engine in
                            Text(engine.displayName).tag(engine)
                        }
                    }
                    .pickerStyle(.segmented)

                    if asrSettings.engine == .parakeet && !models.isInstalled {
                        Label(
                            "Parakeet needs its model installed below, or transcription will fail.",
                            systemImage: "exclamationmark.triangle"
                        )
                        .font(.srBody(13))
                        .foregroundStyle(Color.srDanger)
                    }

                    // The Apple engine is iOS 26+. On older systems the
                    // default silently becomes "install Parakeet or nothing" —
                    // say so here instead of letting the first job fail.
                    if #unavailable(iOS 26.0), asrSettings.engine == .apple {
                        Label(
                            models.isInstalled
                                ? "Apple's engine needs iOS 26. Jobs will use the installed Parakeet model instead."
                                : "Apple's engine needs iOS 26. On this iPhone, install the Parakeet model below or transcription will fail.",
                            systemImage: "exclamationmark.triangle"
                        )
                        .font(.srBody(13))
                        .foregroundStyle(models.isInstalled ? Color.srMuted : Color.srDanger)
                    }

                    Text(engineBlurb)
                        .font(.srBody(13))
                        .foregroundStyle(Color.srMuted)
                } header: {
                    Text("Transcription engine")
                } footer: {
                    Text("Finished transcripts carry a badge showing which engine produced them.")
                        .font(.srBody(12))
                }

                Section {
                    LabeledContent("Saving to") {
                        Text(destinationName)
                            .font(.srMono(13))
                            .foregroundStyle(Color.srMuted)
                    }
                    if send.destination.status == .unavailable {
                        Text(ReceivedFileDestination.unavailableMessage)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srDanger)
                    }
                    if let folderError {
                        Text(folderError)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srDanger)
                    }
                    Button("Choose folder…") { showFolderImporter = true }
                    if send.destination.status != .receivedFolder {
                        Button("Use Received folder") {
                            send.destination.useReceivedFolder()
                            folderError = nil
                            destinationRefresh += 1
                        }
                    }
                } header: {
                    Text("Received files")
                } footer: {
                    Text("Files your computers send are saved here without asking. The Received folder is in Files under On My iPhone > ScrapeRight.")
                        .font(.srBody(12))
                }
                .id(destinationRefresh)
                .fileImporter(isPresented: $showFolderImporter, allowedContentTypes: [.folder]) { result in
                    switch result {
                    case .success(let url):
                        do {
                            try send.destination.choose(folder: url)
                            folderError = nil
                        } catch {
                            folderError = "That folder could not be used: \(error.localizedDescription)"
                        }
                    case .failure(let error):
                        folderError = error.localizedDescription
                    }
                    destinationRefresh += 1
                }

                Section("Parakeet model") {
                    LabeledContent("Status") {
                        Text(modelStatus)
                            .font(.srMono(13))
                            .foregroundStyle(models.isInstalled ? Color.srAccent : Color.srMuted)
                    }
                    Button("Import model archive…") { showModelImporter = true }
                    if let error = models.lastError {
                        Text(error)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srDanger)
                    }
                    Text(
                        """
                        Parakeet TDT v3 — the same locked model the desktop app runs, \
                        ~425 MB. If HeardRight already installed it on this iPhone, ScrapeRight \
                        reuses that copy and downloads nothing.
                        """
                    )
                        .font(.srBody(13))
                        .foregroundStyle(Color.srMuted)
                }

                Section {
                    Toggle(isOn: syncEnabledBinding) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("iCloud sync")
                            Text(syncStatusLine)
                                .font(.srBody(13))
                                .foregroundStyle(syncStatusColor)
                        }
                    }
                    if sync.isEnabled {
                        LabeledContent("Mac") {
                            Text(sync.macRegistered ? "Registered" : "None seen yet")
                                .font(.srMono(13))
                                .foregroundStyle(sync.macRegistered ? Color.srAccent : Color.srMuted)
                        }
                        if let lastSync = sync.lastSyncAt {
                            LabeledContent("Last sync") {
                                Text(lastSync, format: .relative(presentation: .named))
                                    .font(.srMono(13))
                                    .foregroundStyle(Color.srMuted)
                            }
                        }
                    }
                } header: {
                    Text("iCloud")
                } footer: {
                    Text(
                        "Jobs this iPhone can't run — hosted video, sign-in-only posts, media downloads — queue on your Mac over your private iCloud account and the transcript syncs back. Nothing leaves your iCloud. Also turn on iCloud sync in ScrapeRight on the Mac."
                    )
                    .font(.srBody(12))
                }

                Section {
                    LabeledContent("Status") {
                        Text(proStatusText)
                            .font(.srMono(13))
                            .foregroundStyle(proResolver.isPro ? Color.srAccent : Color.srMuted)
                    }

                    switch proResolver.state {
                    case .resolving:
                        HStack(spacing: 8) {
                            ProgressView()
                            Text("Checking Pro status…")
                                .font(.srBody(13))
                                .foregroundStyle(Color.srMuted)
                        }
                    case .pro(let source):
                        Text(source == .storeKit
                             ? "Active through your App Store purchase."
                             : "Active through your ScrapeRight license.")
                            .font(.srBody(13))
                            .foregroundStyle(Color.srMuted)
                        Button("Remove Pro from this device", role: .destructive) {
                            Task {
                                await license.remove()
                                await proResolver.resolve()
                            }
                        }
                    case .needsReconnect:
                        Text("Pro needs reconnecting on this iPhone. Enter your license key below or restore an App Store purchase.")
                            .font(.srBody(13))
                            .foregroundStyle(Color.srMuted)
                        licenseKeyEntry
                        Button("Restore purchase") {
                            Task {
                                await storeKit.restore()
                                await proResolver.resolve()
                            }
                        }
                    case .free(let failure):
                        Button {
                            Task {
                                await storeKit.purchase(productID: ProPurchases.productID)
                                await proResolver.resolve()
                            }
                        } label: {
                            if let product = storeKit.product(ProPurchases.productID) {
                                Text("Unlock Pro — \(product.displayPrice), one time")
                            } else {
                                Text("Unlock Pro")
                            }
                        }
                        Button("Restore purchase") {
                            Task {
                                await storeKit.restore()
                                await proResolver.resolve()
                            }
                        }
                        licenseKeyEntry
                        if let failure {
                            Text(proSyncMessage(failure))
                                .font(.srBody(13))
                                .foregroundStyle(Color.srMuted)
                        }
                    }

                    // The Pro continuity bridge travels in the synchronizable
                    // iCloud Keychain — an opaque token, never the raw key.
                    Toggle(isOn: appleSyncBinding) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Sync Pro across Apple devices")
                            Text("Uses your iCloud Keychain so a license key or App Store purchase applies on your Mac, iPad, and iPhone. Off, Pro stays on this device.")
                                .font(.srBody(13))
                                .foregroundStyle(Color.srMuted)
                        }
                    }
                    if !appleSyncConflictMessage.isEmpty {
                        Text(appleSyncConflictMessage)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srDanger)
                    }
                    if let error = storeKit.lastError {
                        Text(error)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srDanger)
                    } else if !storeKit.message.isEmpty {
                        Text(storeKit.message)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srMuted)
                    }
                    if !license.message.isEmpty {
                        Text(license.message)
                            .font(.srBody(13))
                            .foregroundStyle(Color.srMuted)
                    }
                } header: {
                    Text("Pro")
                }

                Section("About") {
                    LabeledContent("Version") {
                        Text(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "—")
                            .font(.srMono(13))
                    }
                    Link("scraperight.cc", destination: URL(string: "https://scraperight.cc")!)
                    Text("Local files are processed on this device. URL jobs contact the source you pasted. No upload box, no account for Free.")
                        .font(.srBody(13))
                        .foregroundStyle(Color.srMuted)
                }
            }
            .scrollContentBackground(.hidden)
            .listRowBackground(Color.srSurface)
            .background(Color.srBackground)
            .navigationTitle("Settings")
            .fileImporter(
                isPresented: $showModelImporter,
                allowedContentTypes: [.gzip, .item],
                allowsMultipleSelection: false
            ) { result in
                guard case .success(let urls) = result, let url = urls.first else { return }
                Task { await models.importArchive(at: url) }
            }
        }
    }

    private var proStatusText: String {
        switch proResolver.state {
        case .resolving: return "Checking…"
        case .pro: return "Pro"
        case .needsReconnect: return "Reconnect"
        case .free: return "Free"
        }
    }

    /// Desktop-key restore entry, shown whenever this iPhone isn't already
    /// Pro. Activation goes through the renewable device-lease endpoint
    /// (`LicenseController.activateViaLease`) — the same lease shape a
    /// bridge exchange produces.
    @ViewBuilder
    private var licenseKeyEntry: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Bought ScrapeRight Pro on a Mac or PC? Enter the license key from your purchase email.")
                .font(.srBody(13))
                .foregroundStyle(Color.srMuted)
            HStack {
                TextField("SR-XXXX-XXXX-XXXX", text: $licenseKeyInput)
                    .font(.srMono(13))
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                Button("Restore") {
                    let key = licenseKeyInput
                    Task {
                        await license.activateLease(LicenseEntitlement.formatLicenseKey(key))
                        if license.isMobilePro { licenseKeyInput = "" }
                        await proResolver.resolve()
                    }
                }
                .disabled(license.isBusy || licenseKeyInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }

    private var appleSyncBinding: Binding<Bool> {
        Binding(
            get: { appleSyncEnabled },
            set: { on in setAppleSyncEnabled(on) }
        )
    }

    private func setAppleSyncEnabled(_ on: Bool) {
        appleSyncEnabled = on
        let outcomes = AppleSyncedCredentialStore.shared.setSyncEnabled(on, accounts: [.proBridge])
        if on {
            // A device already holding a bridge token can claim Pro as soon
            // as sync is on — re-resolve so the token gets consumed now.
            Task { await proResolver.resolve() }
            let conflicts = outcomes.values.filter { $0 == .conflict }.count
            appleSyncConflictMessage = conflicts > 0
                ? "The synced Pro token differs between this device and iCloud — both were kept. Restore Pro again on the device whose copy should win."
                : ""
        } else {
            appleSyncConflictMessage = ""
        }
    }

    private func proSyncMessage(_ failure: ProBridgeFailure) -> String {
        switch failure {
        case .noBridgeAvailable:
            return "No Pro access received from iCloud yet. Buy Pro here or restore a license key — and turn on “Sync Pro across Apple devices” on both this iPhone and your other devices."
        case .sourceInactive(let detail):
            return detail.isEmpty
                ? "This Pro source is no longer active. If you bought Pro on a Mac or PC, enter the license key above."
                : "This Pro source is no longer active (\(detail))."
        case .seatLimitReached:
            return "Every device seat on this license is taken. Remove ScrapeRight Pro on one of your other devices, then try again."
        case .unreachable(let detail):
            return detail.isEmpty
                ? "Could not reach the license server. Existing Pro access on this iPhone is unchanged."
                : "Could not reach the license server (\(detail)). Existing Pro access is unchanged."
        case .rejected(let detail):
            return detail.isEmpty
                ? "The license server could not restore Pro on this iPhone."
                : detail
        }
    }

    private var syncEnabledBinding: Binding<Bool> {
        Binding(
            get: { sync.isEnabled },
            set: { value in Task { await sync.setEnabled(value) } }
        )
    }

    private var syncStatusLine: String {
        switch sync.state {
        case .off: return "Off — everything stays on this iPhone."
        case .syncing: return "Syncing…"
        case .ready: return "On — Mac jobs and the library sync over iCloud."
        case .unavailable(let detail): return detail
        }
    }

    private var syncStatusColor: Color {
        switch sync.state {
        case .unavailable: return .srDanger
        case .ready, .syncing: return .srAccent
        case .off: return .srMuted
        }
    }

    private var engineBlurb: String {
        switch asrSettings.engine {
        case .apple:
            return "Apple's on-device speech engine (iOS 26+). Nothing to download, nothing leaves the device."
        case .parakeet:
            return "The desktop's locked engine, running on this iPhone's Neural Engine. Needs its model installed."
        }
    }
}
