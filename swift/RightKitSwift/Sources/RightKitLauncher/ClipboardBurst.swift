#if os(macOS)
import RightKitNotchCore
import RightKitDesktopTweaks
import RightKitNotch
import RightKitUI
import AppKit
import RightKitAccessibility
import RightKitOverlay
import SwiftUI

// Copy burst (bogusyogi/pulse#387, supersedes the explicit paste stack of #376).
//
// A second copy landing under 60 s after the previous one starts a burst. Its items show as
// dots on the Send cell, in copy order; hovering a dot previews the item; while the burst lives a
// plain Cmd+V pastes the oldest item that has not been pasted (Fn+V too: the Fn tap turns it into
// Cmd+V ahead of this one), and each pasted item drops its dot. Cmd-click (Fn-click when Fn acts as
// Cmd) selects several, and the next Cmd+V pastes them together. The burst ends when the last item
// is pasted, after 5 minutes with no copy or paste, or when it is dismissed with the small x.
//
// The paste itself happens on the tap's thread: the pasteboard is written before the key reaches the
// app, and the key is passed through. Everything the tap needs is cached beforehand in
// `ClipBurstQueue`, so the tap never waits on the main thread.

typealias ClipBurstPart = (type: NSPasteboard.PasteboardType, data: Data)

/// What the tap thread and the main thread share, behind one lock.
final class ClipBurstQueue: @unchecked Sendable {
    private struct Entry {
        let id: String
        let items: [[ClipBurstPart]]
    }

    private let lock = NSLock()
    private var entries: [Entry] = []
    private var selected: Set<String> = []
    private var generation = 0

    var ids: [String] { lock.lock(); defer { lock.unlock() }; return entries.map { $0.id } }
    var selection: Set<String> { lock.lock(); defer { lock.unlock() }; return selected }
    var isEmpty: Bool { lock.lock(); defer { lock.unlock() }; return entries.isEmpty }
    var currentGeneration: Int { lock.lock(); defer { lock.unlock() }; return generation }

    func contains(_ id: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return entries.contains { $0.id == id }
    }

    func reset(_ list: [(id: String, items: [[ClipBurstPart]])]) {
        lock.lock()
        entries = list.map { Entry(id: $0.id, items: $0.items) }
        selected = []
        generation += 1
        lock.unlock()
    }

    func append(_ id: String, _ items: [[ClipBurstPart]]) {
        lock.lock()
        entries.append(Entry(id: id, items: items))
        generation += 1
        lock.unlock()
    }

    func toggle(_ id: String) {
        lock.lock()
        if entries.contains(where: { $0.id == id }) {
            if selected.contains(id) { selected.remove(id) } else { selected.insert(id) }
        }
        generation += 1
        lock.unlock()
    }

    /// What the next paste puts on the pasteboard: the selection in copy order, else the oldest item.
    func nextBatch() -> [[ClipBurstPart]]? {
        lock.lock(); defer { lock.unlock() }
        return Self.batch(entries, selected)?.items
    }

    /// The newest copy, for when a burst ends with items left.
    func newest() -> [[ClipBurstPart]]? {
        lock.lock(); defer { lock.unlock() }
        return entries.last.map { Self.combine([$0.items]) }
    }

    /// The paste, on the tap thread: writes the pasteboard with what is due, drops those items and
    /// returns the new generation, or nil when nothing is queued. Called before the key reaches the app.
    func pasteNow() -> Int? {
        lock.lock(); defer { lock.unlock() }
        guard let batch = Self.batch(entries, selected) else { return nil }
        guard Self.write(batch.items) else { return nil }
        entries.removeAll { batch.ids.contains($0.id) }
        selected = []
        generation += 1
        return generation
    }

    private static func batch(_ entries: [Entry], _ selected: Set<String>) -> (ids: Set<String>, items: [[ClipBurstPart]])? {
        guard !entries.isEmpty else { return nil }
        let due = selected.isEmpty ? [entries[0]] : entries.filter { selected.contains($0.id) }
        guard !due.isEmpty else { return nil }
        return (Set(due.map { $0.id }), combine(due.map { $0.items }))
    }

