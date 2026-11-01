import Foundation
import Observation

// ENG-110: optional large tool/model packs (model weights, WASM runtime binaries) ship as
// iOS On-Demand Resources rather than in the base app bundle, fetched post-install on demand.
// Pack payloads are tagged into Xcode's ODR resource groups. The first shipped pack carries a
// versioned portable-tool manifest; future model weights or runtime binaries can join its tag
// without changing fetch semantics. The manager exposes
// `fetchedPacks: [String]`; LocalRuntime transports those facts into Rust's
// `MobileToolCapabilities.fetched_resource_packs` capability manifest.

/// One entry in the declared catalog of optional on-demand resource packs.
public struct OnDemandResourcePack: Sendable, Equatable, Identifiable {
    public var id: String { tag }
    /// The ODR tag as declared in the Xcode project's "On Demand Resources Tags" build
    /// setting / Asset Catalog tag assignment. Must exactly match the project-side tag.
    public let tag: String
    public let displayName: String
    /// Approximate installed size, for UI display before a fetch begins. Not authoritative —
    /// `NSBundleResourceRequest` reports the real size once a fetch starts.
    public let approximateSizeBytes: Int64
    /// Versioned resource manifest supplied by this tag. A tag is not exposed
    /// to runtime capability discovery until this file validates.
    public let manifestFileName: String
    /// Files supplied by this ODR tag. A fetched request is not useful until
    /// its declared payload resolves in the app bundle.
    public let payloadFileNames: [String]

    public init(
        tag: String,
        displayName: String,
        approximateSizeBytes: Int64,
        manifestFileName: String = "manifest.json",
        payloadFileNames: [String] = []
    ) {
        self.tag = tag
        self.displayName = displayName
        self.approximateSizeBytes = approximateSizeBytes
        self.manifestFileName = manifestFileName
        self.payloadFileNames = payloadFileNames
    }
}

/// Resource-owned declaration projected to capability discovery only after
/// its ODR request has granted access. This makes tag residency insufficient
/// on its own: an absent, stale, or mismatched payload cannot advertise tools.
public struct OnDemandResourceManifest: Codable, Sendable, Equatable {
    public let schema: String
    public let tag: String
    public let capabilities: [String]
    public let files: [String]

    fileprivate func validates(pack: OnDemandResourcePack) -> Bool {
        schema == "app.coderight.odr-pack.v1"
            && tag == pack.tag
            && Set(files) == Set(pack.payloadFileNames)
            && !capabilities.contains(where: { $0.isEmpty })
    }
}

/// Fetch/pin state for one pack, tracked independently of whether a fetch is in flight.
public enum OnDemandResourceState: Sendable, Equatable {
    case notFetched
    case fetching(progress: Double)
    case fetched
    case pinned
    case failed(OnDemandResourceError)
}

public enum OnDemandResourceError: Sendable, Equatable, LocalizedError {
    case unknownTag(String)
    case noNetwork
    case insufficientSpace
    case cancelled
    case invalidManifest(String)
    case underlying(String)

    public var errorDescription: String? {
        switch self {
        case .unknownTag(let tag):
            return "\"\(tag)\" is not a declared resource pack."
        case .noNetwork:
            return "No network connection available to fetch this pack."
        case .insufficientSpace:
            return "Not enough free space to fetch this pack."
        case .cancelled:
            return "The fetch was cancelled."
        case .invalidManifest(let tag):
            return "\"\(tag)\" did not provide its declared portable-resource manifest."
        case .underlying(let message):
            return message
        }
    }

