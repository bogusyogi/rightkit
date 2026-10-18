import Foundation
import RightKitArtifacts
import Testing

@testable import RightKitSpeech

// MARK: Helpers

private final class Box<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var _value: Value
    init(_ value: Value) { _value = value }
    var value: Value {
        get { lock.withLock { _value } }
        set { lock.withLock { _value = newValue } }
    }
    func mutate(_ body: (inout Value) -> Void) { lock.withLock { body(&_value) } }
}

private func scratch() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-models-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}

private func http(_ status: Int, headers: [String: String] = [:]) -> HTTPURLResponse {
    HTTPURLResponse(url: URL(string: "https://worker.example/v1/model-grants")!, statusCode: status, httpVersion: nil, headerFields: headers)!
}

/// A `.tar.gz` made with the system tar holding `model/model.bin` (= `weights`) and `model/Enc.mlmodelc/w`.
private func makeArchive(in root: URL, weights: String, topLevel: String = "model") throws -> URL {
    let stage = root.appendingPathComponent("stage-\(UUID().uuidString)")
    let model = stage.appendingPathComponent(topLevel)
    try FileManager.default.createDirectory(at: model.appendingPathComponent("Enc.mlmodelc"), withIntermediateDirectories: true)
    try weights.write(to: model.appendingPathComponent("model.bin"), atomically: true, encoding: .utf8)
    try "w".write(to: model.appendingPathComponent("Enc.mlmodelc/w"), atomically: true, encoding: .utf8)
    let archive = root.appendingPathComponent("archive-\(UUID().uuidString).tar.gz")
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/tar")
    process.arguments = ["-czf", archive.path, "-C", stage.path, topLevel]
    try process.run()
    process.waitUntilExit()
    #expect(process.terminationStatus == 0)
    return archive
}

private func sha(_ url: URL) throws -> String { try ArtifactInstaller.sha256(of: url) }

private func makeSpec(sha: String?) -> ModelArtifactSpec {
    ModelArtifactSpec(
        id: "test-model", backend: "test", platform: "ios", route: "/test/model.tar.gz", directoryName: "model",
        expectedSHA256: sha, requiredFiles: ["model.bin"], requiredStageNames: ["Enc"])
}

/// Stands in for loading Core ML: records the ready marker the real validator writes.
private func fakeInstaller(failWhenContains failing: String? = nil) -> ModelInstaller {
    ModelInstaller(validate: { _, directory in
        if let failing, (try? String(contentsOf: directory.appendingPathComponent("model.bin"), encoding: .utf8)) == failing {
            throw SpeechError.invalidContract("injected validation failure")
        }
        try Data("{}".utf8).write(to: directory.appendingPathComponent(CoreMLModelValidator.readyMarkerName))
    })
}

private struct FakeTransport: ModelHTTPTransport {
    let handler: @Sendable (URLRequest) throws -> (Data, HTTPURLResponse)
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) { try handler(request) }
}

