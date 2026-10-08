import Foundation

// The rightkit-record helper wire protocol (see crates/rightkit-record/src/protocol.rs).
// Every frame is a 4-byte big-endian length followed by one JSON object with a `kind`:
// request (core to helper), response, event and bye (helper to core). JSON only; pixels never
// cross the protocol, frames are reported as blake3 hashes.

public let maxFrameBytes = 16 * 1024 * 1024

public enum WireError: Error, CustomStringConvertible {
    case malformed(String)
    case tooLarge(Int)
    case truncated
    case io(String)

    public var description: String {
        switch self {
        case .malformed(let detail): return "malformed frame: \(detail)"
        case .tooLarge(let n): return "frame of \(n) bytes exceeds the \(maxFrameBytes) limit"
        case .truncated: return "stream ended inside a frame"
        case .io(let detail): return "io: \(detail)"
        }
    }
}

// MARK: framing

public enum Framing {
    /// One frame: 4-byte big-endian body length, then the body.
    public static func frame(body: Data) throws -> Data {
        if body.isEmpty { throw WireError.malformed("empty frame body") }
        if body.count > maxFrameBytes { throw WireError.tooLarge(body.count) }
        var out = Data(capacity: body.count + 4)
        var length = UInt32(body.count).bigEndian
        withUnsafeBytes(of: &length) { out.append(contentsOf: $0) }
        out.append(body)
        return out
    }

    public static func encode(_ object: [String: Any]) throws -> Data {
        try frame(body: try json(object))
    }

    public static func json(_ object: [String: Any]) throws -> Data {
        guard JSONSerialization.isValidJSONObject(object) else {
            throw WireError.malformed("object is not valid JSON (NaN or infinity?)")
        }
        return try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes])
    }

    public static func parse(_ body: Data) throws -> [String: Any] {
        guard let value = try? JSONSerialization.jsonObject(with: body), let object = value as? [String: Any] else {
            throw WireError.malformed("body is not a JSON object")
        }
        return object
    }

    /// Splits a byte buffer into frame bodies. Used by tests; the helper reads from a descriptor.
    public static func bodies(of stream: Data) throws -> [Data] {
        var bodies = [Data]()
        var offset = stream.startIndex
        while offset < stream.endIndex {
            guard stream.endIndex - offset >= 4 else { throw WireError.truncated }
            let length = stream[offset..<offset + 4].reduce(0) { ($0 << 8) | Int($1) }
            offset += 4
            if length == 0 { throw WireError.malformed("zero-length frame") }
            if length > maxFrameBytes { throw WireError.tooLarge(length) }
            guard stream.endIndex - offset >= length else { throw WireError.truncated }
            bodies.append(stream[offset..<offset + length])
            offset += length
        }
        return bodies
    }
}

/// Reads frames from a file descriptor. Blocks.
public final class FrameReader {
    private let fd: Int32
    public init(fd: Int32) { self.fd = fd }

    /// The next body; nil on a clean end of stream between frames.
    public func next() throws -> Data? {
        var header = [UInt8](repeating: 0, count: 4)
        guard try fill(&header, allowCleanEnd: true) else { return nil }
        let length = header.reduce(0) { ($0 << 8) | Int($1) }
        if length == 0 { throw WireError.malformed("zero-length frame") }
        if length > maxFrameBytes { throw WireError.tooLarge(length) }
        var body = [UInt8](repeating: 0, count: length)
        _ = try fill(&body, allowCleanEnd: false)
        return Data(body)
    }

    private func fill(_ buffer: inout [UInt8], allowCleanEnd: Bool) throws -> Bool {
        var filled = 0
        let total = buffer.count
        while filled < total {
            let n = buffer.withUnsafeMutableBytes { read(fd, $0.baseAddress! + filled, total - filled) }
            if n == 0 {
                if filled == 0 && allowCleanEnd { return false }
                throw WireError.truncated
            }
            if n < 0 {
                if errno == EINTR { continue }
                throw WireError.io(String(cString: strerror(errno)))
            }
            filled += n
        }
        return true
    }
}

/// Writes frames to a file descriptor. One lock keeps frames whole when events (capture worker)
/// and responses (request thread) interleave.
public final class FrameWriter {
    private let fd: Int32
    private let lock = NSLock()
    public init(fd: Int32) { self.fd = fd }

