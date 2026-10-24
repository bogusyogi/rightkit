import Foundation
import RightKitSpeech

// Touch the Parakeet engine and the selector so nothing is dead-stripped before the scan.
let selector = SpeechEngineSelector(parakeetModelDirectory: URL(fileURLWithPath: "/nonexistent"))
do { _ = try selector.resolve(.parakeet) } catch { print("parakeet: \(error)") }
do { _ = try selector.resolve(.whisper) } catch { print("whisper: \(error)") }
print(String(describing: ParakeetEngine.self))