private func grantJSON(token: String = "tok", expiresAt: UInt64, route: String = "/test/model.tar.gz", sha: String) -> Data {
    Data(#"{"token":"\#(token)","expiresAt":\#(expiresAt),"route":"\#(route)","sha256":"\#(sha)"}"#.utf8)
}

private let fixedNow = Date(timeIntervalSince1970: 1_800_000_000)

// MARK: Grant client

struct ModelGrantClientTests {
    private let request = ModelGrantClient.GrantRequest(
        workerBase: "https://worker.example/", route: "/test/model.tar.gz", model: "test-model",
        platform: "ios", sha256: "ABCDEF", deviceID: "ios-0123456789abcdef")

    private func client(_ handler: @escaping @Sendable (URLRequest) throws -> (Data, HTTPURLResponse)) -> ModelGrantClient {
        ModelGrantClient(transport: FakeTransport(handler: handler), now: { fixedNow })
    }

    @Test func postsTheExactScopeAndReturnsTheGrant() async throws {
        let seen = Box<URLRequest?>(nil)
        let grant = try await client { urlRequest in
            seen.value = urlRequest
            return (grantJSON(expiresAt: 1_800_000_300, sha: "abcdef"), http(200))
        }.acquireGrant(request)
        #expect(grant == ModelGrant(token: "tok", expiresAt: 1_800_000_300))
        #expect(grant.bearer == "Bearer tok")
        let sent = try #require(seen.value)
        #expect(sent.url?.absoluteString == "https://worker.example/v1/model-grants")
        #expect(sent.httpMethod == "POST")
        #expect(sent.value(forHTTPHeaderField: "content-type") == "application/json")
        let body = try #require(sent.httpBody)
        let json = try #require(try JSONSerialization.jsonObject(with: body) as? [String: String])
        #expect(json == ["route": "/test/model.tar.gz", "model": "test-model", "platform": "ios", "sha256": "ABCDEF", "deviceId": "ios-0123456789abcdef"])
    }

    @Test func aGrantForAnotherRouteOrDigestIsRefused() async {
        let wrongRoute = client { _ in (grantJSON(expiresAt: 1_800_000_300, route: "/other", sha: "abcdef"), http(200)) }
        await #expect(throws: ModelGrantClient.GrantError.scopeMismatch) { try await wrongRoute.acquireGrant(request) }
        let wrongDigest = client { _ in (grantJSON(expiresAt: 1_800_000_300, sha: "000000"), http(200)) }
        await #expect(throws: ModelGrantClient.GrantError.scopeMismatch) { try await wrongDigest.acquireGrant(request) }
    }

    @Test func anExpiredGrantIsRefusedButClockSkewIsTolerated() async throws {
        let expired = client { _ in (grantJSON(expiresAt: 1_800_000_000 - 31, sha: "abcdef"), http(200)) }
        await #expect(throws: ModelGrantClient.GrantError.grantExpired) { try await expired.acquireGrant(request) }
        let skewed = client { _ in (grantJSON(expiresAt: 1_800_000_000 - 10, sha: "abcdef"), http(200)) }
        _ = try await skewed.acquireGrant(request)
    }

    @Test func rateLimitingKeepsRetryAfterAndIsNeverReplayed() async {
        let calls = Box(0)
        let limited = client { _ in
            calls.mutate { $0 += 1 }
            return (Data(), http(429, headers: ["Retry-After": "7"]))
        }
        await #expect(throws: ModelGrantClient.GrantError.rateLimited(retryAfter: 7)) { try await limited.acquireGrant(request) }
        #expect(calls.value == 1)
    }

    @Test func otherStatusesAndBadBodiesAreTyped() async {
        let server = client { _ in (Data(), http(503)) }
        await #expect(throws: ModelGrantClient.GrantError.httpStatus(503)) { try await server.acquireGrant(request) }
        let garbage = client { _ in (Data("nope".utf8), http(200)) }
        await #expect(throws: ModelGrantClient.GrantError.malformedResponse) { try await garbage.acquireGrant(request) }
        let empty = client { _ in (grantJSON(token: " ", expiresAt: 1_800_000_300, sha: "abcdef"), http(200)) }
        await #expect(throws: ModelGrantClient.GrantError.malformedResponse) { try await empty.acquireGrant(request) }
    }

    @Test func aTransportFailureIsReportedAndACancellationStaysACancellation() async {
        let offline = client { _ in throw URLError(.notConnectedToInternet) }
        await #expect(throws: (any Error).self) { try await offline.acquireGrant(request) }
        let cancelled = client { _ in throw URLError(.cancelled) }
        await #expect(throws: CancellationError.self) { try await cancelled.acquireGrant(request) }
    }

    @Test func onlyAnHTTPSHostWithoutCredentialsIsAcceptedAndNothingIsSentOtherwise() async {
        let sent = Box(0)
        let spy = client { _ in sent.mutate { $0 += 1 }; return (Data(), http(200)) }
        for base in ["http://worker.example", "https://user:pw@worker.example", "https://", "ftp://worker.example", ""] {
            let bad = ModelGrantClient.GrantRequest(
                workerBase: base, route: request.route, model: request.model, platform: request.platform,
                sha256: request.sha256, deviceID: request.deviceID)
            await #expect(throws: ModelGrantClient.GrantError.badWorkerBase) { try await spy.acquireGrant(bad) }
        }
        #expect(sent.value == 0)
        #expect(ModelGrantClient.endpoint(workerBase: " https://worker.example// ")?.absoluteString == "https://worker.example/v1/model-grants")
    }

    @Test func retryAfterParsesSecondsAndHTTPDates() {
        let now = Date(timeIntervalSince1970: 784_111_000)
        #expect(ModelGrantClient.retryAfter("120", now: now) == 120)
        #expect(ModelGrantClient.retryAfter("Sun, 06 Nov 1994 08:49:37 GMT", now: now) == 777)
        #expect(ModelGrantClient.retryAfter("soon", now: now) == nil)
        #expect(ModelGrantClient.retryAfter(nil, now: now) == nil)
    }

    @Test func deviceIDsFollowTheWorkerContractAndStayStable() throws {
        #expect(ModelGrantClient.isValidDeviceID("ios-0123456789abcdef"))
        #expect(!ModelGrantClient.isValidDeviceID("short"))
        #expect(!ModelGrantClient.isValidDeviceID("-leading-dash-0123456789"))
        #expect(!ModelGrantClient.isValidDeviceID("has space in it 0123456789"))
        #expect(!ModelGrantClient.isValidDeviceID(String(repeating: "a", count: 129)))

        let suite = "rightkit.tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let first = ModelGrantClient.stableDeviceID(defaults: defaults)
        #expect(ModelGrantClient.isValidDeviceID(first))
        #expect(ModelGrantClient.stableDeviceID(defaults: defaults) == first)
        let vendor = UUID()
        #expect(ModelGrantClient.stableDeviceID(vendorIdentifier: vendor, defaults: defaults) == "ios-" + vendor.uuidString.replacingOccurrences(of: "-", with: ""))
    }
}

// MARK: Pure rules

struct ModelDownloadRuleTests {
    @Test func configResolvesBuildValuesAndRoutesThroughTheWorker() {
        let routed = ModelDownloadConfig.fromValues(
            urlString: "$(HR_URL)", shaString: " abc ", workerBaseString: "https://w.example/", route: "/r/model.tar.gz")
        #expect(routed.signedURL?.absoluteString == "https://w.example/r/model.tar.gz")
        #expect(routed.workerBase == "https://w.example/")
        #expect(routed.expectedSHA256 == "abc")

        let explicit = ModelDownloadConfig.fromValues(
            urlString: "https://cdn.example/m.tar.gz", shaString: "abc", workerBaseString: "https://w.example", route: "/r")
        #expect(explicit.signedURL?.absoluteString == "https://cdn.example/m.tar.gz")
        #expect(explicit.workerBase == nil, "an explicit signed URL needs no grant")

        let unset = ModelDownloadConfig.fromValues(urlString: "https:", shaString: "", workerBaseString: nil, route: "/r")
        #expect(unset.signedURL == nil && unset.expectedSHA256 == nil)
    }

    @Test func retriesCoverTransientFailuresOnly() {
        #expect(ModelDownloadPolicy.shouldRetry(URLError(.timedOut), attempt: 1))
        #expect(ModelDownloadPolicy.shouldRetry(URLError(.networkConnectionLost), attempt: 2))
        #expect(!ModelDownloadPolicy.shouldRetry(URLError(.timedOut), attempt: 3), "the last attempt is final")
        #expect(!ModelDownloadPolicy.shouldRetry(URLError(.cancelled), attempt: 1))
        #expect(!ModelDownloadPolicy.shouldRetry(CancellationError(), attempt: 1))
        #expect(!ModelDownloadPolicy.shouldRetry(URLError(.badURL), attempt: 1))
        #expect(ModelDownloadPolicy.shouldRetry(ModelDownloadError.httpStatus(503), attempt: 1))
        #expect(ModelDownloadPolicy.shouldRetry(ModelDownloadError.httpStatus(429), attempt: 1))
        #expect(ModelDownloadPolicy.shouldRetry(ModelDownloadError.httpStatus(408), attempt: 1))
        #expect(!ModelDownloadPolicy.shouldRetry(ModelDownloadError.httpStatus(404), attempt: 1))
        #expect(!ModelDownloadPolicy.shouldRetry(ModelDownloadError.httpStatus(403), attempt: 1))
    }

    @Test func theRequestCarriesTheBearerAndTheNetworkRules() {
        let url = URL(string: "https://w.example/m.tar.gz")!
        let request = ModelDownloadPolicy.request(url: url, bearerToken: "tok", userAgent: "App/1", allowsCellular: false)
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
        #expect(request.value(forHTTPHeaderField: "User-Agent") == "App/1")
        #expect(request.timeoutInterval == 60)
        #expect(!request.allowsCellularAccess && !request.allowsExpensiveNetworkAccess && !request.allowsConstrainedNetworkAccess)
        let open = ModelDownloadPolicy.request(url: url, bearerToken: nil, userAgent: "App/1", allowsCellular: true)
        #expect(open.value(forHTTPHeaderField: "Authorization") == nil)
        #expect(open.allowsCellularAccess && open.allowsExpensiveNetworkAccess)
    }

    @Test func progressNeverMovesBackwardsAndSurvivesAnUnknownSize() {
        var tracker = ModelDownloadProgressTracker()
        #expect(tracker.update(downloadedBytes: 10, expectedBytes: -1) == 0)
        #expect(tracker.update(downloadedBytes: 250, expectedBytes: 1_000) == 0.25)
        #expect(tracker.update(downloadedBytes: 500, expectedBytes: -1) == 0.5, "the last known size is reused")
        #expect(tracker.update(downloadedBytes: 100, expectedBytes: 1_000) == 0.5, "high-water mark")
        #expect(tracker.update(downloadedBytes: 5_000, expectedBytes: 1_000) == 1)
        tracker.reset()
        #expect(tracker.update(downloadedBytes: 100, expectedBytes: 1_000) == 0.1)
    }

    @Test func structuralInstallChecksFilesAndStages() throws {
        let dir = try scratch()
        defer { try? FileManager.default.removeItem(at: dir) }
        let s = makeSpec(sha: "x")
        #expect(!s.isStructurallyInstalled(at: dir))
        try "w".write(to: dir.appendingPathComponent("model.bin"), atomically: true, encoding: .utf8)
        #expect(!s.isStructurallyInstalled(at: dir), "the stage is still missing")
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("Enc.mlpackage"), withIntermediateDirectories: true)
        #expect(s.isStructurallyInstalled(at: dir), "either stage form counts")
        #expect(s.withExpectedSHA256("y").requiredStageNames == ["Enc"], "re-pinning keeps the artifact's own contract")
    }
}

