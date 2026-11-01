import Foundation

/// Wire-parity mirror of the desktop `Kind` (src/contract.ts / crates/detect
/// JobKind). Raw values match the desktop wire strings so artifacts and
/// diagnostics stay comparable across platforms.
enum JobKind: String, Codable, CaseIterable, Sendable {
    case instagramReel = "reel"
    case instagramPostMaybe = "instagram_post"
    case carousel
    case youtube
    case tiktok
    case threads
    case facebook
    case videoUrl = "video_url"
    case mediaUrl = "media_url"
    case article
    case localFile = "local_file"
    /// Pasted or received plain text: stored as a finished result as-is, no extraction.
    case text
    case unknown

    /// Kinds whose ingestion still needs the desktop pipeline. Handled
    /// natively on iOS instead:
    ///  * `youtube` / `mediaUrl` — YouTubeKit local stream extraction or a
    ///    plain download, then AVFoundation decode + on-device ASR.
    ///  * `threads` — text posts read through the WKWebView render (Threads
    ///    is JS-rendered, so a plain fetch returns an empty shell).
    ///  * `carousel` / `instagramPostMaybe` — public posts captured through
    ///    the same WebView (screenshot → Next → OCR). Cookies are the
    ///    desktop's fallback for private/age-gated posts, not its normal
    ///    path, so public carousels need no auth; walled ones are detected
    ///    and refused.
    ///  * `article` / `localFile` — fetch/readability and the local extractors.
    /// What's left needs per-platform video extractors that don't exist on
    /// iOS. They stay classified so the error names the real reason.
    var needsDesktopPipeline: Bool {
        switch self {
        case .instagramReel, .tiktok, .facebook, .videoUrl:
            return true
        case .youtube, .mediaUrl, .article, .threads,
             .carousel, .instagramPostMaybe, .localFile, .text, .unknown:
            return false
        }
    }
}

/// Mirror of the desktop `Stage`. `downloading` covers the article fetch.
enum JobStage: String, Codable, Sendable {
    case queued, detecting, downloading, transcribing, writing, done, error, cancelled
    /// WS-12: handed to the user's Mac as a CloudKit QueueRequest; the phone
    /// waits for the transcript to sync back. Not re-run on relaunch.
    case queuedOnMac = "queued_on_mac"
}

struct Job: Identifiable, Codable, Sendable, Equatable, Hashable {
    let id: UUID
    let input: String
    var kind: JobKind
    var stage: JobStage
    var pct: Double
    var title: String?
    var error: String?
    var createdAt: Date
    /// Set when the job finishes and its output lands in the library.
    var artifactID: UUID?
    /// Files imported via the file picker arrive security-scoped; the app
    /// copies them into its container and stores the container-relative
    /// filename here (survives relaunch, unlike a scoped URL).
    var importedFileName: String?
    /// WS-12: CloudKit QueueRequest id while `stage == .queuedOnMac`.
    var macRequestID: String?
    /// WS-12: latest Mac-side status text ("Running on Mac"…), display only.
    var macDetail: String?

    init(input: String, kind: JobKind, importedFileName: String? = nil) {
        self.id = UUID()
        self.input = input
        self.kind = kind
        self.stage = .queued
        self.pct = 0
        self.createdAt = Date()
        self.importedFileName = importedFileName
    }
}
