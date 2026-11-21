#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Combine
import Foundation

/// Where a model's setup is, for a settings row or an onboarding step. The raw values are neutral
/// default labels; a host that wants other words maps the case itself.
public enum ModelPhase: String, Sendable, Equatable {
    case missing = "Not downloaded"
    case downloading = "Downloading"
    case downloaded = "Downloaded"
    case installed = "Installed"
    case validating = "Validating"
    case ready = "Ready"
    case failed = "Failed"
}

/// One model the host offers. The host owns the catalogue (ids, routes, digests, names); this owner
/// owns the transaction that makes one of them ready.
public protocol ModelSetupModel: Hashable, Sendable {
    /// Stable id. It is the staged-download id, the background task description and the grant's `model`,
    /// so it must equal ``ModelArtifactSpec/id`` of ``spec``.
    var modelID: String { get }
    /// Shown by the host's copy; never interpreted here.
    var displayName: String { get }
    var spec: ModelArtifactSpec { get }
    /// Earlier installs this model replaces. They are deleted once this model is ready.
    var retiredSpecs: [ModelArtifactSpec] { get }
}

/// Every sentence ``ModelSetupController`` publishes in ``ModelSetupController/detail``. The defaults are
/// neutral English; a host passes its own wording. Closures taking a `String` receive the model's display name.
public struct ModelSetupCopy: Sendable {
    public var missing: @Sendable (String) -> String = { "\($0) will be stored in app data." }
    /// Name and whole percent (nil before the first size is known).
    public var downloading: @Sendable (String, Int?) -> String = { name, percent in
        percent.map { "Downloading \(name)... \($0)%" } ?? "Downloading \(name)..."
    }
    /// The attempt about to start and the maximum.
    public var retrying: @Sendable (Int, Int) -> String = { "Network interrupted. Retrying model download (\($0) of \($1))..." }
    public var installing = "Installing model archive..."
    public var installingDownloadedArchive = "Installing downloaded model archive..."
    public var installingImportedArchive = "Installing imported model archive..."
    public var validating = "Validating Core ML stages..."
    public var ready: @Sendable (String) -> String = { "\($0) is installed and Core ML stages loaded." }
    public var installedNeedsValidation: @Sendable (String) -> String = { "\($0) files are installed. Validate Core ML to finish setup." }
    public var archiveDownloaded: @Sendable (String) -> String = { "\($0) archive is downloaded. Install it to finish setup." }
    public var downloadCancelled = "Model download cancelled. You can retry when you are ready."
    public var setupCancelled = "Model setup cancelled. You can retry when you are ready."
    public var endpointNotConfigured: @Sendable (String) -> String = { "Model endpoint is not configured for \($0)." }
    public var checksumNotConfigured: @Sendable (String) -> String = { "Expected SHA-256 is not configured for \($0)." }
    public var archiveChecksumNotConfigured: @Sendable (String) -> String = {
        "\($0) archive is downloaded, but the expected SHA-256 is not configured."
    }
    public var removing = "Removing local model files..."

    public init() {}
}

/// Everything ``ModelSetupController`` needs from its host. No product name, endpoint, bundle id, model id
/// or sentence lives in the controller; they all arrive here.
public struct ModelSetupConfiguration<Model: ModelSetupModel>: Sendable {
    /// The models a user can pick, in display order.
    public var availableModels: [Model]
    /// Folder under Application Support that holds `models/` and `downloads/` (see ``ModelStoreLayout``).
    public var storageFolderName: String
    /// `User-Agent` of the grant call and the download.
    public var userAgent: String
    /// The background `URLSession` identifier. Stable across launches, unique per app.
    public var sessionIdentifier: String
    /// App Group whose container parks a download that finished while the app was not running; nil uses Application Support.
    public var stagingAppGroup: String?
    /// The stable per-install id sent with grant requests (see ``ModelGrantClient/stableDeviceID(prefix:vendorIdentifier:defaults:key:)``).
    public var deviceID: @Sendable () -> String
    /// True for every id the host ever shipped (including retired ones), so a stray background task cannot stage arbitrary files.
    public var isKnownModelID: @Sendable (String) -> Bool
    /// Reads the persisted selection, including any migration from a retired choice.
    public var loadSelectedModel: @Sendable (UserDefaults) -> Model
    public var saveSelectedModel: @Sendable (Model, UserDefaults) -> Void
    /// Build-time URL, digest and Worker base for one model.
    public var downloadConfig: @Sendable (Model) -> ModelDownloadConfig
    /// `UserDefaults` keys: the cellular opt-in, and the "setup was in progress" hint with its start time.
    public var allowCellularDefaultsKey: String
    public var pendingPreparationDefaultsKey: String
    public var pendingPreparationStartedAtDefaultsKey: String
    /// Chooses the authorization of a Worker-routed download per model; nil asks the Worker for a grant.
    public var authorizer: ModelDownloadAuthorizer?
    /// Loads and primes the model's runtime after it is ready. Errors are ignored by the controller.
    public var warm: @Sendable (Model, URL) async throws -> Void
    /// Drops any cached runtime for a model directory (called before its files change or go away).
    public var invalidate: @Sendable (URL) async -> Void
    /// Called each time a refresh finds the selected model installed and validated.
    public var onReady: @Sendable (ModelStoreLayout) -> Void
    /// Host event sink (`name`, fields). Fields carry only counters and static labels.
    public var event: @Sendable (String, [String: any Sendable]) -> Void
    /// Maps an error to the static reason label of a failed-download event; nil falls back to the built-in classes.
    public var failureReason: (@Sendable (Error) -> String?)?
    public var copy: ModelSetupCopy

