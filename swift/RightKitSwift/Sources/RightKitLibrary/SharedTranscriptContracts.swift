import Foundation

enum Shared {
    static let appGroup = "group.app.heardright.ios.native"
    static let latestRawTranscriptKey = "latestRawTranscript"
    static let latestTranscriptKey = "latestTranscript"
    static let latestTranscriptDogfoodSessionIDKey = "latestTranscriptDogfoodSessionID"
    /// App Group deletion authority shared by the app, Share extension, and
    /// Watch hand-off. Values are a JSON dictionary keyed by logical UUID.
    static let transcriptTombstonesKey = "transcriptTombstones.v1"
    /// Observed CloudKit record-name aliases, including aliases seen before a
    /// local delete. Kept separately so a later delete can tombstone every
    /// historical record name for one logical transcript.
    static let transcriptAliasesKey = "transcriptAliases.v1"
    static let transcriptPendingCloudDeletionIDsKey = "transcriptPendingCloudDeletionIDs.v1"
    /// Active account/container/zone identity for cloud deletion authority.
    /// Values are intentionally not migrated: an unknown prior account must
    /// never inherit another account's tombstones or pending deletions.
    static let transcriptCloudScopeKey = "transcriptCloudScope.v1"
    /// Last fully-applied private-zone change token. Kept outside transcript
    /// rows so a restart resumes from CloudKit's durable change cursor.
    static let transcriptCloudChangeTokenKey = "transcriptCloudChangeToken.v1"
    static let transcriptCloudRemoteVersionsKey = "transcriptCloudRemoteVersions.v1"
    /// Monotonic fence for history hydration. It is persisted because
    /// hydration and CloudKit work run in separate actors/processes.
    static let transcriptMutationEpochKey = "transcriptMutationEpoch.v1"
    static let latestTranscriptRecordIDKey = "latestTranscriptRecordID"
    static let pendingShortcutActionKey = "pendingShortcutAction"
    static let pendingShortcutActionSavedAtKey = "pendingShortcutActionSavedAt"
    static let recentRawTranscriptsKey = "recentRawTranscripts"
    static let recentTranscriptRecordsKey = "recentTranscriptRecords"
    static let recordingActiveKey = "recordingActive"
    static let recordingActiveSinceKey = "recordingActiveSince"
    static let recordingMicLevelKey = "recordingMicLevel"
    static let recordingMicLevelAtKey = "recordingMicLevelAt"
    static let recordingProcessKey = "recordingProcessHandle"
    /// Session-bound ownership lease shared by the app and App Intents.
    /// A process handle alone cannot prove that a state row belongs to the
    /// current recording after an app relaunch.
    static let recordingOwnerKey = "recordingOwner"
    static let recordingOwnerSavedAtKey = "recordingOwnerSavedAt"
    static let recordingOwnerSessionKey = "recordingOwnerSessionID"
    static let recordingOwnerTTL: TimeInterval = 30
    static let selectedPolishModeKey = "selectedPolishMode"
    static let advancedCleanupEnabledKey = "advancedCleanupEnabled"
    static let aiPolishEnabledKey = "aiPolishEnabled"
    static let maxRecentTranscripts = 200
    static let maxPendingShortcutAge: TimeInterval = 60
    static let maxRecordingActiveAge: TimeInterval = 2 * 60
    /// Hard cap on one recording. The app stops capture here, and the Live
    /// Activity timer bounds its range to it so `Text(timerInterval:)` sizes
    /// for "10:00" instead of the widest value up to `Date.distantFuture`.
    static let maxRecordingDuration: TimeInterval = 10 * 60
    static let recordingActivityStaleAfter: TimeInterval = 60
    /// Stable per-device identity, shared across the app/Share extension/Watch
    /// hand-off so a transcript's `originDeviceID` (see `LibraryTranscript`)
    /// is meaningful even from an extension process. Generated once and
    /// persisted in the App Group; never synced by CloudKit itself, mirroring
    /// `pro_bridge`/`cloud_sync`'s device-local consent state on desktop.
    static let deviceIDKey = "heardright.device.id.v1"

    /// This device's stable identity, creating and persisting one on first
    /// access if none exists yet.
    static var deviceID: String {
        let defaults = UserDefaults(suiteName: appGroup) ?? .standard
        if let existing = defaults.string(forKey: deviceIDKey), !existing.isEmpty {
            return existing
        }
        let generated = UUID().uuidString
        defaults.set(generated, forKey: deviceIDKey)
        return generated
    }
}