// MARK: Staging and downloader

struct ModelDownloadStagingTests {
    @Test func aFinishedDownloadIsStagedWithAMarkerAndAdoptedOnce() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let staging = ModelDownloadStaging(root: root.appendingPathComponent("staging"))
        let location = root.appendingPathComponent("tmp.download")
        try Data(repeating: 7, count: 100).write(to: location)

        #expect(staging.stageCompleted(location: location, model: "m", response: http(200)))
        #expect(!FileManager.default.fileExists(atPath: location.path), "the file was moved, not copied")
        #expect(staging.stagedArchive(for: "m") != nil)
        #expect(staging.stagedArchive(for: "other") == nil)

        let destination = root.appendingPathComponent("downloads/m.tar.gz")
        #expect(try staging.adopt(model: "m", to: destination))
        #expect(try Data(contentsOf: destination).count == 100)
        #expect(staging.stagedArchive(for: "m") == nil)
        #expect(try !staging.adopt(model: "m", to: destination), "a second adoption finds nothing")
    }

    @Test func aFailedResponseOrATamperedArchiveStagesNothing() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let staging = ModelDownloadStaging(root: root.appendingPathComponent("staging"))
        let location = root.appendingPathComponent("tmp.download")
        try Data(repeating: 1, count: 10).write(to: location)
        #expect(!staging.stageCompleted(location: location, model: "m", response: http(404)))
        #expect(!staging.stageCompleted(location: location, model: "m", response: nil))
        #expect(staging.stagedArchive(for: "m") == nil)

        #expect(staging.stageCompleted(location: location, model: "m", response: http(200)))
        let archive = try #require(staging.stagedArchive(for: "m"))
        try Data(repeating: 1, count: 3).write(to: archive)  // size no longer matches its marker
        #expect(staging.stagedArchive(for: "m") == nil)
        staging.discard(model: "m")
    }
}