    /// Maps an NSError from NSBundleResourceRequest into a typed failure. ODR surfaces these
    /// through NSBundleResourceRequestError / NSError userInfo rather than a typed Swift error.
    static func from(_ error: Error) -> OnDemandResourceError {
        let nsError = error as NSError
        // ODR failures are NSCocoaErrorDomain codes (FoundationErrors.h 4992-4994);
        // there is no NSBundleResourceRequestErrorDomain symbol in the SDK.
        if nsError.domain == NSCocoaErrorDomain {
            switch nsError.code {
            case NSBundleOnDemandResourceOutOfSpaceError:
                return .insufficientSpace
            case NSBundleOnDemandResourceExceededMaximumSizeError:
                return .insufficientSpace
            case NSBundleOnDemandResourceInvalidTagError:
                return .unknownTag("")
            default:
                break
            }
        }
        if nsError.domain == NSURLErrorDomain, nsError.code == NSURLErrorNotConnectedToInternet {
            return .noNetwork
        }
        if nsError.domain == NSURLErrorDomain, nsError.code == NSURLErrorCancelled {
            return .cancelled
        }
        return .underlying(nsError.localizedDescription)
    }
}

/// Abstraction over `NSBundleResourceRequest` so tests can inject a fake and never trigger a
/// real ODR fetch. `@unchecked Sendable`: `NSBundleResourceRequest` is a UIKit-adjacent
/// Foundation type that is not itself `Sendable`; every request created by this manager is
/// used only from the `@MainActor`-isolated manager, so isolation — not the type system —
/// is what makes crossing safe here.
public protocol OnDemandResourceRequesting: AnyObject {
    var loadingPriority: Double { get set }
    var progress: Progress { get }
    func beginAccessingResources(completionHandler: @escaping @Sendable (Error?) -> Void)
    func conditionallyBeginAccessingResources(completionHandler: @escaping @Sendable (Bool) -> Void)
    func endAccessingResources()
}

extension NSBundleResourceRequest: OnDemandResourceRequesting {}

/// Creates a resource request for a set of ODR tags. Injectable so tests never touch the
/// real bundle / network. `@Sendable` because the manager may build a request from a
/// background continuation callback.
public typealias OnDemandResourceRequestFactory =
    @Sendable (Set<String>) -> OnDemandResourceRequesting

/// Resolves the resource-owned manifest after an ODR request grants access.
/// Injectable so lifecycle tests remain independent of an app bundle.
public typealias OnDemandResourceManifestResolver =
    @Sendable (OnDemandResourcePack) -> OnDemandResourceManifest?

@MainActor
@Observable
public final class OnDemandResourceManager {
    public static let shippedCatalog = [
        OnDemandResourcePack(
            tag: "coderight-portable-tools-v1",
            displayName: "Portable tools",
            approximateSizeBytes: 1_024,
            payloadFileNames: ["portable-tools.json"]
        )
    ]

    /// Declared optional packs. The manager must also function with an empty
    /// injected catalog for deployments that ship no ODR payloads.
    public private(set) var catalog: [OnDemandResourcePack]
    /// Tags currently resident and accessible on-device, kept current by `refreshFetchedPacks()`
    /// (called on init) and by every successful fetch/pin. LocalRuntime sends
    /// this list into `MobileToolCapabilities.fetched_resource_packs`.
    public private(set) var fetchedPacks: [String] = []
    /// Validated resource declarations for currently accessible tags. This is
    /// direct evidence for capability projection; callers never infer tools
    /// from a catalog entry alone.
    public private(set) var fetchedManifests: [String: OnDemandResourceManifest] = [:]
    public private(set) var states: [String: OnDemandResourceState] = [:]

    private let requestFactory: OnDemandResourceRequestFactory
    private let manifestResolver: OnDemandResourceManifestResolver
    /// Held per-tag so `endAccessingResources()` can be called on release/unpin, and so a
    /// pinned request's access grant is not dropped when it goes out of scope.
    private var activeRequests: [String: OnDemandResourceRequesting] = [:]
    /// NSBundleResourceRequest's documented default `loadingPriority`. The SDK
    /// exports a symbol only for the urgent end of the scale, so the default is
    /// named here rather than repeated as a bare literal at each call site.
    private static let defaultLoadingPriority: Double = 0.5

    private var pinnedTags: Set<String> = []
    private var progressObservations: [String: NSKeyValueObservation] = [:]
    private var fetchGenerations: [String: UUID] = [:]

