import RightKitImportQueue
import Foundation
import YouTubeKit

enum MediaURLError: LocalizedError {
    case invalidURL
    case noDecodableAudioStream
    case downloadFailed(Int)

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "That doesn't look like a valid media URL."
        case .noDecodableAudioStream:
            return "No AAC audio stream was available for this video."
        case .downloadFailed(let status):
            return "The media download failed (HTTP \(status))."
        }
    }
}

/// Resolves a remote media job to a local audio file the transcriber can
/// read. YouTube: YouTubeKit LOCAL stream extraction (never the library's
/// remote-server fallback), preferring the highest-bitrate m4a/AAC
/// audio-only stream — AVFoundation decodes AAC natively, so no FFmpeg.
/// Direct media URLs (podcast enclosures, CDN clips): plain download.
enum MediaURLExtractor {
    struct FetchedAudio: Sendable {
        let fileURL: URL
        let title: String?
    }

    static func fetchAudio(for kind: JobKind, input: String) async throws -> FetchedAudio {
        guard let url = URL(string: input) else { throw MediaURLError.invalidURL }
        switch kind {
        case .youtube:
            return try await fetchYouTubeAudio(url: url)
        default:
            let ext = url.pathExtension.isEmpty ? "mp3" : url.pathExtension
            let file = try await download(url, fileExtension: ext)
            return FetchedAudio(fileURL: file, title: url.lastPathComponent)
        }
    }

    private static func fetchYouTubeAudio(url: URL) async throws -> FetchedAudio {
        let video = YouTube(url: url)
        let streams = try await video.streams
        guard let audio = streams
            .filterAudioOnly()
            .filter({ $0.fileExtension == .m4a })
            .highestAudioBitrateStream() else {
            throw MediaURLError.noDecodableAudioStream
        }
        let file = try await download(audio.url, fileExtension: "m4a")
        let title = (try? await video.metadata)?.title
        return FetchedAudio(fileURL: file, title: title)
    }

    private static func download(_ url: URL, fileExtension: String) async throws -> URL {
        let (temporary, response) = try await URLSession.shared.download(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            try? FileManager.default.removeItem(at: temporary)
            throw MediaURLError.downloadFailed(http.statusCode)
        }
        let destination = FileManager.default.temporaryDirectory
            .appendingPathComponent("media-\(UUID().uuidString).\(fileExtension)")
        try FileManager.default.moveItem(at: temporary, to: destination)
        return destination
    }
}