struct ModelDownloaderTests {
    private func makeDownloader(_ root: URL, known: Set<String> = ["m"]) -> ModelDownloader {
        ModelDownloader(configuration: .init(
            sessionIdentifier: "rightkit.tests.downloader", staging: ModelDownloadStaging(root: root.appendingPathComponent("staging")),
            workDirectory: root.appendingPathComponent("work"), isKnownModel: { known.contains($0) }))
    }

    @Test func aFinishedDownloadReachesItsWaiterAsARetainedFile() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let downloader = makeDownloader(root)
        let location = root.appendingPathComponent("system.tmp")
        try Data(repeating: 9, count: 64).write(to: location)
        let progress = Box<[Int64]>([])

        let file: URL = try await withCheckedThrowingContinuation { continuation in
            let accepted = downloader.register(taskIdentifier: 5, onProgress: { written, _ in progress.mutate { $0.append(written) } }, continuation: continuation)
            #expect(accepted)
            downloader.reportProgress(taskIdentifier: 5, written: 32, expected: 64)
            downloader.handleFinished(taskIdentifier: 5, modelID: "m", response: http(200), location: location)
        }
        #expect(try Data(contentsOf: file).count == 64)
        #expect(file.deletingLastPathComponent().lastPathComponent == "work")
        #expect(progress.value == [32])
        #expect(downloader.configuration.staging.stagedArchive(for: "m") == nil, "a waited download is not staged")
    }

    @Test func aNon2xxAnswerFailsTheWaiterAndKeepsNothing() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let downloader = makeDownloader(root)
        let location = root.appendingPathComponent("system.tmp")
        try Data("<html>forbidden</html>".utf8).write(to: location)
        await #expect(throws: ModelDownloadError.httpStatus(403)) {
            _ = try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                downloader.register(taskIdentifier: 6, onProgress: { _, _ in }, continuation: continuation)
                downloader.handleFinished(taskIdentifier: 6, modelID: "m", response: http(403), location: location)
            }
        }
        #expect(!FileManager.default.fileExists(atPath: root.appendingPathComponent("work").path)
            || ((try? FileManager.default.contentsOfDirectory(atPath: root.appendingPathComponent("work").path)) ?? []).isEmpty)
    }

    @Test func aWaiterResumesExactlyOnceEvenIfFinishAndFailureRace() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let downloader = makeDownloader(root)
        let location = root.appendingPathComponent("system.tmp")
        try Data(repeating: 1, count: 8).write(to: location)
        let file = try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
            downloader.register(taskIdentifier: 7, onProgress: { _, _ in }, continuation: continuation)
            downloader.handleFinished(taskIdentifier: 7, modelID: "m", response: http(200), location: location)
            downloader.fail(taskIdentifier: 7, error: URLError(.cancelled))  // the late didCompleteWithError
        }
        #expect(FileManager.default.fileExists(atPath: file.path))
    }

    @Test func aSecondWaiterForTheSameTransferIsRefused() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let downloader = makeDownloader(root)
        let registrations = Box<[Bool]>([])
        let first = Task { () -> URL in
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                let accepted = downloader.register(taskIdentifier: 8, onProgress: { _, _ in }, continuation: continuation)
                registrations.mutate { $0.append(accepted) }
            }
        }
        while registrations.value.isEmpty { await Task.yield() }
        let second = Task { () -> URL in
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                let accepted = downloader.register(taskIdentifier: 8, onProgress: { _, _ in }, continuation: continuation)
                registrations.mutate { $0.append(accepted) }
                if !accepted { continuation.resume(throwing: ModelDownloadError.alreadyInProgress) }
            }
        }
        await #expect(throws: ModelDownloadError.alreadyInProgress) { try await second.value }
        #expect(registrations.value == [true, false])
        downloader.fail(taskIdentifier: 8, error: URLError(.cancelled))
        await #expect(throws: URLError.self) { try await first.value }
    }

    @Test func aDownloadThatFinishesWithNoWaiterIsStagedAndAnnounced() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let downloader = makeDownloader(root)
        let announced = Box<[String]>([])
        downloader.onStaged = { model in announced.mutate { $0.append(model) } }
        let location = root.appendingPathComponent("system.tmp")
        try Data(repeating: 3, count: 40).write(to: location)

        downloader.handleFinished(taskIdentifier: 99, modelID: "m", response: http(200), location: location)
        #expect(announced.value == ["m"])
        #expect(downloader.configuration.staging.stagedArchive(for: "m") != nil)

        let stray = root.appendingPathComponent("stray.tmp")
        try Data(repeating: 3, count: 40).write(to: stray)
        downloader.handleFinished(taskIdentifier: 100, modelID: "unknown", response: http(200), location: stray)
        downloader.handleFinished(taskIdentifier: 101, modelID: nil, response: http(200), location: stray)
        #expect(announced.value == ["m"], "an unknown model id is never staged")
    }

    @Test func aForeignSessionIdentifierIsAnsweredAtOnce() {
        let downloader = makeDownloader(FileManager.default.temporaryDirectory)
        let called = Box(false)
        downloader.handleEventsForBackgroundURLSession(identifier: "someone.else") { called.value = true }
        #expect(called.value)
    }
}

