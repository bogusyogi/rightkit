#if os(macOS)
import AppKit
import Combine
import RightKitOverlay
import SwiftUI

/// **Whether a toast's clock stands still**: true while a screenshot session
/// (the Snip / Screen / Window tools) or the tool wheel is open, so the owner can
/// snip a picture of a toast. Every toast timer — the notch's notice deadline and
/// its life ring, the disk-image and Send cards' expiries — pauses with what it
/// had left and goes on from there when this lets go. The Windows notch does the
/// same.
@MainActor
final class NoticeHold {
    static let shared = NoticeHold()

    /// The tool wheel's panel is up.
    var wheelOpen = false { didSet { update() } }
    /// Screenshot runs in flight (a counter: runs may overlap).
    var shots = 0 { didSet { update() } }
    private(set) var isHeld = false
    private var observers: [@MainActor (Bool) -> Void] = []

    /// Told whenever the hold begins or ends; for the app's lifetime.
    func observe(_ body: @escaping @MainActor (Bool) -> Void) { observers.append(body) }

    private func update() {
        guard (wheelOpen || shots > 0) != isHeld else { return }
        if wheelOpen || shots > 0 {
            settle()
        } else {
            // The wheel closing hands straight to the screenshot it started:
            // let that begin before the clocks are let go, so they don't blink.
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.settle() }
            }
        }
    }

    private func settle() {
        let held = wheelOpen || shots > 0
        guard held != isHeld else { return }
        isHeld = held
        for body in observers { body(held) }
    }
}

/// One notch per display: owns a `NotchWindowController` for each screen the
/// scope asks for and fans every reading, session and setting out to all of
/// them.
///
/// Each controller keeps its own model — hover, open state and screen size are
/// per-display facts, and sharing one model would unfold every notch when the
/// pointer reaches any of them. The fleet is the only thing that is shared: it
/// remembers the latest of everything so a controller created later (a display
/// plugged in at noon) starts with today's readings rather than empty rings.
@MainActor
final class NotchFleet {
    private var controllers: [CGDirectDisplayID: NotchWindowController] = [:]
    private var cancellables = Set<AnyCancellable>()
    /// Every model a reading has to reach.
    private var models: [NotchViewModel] { controllers.values.map(\.model) }

    private(set) var scope: NotchScreenScope
    private var edge: NotchEdge
    private var visibility: NotchVisibility = .onHover

    private var snapshots: [ProviderSnapshot] = []
    private var refreshing: Set<String> = []
    /// Exposed read-only rather than private: completion-watching needs the
    /// merged dict after a fan-out, the same way it read `controller.model
    /// .sessions` before there was more than one controller.
    private(set) var sessions: [String: [AgentSession]] = [:]
    /// Which display a single, unassigned controller should sit on. Only
    /// consulted by `.mainDisplay` — every controller under `.allDisplays`
    /// already has its own `assignedScreen`, which wins over this in
    /// `NotchWindowController.currentScreen()`.
    private var displayPreference: DisplayPreference = .followActiveWindow
    private var resetTimeFormat: ResetTimeFormat = .automatic
    private var accentColor: AccentColorChoice = .green
    private var watchLimit: Double = 0.70
    private var criticalLimit: Double = 0.90
    private var colorTransitionStyle: ColorTransitionStyle = .hardStep
    /// One choice for the whole fleet, like the edge and the size: a weekly
    /// ring on one display and not another would read as a bug.
    private var weeklyRing: WeeklyRing = .off
    private var weeklyRingDashed: Bool = false
    private var showsNotchReadings: Bool = true
    private var showsSettingsHandle = true
    /// Minimise (#375): the button's switch and the remembered state, one for the whole fleet.
    private var minimizeButton = true
    private var minimized = false
    private var showsHoverCards = true
    private var weeklyReading: Bool = false
    private var foldsForFullScreen = true
    private var surfaceStyle: NotchSurfaceStyle = .glass
    /// The ⌥-drag nudge along the current edge. One value for the whole
    /// fleet, the same as `edge` itself — displays do not each get their own
    /// edge, so they do not each get their own nudge either.
    private var alongOffset: CGFloat = 0
    /// One size for the whole fleet, for the same reason the edge is: a notch
    /// that were larger on one display than another would read as a bug.
    private var scale: CGFloat = 1