    public init(
        availableModels: [Model],
        storageFolderName: String,
        userAgent: String,
        sessionIdentifier: String,
        stagingAppGroup: String?,
        deviceID: @escaping @Sendable () -> String,
        isKnownModelID: @escaping @Sendable (String) -> Bool,
        loadSelectedModel: @escaping @Sendable (UserDefaults) -> Model,
        saveSelectedModel: @escaping @Sendable (Model, UserDefaults) -> Void,
        downloadConfig: @escaping @Sendable (Model) -> ModelDownloadConfig,
        allowCellularDefaultsKey: String,
        pendingPreparationDefaultsKey: String,
        pendingPreparationStartedAtDefaultsKey: String,
        authorizer: ModelDownloadAuthorizer? = nil,
        warm: @escaping @Sendable (Model, URL) async throws -> Void = { _, _ in },
        invalidate: @escaping @Sendable (URL) async -> Void = { _ in },
        onReady: @escaping @Sendable (ModelStoreLayout) -> Void = { _ in },
        event: @escaping @Sendable (String, [String: any Sendable]) -> Void = { _, _ in },
        failureReason: (@Sendable (Error) -> String?)? = nil,
        copy: ModelSetupCopy = ModelSetupCopy()
    ) {
        self.availableModels = availableModels
        self.storageFolderName = storageFolderName
        self.userAgent = userAgent
        self.sessionIdentifier = sessionIdentifier
        self.stagingAppGroup = stagingAppGroup
        self.deviceID = deviceID
        self.isKnownModelID = isKnownModelID
        self.loadSelectedModel = loadSelectedModel
        self.saveSelectedModel = saveSelectedModel
        self.downloadConfig = downloadConfig
        self.allowCellularDefaultsKey = allowCellularDefaultsKey
        self.pendingPreparationDefaultsKey = pendingPreparationDefaultsKey
        self.pendingPreparationStartedAtDefaultsKey = pendingPreparationStartedAtDefaultsKey
        self.authorizer = authorizer
        self.warm = warm
        self.invalidate = invalidate
        self.onReady = onReady
        self.event = event
        self.failureReason = failureReason
        self.copy = copy
    }
}

/// Observable model setup for SwiftUI: which model is selected, whether it is missing, downloading,
/// installed, validated or failed, and the actions that move it along. The transaction itself (grant,
/// background download with retries, digest check, transactional install, Core ML validation) is
/// ``ModelDownloadCoordinator``; this class is the state machine and lifecycle around it (selection,
/// cancellation, a resumable "setup was interrupted" hint, adoption of a background download that finished
/// while the app was away, retired-model cleanup, import of a user-supplied archive, warm-up).
///
/// Keep one instance for the life of the app: it owns the one ``ModelDownloader``. Call
/// ``handleEventsForBackgroundURLSession(identifier:completionHandler:)`` from the app delegate.
@MainActor
public final class ModelSetupController<Model: ModelSetupModel>: ObservableObject {
    @Published public private(set) var phase: ModelPhase = .missing
    @Published public private(set) var progress = 0.0
    @Published public private(set) var detail: String
    @Published public private(set) var selectedModel: Model
    @Published public private(set) var allowsCellularModelDownloads: Bool
    @Published public private(set) var isWarmingSelectedModel = false
    /// True from launch until every install an earlier run or the earlier installer left half done is
    /// settled. While it is true the phase is `validating` and no readiness is reported.
    @Published public private(set) var isRecoveringInstalls = true

