import Foundation

/*
 Provenance (exact donor revision):
 - baryhuang/claude-code-by-agents @ 7655042eb986bf146079032726a7347ac4547c0e
 - OpenAgents/Models/Attachment.swift (PendingAttachment fields)
 - OpenAgents/State/AppRouter.swift (pending external URL handoff)

 This is the smallest CodeRight-named value type needed to preserve the
 donor router's deferred external-file lifecycle. Target composer code can
 consume it without importing donor models.
 */

public struct DeepLinkPendingAttachment: Identifiable, Sendable, Equatable {
    public let id: UUID
    public let filename: String
    public let contentType: String
    public let data: Data

    public init(
        id: UUID = UUID(),
        filename: String,
        contentType: String,
        data: Data
    ) {
        self.id = id
        self.filename = filename
        self.contentType = contentType
        self.data = data
    }

    public var isImage: Bool { contentType.hasPrefix("image/") }
    public var size: Int { data.count }
}