    public func write(_ object: [String: Any]) throws {
        let data = try Framing.encode(object)
        lock.lock()
        defer { lock.unlock() }
        try data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let n = Foundation.write(fd, raw.baseAddress! + offset, raw.count - offset)
                if n < 0 {
                    if errno == EINTR { continue }
                    throw WireError.io(String(cString: strerror(errno)))
                }
                offset += n
            }
        }
    }
}

// MARK: vocabulary

public struct Rect: Equatable {
    public var x: Double, y: Double, width: Double, height: Double
    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x; self.y = y; self.width = width; self.height = height
    }
    public var json: [String: Any] { ["x": x, "y": y, "width": width, "height": height] }
    static func parse(_ value: Any?, _ key: String) throws -> Rect {
        guard let o = value as? [String: Any] else { throw PlanError.field(key, "must be an object") }
        return Rect(x: try Fields.double(o, "x"), y: try Fields.double(o, "y"),
                    width: try Fields.double(o, "width"), height: try Fields.double(o, "height"))
    }
}

public enum Visibility: String {
    case offScreenVisible = "off_screen_visible"
    case onScreenVisible = "on_screen_visible"
    case appHidden = "app_hidden"
    case occluded
    case minimized
}

public enum DropReason: String {
    case backpressure, encoder
    case captureGap = "capture_gap"
    case unknown
}

public enum Permission: String {
    case granted, denied, undetermined
}

public enum Codec: String {
    case h264, hevc
}

/// Why a take cannot finish as a complete bundle. Stable kinds match `Failure::reason` in Rust.
public enum Failure: Equatable {
    case helperLost, diskFull, targetExited, permissionRevoked, maskGeometryUnknown
    case windowHidden, windowMinimized
    case protocolViolation(String)
    case io(String)

    public var json: [String: Any] {
        switch self {
        case .helperLost: return ["kind": "helper_lost"]
        case .diskFull: return ["kind": "disk_full"]
        case .targetExited: return ["kind": "target_exited"]
        case .permissionRevoked: return ["kind": "permission_revoked"]
        case .maskGeometryUnknown: return ["kind": "mask_geometry_unknown"]
        case .windowHidden: return ["kind": "window_hidden"]
        case .windowMinimized: return ["kind": "window_minimized"]
        case .protocolViolation(let detail): return ["kind": "protocol", "detail": detail]
        case .io(let detail): return ["kind": "io", "detail": detail]
        }
    }
}

public struct FrameSample: Equatable {
    public var index: UInt64
    public var hostNs: UInt64
    public var samplePtsNs: UInt64
    public var rawHash: String
    public init(index: UInt64, hostNs: UInt64, samplePtsNs: UInt64, rawHash: String) {
        self.index = index; self.hostNs = hostNs; self.samplePtsNs = samplePtsNs; self.rawHash = rawHash
    }
    var json: [String: Any] {
        ["index": index, "hostNs": hostNs, "samplePtsNs": samplePtsNs, "rawHash": rawHash]
    }
}

public enum HelperEvent: Equatable {
    case ready(FrameSample)
    case frame(FrameSample)
    case dropped(hostNs: UInt64, count: UInt32, intervalMs: Double, reason: DropReason)
    case geometry(hostNs: UInt64, boundsPt: Rect, scale: Double, dpi: UInt32)
    case failed(Failure)
    case exited
}

/// A refusal with a stable code (`Code::as_str` in Rust).
public struct HelperError: Error, Equatable {
    public var code: String
    public var message: String
    public init(_ code: String, _ message: String) { self.code = code; self.message = message }

    public static func argInvalid(_ m: String) -> HelperError { .init("arg_invalid", m) }
    public static func unsupported(_ m: String) -> HelperError { .init("unsupported", m) }
    public static func permissionDenied(_ m: String) -> HelperError { .init("permission_denied", m) }
    public static func maskGeometryUnknown(_ m: String) -> HelperError { .init("mask_geometry_unknown", m) }
    public static func windowHidden(_ m: String) -> HelperError { .init("window_hidden", m) }
    public static func windowMinimized(_ m: String) -> HelperError { .init("window_minimized", m) }
    public static func codecUnavailable(_ m: String) -> HelperError { .init("codec_unavailable", m) }
    public static func outExists(_ m: String) -> HelperError { .init("out_exists", m) }
    public static func alreadyRecording(_ m: String) -> HelperError { .init("already_recording", m) }
    public static func notRecording(_ m: String) -> HelperError { .init("not_recording", m) }
    public static func notFound(_ m: String) -> HelperError { .init("not_found", m) }
    public static func helper(_ m: String) -> HelperError { .init("helper_failed", m) }
    public static func io(_ m: String) -> HelperError { .init("io", m) }
}