    /// Images and files stay separate pasteboard items; text only is joined with newlines.
    static func combine(_ lists: [[[ClipBurstPart]]]) -> [[ClipBurstPart]] {
        if lists.count == 1 { return lists[0] }
        let textOnly = lists.allSatisfy { list in
            list.count == 1 && list[0].contains { $0.type == .string }
                && !list[0].contains { $0.type == .png || $0.type == .fileURL }
        }
        if textOnly {
            let joined = lists.compactMap { list in
                list[0].first { $0.type == .string }.flatMap { String(data: $0.data, encoding: .utf8) }
            }.joined(separator: "\n")
            return [[(type: .string, data: Data(joined.utf8))]]
        }
        return lists.flatMap { $0 }
    }

    /// Replaces the general pasteboard, marked as Pulse's own so the history does not store it again.
    /// Safe on any thread.
    @discardableResult
    static func write(_ items: [[ClipBurstPart]]) -> Bool {
        guard !items.isEmpty else { return false }
        var objects: [NSPasteboardItem] = []
        for (index, parts) in items.enumerated() {
            let object = NSPasteboardItem()
            for part in parts { object.setData(part.data, forType: part.type) }
            if index == 0 {
                object.setString(Bundle.main.bundleIdentifier ?? "dev.orthic.pulse", forType: LauncherClipboard.sourceType)
                object.setString("1", forType: LauncherClipboard.ownType)
            }
            objects.append(object)
        }
        let board = NSPasteboard.general
        board.clearContents()
        return board.writeObjects(objects)
    }
}

extension LauncherClipboard {
    private static let burstMaxBytes = 32 * 1024 * 1024

    /// The item as pasteboard items, every format it has (the same formats `write` offers), or nil
    /// when its payload cannot be read or is too large to hold ready.
    func burstParts(for item: ClipItem) -> [[ClipBurstPart]]? {
        guard item.size <= Self.burstMaxBytes else { return nil }
        switch item.kind {
        case .text, .link, .colour, .code:
            guard let body = text(for: item) else { return nil }
            return [[(type: .string, data: Data(body.utf8))]]
        case .rich:
            guard let body = text(for: item) else { return nil }
            var parts: [ClipBurstPart] = []
            if let html = payload(of: item, "html") { parts.append((type: .html, data: html)) }
            if let rtf = payload(of: item, "rtf") { parts.append((type: .rtf, data: rtf)) }
            parts.append((type: .string, data: Data(body.utf8)))
            return [parts]
        case .image:
            guard let png = imageData(for: item) else { return nil }
            return [[(type: .png, data: png)]]
        case .file:
            let paths = files(for: item)
            guard !paths.isEmpty else { return nil }
            return paths.map { [(type: .fileURL, data: Data(URL(fileURLWithPath: $0).absoluteString.utf8))] }
        }
    }
}

/// The burst: the dots on the Send cell, and the tap that pastes them.
@MainActor
final class ClipBurst: ObservableObject {
    static let shared = ClipBurst()

    struct Dot: Identifiable, Equatable {
        let id: String
        var selected: Bool
    }

    /// Oldest first; one per item not yet pasted. Empty when there is no burst.
    @Published private(set) var dots: [Dot] = []

    private static let windowSeconds: Double = 60
    private static let idleSeconds: TimeInterval = 300

    private let queue = ClipBurstQueue()
    private var hub: EventTapHub?
    private var token: EventTapToken?
    private var lastCopy: (id: String, at: Double)?
    private var idleWork: DispatchWorkItem?
    private var restageWork: DispatchWorkItem?

    var isActive: Bool { hub != nil }

    // MARK: Copies

    /// The history captured a copy (a new item, or an old one copied again).
    func noteCopy(id: String, at: Double) {
        guard let store = LauncherClipboard.current, store.settings.enabled, store.settings.copyBurst,
              !Runtime.isUnderTest else { return }
        let previous = lastCopy
        lastCopy = (id, at)
        if isActive {
            // A copy of something still waiting is the same dot; anything else joins the end.
            if !queue.contains(id), let item = store.item(id: id), let parts = store.burstParts(for: item) {
                queue.append(id, parts)
                changed(store)
            } else {
                touch()
            }
            return
        }
        guard let previous, previous.id != id, at - previous.at >= 0, at - previous.at < Self.windowSeconds,
              let first = store.item(id: previous.id), let firstParts = store.burstParts(for: first),
              let second = store.item(id: id), let secondParts = store.burstParts(for: second),
              startTap() else { return }
        queue.reset([(previous.id, firstParts), (id, secondParts)])
        changed(store)
    }

