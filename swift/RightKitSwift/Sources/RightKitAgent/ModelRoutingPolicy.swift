import Foundation
import Observation

/// One persisted choice of provider account plus model. Credentials remain in
/// `ProviderAccountStore`; routing policy only stores non-secret identifiers.
struct ModelReference: Codable, Hashable, Identifiable, Sendable {
    let accountID: String
    let modelID: String

    var id: String { "\(accountID)::\(modelID)" }
}

/// Stable roles exposed in iOS settings. A role without an explicit model
/// inherits orchestrator choice when mobile subagent execution is enabled.
enum LocalAgentRole: String, CaseIterable, Codable, Identifiable, Sendable {
    case explorer
    case implementer
    case reviewer
    case verifier

    var id: String { rawValue }

    var title: String {
        switch self {
        case .explorer: return "Explorer"
        case .implementer: return "Implementer"
        case .reviewer: return "Reviewer"
        case .verifier: return "Verifier"
        }
    }

    var detail: String {
        switch self {
        case .explorer: return "Finds relevant code & facts."
        case .implementer: return "Builds bounded changes."
        case .reviewer: return "Reviews design & implementation."
        case .verifier: return "Checks requested behavior independently."
        }
    }
}

/// Persisted provider inventory plus user routing choices. Provider catalog
/// responses are advisory metadata; only models explicitly enabled here reach
/// chat picker or assignment menus.
@MainActor
@Observable
final class ModelRoutingStore {
    private static let defaultsKey = "app.coderight.modelRouting"

    private struct Payload: Codable {
        var catalogs: [String: [String]] = [:]
        var refreshedAt: [String: Date] = [:]
        var visibleModels: [String: Set<String>] = [:]
        var orchestrator: ModelReference?
        var roleModels: [LocalAgentRole: ModelReference] = [:]
    }

    private let defaults: UserDefaults
    private var payload = Payload()

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        guard let data = defaults.data(forKey: Self.defaultsKey),
              let decoded = try? JSONDecoder().decode(Payload.self, from: data)
        else { return }
        payload = decoded
    }

    func catalog(for accountID: String) -> [String] {
        payload.catalogs[accountID] ?? []
    }

    func refreshedAt(for accountID: String) -> Date? {
        payload.refreshedAt[accountID]
    }

    func isVisible(_ modelID: String, for accountID: String) -> Bool {
        payload.visibleModels[accountID]?.contains(modelID) ?? false
    }

    func enabledModels(for accountID: String, constrainedTo modelIDs: [String]? = nil) -> [String] {
        let source = modelIDs ?? catalog(for: accountID)
        let visible = payload.visibleModels[accountID] ?? []
        return source.filter(visible.contains)
    }

    func setCatalog(_ modelIDs: [String], for accountID: String) {
        let catalog = Array(Set(modelIDs.filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty })).sorted()
        let priorVisible = payload.visibleModels[accountID]
        payload.catalogs[accountID] = catalog
        payload.refreshedAt[accountID] = Date()
        // First discovery enables every returned model. Later refreshes retain
        // user choices & discard models provider no longer reports.
        payload.visibleModels[accountID] = priorVisible.map { $0.intersection(Set(catalog)) } ?? Set(catalog)
        repairReferences()
        persist()
    }

    func setVisible(_ visible: Bool, modelID: String, accountID: String) {
        var models = payload.visibleModels[accountID] ?? []
        if visible { models.insert(modelID) } else { models.remove(modelID) }
        payload.visibleModels[accountID] = models
        repairReferences()
        persist()
    }

    func setOrchestrator(_ reference: ModelReference?) {
        payload.orchestrator = validated(reference)
        persist()
    }

    func orchestrator() -> ModelReference? { validated(payload.orchestrator) }

    func model(for role: LocalAgentRole) -> ModelReference? {
        validated(payload.roleModels[role])
    }

    func setModel(_ reference: ModelReference?, for role: LocalAgentRole) {
        if let validated = validated(reference) { payload.roleModels[role] = validated }
        else { payload.roleModels.removeValue(forKey: role) }
        persist()
    }

    func availableReferences(accounts: [ProviderAccount]) -> [ModelReference] {
        accounts.flatMap { account in
            enabledModels(for: account.id).map { ModelReference(accountID: account.id, modelID: $0) }
        }
    }

    func preferredModel(for accountID: String) -> String? {
        guard let choice = orchestrator(), choice.accountID == accountID else { return nil }
        return choice.modelID
    }

    func removeAccount(_ accountID: String) {
        payload.catalogs.removeValue(forKey: accountID)
        payload.refreshedAt.removeValue(forKey: accountID)
        payload.visibleModels.removeValue(forKey: accountID)
        if payload.orchestrator?.accountID == accountID { payload.orchestrator = nil }
        payload.roleModels = payload.roleModels.filter { $0.value.accountID != accountID }
        persist()
    }

    private func validated(_ reference: ModelReference?) -> ModelReference? {
        guard let reference,
              payload.visibleModels[reference.accountID]?.contains(reference.modelID) == true
        else { return nil }
        return reference
    }

    private func repairReferences() {
        payload.orchestrator = validated(payload.orchestrator)
        payload.roleModels = payload.roleModels.compactMapValues(validated)
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(payload) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }
}