    public let availableModels: [Model]
    public let configuration: ModelSetupConfiguration<Model>
    public let layout: ModelStoreLayout
    public let downloader: ModelDownloader
    public let installer: ModelInstaller

    private let defaults: UserDefaults
    private let coordinator: ModelDownloadCoordinator
    private let cellular: AtomicFlag
    private var preparationGeneration = UUID()
    private var maxReportedProgress = 0.0
    private var activeExternalWarmObservers = 0
    private var isChangingModelFiles = false
    nonisolated(unsafe) private var setupTask: Task<Void, Never>?
    nonisolated(unsafe) private var warmTask: Task<Void, Never>?
    nonisolated(unsafe) private var recoveryTask: Task<Void, Never>?
    nonisolated(unsafe) private var cancellationTask: Task<Void, Never>?

    private final class AtomicFlag: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: Bool
        init(_ value: Bool) { stored = value }
        var value: Bool {
            get { lock.withLock { stored } }
            set { lock.withLock { stored = newValue } }
        }
    }

    /// - Parameters:
    ///   - defaults: where the selection, the cellular opt-in and the interrupted-setup hint persist.
    ///   - appSupportDirectory: the Application Support folder; nil uses the user's.
    public init(
        configuration: ModelSetupConfiguration<Model>,
        defaults: UserDefaults = .standard,
        appSupportDirectory: URL? = nil
    ) {
        self.configuration = configuration
        self.defaults = defaults
        availableModels = configuration.availableModels
        let appSupport = appSupportDirectory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let layout = ModelStoreLayout(root: appSupport.appendingPathComponent(configuration.storageFolderName, isDirectory: true))
        self.layout = layout
        let staging = ModelDownloadStaging(
            root: ModelDownloadStaging.defaultRoot(appGroupIdentifier: configuration.stagingAppGroup, fallback: layout.root))
        downloader = ModelDownloader(configuration: .init(
            sessionIdentifier: configuration.sessionIdentifier, staging: staging,
            workDirectory: FileManager.default.temporaryDirectory, isKnownModel: configuration.isKnownModelID))
        let allowsCellular = defaults.bool(forKey: configuration.allowCellularDefaultsKey)
        let flag = AtomicFlag(allowsCellular)
        cellular = flag
        let modelInstaller = ModelInstaller()
        installer = modelInstaller
        coordinator = ModelDownloadCoordinator(
            layout: layout, downloader: downloader, installer: modelInstaller,
            grantClient: ModelGrantClient(userAgent: configuration.userAgent),
            userAgent: configuration.userAgent, deviceID: configuration.deviceID,
            allowsCellular: { flag.value }, authorizer: configuration.authorizer)
        let resolved = configuration.loadSelectedModel(defaults)
        selectedModel = resolved
        allowsCellularModelDownloads = allowsCellular
        // Nothing is reported ready until recovery has settled every install (see `isRecoveringInstalls`).
        phase = .validating
        progress = 0.9
        detail = configuration.copy.validating
        downloader.onStaged = { [weak self] _ in
            Task { @MainActor [weak self] in await self?.finishCachedModelSetupIfNeeded() }
        }
        // Normal startup rejoins the background session, so a transfer the system kept running (or finished)
        // while the app was away is delivered to this process instead of waiting for a user action.
        downloader.reattach()
        // An activation that a crash or kill (or the earlier installer) left half done is resolved off the
        // main actor, legacy aside and pending marker first, then the phase is read and a running transfer
        // is rejoined.
        recoveryTask = Task { [weak self] in
            var failure: String?
            for model in configuration.availableModels {
                do { _ = try await modelInstaller.recoverInterruptedInstall(model.spec, layout: layout) }
                catch { if failure == nil { failure = error.localizedDescription } }
            }
            self?.finishRecovery(failure: failure)
        }
    }

    private func finishRecovery(failure: String?) {
        recoveryTask = nil
        guard failure == nil else {
            phase = .failed
            progress = 0
            detail = failure ?? configuration.copy.validating
            return
        }
        isRecoveringInstalls = false
        refresh()
        Task { [weak self] in await self?.rejoinRunningTransferIfAny() }
    }

    /// Suspends until launch recovery has settled, so no action reads or changes an unsettled install.
    private func awaitRecovery() async {
        await recoveryTask?.value
    }

    /// A download the system kept running for the selected model is picked up as a preparation, so the row
    /// shows its progress and the finished archive is installed in this process.
    private func rejoinRunningTransferIfAny() async {
        let model = selectedModel
        guard setupTask == nil, phase == .missing, !installIsCurrent(model),
              await downloader.hasActiveDownload(modelID: model.modelID) else { return }
        guard setupTask == nil, phase == .missing, model == selectedModel else { return }
        startPreparingModel()
    }

    deinit {
        setupTask?.cancel()
        warmTask?.cancel()
    }

    // MARK: Derived

    public var modelDirectory: URL { layout.modelDirectory(for: selectedModel.spec) }
    public var archiveURL: URL { layout.archiveURL(for: selectedModel.spec) }

    /// The selected model's spec with the build-time digest applied when there is one.
    private func effectiveSpec(_ model: Model) -> ModelArtifactSpec {
        configuration.downloadConfig(model).expectedSHA256.map(model.spec.withExpectedSHA256) ?? model.spec
    }

    /// True when the model's install on disk is complete AND is the archive the build pins: judged against
    /// the EFFECTIVE spec (build-time digest applied) and the digest recorded by the install's manifest, the
    /// current one or the earlier installer's (``ModelInstallProof``). A complete install of any other
    /// archive, or one whose origin cannot be proven, is not current and is replaced by a download.
    private func installIsCurrent(_ model: Model) -> Bool {
        let spec = effectiveSpec(model)
        return spec.isStructurallyInstalled(at: layout.modelDirectory(for: spec))
            && ModelInstallProof.matchesPin(spec, modelsRoot: layout.modelsRoot)
    }

    /// True when a setup was started and neither finished nor was cancelled, and none is running now.
    public var hasInterruptedPreparation: Bool {
        defaults.string(forKey: configuration.pendingPreparationDefaultsKey) != nil
            && setupTask == nil && phase != .downloading && phase != .validating
    }

    public func handleEventsForBackgroundURLSession(identifier: String, completionHandler: @escaping @Sendable () -> Void) {
        downloader.handleEventsForBackgroundURLSession(identifier: identifier, completionHandler: completionHandler)
    }

    // MARK: Settings

    public func setAllowsCellularModelDownloads(_ allowed: Bool) {
        guard phase != .downloading else { return }
        allowsCellularModelDownloads = allowed
        cellular.value = allowed
        defaults.set(allowed, forKey: configuration.allowCellularDefaultsKey)
    }

    public func selectModel(_ model: Model) {
        guard !isRecoveringInstalls, !isChangingModelFiles, phase != .downloading, phase != .validating else { return }
        cancelSelectedModelWarm()
        selectedModel = model
        configuration.saveSelectedModel(model, defaults)
        refresh()
    }

    // MARK: State from disk

    /// Hands a download that finished while the app was not running to the archive path. Nothing is
    /// activated here; the digest check and the transactional install run in ``prepareModel(waitForReady:)``.
    public func adoptStagedDownloadIfPresent() {
        guard !isRecoveringInstalls, !isChangingModelFiles, setupTask == nil, phase != .downloading, phase != .validating else { return }
        let spec = selectedModel.spec
        let staging = downloader.configuration.staging
        guard staging.stagedArchive(for: spec.id) != nil else { return }
        if installIsCurrent(selectedModel) || FileManager.default.fileExists(atPath: archiveURL.path) {
            staging.discard(model: spec.id)
            return
        }
        do {
            if try staging.adopt(model: spec.id, to: archiveURL) {
                configuration.event("model_background_download_adopted", ["model": spec.id])
            }
        } catch {
            configuration.event("model_background_download_adopt_failed", [
                "model": spec.id, "error": error.localizedDescription,
            ])
        }
    }

    public func refresh() {
        // Readiness is not exposed until launch recovery settled the installs (`finishRecovery` refreshes).
        guard !isRecoveringInstalls, !isChangingModelFiles else { return }
        adoptStagedDownloadIfPresent()
        let model = selectedModel
        let name = model.displayName
        let copy = configuration.copy
        let installed = installIsCurrent(model)
        if installed, CoreMLModelValidator.hasReadyMarker(at: modelDirectory) {
            removeRetainedArchiveIfReady()
            removeRetiredModelsIfReplaced()
            phase = .ready
            progress = 1
            detail = copy.ready(name)
            configuration.onReady(layout)
        } else if installed {
            phase = .installed
            progress = 1
            detail = copy.installedNeedsValidation(name)
        } else if FileManager.default.fileExists(atPath: archiveURL.path) {
            phase = .downloaded
            progress = 1
            detail = copy.archiveDownloaded(name)
        } else {
            phase = .missing
            progress = 0
            detail = copy.missing(name)
        }
    }

    /// Deletes retired installs (and their archives) once the current model is installed and validated.
    /// Best effort: a failure leaves the old files for the next ready refresh and never affects the ready model.
    private func removeRetiredModelsIfReplaced() {
        let model = selectedModel
        for retired in model.retiredSpecs {
            let directory = layout.modelDirectory(for: retired)
            guard FileManager.default.fileExists(atPath: directory.path)
                || FileManager.default.fileExists(atPath: layout.archiveURL(for: retired).path) else { continue }
            let installer = installer
            let layout = layout
            let configuration = configuration
            Task {
                await configuration.invalidate(directory)
                do {
                    try installer.remove(retired, layout: layout)
                    configuration.event("retired_model_removed", ["model": model.modelID, "retired": retired.directoryName])
                } catch {
                    configuration.event("retired_model_remove_failed", [
                        "model": model.modelID, "retired": retired.directoryName, "error": error.localizedDescription,
                    ])
                }
            }
        }
    }

    /// Drops the compressed archive once the expanded model is validated; keeping it would nearly double storage.
    private func removeRetainedArchiveIfReady() {
        guard FileManager.default.fileExists(atPath: archiveURL.path) else { return }
        do {
            try FileManager.default.removeItem(at: archiveURL)
            configuration.event("model_archive_removed", ["model": selectedModel.modelID])
        } catch {
            configuration.event("model_archive_cleanup_failed", [
                "model": selectedModel.modelID, "error": error.localizedDescription,
            ])
        }
    }

    // MARK: Warm-up

    public func beginExternalWarmObservation() {
        activeExternalWarmObservers += 1
        isWarmingSelectedModel = true
    }

    public func endExternalWarmObservation() {
        activeExternalWarmObservers = max(0, activeExternalWarmObservers - 1)
        if activeExternalWarmObservers == 0 && warmTask == nil { isWarmingSelectedModel = false }
    }

    /// Starts a prime of the selected model now. The host's runtime owns one shared prime, so repeated calls
    /// cannot duplicate a runtime.
    public func warmSelectedModelIfReady() {
        guard phase == .ready, warmTask == nil, !isWarmingSelectedModel else { return }
        let model = selectedModel
        let directory = modelDirectory
        let configuration = configuration
        let startedAt = CFAbsoluteTimeGetCurrent()
        isWarmingSelectedModel = true
        configuration.event("model_warm_started", ["model": model.modelID])
        warmTask = Task { [weak self] in
            guard !Task.isCancelled else { return }
            let result: Result<Void, Error>
            do {
                try await configuration.warm(model, directory)
                result = .success(())
            } catch {
                result = .failure(error)
            }
            // Cancellation or a model change already reset the published state.
            guard !Task.isCancelled else { return }
            let elapsedMS = Int((CFAbsoluteTimeGetCurrent() - startedAt) * 1000)
            switch result {
            case .success:
                configuration.event("model_warm_ready", ["model": model.modelID, "elapsed_ms": elapsedMS])
            case let .failure(error):
                configuration.event("model_warm_failed", [
                    "model": model.modelID, "elapsed_ms": elapsedMS, "error": error.localizedDescription,
                ])
            }
            await MainActor.run { [weak self] in
                self?.isWarmingSelectedModel = false
                self?.warmTask = nil
            }
        }
    }

    public func cancelSelectedModelWarm() {
        if warmTask != nil { configuration.event("model_warm_cancelled", ["model": selectedModel.modelID]) }
        warmTask?.cancel()
        warmTask = nil
        isWarmingSelectedModel = false
    }

    // MARK: Preparation

    private func markPreparationStarted() {
        defaults.set(selectedModel.modelID, forKey: configuration.pendingPreparationDefaultsKey)
        defaults.set(Date(), forKey: configuration.pendingPreparationStartedAtDefaultsKey)
    }

    private func clearPreparationMarker() {
        defaults.removeObject(forKey: configuration.pendingPreparationDefaultsKey)
        defaults.removeObject(forKey: configuration.pendingPreparationStartedAtDefaultsKey)
    }

    private func beginPreparation() -> UUID {
        let generation = UUID()
        preparationGeneration = generation
        return generation
    }

    private func completePreparation(_ generation: UUID) {
        guard preparationGeneration == generation else { return }
        setupTask = nil
        preparationGeneration = UUID()
        // The attempt has been observed by the owner; the filesystem decides the next phase on relaunch.
        clearPreparationMarker()
    }

    private func isCurrentPreparation(_ generation: UUID) -> Bool {
        preparationGeneration == generation && !Task.isCancelled
    }

    /// Makes the selected model ready: validates an installed one, otherwise installs a retained or staged
    /// archive, otherwise downloads it.
    public func prepareModel(waitForReady: Bool = true) async {
        guard setupTask == nil, !isChangingModelFiles else { return }
        let generation = beginPreparation()
        await prepareModel(waitForReady: waitForReady, generation: generation)
        completePreparation(generation)
    }

    private func prepareModel(waitForReady _: Bool, generation: UUID) async {
        await awaitRecovery()
        await cancellationTask?.value
        guard !isRecoveringInstalls, !isChangingModelFiles, isCurrentPreparation(generation) else { return }
        cancellationTask = nil
        isChangingModelFiles = true
        defer { isChangingModelFiles = false }
        if installIsCurrent(selectedModel) {
            await validateInstalledModel(generation: generation)
        } else {
            await downloadAndInstall(generation: generation)
        }
    }

    public func startPreparingModel(waitForReady: Bool = true) {
        guard setupTask == nil, !isChangingModelFiles else { return }
        let generation = beginPreparation()
        markPreparationStarted()
        setupTask = Task { [weak self] in
            guard let self else { return }
            await self.prepareModel(waitForReady: waitForReady, generation: generation)
            guard !Task.isCancelled else { return }
            await MainActor.run { self.completePreparation(generation) }
        }
    }

    public func cancelModelPreparation() {
        preparationGeneration = UUID()
        cancelSelectedModelWarm()
        setupTask?.cancel()
        setupTask = nil
        cancellationTask = Task { [downloader, selectedModel] in await downloader.cancelDownloads(modelID: selectedModel.modelID) }
        clearPreparationMarker()
        guard phase == .downloading || phase == .validating else { return }
        phase = .missing
        progress = 0
        detail = configuration.copy.setupCancelled
    }

    /// Finishes a setup that a background download or an earlier run left half done.
    public func finishCachedModelSetupIfNeeded() async {
        await awaitRecovery()
        guard phase != .downloading, phase != .validating else { return }
        refresh()
        guard phase == .installed || phase == .downloaded else { return }
        await prepareModel()
    }

    public func removeModel() async {
        await awaitRecovery()
        guard !isRecoveringInstalls, !isChangingModelFiles, phase != .downloading, phase != .validating else { return }
        isChangingModelFiles = true
        defer { isChangingModelFiles = false }
        cancelSelectedModelWarm()
        setupTask?.cancel()
        setupTask = nil
        clearPreparationMarker()
        phase = .missing
        progress = 0
        detail = configuration.copy.removing
        let model = selectedModel
        let installer = installer
        let layout = layout
        do {
            try await Task.detached { try installer.remove(model.spec, layout: layout) }.value
            await configuration.invalidate(layout.modelDirectory(for: model.spec))
            phase = .missing
            progress = 0
            detail = configuration.copy.missing(model.displayName)
        } catch {
            phase = .failed
            progress = 0
            detail = error.localizedDescription
        }
    }

    /// Installs a user-supplied archive for the selected model. The digest must match the model's pinned one.
    public func importModelArchive(from sourceURL: URL) async {
        await awaitRecovery()
        guard !isRecoveringInstalls, !isChangingModelFiles, phase != .downloading, phase != .validating else { return }
        isChangingModelFiles = true
        defer { isChangingModelFiles = false }
        let model = selectedModel
        let generation = beginPreparation()
        defer { completePreparation(generation) }
        phase = .validating
        progress = 0.7
        detail = configuration.copy.installingImportedArchive
        let hasSecurityScope = sourceURL.startAccessingSecurityScopedResource()
        defer { if hasSecurityScope { sourceURL.stopAccessingSecurityScopedResource() } }
        do {
            let spec = effectiveSpec(model)
            let directory = try await installer.install(archive: sourceURL, spec: spec, layout: layout)
            await configuration.invalidate(directory)
            try Task.checkCancellation()
            guard isCurrentPreparation(generation) else { throw CancellationError() }
            finishReady(model: model)
            try? await configuration.warm(model, directory)
        } catch {
            guard isCurrentPreparation(generation) else { return }
            phase = .failed
            progress = 0
            detail = error.localizedDescription
        }
    }

    public func reportModelImportFailure(_ error: Error) {
        phase = .failed
        progress = 0
        detail = error.localizedDescription
    }

    // MARK: Install and validate

    private func finishReady(model: Model) {
        phase = .ready
        progress = 1
        detail = configuration.copy.ready(model.displayName)
        configuration.onReady(layout)
        removeRetainedArchiveIfReady()
        removeRetiredModelsIfReplaced()
    }

    /// The model's files are on disk but not validated: load every Core ML stage and write the ready marker.
    private func validateInstalledModel(generation: UUID) async {
        let model = selectedModel
        let spec = model.spec
        let directory = modelDirectory
        phase = .validating
        progress = 0.9
        detail = configuration.copy.validating
        do {
            _ = try await Task.detached {
                try CoreMLModelValidator().validateInstalledArtifact(
                    backend: spec.backend, requiredFiles: spec.requiredFiles,
                    requiredStageNames: spec.requiredStageNames, at: directory)
            }.value
            try Task.checkCancellation()
            guard isCurrentPreparation(generation) else { throw CancellationError() }
            finishReady(model: model)
            try? await configuration.warm(model, directory)
        } catch let error where ModelDownloadPolicy.isCancellation(error) {
            guard preparationGeneration == generation else { return }
            phase = .missing
            progress = 0
            detail = configuration.copy.setupCancelled
        } catch {
            guard preparationGeneration == generation else { return }
            phase = .failed
            progress = 0
            detail = error.localizedDescription
        }
    }

    private func downloadAndInstall(generation: UUID) async {
        let model = selectedModel
        let name = model.displayName
        let copy = configuration.copy
        let spec = effectiveSpec(model)
        let downloadConfig = configuration.downloadConfig(model)
        let archive = layout.archiveURL(for: spec)
        let hasArchive = FileManager.default.fileExists(atPath: archive.path)
        let attempt = DownloadAttempt(
            model: model.modelID, networkPolicy: allowsCellularModelDownloads ? "cellular_allowed" : "wifi_only",
            event: configuration.event)

        if hasArchive {
            guard spec.expectedSHA256 != nil else {
                fail(copy.archiveChecksumNotConfigured(name))
                return
            }
            phase = .downloaded
            progress = 1
            detail = copy.installingDownloadedArchive
        } else {
            attempt.record("model_download_started")
            guard downloadConfig.signedURL != nil else {
                fail(copy.endpointNotConfigured(name))
                attempt.record("model_download_failed", reason: "missing_endpoint")
                return
            }
            guard spec.expectedSHA256 != nil else {
                fail(copy.checksumNotConfigured(name))
                attempt.record("model_download_failed", reason: "missing_checksum")
                return
            }
            phase = .downloading
            progress = 0
            detail = copy.downloading(name, nil)
        }
        maxReportedProgress = 0

        let report: @Sendable (ModelDownloadProgress) -> Void = { [weak self] update in
            Task { @MainActor [weak self] in
                self?.apply(update, generation: generation, name: name, hasArchive: hasArchive, archive: archive, attempt: attempt)
            }
        }
        var didCompleteDownload = hasArchive
        do {
            try Task.checkCancellation()
            guard isCurrentPreparation(generation) else { throw CancellationError() }
            let directory = try await coordinator.prepare(spec, config: downloadConfig, progress: report)
            didCompleteDownload = true
            try Task.checkCancellation()
            guard isCurrentPreparation(generation) else { throw CancellationError() }
            await configuration.invalidate(directory)
            try Task.checkCancellation()
            guard isCurrentPreparation(generation) else { throw CancellationError() }
            finishReady(model: model)
            try? await configuration.warm(model, directory)
        } catch let error where ModelDownloadPolicy.isCancellation(error) {
            guard preparationGeneration == generation else { return }
            if !didCompleteDownload { attempt.record("model_download_cancelled", reason: "cancelled") }
            let wasDownloading = phase == .downloading
            phase = .missing
            progress = 0
            detail = wasDownloading ? copy.downloadCancelled : copy.setupCancelled
        } catch {
            guard preparationGeneration == generation else { return }
            if !didCompleteDownload {
                let reason = configuration.failureReason?(error) ?? Self.failureReason(error)
                attempt.record("model_download_failed", reason: reason)
            }
            phase = .failed
            progress = 0
            detail = error.localizedDescription
        }
    }

    private func apply(
        _ update: ModelDownloadProgress, generation: UUID, name: String, hasArchive: Bool, archive: URL,
        attempt: DownloadAttempt
    ) {
        guard isCurrentPreparation(generation), phase != .ready, phase != .failed, phase != .missing else { return }
        let copy = configuration.copy
        switch update.phase {
        case .checking, .requestingGrant, .ready:
            break
        case .downloading:
            guard phase == .downloading else { return }
            // The transfer is 2%...90% of the job; show it as 0...100% of the download. Never backwards.
            let fraction = min(max((update.fraction - 0.02) / 0.88, 0), 1)
            guard fraction >= maxReportedProgress else { return }
            maxReportedProgress = fraction
            progress = fraction
            detail = copy.downloading(name, Int((fraction * 100).rounded()))
        case let .retrying(next):
            maxReportedProgress = 0
            progress = 0
            detail = copy.retrying(next, ModelDownloadPolicy.maxAttempts)
            attempt.record("model_download_retried", attempt: next)
            attempt.resetAttempt()
        case .usingStagedArchive:
            phase = .downloaded
            progress = 1
            detail = copy.installingDownloadedArchive
        case .installing:
            if phase == .downloading {
                attempt.observeFile(at: archive)
                attempt.record("model_download_completed")
            }
            progress = 1
            detail = hasArchive ? copy.installingDownloadedArchive : copy.installing
        case .validating:
            phase = .validating
            progress = 0.9
            detail = copy.validating
        }
    }

    private func fail(_ message: String) {
        phase = .failed
        progress = 0
        detail = message
    }

    private static func failureReason(_ error: Error) -> String {
        if case let ModelDownloadError.httpStatus(status) = error {
            return status >= 500 ? "http_5xx" : status >= 400 ? "http_4xx" : "http_error"
        }
        if error is ModelDownloadError { return "retry_exhausted" }
        let value = error as NSError
        guard value.domain == NSURLErrorDomain else { return "unknown" }
        switch value.code {
        case NSURLErrorTimedOut, NSURLErrorCannotFindHost, NSURLErrorCannotConnectToHost,
             NSURLErrorNetworkConnectionLost, NSURLErrorDNSLookupFailed, NSURLErrorNotConnectedToInternet:
            return "network"
        default:
            return "network_error"
        }
    }
}