    public init(
        catalog: [OnDemandResourcePack] = OnDemandResourceManager.shippedCatalog,
        requestFactory: @escaping OnDemandResourceRequestFactory = { tags in
            NSBundleResourceRequest(tags: tags)
        },
        manifestResolver: @escaping OnDemandResourceManifestResolver = {
            OnDemandResourceManager.loadManifest(for: $0)
        }
    ) {
        self.catalog = catalog
        self.requestFactory = requestFactory
        self.manifestResolver = manifestResolver
        for pack in catalog {
            states[pack.tag] = .notFetched
        }
    }

    public func state(for tag: String) -> OnDemandResourceState {
        states[tag] ?? .notFetched
    }

    /// Produces capability-manifest facts from actual ODR availability. The
    /// caller serializes this list into its engine capability query, so a pack
    /// is never advertised merely because it appears in `catalog`.
    public var fetchedResourcePackTags: [String] { fetchedPacks }

    /// Capability facts read from resource-owned manifests. Every entry is
    /// backed by a currently held ODR access request.
    public var fetchedCapabilityIDs: [String] {
        fetchedManifests.values
            .flatMap(\.capabilities)
            .sorted()
    }

    /// Probes each catalog tag with `conditionallyBeginAccessingResources` so packs already
    /// resident from a previous launch (or already downloaded by the App Store) are reflected
    /// in `fetchedPacks` without forcing a fetch. Call once at launch. Never blocks the main
    /// thread — the underlying check runs asynchronously and this method suspends, not spins.
    public func refreshFetchedPacks() async {
        guard !catalog.isEmpty else { return }
        var resident: [String] = []
        for pack in catalog {
            // A held access grant itself keeps this pack resident. Do not
            // replace it with a second request during Settings/capability
            // refreshes: replacement would silently discard lifecycle owner.
            if activeRequests[pack.tag] != nil,
                fetchedManifests[pack.tag] != nil
            {
                resident.append(pack.tag)
                continue
            }
            let request = requestFactory(Set([pack.tag]))
            let isResident = await withCheckedContinuation {
                (continuation: CheckedContinuation<Bool, Never>) in
                request.conditionallyBeginAccessingResources { available in
                    continuation.resume(returning: available)
                }
            }
            if isResident, let manifest = validatedManifest(for: pack) {
                resident.append(pack.tag)
                activeRequests[pack.tag] = request
                fetchedManifests[pack.tag] = manifest
                states[pack.tag] = pinnedTags.contains(pack.tag) ? .pinned : .fetched
            } else {
                request.endAccessingResources()
                fetchedManifests[pack.tag] = nil
                if isResident {
                    states[pack.tag] = .failed(.invalidManifest(pack.tag))
                } else {
                    states[pack.tag] = .notFetched
                }
            }
        }
        fetchedPacks = resident.sorted()
    }

    /// Fetches one pack's resources. Never blocks the main thread: `beginAccessingResources`
    /// is asynchronous and this method suspends on a continuation rather than polling.
    /// Progress updates land on `states[tag]` via KVO on `Progress`, which this method
    /// re-dispatches onto the main actor.
    public func fetch(tag: String) async throws {
        guard let pack = catalog.first(where: { $0.tag == tag }) else {
            let error = OnDemandResourceError.unknownTag(tag)
            states[tag] = .failed(error)
            throw error
        }
        if case .fetched = states[pack.tag] { return }
        if case .pinned = states[pack.tag] { return }

        let request = requestFactory(Set([pack.tag]))
        let generation = UUID()
        activeRequests[pack.tag] = request
        fetchGenerations[pack.tag] = generation
        states[pack.tag] = .fetching(progress: 0)

        let observation = request.progress.observe(\.fractionCompleted, options: [.new]) {
            [weak self] progress, _ in
            let fraction = progress.fractionCompleted
            Task { @MainActor [weak self] in
                guard let self, case .fetching = self.states[pack.tag] else { return }
                self.states[pack.tag] = .fetching(progress: fraction)
            }
        }
        progressObservations[pack.tag] = observation

        do {
            try await withCheckedThrowingContinuation {
                (continuation: CheckedContinuation<Void, Error>) in
                request.beginAccessingResources { error in
                    if let error {
                        continuation.resume(throwing: error)
                    } else {
                        continuation.resume(returning: ())
                    }
                }
            }
            guard fetchGenerations[pack.tag] == generation else {
                throw OnDemandResourceError.cancelled
            }
            guard let manifest = validatedManifest(for: pack) else {
                throw OnDemandResourceError.invalidManifest(pack.tag)
            }
            progressObservations[pack.tag] = nil
            fetchGenerations[pack.tag] = nil
            fetchedManifests[pack.tag] = manifest
            states[pack.tag] = pinnedTags.contains(pack.tag) ? .pinned : .fetched
            if !fetchedPacks.contains(pack.tag) {
                fetchedPacks = (fetchedPacks + [pack.tag]).sorted()
            }
        } catch {
            progressObservations[pack.tag] = nil
            if fetchGenerations[pack.tag] == generation {
                fetchGenerations[pack.tag] = nil
                activeRequests[pack.tag]?.endAccessingResources()
                activeRequests[pack.tag] = nil
            }
            let typed = (error as? OnDemandResourceError) ?? OnDemandResourceError.from(error)
            if typed != .cancelled {
                states[pack.tag] = .failed(typed)
            }
            throw typed
        }
    }