// MARK: Installer and coordinator

struct ModelInstallerTests {
    @Test func installsAVerifiedArchiveValidatesItAndLeavesNoAside() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
        let archive = try makeArchive(in: root, weights: "v1")
        let s = makeSpec(sha: try sha(archive))
        let validating = Box(0)

        let directory = try await fakeInstaller().install(archive: archive, spec: s, layout: layout, onValidating: { validating.mutate { $0 += 1 } })
        #expect(directory.lastPathComponent == "model")
        #expect(try String(contentsOf: directory.appendingPathComponent("model.bin"), encoding: .utf8) == "v1")
        #expect(validating.value == 1)
        #expect(await fakeInstaller().isReady(s, layout: layout))
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: layout.modelsRoot.path).filter { $0.hasPrefix(".aside") }
        #expect(leftovers.isEmpty, "no previous copy survives a settled install")
    }

    @Test func aMissingDigestRefusesBeforeTouchingTheDisk() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
        let archive = try makeArchive(in: root, weights: "v1")
        await #expect(throws: ModelDownloadError.missingExpectedDigest("model")) {
            try await fakeInstaller().install(archive: archive, spec: makeSpec(sha: nil), layout: layout)
        }
        #expect(!FileManager.default.fileExists(atPath: layout.modelsRoot.path))
    }

    @Test func aWrongArchiveIsRejectedAndThePreviousModelStays() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
        let v1 = try makeArchive(in: root, weights: "v1")
        let s = makeSpec(sha: try sha(v1))
        try await fakeInstaller().install(archive: v1, spec: s, layout: layout)

        let other = try makeArchive(in: root, weights: "tampered")
        await #expect(throws: ModelDownloadError.self) { try await fakeInstaller().install(archive: other, spec: s, layout: layout) }
        #expect(try String(contentsOf: layout.modelDirectory(for: s).appendingPathComponent("model.bin"), encoding: .utf8) == "v1")
    }

    @Test func aReplacementThatFailsValidationRestoresTheNewestWorkingModel() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
        let v1 = try makeArchive(in: root, weights: "v1")
        try await fakeInstaller().install(archive: v1, spec: makeSpec(sha: try sha(v1)), layout: layout)

        let v2 = try makeArchive(in: root, weights: "v2-broken")
        let s2 = makeSpec(sha: try sha(v2))
        await #expect(throws: ArtifactError.self) {
            try await fakeInstaller(failWhenContains: "v2-broken").install(archive: v2, spec: s2, layout: layout)
        }
        let directory = layout.modelDirectory(for: s2)
        #expect(try String(contentsOf: directory.appendingPathComponent("model.bin"), encoding: .utf8) == "v1", "the working model is back")
        #expect(CoreMLModelValidator.hasReadyMarker(at: directory))
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: layout.modelsRoot.path).filter { $0.hasPrefix(".aside") }
        #expect(leftovers.isEmpty)
    }

    @Test func aStructurallyIncompleteArchiveIsRejectedBeforeActivation() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
        let archive = try makeArchive(in: root, weights: "v1")
        var incomplete = makeSpec(sha: try sha(archive))
        incomplete = ModelArtifactSpec(
            id: incomplete.id, backend: incomplete.backend, platform: incomplete.platform, route: incomplete.route,
            directoryName: incomplete.directoryName, expectedSHA256: incomplete.expectedSHA256,
            requiredFiles: ["model.bin", "vocab.txt"], requiredStageNames: ["Enc"])
        await #expect(throws: ArtifactError.self) { try await fakeInstaller().install(archive: archive, spec: incomplete, layout: layout) }
        #expect(!FileManager.default.fileExists(atPath: layout.modelDirectory(for: incomplete).path))
    }

    @Test func removeDeletesTheModelAndTheRetainedArchive() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
        let archive = try makeArchive(in: root, weights: "v1")
        let s = makeSpec(sha: try sha(archive))
        try await fakeInstaller().install(archive: archive, spec: s, layout: layout)
        try Data("x".utf8).write(to: layout.archiveURL(for: s))
        try fakeInstaller().remove(s, layout: layout)
        #expect(!FileManager.default.fileExists(atPath: layout.modelDirectory(for: s).path))
        #expect(!FileManager.default.fileExists(atPath: layout.archiveURL(for: s).path))
        let stillReady = await fakeInstaller().isReady(s, layout: layout)
        #expect(!stillReady)
    }
}

