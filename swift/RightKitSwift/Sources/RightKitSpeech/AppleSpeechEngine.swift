import AVFoundation
import Foundation
import Speech

/// Apple's on-device speech-to-text as a zero-download engine (ScrapeRight `AppleSpeechTranscriber`
/// plus CodeRight's legacy fallback). Uses `SpeechAnalyzer`/`SpeechTranscriber` where the OS has
/// it (iOS/macOS 26+, OS-managed model asset installed on first use) and `SFSpeechRecognizer`
/// restricted to on-device recognition before that.
public struct AppleSpeechEngine: SpeechTranscribing {
    public var kind: SpeechEngineKind { .apple }
    public var locale: Locale

    public init(locale: Locale = Locale(identifier: "en_US")) {
        self.locale = locale
    }

    /// True when the modern analyzer API exists on this OS.
    public static var hasModernAnalyzer: Bool {
        if #available(iOS 26.0, macOS 26.0, *) { return true }
        return false
    }

    public func transcribe(fileURL: URL) async throws -> String {
        if #available(iOS 26.0, macOS 26.0, *) {
            return try await Self.modernTranscribe(fileURL: fileURL, locale: locale)
        }
        return try await Self.legacyTranscribe(fileURL: fileURL, locale: locale)
    }

    @available(iOS 26.0, macOS 26.0, *)
    private static func modernTranscribe(fileURL: URL, locale: Locale) async throws -> String {
        guard let supported = await SpeechTranscriber.supportedLocale(equivalentTo: locale) else {
            throw SpeechError.decodeFailed("on-device speech model unavailable for \(locale.identifier)")
        }
        let transcriber = SpeechTranscriber(locale: supported, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
        // OS-managed model asset: install on first use.
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
            try await request.downloadAndInstall()
        }
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        // Results must be consumed concurrently with analysis.
        async let collected: String = transcriber.results.reduce(into: "") { text, result in
            text += String(result.text.characters)
        }
        let audioFile = try AVAudioFile(forReading: fileURL)
        if let lastSample = try await analyzer.analyzeSequence(from: audioFile) {
            try await analyzer.finalizeAndFinish(through: lastSample)
        } else {
            await analyzer.cancelAndFinishNow()
        }
        return try await collected.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func legacyTranscribe(fileURL: URL, locale: Locale) async throws -> String {
        let status = await withCheckedContinuation { (continuation: CheckedContinuation<SFSpeechRecognizerAuthorizationStatus, Never>) in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) }
        }
        guard status == .authorized else { throw SpeechError.permissionDenied("Speech Recognition") }
        guard let recognizer = SFSpeechRecognizer(locale: locale), recognizer.isAvailable else {
            throw SpeechError.engineUnavailable("no speech recognizer for \(locale.identifier)")
        }
        let request = SFSpeechURLRecognitionRequest(url: fileURL)
        request.shouldReportPartialResults = false
        request.addsPunctuation = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        return try await withCheckedThrowingContinuation { continuation in
            let gate = ResumeOnce()
            recognizer.recognitionTask(with: request) { result, error in
                if let error {
                    if gate.fire() { continuation.resume(throwing: SpeechError.decodeFailed(error.localizedDescription)) }
                } else if let result, result.isFinal {
                    let text = result.bestTranscription.formattedString
                    if gate.fire() { continuation.resume(returning: text) }
                }
            }
        }
    }
}

private final class ResumeOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    func fire() -> Bool { lock.withLock { if done { return false }; done = true; return true } }
}
