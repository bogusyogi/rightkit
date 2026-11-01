import RightKitImportQueue
import Foundation

/// Stage-1 job classifier — a faithful subset port of `crates/detect`
/// (pure string parsing, no network calls). Kept in sync by
/// `SourceDetectTests`, which mirror the Rust crate's fixtures.
enum SourceDetect {
    private static let mediaExtensions: Set<String> = [
        "mp3", "m4a", "wav", "flac", "ogg", "opus", "aac",
        "mp4", "mov", "mkv", "webm", "avi", "m4v",
    ]
    private static let imageExtensions: Set<String> = [
        "png", "jpg", "jpeg", "heic", "heif", "gif", "bmp", "tif", "tiff", "webp",
    ]
    /// Hosted-video sites yt-dlp can extract (desktop routes these to the
    /// audio pipeline; iOS classifies them to explain why it can't).
    private static let videoHosts: Set<String> = [
        "x.com", "twitter.com", "vimeo.com", "twitch.tv", "reddit.com",
        "linkedin.com", "loom.com", "dailymotion.com", "streamable.com",
        "soundcloud.com",
    ]

    static func classify(_ input: String) -> JobKind {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .unknown }

        let lower = trimmed.lowercased()
        if lower.hasPrefix("http://") || lower.hasPrefix("https://") {
            return classifyWebURL(trimmed)
        }
        if lower.hasPrefix("file://") {
            return classifyLocalPath(URL(string: trimmed)?.path ?? trimmed)
        }
        if trimmed.contains("/") || trimmed.contains("\\") || trimmed.hasPrefix("~") {
            return classifyLocalPath(trimmed)
        }
        return .unknown
    }

    static func isImagePath(_ path: String) -> Bool {
        imageExtensions.contains(fileExtension(of: path))
    }

    static func isMediaPath(_ path: String) -> Bool {
        mediaExtensions.contains(fileExtension(of: path))
    }

    static func isPDFPath(_ path: String) -> Bool {
        fileExtension(of: path) == "pdf"
    }

    // MARK: - Internals

    private static func classifyWebURL(_ raw: String) -> JobKind {
        guard let url = URL(string: raw), var host = url.host?.lowercased() else {
            return .unknown
        }
        for prefix in ["www.", "m.", "vm."] where host.hasPrefix(prefix) {
            host = String(host.dropFirst(prefix.count))
        }
        let path = url.path.lowercased()

        switch host {
        case "instagram.com":
            if path.contains("/reel/") || path.contains("/reels/") { return .instagramReel }
            if path.contains("/p/") { return .instagramPostMaybe }
            return .article
        case "youtube.com", "youtu.be":
            return .youtube
        case "tiktok.com":
            return .tiktok
        case "threads.com", "threads.net":
            return .threads
        case "facebook.com", "fb.watch":
            return .facebook
        default:
            break
        }
        if videoHosts.contains(host) { return .videoUrl }
        if mediaExtensions.contains(fileExtension(of: path)) { return .mediaUrl }
        return .article
    }

    private static func classifyLocalPath(_ path: String) -> JobKind {
        let ext = fileExtension(of: path)
        if mediaExtensions.contains(ext) || imageExtensions.contains(ext) || ext == "pdf" {
            return .localFile
        }
        return .unknown
    }

    private static func fileExtension(of path: String) -> String {
        (path as NSString).pathExtension.lowercased()
    }
}