struct ModelDownloadCoordinatorTests {
    private final class Rig: @unchecked Sendable {
        let root: URL
        let layout: ModelStoreLayout
        let staging: ModelDownloadStaging
        let archive: URL
        let digest: String
        let fetches = Box<[URLRequest]>([])
        let grants = Box<[URLRequest]>([])
        let sleeps = Box<[TimeInterval]>([])
        let failures = Box<[Error]>([])  // errors to throw from fetch, front first
        let hang = Box(false)  // fetch waits until its task is cancelled
        let phases = Box<[ModelDownloadProgress.Phase]>([])

        init(weights: String = "v1") throws {
            root = try scratch()
            layout = ModelStoreLayout(root: root.appendingPathComponent("store"))
            staging = ModelDownloadStaging(root: root.appendingPathComponent("staging"))
            archive = try makeArchive(in: root, weights: weights)
            digest = try sha(archive)
        }

        deinit { try? FileManager.default.removeItem(at: root) }

        var spec: ModelArtifactSpec { makeSpec(sha: digest) }

        func coordinator(installer: ModelInstaller = fakeInstaller(), serving: URL? = nil) -> ModelDownloadCoordinator {
            let digest = self.digest
            let served = serving ?? archive
            let grants = self.grants, fetches = self.fetches, failures = self.failures, sleeps = self.sleeps
            let hang = self.hang
            let root = self.root
            let transport = FakeTransport { request in
                grants.mutate { $0.append(request) }
                return (grantJSON(token: "grant-\(grants.value.count)", expiresAt: UInt64(Date().timeIntervalSince1970) + 600, sha: digest), http(200))
            }
            return ModelDownloadCoordinator(
                layout: layout, staging: staging, installer: installer,
                grantClient: ModelGrantClient(transport: transport, now: { Date() }),
                userAgent: "Tests/1", deviceID: { "ios-0123456789abcdef" }, allowsCellular: { false },
                fetch: { request, _, onProgress in
                    fetches.mutate { $0.append(request) }
                    if hang.value { try await Task.sleep(nanoseconds: 60_000_000_000) }
                    if let failure = failures.value.first {
                        failures.mutate { $0.removeFirst() }
                        throw failure
                    }
                    onProgress(50, 100)
                    onProgress(100, 100)
                    let copy = root.appendingPathComponent("served-\(UUID().uuidString).download")
                    try FileManager.default.copyItem(at: served, to: copy)
                    return copy
                },
                sleep: { seconds in sleeps.mutate { $0.append(seconds) } })
        }

