#if os(macOS)
import Foundation

let macMediaProtocolVersion: UInt32 = 1
let maxJsonlLineBytes = 1_048_576

enum JSONValue: Codable {
    case null, bool(Bool), number(Double), string(String), array([JSONValue]), object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Double.self) { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([JSONValue].self) { self = .array(value) }
        else { self = .object(try container.decode([String: JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    var objectValue: [String: JSONValue]? { if case .object(let value) = self { return value }; return nil }
    var stringValue: String? { if case .string(let value) = self { return value }; return nil }
    var arrayValue: [JSONValue]? { if case .array(let value) = self { return value }; return nil }
}

struct RequestEnvelope: Decodable {
    let protocolVersion: UInt32
    let requestId: String
    let operation: String
    let payload: JSONValue
}

struct NativeVideoOutputSpec: Codable {
    let width: UInt32
    let height: UInt32
    let frameRateNum: UInt32
    let frameRateDen: UInt32
}

struct NativeAudioOutputSpec: Codable {
    let sampleRate: UInt32
    let channels: UInt16
}

struct TimelineRenderRequest: Codable {
    let schemaVersion: UInt32
    let lockedCutSha256: String
    let graph: JSONValue
    let outputPath: String
    let allowedRoots: [String]
    let video: NativeVideoOutputSpec
    let audio: NativeAudioOutputSpec
    let mode: String

    private enum CodingKeys: String, CodingKey { case schemaVersion, lockedCutSha256, graph, outputPath, allowedRoots, video, audio, mode }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let keys = Set(c.allKeys)
        guard keys.count == 8 else { throw DecodingError.dataCorruptedError(forKey: .schemaVersion, in: c, debugDescription: "unknown timeline request field") }
        schemaVersion = try c.decode(UInt32.self, forKey: .schemaVersion)
        lockedCutSha256 = try c.decode(String.self, forKey: .lockedCutSha256)
        graph = try c.decode(JSONValue.self, forKey: .graph)
        outputPath = try c.decode(String.self, forKey: .outputPath)
        allowedRoots = try c.decode([String].self, forKey: .allowedRoots)
        video = try c.decode(NativeVideoOutputSpec.self, forKey: .video)
        audio = try c.decode(NativeAudioOutputSpec.self, forKey: .audio)
        mode = try c.decode(String.self, forKey: .mode)
    }
}

struct TimelineRenderResult: Codable {
    let schemaVersion: UInt32
    let artifactSha256: String
    let pixelSha256: String
    let audioSha256: String
    let duration: MacMediaRationalTime
    let renderedFrames: UInt64
    let audioFrames: UInt64
    let nodeReceipts: [JSONValue]
}

struct MacAssetInspectRequest: Codable {
    let sourcePath: String
    let allowedRoots: [String]
}

struct MacVisionBatchRequest: Codable {
    let frames: [MacVisionFrameRequest]
    let allowedRoots: [String]
}

struct ErrorPayload: Codable {
    let code: String
    let message: String
    let retryable: Bool
}

struct MacMediaCapabilities: Codable {
    let avFoundation: Bool
    let vision: Bool
    let caption: Bool
    let preview: Bool
    let audio: Bool
    let metal: Bool
    let osVersion: String
    let workerVersion: String
}

struct ResponseEnvelope: Encodable {
    let protocolVersion: UInt32
    let requestId: String
    let operation: String
    let ok: Bool
    let result: JSONValue?
    let error: ErrorPayload?
    let capabilities: MacMediaCapabilities?
    let elapsedNanoseconds: UInt64
}

func failure(_ request: RequestEnvelope, _ code: String, _ message: String, _ retryable: Bool = false, elapsed: UInt64 = 0) -> ResponseEnvelope {
    ResponseEnvelope(protocolVersion: macMediaProtocolVersion, requestId: request.requestId, operation: request.operation, ok: false, result: nil, error: ErrorPayload(code: code, message: message, retryable: retryable), capabilities: nil, elapsedNanoseconds: elapsed)
}
#endif