    /// Hooked up by the app delegate; driven by the notch's own chrome.
    var onRefresh: (() -> Void)?
    /// Every notch reports a look through the same closure: the store spaces
    /// them, so two screens' notches opening together are one fetch.
    var onLook: (() -> Void)?
    /// Pulse fork: told whenever the CPU/System hover card opens on any notch
    /// or the last one closes, so temperatures and fans are read only then.
    var onSystemHover: ((Bool) -> Void)?
    /// Pulse fork: told when the Disks hover card opens on any notch, so free and
    /// reclaimable space are both read at once for it.
    var onDisksHover: ((Bool) -> Void)?
    private var hoveredCards: [ObjectIdentifier: String] = [:]
    private var reportedSystemHover = false
    private var reportedDisksHover = false
    var onRefreshProvider: ((String) async -> Void)?
    var onOpenSettings: (() -> Void)?
    /// Pulse fork: open the hub on a section ("storage", "monitor", "accounts").
    var onOpenHub: ((String) -> Void)?
    /// The notch's answer to an update it offered.
    var onUpdateChoice: ((UpdateChoice) -> Void)?
    private var updatePrompt: UpdatePrompt?
    /// The notch's answer to a disk image card, and the pointer on it.
    var onDiskImageChoice: ((DiskImageChoice) -> Void)?
    var onDiskImageHover: ((Bool) -> Void)?
    private var diskImagePrompt: DiskImagePrompt?

    private var updatePending = false
    private var permissionsPending = false

    func apply(permissionsPending: Bool) {
        self.permissionsPending = permissionsPending
        for controller in controllers.values where controller.model.permissionsPending != permissionsPending {
            controller.model.permissionsPending = permissionsPending
        }
    }

    /// A newer version waiting — see `NotchViewModel.updatePending`.
    func apply(updatePending: Bool) {
        self.updatePending = updatePending
        for controller in controllers.values where controller.model.updatePending != updatePending {
            controller.model.updatePending = updatePending
        }
    }

    /// A disk image's card on every notch in the fleet, or nil to take it down.
    /// Returns whether any notch could show it: a Hidden notch comes out for it and
    /// goes again, so only a fleet with no panel has nowhere to ask.
    @discardableResult
    func apply(diskImagePrompt: DiskImagePrompt?) -> Bool {
        self.diskImagePrompt = diskImagePrompt
        var shown = false
        for controller in controllers.values where controller.apply(diskImagePrompt: diskImagePrompt) {
            shown = true
        }
        // The slot is free again: the notices that waited come out next.
        if diskImagePrompt == nil { drainNotices() }
        return diskImagePrompt == nil || shown
    }

    /// An update to offer in the notch, or how its install is going; nil once
    /// answered or done.
    func apply(updatePrompt: UpdatePrompt?) {
        self.updatePrompt = updatePrompt
        for controller in controllers.values {
            controller.apply(updatePrompt: updatePrompt)
        }
        if updatePrompt == nil { drainNotices() }
    }
    var onFocusSession: ((pid_t) -> Void)?
    var signInItems: [(title: String, action: () -> Void)] = []
    /// An ⌥-drag on any one panel settled at a new offset. Persisting it is
    /// Preferences' job, same division `apply(edge:)` already keeps.
    var onReposition: ((CGFloat) -> Void)?
    /// A move handle carried a notch to another edge. Persisting it is
    /// Preferences' job, the same division `onReposition` keeps.
    var onMoveToEdge: ((NotchEdge, CGFloat?) -> Void)?
    /// The minimise button was clicked on a notch: persisting the new state is Preferences' job.
    var onMinimizedChange: ((Bool) -> Void)?

    init(scope: NotchScreenScope, edge: NotchEdge) {
        self.scope = scope
        self.edge = edge
        // Every notch's toast waits while a screenshot session or the wheel is open.
        NoticeHold.shared.observe { [weak self] held in
            for model in self?.models ?? [] { model.noticeToolHold = held }
        }
    }