    // MARK: Dots

    /// A click on a dot: with Cmd (or Fn when Fn acts as Cmd) it adds or removes the dot from the selection.
    func click(_ id: String, modifiers: NSEvent.ModifierFlags) {
        let fnActsAsCommand = UserDefaults.standard.bool(forKey: GeneratedSettings.Key.convFnCommand)
        guard modifiers.contains(.command) || (fnActsAsCommand && modifiers.contains(.function)),
              let store = LauncherClipboard.current else { return }
        queue.toggle(id)
        changed(store)
    }

    /// The small x: the burst is over, and Cmd+V is normal again.
    func dismiss() { end(leaveNewest: true) }

    /// The item a dot stands for, for the hover preview.
    func item(for id: String) -> ClipItem? { LauncherClipboard.current?.item(id: id) }

    // MARK: The tap

    private func startTap() -> Bool {
        guard hub == nil, RightKitAXTrust.isTrusted() else { return hub != nil }
        let hub = EventTapHub(location: .cgSessionEventTap, events: [.keyDown])
        guard hub.start() else { return false }
        let queue = queue
        token = hub.register(priority: 0) { type, event in
            guard type == .keyDown,
                  event.getIntegerValueField(.keyboardEventKeycode) == 9,
                  event.flags.contains(.maskCommand),
                  event.flags.intersection([.maskShift, .maskAlternate, .maskControl]).isEmpty,
                  event.getIntegerValueField(.keyboardEventAutorepeat) == 0,
                  let generation = queue.pasteNow()
            else { return .pass }
            DispatchQueue.main.async {
                MainActor.assumeIsolated { ClipBurst.shared.pasted(generation) }
            }
            return .pass
        }
        self.hub = hub
        return true
    }

    private func stopTap() {
        if let token { hub?.unregister(token) }
        hub?.stop()
        hub = nil
        token = nil
    }

    /// Main thread, after the tap pasted: the dots follow the queue, and the next item goes on the
    /// pasteboard a moment later, once the app has read this one.
    private func pasted(_ generation: Int) {
        guard isActive, let store = LauncherClipboard.current else { return }
        refresh()
        touch()
        ClipBurstPreview.shared.hide()
        if queue.isEmpty {
            end(leaveNewest: false)
            return
        }
        restageWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.isActive, self.queue.currentGeneration == generation else { return }
                self.stage(store)
            }
        }
        restageWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: work)
    }

    // MARK: State

    private func changed(_ store: LauncherClipboard) {
        refresh()
        stage(store)
        touch()
    }

    /// The pasteboard holds what the next paste will give, so a menu Paste matches Cmd+V.
    private func stage(_ store: LauncherClipboard) {
        guard let batch = queue.nextBatch(), ClipBurstQueue.write(batch) else { return }
        store.noteOwnWrite()
    }

    private func refresh() {
        let selected = queue.selection
        dots = queue.ids.map { Dot(id: $0, selected: selected.contains($0)) }
    }

    private func touch() {
        idleWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.end(leaveNewest: true) }
        }
        idleWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.idleSeconds, execute: work)
    }

    /// Ends the burst. With items still waiting, the pasteboard goes to the newest copy, which is what
    /// the person last copied.
    func end(leaveNewest: Bool) {
        guard isActive else { return }
        idleWork?.cancel(); idleWork = nil
        restageWork?.cancel(); restageWork = nil
        stopTap()
        if leaveNewest, let items = queue.newest(), ClipBurstQueue.write(items) {
            LauncherClipboard.current?.noteOwnWrite()
        }
        queue.reset([])
        dots = []
        ClipBurstPreview.shared.hide()
    }
}

// MARK: - The dots

/// The dots around the Send ring: one per item, in copy order from the top, clockwise; the x before the
/// first. A single copy shows nothing.
struct ClipBurstDots: View {
    @ObservedObject private var burst = ClipBurst.shared

    private let spacing: Double = 12
    private let dotSize: CGFloat = 6

