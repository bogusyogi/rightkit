import CryptoKit
import Foundation

/// Must match desktop cloud_sync::logical_cloud_id byte for byte. A stable
/// identity lets old Mac records, edits & tombstones refer to the same row.
enum TranscriptCloudRecordIdentity {
    struct Header {
        let id: UUID
        let updatedAt: Date
    }

    static func header(
        id: Any?,
        updatedAt: Any?,
        recordName: String,
        fallbackDate: Date?
    ) -> Header? {
        let raw = ((id as? String)?.isEmpty == false ? id as? String : nil) ?? recordName
        guard !raw.isEmpty, let date = (updatedAt as? Date) ?? fallbackDate else { return nil }
        return Header(id: logicalID(raw), updatedAt: date)
    }

    static func logicalID(_ raw: String) -> UUID {
        if raw.hasPrefix("cloudkit-"), let id = UUID(uuidString: String(raw.dropFirst(9))) {
            return id
        }
        if let id = UUID(uuidString: raw) { return id }
        var input = Data("heardright-cloud-id\0".utf8)
        input.append(contentsOf: raw.utf8)
        var bytes = Array(SHA256.hash(data: input).prefix(16))
        bytes[6] = (bytes[6] & 0x0f) | 0x50
        bytes[8] = (bytes[8] & 0x3f) | 0x80
        return UUID(uuid: (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                           bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
    }
}