    /// Guards `apply(scope:)`/`apply(displayPreference:)` from reconciling
    /// before `show()` ever has: either one can otherwise build the fleet's
    /// first (and, for `.mainDisplay`, only) controller using whatever the
    /// app delegate has assigned to `onOpenSettings`/`onRefreshProvider`/etc.
    /// *so far* — nil, if either is called before that wiring runs — and
    /// `reconcile`'s own "keep the existing controller, just move it" fast
    /// path never revisits a controller's callbacks once made. A controller
    /// built with every action wired to nothing looks, from the screen,
    /// identical to a working one: it still opens, its rings still draw, and
    /// nothing about it says why every click does nothing.
    private var hasShown = false

    func show() {
        hasShown = true
        reconcile(screens: NSScreen.screens)
        NotificationCenter.default.publisher(
            for: NSApplication.didChangeScreenParametersNotification
        )
        .sink { [weak self] _ in
            MainActor.assumeIsolated {
                self?.reconcile(screens: NSScreen.screens)
            }
        }
        .store(in: &cancellables)
    }

    func stop() {
        cancellables.removeAll()
        for id in controllers.keys {
            controllers[id]?.retire()
        }
        controllers.removeAll()
    }

    // MARK: - Settings

    func apply(scope: NotchScreenScope) {
        self.scope = scope
        guard hasShown else { return }
        reconcile(screens: NSScreen.screens)
    }

    func apply(edge: NotchEdge) {
        self.edge = edge
        for controller in controllers.values {
            controller.apply(edge: edge)
        }
    }

    func apply(_ visibility: NotchVisibility) {
        self.visibility = visibility
        for controller in controllers.values {
            controller.apply(visibility)
        }
        reportHidden()
    }

    /// Pulse fork: told whenever the notch becomes invisible — set to Hide,
    /// or folded away on every display by a full-screen app — and when it is
    /// visible again, so the app can slow the System and Disks sampling.
    var onHiddenChange: ((Bool) -> Void)?
    private var reportedHidden = false

    private func noteHoverCard(_ controller: NotchWindowController, providerID: String?) {
        hoveredCards[ObjectIdentifier(controller)] = providerID
        reportSystemHover()
    }

    private func reportSystemHover() {
        // A retired controller's card is gone with it.
        let live = Set(controllers.values.map { ObjectIdentifier($0) })
        hoveredCards = hoveredCards.filter { live.contains($0.key) }
        let disksHovering = hoveredCards.values.contains(SystemProviders.disksID)
        if disksHovering != reportedDisksHover {
            reportedDisksHover = disksHovering
            onDisksHover?(disksHovering)
        }
        let hovering = hoveredCards.values.contains(SystemProviders.cpuID)
        guard hovering != reportedSystemHover else { return }
        reportedSystemHover = hovering
        onSystemHover?(hovering)
    }

    private func reportHidden() {
        reportSystemHover()
        let hidden = visibility == .hidden
            || (!controllers.isEmpty && controllers.values.allSatisfy(\.isFoldedForFullScreen))
        guard hidden != reportedHidden else { return }
        reportedHidden = hidden
        onHiddenChange?(hidden)
    }

    /// Only `.mainDisplay` has a single, unassigned controller for this to
    /// mean anything to — reconciling picks it a screen using the new
    /// preference the same way it does on a screen-parameter change.
    func apply(displayPreference: DisplayPreference) {
        self.displayPreference = displayPreference
        guard hasShown else { return }
        reconcile(screens: NSScreen.screens)
    }

    func apply(resetTimeFormat: ResetTimeFormat) {
        self.resetTimeFormat = resetTimeFormat
        for controller in controllers.values {
            controller.model.resetTimeFormat = resetTimeFormat
        }
    }

    func apply(foldsForFullScreen: Bool) {
        self.foldsForFullScreen = foldsForFullScreen
        for controller in controllers.values {
            controller.apply(foldsForFullScreen: foldsForFullScreen)
        }
    }

    func apply(showsNotchReadings: Bool) {
        self.showsNotchReadings = showsNotchReadings
        // Through the controller, which relays the window out: this one
        // changes the ring's size and so the notch's own length.
        for controller in controllers.values {
            controller.apply(showsNotchReadings: showsNotchReadings)
        }
    }

    func apply(showsSettingsHandle: Bool) {
        self.showsSettingsHandle = showsSettingsHandle
        for controller in controllers.values {
            controller.model.showsSettingsHandle = showsSettingsHandle
        }
    }