    var body: some View {
        let radius = NotchLayout.ringDiameter / 2 + 7
        ZStack {
            if !burst.dots.isEmpty {
                ForEach(Array(burst.dots.enumerated()), id: \.element.id) { index, dot in
                    let angle = Angle.degrees(-80 + spacing * Double(index))
                    Circle()
                        .fill(dot.selected ? Palette.textPrimary : Palette.textPrimary.opacity(0.55))
                        .overlay(Circle().strokeBorder(Palette.textPrimary, lineWidth: dot.selected ? 1.5 : 0).padding(-2))
                        .frame(width: dotSize, height: dotSize)
                        .contentShape(Circle().inset(by: -5))
                        .offset(x: radius * CGFloat(cos(angle.radians)), y: radius * CGFloat(sin(angle.radians)))
                        .onHover { inside in
                            if inside { ClipBurstPreview.shared.show(dot.id) } else { ClipBurstPreview.shared.hide(dot.id) }
                        }
                        .onTapGesture { burst.click(dot.id, modifiers: NSEvent.modifierFlags) }
                        .accessibilityLabel(Text(verbatim: "\(index + 1)"))
                }
                let corner = Angle.degrees(-80 - spacing)
                Button { burst.dismiss() } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 6, weight: .bold))
                        .foregroundStyle(Palette.textPrimary.opacity(0.7))
                        .frame(width: 10, height: 10)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .offset(x: radius * CGFloat(cos(corner.radians)), y: radius * CGFloat(sin(corner.radians)))
                .accessibilityLabel(Text(verbatim: L10n.t("Dismiss copy burst")))
            }
        }
        .frame(width: NotchLayout.ringDiameter, height: NotchLayout.ringDiameter)
    }
}

// MARK: - The hover preview

/// A small card beside the pointer: the picture, or the first lines of the text. It never touches the
/// notch, takes no mouse events, and stays out of screenshots when the clipboard switch says so.
@MainActor
final class ClipBurstPreview {
    static let shared = ClipBurstPreview()

    private var panel: RightKitOverlayPanel?
    private var shownID: String?

    func show(_ id: String) {
        guard let store = LauncherClipboard.current, let item = store.item(id: id) else { return }
        shownID = id
        let host = NSHostingView(rootView: ClipBurstPreviewCard(item: item, image: store.thumbnail(for: item)))
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
        let panel = self.panel ?? {
            // The old NSPanel hid on deactivate (its default); the package's default is false.
            let made = RightKitOverlayPanel(contentRect: .zero, configuration: RightKitOverlayPanelConfiguration(
                collectionBehavior: RightKitOverlayPanelConfiguration.transientFullScreenAuxiliary,
                hasShadow: true,
                startsClickThrough: true,
                hidesOnDeactivate: true))
            self.panel = made
            return made
        }()
        panel.sharingType = store.settings.hideFromCapture ? .none : .readOnly
        panel.contentView = host
        let size = host.fittingSize
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main
        var origin = NSPoint(x: mouse.x - size.width / 2, y: mouse.y - size.height - 18)
        if let visible = screen?.visibleFrame {
            if origin.y < visible.minY { origin.y = mouse.y + 18 }
            origin.x = min(max(origin.x, visible.minX + 4), visible.maxX - size.width - 4)
        }
        panel.setFrame(NSRect(origin: origin, size: size), display: true)
        panel.orderFrontRegardless()
    }

    func hide(_ id: String? = nil) {
        if let id, shownID != id { return }
        shownID = nil
        panel?.orderOut(nil)
    }
}

private struct ClipBurstPreviewCard: View {
    let item: ClipItem
    let image: NSImage?

    var body: some View {
        Group {
            if item.kind == .image, let image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
                    .frame(maxWidth: 220, maxHeight: 160)
            } else if item.kind == .image {
                Text(verbatim: item.preview).lineLimit(1)
            } else {
                Text(verbatim: String(item.preview.prefix(240)))
                    .lineLimit(6)
                    .frame(maxWidth: 240, alignment: .leading)
            }
        }
        .font(.system(size: 11))
        .foregroundStyle(Color.white)
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.black.opacity(0.92)))
        .fixedSize()
    }
}
#endif
