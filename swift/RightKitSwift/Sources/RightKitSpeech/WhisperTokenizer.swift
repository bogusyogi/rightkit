import Foundation


struct WhisperTokenizer {
    private let idToToken: [Int: String]
    private let tokenToID: [String: Int]
    private let byteDecoder: [UnicodeScalar: UInt8]

    init(tokenizerURL: URL) throws {
        let data = try Data(contentsOf: tokenizerURL)
        let raw = try JSONSerialization.jsonObject(with: data)
        guard let root = raw as? [String: Any],
              let model = root["model"] as? [String: Any],
              let vocab = model["vocab"] as? [String: Int] else {
            throw SpeechError.invalidTokenizerJSON
        }

        var tokenToID = vocab
        if let addedTokens = root["added_tokens"] as? [[String: Any]] {
            for token in addedTokens {
                guard let content = token["content"] as? String,
                      let id = token["id"] as? Int else {
                    continue
                }
                tokenToID[content] = id
            }
        }

        self.tokenToID = tokenToID
        // ponytail: prefer merge(_:uniquingKeysWith:) over Dictionary(uniqueKeysWithValues:) so
        // duplicate IDs in the upstream tokenizer.json never crash the loader; last write wins.
        var reverse: [Int: String] = [:]
        for (token, id) in tokenToID {
            reverse.merge([id: token], uniquingKeysWith: { _, new in new })
        }
        idToToken = reverse
        byteDecoder = Self.makeByteDecoder()
    }

    func tokenID(_ token: String) -> Int? {
        tokenToID[token]
    }

    func token(_ id: Int) -> String? {
        idToToken[id]
    }

    func decode(_ ids: [Int], skipSpecialTokens: Bool = true) throws -> String {
        var bytes: [UInt8] = []
        for id in ids {
            guard let token = idToToken[id] else {
                throw SpeechError.unknownToken(id)
            }
            if skipSpecialTokens, token.hasPrefix("<|"), token.hasSuffix("|>") {
                continue
            }
            for scalar in token.unicodeScalars {
                if let byte = byteDecoder[scalar] {
                    bytes.append(byte)
                }
            }
        }
        return String(decoding: bytes, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func makeByteDecoder() -> [UnicodeScalar: UInt8] {
        let passthrough = Array(33...126) + Array(161...172) + Array(174...255)
        let used = Set(passthrough)
        var pairs: [(UInt8, UnicodeScalar)] = passthrough.map {
            (UInt8($0), UnicodeScalar($0)!)
        }
        var next = 256
        for byte in 0...255 where !used.contains(byte) {
            pairs.append((UInt8(byte), UnicodeScalar(next)!))
            next += 1
        }
        var result: [UnicodeScalar: UInt8] = [:]
        for pair in pairs {
            result.merge([pair.1: pair.0], uniquingKeysWith: { _, new in new })
        }
        return result
    }
}
