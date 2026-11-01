#if os(iOS)
import SwiftUI
import UIKit

/*
 Provenance (read-only donor): OpenMausBot @ 6951c40156ba0916a10b6e49e5051b7785198b93,
 `ios/App/ChatView.swift:298-300` (`.sheet(item:)`), `:498-517`
 (Markdown/JSON actions), and `:830-843` (`UIActivityViewController`).
 The view below keeps donor labels, item-based sheet lifecycle, and native
 activity sharing while accepting CodeRight's thin local snapshot protocol.
*/

public struct TranscriptActivityShareSheet: UIViewControllerRepresentable {
    public let items: [Any]

    public init(items: [Any]) {
        self.items = items
    }

    public func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    public func updateUIViewController(
        _ controller: UIActivityViewController,
        context: Context
    ) {}
}

/// Reusable donor-shaped share menu. Integration only needs to provide a
/// snapshot; session/network lifecycle remains outside this port.
public struct TranscriptExportMenu: View {
    private let input: any TranscriptExportInput
    private let writer: TranscriptExportWriter
    @State private var shareFile: CodeRightTranscriptShareFile?
    @State private var exportFailure: ExportFailure?
    @State private var exportInProgress = false

    public init(
        input: any TranscriptExportInput,
        directory: URL = FileManager.default.temporaryDirectory,
        fileManager: FileManager = .default
    ) {
        self.input = input
        writer = TranscriptExportWriter(directory: directory, fileManager: fileManager)
    }

    public var body: some View {
        Menu {
            Button {
                export(.markdown)
            } label: {
                Label("Share as Markdown", systemImage: "doc.text")
            }
            .disabled(exportInProgress)

            Button {
                export(.json)
            } label: {
                Label("Share as JSON", systemImage: "curlybraces")
            }
            .disabled(exportInProgress)
        } label: {
            Image(systemName: "square.and.arrow.up")
        }
        .disabled(exportInProgress)
        .accessibilityLabel("Share transcript")
        .accessibilityHint("Choose Markdown or JSON")
        .sheet(item: $shareFile) { file in
            TranscriptActivityShareSheet(items: [file.url])
        }
        .alert(item: $exportFailure) { failure in
            Alert(
                title: Text("Couldn't export transcript"),
                message: Text(failure.message),
                dismissButton: .default(Text("OK"))
            )
        }
    }

    private func export(_ format: TranscriptExportFormat) {
        exportFailure = nil
        exportInProgress = true
        defer { exportInProgress = false }
        do {
            shareFile = try writer.write(input: input, format: format)
        } catch {
            exportFailure = ExportFailure(message: error.localizedDescription)
        }
    }

    private struct ExportFailure: Identifiable {
        let message: String

        var id: String { message }
    }
}
#endif