    /// Cancels an in-flight fetch for `tag`, if any, and releases its access grant.
    public func cancelFetch(tag: String) {
        guard case .fetching = states[tag] else { return }
        activeRequests[tag]?.progress.cancel()
        progressObservations[tag] = nil
        fetchGenerations[tag] = nil
        activeRequests[tag]?.endAccessingResources()
        activeRequests[tag] = nil
        fetchedManifests[tag] = nil
        states[tag] = .notFetched
    }

    /// Raises the request's loading priority to `NSBundleResourceRequestLoadingPriorityUrgent`
    /// so the system does not purge the pack while it is actively in use. A pinned pack must
    /// already be fetched (or fetching) — pin does not itself fetch.
    public func pin(tag: String) {
        pinnedTags.insert(tag)
        guard let request = activeRequests[tag] else { return }
        request.loadingPriority = NSBundleResourceRequestLoadingPriorityUrgent
        if case .fetched = states[tag] {
            states[tag] = .pinned
        }
    }

    /// Lowers the request's loading priority back to the default, allowing iOS to purge the
    /// pack under disk pressure, and releases the access grant.
    public func unpin(tag: String) {
        pinnedTags.remove(tag)
        guard let request = activeRequests[tag] else { return }
        // The SDK exports only ...LoadingPriorityUrgent; the documented default
        // priority is 0.5, which is what "back to default" means here.
        request.loadingPriority = Self.defaultLoadingPriority
        if case .pinned = states[tag] {
            states[tag] = .fetched
        }
    }

    /// Releases a fetched pack's access grant entirely (beyond unpin) and removes it from
    /// `fetchedPacks`. iOS may still keep the on-disk data until it needs the space.
    public func release(tag: String) {
        pinnedTags.remove(tag)
        activeRequests[tag]?.endAccessingResources()
        activeRequests[tag] = nil
        progressObservations[tag] = nil
        fetchedManifests[tag] = nil
        states[tag] = .notFetched
        fetchedPacks.removeAll { $0 == tag }
    }

    private func validatedManifest(for pack: OnDemandResourcePack) -> OnDemandResourceManifest? {
        guard let manifest = manifestResolver(pack), manifest.validates(pack: pack) else {
            return nil
        }
        return manifest
    }

    @usableFromInline
    nonisolated static func loadManifest(for pack: OnDemandResourcePack) -> OnDemandResourceManifest? {
        let fileName = pack.manifestFileName as NSString
        let resource = fileName.deletingPathExtension
        let ext = fileName.pathExtension
        guard let url = Bundle.main.url(forResource: resource, withExtension: ext),
            let data = try? Data(contentsOf: url)
        else { return nil }
        return try? JSONDecoder().decode(OnDemandResourceManifest.self, from: data)
    }
}
