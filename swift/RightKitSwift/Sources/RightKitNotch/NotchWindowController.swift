#if os(macOS)
import AppKit
import RightKitOverlay
import SwiftUI
import Combine

@MainActor
final class NotchWindowController {
    let model = NotchViewModel()
    var displayPreference: DisplayPreference = .followActiveWindow

    /// Hooked up by the app delegate; drives the menu's "Refresh now".
    var onRefresh: (() -> Void)?
    /// The notch has just opened, or a ring has just been pointed at — so the
    /// numbers on it are about to be read. Distinct from `onRefresh`, which is
    /// **Refresh now** and always fetches; this one may decide it has fetched
    /// recently enough. See `UsageStore.refreshBecauseSomeoneIsLooking`.
    var onLook: (() -> Void)?
    /// Pulse fork: the provider whose hover card is open on this notch, or nil
    /// when none is. The app uses it to read System detail (temperatures, fans)
    /// only while that card is showing.
    var onHoverCard: ((String?) -> Void)?
    /// The notice that was up has gone, by its deadline or its ×: the fleet
    /// brings up the next one that waited.
    var onNoticeGone: (() -> Void)?
    /// One "Sign in to …" item per provider that needs a browser session.
    var signInItems: [(title: String, action: () -> Void)] = []
    /// Driven by the notch's own chrome.

    /// Refetch a single provider, asked for by clicking its ring.
    var onRefreshProvider: ((String) async -> Void)?
    /// Open the settings window, asked for by clicking the handle.
    var onOpenSettings: (() -> Void)?
    var onOpenHub: ((String) -> Void)?
    /// An ⌥-drag on the pill settled at a new `model.alongOffset`. The
    /// controller only holds the live value; persisting it per edge is
    /// Preferences' job, the same division `apply(edge:)` already keeps.
    var onReposition: ((CGFloat) -> Void)?
    /// A move settled on a new edge — and, carried there by an ⌥-drag, where
    /// along it it was let go, as that edge's offset; nil keeps the offset the
    /// edge remembers. The fleet owns writing that to preferences, for the
    /// same reason it owns `onReposition`.
    var onMoveToEdge: ((NotchEdge, CGFloat?) -> Void)?
    /// The minimise button was clicked: the new standing state, for Preferences to keep.
    var onMinimizedChange: ((Bool) -> Void)?

    private var panel: NotchPanel?
    private var hostingView: NotchHostingView<NotchRootView>?

    /// The display this notch belongs to. Nil follows the menu-bar screen,
    /// which is what a single-controller setup did before the fleet existed —
    /// so leaving it unset changes nothing.
    var assignedScreen: NSScreen?
    private var cancellables = Set<AnyCancellable>()
    private var clearHoverWork: DispatchWorkItem?
    /// The one deadline a notice has, and how it is held while the pointer is
    /// on the card: what was left when the hold began.
    private var noticeWork: DispatchWorkItem?
    private var noticeLeft: TimeInterval = 0
    private var noticeRunStart: Date?
    private var clockTimer: Timer?
    /// The pointer watch: mouse moves plus a poll, one tracker at a time. See `scheduleCursorPoll`.
    private var cursorTracker: RightKitOverlayPointerTracker?
    /// Full-screen state on its own, slower beat. See `startWatchingFullScreen`.
    private var fullScreenTimer: Timer?
    private var fullScreenFollowUp: DispatchWorkItem?
    static let fullScreenPollInterval: TimeInterval = 5

    /// Hover in is quick; hover out waits, because the pointer has to cross the
    /// gap between the notch and the card without the card vanishing under it.
    private let hoverGrace: TimeInterval = 0.12
    /// Longer than the hover grace: folding shut is a bigger movement than
    /// dismissing a tooltip, and doing it the instant the pointer strays feels
    /// twitchy rather than responsive.
    private let foldGrace: TimeInterval = 0.25
    private var foldWork: DispatchWorkItem? {
        didSet {
            // A fold that is not going to happen after all — pinned, peeked,
            // hovered again, dragged: the arcs that set off home come back out.
            if foldWork == nil, model.isExpanded, model.handlesTuckedAway {
                model.handlesTuckedAway = false
            }
        }
    }
    /// Folds the notch again after a peek, when nothing else is holding it open.
    private var peekWork: DispatchWorkItem?
    /// The session a peek is currently offering, and how long the offer lasts.
    ///
    /// A click on the open notch normally pins it or refetches a ring; while
    /// this is set and unexpired it jumps to the session instead. The expiry is
    /// what keeps the two apart — without it, the *next* click on the notch,
    /// minutes later and about something else, would still be raising a
    /// terminal window.
    private var pendingFocus: (pid: pid_t, until: Date)?
    /// When the current peek's five seconds are up.
    ///
    /// The hover fold has to be told to leave it alone until then. Without
    /// this the cursor poll — which runs every 0.3s and asks "is the pointer on
    /// the notch?", to which the answer during a peek is almost always no —
    /// scheduled a fold immediately, and the notch opened and shut inside a
    /// second. A peek is not the pointer arriving, so the pointer leaving is
    /// not what should end it.
    private var peekUntil: Date?
    /// The standing visibility choice, so a peek never overrides Hidden.
    private var visibility: NotchVisibility = .onHover
    /// Whether we have pushed the pointing hand onto the cursor stack.
    private var shownCursor: NSCursor?
    /// Option-drag moves the whole notch under the pointer. Hovering rings
    /// while that happens is accidental — the pointer necessarily crosses
    /// them as the panel follows it — so cursor tracking is suspended until
    /// the drag ends.
    private var isOptionDragging = false

    /// Determines whether a full-screen application window is active on this notch's display.
    /// Default implementation queries WindowServer and NSWorkspace; overridable for testing.
    lazy var isFullScreenActive: () -> Bool = { [weak self] in
        self?.fullScreenReading() ?? false
    }

    /// The last answer about full screen. The cursor never asks WindowServer:
    /// it reads this. The answer is refreshed off the main thread (the window
    /// list is a blocking IPC) on a space, app or screen change and on the
    /// 2 s safety timer — see `refreshFullScreen`.
    private var fullScreenCache: (screen: NSScreen?, value: Bool)?
    private var fullScreenRefreshInFlight = false
    private var fullScreenRefreshQueued = false

    private func fullScreenReading() -> Bool {
        let screen = currentScreen()
        if fullScreenCache?.screen !== screen { refreshFullScreen() }
        return fullScreenCache?.value ?? false
    }

