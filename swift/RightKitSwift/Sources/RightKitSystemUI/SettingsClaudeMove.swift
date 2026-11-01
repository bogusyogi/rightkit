#if os(macOS)
import RightKitEngineClient
import Foundation
import Observation
import RightKitHub
import SwiftUI

/// One dry run of a Claude chat move (engine `claude_move_offers`, core `move_plan`): counts only.
struct SettingsClaudeMovePlan: Decodable, Sendable, Equatable, Identifiable {
    var from: String
    var to: String
    var wouldAdd: Int
    var wouldUpdateIfNewer: Int
    var planId: String
    var targetRunning: Bool
    var id: String { planId }

    enum CodingKeys: String, CodingKey {
        case from, to
        case wouldAdd = "would_add"
        case wouldUpdateIfNewer = "would_update_if_newer"
        case planId = "plan_id"
        case targetRunning = "target_running"
    }
}

private struct SettingsClaudeMoveApplied: Decodable, Sendable {
    var filesChanged: Int?
    enum CodingKeys: String, CodingKey { case filesChanged = "files_changed" }
}

private struct SettingsClaudeMoveApply: Encodable, Sendable {
    var planId: String
}

/// "Move chats" (#353): what the account scan found in another Claude folder that the signed-in one
/// lacks. Apply sends only the id of the dry run the user confirmed; Pulse never closes Claude.
@MainActor
@Observable
final class SettingsClaudeMoveModel {
    private let engine: any EngineClient
    private(set) var offers: [SettingsClaudeMovePlan] = []
    var message: String?
    /// True while the confirm is open: a reload would replace the plan the user is reading.
    var confirming = false

    init(engine: any EngineClient) {
        self.engine = engine
    }

    func load() async {
        if confirming { return }
        offers = (try? await engine.call("claude_move_offers", EngineEmpty(), as: [SettingsClaudeMovePlan].self, timeout: .seconds(30))) ?? []
    }

    func apply(_ plan: SettingsClaudeMovePlan) async {
        do {
            let done = try await engine.call(
                "claude_move_apply", SettingsClaudeMoveApply(planId: plan.planId),
                as: SettingsClaudeMoveApplied.self, timeout: .seconds(120))
            message = "Moved chats from \(plan.from) into \(plan.to) (\(done.filesChanged ?? 0) files, backed up first)."
        } catch {
            message = SettingsFormat.describe(error)
        }
        await load()
    }
}

/// The Accounts row listing each offered move; the confirm shows the dry-run counts.
struct SettingsClaudeMoveRow: View {
    let model: SettingsClaudeMoveModel
    @State private var pending: SettingsClaudeMovePlan?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.spacing.sm) {
            ForEach(model.offers) { offer in
                HStack(spacing: theme.spacing.sm) {
                    Text(verbatim: "\(offer.from) has chats \(offer.to) lacks.")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                    Spacer()
                    SettingsButton(
                        "Move chats…", enabled: !offer.targetRunning,
                        hint: offer.targetRunning ? "Claude is running on the target account; close it yourself first" : nil
                    ) { model.confirming = true; pending = offer }
                }
            }
            if let message = model.message {
                Text(verbatim: message)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
            }
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, theme.spacing.sm + 2)
        .frame(maxWidth: .infinity, alignment: .leading)
        .confirmationDialog(
            "Move chats?", isPresented: Binding(get: { pending != nil }, set: { if !$0 { pending = nil; model.confirming = false } }),
            presenting: pending
        ) { plan in
            Button("Move") {
                pending = nil
                model.confirming = false
                Task { await model.apply(plan) }
            }
            Button("Cancel", role: .cancel) { pending = nil; model.confirming = false }
        } message: { plan in
            Text(
                "From \(plan.from) into \(plan.to): \(plan.wouldAdd) to add, \(plan.wouldUpdateIfNewer) to update if newer. A backup is taken first (pulse claude restore undoes it). \(plan.from) is left as it is. Claude is not closed."
            )
        }
    }
}
#endif