    func apply(minimizeButton: Bool) {
        self.minimizeButton = minimizeButton
        for controller in controllers.values {
            controller.model.setMinimize(enabled: minimizeButton, minimized: minimized)
        }
    }

    func apply(minimized: Bool) {
        self.minimized = minimized
        for controller in controllers.values {
            controller.model.setMinimize(enabled: minimizeButton, minimized: minimized)
        }
    }

    func apply(showsHoverCards: Bool) {
        self.showsHoverCards = showsHoverCards
        for controller in controllers.values {
            controller.model.showsHoverCards = showsHoverCards
            // Turning the cards off closes the one that is open.
            if !showsHoverCards { controller.model.hoveredIndex = nil }
        }
    }

    func apply(weeklyReading: Bool) {
        self.weeklyReading = weeklyReading
        for controller in controllers.values {
            controller.model.weeklyReading = weeklyReading
        }
    }

    func apply(weeklyRingDashed: Bool) {
        self.weeklyRingDashed = weeklyRingDashed
        for controller in controllers.values {
            controller.model.weeklyRingDashed = weeklyRingDashed
        }
    }

    func apply(weeklyRing: WeeklyRing) {
        self.weeklyRing = weeklyRing
        for controller in controllers.values {
            controller.model.weeklyRing = weeklyRing
        }
    }

    func apply(watchLimit: Double, criticalLimit: Double) {
        self.watchLimit = watchLimit
        self.criticalLimit = criticalLimit
        for controller in controllers.values {
            controller.model.watchLimit = watchLimit
            controller.model.criticalLimit = criticalLimit
        }
    }

    func apply(colorTransitionStyle: ColorTransitionStyle) {
        self.colorTransitionStyle = colorTransitionStyle
        for controller in controllers.values {
            controller.model.colorTransitionStyle = colorTransitionStyle
        }
    }

    func apply(accentColor: AccentColorChoice) {
        self.accentColor = accentColor
        for controller in controllers.values {
            controller.model.accentColor = accentColor
        }
    }

    func apply(surfaceStyle: NotchSurfaceStyle) {
        self.surfaceStyle = surfaceStyle
        for controller in controllers.values {
            controller.model.surfaceStyle = surfaceStyle
        }
    }

    func apply(alongOffset: CGFloat) {
        self.alongOffset = alongOffset
        for controller in controllers.values {
            controller.apply(alongOffset: alongOffset)
        }
    }

    /// Through `controller.apply(size:)` rather than by setting the model
    /// directly, because the panel has to be rebuilt around the new size —
    /// the same division `apply(edge:)` keeps.
    func apply(scale: CGFloat) {
        self.scale = scale
        for controller in controllers.values {
            controller.apply(scale: scale)
        }
    }

    // MARK: - Readings

    func setSnapshots(_ snapshots: [ProviderSnapshot]) {
        self.snapshots = snapshots
        for model in models {
            model.updateSnapshots(snapshots)
            model.refreshClock()
        }
    }

    /// The notch on the display under `point`, for the clipboard strip: nil when that display
    /// has no notch (the strip then uses its bottom layout, 7.1).
    func stripAnchor(at point: NSPoint) -> NotchWindowController.StripAnchor? {
        for controller in controllers.values {
            guard let anchor = controller.stripAnchor(), anchor.screen.frame.contains(point) else { continue }
            return anchor
        }
        return nil
    }

    /// The controller whose notch is on the display under `point`.
    func stripNotch(at point: NSPoint) -> NotchWindowController? {
        controllers.values.first { $0.currentScreen()?.frame.contains(point) == true }
    }

    /// Opens every panel for a moment, because something happened — the same
    /// announcement on every display rather than only the one you happen to
    /// be looking at.
    func peek(for duration: TimeInterval, focusing pid: pid_t?, evenWhenHidden: Bool = true) {
        for controller in controllers.values {
            controller.peek(for: duration, focusing: pid, evenWhenHidden: evenWhenHidden)
        }
    }

    // MARK: - The card queue (liquid-motion pass §4)

    /// A notice that arrived while the card slot was taken.
    private struct QueuedNotice {
        var event: UsageResetEvent
        var duration: TimeInterval
    }