    /// Asks again, off the main thread; one call in flight at a time.
    private func refreshFullScreen() {
        guard !fullScreenRefreshInFlight else {
            fullScreenRefreshQueued = true
            return
        }
        let screen = currentScreen()
        guard let context = FullScreenDetector.context(on: screen) else {
            publishFullScreen(false, screen: screen)
            return
        }
        if context.hiddenMenuBarFullScreen {
            publishFullScreen(true, screen: screen)
            return
        }
        fullScreenRefreshInFlight = true
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let value = FullScreenDetector.evaluate(context, windows: FullScreenDetector.onScreenWindows())
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.finishFullScreenRefresh(value, screen: screen) }
            }
        }
    }

    private func finishFullScreenRefresh(_ value: Bool, screen: NSScreen?) {
        fullScreenRefreshInFlight = false
        publishFullScreen(value, screen: screen)
        if fullScreenRefreshQueued {
            fullScreenRefreshQueued = false
            refreshFullScreen()
        }
    }

    private func publishFullScreen(_ value: Bool, screen: NSScreen?) {
        let changed = fullScreenCache?.value != value
        fullScreenCache = (screen, value)
        // The fold follows the answer as soon as it lands, not on the next beat.
        if changed { handleActiveSpaceOrAppChange() }
    }

    /// Whether a frontmost full-screen app may fold the notch at all. A
    /// setting rather than a rule: on a screen kept full-screen all day the
    /// fold reads as the notch refusing to stay put, not as it tidying up.
    var foldsForFullScreen = true

    /// Pulse fork: whether a full-screen app is currently folding this notch
    /// away. The fleet reads it to slow the System and Disks sampling while
    /// nobody can see them.
    private(set) var isFoldedForFullScreen = false
    var onFoldChange: (() -> Void)?

    /// When a full-screen app is active on the current space, auto-folds the notch.
    /// When returning to a desktop space with `isAlwaysOn`, restores the unfolded state.
    func handleActiveSpaceOrAppChange() {
        let folded = foldsForFullScreen && isFullScreenActive()
        if folded != isFoldedForFullScreen {
            isFoldedForFullScreen = folded
            onFoldChange?()
        }
        if folded && !model.isPinned {
            if let panel {
                let local = localCursor(in: panel.frame)
                let overTooltip = model.hoveredIndex
                    .flatMap(tooltipRect(index:))
                    .map { model.isExpanded && $0.contains(local) } ?? false
                if liveRect.contains(local) || overTooltip {
                    return
                }
            }
            foldForFullScreen()
        } else if (model.isAlwaysOn || model.isPinned) && !model.isExpanded {
            withAnimation(NotchMotion.unfold) {
                model.isExpanded = true
            }
            updateInteractiveRects()
        }
    }

    /// Immediately folds the notch and clears pending hover timers when a full-screen app takes focus.
    func foldForFullScreen() {
        if let peekUntil, peekUntil > Date() { return }
        foldWork?.cancel()
        foldWork = nil
        guard model.isExpanded else { return }
        withAnimation(NotchMotion.unfold) {
            model.isExpanded = false
            model.hoveredIndex = nil
        }
        setPointing(false)
        updateInteractiveRects()
    }

    /// Re-evaluated on the spot rather than on the next cursor poll, so the
    /// notch answers the setting in the same beat: switched off under a
    /// frontmost full-screen app, an always-on notch comes straight back.
    func apply(foldsForFullScreen: Bool) {
        self.foldsForFullScreen = foldsForFullScreen
        // The full-screen poll is a WindowServer round trip every 2 s; with the fold off it
        // has nothing to decide, so it is not running (#148).
        if foldsForFullScreen {
            if fullScreenTimer == nil, cursorTracker != nil { startWatchingFullScreen() }
        } else {
            fullScreenTimer?.invalidate()
            fullScreenTimer = nil
        }
        if !foldsForFullScreen {
            // A fold already in flight captured ignoreAlwaysOn and would land
            // once more against an always-on notch, even as the setting that
            // caused it is being switched off.
            foldWork?.cancel()
            foldWork = nil
        }
        handleActiveSpaceOrAppChange()
    }

    func show() {
        relocate()
        startWatchingCursor()
        startWatchingFullScreen()
        startClock()

        NotificationCenter.default.publisher(
            for: NSApplication.didChangeScreenParametersNotification
        )
        .sink { [weak self] _ in
            MainActor.assumeIsolated {
                self?.relocate()
                self?.fullScreenMayHaveChanged()  // screen arrangement changed
            }
        }
        .store(in: &cancellables)

        NSWorkspace.shared.notificationCenter.publisher(
            for: NSWorkspace.activeSpaceDidChangeNotification
        )
        .sink { [weak self] _ in
            MainActor.assumeIsolated { self?.fullScreenMayHaveChanged() }
        }
        .store(in: &cancellables)

        NSWorkspace.shared.notificationCenter.publisher(
            for: NSWorkspace.didActivateApplicationNotification
        )
        .sink { [weak self] _ in
            MainActor.assumeIsolated { self?.fullScreenMayHaveChanged() }
        }
        .store(in: &cancellables)

        model.$hoveredIndex
            .sink { [weak self] index in
                MainActor.assumeIsolated {
                    self?.updateInteractiveRects()
                    // The card that is coming up is the one place every window,
                    // percentage and reset time is written out, and on a notch
                    // held open there is no unfold to notice instead.
                    if index != nil {
                        // The clock only ticks per second under a card, so the
                        // card opens on the current time, not the last beat's.
                        self?.model.now = Date()
                        self?.onLook?()
                    }
                    self?.onHoverCard?(index.flatMap { self?.model.snapshots[safe: $0] }?.providerID)
                    // Pulse fork: ⌘V sends the clipboard while the pointer is on the Send cell.
                    if let self {
                        let id = index.flatMap { self.model.snapshots[safe: $0] }?.providerID
                        NearbySharing.shared.hoverChanged(ObjectIdentifier(self),
                                                          overSend: id == NearbySharing.providerID)
                    }
                }
            }
            .store(in: &cancellables)

        model.$activeResetAlert
            .sink { [weak self] alert in
                MainActor.assumeIsolated {
                    self?.updateInteractiveRects()
                    // Closed by its ×, or gone: no deadline left to keep.
                    if alert == nil { self?.noticeEnded() }
                }
            }
            .store(in: &cancellables)

        // The pointer on a notice, or a screenshot session or the tool wheel being
        // open, holds its deadline; with none of them, the deadline goes on.
        // The tool hold rides along as its own flag: a @Published value arrives in
        // the sink before it is stored, so the model cannot be asked for it here.
        model.$noticeHeld.combineLatest(model.$noticeToolHold)
            .map { ($0 || $1, $1) }
            .removeDuplicates { $0 == $1 }
            .sink { [weak self] held, tool in
                MainActor.assumeIsolated { self?.holdNotice(held, tool: tool) }
            }
            .store(in: &cancellables)

        model.$updatePrompt
            .sink { [weak self] _ in
                MainActor.assumeIsolated { self?.updateInteractiveRects() }
            }
            .store(in: &cancellables)

        model.$diskImagePrompt
            .sink { [weak self] _ in
                MainActor.assumeIsolated { self?.updateInteractiveRects() }
            }
            .store(in: &cancellables)

        // A model can gain speed rows without changing the cell count. Read
        // after Published's willSet so sizing sees the new card contents too.
        model.$snapshots
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.relocate() }
            .store(in: &cancellables)
        // The panel is sized for every cell even while minimised (#375), so a change in the
        // cells that are not drawn can still change its size.
        model.$fullSnapshots
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.relocate() }
            .store(in: &cancellables)

        // No `receive(on:)`: the appearance has to be on the window before the
        // next draw, or the frame's hexes and the glass would be resolved
        // against the appearance the panel is about to stop having.
        model.$surfaceStyle
            .removeDuplicates()
            .sink { [weak self] style in
                MainActor.assumeIsolated { self?.applyPanelAppearance(style) }
            }
            .store(in: &cancellables)

        // Reduce transparency resolves the glass style to the solid one, so
        // turning it on or off in System Settings changes what the panel's
        // appearance has to be. Nothing else republishes that: the style the
        // model holds has not changed.
        NSWorkspace.shared.notificationCenter.publisher(
            for: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification
        )
        .sink { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.applyPanelAppearance(self.model.surfaceStyle)
            }
        }
        .store(in: &cancellables)
    }

    private func applyPanelAppearance(_ style: NotchSurfaceStyle) {
        panel?.appearance = style.panelAppearance(
            reduceTransparency: NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
        )
    }

    func stop() {
        setPointing(false)
        peekUntil = nil
        peekWork?.cancel()
        foldWork?.cancel()
        cursorTracker?.stop()
        cursorTracker = nil
        fullScreenTimer?.invalidate()
        fullScreenTimer = nil
        fullScreenFollowUp?.cancel()
        fullScreenFollowUp = nil
        clockTimer?.invalidate()
        cancellables.removeAll()
    }

    // MARK: - Placement

    /// The screen this notch lives on: its assigned display while that display
    /// is still connected, the menu-bar screen otherwise — so unplugging the
    /// display never strands the panel on a screen that no longer exists.
    /// Assigned first — the fleet has already picked this display for this
    /// controller, which is the whole point of there being more than one
    /// controller. `displayPreference` only comes into play once nothing has
    /// been assigned, which is the single-controller case `.mainDisplay`
    /// scope leaves it in.
    func currentScreen() -> NSScreen? {
        if let assigned = assignedScreen,
           NSScreen.screens.contains(where: { $0 === assigned }) {
            return assigned
        }
        return NotchGeometry.preferredScreen(from: NSScreen.screens, preference: displayPreference)
    }

    func relocate(cellCount: Int? = nil) {
        guard let screen = currentScreen() else { return }
        model.adopt(screen: screen)
        let size = model.panelSize(cellCount: cellCount ?? model.layoutCount)
        let frame = NotchGeometry.panelFrame(
            for: screen, panelSize: size, edge: model.edge,
            alongOffset: model.alongOffset, slack: model.slack,
            // In the hand, right up to the screen's end: the room kept for the
            // handles past the notch's ends held it short of each corner, and
            // the passage round the corner — drawn from the corner — started a
            // step away from where the notch had stopped.
            trailingExtent: isOptionDragging ? 0 : model.trailingExtent,
            leadingExtent: isOptionDragging ? 0 : model.leadingExtent,
            heldBar: model.holdsOffTheCutout ? model.plainBarLength : nil
        )

        if let panel {
            // A frame set to the size it already has is not free: AppKit still
            // runs a layout pass over the hosting view and redisplays. Most
            // calls here (every reading that changes the cell list's contents)
            // arrive at the frame the panel is already in.
            if panel.frame != frame { panel.setFrame(frame, display: true) }
        } else {
            let panel = NotchPanel(contentRect: frame)
            panel.appearance = model.surfaceStyle.panelAppearance(
                reduceTransparency: NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
            )
            let hosting = NotchHostingView(rootView: NotchRootView(model: model))
            panel.contextMenuProvider = { [weak self] in self?.contextMenu() }
            panel.onClick = { [weak self] point in self?.handleClick(at: point) }
            panel.onDragStart = { [weak self] pressed in self?.beginOptionDrag(pressedAt: pressed) }
            panel.allowsOptionDrag = { UserDefaults.standard.object(forKey: "notchOptionDrag") as? Bool ?? true }
            // The bulb carries the notch without ⌥, once the pointer has moved
            // 3pt from the press — see `NotchPanel.trackPressOrDrag`. Only while
            // it is out.
            panel.startsDrag = { [weak self] point in
                guard let self, let panel = self.panel else { return false }
                let local = CGPoint(x: point.x, y: panel.frame.height - point.y)
                return self.model.isExpanded && self.model.isHoveringSettings && self.isOverHandle(local)
            }
            panel.onDrag = { [weak self] dx, dy in self?.dragged(dx: dx, dy: dy) }
            panel.onDragEnd = { [weak self] in
                // Where it lands is saved by the landing — see `putDown` — not
                // here: a drag let go near the hole is not where it stays.
                self?.endOptionDrag()
            }

            // The hosting view goes *inside* a plain container rather than
            // being the content view itself.
            //
            // As the content view, SwiftUI gets a say in the window's frame: it
            // reports the content's ideal size, and this view's root is a
            // `GeometryReader`, whose ideal size is 10x10. On the side edges
            // that never surfaced. Turned horizontal, AppKit started walking
            // the window down toward it — 522pt of height to 266, to 10, to
            // zero — until nothing was drawn at all and the constraint pass
            // gave up and threw, taking the app with it.
            //
            // A container removes the channel instead of arguing with it. The
            // panel's size comes from `NotchGeometry` and from nowhere else,
            // which is what every hit region in this file already assumes.
            let container = NotchContainerView(frame: CGRect(origin: .zero, size: frame.size))
            container.autoresizingMask = [.width, .height]
            hosting.frame = container.bounds
            hosting.autoresizingMask = [.width, .height]
            container.addSubview(hosting)
            panel.contentView = container
            panel.setClickThrough(true)
            if !Runtime.isUnderTest { panel.orderFrontRegardless() }
            self.panel = panel
            self.hostingView = hosting
            // Pulse fork: files dropped on the Send cell are sent to the chosen device.
            hosting.registerForDraggedTypes([.fileURL])
            hosting.onDropTargeted = { on in
                MainActor.assumeIsolated { NearbySharing.shared.setDropTargeting(on) }
            }
            hosting.onDropFiles = { urls in
                MainActor.assumeIsolated { NearbySharing.shared.drop(urls: urls) }
            }
        }
        // Use the actual panel origin: near a corner its transparent padding
        // can extend offscreen, while the tooltip itself must stay visible.
        if let panel {
            let visible = panel.frame.intersection(screen.frame)
            let range: ClosedRange<CGFloat> = model.edge.isVertical
                ? (panel.frame.maxY - visible.maxY)...(panel.frame.maxY - visible.minY)
                : (visible.minX - panel.frame.minX)...(visible.maxX - panel.frame.minX)
            if model.visibleAlongRange != range { model.visibleAlongRange = range }
        }

        // The frame AppKit actually gave us, which is what the flush right-hand
        // edge depends on.
        if let panel {
            Log.usage.debug("panel \(NSStringFromRect(panel.frame), privacy: .public) on screen \(NSStringFromRect(screen.frame), privacy: .public)")
        }
        updateInteractiveRects()
    }

    /// Feeds a raw pointer delta from an ⌥-drag into `model.alongOffset` and
    /// re-places the panel at once, so the pill tracks the cursor rather than
    /// catching up once the button lifts.
    ///
    /// Both deltas are used as `NSEvent` reports them, unflipped: `deltaY`
    /// positive is the pointer moving *down* the screen, `deltaX` positive is
    /// it moving *right*. `NotchGeometry` is written to match — it subtracts
    /// the offset from a vertical edge's y (which AppKit grows *up*, so
    /// subtracting more moves the pill down) and adds it to a horizontal
    /// edge's x — so no sign flip belongs here; adding one would make the
    /// pill run away from the cursor instead of following it.
    private func dragged(dx: CGFloat, dy: CGFloat) {
        // Again on every step: whatever is under the pointer would otherwise
        // put its own cursor back.
        NSCursor.closedHand.set()
        guard let screen = currentScreen() else { return }
        travel(on: screen)
    }

    /// Where the pointer has taken a held notch, before the hole's pull.
    private var heldPointer: CGFloat = 0

    /// `pressedAt` is where the pointer went down, in screen coordinates, for a
    /// drag that began later than the press — the notch keeps its place from
    /// there, so it follows the pointer by the whole distance since the press.
    private func beginOptionDrag(pressedAt: CGPoint? = nil) {
        guard !isOptionDragging, !settling else { return }
        isOptionDragging = true
        dragStartEdge = model.edge
        defer {
            if let screen = currentScreen() {
                travelSizes = Dictionary(uniqueKeysWithValues:
                    NotchEdge.allCases.map { ($0, model.travelSize(on: $0)) })
                grip(on: screen, pointer: pressedAt ?? NSEvent.mouseLocation)
                // Up now, empty, so the first corner has nothing to wait for.
                let overlay = CornerPassageOverlay(screen: screen)
                overlay.prepare()
                passage = overlay
                // Drawn from the press, not the first movement: taking hold of
                // it is itself a movement, and the drawing taking over part
                // way through cut it short.
                travel(on: screen)
            }
        }
        clearHoverWork?.cancel()
        clearHoverWork = nil
        foldWork?.cancel()
        foldWork = nil
        model.hoveredIndex = nil
        pickUp()
        // Taken by its bulb, out — or by ⌥-drag, with nothing out.
        let fromHover = model.isExpanded && model.isHoveringSettings
        model.isHoveringSettings = false
        // In the hand: the hand closes on it.
        setCursor(.closedHand)
        model.carry = Carry(at: Date(), fromHover: fromHover)
        updateInteractiveRects()
    }

    private func endOptionDrag() {
        guard isOptionDragging, !settling else { return }
        model.carry?.releasedAt = Date()
        // **Let go round a corner, it flows off it.** On along the border, on
        // the same glide it followed the hand with, to where the whole of it
        // is on whichever edge more of it was already on — straightening as
        // it goes — and lands there. Pulled onto the edge in one frame, it
        // snapped from the bend to its resting shape: the stiffness.
        //
        // **And let go anywhere, it comes to rest rather than stopping dead.**
        // It had been a step behind the hand, gliding, and letting go put it
        // under the hand in one frame — then, freed of the drag, pulled it
        // back in from the corner in another, clear of the room its handles
        // need. Both are now the end of the same glide: on to where it will
        // stay, and it lands on arriving.
        guard let screen = currentScreen() else { finishDrag(); return }
        let frame = screen.frame
        let track = BorderTrack(width: frame.width, height: frame.height)
        var target = travelTarget ?? travelShown
        if let round = passing {
            let (first, second) = BorderTrack.edges(of: round.corner)
            let onto = round.before >= round.after ? first : second
            let half = travelSize(onto).length / 2 + 1
            let corner = track.position(of: round.corner)
            target = track.wrapped(onto == first ? corner - half : corner + half)
        }
        guard let place = target else { finishDrag(); return }
        travelTarget = resting(place, on: track)
        settling = true
        startFollowing()
    }

    /// Where the notch comes to rest for a middle at `place`: on its edge, and
    /// within the room kept either end for its handles. Beside the Mac's notch
    /// the landing decides instead — see `putDown`.
    private func resting(_ place: CGFloat, on track: BorderTrack) -> CGFloat {
        let (edge, along) = track.place(at: place)
        if let screen = currentScreen(), besideTheHole(place, on: track, screen: screen) { return place }
        let half = travelSize(edge).length / 2
        let size = edge.isVertical ? track.height : track.width
        // The room the handles keep at each end once it is down — worked out
        // for a notch clear of the hole, which is where this one lands, not
        // from how the notch is now. Short of it, it came to rest in a corner
        // and was then pushed out of it by that much in a frame: a landing
        // and then a jump. On the top edge of a display with a notch it was
        // not kept at all.
        let room = model.freeTrailingExtent
        // The notch's leading end is at the smaller `along` on every edge;
        // the settings button and its grip are past its trailing one.
        let low = half, high = size - half - room
        let held = low <= high ? min(max(along, low), high) : size / 2
        let point = edge.isVertical ? CGPoint(x: 0, y: held) : CGPoint(x: held, y: 0)
        return track.position(on: edge, of: point)
    }

    /// Let go round a corner and flowing off it — see `endOptionDrag`.
    private var settling = false

    private func finishDrag() {
        settling = false
        // Landed: the dots fade and the arc comes back, on the notch itself
        // from here — the same frame the drawing would have shown.
        let landed = Date()
        model.carry?.landedAt = landed
        DispatchQueue.main.asyncAfter(deadline: .now() + Carry.settles) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.model.carry?.landedAt == landed else { return }
                self.model.carry = nil
            }
        }
        let landing = stopTravelling()
        isOptionDragging = false
        // The notch itself again, exactly where the drawing of it came to rest.
        if let place = landing, let screen = currentScreen() {
            let frame = screen.frame
            endOverlay(at: place, on: BorderTrack(width: frame.width, height: frame.height), frame: frame)
        }
        // Gone round onto another edge: that is the edge now, and where along
        // it is saved by the landing below, against it.
        if let start = dragStartEdge, model.edge != start {
            onMoveToEdge?(model.edge, model.holdsOffTheCutout ? nil : model.alongOffset)
        }
        dragStartEdge = nil
        passage?.hide()
        passage = nil
        // Set down: the hand lets go.
        setCursor(nil)
        // Back inside the room kept for the handles, now it is not in the hand.
        if !model.holdsOffTheCutout { relocate() }
        putDown()
        cursorMoved()
    }

    // MARK: - Round the screen's edges

    /// The edge an ⌥-drag started on, so letting go knows whether it went
    /// round onto another.
    private var dragStartEdge: NotchEdge?

    /// **How much nearer another edge the pointer has to be before the notch
    /// goes round onto it** — enough that a pointer near a corner does not
    /// send it back and forth between the two.
    static let edgeSwitchMargin: CGFloat = 40

    /// **The ⌥-dragged notch keeps to the screen's frame, and goes round it.**
    ///
    /// Where it is is a place on the screen's border — see `BorderTrack` —
    /// following the pointer's place on it, held at the distance from it that
    /// it was picked up at. Along an edge it is the notch, sliding: on the top
    /// edge of a display with a notch, in the hand beside the hole with the
    /// hole's pull on it. Reaching round a corner, it is drawn wrapping the
    /// corner instead, on a surface over the whole screen, with the notch
    /// itself faded out above it; clear of the corner on the next edge, the
    /// notch is back, there. Going round one frame at a time the wrap is one
    /// band sliding round the bend — see `CornerPassageView`.
    private func travel(on screen: NSScreen) {
        let frame = screen.frame
        let track = BorderTrack(width: frame.width, height: frame.height)
        let local = CGPoint(x: NSEvent.mouseLocation.x - frame.minX,
                            y: frame.maxY - NSEvent.mouseLocation.y)
        // The pointer's place on the border — from the edge it is nearest, or
        // near a corner from both edges at once — and when the reading changes,
        // the grip taken up so the notch does not jump.
        let appKit = NSEvent.mouseLocation
        let edge = Self.stickyEdge(current: pointerEdge, pointer: appKit, frame: frame)
        let reading = Self.reading(of: local, on: track, nearest: edge)
        if reading != pointerReading {
            var step = Self.place(of: local, on: track, by: pointerReading)
                - Self.place(of: local, on: track, by: reading)
            if step > track.perimeter / 2 { step -= track.perimeter }
            if step < -track.perimeter / 2 { step += track.perimeter }
            grip += step
            pointerReading = reading
        }
        pointerEdge = edge
        // Where the hand has it. The notch follows on the display's own beat —
        // see `follow`.
        travelTarget = track.wrapped(Self.place(of: local, on: track, by: pointerReading) + grip)
        startFollowing()
    }

    /// **How the pointer's place on the border is read.** Along the edge it is
    /// nearest; or, within `cornerReach` of both edges at a corner, from both
    /// at once — how far it is from the edge before the corner, less how far
    /// from the edge after, so moving along either one, or away from either,
    /// moves it round. Read from one edge there, the pointer stopped by the
    /// screen's corner moved nothing until it was well down the next edge, and
    /// the notch sat pinned in the corner the while.
    enum Reading: Equatable {
        case edge(NotchEdge)
        case corner(BorderTrack.Corner)
    }

    static let cornerReach: CGFloat = 160

    static func reading(of point: CGPoint, on track: BorderTrack, nearest: NotchEdge) -> Reading {
        for corner in BorderTrack.Corner.allCases {
            let (before, after) = BorderTrack.edges(of: corner)
            if distance(to: before, of: point, on: track) < cornerReach,
               distance(to: after, of: point, on: track) < cornerReach {
                return .corner(corner)
            }
        }
        return .edge(nearest)
    }

    static func place(of point: CGPoint, on track: BorderTrack, by reading: Reading) -> CGFloat {
        switch reading {
        case .edge(let edge):
            return track.position(on: edge, of: point)
        case .corner(let corner):
            let (before, after) = BorderTrack.edges(of: corner)
            return track.wrapped(track.position(of: corner)
                                 + distance(to: before, of: point, on: track)
                                 - distance(to: after, of: point, on: track))
        }
    }

    /// How far a point in the screen's top-left-origin space is from `edge`.
    static func distance(to edge: NotchEdge, of point: CGPoint, on track: BorderTrack) -> CGFloat {
        switch edge {
        case .top:    return max(0, point.y)
        case .bottom: return max(0, track.height - point.y)
        case .left:   return max(0, point.x)
        case .right:  return max(0, track.width - point.x)
        }
    }

    /// How the pointer's place is being read right now.
    private var pointerReading: Reading = .edge(.top)

    /// **Draws the notch with its middle at `place` on the border.**
    ///
    /// In the hand it is drawn on the one surface over the whole screen, along
    /// the edges and round the corners alike — see `CornerPassageView` — and
    /// the notch's own window waits, out of sight, where it will land. Handing
    /// it back and forth at every corner was a jump each way, the two never
    /// quite the same, and moving a window every frame along the edges was a
    /// stutter the screen does not keep time with. Only beside the Mac's notch
    /// is it the notch itself, for the hole's pull and the join.
    private func show(at place: CGFloat, on screen: NSScreen) {
        let frame = screen.frame
        let track = BorderTrack(width: frame.width, height: frame.height)
        if besideTheHole(place, on: track, screen: screen) {
            endOverlay(at: place, on: track, frame: frame)
            put(at: place, on: track, frame: frame)
            return
        }
        let speed = abs(travelVelocity)
        let stretch = Self.maxStretch * min(1, speed / Self.stretchSpeed)
        showOverlay(at: place, on: track, screen: screen,
                    stretch: stretch, heading: travelVelocity >= 0 ? 1 : -1)
    }

    /// Whether a middle at `place` has the notch within the Mac's notch's reach.
    private func besideTheHole(_ place: CGFloat, on track: BorderTrack, screen: NSScreen) -> Bool {
        guard let cutout = screen.hardwareNotch else { return false }
        let (edge, along) = track.place(at: place)
        guard edge == .top else { return false }
        let bar = model.plainBarLength
        let free = along - track.width / 2 - cutout.width / 2 + NotchGeometry.cutoutOverlap - bar / 2
        return NotchGeometry.cutoutFreelyNear(alongOffset: free, width: cutout.width, bar: bar)
    }

    // MARK: - Following the hand

    /// Where the hand has the notch's middle on the border, where it is
    /// drawn, and how fast that is moving along it.
    private var travelTarget: CGFloat?
    private var travelShown: CGFloat?
    private var travelVelocity: CGFloat = 0
    private var follower: CADisplayLink?
    private var ticker: DisplayTick?
    private var lastTick: CFTimeInterval?

    /// **The notch follows the hand on a spring**, not an ease: it carries its
    /// speed, catches up, and comes to rest with the smallest give past the
    /// hand and back, the way something poured does — where an ease just
    /// slowed to a stop. How quick, and how much give: a hand's pull is
    /// brisk and all but dead; flowing off a corner on its own it is gentler,
    /// with a little more.
    static let followResponse: CGFloat = 0.2
    static let followDamping: CGFloat = 0.84
    static let settleResponse: CGFloat = 0.3
    static let settleDamping: CGFloat = 0.74

    /// **How much it stretches with speed**: dragged fast it draws out along
    /// the way it is going and thins, its back end trailing, and it takes its
    /// own shape again as it slows. As much as this share of its length, at
    /// `stretchSpeed` points a second.
    static let maxStretch: CGFloat = 0.14
    static let stretchSpeed: CGFloat = 2600

    /// One frame of the spring: how far it moves toward a target `gap` away,
    /// and its speed after. Two half steps, which keeps a stiff spring steady
    /// at a slow frame.
    static func spring(gap: CGFloat, velocity: CGFloat, elapsed: CGFloat,
                       response: CGFloat, damping: CGFloat) -> (moved: CGFloat, velocity: CGFloat) {
        let omega = 2 * .pi / response
        var moved: CGFloat = 0, velocity = velocity
        for _ in 0..<2 {
            let dt = elapsed / 2
            let acceleration = omega * omega * (gap - moved) - 2 * damping * omega * velocity
            velocity += acceleration * dt
            moved += velocity * dt
        }
        return (moved, velocity)
    }

    private func startFollowing() {
        guard follower == nil, let screen = currentScreen() else { return }
        // On the display's own beat, once a frame — a timer of its own ran in
        // and out of step with the screen and moved the notch twice in one
        // frame and not at all in the next.
        let tick = DisplayTick { [weak self] link in
            MainActor.assumeIsolated { self?.follow(link) }
        }
        let link = screen.displayLink(target: tick, selector: #selector(DisplayTick.tick(_:)))
        // Common modes: the drag holds the run loop in event tracking.
        link.add(to: .main, forMode: .common)
        follower = link
        ticker = tick
        lastTick = nil
    }

    private func stopFollowing() {
        follower?.invalidate()
        follower = nil
        ticker = nil
        lastTick = nil
    }

    /// One frame: the spring's pull toward where the hand has it, and the
    /// notch drawn where that leaves it — stretched by how fast it is going.
    private func follow(_ link: CADisplayLink) {
        guard let target = travelTarget, let screen = currentScreen() else { return }
        let track = BorderTrack(width: screen.frame.width, height: screen.frame.height)
        let elapsed = CGFloat(min(max(link.timestamp - (lastTick ?? link.timestamp - link.duration), 0), 1.0 / 30))
        lastTick = link.timestamp
        guard let shown = travelShown else {
            travelShown = target
            travelVelocity = 0
            show(at: target, on: screen)
            return
        }
        var gap = target - shown
        if gap > track.perimeter / 2 { gap -= track.perimeter }
        if gap < -track.perimeter / 2 { gap += track.perimeter }
        if abs(gap) < (settling ? 0.3 : 0.05), abs(travelVelocity) < (settling ? 6 : 1) {
            travelVelocity = 0
            // Flowed off the corner and come to rest: now it lands.
            if settling { finishDrag() }
            return
        }
        let (position, velocity) = Self.spring(
            gap: gap, velocity: travelVelocity, elapsed: elapsed,
            response: settling ? Self.settleResponse : Self.followResponse,
            damping: settling ? Self.settleDamping : Self.followDamping)
        travelVelocity = velocity
        let next = track.wrapped(shown + position)
        travelShown = next
        show(at: next, on: screen)
    }

    /// Done following: at where it was going, and that place.
    @discardableResult
    private func stopTravelling() -> CGFloat? {
        stopFollowing()
        let place = travelTarget ?? travelShown
        if let place, let screen = currentScreen() {
            travelShown = place
            show(at: place, on: screen)
        }
        travelTarget = nil
        travelShown = nil
        travelVelocity = 0
        return place
    }

    /// The notch on its edge with its middle at `place` on the border.
    private func put(at place: CGFloat, on track: BorderTrack, frame: CGRect) {
        let (edge, along) = track.place(at: place)
        if edge != model.edge {
            model.hoveredIndex = nil
            model.holdsOffTheCutout = false
            model.edge = edge
        }
        let point = edge.isVertical
            ? CGPoint(x: frame.minX, y: frame.maxY - along)
            : CGPoint(x: frame.minX + along, y: frame.maxY)
        let offset = Self.offset(along: edge, at: point, on: frame)
        if let cutout = heldCutout {
            // The hand's own measure beside the hole: from its right wall to
            // the bar's leading tip, so the bar's middle is at `place`; then
            // the hole's pull on it.
            let bar = model.plainBarLength
            heldPointer = offset - cutout.width / 2 + NotchGeometry.cutoutOverlap - bar / 2
            model.holdsOffTheCutout = true
            model.alongOffset = NotchGeometry.magnetised(heldPointer, width: cutout.width, bar: bar)
        } else {
            // The panel's middle, not the notch's: minimised, the open notch hangs off one end
            // of the panel's middle (see `NotchViewModel.minimizeDrift`).
            model.alongOffset = offset - model.minimizeDrift
        }
        relocate()
        updateInteractiveRects()
    }

    /// Takes hold: how far along the border the notch's middle is from the
    /// pointer's place, so it keeps that distance as it travels.
    private func grip(on screen: NSScreen, pointer: CGPoint) {
        let frame = screen.frame
        let track = BorderTrack(width: frame.width, height: frame.height)
        pointerEdge = model.edge
        let local = CGPoint(x: pointer.x - frame.minX,
                            y: frame.maxY - pointer.y)
        pointerReading = Self.reading(of: local, on: track, nearest: model.edge)
        guard let panel else { grip = 0; return }
        let wing = model.cellWing
        let middle = model.edge.isVertical
            ? CGPoint(x: 0, y: frame.maxY - panel.frame.maxY + wing.lead + wing.length / 2)
            : CGPoint(x: panel.frame.minX - frame.minX + wing.lead + wing.length / 2, y: 0)
        var distance = track.position(on: model.edge, of: middle)
            - Self.place(of: local, on: track, by: pointerReading)
        if distance > track.perimeter / 2 { distance -= track.perimeter }
        if distance < -track.perimeter / 2 { distance += track.perimeter }
        grip = distance
    }

    /// How far along the border the notch's middle is from the pointer's place.
    private var grip: CGFloat = 0
    /// The edge the pointer's place on the border is read from.
    private var pointerEdge: NotchEdge = .top

    // MARK: - Drawn on the screen while in the hand

    private var passage: CornerPassageOverlay?
    /// Whether the drawing is standing in for the notch right now.
    private var overlaid = false
    /// Where the notch was going round when the drag last moved, for a let-go
    /// mid-corner to flow off from.
    private var passing: (corner: BorderTrack.Corner, before: CGFloat, after: CGFloat)?

    /// The notch in the hand on each edge — see `NotchViewModel.travelSize` —
    /// worked out when the drag began.
    private var travelSizes: [NotchEdge: NotchViewModel.TravelSize] = [:]

    private func travelSize(_ edge: NotchEdge) -> NotchViewModel.TravelSize {
        travelSizes[edge] ?? model.travelSize(on: edge)
    }

    /// **The notch at `place` on the border**: the corner it is going round, if
    /// any, how far round, and how long it is there — turning from the one
    /// edge's length to the other's as it goes round, so there is no size it
    /// arrives at in a step.
    private func shape(at place: CGFloat, on track: BorderTrack, stretch: CGFloat = 0)
    -> (round: (corner: BorderTrack.Corner, before: CGFloat, after: CGFloat)?, length: CGFloat, turned: CGFloat) {
        let drawn = 1 + stretch
        var length = travelSize(track.place(at: place).edge).length * drawn
        guard var round = track.corner(for: place, length: length) else { return (nil, length, 0) }
        let (first, second) = BorderTrack.edges(of: round.corner)
        var turned: CGFloat = 0
        // Twice: how far round decides the length, and the length how far round.
        for _ in 0..<2 {
            let t = min(max(round.after / max(length, 1), 0), 1)
            turned = t * t * (3 - 2 * t)
            length = (travelSize(first).length
                      + (travelSize(second).length - travelSize(first).length) * turned) * drawn
            guard let again = track.corner(for: place, length: length) else { return (nil, length, turned) }
            round = again
        }
        return (round, length, turned)
    }

    private func showOverlay(at hand: CGFloat, on track: BorderTrack, screen: NSScreen,
                             stretch: CGFloat = 0, heading: CGFloat = 1) {
        let overlay = passage ?? {
            let made = CornerPassageOverlay(screen: screen)
            passage = made
            return made
        }()
        if !overlaid { passageEdge = model.edge }
        let bleed = NotchRootView.bezelBleed
        let scale = model.sizeScale
        // Stretched, it draws out behind: its front stays with the hand and
        // its middle falls back by half what it gained.
        let resting = travelSize(track.place(at: hand).edge).length
        let back = heading * resting * stretch / 2
        let place = track.wrapped(hand - back)
        let (round, length, turned) = shape(at: place, on: track, stretch: stretch)
        let thin = 1 - stretch * 0.35
        let edges = round.map { BorderTrack.edges(of: $0.corner) }
        let from = travelSize(edges?.before ?? track.place(at: place).edge)
        let to = travelSize(edges?.after ?? track.place(at: place).edge)
        func mix(_ a: CGFloat, _ b: CGFloat) -> CGFloat { a + (b - a) * turned }
        overlay.show(CornerPassageView(
            track: track, corner: round?.corner ?? .topLeft,
            before: round?.before ?? length, after: round?.after ?? 0,
            depth: bleed + (from.depth - bleed) * thin,
            depthAfter: bleed + (to.depth - bleed) * thin, bleed: bleed,
            cornerRadius: model.drawnCornerRadius * scale, flare: model.flare * scale,
            place: place, rings: passageRings(from: from, to: to, turned: turned, carriedBy: back),
            ringInset: mix(from.ringAcross, to.ringAcross) - bleed,
            ringScale: scale,
            straight: round == nil ? track.place(at: place).edge : nil,
            arcs: passageArcs(at: place, length: length, on: track),
            carry: model.carry))
        passing = round.map { ($0.corner, $0.before, $0.after) }
        guard !overlaid else { return }
        overlaid = true
        // Handed over, not faded: the drawing is exactly where the notch is,
        // so the notch simply goes — a turn later, once the drawing is on
        // screen under it, so there is no frame with neither.
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.overlaid else { return }
                self.panel?.alphaValue = 0
            }
        }
    }

    /// The notch itself again, put where the drawing had it first.
    private func endOverlay(at place: CGFloat, on track: BorderTrack, frame: CGRect) {
        guard overlaid else { return }
        overlaid = false
        passing = nil
        put(at: place, on: track, frame: frame)
        panel?.alphaValue = 1
        let overlay = passage
        // Emptied a turn later, for the same reason the notch went a turn late.
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                guard self?.overlaid == false else { return }
                overlay?.clear()
            }
        }
    }

    /// The edge the notch was on when the drawing took over, whose layout its
    /// rings keep all the way round.
    private var passageEdge: NotchEdge = .top

    /// **The handles' resting arcs, at the two ends of the notch** — the
    /// settings arc at the end that was the trailing one when the drawing took
    /// over, the move arc (when it is shown) at the other — each in the pocket
    /// of the flare at the end it hangs off, facing back along the notch, on
    /// whatever edge that end is on. Left out, they went out at every pick-up
    /// and came back at every landing.
    private func passageArcs(at place: CGFloat, length: CGFloat, on track: BorderTrack) -> [PassageArc] {
        let scale = model.sizeScale
        let sign: CGFloat = passageEdge == .top || passageEdge == .right ? 1 : -1
        let flare = model.flare * scale
        var arcs: [PassageArc] = []
        // The settings arc alone: the grip that moves the notch comes out with
        // the settings button, and has no arc of its own.
        for (id, towardEnd) in [(0, sign)] {
            let end = track.wrapped(place + towardEnd * length / 2)
            let (edge, along) = track.place(at: end)
            // Which way along this edge the rest of the notch is: back toward
            // the middle, the border's own direction there one way or other.
            let runs: CGFloat = edge == .top || edge == .right ? 1 : -1
            let bodyToward = runs * -towardEnd
            let trim = bodyToward > 0
                ? MoveHandle.restingTrim(for: edge, convex: false)
                : SettingsOrb.restingTrim(for: edge, convex: false)
            let w = track.width, h = track.height
            let centre: CGPoint
            switch edge {
            case .top:    centre = CGPoint(x: along, y: flare)
            case .bottom: centre = CGPoint(x: along, y: h - flare)
            case .left:   centre = CGPoint(x: flare, y: along)
            case .right:  centre = CGPoint(x: w - flare, y: along)
            }
            let away = edge.isVertical ? CGPoint(x: 0, y: -bodyToward) : CGPoint(x: -bodyToward, y: 0)
            arcs.append(PassageArc(id: id, centre: centre, edge: edge, trim: trim,
                                   radius: (model.flare - NotchLayout.orbClearance) * scale,
                                   gap: NotchLayout.orbClearance * scale,
                                   stroke: NotchLayout.orbStroke * scale,
                                   away: away, reach: model.gripReach))
        }
        return arcs
    }

    /// The rings, as the notch carries them — the very cells, readings and
    /// all, so the drawing and the notch are the same to the point: each one's
    /// distance from the notch's middle along the border, turning from where
    /// it is on the one edge to where it is on the next. Kept in the order the
    /// notch had them when the drawing took over, so none of them ever swaps
    /// places mid-drag.
    private func passageRings(from: NotchViewModel.TravelSize, to: NotchViewModel.TravelSize,
                              turned: CGFloat, carriedBy back: CGFloat = 0) -> [PassageRing] {
        // The border runs the same way as the notch's own stack on the top and
        // right edges, and the other way on the bottom and left.
        let sign: CGFloat = passageEdge == .top || passageEdge == .right ? 1 : -1
        return model.snapshots.enumerated().map { index, snapshot in
            // The cell's middle, which is where it is placed: its ring's, moved
            // down the stack past a reading under it on a side edge.
            let a = (from.ringCenters[safe: index] ?? 0) + from.cellShift - from.length / 2
            let b = (to.ringCenters[safe: index] ?? 0) + to.cellShift - to.length / 2
            return PassageRing(
                id: snapshot.id,
                cell: ProviderCell(snapshot: snapshot,
                                   activity: model.activity(for: snapshot),
                                   isRefreshing: model.isRefreshing(snapshot),
                                   weeklyRing: model.weeklyRing,
                                   showsWeeklyReading: model.weeklyReading,
                                   showsReading: model.showsCellReading),
                // The rings are solid: stretching the notch does not move
                // them off the hand.
                offset: sign * (a + (b - a) * turned) + back)
        }
    }

    /// The edge the notch belongs on for a pointer at `pointer`: the one it is
    /// on, unless another is nearer by more than `edgeSwitchMargin`.
    static func stickyEdge(current: NotchEdge, pointer: CGPoint, frame: CGRect) -> NotchEdge {
        let nearest = NotchEdge.allCases.min {
            distance(from: $0, of: pointer, on: frame) < distance(from: $1, of: pointer, on: frame)
        } ?? current
        let here = distance(from: current, of: pointer, on: frame)
        let there = distance(from: nearest, of: pointer, on: frame)
        return there + edgeSwitchMargin < here ? nearest : current
    }

    /// How far `point` is from `edge` of `frame`, into the screen.
    static func distance(from edge: NotchEdge, of point: CGPoint, on frame: CGRect) -> CGFloat {
        switch edge {
        case .top:    return frame.maxY - point.y
        case .bottom: return point.y - frame.minY
        case .left:   return point.x - frame.minX
        case .right:  return frame.maxX - point.x
        }
    }

    /// The offset along `edge` that centres the notch on `point`, in the
    /// measure `NotchGeometry.panelFrame` places it by: x to the right of the
    /// middle on a horizontal edge, y *down* from the middle on a vertical one.
    static func offset(along edge: NotchEdge, at point: CGPoint, on frame: CGRect) -> CGFloat {
        edge.isVertical ? frame.midY - point.y : point.x - frame.midX
    }

    // MARK: - The notch in the hand

    /// The hole on the screen the notch is on, if it is on the one edge that
    /// has one.
    private var heldCutout: HardwareNotch? {
        guard model.edge == .top else { return nil }
        return currentScreen()?.hardwareNotch
    }

    /// **Picked up, it lets go of the hole.**
    ///
    /// A joined notch is attached and cannot follow the pointer: dragging it
    /// used to move only how deep it was buried, which nothing on screen shows,
    /// so the drag went dead for fifty points and then jumped. In the hand it is
    /// a lone bar measured the plain way — leading tip point for point with the
    /// pointer — and it lets go of the hole on the fold's spring. The window is
    /// the same before and after, which is the only reason that may be eased.
    private func pickUp() {
        guard let cutout = heldCutout, !model.holdsOffTheCutout else { return }
        let free = NotchGeometry.freeOffset(fromStanding: model.alongOffset,
                                            width: cutout.width, bar: model.plainBarLength)
        let wasJoined = model.mergesWithCutout
        heldPointer = free
        let lift = {
            self.model.revealsTheOtherCopy = false
            self.model.holdsOffTheCutout = true
            self.model.alongOffset = free
            self.relocate()
        }
        if wasJoined { withAnimation(NotchMotion.lift, lift) } else { lift() }
    }

    /// **Put down near the hole, it glides onto the nearer wall and joins it.**
    ///
    /// One movement: onto the wall, in past it, and the hole's size taken, with
    /// the other copy drawn out of the far wall at the same time. It does not
    /// move the window, because the window is the same anywhere near the hole;
    /// that is what lets it ease.
    ///
    /// Put down out of reach, it stays exactly where it was put.
    private func putDown() {
        guard let cutout = heldCutout, model.holdsOffTheCutout else {
            onReposition?(model.alongOffset)
            return
        }
        let bar = model.plainBarLength
        guard let target = NotchGeometry.cutoutLanding(alongOffset: model.alongOffset,
                                                       width: cutout.width, bar: bar)
        else {
            // Out of reach: say where it is the way it is kept, and it stays.
            model.holdsOffTheCutout = false
            model.alongOffset = NotchGeometry.standingOffset(fromFree: model.alongOffset,
                                                             width: cutout.width, bar: bar)
            relocate()
            updateInteractiveRects()
            onReposition?(model.alongOffset)
            return
        }

        // **One movement**, wherever it was let go. The bar travels onto the
        // wall and in past it, takes the hole's size, and the other copy comes
        // out of the far wall, all on one spring. It used to stop at the wall
        // and take the hole a beat later — two movements, the first all but
        // settled before the second set off, which read as landing and then
        // landing again. The end at the wall closes up by where it is, not on
        // the clock (see `SideNotchShape.Dip.closes`), so there is nothing
        // left to wait for.
        withAnimation(NotchMotion.unfold) {
            model.revealsTheOtherCopy = false
            model.holdsOffTheCutout = false
            model.alongOffset = target
            relocate()
        }
        updateInteractiveRects()
        onReposition?(model.alongOffset)
    }

    // MARK: - Hit regions

    /// The panel's real size, which AppKit may have rounded up from the one we
    /// asked for — and which the flush edge depends on.
    private var placement: NotchPlacement {
        NotchPlacement(edge: model.edge, panelSize: panel?.frame.size ?? model.panelSize)
    }

    /// The notch itself, in panel coordinates with a top-left origin.
    private var notchRect: CGRect {
        placement.rect(
            along: model.wings.first?.lead ?? model.slack,
            across: 0,
            length: model.drawnAlongExtent,
            depth: model.notchDepth * model.sizeScale
        )
    }

    /// What wakes the folded notch. Larger than the pill it surrounds, and
    /// exactly the hardware notch when it is joined to one — see
    /// `NotchViewModel.wakeLength` for both halves of that.
    private var pillRect: CGRect {
        // Joined, that is both copies and the hole between them: the hardware's
        // own notch is part of the target, which is the whole point of the
        // notch being drawn as part of it.
        let joined = model.mergesWithCutout
        let length = joined ? model.drawnAlongExtent : model.wakeLength
        let lead = joined
            ? (model.wings.first?.lead ?? model.slack)
            : model.restingAlongLead
                + (model.restingLength * model.sizeScale - model.wakeLength) / 2
        return placement.rect(along: lead, across: 0, length: length, depth: model.wakeDepth)
    }

    /// The pointer's place in the notch's own measurements — what the handle's
    /// predicates are written in. The bulb scales with the notch, so its hit
    /// tests are asked in the space it was drawn in.
    private func notchMeasure(_ local: CGPoint) -> (along: CGFloat, across: CGFloat) {
        ((placement.along(of: local) - model.handleWing.lead) / model.sizeScale,
         placement.across(of: local) / model.sizeScale)
    }

    /// Where the handle is called out, as a box in panel coordinates: from half
    /// a ring back of the end ring's middle to 44pt past the shape's end, across
    /// the notch's depth. Mirrors `NotchViewModel.isInHandleZone`.
    private var handleZoneRect: CGRect {
        let ring = model.ringCenter(index: model.handleRingIndex)
        let half = NotchLayout.ringDiameter / 2
        let low = model.handleDirection > 0
            ? ring - half
            : -NotchViewModel.handleZoneReach
        let high = model.handleDirection > 0
            ? model.shapeLength + NotchViewModel.handleZoneReach
            : ring + half
        return placement.rect(along: model.handleWing.lead + low * model.sizeScale,
                              across: 0,
                              length: (high - low) * model.sizeScale,
                              depth: model.notchDepth * model.sizeScale)
    }

    /// The bulb's box, out, in panel coordinates — what a press on it answers to.
    private var bulbRect: CGRect {
        let at = model.bulbOutPoint
        let centre = placement.point(along: model.handleWing.lead + at.x * model.sizeScale,
                                     across: at.y * model.sizeScale)
        let reach = NotchBulb.hitRadius * model.sizeScale
        return CGRect(x: centre.x - reach, y: centre.y - reach, width: 2 * reach, height: 2 * reach)
    }

    /// The handle's reach, for deciding whether the panel takes events at all:
    /// the zone and the bulb, clipped to the panel. Whether a point is actually
    /// *on* the bulb is a finer question than a box can answer — see
    /// `isOverHandle`.
    private var handleRect: CGRect {
        let panelBounds = CGRect(origin: .zero, size: placement.panelSize)
        let reach = handleZoneRect.union(bulbRect).intersection(panelBounds)
        return reach.isNull ? .zero : reach
    }

    /// Whether the pointer is on the bulb — what a click or a press acts on —
    /// rather than merely in the zone that brings it out.
    private func isOverHandle(_ local: CGPoint) -> Bool {
        // The handle switched off in Settings is not there to hover or click.
        guard model.showsSettingsHandle else { return false }
        let point = notchMeasure(local)
        return model.isOnBulb(along: point.along, across: point.across)
    }

    /// Whether the pointer is where the handle comes out: the end ring and the
    /// ground below it. Drives `isHoveringSettings`.
    private func isInHandleZone(_ local: CGPoint) -> Bool {
        guard model.showsSettingsHandle else { return false }
        let point = notchMeasure(local)
        return model.isInHandleZone(along: point.along, across: point.across)
    }

    /// Where the minimise button is called out, as a box in panel coordinates: from 44pt before
    /// the leading end to half a ring past the leading end ring's middle, across the notch's
    /// depth. Mirrors `NotchViewModel.isInMinimizeZone`; the button lies inside it in either
    /// minimise state, since the open notch starts at the same place in both.
    private var minimizeZoneRect: CGRect {
        let low = -NotchViewModel.handleZoneReach
        let high = model.ringCenter(index: 0) + NotchLayout.ringDiameter / 2
        let rect = placement.rect(along: model.handleWing.lead + low * model.sizeScale,
                                  across: 0,
                                  length: (high - low) * model.sizeScale,
                                  depth: model.notchDepth * model.sizeScale)
        let reach = rect.intersection(CGRect(origin: .zero, size: placement.panelSize))
        return reach.isNull ? .zero : reach
    }

    /// Whether the pointer is where the minimise button comes out.
    private func isInMinimizeZone(_ local: CGPoint) -> Bool {
        guard model.canMinimize else { return false }
        let point = notchMeasure(local)
        return model.isInMinimizeZone(along: point.along, across: point.across)
    }

    /// Whether the pointer is on the button itself.
    private func isOverMinimizeButton(_ local: CGPoint) -> Bool {
        guard model.canMinimize else { return false }
        let point = notchMeasure(local)
        return model.hitsMinimizeButton(along: point.along, across: point.across)
    }

    /// The only region that takes the mouse. Everything else in the panel is a
    /// hole — which matters far more folded than open, since the point of
    /// folding away is to stop being in the way.
    private var liveRect: CGRect {
        guard model.isExpanded else { return pillRect }
        // The minimise button's zone is part of the open notch while the button is on.
        let body = model.canMinimize ? notchRect.union(minimizeZoneRect) : notchRect
        // The bulb hangs below the shape, so the live region is both together.
        // With the handle off its hit area goes too (Option-drag still moves the notch).
        guard model.showsSettingsHandle else { return body }
        return body.union(handleRect)
    }

    /// The card, its tail, and the gap between the tail and the notch — so
    /// sliding the pointer off the notch and onto the card never leaves it.
    private func tooltipRect(index: Int) -> CGRect? {
        guard model.snapshots.indices.contains(index) else { return nil }
        let snapshot = model.snapshots[index]
        let cardHeight = NotchLayout.cardHeight(
            windowCount: snapshot.windows.count,
            groupCount: snapshot.windowGroupCount,
            moneyWindowCount: snapshot.windows.filter { $0.money != nil }.count,
            sessionCount: model.activity(for: snapshot.id)?.sessions.count ?? 0,
            sessionCap: model.sessionCap,
            statusMessage: snapshot.statusMessage,
            blockMessage: snapshot.block?.summary(now: model.now),
            hasTokenUsage: snapshot.tokenUsage != nil,
            hasPlan: snapshot.subtitle != nil,
            hasResetCredits: snapshot.hasAvailableResetCredits,
            compactRowCount: snapshot.compactRowCount,
            extraHeight: snapshot.sendExtraHeight)
        // Across the stack the region is the card, its tail, and the gap the
        // pointer has to cross. Along it, the card's own extent.
        // The minimised Send card also carries the warning strip above it.
        let strip = snapshot.providerID == NearbySharing.providerID && model.warningLine != nil
            ? NotchLayout.cardWarningStrip : 0
        let cardAcross = model.edge.isVertical ? NotchLayout.cardWidth : cardHeight + strip
        let cardAlong = model.edge.isVertical ? cardHeight + strip : NotchLayout.cardWidth
        let centre = model.tooltipAlong(index: index, length: cardAlong)
        return placement.rect(
            along: centre - cardAlong / 2,
            // The card's own extent does not scale, and it begins where the
            // drawn notch ends.
            across: model.notchDrawnDepth,
            length: cardAlong,
            depth: NoticeMetrics.gap + NoticeMetrics.tailLength + cardAcross
        )
    }

    private func resetCardRect(event: UsageResetEvent) -> CGRect? {
        let size = NoticeCard.size(for: NoticePrompt(event: event))
        let cardAcross = model.edge.isVertical ? size.width : size.height
        let cardAlong = model.edge.isVertical ? size.height : size.width
        let centre = model.resetAlertCardAlong(for: event)
        return placement.rect(
            along: centre - cardAlong / 2,
            across: model.notchDrawnDepth,
            length: cardAlong,
            depth: NoticeMetrics.gap + NoticeMetrics.tailLength + cardAcross
        )
    }

    /// Where the update card is, on the notch's middle — see `UpdateCard`.
    private var updateCardRect: CGRect? {
        guard model.isExpanded, let prompt = model.updatePrompt else { return nil }
        let size = NoticeCard.size(for: NoticePrompt(update: prompt))
        let across = model.edge.isVertical ? size.width : size.height
        let along = model.edge.isVertical ? size.height : size.width
        let centre = model.cardAlong(centredOn: model.notchMiddleAlong, length: along)
        return placement.rect(
            along: centre - along / 2,
            across: model.notchDrawnDepth,
            length: along,
            depth: NoticeMetrics.gap + NoticeMetrics.tailLength + across
        )
    }

    /// **An update offered, or installing**: the notch opens for it and stays
    /// open until it is answered — see `UpdateCard`.
    ///
    /// On a Hidden notch the offer stays pending (`offeredUpdate`) and is not put on
    /// the model: it neither holds the notch out nor takes the card slot a toast
    /// needs. It shows the next time the notch is visible (`apply(_ visibility:)`).
    func apply(updatePrompt: UpdatePrompt?) {
        offeredUpdate = updatePrompt
        let shown = visibility == .hidden ? nil : updatePrompt
        let arriving = model.updatePrompt == nil && shown != nil
        withAnimation(.spring(response: 0.4, dampingFraction: 0.82)) {
            model.updatePrompt = shown
        }
        if arriving, panel != nil {
            model.hoveredIndex = nil
            setExpanded(true)
        }
        updateInteractiveRects()
        // Answered: back to whatever the pointer says.
        if shown == nil {
            cursorMoved()
            hideAgainIfHidden()
        }
    }

    /// The update offer as the fleet last gave it, whether or not this notch can show it now.
    private var offeredUpdate: UpdatePrompt?

    /// Where the disk image card is, on the notch's middle — see `DiskImageCard`.
    private var diskImageCardRect: CGRect? {
        guard model.isExpanded, let prompt = model.diskImagePrompt else { return nil }
        let size = NoticeCard.size(for: NoticePrompt(disk: prompt))
        let across = model.edge.isVertical ? size.width : size.height
        let along = model.edge.isVertical ? size.height : size.width
        let centre = model.diskImageCardAlong(for: prompt)
        return placement.rect(
            along: centre - along / 2,
            across: model.notchDrawnDepth,
            length: along,
            depth: NoticeMetrics.gap + NoticeMetrics.tailLength + across
        )
    }

    /// **A disk image's card**: the notch opens for it and holds open while
    /// it is up — installed with Undo, or a question. Returns whether this
    /// notch could show it (a hidden notch comes out for it and goes again).
    @discardableResult
    func apply(diskImagePrompt: DiskImagePrompt?) -> Bool {
        let arriving = model.diskImagePrompt == nil && diskImagePrompt != nil
        if arriving, panel == nil { return false }
        if arriving { hideWork?.cancel(); hideWork = nil }
        withAnimation(.spring(response: 0.4, dampingFraction: 0.82)) {
            model.diskImagePrompt = diskImagePrompt
        }
        if arriving {
            model.hoveredIndex = nil
            if !Runtime.isUnderTest { panel?.orderFrontRegardless() }
            setExpanded(true)
        }
        updateInteractiveRects()
        // Gone: back to whatever the pointer says.
        if diskImagePrompt == nil { cursorMoved(); hideAgainIfHidden() }
        return true
    }

    /// The Send cell, in panel coordinates, while the notch is open: the one
    /// place a file drag is taken.
    private var sendCellRect: CGRect? {
        guard model.isExpanded,
              let index = model.snapshots.firstIndex(where: { $0.providerID == NearbySharing.providerID })
        else { return nil }
        let pitch = model.cellPitch * model.sizeScale
        let centre = model.ringAlong(index: index, in: model.cellWing)
        return placement.rect(along: centre - pitch / 2, across: 0, length: pitch,
                              depth: model.notchDepth * model.sizeScale)
    }

    private func updateInteractiveRects() {
        var rects = [liveRect]
        if let card = updateCardRect { rects.append(card) }
        if let card = diskImageCardRect { rects.append(card) }
        if model.isExpanded, let event = model.activeResetAlert, let card = resetCardRect(event: event) {
            rects.append(card)
        }
        if model.isExpanded, let index = model.hoveredIndex, let card = tooltipRect(index: index) {
            rects.append(card)
        }
        hostingView?.interactiveRects = rects
        hostingView?.dropRect = sendCellRect
        if let panel {
            // Runs for every mouse event on the screen. AppKit does not skip an
            // unchanged value: each assignment re-sends the window's event mask
            // and tags to WindowServer and flushes a layout pass.
            // The rects are in the panel's own top-left-origin space, as the pointer
            // from `localCursor` is. The rule and the change-only toggle are RightKit's
            // (`RightKitOverlayHitRegions`, `RightKitOverlayPanel.setClickThrough`).
            let regions = RightKitOverlayHitRegions(rects, origin: .topLeft)
            panel.setClickThrough(regions.ignoresMouseEvents(at: localCursor(in: panel.frame)) ?? true)
        }
    }

    // MARK: - Cursor tracking

    /// A global monitor catches the outside-to-inside crossing while the panel
    /// is still ignoring events; a local one catches the way back out.
    ///
    /// A slow poll backs both of them up, because a cursor that never moves
    /// produces no events at all — so a notch that appears, resizes or is
    /// re-anchored underneath a parked pointer would otherwise sit there with
    /// stale hover state until the user jogged the mouse.
    /// A space or app switch: answered at once, and once more a moment later,
    /// because an app that has just come forward is often still animating
    /// into full screen when the notification arrives.
    private func fullScreenMayHaveChanged() {
        refreshFullScreen()
        handleActiveSpaceOrAppChange()
        fullScreenFollowUp?.cancel()
        let followUp = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                self?.refreshFullScreen()
                self?.handleActiveSpaceOrAppChange()
            }
        }
        fullScreenFollowUp = followUp
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: followUp)
    }

    /// Full screen that posts no notification — a video going full screen in
    /// the browser already in front, a game sizing its window to the display —
    /// is only caught by asking, and asking is a WindowServer round trip.
    /// It rode the 0.3s cursor poll, which made it three of those a second
    /// for as long as the app ran. Every two seconds is soon enough for a
    /// fold nobody is waiting on, and switches are answered by the
    /// notifications above without waiting for it. (From #202.)
    private func startWatchingFullScreen() {
        fullScreenTimer?.invalidate()
        fullScreenTimer = nil
        guard foldsForFullScreen else { refreshFullScreen(); return }
        let poll = Timer(timeInterval: Self.fullScreenPollInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.foldsForFullScreen else { return }
                self.refreshFullScreen()
                self.handleActiveSpaceOrAppChange()
            }
        }
        poll.tolerance = Self.fullScreenPollInterval / 4
        RunLoop.main.add(poll, forMode: .common)
        fullScreenTimer = poll
        refreshFullScreen()
    }

    /// The cursor poll is only a safety net behind the mouse monitors below (a panel that
    /// moved under a still pointer, a hover state left to clear). Folded with the pointer
    /// away and nothing hovered it ticks once a second; any other state, 0.3 s as before.
    /// (#148: three wakeups a second for the life of the app when nothing was happening.)
    private static let cursorPollActive: TimeInterval = 0.3
    private static let cursorPollIdle: TimeInterval = 1.0
    private var cursorPollIsIdle = false

    /// The tracker's interval is fixed, so an idle change replaces it. The new one is
    /// stored before it starts: its first tick may ask for the other state, and that
    /// call must find and stop this tracker rather than leave it running.
    private func scheduleCursorPoll(idle: Bool) {
        cursorTracker?.stop()
        cursorPollIsIdle = idle
        let tracker = RightKitOverlayPointerTracker(
            pollInterval: idle ? Self.cursorPollIdle : Self.cursorPollActive
        ) { [weak self] _ in
            self?.cursorMoved()
        }
        cursorTracker = tracker
        tracker.start()
    }

    private func noteCursorPoll(idle: Bool) {
        guard cursorTracker != nil, cursorPollIsIdle != idle else { return }
        scheduleCursorPoll(idle: idle)
    }

    /// Mouse moves (global and local) and the poll both come through the tracker.
    private func startWatchingCursor() {
        scheduleCursorPoll(idle: false)
    }

    private func localCursor(in frame: CGRect) -> CGPoint {
        let mouse = NSEvent.mouseLocation
        return CGPoint(x: mouse.x - frame.minX, y: frame.maxY - mouse.y)
    }

    // Not private: tests drive the hover fold through it, the same way they
    // drive the event fold through handleActiveSpaceOrAppChange.
    func cursorMoved() {
        guard let panel, !isOptionDragging else { return }
        let local = localCursor(in: panel.frame)
        // Nothing to do while folded with the pointer away and no hover state
        // left to clear: skips the card, grip and cursor work on every mouse event.
        // The hit regions are re-laid when the layout changes (every site that
        // moves a rect calls `updateInteractiveRects`), so here they are touched
        // only if the panel still catches the mouse; folded, nothing else is a rect.
        if !model.isExpanded, model.hoveredIndex == nil, foldWork == nil, clearHoverWork == nil,
           shownCursor == nil,
           !model.isHoveringSettings, !model.isHoveringMinimize, !model.isOnMinimizeButton,
           model.previewMinimized == nil, !model.isOverWarningBand,
           (!CGRect(origin: .zero, size: panel.frame.size).contains(local) || !liveRect.contains(local)) {
            if !panel.ignoresMouseEvents { updateInteractiveRects() }
            noteCursorPoll(idle: true)
            return
        }
        noteCursorPoll(idle: false)
        // Open with the pointer off the panel and nothing hovered, which is every poll of a
        // notch held open by "Always show". Nothing below can find a cell, a handle or a card
        // out there; the one decision left is whether to fold, so that is still asked and the
        // rects are left alone. This ran in full every 0.3 s: a fifth of the main thread's work.
        if model.isExpanded, model.hoveredIndex == nil, foldWork == nil, clearHoverWork == nil,
           shownCursor == nil,
           !model.isHoveringSettings, !model.isHoveringMinimize, !model.isOnMinimizeButton,
           model.previewMinimized == nil, !model.isOverWarningBand,
           !CGRect(origin: .zero, size: panel.frame.size).contains(local) {
            setExpanded(false, ignoreAlwaysOn: foldsForFullScreen && isFullScreenActive())
            if !panel.ignoresMouseEvents { updateInteractiveRects() }
            return
        }
        var overTooltip = model.hoveredIndex
            .flatMap(tooltipRect(index:))
            .map { model.isExpanded && $0.contains(local) } ?? false
        // A notice under the pointer holds the notch open as well as its own
        // deadline (see `holdNotice`): reading it must not fold it away.
        if !overTooltip, model.isExpanded, model.hoveredIndex == nil,
           let event = model.activeResetAlert, let card = resetCardRect(event: event) {
            overTooltip = card.contains(local)
        }
        // The fold setting gates this check as surely as the one in
        // handleActiveSpaceOrAppChange: left ungated, the hover fold out-votes
        // "Always show" under a full-screen app while the other path keeps
        // restoring it — the notch ends up folding on every poll.
        setExpanded(liveRect.contains(local) || overTooltip,
                    ignoreAlwaysOn: foldsForFullScreen && isFullScreenActive())

        var target: Int?
        if model.isExpanded, notchRect.contains(local) {
            // The whole stack is the hover band, nearest ring first.
            target = model.hoverRing(along: placement.along(of: local))
        } else if model.isExpanded, let current = model.hoveredIndex,
                  let card = tooltipRect(index: current),
                  card.contains(local) {
            target = current
        }

        // The handle's zone, which brings the bulb out; the pointing hand only
        // on the bulb itself.
        let overHandle = model.isExpanded && isInHandleZone(local)
        if model.isHoveringSettings != overHandle {
            model.isHoveringSettings = overHandle
        }
        // The minimise button's end (#375): the button comes out, and dwelling on it previews
        // the other state.
        let overMinimizeZone = model.isExpanded && model.carry == nil && isInMinimizeZone(local)
        let overMinimize = overMinimizeZone && isOverMinimizeButton(local)
        updateMinimizeHover(zone: overMinimizeZone, onButton: overMinimize)
        // The Send ring's centre band while the notch is minimised and a gauge is in warning:
        // the Send hover card opens as on any ring, with the line naming the gauge in a strip
        // above it (`NotchRootView.noticeSlot`).
        var overBand = false
        if model.isExpanded, notchRect.contains(local) {
            let point = notchMeasure(local)
            overBand = model.isOnWarningBand(along: point.along, across: point.across)
        }
        if model.isOverWarningBand != overBand { model.isOverWarningBand = overBand }
        let pointing = Self.wantsPointingHand(isExpanded: model.isExpanded, cellIndex: target)
            || (overHandle && isOverHandle(local)) || overMinimize
        setCursor(pointing ? .pointingHand : nil)

        // Hover cards switched off in Settings: no card opens, so nothing is hovered.
        let cardTarget = model.showsHoverCards && !overMinimize ? target : nil
        if let cardTarget {
            clearHoverWork?.cancel()
            clearHoverWork = nil
            if model.hoveredIndex != cardTarget {
                withAnimation(.spring(response: 0.18, dampingFraction: 0.85)) {
                    model.hoveredIndex = cardTarget
                }
            }
        } else if model.hoveredIndex != nil, clearHoverWork == nil {
            let work = DispatchWorkItem { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.clearHoverWork = nil
                    withAnimation(.easeOut(duration: 0.18)) { self.model.hoveredIndex = nil }
                }
            }
            clearHoverWork = work
            DispatchQueue.main.asyncAfter(deadline: .now() + hoverGrace, execute: work)
        }

        updateInteractiveRects()
    }

    // MARK: - Minimise (#375)

    /// How long the pointer rests on the button before it previews the other state.
    private let minimizePreviewDwell: TimeInterval = 0.25
    private var minimizeDwell: DispatchWorkItem?
    /// False from a click until the pointer leaves the button: the button is still under it,
    /// and the state it just committed must not flip into a preview of the other one.
    private var minimizePreviewArmed = true

    /// Hover on the button's end. The preview is visual only: nothing is written until the
    /// click. It never moves the button — the open notch starts at the same place in both
    /// states (see `NotchViewModel.wings`), so the pointer stays on it.
    private func updateMinimizeHover(zone: Bool, onButton: Bool) {
        if model.isHoveringMinimize != zone { model.isHoveringMinimize = zone }
        if model.isOnMinimizeButton != onButton { model.isOnMinimizeButton = onButton }
        if onButton {
            guard minimizeDwell == nil, model.previewMinimized == nil, minimizePreviewArmed else { return }
            let work = DispatchWorkItem { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.minimizeDwell = nil
                    guard self.model.isOnMinimizeButton, self.minimizePreviewArmed,
                          self.model.previewMinimized == nil else { return }
                    self.model.previewMinimized = !self.model.isMinimized
                    self.updateInteractiveRects()
                }
            }
            minimizeDwell = work
            DispatchQueue.main.asyncAfter(deadline: .now() + minimizePreviewDwell, execute: work)
        } else {
            minimizeDwell?.cancel()
            minimizeDwell = nil
            minimizePreviewArmed = true
            if model.previewMinimized != nil {
                model.previewMinimized = nil
                updateInteractiveRects()
            }
        }
    }

    /// A click on the button commits the state that was being previewed (or flips it, when the
    /// click came before the preview) and keeps it: persisted through `onMinimizedChange`.
    private func toggleMinimized() {
        minimizeDwell?.cancel()
        minimizeDwell = nil
        minimizePreviewArmed = false
        let wanted = !model.isMinimized
        // The standing state first, while the preview still draws the same thing, so the
        // notch changes once.
        model.isMinimized = wanted
        model.previewMinimized = nil
        onMinimizedChange?(wanted)
        updateInteractiveRects()
    }

    /// Opens on contact, folds shut after a pause — unless it has been pinned
    /// open, in which case the pointer is not what decides.
    ///
    /// `ignoreAlwaysOn` is only read when it can change the outcome — a fold
    /// about to be scheduled on a notch that "Always show" would otherwise
    /// hold open — because answering it asks WindowServer.
    private func setExpanded(_ wanted: Bool, ignoreAlwaysOn: @autoclosure () -> Bool = false) {
        if wanted {
            foldWork?.cancel()
            foldWork = nil
            // Back before it folded: the arcs come out again.
            if model.handlesTuckedAway { model.handlesTuckedAway = false }
            guard !model.isExpanded else { return }
            withAnimation(NotchMotion.unfold) { model.isExpanded = true }
            onLook?()
            return
        }

        // An update offered holds it open until it is answered.
        if model.updatePrompt != nil { return }
        // So does a disk image's card, until it is answered or goes by itself.
        if model.diskImagePrompt != nil { return }
        // A peek holds the notch open for its own duration; only after that
        // does the pointer get a say again.
        if let peekUntil, peekUntil > Date() { return }
        guard model.isExpanded, foldWork == nil, !model.isPinned else { return }
        // Pinned is settled above; what is left to decide is whether "Always
        // show" holds it, and only a frontmost full-screen app overrules that.
        let ignoresAlwaysOn = model.isAlwaysOn && ignoreAlwaysOn()
        guard ignoresAlwaysOn || !model.isAlwaysOn else { return }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                let stillHoldsOpen = self.model.isPinned || (self.model.isAlwaysOn && !ignoresAlwaysOn)
                guard !stillHoldsOpen else {
                    self.foldWork = nil
                    return
                }
                withAnimation(NotchMotion.unfold) {
                    self.model.isExpanded = false
                    self.model.hoveredIndex = nil
                }
                // Cleared once folded, so letting go of the pending fold does
                // not bring the arcs back out on its way shut.
                self.foldWork = nil
                self.setPointing(false)
                self.updateInteractiveRects()
            }
        }
        foldWork = work
        // Left from the settings button or its dots: the button turns back
        // into its arc first — see `DiscToArc` — and only then does the arc
        // go home and the notch fold. At once, the arc was on its way in
        // before the button had become it.
        let fromHandle = model.isHoveringSettings
        let turnBack: TimeInterval = fromHandle ? SettingsOrb.turnsBack : 0
        // The arcs start home, while the notch is still open to go into — it
        // folds once they are in.
        if turnBack > 0 {
            DispatchQueue.main.asyncAfter(deadline: .now() + turnBack) { [weak self] in
                MainActor.assumeIsolated {
                    guard let self, self.foldWork === work else { return }
                    self.model.handlesTuckedAway = true
                }
            }
        } else {
            model.handlesTuckedAway = true
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + foldGrace + turnBack, execute: work)
    }

    /// The rings are buttons, so they should say so.
    static func wantsPointingHand(isExpanded: Bool, cellIndex: Int?) -> Bool {
        isExpanded && cellIndex != nil
    }

    /// Pushed and popped rather than `set`, so leaving restores whatever cursor
    /// the app underneath had chosen. Setting `.arrow` on the way out would
    /// stamp an arrow over someone else's text caret.
    private func setPointing(_ wanted: Bool) {
        setCursor(wanted ? .pointingHand : nil)
    }

    /// The cursor over the notch: a pointing hand on what clicks, an open hand
    /// on the grip, a closed one while the notch is carried — or the system's
    /// own. One pushed at a time, so each change pops the last.
    private func setCursor(_ wanted: NSCursor?) {
        BackgroundCursor.enable()
        guard wanted !== shownCursor else { return }
        if shownCursor != nil { NSCursor.pop() }
        wanted?.push()
        shownCursor = wanted
    }

    /// A click on a ring refetches that provider; a click anywhere else on the
    /// open notch pins it. The ring is the more specific target, so it wins.
    func handleClick(at locationInWindow: CGPoint) {
        guard let panel else {
            setExpanded(true)
            return
        }
        // Use the event position even if the pointer has moved since the click.
        let local = CGPoint(x: locationInWindow.x, y: panel.frame.height - locationInWindow.y)

        // The minimise button (#375) sits off the leading end ring, over the notch's own end:
        // its click is its own, ahead of the permissions prompt and the cells.
        if model.isExpanded, isOverMinimizeButton(local) {
            toggleMinimized()
            return
        }

        if model.permissionsPending,
           notchRect.contains(local) || (model.isExpanded && isOverHandle(local)) {
            onOpenHub?("permissions")
            return
        }

        // The handle sits inside the notch, so it has to be tested before the
        // cells — otherwise the cell band nearest the foot of the stack swallows
        // it and clicking the gear refetches a provider instead.
        if model.isExpanded, isOverHandle(local) {
            // The same turn the SwiftUI tap gives it, so the gear responds
            // however the click reached it — this path and the tap gesture
            // are two routes to one action.
            model.settingsSpins += 1
            onOpenSettings?()
            return
        }
        // A peek is a question — "this one just finished, do you want it?" —
        // and the click that follows is the answer. It outranks pinning and
        // refetching for as long as the offer stands, and for no longer.
        //
        // Tested before the folded case below, not after: the grace period
        // outlives the peek by a couple of seconds precisely so that a hand
        // that arrived late still lands on the session, and answering it by
        // merely re-opening the notch would waste that click.
        if takePendingFocus() {
            peekWork?.cancel()
            peekWork = nil
            peekUntil = nil
            withAnimation(NotchMotion.unfold) {
                model.isExpanded = false
                model.hoveredIndex = nil
            }
            setPointing(false)
            updateInteractiveRects()
            return
        }
        // Clicks on the tooltip card belong to whatever is drawn there — the
        // session rows take their own taps — and must not fall through to the
        // cell refetch or the pin toggle underneath.
        if model.isExpanded, let index = model.hoveredIndex,
           let card = tooltipRect(index: index), card.contains(local) {
            return
        }
        guard model.isExpanded else {
            // Opens it, the same as the pointer arriving would — it must not
            // also pin it. The pill's hot zone is deliberately generous, since
            // it is a small target on a screen edge, so a click aimed at
            // something else nearby can land here without the notch ever
            // having been seen open. Pinning is what a click on a notch that
            // is *already* open does; folding it back in later is exactly
            // the ordinary hover behaviour, which a plain `setExpanded` leaves
            // intact.
            setExpanded(true)
            return
        }
        // Pulse fork: a click on the open notch opens the hub — a ring on
        // its own page (disks → Storage, system → Monitor, an account →
        // Accounts), anywhere else on the Overview. Readings refresh on their own.
        if notchRect.contains(local),
           let index = cellIndex(along: placement.along(of: local)),
           model.snapshots.indices.contains(index) {
            let id = model.snapshots[index].providerID
            // The Tools gauge is the way to Settings: the hub opens there
            // directly, with the same turn of the bulb's gear a click on the
            // bulb gives it.
            if id == SystemProviders.toolsID {
                model.settingsSpins += 1
                onOpenSettings?()
                return
            }
            let section = id == SystemProviders.disksID ? "storage"
                : id == SystemProviders.cpuID ? "monitor"
                : id == SystemProviders.sendID ? "general" : "accounts"
            onOpenHub?(section)
            return
        }
        if notchRect.contains(local) { onOpenHub?("overview") }
        // Anything else on an open notch does nothing. A click here used to
        // pin it, which read as the notch locking itself: the rings are small
        // targets, a click aimed at one lands beside it easily, and `isPinned`
        // has no drawn state — so the notch simply stopped folding and nothing
        // on screen said why or how to undo it. Keep open is on the
        // right-click menu, where it is named and carries a checkmark.
    }

    /// Move the notch to another screen edge.
    ///
    /// It goes out where it was, crosses while there is nothing to see, and
    /// then **opens** where it now is — the same unfold hovering uses, so a
    /// move ends the way reaching for it does rather than with a bar appearing
    /// at full size.
    ///
    /// Changing the placement moves the panel, turns the shape on its side and
    /// relays the whole stack, all in one frame. Done in view that is a jump no
    /// animation can smooth over, and animating a panel across a corner looks
    /// like a bug rather than a choice — hence the crossing rather than a
    /// slide.
    /// A new size choice: set it, then rebuild the panel around it.
    ///
    /// Set-then-relocate rather than a subscription on `model.$sizeScale`,
    /// because `@Published` fires in `willSet` — a sink here would recompute
    /// the panel from the size that is being replaced. `apply(edge:)` is the
    /// same shape for the same reason.

    func apply(alongOffset: CGFloat) {
        guard model.alongOffset != alongOffset else { return }
        model.alongOffset = alongOffset
        relocate()
    }

    /// The size to start at, before there is a panel to relayout.
    ///
    /// A display plugged in later builds its panel at the current size rather
    /// than at medium and resizing a beat afterwards.
    func prime(scale: CGFloat) {
        model.requestedScale = scale
    }

    /// Whether each ring carries its percentage.
    ///
    /// Relaid out, not merely set: beside the hardware the reading is paid for
    /// out of ring size, so turning it on changes the ring, the strip's length
    /// and the width of the window around it. Set without relocating, the
    /// window keeps its old size and the shape — which centres itself in it —
    /// slides away from the settings arc and the tooltip, both placed from
    /// `slack`. Every setting that moves `panelSize` has to come through here.
    func apply(showsNotchReadings: Bool) {
        NotchLayout.showsReadings = showsNotchReadings
        guard model.showsNotchReadings != showsNotchReadings else { return }
        model.showsNotchReadings = showsNotchReadings
        relocate()
        updateInteractiveRects()
    }

    func apply(scale: CGFloat) {
        // Against the setting, not against `sizeScale`: the hardware's own notch
        // answers that one, so comparing with it would drop every change the
        // user makes while the notch is merged — and the value still governs
        // every other edge it might be moved to.
        guard model.requestedScale != scale else { return }

        // A drag arrives as a stream of tiny deltas; a preset, or a switch
        // between the two controls, arrives as one large one.
        let isDrag = abs(scale - model.requestedScale) < Self.steppedScaleDelta

        // The drawn shape follows every tick — that part is a redraw and it is
        // cheap. Re-laying the *window* out is not: `relocate` recomputes the
        // panel size through `maxCardHeight` and the `sessionCap` search, then
        // asks the compositor to resize a full-height window. Sixty of those a
        // second is what makes a drag feel like it is pulling something heavy.
        model.requestedScale = scale
        if isDrag {
            coalesceRelocate()
        } else {
            pendingRelocate?.cancel()
            pendingRelocate = nil
            relocate()
            updateInteractiveRects()
        }
    }

    /// Above this, a size change was *chosen* rather than dragged. The
    /// smallest gap between two presets is 0.2 and a drag tick is a fraction
    /// of a percent, so there is a wide margin either way.
    private static let steppedScaleDelta: CGFloat = 0.05

    /// The window is re-laid out at most this often while a drag is in
    /// flight. The panel is larger than the notch by the whole tooltip slack,
    /// so it can be a tenth of a second out of date without anything showing.
    private static let relocateInterval: TimeInterval = 0.1

    /// Resize the window on a budget, and always once the drag has stopped.
    private func coalesceRelocate() {
        let now = Date()
        if now.timeIntervalSince(lastRelocate) >= Self.relocateInterval {
            lastRelocate = now
            relocate()
            return
        }
        // Too soon. Replace any pending catch-up with one scheduled from now,
        // so a drag that stops mid-interval still ends up correctly sized.
        pendingRelocate?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.lastRelocate = Date()
            self.relocate()
            self.updateInteractiveRects()
        }
        pendingRelocate = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.relocateInterval,
                                      execute: work)
    }

    private var lastRelocate = Date.distantPast
    private var pendingRelocate: DispatchWorkItem?
    func apply(edge: NotchEdge) {
        guard model.edge != edge else { return }
        guard let panel else {   // before there is anything on screen to fade
            model.edge = edge
            relocate()
            return
        }

        let wasOpen = model.isExpanded
        model.hoveredIndex = nil
        setPointing(false)

        // Clicking through the picker starts a move before the last one has
        // landed, and a stale completion would drop the notch on an edge the
        // user has already moved on from.
        edgeChange += 1
        let change = edgeChange

        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.edgeCrossfade
            panel.animator().alphaValue = 0
        } completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self, let panel = self.panel, change == self.edgeChange else { return }

                // Land folded, and at full strength: the opening *is* the
                // animation, and fading in underneath it would be two at once.
                self.model.edge = edge
                self.model.isExpanded = false
                self.relocate()
                self.updateInteractiveRects()
                panel.alphaValue = 1

                guard wasOpen else { return }
                // A beat, then open. Not decoration: setting it shut and open
                // again inside one turn lets SwiftUI coalesce the pair, and the
                // notch arrives at full size having animated nothing.
                DispatchQueue.main.asyncAfter(deadline: .now() + Self.arrivalBeat) {
                    MainActor.assumeIsolated {
                        guard change == self.edgeChange else { return }
                        withAnimation(NotchMotion.unfold) { self.model.isExpanded = true }
                        // The panel too. The relocate above ran while the notch
                        // was folded, and opening it here without another one
                        // left the window at the folded size: the shape centres
                        // on the panel it is in, so it sat some 19pt left of
                        // where the settings orb and the tooltip — both placed
                        // from `slack` — expected it. That is the arc drifting
                        // off the corner and the card pointing wide of its ring
                        // after an edge change, and only after one.
                        self.relocate()
                        self.updateInteractiveRects()
                    }
                }
            }
        }
    }

    func apply(displayPreference: DisplayPreference) {
        guard self.displayPreference != displayPreference else { return }
        self.displayPreference = displayPreference
        relocate()
    }

    /// Half the crossing, each way. Short: it is a settings change, not a
    /// flourish, and the notch should be back before you have looked up.
    private static let edgeCrossfade: TimeInterval = 0.16
    /// The pause between landing and opening.
    private static let arrivalBeat: TimeInterval = 0.05
    private var edgeChange = 0

    func apply(_ visibility: NotchVisibility) {
        self.visibility = visibility
        // A standing choice outranks a peek that happens to be in flight.
        hideWork?.cancel()
        hideWork = nil
        peekWork?.cancel()
        peekWork = nil
        peekUntil = nil
        // A pending update offer waits out Hidden (see `apply(updatePrompt:)`).
        if visibility == .hidden, model.updatePrompt != nil { model.updatePrompt = nil }
        defer {
            if visibility != .hidden, offeredUpdate != nil, model.updatePrompt == nil {
                apply(updatePrompt: offeredUpdate)
            }
        }
        switch visibility {
        case .alwaysShow:
            if !Runtime.isUnderTest { panel?.orderFrontRegardless() }
            // Any pin made by hand is subsumed by the setting, exactly as it is
            // for the other two. Leaving it set would hold `handleActiveSpaceOrAppChange`
            // off for the rest of the session, so a pin made in hover mode would
            // silently disable the full-screen fold once Always show was chosen.
            model.isPinned = false
            model.isAlwaysOn = true
            foldWork?.cancel()
            foldWork = nil
            withAnimation(NotchMotion.unfold) { model.isExpanded = true }
        case .onHover:
            if !Runtime.isUnderTest { panel?.orderFrontRegardless() }
            model.isPinned = false
            model.isAlwaysOn = false
            // Fold now rather than waiting for the pointer to leave: it may
            // already be somewhere else, in which case nothing would arrive to
            // close it and "on hover" would look exactly like "always show".
            withAnimation(NotchMotion.unfold) {
                model.isExpanded = false
                model.hoveredIndex = nil
            }
        case .hidden:
            model.isPinned = false
            model.isAlwaysOn = false
            model.isExpanded = false
            model.hoveredIndex = nil
            // Ordered out rather than made transparent. An invisible panel that
            // still takes the screen edge would keep swallowing the pointer.
            panel?.orderOut(nil)
        }
        setPointing(false)
        updateInteractiveRects()
    }

    // MARK: - Peeking

    /// Open the notch by itself for a moment, because something happened.
    ///
    /// Distinct from `setExpanded(true)`, which is the pointer arriving: this
    /// has no pointer to leave again, so it schedules its own close. The close
    /// checks the same two conditions the hover fold does — pinned open, or the
    /// pointer now resting on it — because a peek that arrives while you are
    /// already reading the notch must not yank it shut underneath you.
    ///
    /// `pid` is the agent's process, used only if the peek is clicked; nil
    /// leaves the click doing what it ordinarily does.
    ///
    /// Hidden does not stop it: the notch comes out, shows what happened, and goes
    /// again (`hideAgainIfHidden`). `evenWhenHidden: false` is for a peek that only
    /// previews a setting, which a hidden notch has no use for.
    func peek(for duration: TimeInterval, focusing pid: pid_t?, evenWhenHidden: Bool = true) {
        guard let panel, evenWhenHidden || visibility != .hidden else {
            Log.usage.debug("peek skipped: no panel, or hidden for a preview")
            return
        }
        hideWork?.cancel()
        hideWork = nil
        Log.usage.debug("peek for \(duration, privacy: .public)s, pid \(pid ?? -1, privacy: .public)")

        if let pid {
            pendingFocus = (pid: pid, until: Date().addingTimeInterval(duration + Self.focusGrace))
        }
        peekUntil = Date().addingTimeInterval(duration)

        if !Runtime.isUnderTest { panel.orderFrontRegardless() }
        foldWork?.cancel()
        foldWork = nil
        peekWork?.cancel()
        withAnimation(NotchMotion.unfold) { model.isExpanded = true }
        updateInteractiveRects()

        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, let panel = self.panel else { return }
                self.peekWork = nil
                self.peekUntil = nil
                let stillHoldsOpen = self.model.isPinned || (self.model.isAlwaysOn && !(self.foldsForFullScreen && self.isFullScreenActive()))
                guard !stillHoldsOpen else { return }
                // A notice the pointer is holding keeps the notch it hangs from.
                guard !((self.model.noticeHeld || self.model.noticeToolHold) && self.model.activeResetAlert != nil) else { return }
                // Left open if the peek did its job and the pointer is already
                // there; the ordinary hover fold takes it from here.
                // (A hidden notch has no hover to hand over to: it goes back in.)
                guard self.visibility == .hidden
                        || !self.liveRect.contains(self.localCursor(in: panel.frame)) else { return }
                withAnimation(NotchMotion.unfold) {
                    self.model.isExpanded = false
                    self.model.hoveredIndex = nil
                }
                self.setPointing(false)
                self.updateInteractiveRects()
                self.hideAgainIfHidden()
                Log.usage.debug("peek folded")
            }
        }
        peekWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + duration, execute: work)
    }

    // MARK: - The clipboard strip

    /// Where the clipboard strip hangs from this notch (docs/clipboard-spec.md 7.1): the display,
    /// the edge, the notch's size scale, how deep its face is, and its middle along the display's
    /// edge, counted from the display's left (top and bottom edges) or top (left and right).
    struct StripAnchor {
        var screen: NSScreen
        var edge: NotchEdge
        var scale: CGFloat
        var inset: CGFloat
        var middle: CGFloat
        /// The notch's own window, so the strip's can sit just under it.
        var window: NSWindow
    }

    func stripAnchor() -> StripAnchor? {
        guard let panel, let screen = currentScreen() else { return nil }
        let frame = panel.frame
        let display = screen.frame
        let middle = model.edge.isVertical
            ? (display.maxY - frame.maxY) + model.notchMiddleAlong
            : (frame.minX - display.minX) + model.notchMiddleAlong
        return StripAnchor(screen: screen, edge: model.edge, scale: model.sizeScale,
                           inset: model.notchDrawnDepth, middle: middle, window: panel)
    }

    /// Keeps the notch out while the strip hangs from it, and lets it go again after: a Hidden or
    /// folded notch comes out for the strip and goes when the strip does.
    func holdOpenForStrip(_ held: Bool) {
        if held {
            peek(for: 3600, focusing: nil)
        } else {
            peek(for: 0.6, focusing: nil)
        }
    }

    /// Whether this notch has somewhere to put a card. A hidden one has: it comes
    /// out for the card and goes again.
    var canShowCards: Bool { panel != nil }

    private var hideWork: DispatchWorkItem?

    /// A Hidden notch that came out for a toast goes back once nothing of it is up:
    /// folded first, then taken off the screen so it swallows no pointer.
    private func hideAgainIfHidden() {
        guard visibility == .hidden else { return }
        hideWork?.cancel()
        let idle: @MainActor (NotchWindowController) -> Bool = { c in
            c.visibility == .hidden && c.peekWork == nil && c.model.activeResetAlert == nil
                && c.model.diskImagePrompt == nil && c.model.updatePrompt == nil
        }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, idle(self) else { return }
                withAnimation(NotchMotion.unfold) {
                    self.model.isExpanded = false
                    self.model.hoveredIndex = nil
                }
                self.setPointing(false)
                self.updateInteractiveRects()
                let off = DispatchWorkItem { [weak self] in
                    MainActor.assumeIsolated {
                        guard let self, idle(self) else { return }
                        self.hideWork = nil
                        self.panel?.orderOut(nil)
                    }
                }
                self.hideWork = off
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.45, execute: off)
            }
        }
        hideWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: work)
    }

    /// Open the notch and show a usage reset notification modal card.
    ///
    /// Returns whether the card was actually shown: only a notch with no panel has nowhere
    /// to put it, and the caller owes the user another way of hearing about it.
    /// One deadline takes it down, held while the pointer is on the card; the
    /// fleet only calls this when the card slot is free (`NotchFleet`'s queue).
    @discardableResult
    func showResetAlert(_ event: UsageResetEvent, duration: TimeInterval = 5.0) -> Bool {
        guard canShowCards else {
            Log.usage.debug("reset alert skipped: no panel")
            return false
        }
        model.noticeHeld = false
        model.resetAlertLife = NoticePrompt.Life(seconds: duration)
        model.activeResetAlert = event
        peek(for: duration, focusing: nil)
        startNoticeDeadline(after: duration)
        // Up while a screenshot session or the wheel is open: its clock waits too.
        if model.noticeToolHold { holdNotice(true, tool: true) }
        return true
    }

    /// Takes the notice on the card down when it is `providerID`'s (its request was answered).
    func withdrawResetAlert(providerID: String) {
        guard model.activeResetAlert?.providerID == providerID else { return }
        model.activeResetAlert = nil
        updateInteractiveRects()
    }

    /// The notice's one scheduled deadline.
    private func startNoticeDeadline(after seconds: TimeInterval) {
        noticeWork?.cancel()
        noticeLeft = seconds
        noticeRunStart = Date()
        guard let event = model.activeResetAlert else { return }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.model.activeResetAlert == event else { return }
                self.model.activeResetAlert = nil
                self.updateInteractiveRects()
            }
        }
        noticeWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: work)
    }

    /// The pointer came onto the notice (the clock stops where it is) or left it
    /// (the clock goes on with what was left).
    private func holdNotice(_ held: Bool, tool: Bool) {
        guard model.activeResetAlert != nil,
              model.updatePrompt == nil, model.diskImagePrompt == nil else {
            if !held, peekUntil == Date.distantFuture { peekUntil = nil }
            return
        }
        if held {
            guard let started = noticeRunStart else { return }
            noticeWork?.cancel()
            noticeWork = nil
            noticeLeft = max(0, noticeLeft - Date().timeIntervalSince(started))
            noticeRunStart = nil
            // The peek that holds the notch open would end on its old schedule:
            // park it until the clock goes on.
            if tool {
                peekWork?.cancel()
                peekWork = nil
                peekUntil = Date.distantFuture
            }
        } else if noticeRunStart == nil {
            let left = max(noticeLeft, 0.5)
            startNoticeDeadline(after: left)
            if peekUntil == Date.distantFuture {
                peek(for: left, focusing: nil)
                if peekUntil == Date.distantFuture { peekUntil = nil }  // the panel was gone
            }
        }
    }

    /// The notice is gone: drop its deadline, and tell the fleet so the next
    /// one can come out once this one has gone in.
    private func noticeEnded() {
        noticeWork?.cancel()
        noticeWork = nil
        noticeRunStart = nil
        if peekUntil == Date.distantFuture { peekUntil = nil }
        model.resetAlertLife = nil
        hideAgainIfHidden()
        onNoticeGone?()
    }

    /// How long after a peek folds a click still counts as answering it. Covers
    /// the reach for the mouse that started while the notch was still open.
    private static let focusGrace: TimeInterval = 2

    /// Raise the terminal the peeked session is running in, if the offer stands.
    private func takePendingFocus() -> Bool {
        guard let pending = pendingFocus, pending.until > Date() else {
            pendingFocus = nil
            return false
        }
        pendingFocus = nil
        // The same exact-tab jump a session row gives, not just the app.
        Task { _ = await SessionFocus.focus(pid: pending.pid) }
        return true
    }

    /// Tear down a controller whose display is gone: hide first so no panel
    /// lingers on a screen that no longer exists, then stop its timers and
    /// monitors — a retired controller that kept polling would relocate
    /// another display's panel underneath a parked pointer.
    func retire() {
        apply(.hidden)
        stop()
    }

    /// Clicking the open notch pins it, so it stays put while you read it.
    func togglePinned() {
        model.isPinned.toggle()
        if model.isPinned {
            foldWork?.cancel()
            foldWork = nil
            withAnimation(NotchMotion.unfold) { model.isExpanded = true }
        }
        updateInteractiveRects()
    }

    /// Which ring a point along the panel is on.
    ///
    /// The copy that carries the readings, and only that one: the mirror is the
    /// container with nothing in it, so there is nothing on it to point at.
    func cellIndex(along: CGFloat) -> Int? {
        let wing = model.cellWing
        guard model.alongWithin(along, of: wing) != nil else { return nil }
        let pitch = model.cellPitch * model.sizeScale
        for index in model.snapshots.indices {
            let centre = model.ringAlong(index: index, in: wing)
            if abs(along - centre) <= pitch / 2 { return index }
        }
        return nil
    }

    // MARK: - Odds and ends

    private func startClock() {
        // Keeps "Resets in N min" from going stale while the tooltip is open.
        //
        // Once a second, where it used to be once every thirty. A card open on
        // "Resets in 12 min" was up to half a minute behind the clock it is read
        // against, and inside the last minute — where the copy now counts in
        // seconds — thirty seconds is most of what is left. What keeps that
        // cheap is `tickClock`: a second is only *published* when something on
        // screen counts in them.
        // Rate: 1 s while a hover card is open (it counts in seconds), 30 s otherwise, which is
        // all a folded or card-less notch ever published (#148: the 1 s wake ran all day to
        // return early 29 times in 30). A card opening re-times it and reads the clock at once.
        scheduleClock(fast: model.hoveredIndex != nil)
        model.$hoveredIndex
            .map { $0 != nil }
            .removeDuplicates()
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] hovering in
                MainActor.assumeIsolated {
                    guard let self, self.clockTimer != nil else { return }
                    self.scheduleClock(fast: hovering)
                    if hovering { self.tickClock() }
                }
            }
            .store(in: &cancellables)
    }

    private func scheduleClock(fast: Bool) {
        clockTimer?.invalidate()
        let interval: TimeInterval = fast ? 1 : 30
        let timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tickClock() }
        }
        timer.tolerance = fast ? 0.1 : 5
        RunLoop.main.add(timer, forMode: .common)
        clockTimer = timer
    }

    /// Publishes the time to the view, as often as the view has a use for it.
    ///
    /// `model.now` is `@Published` and every card is drawn against it, so
    /// setting it is a SwiftUI update of the whole notch. That is worth doing
    /// every second while a card is open and being read, and worth doing at the
    /// old thirty-second pace when the notch is folded away — nothing on a
    /// collapsed notch is measured in seconds, so redrawing one every second for
    /// the rest of the day buys nobody anything.
    private func tickClock() {
        let now = Date()
        // Only a hover card counts in seconds. An open notch with no card
        // under the pointer (pinned, Always show, a peek) shows rings and
        // percentages, so it waits for the thirty-second beat like a folded one.
        let readsAsClock = model.hoveredIndex != nil
        // 25, not 30: the timer's own tolerance must not make a beat miss its turn.
        guard readsAsClock || now.timeIntervalSince(model.now) >= 25 else { return }
        model.now = now
    }

    /// Pulse fork: right-click offers one thing — Quit. No Dock tile and no
    /// menu-bar item exist, so this is the way out.
    private func contextMenu() -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.addItem(
            withTitle: L10n.t("Quit Pulse"),
            action: #selector(NSApplication.terminate(_:)),
            keyEquivalent: "q"
        ).isEnabled = true
        return menu
    }

    private lazy var menuActions = MenuActions(
        refresh: { [weak self] in self?.onRefresh?() },
        signIn: { [weak self] index in self?.signInItems[safe: index]?.action() },
        togglePinned: { [weak self] in self?.togglePinned() }
    )
}


/// A menu item needs an Objective-C target, which a `@MainActor` Swift class
/// with closures cannot be directly.
final class MenuActions: NSObject {
    private let refresh: () -> Void
    private let signIn: (Int) -> Void
    private let pin: () -> Void

    init(
        refresh: @escaping () -> Void,
        signIn: @escaping (Int) -> Void,
        togglePinned: @escaping () -> Void
    ) {
        self.refresh = refresh
        self.signIn = signIn
        self.pin = togglePinned
    }

    @objc func refreshNow(_ sender: Any?) { refresh() }
    @objc func togglePinned(_ sender: Any?) { pin() }

    @objc func signIn(_ sender: Any?) {
        guard let item = sender as? NSMenuItem else { return }
        signIn(item.tag)
    }
}

extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

/// Receives a display link's ticks for something that is not an `NSObject`.
final class DisplayTick: NSObject {
    private let action: (CADisplayLink) -> Void
    init(_ action: @escaping (CADisplayLink) -> Void) { self.action = action }
    @objc func tick(_ link: CADisplayLink) { action(link) }
}
#endif