enum ShortcutAction: String {
    case toggleDictation
    case startDictation
    case stopDictation
    case stopAndCopyTranscript
    case cancelDictation
    case copyLatestTranscript
    case openApp
}

/// Tri-state mirror of the cross-extension App Group recording flag.
/// MM-P2-STALE-RECORDING-FLAG: stale state (e.g. after a crash) gets a visible UI affordance
/// instead of silently claiming "Recording in HeardRight" for the full
/// `maxRecordingActiveAge` window.
enum RecordingActivityState: Equatable {
    case off
    case live(startedAt: Date)
    case stale(startedAt: Date)

    var isLive: Bool {
        if case .live = self { return true }
        return false
    }

    var isStale: Bool {
        if case .stale = self { return true }
        return false
    }
}

enum RecordingRecoveryState {
    static func canRetryCapturedAudio(
        hasCapturedAudio: Bool,
        isRecording: Bool,
        isMicTesting: Bool,
        isTranscribing: Bool
    ) -> Bool {
        hasCapturedAudio && !isRecording && !isMicTesting && !isTranscribing
    }
}

/// Which engine produced a transcript. `.localModel` is the only value that
/// exists: HeardRight transcribes with its own Core ML model or it refuses to
/// record. The tag is kept so records stay self-describing if a second local
/// model is ever added.
enum TranscriptEngineTag: String, Codable, Equatable {
    case localModel = "local_model"
}

struct SharedDisplayTranscript: Identifiable, Equatable {
    let id: UUID
    let text: String
    let createdAt: Date
    let dogfoodSessionID: String?
    let engine: TranscriptEngineTag?

    init(
        id: UUID = UUID(),
        text: String,
        createdAt: Date,
        dogfoodSessionID: String? = nil,
        engine: TranscriptEngineTag? = nil
    ) {
        self.id = id
        self.text = text
        self.createdAt = createdAt
        self.dogfoodSessionID = dogfoodSessionID
        self.engine = engine
    }
}

struct SharedTranscriptRecord: Codable, Equatable {
    let id: UUID
    let rawText: String
    let createdAt: Date
    let dogfoodSessionID: String?
    /// Optional so records written before engine tagging still decode.
    let engine: TranscriptEngineTag?

    init(
        id: UUID = UUID(),
        rawText: String,
        createdAt: Date,
        dogfoodSessionID: String? = nil,
        engine: TranscriptEngineTag? = nil
    ) {
        self.id = id
        self.rawText = rawText
        self.createdAt = createdAt
        self.dogfoodSessionID = dogfoodSessionID
        self.engine = engine
    }
}

/// Durable deletion authority for one logical transcript. `remoteRecordNames`
/// retains legacy CloudKit aliases so a delete cannot leave a live twin behind.
struct SharedTranscriptTombstone: Codable, Equatable {
    let id: UUID
    var deletedAt: Date
    var remoteRecordNames: [String]
}

enum DeepLink {
    static let scheme = "heardright"

    static func url(for action: ShortcutAction) -> URL {
        switch action {
        case .toggleDictation:
            return URL(string: "\(scheme)://toggle")!
        case .startDictation:
            return URL(string: "\(scheme)://dictate")!
        case .stopDictation:
            return URL(string: "\(scheme)://stop")!
        case .stopAndCopyTranscript:
            return URL(string: "\(scheme)://stop-copy")!
        case .cancelDictation:
            return URL(string: "\(scheme)://cancel")!
        case .copyLatestTranscript:
            return URL(string: "\(scheme)://copy-latest")!
        case .openApp:
            return URL(string: "\(scheme)://open")!
        }
    }

    static func action(for url: URL) -> ShortcutAction? {
        guard url.scheme == scheme else { return nil }
        let route = url.host ?? url.pathComponents.dropFirst().first ?? ""
        switch route {
        case "toggle", "toggle-dictation":
            return .toggleDictation
        case "dictate", "start":
            return .startDictation
        case "stop":
            return .stopDictation
        case "stop-copy", "stop-and-copy":
            return .stopAndCopyTranscript
        case "cancel", "cancel-dictation":
            return .cancelDictation
        case "copy-latest", "copy", "insert-last", "insert":
            return .copyLatestTranscript
        case "open":
            return .openApp
        default:
            return nil
        }
    }
}
