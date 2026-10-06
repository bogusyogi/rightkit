import Foundation

/// Real-model fixtures. Models are READ-ONLY donors from the HeardRight checkout / installed app;
/// override with RIGHTKIT_PARAKEET_MODEL / RIGHTKIT_WHISPER_MODEL. Tests that need a model are
/// skipped (visibly, via `.enabled(if:)`) when none exists on this machine.
enum Fixtures {
    static func firstExisting(_ envKey: String, _ candidates: [String], marker: String) -> URL? {
        var paths = candidates
        if let override = ProcessInfo.processInfo.environment[envKey] { paths.insert(override, at: 0) }
        for path in paths {
            let url = URL(fileURLWithPath: path, isDirectory: true)
            if FileManager.default.fileExists(atPath: url.appendingPathComponent(marker).path) { return url }
        }
        return nil
    }

    static let parakeetModel = firstExisting(
        "RIGHTKIT_PARAKEET_MODEL",
        [
            "/Volumes/D/claude/heardright/model_registry/coreml/parakeet-tdt-v3",
            NSHomeDirectory() + "/Library/Application Support/HeardRightNext/models/parakeet-tdt-v3",
        ],
        marker: "pipeline.json"
    )

    static let whisperModel = firstExisting(
        "RIGHTKIT_WHISPER_MODEL",
        ["/Volumes/D/claude/heardright/model_registry/coreml/whisper-multi"],
        marker: "tokenizer.json"
    )

    static var hasSay: Bool { FileManager.default.isExecutableFile(atPath: "/usr/bin/say") }

    /// Text-to-speech to a 16 kHz mono 16-bit WAV file. `say -o` writes a file and plays nothing.
    static func synthesize(_ text: String, voice: String = "Samantha", in directory: URL) throws -> URL {
        let url = directory.appendingPathComponent("\(UUID().uuidString).wav")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/say")
        process.arguments = ["-v", voice, "-o", url.path, "--data-format=LEI16@16000", text]
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw NSError(domain: "say", code: Int(process.terminationStatus)) }
        return url
    }

    static func scratchDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-speech-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// Lowercased words only: transcripts differ in punctuation and casing between engines.
    static func words(_ text: String) -> [String] {
        text.lowercased().split(whereSeparator: { !$0.isLetter && !$0.isNumber && $0 != "'" }).map(String.init)
    }
}