        func config(worker: Bool = true) -> ModelDownloadConfig {
            ModelDownloadConfig(
                signedURL: URL(string: "https://worker.example/test/model.tar.gz"), expectedSHA256: digest,
                workerBase: worker ? "https://worker.example" : nil)
        }

        func prepare(_ coordinator: ModelDownloadCoordinator, config: ModelDownloadConfig? = nil) async throws -> URL {
            let phases = self.phases
            return try await coordinator.prepare(spec, config: config ?? self.config()) { progress in
                phases.mutate { $0.append(progress.phase) }
            }
        }
    }

    @Test func downloadsWithAGrantInstallsValidatesAndDropsTheArchive() async throws {
        let rig = try Rig()
        let directory = try await rig.prepare(rig.coordinator())
        #expect(try String(contentsOf: directory.appendingPathComponent("model.bin"), encoding: .utf8) == "v1")
        #expect(rig.grants.value.count == 1)
        #expect(rig.fetches.value.count == 1)
        #expect(rig.fetches.value.first?.value(forHTTPHeaderField: "Authorization") == "Bearer grant-1")
        #expect(!FileManager.default.fileExists(atPath: rig.layout.archiveURL(for: rig.spec).path), "the expanded model is the source of truth")
        let phases = rig.phases.value
        #expect(phases.first == .checking && phases.last == .ready)
        for expected in [ModelDownloadProgress.Phase.requestingGrant, .downloading, .installing, .validating] {
            #expect(phases.contains(expected), "missing \(expected)")
        }
    }

    @Test func anInstalledModelIsReusedWithoutAnyNetwork() async throws {
        let rig = try Rig()
        _ = try await rig.prepare(rig.coordinator())
        let again = try await rig.prepare(rig.coordinator())
        #expect(again == rig.layout.modelDirectory(for: rig.spec))
        #expect(rig.fetches.value.count == 1 && rig.grants.value.count == 1)
        #expect(await rig.coordinator().isReady(rig.spec))
    }

    @Test func aStagedArchiveIsAdoptedInsteadOfDownloading() async throws {
        let rig = try Rig()
        let location = rig.root.appendingPathComponent("system.tmp")
        try FileManager.default.copyItem(at: rig.archive, to: location)
        #expect(rig.staging.stageCompleted(location: location, model: rig.spec.id, response: http(200)))

        _ = try await rig.prepare(rig.coordinator())
        #expect(rig.fetches.value.isEmpty && rig.grants.value.isEmpty)
        #expect(rig.phases.value.contains(.usingStagedArchive))
        #expect(rig.staging.stagedArchive(for: rig.spec.id) == nil)
    }

    @Test func aTransientFailureRetriesWithBackoffAndThenSucceeds() async throws {
        let rig = try Rig()
        rig.failures.value = [URLError(.timedOut), ModelDownloadError.httpStatus(503)]
        _ = try await rig.prepare(rig.coordinator())
        #expect(rig.fetches.value.count == 3)
        #expect(rig.sleeps.value == [1, 2])
        #expect(rig.phases.value.contains(.retrying(attempt: 2)) && rig.phases.value.contains(.retrying(attempt: 3)))
    }

    @Test func aPermanentFailureIsNotRetried() async throws {
        let rig = try Rig()
        rig.failures.value = [ModelDownloadError.httpStatus(404)]
        await #expect(throws: ModelDownloadError.httpStatus(404)) { try await rig.prepare(rig.coordinator()) }
        #expect(rig.fetches.value.count == 1 && rig.sleeps.value.isEmpty)
    }

    @Test func retriesRunOutOnTheLastAttemptWithTheRealError() async throws {
        let rig = try Rig()
        rig.failures.value = [URLError(.timedOut), URLError(.timedOut), URLError(.timedOut)]
        await #expect(throws: (any Error).self) { try await rig.prepare(rig.coordinator()) }
        #expect(rig.fetches.value.count == 3)
    }

    @Test func aRejectedGrantIsRefreshedOnceThenTheDownloadSucceeds() async throws {
        let rig = try Rig()
        rig.failures.value = [ModelDownloadError.httpStatus(401)]
        _ = try await rig.prepare(rig.coordinator())
        #expect(rig.grants.value.count == 2)
        #expect(rig.fetches.value.map { $0.value(forHTTPHeaderField: "Authorization") } == ["Bearer grant-1", "Bearer grant-2"])
        #expect(rig.sleeps.value.isEmpty, "a grant refresh is not a network retry")
    }

