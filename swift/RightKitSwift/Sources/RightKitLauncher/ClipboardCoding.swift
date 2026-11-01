import Foundation

// Reading and writing the clipboard files by hand (docs/clipboard-spec.md 2.5 and 6): keys in
// the spec's order, compact, unknown keys kept on every row that is rewritten. Nothing here
// logs a value.

// MARK: - JSON values

extension ClipJSON {
    init?(any value: Any) {
        switch value {
        case is NSNull:
            self = .null
        case let number as NSNumber:
            if CFGetTypeID(number) == CFBooleanGetTypeID() {
                self = .bool(number.boolValue)
            } else {
                self = .number(number.doubleValue)
            }
        case let string as String:
            self = .string(string)
        case let array as [Any]:
            self = .array(array.compactMap { ClipJSON(any: $0) })
        case let object as [String: Any]:
            var map: [String: ClipJSON] = [:]
            for (key, inner) in object {
                if let converted = ClipJSON(any: inner) { map[key] = converted }
            }
            self = .object(map)
        default:
            return nil
        }
    }

    /// nil when the bytes are not JSON.
    static func parse(_ data: Data) -> ClipJSON? {
        guard let any = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) else { return nil }
        return ClipJSON(any: any)
    }

    var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    var doubleValue: Double? {
        if case .number(let value) = self { return value }
        return nil
    }

    var intValue: Int? {
        guard case .number(let value) = self, value.isFinite, abs(value) < 9e15 else { return nil }
        return Int(value)
    }

    var boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    var arrayValue: [ClipJSON]? {
        if case .array(let value) = self { return value }
        return nil
    }

    var objectValue: [String: ClipJSON]? {
        if case .object(let value) = self { return value }
        return nil
    }
}

