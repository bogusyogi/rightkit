#if os(iOS)
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The universal queue: paste a URL, add from Photos or Files, or share into
/// the app from anywhere — everything lands here and is processed by the same
/// serial worker.
struct QueueView: View {
    @EnvironmentObject private var queue: QueueStore
    @EnvironmentObject private var artifacts: ArtifactStore
    @EnvironmentObject private var send: SendCoordinator
    @State private var input = ""
    @State private var showImporter = false
    @State private var showPhotosPicker = false
    @State private var pickedPhotos: [PhotosPickerItem] = []
    /// The paste field is the primary control on this screen, so the keyboard
    /// needs a way out that isn't "submit something". Three, in fact: swipe
    /// the list, tap Done, or submit.
    @FocusState private var pasteFieldFocused: Bool

    var body: some View {
        NavigationStack {
            List {
                Section {
                    pasteBar
                        .listRowBackground(Color.srSurface)
                }
                Section {
                    if queue.jobs.isEmpty {
                        emptyState
                    } else {
                        ForEach(queue.jobs) { job in
                            row(for: job)
                                .listRowBackground(Color.srSurface)
                                .swipeActions {
                                    Button(role: .destructive) {
                                        queue.remove(job.id)
                                    } label: {
                                        Label("Delete", systemImage: "trash")
                                    }
                                    if job.stage == .error || job.stage == .cancelled {
                                        Button {
                                            queue.retry(job.id)
                                        } label: {
                                            Label("Retry", systemImage: "arrow.clockwise")
                                        }
                                    }
                                    if isCancellable(job.stage) {
                                        Button {
                                            queue.cancel(job.id)
                                        } label: {
                                            Label("Cancel", systemImage: "xmark.circle")
                                        }
                                    }
                                }
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            // Drag the list down to dismiss — the gesture iOS users already
            // expect from Messages and Mail.
            .scrollDismissesKeyboard(.interactively)
            .background(Color.srBackground)
            // The wordmark IS the title here. Rendering it as a list header
            // under a "Queue" navigation title stacked two headings and put
            // the brand mark below a generic noun.
            .navigationTitle("")
            .toolbar {
                ToolbarItem(placement: .principal) {
                    BrandLockup(size: 22)
                }
                // Explicit escape hatch above the keyboard. Without it the only
                // way out of the paste field was to submit something.
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button("Done") { pasteFieldFocused = false }
                        .font(.srBody(16, weight: .medium))
                }
            }
            .navigationDestination(for: Job.self) { job in
                QueueDetailView(job: job)
            }
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Menu {
                        Button {
                            showPhotosPicker = true
                        } label: {
                            Label("Photos & videos", systemImage: "photo.on.rectangle")
                        }
                        Button {
                            showImporter = true
                        } label: {
                            Label("Files", systemImage: "folder")
                        }
                    } label: {
                        Label("Add", systemImage: "plus")
                    }
                }
            }
            // Multi-select is the carousel path: pick every slide, get one
            // artifact per slide (desktop parity). Videos land here too, which
            // is how a saved reel gets transcribed without us touching the
            // platform.
            .fileImporter(
                isPresented: $showImporter,
                allowedContentTypes: [.pdf, .image, .audio, .movie],
                allowsMultipleSelection: true
            ) { result in
                guard case .success(let urls) = result else { return }
                // Computers nearby: stage copies and ask Process / Send to.
                if send.hasPeers {
                    let staged: [URL] = urls.compactMap { url in
                        let scoped = url.startAccessingSecurityScopedResource()
                        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                        return Self.stage(url)
                    }
                    if !staged.isEmpty { send.offer(files: staged) }
                    return
                }
                for url in urls {
                    let scoped = url.startAccessingSecurityScopedResource()
                    queue.addImportedFile(at: url)
                    if scoped { url.stopAccessingSecurityScopedResource() }
                }
            }
            .photosPicker(
                isPresented: $showPhotosPicker,
                selection: $pickedPhotos,
                maxSelectionCount: 20,
                matching: .any(of: [.images, .videos])
            )
            .onChange(of: pickedPhotos) { _, items in
                guard !items.isEmpty else { return }
                Task { await importPickedPhotos(items) }
            }
        }
    }

    private func isCancellable(_ stage: JobStage) -> Bool {
        switch stage {
        case .queued, .detecting, .downloading, .transcribing, .writing, .queuedOnMac: return true
        case .done, .error, .cancelled: return false
        }
    }

    /// A finished job links to its result in place; anything still working
    /// (or errored) stays a plain, non-tappable row.
    @ViewBuilder
    private func row(for job: Job) -> some View {
        if job.stage == .done {
            NavigationLink(value: job) { JobRow(job: job) }
        } else {
            JobRow(job: job)
        }
    }

    private var pasteBar: some View {
        HStack(spacing: 8) {
            TextField("Paste a link or article URL", text: $input)
                .font(.srBody(16))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .submitLabel(.go)
                .focused($pasteFieldFocused)
                .onSubmit(submit)
            Button(action: submit) {
                Image(systemName: "arrow.right.circle.fill")
                    .font(.title2)
            }
            .accessibilityLabel("Add to queue")
            .disabled(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Text out of anything.")
                .font(.srDisplay(22, weight: .semibold))
                .foregroundStyle(Color.srText)
            Text("Paste a link, import a PDF, image, or recording — or share into ScrapeRight from any app.")
                .font(.srBody(15))
                .foregroundStyle(Color.srMuted)
        }
        .padding(.vertical, 8)
        .listRowBackground(Color.clear)
    }

    private func submit() {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        queue.add(input: trimmed)
        input = ""
        // The job is queued and the field is empty — leaving the keyboard up
        // over a blank field just hides the row that was created.
        pasteFieldFocused = false
    }

    /// Photos items aren't file URLs — copy each into a temp file the queue can
    /// import, preserving the original extension so the runner routes it to
    /// OCR or ASR correctly.
    private func importPickedPhotos(_ items: [PhotosPickerItem]) async {
        let offerToPeers = send.hasPeers
        var staged: [URL] = []
        for item in items {
            guard let data = try? await item.loadTransferable(type: Data.self) else { continue }
            let ext = item.supportedContentTypes.first?.preferredFilenameExtension
                ?? (item.supportedContentTypes.first?.conforms(to: .movie) == true ? "mov" : "jpg")
            let name = "\(UUID().uuidString).\(ext)"
            let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
            guard (try? data.write(to: url)) != nil else { continue }
            if offerToPeers { staged.append(url); continue }
            queue.addImportedFile(at: url, originalName: name)
            try? FileManager.default.removeItem(at: url)
        }
        pickedPhotos = []
        if !staged.isEmpty { send.offer(files: staged) }
    }

    /// Copy a picked file out of its security scope so it can be handled after
    /// the user chooses Process or Send to. The original name is kept.
    private static func stage(_ url: URL) -> URL? {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("SendStaging-\(UUID().uuidString)", isDirectory: true)
        let destination = folder.appendingPathComponent(url.lastPathComponent)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try FileManager.default.copyItem(at: url, to: destination)
            return destination
        } catch {
            return nil
        }
    }
}

private struct JobRow: View {
    let job: Job

