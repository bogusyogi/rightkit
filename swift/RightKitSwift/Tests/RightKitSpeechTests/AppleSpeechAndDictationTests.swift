import Foundation
import Testing

@testable import RightKitSpeech

struct DictationDraftTests {
    @Test func draftReplacesTheSpokenPartAgainstOneFrozenBase() {
        // Partial results re-render the SAME draft; they must never accumulate.
        let base = "  Fix the build  "
        #expect(Dictation.draft(base: base, transcript: "please") == "Fix the build please")
        #expect(Dictation.draft(base: base, transcript: "please run the tests") == "Fix the build please run the tests")
        #expect(Dictation.draft(base: base, transcript: "  ") == "Fix the build")
        #expect(Dictation.draft(base: "", transcript: "hello") == "hello")
    }

    @Test func localeCandidatesAreOrderedBoundedAndUnique() {
        let candidates = Dictation.localeCandidates(
            preferredLanguages: ["fr-FR", "en-US", "de-DE", "es-ES", "it-IT"], current: Locale(identifier: "fr_FR"))
        let ids = candidates.map(Dictation.canonicalIdentifier)
        #expect(ids.first == "fr_fr")
        #expect(ids.count == Set(ids).count)
        #expect(!ids.contains("es_es"), "only the first three preferred languages count")
        #expect(ids.last == "en_us")
    }
}
