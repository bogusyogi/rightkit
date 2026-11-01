#if os(iOS)
// Provenance: direct behavioral transplant from Bot commit
// 6951c40156ba0916a10b6e49e5051b7785198b93, ios/App/LiveActivities.swift.
// the app-specific event/state adapters are isolated in this file.

@preconcurrency import ActivityKit
import Combine
import Foundation
import RightKitLiveActivity

/// Donor-shaped activity kind. `toReview` remains representable but, as in the donor,
/// is excluded from Live Activities until it becomes an actionable waiting state.
public enum LiveActivityUpdateKind: String, Equatable, Sendable {
    case needsYou
    case working
    case toReview
}

/// Minimal bot identity needed by ActivityKit. A the app event adapter supplies `face` and
/// `color`; this keeps the coordinator independent from daemon/session model types.
public struct LiveActivityBot: Equatable, Sendable {
    public let id: String
    public let threadId: String
    public let name: String
    public let color: String
    public let face: String

    public init(
        id: String,
        threadId: String,
        name: String,
        color: String,
        face: String = "idle"
    ) {
        self.id = id
        self.threadId = threadId
        self.name = name
        self.color = color
        self.face = face
    }
}

/// A pending card projected by the event adapter. Values are intentionally plain so they can
/// be built from either remote `ProtocolEvent` rows or local runtime events.
public struct LiveActivityCard: Equatable, Sendable {
    public let isPending: Bool
    public let requestId: String?
    public let options: [String]
    public let isPermission: Bool
    public let title: String

    public init(
        isPending: Bool = false,
        requestId: String? = nil,
        options: [String] = [],
        isPermission: Bool = false,
        title: String = ""
    ) {
        self.isPending = isPending
        self.requestId = requestId
        self.options = options
        self.isPermission = isPermission
        self.title = title
    }
}

/// One update consumed by the donor coordinator semantics.
public struct LiveActivityUpdate: Equatable, Sendable {
    public let bot: LiveActivityBot
    public let kind: LiveActivityUpdateKind
    public let line: String
    public let card: LiveActivityCard?

    public init(
        bot: LiveActivityBot,
        kind: LiveActivityUpdateKind,
        line: String,
        card: LiveActivityCard? = nil
    ) {
        self.bot = bot
        self.kind = kind
        self.line = line
        self.card = card
    }
}

/// Snapshot equivalent of the donor's `CompanionState` subset used by Live Activities.
public struct LiveActivityState: Equatable, Sendable {
    public let updates: [LiveActivityUpdate]

    public init(updates: [LiveActivityUpdate] = []) {
        self.updates = updates
    }
}

/// Event adapter seam: map the app `ProtocolEvent` / local runtime events into this
/// donor-shaped state publisher, then let the coordinator own all ActivityKit lifecycle.
/// The adapter also handles the app-side answer route; the widget process only invokes intent.
@MainActor
public protocol LiveActivityEventAdapter: AnyObject {
    var liveActivityState: AnyPublisher<LiveActivityState, Never> { get }

    func answer(
        threadId: String,
        requestId: String,
        choice: String,
        isPermission: Bool
    ) async
}

/// Mirrors the adapter's update stream into one Live Activity through RightKitLiveActivity's
/// `LiveActivityHost` (fenced start/adopt/update/end). The host runs a single activity per type,
/// so the bot that needs the user is shown first, otherwise the first working bot.
@MainActor
public final class LiveActivityCoordinator {
    private var cancellable: AnyCancellable?
    private let host = LiveActivityHost<BotActivityAttributes>()
    private var shownBotId: String?
    private var lastSent: BotActivityAttributes.ContentState?
    /// When each bot's current kind began, so an update does not reset its clock.
    private var since: [String: (kind: String, at: Date)] = [:]

    /// Attach once to a the app event adapter. Replacing an existing subscription is safe.
    public func attach(to adapter: any LiveActivityEventAdapter) {
        // Answer from the island: the intent runs in this app process.
        AnswerApprovalIntent.handler = { [weak adapter] threadId, requestId, choice, isPermission in
            await adapter?.answer(
                threadId: threadId,
                requestId: requestId,
                choice: choice,
                isPermission: isPermission
            )
        }
        cancellable = adapter.liveActivityState
            .debounce(for: .milliseconds(400), scheduler: DispatchQueue.main)
            .sink { [weak self] state in
                Task { @MainActor [weak self] in
                    await self?.sync(state)
                }
            }
    }

    private func sync(_ state: LiveActivityState) async {
        let wanted = state.updates.filter { $0.kind != .toReview }
        let wantedIds = Set(wanted.map(\.bot.id))
        since = since.filter { wantedIds.contains($0.key) }
        guard let update = wanted.first(where: { $0.kind == .needsYou }) ?? wanted.first else {
            if shownBotId != nil {
                shownBotId = nil
                lastSent = nil
                await host.end(nil, policy: .immediate)
            }
            return
        }
        let bot = update.bot
        let kind = update.kind == .needsYou ? "needsYou" : "working"
        if since[bot.id]?.kind != kind { since[bot.id] = (kind, Date()) }
        let content = BotActivityAttributes.ContentState(
            face: bot.face,
            kind: kind,
            headline: update.kind == .needsYou ? "\(bot.name) needs you" : "\(bot.name) is working",
            line: update.line.isEmpty ? (update.card?.title ?? "") : update.line,
            requestId: update.card?.isPending == true ? update.card?.requestId : nil,
            options: update.card?.isPending == true ? (update.card?.options ?? []) : [],
            isPermission: update.card?.isPermission ?? false,
            since: since[bot.id]?.at ?? Date()
        )
        let previous = shownBotId == bot.id ? lastSent : nil
        if previous == content { return }
        // A bot stopping for you is worth an alert: the island opens and the lock screen
        // lights up. Working is not.
        let alert: AlertConfiguration? = update.kind == .needsYou
            ? AlertConfiguration(
                title: LocalizedStringResource(stringLiteral: content.headline),
                body: LocalizedStringResource(stringLiteral: content.line),
                sound: .default
            )
            : nil
        let activityContent = ActivityContent(state: content, staleDate: nil)
        if shownBotId == bot.id, host.activity != nil {
            let newAsk = update.kind == .needsYou && previous?.requestId != content.requestId
            await host.update(activityContent, alert: newAsk ? alert : nil)
        } else {
            let attributes = BotActivityAttributes(
                botId: bot.id, threadId: bot.threadId, name: bot.name, color: bot.color
            )
            let botId = bot.id
            do {
                let outcome = try await host.start(
                    attributes: attributes,
                    content: activityContent,
                    isMatchingOrphan: { $0.attributes.botId == botId }
                )
                // A fresh activity cannot alert on request; one immediate alerting update does it.
                if outcome == .adopted || alert != nil {
                    await host.update(activityContent, alert: alert)
                }
            } catch {
                shownBotId = nil
                lastSent = nil
                return
            }
        }
        shownBotId = bot.id
        lastSent = content
    }
}
#endif
