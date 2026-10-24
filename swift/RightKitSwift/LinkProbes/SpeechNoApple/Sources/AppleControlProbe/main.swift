import Foundation
import RightKitAppleSpeech
import RightKitSpeech

var selector = SpeechEngineSelector()
selector.registerAppleSpeech()
print(String(describing: type(of: try selector.resolve(.apple))))
