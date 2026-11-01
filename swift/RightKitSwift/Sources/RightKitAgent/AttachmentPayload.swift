// Provenance: CodeRight integration seam for the direct attachment port from
// baryhuang/claude-code-by-agents at 7655042eb986bf146079032726a7347ac4547c0e.
// This protocol deliberately carries no wire-format or upload assumptions.

import Foundation

/// Minimal transport-facing shape for a pending attachment.
///
/// The composer can collect `PendingAttachment` values without depending on
/// `MessagePayload`. The networking owner can later encode any value conforming
/// to this protocol as the daemon's attachment field or upload part.
protocol AttachmentPayload: Sendable {
    var filename: String { get }
    var contentType: String { get }
    var data: Data { get }
}

extension PendingAttachment: AttachmentPayload {}
