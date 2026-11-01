#if os(iOS)
import AVFoundation
import PDFKit
import SwiftUI
import UIKit

/// Copy the whole extracted text in one tap. The single most-used action on a
/// result — the point of the app is getting text *out* — so it is a first-class
/// button, not buried in the share sheet.
struct CopyTextButton: View {
    let text: String
    @State private var copied = false

    var body: some View {
        Button {
            UIPasteboard.general.string = text
            copied = true
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            Task {
                try? await Task.sleep(nanoseconds: 1_600_000_000)
                copied = false
            }
        } label: {
            Label(copied ? "Copied" : "Copy text", systemImage: copied ? "checkmark" : "doc.on.doc")
        }
        .disabled(text.isEmpty)
        .accessibilityLabel(copied ? "Copied" : "Copy all text")
    }
}

/// What the job started from, previewed in whatever form that source has:
/// an image, the first page of a PDF, a video frame, a named file chip for
/// audio, or the link itself for URL jobs. Every kind shows *something* —
/// a result screen that only says "done" is the bug this replaces.
struct OriginalPreview: View {
    let fileURL: URL?
    let sourceInput: String

    @State private var thumbnail: UIImage?
    @State private var didLoad = false

    var body: some View {
        Group {
            if let fileURL {
                if let thumbnail {
                    Image(uiImage: thumbnail)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: .infinity)
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                } else if didLoad {
                    fileChip(for: fileURL)
                } else {
                    ProgressView().frame(maxWidth: .infinity, alignment: .leading)
                }
            } else if let url = URL(string: sourceInput), url.scheme?.hasPrefix("http") == true {
                Link(destination: url) {
                    Label(sourceInput, systemImage: "safari")
                        .font(.srMono(12))
                        .lineLimit(2)
                }
            } else {
                Text(sourceInput)
                    .font(.srMono(12))
                    .foregroundStyle(Color.srMuted)
                    .textSelection(.enabled)
            }
        }
        .task(id: fileURL) {
            guard let fileURL else { return }
            thumbnail = await Self.thumbnail(for: fileURL)
            didLoad = true
        }
    }

    /// Audio (and anything unrenderable) still gets a concrete row naming the
    /// file, so the source is never a blank space.
    private func fileChip(for url: URL) -> some View {
        Label {
            Text(url.lastPathComponent)
                .font(.srMono(12))
                .lineLimit(1)
                .truncationMode(.middle)
        } icon: {
            Image(systemName: Self.symbol(for: url))
        }
        .foregroundStyle(Color.srMuted)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.srMuted.opacity(0.10), in: RoundedRectangle(cornerRadius: 10))
    }

    private static func symbol(for url: URL) -> String {
        switch url.pathExtension.lowercased() {
        case "mp3", "m4a", "wav", "aac", "caf", "aiff": return "waveform"
        case "mp4", "mov", "m4v": return "film"
        case "pdf": return "doc.richtext"
        default: return "doc"
        }
    }

    /// One thumbnail path per source type. Images load directly, PDFs render
    /// page 1, video pulls an early frame; audio has no visual and returns nil
    /// so the caller falls back to the file chip.
    static func thumbnail(for url: URL) async -> UIImage? {
        if let image = UIImage(contentsOfFile: url.path) { return image }

        if url.pathExtension.lowercased() == "pdf",
           let page = PDFDocument(url: url)?.page(at: 0) {
            return page.thumbnail(of: CGSize(width: 1000, height: 1400), for: .mediaBox)
        }

        let asset = AVURLAsset(url: url)
        guard (try? await asset.loadTracks(withMediaType: .video))?.isEmpty == false else { return nil }
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        // A frame at 1s beats frame 0, which is often a black fade-in.
        let time = CMTime(seconds: 1, preferredTimescale: 600)
        guard let cgImage = try? await generator.image(at: time).image else { return nil }
        return UIImage(cgImage: cgImage)
    }
}
#endif
