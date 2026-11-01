#if os(iOS)
// Provenance: operative picker/chip extraction from
// baryhuang/claude-code-by-agents at 7655042eb986bf146079032726a7347ac4547c0e,
// OpenAgents/Views/ChatView.swift, `AttachmentChip` lines 1117–1220.
// The iOS-only image branch removes the donor's macOS PlatformImage alias;
// layout, thumbnail sizing, remove affordance, and file metadata UX are retained.

import SwiftUI
import UIKit
import QuickLook

/// Pending attachment rendered in the composer before send.
struct AttachmentChip: View {
    let attachment: PendingAttachment
    let onRemove: () -> Void

    /// Cached thumbnail decoded once on first appear so we don't re-decode
    /// every time SwiftUI re-runs `body`.
    @State private var thumbnail: UIImage?
    @State private var showPreview = false

    var body: some View {
        Group {
            if attachment.isImage {
                imageVariant
            } else {
                fileVariant
            }
        }
        .onAppear { loadThumbnailIfNeeded() }
        .sheet(isPresented: $showPreview) { PendingAttachmentPreview(attachment: attachment) }
        .help("\(attachment.filename) — \(formattedSize)")
    }

    /// 48×48 thumbnail with a small remove button overlay in the top-right
    /// corner. Slack / iMessage / ChatGPT all use this pattern; the visual
    /// is the identification, so we don't show the synthetic filename.
    private var imageVariant: some View {
        ZStack(alignment: .topTrailing) {
            Button { showPreview = true } label: {
            Group {
                if let image = thumbnail {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFill()
                } else {
                    // Placeholder while ImageIO decodes — only visible for
                    // a frame or two on first paste of a large image.
                    Rectangle()
                        .fill(Color.gray.opacity(0.18))
                        .overlay {
                            Image(systemName: "photo")
                                .foregroundStyle(.secondary)
                        }
                }
            }
            .frame(width: 64, height: 64)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(.black.opacity(0.08), lineWidth: 0.5),
            )

            }.buttonStyle(.plain).accessibilityLabel("Preview \(attachment.filename)")
                .accessibilityIdentifier("attachment-preview")

            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(.white, .black.opacity(0.55))
                    .font(.system(size: 18))
                    .frame(width: 44, height: 44).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .offset(x: 12, y: -12)
            .accessibilityLabel("Remove \(attachment.filename)")
        }
    }

    /// Slack-style file row: paperclip + filename + size + remove. Used for
    /// non-image attachments (pasted text-as-file, picked documents, etc.).
    private var fileVariant: some View {
        HStack(spacing: 6) {
            Button { showPreview = true } label: {
            HStack(spacing: 6) {
            Image(systemName: "doc")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(attachment.filename)
                .font(.caption)
                .lineLimit(1)
                .truncationMode(.middle)
            Text(formattedSize)
                .font(.caption2)
                .foregroundStyle(.secondary)
            }.frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityLabel("Preview \(attachment.filename)")
                .accessibilityIdentifier("attachment-preview")
            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .font(.caption)
                    .frame(width: 44, height: 44).contentShape(Rectangle())
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Remove \(attachment.filename)")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.regularMaterial, in: Capsule())
        .frame(maxWidth: 240)
    }

    private func loadThumbnailIfNeeded() {
        guard attachment.isImage, thumbnail == nil else { return }
        thumbnail = attachment.makeThumbnail(maxSide: 48)
    }

    private var formattedSize: String {
        let formatter = ByteCountFormatter()
        formatter.allowedUnits = [.useKB, .useMB, .useGB]
        formatter.countStyle = .file
        return formatter.string(fromByteCount: Int64(attachment.size))
    }
}

/// Horizontal pending-attachment row extracted from ChatView's input bar.
/// It owns removal only; transport/send state stays with the integrating composer.
struct AttachmentChipRow: View {
    @Binding var attachments: [PendingAttachment]

    var body: some View {
        if !attachments.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(attachments) { attachment in
                        AttachmentChip(attachment: attachment) {
                            attachments.removeAll { $0.id == attachment.id }
                        }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
        }
    }
}

/// System Quick Look previews the exact pending bytes before any model request.
private struct PendingAttachmentPreview: View {
    let attachment: PendingAttachment
    @Environment(\.dismiss) private var dismiss
    @State private var previewImage: UIImage?
    @State private var file: URL?
    @State private var directory: URL?
    @State private var failure: String?
    var body: some View {
        NavigationStack {
            Group {
                if let previewImage {
                    Image(uiImage: previewImage).resizable().scaledToFit()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .accessibilityLabel(attachment.filename).accessibilityIdentifier("attachment-image-preview")
                }
                else if let file { AttachmentQuickLook(file: file) }
                else if let failure { ContentUnavailableView("Preview unavailable", systemImage: "doc", description: Text(failure)) }
                else { ProgressView("Preparing preview…") }
            }
            .navigationTitle(attachment.filename).navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }.accessibilityIdentifier("attachment-preview-done")
            } }
        }
        .task {
            guard file == nil, directory == nil, previewImage == nil else { return }
            if attachment.isImage, let decoded = UIImage(data: attachment.data) {
                previewImage = decoded
                return
            }
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
            directory = folder
            do {
                try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
                let safeName = attachment.filename.replacingOccurrences(of: "[^A-Za-z0-9._-]", with: "-", options: .regularExpression)
                let target = folder.appendingPathComponent(safeName.isEmpty ? "Attachment" : safeName)
                // CodeRight-owned: atomic write to an arbitrary URL with a protection class (no shared consumer; RightKit issue withdrawn 2026-10-18)
                try attachment.data.write(to: target, options: [.atomic, .completeFileProtection])
                file = target
            } catch { failure = error.localizedDescription }
        }
        .onDisappear { if let directory { try? FileManager.default.removeItem(at: directory) } }
    }
}

private struct AttachmentQuickLook: UIViewControllerRepresentable {
    let file: URL
    func makeCoordinator() -> Coordinator { Coordinator(file: file) }
    func makeUIViewController(context: Context) -> QLPreviewController {
        let view = QLPreviewController()
        view.dataSource = context.coordinator
        return view
    }
    func updateUIViewController(_ view: QLPreviewController, context: Context) {}
    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        let file: URL
        init(file: URL) { self.file = file }
        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }
        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> any QLPreviewItem { file as NSURL }
    }
}
#endif
