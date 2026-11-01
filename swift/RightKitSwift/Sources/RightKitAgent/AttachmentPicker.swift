#if os(iOS)
// Provenance: operative picker/ingestion extraction from
// baryhuang/claude-code-by-agents at 7655042eb986bf146079032726a7347ac4547c0e,
// OpenAgents/Views/ChatView.swift, `inputBar`/`paperclipControl` lines 465–610
// and `ingestPhotoPickerItems`/`ingestFileURL` lines 884–937. The picker is
// exposed as a binding-driven iOS view so composer integration stays separate.

import Foundation
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// Converts native iOS picker URLs/selections into the shared pending model.
/// ImageDownsampler is applied at this boundary, matching every source ingest site.
enum AttachmentIngestor {
    /// Convert PhotosPicker selections into pending attachments. Raw image bytes
    /// keep the chip and eventual upload path identical to file/paste ingestion.
    @MainActor
    static func photoPickerAttachments(from items: [PhotosPickerItem]) async -> [PendingAttachment] {
        var attachments: [PendingAttachment] = []
        let stamp = Int(Date().timeIntervalSince1970)

        for (index, item) in items.enumerated() {
            do {
                guard let data = try await item.loadTransferable(type: Data.self) else { continue }
                let utType = item.supportedContentTypes.first
                let ext = utType?.preferredFilenameExtension ?? "jpg"
                let mime = utType?.preferredMIMEType ?? "image/jpeg"
                let (finalData, finalType, finalName) = ImageDownsampler.ensureFits(
                    data: data,
                    contentType: mime,
                    filename: "Photo-\(stamp)-\(index).\(ext)",
                )
                attachments.append(PendingAttachment(
                    filename: finalName,
                    contentType: finalType,
                    data: finalData,
                ))
            } catch {
                // A cancelled or unreadable selection is skipped, matching the
                // donor's per-item failure behavior without requiring a logger.
                continue
            }
        }
        return attachments
    }

    /// Read a security-scoped file-import URL and normalize image metadata.
    @MainActor
    static func fileAttachment(from url: URL) -> PendingAttachment? {
        let scoped = url.startAccessingSecurityScopedResource()
        defer {
            if scoped { url.stopAccessingSecurityScopedResource() }
        }
        guard let data = try? Data(contentsOf: url) else { return nil }
        let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
            ?? "application/octet-stream"
        let (finalData, finalType, finalName) = ImageDownsampler.ensureFits(
            data: data,
            contentType: mime,
            filename: url.lastPathComponent,
        )
        return PendingAttachment(
            filename: finalName,
            contentType: finalType,
            data: finalData,
        )
    }
}

/// iOS add control extracted from ChatView. Photos and Files use the same
/// pending-chip pipeline, while send/upload state remains with the integrating composer.
struct AttachmentPickerControl: View {
    @Binding var pendingAttachments: [PendingAttachment]

    @State private var showingPhotoPicker = false
    @State private var photoPickerItems: [PhotosPickerItem] = []
    @State private var showingFileImporter = false

    var body: some View {
        ZStack {
        Menu {
            Button {
                showingPhotoPicker = true
            } label: {
                Label("Photo Library", systemImage: "photo.on.rectangle.angled")
            }
            Button {
                showingFileImporter = true
            } label: {
                Label("Files", systemImage: "folder")
            }
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 20, weight: .regular))
                .foregroundStyle(Theme.text)
                .frame(width: ChatChrome.composerActionSize, height: ChatChrome.composerActionSize)
                .contentShape(Rectangle())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .accessibilityLabel("Add attachment")
        .accessibilityIdentifier("composer-add-attachment")
        }
        .photosPicker(
            isPresented: $showingPhotoPicker,
            selection: $photoPickerItems,
            maxSelectionCount: 10,
            matching: .images,
            photoLibrary: .shared(),
        )
        .onChange(of: photoPickerItems) { _, items in
            guard !items.isEmpty else { return }
            let picked = items
            photoPickerItems = []
            Task { @MainActor in
                let loaded = await AttachmentIngestor.photoPickerAttachments(from: picked)
                pendingAttachments.append(contentsOf: loaded)
            }
        }
        .sheet(isPresented: $showingFileImporter) {
            AttachmentDocumentPicker { urls in
                for url in urls {
                    if let attachment = AttachmentIngestor.fileAttachment(from: url) {
                        pendingAttachments.append(attachment)
                    }
                }
                showingFileImporter = false
            }
        }
    }
}

/// A dedicated system document picker avoids competing with the parent
/// project-folder importer's presentation state.
private struct AttachmentDocumentPicker: UIViewControllerRepresentable {
    let completion: ([URL]) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(completion: completion) }
    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: true)
        picker.allowsMultipleSelection = true
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}
    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        let completion: ([URL]) -> Void
        init(completion: @escaping ([URL]) -> Void) { self.completion = completion }
        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) { completion(urls) }
        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { completion([]) }
    }
}
#endif