/// Thread-safe, content-free state for one model download attempt sequence. It stores only bounded counters
/// and static labels: no URL, token, local path, model bytes or error text is ever sent.
private final class DownloadAttempt: @unchecked Sendable {
    private let model: String
    private let networkPolicy: String
    private let event: @Sendable (String, [String: any Sendable]) -> Void
    private let startedAt = CFAbsoluteTimeGetCurrent()
    private let lock = NSLock()
    private var bytes: Int64 = 0
    private var attempt = 1

    init(model: String, networkPolicy: String, event: @escaping @Sendable (String, [String: any Sendable]) -> Void) {
        self.model = model
        self.networkPolicy = networkPolicy
        self.event = event
    }

    func observeFile(at url: URL) {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let size = attributes[.size] as? NSNumber else { return }
        lock.withLock { bytes = max(0, size.int64Value) }
    }

    func resetAttempt() {
        lock.withLock {
            bytes = 0
            attempt += 1
        }
    }

    func record(_ name: String, attempt: Int? = nil, reason: String? = nil) {
        let (currentBytes, currentAttempt) = lock.withLock { (bytes, max(1, attempt ?? self.attempt)) }
        let elapsedMS = max(0, Int((CFAbsoluteTimeGetCurrent() - startedAt) * 1000))
        var fields: [String: any Sendable] = [
            "model": model,
            "network_policy": networkPolicy,
            "bytes": currentBytes,
            "elapsed_ms": elapsedMS,
            "attempt": currentAttempt,
        ]
        if let reason { fields["reason"] = reason }
        event(name, fields)
    }
}
#endif
