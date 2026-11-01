#if os(iOS)
import SwiftUI
import UIKit

/// A scratchpad for what other devices send to this phone. Every item leaves
/// the list as soon as it is used or dismissed; nothing survives a relaunch.
/// Computers see the phone only while the app is open, so the empty state says so.
struct ReceiveView: View {
    @EnvironmentObject private var receive: ReceiveStore
    @EnvironmentObject private var send: SendCoordinator
    @EnvironmentObject private var queue: QueueStore
    @Environment(\.openURL) private var openURL
    @State private var sharing: ReceivedItem?
    @State private var exporting: ReceivedItem?
    @State private var confirmingSave: ReceivedItem?

    var body: some View {
        NavigationStack {
            List {
                ForEach(receive.offers) { offer in
                    OfferRow(offer: offer, accept: { send.accept(offer) }, decline: { send.decline(offer) })
                        .listRowBackground(Color.srSurface)
                }
                ForEach(receive.notices) { notice in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Label(notice.place == .photos ? "Saved to Photos" : "Saved to Files", systemImage: "checkmark")
                                .font(.srBody(15, weight: .medium))
                                .foregroundStyle(Color.srText)
                            if notice.place == .files {
                                Text(notice.name)
                                    .font(.srMono(11))
                                    .foregroundStyle(Color.srMuted)
                            }
                            if let message = notice.message {
                                Text(message)
                                    .font(.srBody(13))
                                    .foregroundStyle(Color.srMuted)
                            }
                        }
                        Spacer(minLength: 8)
                        Text("from \(notice.senderAlias)")
                            .font(.srMono(11))
                            .foregroundStyle(Color.srMuted)
                        Button { receive.removeNotice(notice.id) } label: { Image(systemName: "xmark") }
                            .buttonStyle(.borderless)
                            .accessibilityLabel("Dismiss")
                    }
                    .listRowBackground(Color.srSurface)
                }
                ForEach(receive.items) { item in
                    ReceivedRow(
                        item: item,
                        copy: { text in
                            UIPasteboard.general.string = text
                            UINotificationFeedbackGenerator().notificationOccurred(.success)
                            receive.remove(item.id)
                        },
                        open: { url in openURL(url); receive.remove(item.id) },
                        process: { url in queue.add(input: url.absoluteString); receive.remove(item.id) },
                        processText: { text in queue.addPasted(text); receive.remove(item.id) },
                        save: {
                            if item.needsSaveConfirmation { confirmingSave = item } else { exporting = item }
                        },
                        share: { sharing = item }
                    )
                    .listRowBackground(Color.srSurface)
                    .swipeActions {
                        Button(role: .destructive) {
                            receive.remove(item.id)
                        } label: {
                            Label("Dismiss", systemImage: "xmark")
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Color.srBackground)
            .navigationTitle("Receive")
            .confirmationDialog(
                "Save a file from an untrusted sender?",
                isPresented: Binding(get: { confirmingSave != nil }, set: { if !$0 { confirmingSave = nil } }),
                titleVisibility: .visible,
                presenting: confirmingSave
            ) { item in
                Button("Save anyway") { exporting = item }
                Button("Cancel", role: .cancel) {}
            } message: { item in
                Text("\(item.senderAlias) is not a trusted computer.")
            }
            .overlay {
                if receive.items.isEmpty && receive.offers.isEmpty && receive.notices.isEmpty {
                    ContentUnavailableView(
                        "Nothing received yet",
                        systemImage: "arrow.down.circle",
                        description: Text("Computers on this Wi-Fi can send text, links and files here while ScrapeRight is open.")
                    )
                }
            }
            .sheet(item: $sharing) { item in
                if let url = item.fileURL {
                    ActivitySheet(url: url) { completed in
                        if completed { receive.remove(item.id) }
                    }
                    .presentationDetents([.medium, .large])
                }
            }
            .sheet(item: $exporting) { item in
                if let url = item.fileURL {
                    ExportPicker(url: url) { completed in
                        if completed { receive.remove(item.id) }
                    }
                }
            }
        }
    }
}

/// A sender that is not proven and trusted: alias only, no content.
private struct OfferRow: View {
    let offer: IncomingOffer
    let accept: () -> Void
    let decline: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("\(offer.senderAlias) wants to send you something")
                .font(.srBody(15, weight: .medium))
                .foregroundStyle(Color.srText)
            Text("Not a trusted computer. Nothing is shown until you accept.")
                .font(.srBody(13))
                .foregroundStyle(Color.srMuted)
            HStack(spacing: 16) {
                Button("Accept", action: accept)
                Button("Decline", role: .destructive, action: decline)
            }
            .font(.srBody(14, weight: .medium))
            .buttonStyle(.borderless)
            .padding(.top, 2)
        }
        .padding(.vertical, 2)
    }
}