public struct SourceInfo: Equatable {
    public var windowId: UInt64, pid: UInt32
    public var appId: String, title: String
    public var boundsPt: Rect
    public var visibility: Visibility
    public init(windowId: UInt64, pid: UInt32, appId: String, title: String, boundsPt: Rect, visibility: Visibility) {
        self.windowId = windowId; self.pid = pid; self.appId = appId; self.title = title
        self.boundsPt = boundsPt; self.visibility = visibility
    }
}

public struct StartInfo: Equatable {
    public var hostNsAtStart: UInt64
    public var width: UInt32, height: UInt32
    public var scale: Double
    public var boundsPt: Rect
    public var dpi: UInt32
    public var codec: String
    public var osBuild: String
    public var visibility: Visibility
    public init(hostNsAtStart: UInt64, width: UInt32, height: UInt32, scale: Double, boundsPt: Rect, dpi: UInt32,
                codec: String, osBuild: String, visibility: Visibility) {
        self.hostNsAtStart = hostNsAtStart; self.width = width; self.height = height; self.scale = scale
        self.boundsPt = boundsPt; self.dpi = dpi; self.codec = codec; self.osBuild = osBuild; self.visibility = visibility
    }
}

public struct Finished: Equatable {
    public var clean: Bool
    public init(clean: Bool) { self.clean = clean }
}

// MARK: plan (decoded from the `start` request)

public enum PlanError: Error, Equatable, CustomStringConvertible {
    case field(String, String)
    public var description: String {
        switch self {
        case .field(let key, let why): return "field '\(key)' \(why)"
        }
    }
}

enum Fields {
    static func get(_ o: [String: Any], _ key: String) throws -> Any {
        guard let v = o[key], !(v is NSNull) else { throw PlanError.field(key, "is missing") }
        return v
    }
    static func string(_ o: [String: Any], _ key: String) throws -> String {
        guard let s = try get(o, key) as? String else { throw PlanError.field(key, "must be a string") }
        return s
    }
    static func double(_ o: [String: Any], _ key: String) throws -> Double {
        guard let n = try get(o, key) as? NSNumber, !isBool(n) else { throw PlanError.field(key, "must be a number") }
        return n.doubleValue
    }
    static func uint64(_ o: [String: Any], _ key: String) throws -> UInt64 {
        guard let n = try get(o, key) as? NSNumber, !isBool(n), !isFloat(n), n.doubleValue >= 0
        else { throw PlanError.field(key, "must be an unsigned integer") }
        return n.uint64Value
    }
    static func uint32(_ o: [String: Any], _ key: String) throws -> UInt32 {
        let v = try uint64(o, key)
        guard v <= UInt64(UInt32.max) else { throw PlanError.field(key, "is out of range") }
        return UInt32(v)
    }
    static func isBool(_ n: NSNumber) -> Bool { CFGetTypeID(n) == CFBooleanGetTypeID() }
    static func isFloat(_ n: NSNumber) -> Bool { CFNumberIsFloatType(n) }
}

public enum Masking: Equatable {
    case declared([Rect])
    case captureSafeState
    case unknown
}

public struct StartPlan: Equatable {
    public var outDir: String
    public var windowId: UInt64
    public var pid: UInt32
    public var fps: UInt32
    public var scale: Double
    public var codec: Codec
    public var audio: [String]
    public var visibility: Visibility
    public var masking: Masking

    public init(outDir: String, windowId: UInt64, pid: UInt32, fps: UInt32, scale: Double, codec: Codec,
                audio: [String], visibility: Visibility, masking: Masking) {
        self.outDir = outDir; self.windowId = windowId; self.pid = pid; self.fps = fps; self.scale = scale
        self.codec = codec; self.audio = audio; self.visibility = visibility; self.masking = masking
    }