/// Compact JSON text. Object keys given as pairs keep their order; `ClipJSON` objects
/// (unknown keys) are written with sorted keys.
enum ClipJSONWriter {
    static func quote(_ text: String) -> String {
        var out = "\""
        for scalar in text.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            default:
                if scalar.value < 0x20 {
                    out += String(format: "\\u%04x", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        out += "\""
        return out
    }

    static func number(_ value: Double) -> String {
        guard value.isFinite else { return "0" }
        if value == value.rounded(), abs(value) < 9e15 { return String(Int64(value)) }
        return String(value)
    }

    static func array(_ values: [String]) -> String {
        "[" + values.joined(separator: ",") + "]"
    }

    /// `pairs` hold already written values.
    static func object(_ pairs: [(String, String)]) -> String {
        "{" + pairs.map { quote($0.0) + ":" + $0.1 }.joined(separator: ",") + "}"
    }

    static func write(_ value: ClipJSON) -> String {
        switch value {
        case .null: return "null"
        case .bool(let flag): return flag ? "true" : "false"
        case .number(let number): return Self.number(number)
        case .string(let text): return quote(text)
        case .array(let items): return array(items.map { write($0) })
        case .object(let map):
            return object(map.keys.sorted().map { ($0, write(map[$0] ?? .null)) })
        }
    }

    static func strings(_ values: [String]) -> String { array(values.map { quote($0) }) }
}

// MARK: - Meta

extension ClipMeta {
    private static let known: Set<String> = [
        "chars", "lines", "language", "host", "scheme", "hex", "original", "count", "names",
        "anyMissing", "width", "height", "format", "screenshot", "stackId", "laneLocked",
        "hasHtml", "hasRtf",
    ]

    init(json: ClipJSON?) {
        self.init()
        guard let map = json?.objectValue else { return }
        chars = map["chars"]?.intValue
        lines = map["lines"]?.intValue
        language = map["language"]?.stringValue
        host = map["host"]?.stringValue
        scheme = map["scheme"]?.stringValue
        hex = map["hex"]?.stringValue
        original = map["original"]?.stringValue
        count = map["count"]?.intValue
        names = map["names"]?.arrayValue?.compactMap { $0.stringValue }
        anyMissing = map["anyMissing"]?.boolValue
        width = map["width"]?.intValue
        height = map["height"]?.intValue
        format = map["format"]?.stringValue
        screenshot = map["screenshot"]?.boolValue
        stackId = map["stackId"]?.stringValue
        laneLocked = map["laneLocked"]?.boolValue
        hasHtml = map["hasHtml"]?.boolValue
        hasRtf = map["hasRtf"]?.boolValue
        for (key, value) in map where !Self.known.contains(key) { extra[key] = value }
    }

    var jsonString: String {
        var pairs: [(String, String)] = []
        func add(_ key: String, _ value: String?) { if let value { pairs.append((key, value)) } }
        add("chars", chars.map { String($0) })
        add("lines", lines.map { String($0) })
        add("language", language.map { ClipJSONWriter.quote($0) })
        add("host", host.map { ClipJSONWriter.quote($0) })
        add("scheme", scheme.map { ClipJSONWriter.quote($0) })
        add("hex", hex.map { ClipJSONWriter.quote($0) })
        add("original", original.map { ClipJSONWriter.quote($0) })
        add("count", count.map { String($0) })
        add("names", names.map { ClipJSONWriter.strings($0) })
        add("anyMissing", anyMissing.map { $0 ? "true" : "false" })
        add("width", width.map { String($0) })
        add("height", height.map { String($0) })
        add("format", format.map { ClipJSONWriter.quote($0) })
        add("screenshot", screenshot.map { $0 ? "true" : "false" })
        add("stackId", stackId.map { ClipJSONWriter.quote($0) })
        // Absent when false (2.4).
        if laneLocked == true { add("laneLocked", "true") }
        add("hasHtml", hasHtml.map { $0 ? "true" : "false" })
        add("hasRtf", hasRtf.map { $0 ? "true" : "false" })
        for key in extra.keys.sorted() { add(key, extra[key].map { ClipJSONWriter.write($0) }) }
        return ClipJSONWriter.object(pairs)
    }
}

// MARK: - Item

extension ClipItem {
    private static let known: Set<String> = [
        "id", "kind", "preview", "title", "sourceApp", "createdAt", "lastUsedAt", "useCount",
        "pinned", "boards", "size", "digest", "lane", "meta", "concealed",
    ]

    /// nil when the row has no usable id, kind or date (the row is dropped on load).
    init?(json: ClipJSON) {
        guard let map = json.objectValue,
              let id = map["id"]?.stringValue, !id.isEmpty,
              let kindName = map["kind"]?.stringValue, let kind = ClipKind(rawValue: kindName),
              let created = map["createdAt"]?.doubleValue else { return nil }
        var app: ClipSourceApp?
        if let source = map["sourceApp"]?.objectValue, let appID = source["id"]?.stringValue {
            app = ClipSourceApp(id: appID, name: source["name"]?.stringValue ?? "")
        }
        var extra: [String: ClipJSON] = [:]
        for (key, value) in map where !Self.known.contains(key) { extra[key] = value }
        self.init(
            id: id, kind: kind, preview: map["preview"]?.stringValue ?? "",
            title: map["title"]?.stringValue, sourceApp: app, createdAt: created,
            lastUsedAt: map["lastUsedAt"]?.doubleValue ?? created,
            useCount: map["useCount"]?.intValue ?? 0, pinned: map["pinned"]?.boolValue ?? false,
            boards: map["boards"]?.arrayValue?.compactMap { $0.stringValue } ?? [],
            size: map["size"]?.intValue ?? 0, digest: map["digest"]?.stringValue ?? "",
            lane: map["lane"]?.stringValue.flatMap { ClipLane(rawValue: $0) } ?? .main,
            meta: ClipMeta(json: map["meta"]), concealed: map["concealed"]?.boolValue ?? false,
            extra: extra)
    }

    /// 2.5: one row, keys in the spec's order, unknown keys after.
    var jsonString: String {
        var pairs: [(String, String)] = [
            ("id", ClipJSONWriter.quote(id)),
            ("kind", ClipJSONWriter.quote(kind.rawValue)),
            ("preview", ClipJSONWriter.quote(preview)),
            ("title", title.map { ClipJSONWriter.quote($0) } ?? "null"),
        ]
        if let app = sourceApp {
            pairs.append(("sourceApp", ClipJSONWriter.object([
                ("id", ClipJSONWriter.quote(app.id)), ("name", ClipJSONWriter.quote(app.name)),
            ])))
        } else {
            pairs.append(("sourceApp", "null"))
        }
        pairs += [
            ("createdAt", ClipJSONWriter.number(createdAt)),
            ("lastUsedAt", ClipJSONWriter.number(lastUsedAt)),
            ("useCount", String(useCount)),
            ("pinned", pinned ? "true" : "false"),
            ("boards", ClipJSONWriter.strings(boards)),
            ("size", String(size)),
            ("digest", ClipJSONWriter.quote(digest)),
            ("lane", ClipJSONWriter.quote(lane.rawValue)),
            ("meta", meta.jsonString),
            ("concealed", concealed ? "true" : "false"),
        ]
        for key in extra.keys.sorted() {
            if let value = extra[key] { pairs.append((key, ClipJSONWriter.write(value))) }
        }
        return ClipJSONWriter.object(pairs)
    }
}

// MARK: - Board

extension ClipBoard {
    private static let known: Set<String> = ["id", "name", "colour", "order", "itemOrder", "createdAt"]

    init?(json: ClipJSON) {
        guard let map = json.objectValue,
              let id = map["id"]?.stringValue, !id.isEmpty,
              let name = map["name"]?.stringValue else { return nil }
        var extra: [String: ClipJSON] = [:]
        for (key, value) in map where !Self.known.contains(key) { extra[key] = value }
        self.init(
            id: id, name: name, colour: map["colour"]?.stringValue ?? ClipBoard.palette[0].hex,
            order: map["order"]?.intValue ?? 0,
            itemOrder: map["itemOrder"]?.arrayValue?.compactMap { $0.stringValue } ?? [],
            createdAt: map["createdAt"]?.doubleValue ?? 0, extra: extra)
    }

    var jsonString: String {
        var pairs: [(String, String)] = [
            ("id", ClipJSONWriter.quote(id)),
            ("name", ClipJSONWriter.quote(name)),
            ("colour", ClipJSONWriter.quote(colour)),
            ("order", String(order)),
            ("itemOrder", ClipJSONWriter.strings(itemOrder)),
            ("createdAt", ClipJSONWriter.number(createdAt)),
        ]
        for key in extra.keys.sorted() {
            if let value = extra[key] { pairs.append((key, ClipJSONWriter.write(value))) }
        }
        return ClipJSONWriter.object(pairs)
    }
}

// MARK: - Files

/// `index.json` (2.5).
enum ClipIndexFile {
    static let version = 1

    enum Decoded {
        case ok([ClipItem])
        case newer
        case unreadable
    }

    static func encode(_ items: [ClipItem], writer: String) -> Data {
        let rows = items.map { $0.jsonString }
        let text = "{\"version\":\(version),\"writer\":\(ClipJSONWriter.quote(writer)),\"items\":"
            + ClipJSONWriter.array(rows) + "}"
        return Data(text.utf8)
    }

    static func decode(_ data: Data) -> Decoded {
        guard let json = ClipJSON.parse(data) else { return .unreadable }
        guard let map = json.objectValue else { return .unreadable }
        if let found = map["version"] {
            guard let number = found.intValue else { return .unreadable }
            if number > version { return .newer }
        }
        guard let rows = map["items"]?.arrayValue else { return .unreadable }
        return .ok(rows.compactMap { ClipItem(json: $0) })
    }
}

/// `boards.json` (6).
enum ClipBoardsFile {
    enum Decoded {
        case ok([ClipBoard])
        case newer
        case unreadable
    }

    static func encode(_ boards: [ClipBoard]) -> Data {
        let text = "{\"version\":\(ClipIndexFile.version),\"boards\":"
            + ClipJSONWriter.array(boards.map { $0.jsonString }) + "}"
        return Data(text.utf8)
    }

    static func decode(_ data: Data) -> Decoded {
        guard let map = ClipJSON.parse(data)?.objectValue else { return .unreadable }
        if let found = map["version"] {
            guard let number = found.intValue else { return .unreadable }
            if number > ClipIndexFile.version { return .newer }
        }
        guard let rows = map["boards"]?.arrayValue else { return .unreadable }
        return .ok(rows.compactMap { ClipBoard(json: $0) })
    }
}
