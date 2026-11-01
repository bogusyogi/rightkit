#if os(iOS)
import AVFoundation
import SwiftUI
import RightKitSpeech

/// Port of `../coderight-web/src/components/InputBar.tsx`'s core send flow (multiline text,
/// Enter-to-send is a hardware-keyboard nicety; the primary path here is the send button).
struct ComposerBar: View {
    let vm: SessionViewModel
    private let configuredDraftScope: ComposerDraftScope?
    private let draftStore: ComposerDraftStore

    @State private var draft = ""
    @State private var draftLoaded = false
    @State private var submissionInFlight = false
    @State private var dictation = SpeechDictation()
    @FocusState private var focused: Bool
    @Environment(\.scenePhase) private var scenePhase

    init(
        vm: SessionViewModel,
        draftScope: ComposerDraftScope? = nil,
        draftStore: ComposerDraftStore = ComposerDraftStore()
    ) {
        self.vm = vm
        configuredDraftScope = draftScope
        self.draftStore = draftStore
    }

    private var draftScope: ComposerDraftScope {
        configuredDraftScope ?? vm.draftScope
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let error = dictation.error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(Theme.danger)
                    .accessibilityLabel("Dictation error: \(error)")
            }

            HStack(alignment: .bottom, spacing: 8) {
                TextField(
                    dictation.isListening || dictation.isStarting ? "Listening…" : "Message CodeRight…",
                    text: $draft,
                    axis: .vertical
                )
                    .lineLimit(1...6)
                    .padding(10)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .stroke(Theme.border, lineWidth: 1)
                    )
                    .focused($focused)
                    .disabled(vm.busy || dictation.isListening || dictation.isStarting)
                    .onSubmit(submit)
                    .accessibilityIdentifier("composer-input")

                Button(action: toggleDictation) {
                    Image(systemName: dictation.isListening ? "mic.fill" : "mic")
                        .font(.system(size: 22, weight: .semibold))
                        .frame(width: 34, height: 34)
                        .background(
                            Circle().fill(
                                dictation.isListening ? Theme.accent.opacity(0.2) : Theme.raised
                            )
                        )
                        .symbolEffect(.pulse, isActive: dictation.isListening)
                }
                .tint(dictation.isListening ? Theme.accent : Theme.text)
                .disabled(vm.busy)
                .accessibilityIdentifier("composer-dictation")
                .accessibilityLabel(dictation.isListening ? "Stop dictation" : "Start dictation")
                .accessibilityValue(
                    dictation.isStarting
                        ? "Starting"
                        : (dictation.isListening ? "Listening" : (dictation.error ?? "Ready"))
                )
                .accessibilityHint("Converts speech to text on this device when supported")

                Button(action: submit) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.system(size: 30))
                }
                .tint(Theme.accent)
                .disabled(isSendDisabled)
                .accessibilityIdentifier("composer-send")
            }
        }
        .padding(12)
        .background(Theme.background)
        // Explicit escape hatch: tap-outside and drag-to-dismiss both need transcript area,
        // which a short transcript may not have once the keyboard covers the screen.
        .toolbar {
            ToolbarItemGroup(placement: .keyboard) {
                Spacer()
                Button("Done") { focused = false }
                    .tint(Theme.accent)
                    .accessibilityIdentifier("composer-keyboard-done")
            }
        }
        .onChange(of: dictation.transcript) { _, transcript in
            draft = Dictation.draft(base: dictation.base, transcript: transcript)
        }
        .task(id: draftScope) {
            draftLoaded = false
            draft = draftStore.load(scope: draftScope)
            draftLoaded = true
        }
        // A task keyed by text is canceled and restarted for every edit, providing a small
        // debounce without retaining a timer or writing each keystroke to disk.
        .task(id: draft) {
            guard draftLoaded else { return }
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            draftStore.save(draft, scope: draftScope)
        }
        .onDisappear {
            guard draftLoaded else { return }
            draftStore.save(draft, scope: draftScope)
            dictation.stop()
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { dictation.stop() }
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)) { notification in
            let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey]
            let value = (raw as? NSNumber)?.uintValue ?? (raw as? UInt)
            if value == AVAudioSession.InterruptionType.began.rawValue {
                dictation.stop()
            }
        }
    }

    private var isSendDisabled: Bool {
        submissionInFlight || vm.busy || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func submit() {
        guard !isSendDisabled else { return }
        let text = draft
        dictation.stop()
        focused = false
        let messageCountBeforeSend = vm.msgs.count
        submissionInFlight = true
        Task { @MainActor in
            await vm.send(text: text)
            let failed = vm.msgs.dropFirst(messageCountBeforeSend).contains { $0.role == .error }
            if !failed {
                draft = ""
                draftStore.remove(scope: draftScope)
            }
            submissionInFlight = false
        }
    }

    private func toggleDictation() {
        guard !vm.busy else { return }
        if !dictation.isListening && !dictation.isStarting {
            focused = false
        }
        dictation.toggle(capturing: draft)
    }
}
#endif