    public static func decode(_ o: [String: Any]) throws -> StartPlan {
        let codecText = try Fields.string(o, "codec")
        guard let codec = Codec(rawValue: codecText) else { throw PlanError.field("codec", "unknown codec '\(codecText)'") }
        let visText = try Fields.string(o, "visibility")
        guard let visibility = Visibility(rawValue: visText) else { throw PlanError.field("visibility", "unknown state '\(visText)'") }
        guard let audio = try Fields.get(o, "audio") as? [String] else { throw PlanError.field("audio", "must be an array") }
        guard let masking = try Fields.get(o, "masking") as? [String: Any] else { throw PlanError.field("masking", "must be an object") }
        let mode = try Fields.string(masking, "mode")
        let decodedMasking: Masking
        switch mode {
        case "declared":
            guard let regions = try Fields.get(masking, "regions") as? [Any] else { throw PlanError.field("regions", "must be an array") }
            decodedMasking = .declared(try regions.map { try Rect.parse($0, "regions") })
        case "capture_safe_state": decodedMasking = .captureSafeState
        case "unknown": decodedMasking = .unknown
        default: throw PlanError.field("masking", "unknown masking mode '\(mode)'")
        }
        return StartPlan(
            outDir: try Fields.string(o, "outDir"), windowId: try Fields.uint64(o, "windowId"),
            pid: try Fields.uint32(o, "pid"), fps: try Fields.uint32(o, "fps"), scale: try Fields.double(o, "scale"),
            codec: codec, audio: audio, visibility: visibility, masking: decodedMasking)
    }

    public var json: [String: Any] {
        let masking: [String: Any]
        switch self.masking {
        case .declared(let regions): masking = ["mode": "declared", "regions": regions.map { $0.json }]
        case .captureSafeState: masking = ["mode": "capture_safe_state"]
        case .unknown: masking = ["mode": "unknown"]
        }
        return ["outDir": outDir, "windowId": windowId, "pid": pid, "fps": fps, "scale": scale,
                "codec": codec.rawValue, "audio": audio, "visibility": visibility.rawValue, "masking": masking]
    }
}

public struct Request {
    public var id: UInt64
    public var op: String
    public var plan: [String: Any]?

    public static func decode(_ o: [String: Any]) throws -> Request {
        guard let kind = o["kind"] as? String, kind == "request" else { throw PlanError.field("kind", "must be 'request'") }
        return Request(id: (o["id"] as? NSNumber)?.uint64Value ?? 0, op: try Fields.string(o, "op"), plan: o["plan"] as? [String: Any])
    }
}

// MARK: messages the helper writes

public enum Messages {
    public static func response(id: UInt64, result: [String: Any]) -> [String: Any] {
        ["kind": "response", "id": id, "ok": true, "result": result]
    }

    public static func response(id: UInt64, sources: [SourceInfo]) -> [String: Any] {
        ["kind": "response", "id": id, "ok": true, "result": sources.map(sourceJSON)]
    }

    public static func errorResponse(id: UInt64, _ error: HelperError) -> [String: Any] {
        ["kind": "response", "id": id, "ok": false, "error": ["code": error.code, "message": error.message]]
    }

    public static func identity(osBuild: String, helperIdentity: String, signed: Bool, permission: Permission) -> [String: Any] {
        ["os": "macos", "osBuild": osBuild, "helper": ["identity": helperIdentity, "signed": signed], "permission": permission.rawValue]
    }

    public static func sourceJSON(_ s: SourceInfo) -> [String: Any] {
        ["windowId": s.windowId, "pid": s.pid, "appId": s.appId, "title": s.title,
         "boundsPt": s.boundsPt.json, "visibility": s.visibility.rawValue]
    }

    public static func startInfo(_ i: StartInfo) -> [String: Any] {
        ["hostNsAtStart": i.hostNsAtStart, "width": i.width, "height": i.height, "scale": i.scale,
         "boundsPt": i.boundsPt.json, "dpi": i.dpi, "codec": i.codec, "osBuild": i.osBuild, "visibility": i.visibility.rawValue]
    }

    public static func finished(_ f: Finished) -> [String: Any] {
        ["clean": f.clean, "tracks": [Any]()]
    }

    public static func event(_ event: HelperEvent) -> [String: Any] {
        let body: [String: Any]
        switch event {
        case .ready(let s): body = ["type": "ready", "sample": s.json]
        case .frame(let s): body = ["type": "frame", "sample": s.json]
        case .dropped(let hostNs, let count, let intervalMs, let reason):
            body = ["type": "dropped", "hostNs": hostNs, "count": count, "intervalMs": intervalMs, "reason": reason.rawValue]
        case .geometry(let hostNs, let bounds, let scale, let dpi):
            body = ["type": "geometry", "hostNs": hostNs, "boundsPt": bounds.json, "scale": scale, "dpi": dpi]
        case .failed(let failure): body = ["type": "failed", "failure": failure.json]
        case .exited: body = ["type": "exited"]
        }
        return ["kind": "event", "event": body]
    }

    public static let bye: [String: Any] = ["kind": "bye"]
}