    /// Notices wait their turn, oldest first: one card slot is shared by an
    /// update, a disk image or nearby sharing card (a question holds it until it
    /// is answered), and the notices that come meanwhile bud out one after
    /// another rather than replacing each other, or being lost behind it.
    private var noticeQueue: [QueuedNotice] = []
    private var noticeDrain: DispatchWorkItem?
    /// Enough to ride out a burst, not a backlog: the oldest is dropped past it.
    private static let noticeQueueLimit = 8

    /// Whether nothing is up in the card slot on any notch.
    private var cardSlotFree: Bool {
        // A pending update offer is not on a Hidden notch, so it does not hold the slot there.
        (updatePrompt == nil || visibility == .hidden) && diskImagePrompt == nil
            && controllers.values.allSatisfy { $0.model.activeResetAlert == nil }
    }

    /// Shows a usage reset notification modal on every panel, or queues it
    /// behind whatever is up. Returns whether at least one notch had somewhere
    /// to show the card (now or in its turn). A Hidden notch comes out for it and
    /// goes again; only a fleet with no panel returns false.
    @discardableResult
    func showResetAlert(_ event: UsageResetEvent, duration: TimeInterval = 5.0) -> Bool {
        guard controllers.values.contains(where: \.canShowCards) else { return false }
        if cardSlotFree && noticeQueue.isEmpty {
            return present(event, duration: duration)
        }
        noticeQueue.append(QueuedNotice(event: event, duration: duration))
        if noticeQueue.count > Self.noticeQueueLimit { noticeQueue.removeFirst() }
        return true
    }

    /// Takes down the notice whose `providerID` is given: one still queued and one on a card now.
    /// Other notices are left alone.
    func withdrawNotice(providerID: String) {
        noticeQueue.removeAll { $0.event.providerID == providerID }
        for controller in controllers.values { controller.withdrawResetAlert(providerID: providerID) }
    }

    private func present(_ event: UsageResetEvent, duration: TimeInterval) -> Bool {
        var shown = false
        for controller in controllers.values {
            shown = controller.showResetAlert(event, duration: duration) || shown
        }
        return shown
    }