private struct ReceivedRow: View {
    let item: ReceivedItem
    let copy: (String) -> Void
    let open: (URL) -> Void
    let process: (URL) -> Void
    let processText: (String) -> Void
    let save: () -> Void
    let share: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(headline)
                .font(.srBody(15, weight: .medium))
                .foregroundStyle(Color.srText)
                .lineLimit(3)
            HStack(spacing: 6) {
                Text("from \(item.senderAlias)")
                Text(item.receivedAt.formatted(date: .abbreviated, time: .shortened))
            }
            .font(.srMono(11))
            .foregroundStyle(Color.srMuted)
            actions
                .font(.srBody(14, weight: .medium))
                .foregroundStyle(Color.srAccent)
                .buttonStyle(.borderless)
                .padding(.top, 2)
        }
        .padding(.vertical, 2)
    }

    private var headline: String {
        switch item.kind {
        case .text(let text): return text
        case .link(let url): return JobDisplay.label(forInput: url.absoluteString)
        case .file(_, let name, _, _), .picture(_, let name): return name
        }
    }

    @ViewBuilder
    private var actions: some View {
        switch item.kind {
        case .text(let text):
            HStack(spacing: 16) {
                Button { copy(text) } label: { Label("Copy", systemImage: "doc.on.doc") }
                Button { processText(text) } label: { Label("Process", systemImage: "text.alignleft") }
            }
        case .link(let url):
            HStack(spacing: 16) {
                Button { open(url) } label: { Label("Open", systemImage: "safari") }
                Button { process(url) } label: { Label("Process", systemImage: "text.alignleft") }
            }
        case .file, .picture:
            HStack(spacing: 16) {
                Button(action: save) { Label("Save", systemImage: "square.and.arrow.down") }
                Button(action: share) { Label("Share", systemImage: "square.and.arrow.up") }
                if case .file(_, _, let size, _) = item.kind {
                    Text(ByteCountFormatter.string(fromByteCount: size, countStyle: .file))
                        .font(.srMono(11))
                        .foregroundStyle(Color.srMuted)
                }
            }
        }
    }
}

/// System share sheet (Save to Files, Photos, AirDrop…) that reports whether
/// the user actually completed an action, so a cancelled share keeps the item.
private struct ActivitySheet: UIViewControllerRepresentable {
    let url: URL
    let finished: (Bool) -> Void

    func makeUIViewController(context: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        controller.completionWithItemsHandler = { _, completed, _, _ in finished(completed) }
        return controller
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

extension ReceivedItem {
    fileprivate var fileURL: URL? {
        switch kind {
        case .file(let url, _, _, _), .picture(let url, _): return url
        case .text, .link: return nil
        }
    }
}

/// System document export picker (Files). Reports whether a save happened, so
/// a cancelled export keeps the item. The destination is the user's choice
/// each time; no folder is remembered.
private struct ExportPicker: UIViewControllerRepresentable {
    let url: URL
    let finished: (Bool) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(finished: finished) }

    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forExporting: [url], asCopy: true)
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}

    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        let finished: (Bool) -> Void
        init(finished: @escaping (Bool) -> Void) { self.finished = finished }
        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) { finished(true) }
        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { finished(false) }
    }
}
#endif
