import Foundation
import Observation
import SwiftUI

/// Supplies optional language-server affordances without making editing depend on an LSP.
public protocol CodeEditorLanguageService: Sendable {
    /// Returns diagnostics produced by the language server for the current source.
    func diagnostics(for source: String, fileURL: URL) async throws -> [CodeEditorDiagnostic]
}

/// A diagnostic that can be presented alongside the source editor.
public struct CodeEditorDiagnostic: Equatable, Identifiable, Sendable {
    /// The severity used to choose a diagnostic's visual treatment.
    public enum Severity: String, Equatable, Sendable {
        /// A diagnostic that blocks or invalidates the source.
        case error
        /// A diagnostic that may need attention but does not block the source.
        case warning
        /// An informational diagnostic from the language service.
        case information
    }

    /// A stable identity for SwiftUI's diagnostic list.
    public let id: UUID
    /// The message supplied by the language service.
    public let message: String
    /// The one-based source line, when supplied by the language service.
    public let line: Int?
    /// The severity supplied by the language service.
    public let severity: Severity

    /// Creates a diagnostic returned by a language service.
    public init(
        id: UUID = UUID(),
        message: String,
        line: Int? = nil,
        severity: Severity
    ) {
        self.id = id
        self.message = message
        self.line = line
        self.severity = severity
    }
}

/// Edits a UTF-8 file and exposes language-server diagnostics when one is attached.
@MainActor
public struct CodeEditorView: View {
    private let fileURL: URL
    private let languageService: (any CodeEditorLanguageService)?
    @State private var editor: EditorState

    /// Creates an editor for a file URL; an absent service intentionally provides no syntax affordances.
    public init(
        fileURL: URL,
        languageService: (any CodeEditorLanguageService)? = nil
    ) {
        self.fileURL = fileURL
        self.languageService = languageService
        _editor = State(initialValue: EditorState(fileURL: fileURL, languageService: languageService))
    }

    public var body: some View {
        VStack(spacing: 0) {
            if editor.isLoading {
                ProgressView("Loading \(fileURL.lastPathComponent)…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let loadError = editor.loadError {
                VStack(spacing: 12) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.title2)
                    Text("Unable to load file")
                        .font(.headline)
                    Text(loadError)
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                    Button("Retry") {
                        Task { await editor.load() }
                    }
                }
                .padding()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                TextEditor(text: Binding(
                    get: { editor.source },
                    set: {
                        editor.source = $0
                        editor.saveError = nil
                        editor.diagnostics = []
                        editor.diagnosticsError = nil
                    }
                ))
                .font(.system(.body, design: .monospaced))
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
            }
        }
        .safeAreaInset(edge: .bottom) {
            VStack(spacing: 6) {
                if let saveError = editor.saveError {
                    Label("Save failed: \(saveError)", systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(.red)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                if let diagnosticsError = editor.diagnosticsError {
                    Label("Diagnostics unavailable: \(diagnosticsError)", systemImage: "exclamationmark.circle")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                HStack {
                    Text(fileURL.lastPathComponent)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Spacer()
                    if languageService != nil {
                        Button("Diagnostics") {
                            Task { await editor.refreshDiagnostics() }
                        }
                        .disabled(editor.isLoading || editor.isRefreshingDiagnostics)
                    }
                    Button(editor.isSaving ? "Saving…" : "Save") {
                        Task { await editor.save() }
                    }
                    .disabled(editor.isLoading || editor.isSaving || editor.loadError != nil)
                }
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
            .background(.bar)
        }
        .task(id: fileURL) {
            await editor.load()
        }
        .overlay(alignment: .top) {
            if !editor.diagnostics.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(editor.diagnostics) { diagnostic in
                        Label {
                            Text((diagnostic.line.map { "Line \($0): " } ?? "") + diagnostic.message)
                        } icon: {
                            Image(systemName: diagnostic.severity.symbolName)
                        }
                        .font(.footnote)
                        .foregroundStyle(diagnostic.severity.color)
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.thinMaterial)
            }
        }
    }
}

/// File IO failures cross a `Task.detached` boundary, so the failure type must
/// be `Sendable`. `String` does not conform to `Error` and `any Error` is not
/// `Sendable`, so the localized message is carried in a concrete type instead.
private struct EditorIOError: Error, Sendable {
    let message: String
}

@MainActor
@Observable
private final class EditorState {
    let fileURL: URL
    let languageService: (any CodeEditorLanguageService)?
    var source = ""
    var isLoading = true
    var isSaving = false
    var isRefreshingDiagnostics = false
    var loadError: String?
    var saveError: String?
    var diagnosticsError: String?
    var diagnostics: [CodeEditorDiagnostic] = []

    init(fileURL: URL, languageService: (any CodeEditorLanguageService)?) {
        self.fileURL = fileURL
        self.languageService = languageService
    }

    func load() async {
        isLoading = true
        loadError = nil
        let url = fileURL
        let result: Result<String, EditorIOError> = await Task.detached(priority: .userInitiated) {
            do {
                return .success(try String(contentsOf: url, encoding: .utf8))
            } catch {
                return .failure(EditorIOError(message: error.localizedDescription))
            }
        }.value

        guard !Task.isCancelled else { return }
        isLoading = false
        switch result {
        case let .success(source):
            self.source = source
            saveError = nil
            diagnostics = []
            diagnosticsError = nil
        case let .failure(failure):
            loadError = failure.message
        }
    }

    func save() async {
        guard !isSaving else { return }
        isSaving = true
        saveError = nil
        let url = fileURL
        let source = source
        let result: Result<Void, EditorIOError> = await Task.detached(priority: .userInitiated) {
            do {
                try source.write(to: url, atomically: true, encoding: .utf8)
                return .success(())
            } catch {
                return .failure(EditorIOError(message: error.localizedDescription))
            }
        }.value

        guard !Task.isCancelled else {
            isSaving = false
            return
        }
        isSaving = false
        if case let .failure(failure) = result {
            saveError = failure.message
        }
    }

    func refreshDiagnostics() async {
        guard let languageService else { return }
        isRefreshingDiagnostics = true
        diagnosticsError = nil
        let source = source
        do {
            diagnostics = try await languageService.diagnostics(for: source, fileURL: fileURL)
        } catch {
            diagnostics = []
            diagnosticsError = error.localizedDescription
        }
        isRefreshingDiagnostics = false
    }
}

private extension CodeEditorDiagnostic.Severity {
    var symbolName: String {
        switch self {
        case .error: return "xmark.octagon.fill"
        case .warning: return "exclamationmark.triangle.fill"
        case .information: return "info.circle.fill"
        }
    }

    var color: Color {
        switch self {
        case .error: return .red
        case .warning: return .orange
        case .information: return .secondary
        }
    }
}