    /// Brings up the next notice that waited, once the card slot is free — a beat
    /// after the last card, so it has gone back into the notch first.
    private func drainNotices() {
        noticeDrain?.cancel()
        noticeDrain = nil
        guard !noticeQueue.isEmpty else { return }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.noticeDrain = nil
                guard self.cardSlotFree, !self.noticeQueue.isEmpty else { return }
                let next = self.noticeQueue.removeFirst()
                _ = self.present(next.event, duration: next.duration)
            }
        }
        noticeDrain = work
        DispatchQueue.main.asyncAfter(deadline: .now() + NotchMotion.noticeIn + 0.05, execute: work)
    }

    func setRefreshing(_ ids: Set<String>) {
        self.refreshing = ids
        for controller in controllers.values where controller.model.refreshing != ids {
            controller.model.refreshing = ids
        }
    }

    func setSessions(providerID id: String, sessions live: [AgentSession]) {
        sessions[id] = live
        for controller in controllers.values {
            controller.model.updateSessions(live, for: id)
        }
    }

    // MARK: - Reconciliation

    /// Which notch a screen is, stable across polls while it stays connected.
    ///
    /// Every real display reports an `NSScreenNumber`; the fallback derives one
    /// from the origin so a screen without a number still gets a notch rather
    /// than none — at worst it is re-created when it moves.
    ///
    /// Nonisolated: pure computation on its argument, safe to call from anywhere.
    nonisolated static func key(for screen: NSScreen) -> CGDirectDisplayID {
        if let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber {
            return CGDirectDisplayID(number.uint32Value)
        }
        let origin = screen.frame.origin
        return CGDirectDisplayID(truncatingIfNeeded: Int(origin.x) * 31 + Int(origin.y))
    }

    private func reconcile(screens: [NSScreen]) {
        let desired: [NSScreen]
        switch scope {
        case .mainDisplay:
            desired = NotchGeometry.preferredScreen(from: screens, preference: displayPreference).map { [$0] } ?? []
        case .allDisplays:
            desired = screens
        }
        let keys = desired.map(Self.key)
        // Screen numbers are unique per display; if they ever are not, fall
        // back to a single notch rather than keying two panels as one.
        guard Set(keys).count == keys.count else {
            for id in controllers.keys {
                controllers[id]?.retire()
            }
            controllers.removeAll()
            if let main = NotchGeometry.preferredScreen(from: screens, preference: displayPreference) {
                controllers[Self.key(for: main)] = makeController(on: main)
            }
            return
        }
        // The common case — one notch following the menu-bar screen — keeps
        // its controller and moves it, the way a single panel always did,
        // instead of tearing a panel down and building another.
        if scope == .mainDisplay, controllers.count == 1,
           let screen = desired.first, let controller = controllers.values.first {
            controller.assignedScreen = screen
            controller.relocate()
            return
        }
        let plan = RightKitOverlayDisplays.orderedPlan(current: controllers.keys, desired: keys)
        for id in plan.remove {
            controllers[id]?.retire()
            controllers[id] = nil
        }
        for (screen, id) in zip(desired, keys) where plan.add.contains(id) {
            controllers[id] = makeController(on: screen)
        }
        reportHidden()
    }

    /// A controller that starts where every other one already is: same edge,
    /// same readings, same open state — a display plugged in at noon must not
    /// open on empty rings.
    private func makeController(on screen: NSScreen) -> NotchWindowController {
        let controller = NotchWindowController()
        controller.assignedScreen = screen
        controller.displayPreference = displayPreference
        controller.foldsForFullScreen = foldsForFullScreen
        controller.onFoldChange = { [weak self] in self?.reportHidden() }
        controller.model.edge = edge
        controller.model.alongOffset = alongOffset
        // Set before `show()`, so a display plugged in later builds its panel
        // at the current size rather than at medium and resizing a beat later.
        controller.prime(scale: scale)
        controller.model.resetTimeFormat = resetTimeFormat
        controller.model.accentColor = accentColor
        controller.model.watchLimit = watchLimit
        controller.model.criticalLimit = criticalLimit
        controller.model.colorTransitionStyle = colorTransitionStyle
        controller.model.weeklyRing = weeklyRing
        controller.model.weeklyRingDashed = weeklyRingDashed
        controller.model.showsNotchReadings = showsNotchReadings
        controller.model.showsSettingsHandle = showsSettingsHandle
        controller.model.showsHoverCards = showsHoverCards
        controller.model.setMinimize(enabled: minimizeButton, minimized: minimized)
        controller.model.weeklyReading = weeklyReading
        controller.model.surfaceStyle = surfaceStyle
        controller.model.noticeToolHold = NoticeHold.shared.isHeld

        controller.onRefresh = onRefresh
        controller.onLook = { [weak self] in self?.onLook?() }
        controller.onNoticeGone = { [weak self] in self?.drainNotices() }
        controller.onHoverCard = { [weak self, weak controller] id in
            guard let controller else { return }
            self?.noteHoverCard(controller, providerID: id)
        }
        controller.onRefreshProvider = onRefreshProvider
        let openSettings: () -> Void = { [weak self] in
            guard let self else { return }
            if self.permissionsPending { self.onOpenHub?("permissions") }
            else { self.onOpenSettings?() }
        }
        controller.onOpenSettings = openSettings
        controller.onOpenHub = onOpenHub
        controller.model.onOpenSettings = openSettings
        controller.model.onOpenHub = onOpenHub
        controller.model.onFocusSession = onFocusSession
        controller.model.onUpdateChoice = { [weak self] in self?.onUpdateChoice?($0) }
        controller.apply(updatePrompt: updatePrompt)
        controller.model.onDiskImageChoice = { [weak self] in self?.onDiskImageChoice?($0) }
        controller.model.onDiskImageHover = { [weak self] in self?.onDiskImageHover?($0) }
        controller.apply(diskImagePrompt: diskImagePrompt)
        controller.model.updatePending = updatePending
        controller.model.permissionsPending = permissionsPending
        controller.onReposition = onReposition
        controller.onMoveToEdge = onMoveToEdge
        controller.onMinimizedChange = { [weak self] in self?.onMinimizedChange?($0) }
        controller.signInItems = signInItems
        controller.model.updateSnapshots(snapshots)
        controller.model.refreshing = refreshing
        controller.model.sessions = sessions
        controller.model.now = Date()
        controller.apply(visibility)
        controller.show()
        return controller
    }
}
#endif