    @Test func aSecondRejectionIsFinal() async throws {
        let rig = try Rig()
        rig.failures.value = [ModelDownloadError.httpStatus(401), ModelDownloadError.httpStatus(401)]
        await #expect(throws: ModelDownloadError.httpStatus(401)) { try await rig.prepare(rig.coordinator()) }
        #expect(rig.grants.value.count == 2)
    }

    @Test func aSignedURLBuildSendsNoGrantRequest() async throws {
        let rig = try Rig()
        _ = try await rig.prepare(rig.coordinator(), config: rig.config(worker: false))
        #expect(rig.grants.value.isEmpty)
        #expect(rig.fetches.value.first?.value(forHTTPHeaderField: "Authorization") == nil)
    }

    @Test func aWrongArchiveFromTheServerIsRejectedAndDeleted() async throws {
        let rig = try Rig()
        let wrong = try makeArchive(in: rig.root, weights: "not what was pinned")
        await #expect(throws: ModelDownloadError.self) { try await rig.prepare(rig.coordinator(serving: wrong)) }
        #expect(!FileManager.default.fileExists(atPath: rig.layout.archiveURL(for: rig.spec).path))
        let ready = await rig.coordinator().isReady(rig.spec)
        #expect(!ready)
    }

    @Test func aFailedValidationKeepsThePreviousModelAndDeletesTheBadArchive() async throws {
        let first = try Rig(weights: "v1")
        _ = try await first.prepare(first.coordinator())

        let broken = try makeArchive(in: first.root, weights: "v2-broken")
        let brokenSpec = makeSpec(sha: try sha(broken))
        let installer = fakeInstaller(failWhenContains: "v2-broken")
        let coordinator = first.coordinator(installer: installer, serving: broken)
        let config = ModelDownloadConfig(
            signedURL: URL(string: "https://worker.example/test/model.tar.gz"), expectedSHA256: brokenSpec.expectedSHA256, workerBase: nil)
        await #expect(throws: ArtifactError.self) { try await coordinator.prepare(brokenSpec, config: config) }
        let model = first.layout.modelDirectory(for: brokenSpec)
        #expect(try String(contentsOf: model.appendingPathComponent("model.bin"), encoding: .utf8) == "v1")
        #expect(!FileManager.default.fileExists(atPath: first.layout.archiveURL(for: brokenSpec).path))
    }

    @Test func missingConfigurationFailsClosedBeforeAnyWork() async throws {
        let rig = try Rig()
        let noURL = ModelDownloadConfig(signedURL: nil, expectedSHA256: rig.digest, workerBase: nil)
        await #expect(throws: ModelDownloadError.notConfigured) { try await rig.prepare(rig.coordinator(), config: noURL) }

        let noDigest = ModelArtifactSpec(
            id: "test-model", backend: "test", platform: "ios", route: "/r", directoryName: "model", expectedSHA256: nil,
            requiredFiles: ["model.bin"], requiredStageNames: ["Enc"])
        let config = ModelDownloadConfig(signedURL: URL(string: "https://w.example/m"), expectedSHA256: nil, workerBase: nil)
        await #expect(throws: ModelDownloadError.missingExpectedDigest("model")) {
            try await rig.coordinator().prepare(noDigest, config: config)
        }
        #expect(rig.fetches.value.isEmpty)
    }

    @Test func cancellingTheCallerStopsTheDownloadWithoutRetrying() async throws {
        let rig = try Rig()
        rig.hang.value = true
        let coordinator = rig.coordinator()
        let task = Task { try await rig.prepare(coordinator) }
        while rig.fetches.value.isEmpty { await Task.yield() }
        task.cancel()
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(rig.sleeps.value.isEmpty, "a cancellation is never retried")
        #expect(rig.grants.value.count == 1)
        let ready = await coordinator.isReady(rig.spec)
        #expect(!ready)
    }

    @Test func aNewDigestUpgradesAnInstalledModel() async throws {
        let rig = try Rig(weights: "v1")
        _ = try await rig.prepare(rig.coordinator())

        let v2 = try makeArchive(in: rig.root, weights: "v2")
        let v2Spec = makeSpec(sha: try sha(v2))
        let upgrade = rig.coordinator(serving: v2)
        let ready = await upgrade.isReady(v2Spec)
        #expect(!ready, "an install pinned to another digest is not the model this build wants")
        let config = ModelDownloadConfig(signedURL: URL(string: "https://worker.example/test/model.tar.gz"), expectedSHA256: v2Spec.expectedSHA256, workerBase: nil)
        let directory = try await upgrade.prepare(v2Spec, config: config)
        #expect(try String(contentsOf: directory.appendingPathComponent("model.bin"), encoding: .utf8) == "v2")
    }
}
