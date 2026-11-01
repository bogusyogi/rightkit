import Foundation
import Observation

/// Durable metadata, rendered transcript, & opaque engine checkpoint for one
/// on-device session. Provider secrets never enter this record.
public struct QueuedInstruction: Codable, Identifiable, Equatable, Sendable {
    public var id: UUID = UUID()
    public var text: String
    public init(text: String) { self.text = text }
}

public struct LocalSessionRecord: Codable, Identifiable, Equatable, Sendable {
    public var id: String
    public var title: String
    public var workspaceId: String
    public var providerAccountId: String
    public var model: String
    public var createdAt: Date
    public var updatedAt: Date
    public var checkpointJson: String?
    public var messages: [TranscriptMessage]
    public var archived: Bool
    public var favorite: Bool
    public var permissionMode: String?
    public var reasoningEffort: String?
    public var parentSessionId: String?
    public var pendingInstructions: [QueuedInstruction]?

    public init(
        id: String = UUID().uuidString,
        title: String = "New local session",
        workspaceId: String,
        providerAccountId: String,
        model: String,
        createdAt: Date = Date(),
        updatedAt: Date = Date(),
        checkpointJson: String? = nil,
        messages: [TranscriptMessage] = [],
        archived: Bool = false,
        favorite: Bool = false,
        permissionMode: String? = nil,
        reasoningEffort: String? = nil,
        parentSessionId: String? = nil
    ) {
        self.id = id
        self.title = title
        self.workspaceId = workspaceId
        self.providerAccountId = providerAccountId
        self.model = model
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.checkpointJson = checkpointJson
        self.messages = messages
        self.archived = archived
        self.favorite = favorite
        self.permissionMode = permissionMode
        self.reasoningEffort = reasoningEffort
        self.parentSessionId = parentSessionId
    }
}

/// Small append-replace journal for local sessions. Writes are atomic & use
/// complete-until-first-unlock protection so background resume remains usable
/// after device unlock without exposing data before first unlock.
@MainActor
@Observable
public final class LocalSessionStore {
    public private(set) var sessions: [LocalSessionRecord] = []
    public private(set) var lastError: String?

    private let fileURL: URL

    public init(fileURL: URL? = nil) {
        let support = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        )[0]
        self.fileURL = fileURL ?? support
            .appendingPathComponent("LocalSessions", isDirectory: true)
            .appendingPathComponent("sessions.json")
        load()
    }

    public var activeSessions: [LocalSessionRecord] {
        sessions
            .filter { !$0.archived }
            .sorted {
                if $0.favorite != $1.favorite { return $0.favorite }
                return $0.updatedAt > $1.updatedAt
            }
    }

    @discardableResult
    public func create(
        workspaceId: String,
        providerAccountId: String,
        model: String,
        permissionMode: String = "acceptEdits",
        reasoningEffort: String? = nil
    ) -> LocalSessionRecord {
        let record = LocalSessionRecord(
            workspaceId: workspaceId,
            providerAccountId: providerAccountId,
            model: model,
            permissionMode: permissionMode,
            reasoningEffort: reasoningEffort
        )
        sessions.append(record)
        persist()
        return record
    }

    public func record(id: String) -> LocalSessionRecord? {
        sessions.first { $0.id == id }
    }

    public func checkpoint(
        id: String,
        checkpointJson: String?,
        messages: [TranscriptMessage]
    ) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[index].checkpointJson = checkpointJson
        sessions[index].messages = messages
        sessions[index].updatedAt = Date()
        if sessions[index].title == "New local session",
           let first = messages.first(where: { $0.role == .user })?.text {
            sessions[index].title = String(first.prefix(64))
        }
        persist()
    }

    public func updateConfiguration(id: String, accountID: String, model: String,
                                    permissionMode: String, reasoningEffort: String?) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[index].providerAccountId = accountID
        sessions[index].model = model
        sessions[index].permissionMode = permissionMode
        sessions[index].reasoningEffort = reasoningEffort
        sessions[index].updatedAt = Date()
        persist()
    }

    public func saveQueue(id: String, instructions: [QueuedInstruction]) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[index].pendingInstructions = instructions
        persist()
    }

    public func rename(id: String, title: String) {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[index].title = String(trimmed.prefix(100))
        sessions[index].updatedAt = Date()
        persist()
    }

    public func setArchived(id: String, archived: Bool) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[index].archived = archived
        sessions[index].updatedAt = Date()
        persist()
    }

    public func setFavorite(id: String, favorite: Bool) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[index].favorite = favorite
        sessions[index].updatedAt = Date()
        persist()
    }

    public func delete(id: String) {
        sessions.removeAll { $0.id == id }
        persist()
    }

    /// Undo restores the exact removed conversation, never its project files.
    public func restore(_ record: LocalSessionRecord) {
        guard !sessions.contains(where: { $0.id == record.id }) else { return }
        sessions.append(record)
        persist()
    }

    private func load() {
        guard let data = try? Data(contentsOf: fileURL) else { return }
        do {
            sessions = try JSONDecoder().decode([LocalSessionRecord].self, from: data)
        } catch {
            lastError = "Local session history could not be read: \(error.localizedDescription)"
        }
    }

    private func persist() {
        do {
            let parent = fileURL.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
            let data = try JSONEncoder().encode(sessions)
            // CodeRight-owned: atomic write to an arbitrary URL with a protection class (no shared consumer; RightKit issue withdrawn 2026-10-18)
            try data.write(
                to: fileURL,
                options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
            )
            lastError = nil
        } catch {
            lastError = "Local session history could not be saved: \(error.localizedDescription)"
        }
    }
}