    /// A queued job has no title until it finishes, and falling back to the
    /// raw input meant rows read as full URLs that wrapped to two lines and
    /// couldn't be scanned. Derive something human from the URL instead, and
    /// keep the full input for the detail view.
    private var label: String { JobDisplay.label(for: job) }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.srBody(15, weight: .medium))
                .foregroundStyle(Color.srText)
                .lineLimit(2)
            HStack(spacing: 6) {
                StatusBadge(stage: job.stage)
                Text(job.kind.rawValue.replacingOccurrences(of: "_", with: " "))
                    .font(.srMono(11))
                    .foregroundStyle(Color.srMuted)
            }
            if job.stage == .queuedOnMac {
                Text(job.macDetail ?? "Queued on your Mac. It runs when your Mac is awake and online; the transcript appears here when it syncs back.")
                    .font(.srBody(13))
                    .foregroundStyle(Color.srMuted)
            }
            if let error = job.error {
                Text(error)
                    .font(.srBody(13))
                    .foregroundStyle(Color.srDanger)
            }
        }
        .padding(.vertical, 2)
    }
}

/// The result of a finished queue job, shown in place so you never have to
/// go hunting in Library: the original (image preview when the source was an
/// image), the transcript, and Share/Save-to-Files for both.
struct QueueDetailView: View {
    @EnvironmentObject private var queue: QueueStore
    @EnvironmentObject private var artifacts: ArtifactStore
    let job: Job

    private var artifact: Artifact? {
        job.artifactID.flatMap { artifacts.artifact(id: $0) }
    }

    private var originalURL: URL? { queue.importedFileURL(for: job) }

    private var text: String {
        artifact.map { artifacts.text(for: $0) } ?? ""
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                sectionLabel("Original")
                OriginalPreview(fileURL: originalURL, sourceInput: job.input)

                HStack(spacing: 8) {
                    CopyTextButton(text: text)
                        .font(.srBody(14, weight: .medium))
                    Spacer(minLength: 0)
                    if let engine = artifact?.asrEngine {
                        EngineBadge(engine: engine)
                    }
                }
                .padding(.top, 4)

                sectionLabel(originalURL == nil ? "Text" : "Extracted")
                if artifact != nil {
                    Text(text)
                        .font(.srMono(14))
                        .foregroundStyle(Color.srText)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    Text("This result is no longer available.")
                        .font(.srBody(14))
                        .foregroundStyle(Color.srMuted)
                }
            }
            .padding()
        }
        .background(Color.srBackground)
        .navigationTitle(job.title ?? "Result")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    CopyTextButton(text: text)
                    if let artifact {
                        ShareLink(item: artifacts.textFileURL(for: artifact)) {
                            Label("Save text (.txt)", systemImage: "doc.text")
                        }
                        ShareLink(
                            item: MarkdownExport(artifact: artifact, text: text),
                            preview: SharePreview(artifact.title)
                        ) {
                            Label("Save Markdown (.md)", systemImage: "doc.richtext")
                        }
                    }
                    if let originalURL {
                        ShareLink(item: originalURL) {
                            Label("Save original", systemImage: "square.and.arrow.down")
                        }
                    }
                } label: {
                    Image(systemName: "square.and.arrow.up")
                }
                .accessibilityLabel("Share or save")
            }
        }
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.srMono(11))
            .foregroundStyle(Color.srMuted)
    }
}

struct StatusBadge: View {
    let stage: JobStage

    var body: some View {
        Text(label)
            .font(.srMono(11))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.16), in: Capsule())
            .foregroundStyle(color)
    }

    private var label: String {
        stage == .queuedOnMac ? "QUEUED ON MAC" : stage.rawValue.uppercased()
    }

    private var color: Color {
        switch stage {
        case .done: return .srAccent
        case .error: return .srDanger
        case .queued, .cancelled: return .srMuted
        case .queuedOnMac: return .srAccent
        case .detecting, .downloading, .transcribing, .writing: return .srText
        }
    }
}
#endif
