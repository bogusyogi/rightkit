import AVFoundation
import SwiftUI
import RightKitSpeech

/*
 Provenance (exact donor revision):
 - openmausbot @ 6951c40156ba0916a10b6e49e5051b7785198b93
 - ios/App/ChatView.swift:554-565, 567-713
 - ios/App/SpeechDictation.swift:55-82, 230-258

 This view keeps donor interaction order: stop dictation before send, freeze
 the base while partials arrive, send on submit, stop on disappearance/scene
 change/interruption, and expose explicit listening/error states. Its sender,
 draft scope, and persistence are thin CodeRight seams so no donor types leak
 into the target app.
 */

/// APP-14 / product atom 20 seam: lets an external picker (command palette, prompt template
/// list) hand text into a live `OpenMausComposerBarPort` without either side holding a direct
/// reference to the other's SwiftUI view identity. The composer observes `pendingInsertion`
/// via `onChange` and clears it once applied, so a relay is safe to reuse across scopes.
@MainActor
@Observable
public final class OpenMausComposerInsertionRelay {
    public var pendingInsertion: String?

    public init() {}

    public func insert(_ text: String) {
        pendingInsertion = text
    }
}

/// Standalone composer surface for integration into CodeRight's session view.
/// Existing `ComposerBar` can adopt its state rules directly, or embed this
/// view with a small `OpenMausComposerMessageSending` adapter.
public struct OpenMausComposerBarPort: View {
    private let scope: ComposerDraftScope
    private let sender: @MainActor (String) async -> OpenMausComposerSendResult
    private let draftStore: ComposerDraftStore
    private let placeholder: String
    private let onSendAttempt: @MainActor () -> Void
    /// product atom 20 / APP-14 seam: resolves a typed "/command" to a saved prompt
    /// template's body before send, so a slash command expands in place rather than being
    /// sent to the daemon literally. Nil means no template store is wired up yet.
    private let slashCommandResolver: (@MainActor (String) -> String?)?
    /// APP-14 seam: opens the command palette from the composer toolbar. Nil hides the
    /// palette trigger entirely rather than showing a dead button.
    private let onOpenPalette: (@MainActor () -> Void)?
    /// APP-14 / atom 20 seam: external insertion channel — see
    /// `OpenMausComposerInsertionRelay`. Nil disables external insertion (dictation and typed
    /// slash commands still work).
    private let insertionRelay: OpenMausComposerInsertionRelay?

    @State private var draft = ""
    @State private var draftLoaded = false
    @State private var submissionInFlight = false
    @State private var submissionError: String?
    @State private var dictation = SpeechDictation()
    @FocusState private var focused: Bool
    @Environment(\.scenePhase) private var scenePhase

    public init(
        scope: ComposerDraftScope,
        placeholder: String = "Ask CodeRight",
        draftStore: ComposerDraftStore = ComposerDraftStore(),
        onSendAttempt: @escaping @MainActor () -> Void = {},
        slashCommandResolver: (@MainActor (String) -> String?)? = nil,
        onOpenPalette: (@MainActor () -> Void)? = nil,
        insertionRelay: OpenMausComposerInsertionRelay? = nil,
        sender: @escaping @MainActor (String) async -> OpenMausComposerSendResult
    ) {
        self.scope = scope
        self.placeholder = placeholder
        self.draftStore = draftStore
        self.onSendAttempt = onSendAttempt
        self.slashCommandResolver = slashCommandResolver
        self.onOpenPalette = onOpenPalette
        self.insertionRelay = insertionRelay
        self.sender = sender
    }


    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let error = dictation.error ?? submissionError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(Theme.body(size: 12, weight: .medium, relativeTo: .caption))
                    .foregroundStyle(Theme.danger)
                    .accessibilityLabel("Composer error: \(error)")
            }

            VStack(alignment: .leading, spacing: 0) {
                TextField(
                    dictation.isListening || dictation.isStarting ? "Listening…" : placeholder,
                    text: $draft,
                    axis: .vertical
                )
                .lineLimit(1...6)
                .font(Theme.body(size: 16))
                .foregroundStyle(Theme.text)
                .tint(Theme.text)
                .padding(.horizontal, 12)
                .frame(minHeight: ChatChrome.composerInputMinHeight, alignment: .top)
                .focused($focused)
                // Donor uses allowsHitTesting while capture is active: it
                // keeps partial transcript replacement deterministic while
                // leaving the text visually undimmed.
                .allowsHitTesting(!dictation.isListening && !dictation.isStarting)
                .disabled(submissionInFlight)
                .submitLabel(.send)
                .onSubmit(submit)
                .accessibilityIdentifier("composer-input")

                HStack(spacing: 2) {
                    if let onOpenPalette {
                        ChatCircleButton(
                            systemName: "command",
                            accessibilityLabel: "Open command palette",
                            disabled: submissionInFlight,
                            action: onOpenPalette
                        )
                        .accessibilityIdentifier("composer-open-palette")
                    }

                    Spacer(minLength: 4)

                    ChatCircleButton(
                        systemName: dictation.isListening ? "mic.fill" : "mic",
                        accessibilityLabel: dictation.isListening ? "Stop dictation" : "Start dictation",
                        disabled: submissionInFlight,
                        action: toggleDictation
                    )
                    .symbolEffect(.pulse, isActive: dictation.isListening)
                    .accessibilityIdentifier("composer-dictation")
                    .accessibilityValue(
                        dictation.isStarting
                            ? "Starting"
                            : (dictation.isListening ? "Listening" : (dictation.error ?? "Ready"))
                    )
                    .accessibilityHint("Converts speech to text on this device when supported")

                    Button(action: submit) {
                        Group {
                            if submissionInFlight {
                                ProgressView().controlSize(.small)
                            } else {
                                Image(systemName: "arrow.up")
                                    .font(.system(size: 17, weight: .semibold))
                            }
                        }
                        .foregroundStyle(canSend ? Theme.background : Theme.muted)
                        .frame(width: ChatChrome.composerActionSize, height: ChatChrome.composerActionSize)
                        .background(canSend ? Theme.text : Theme.raised, in: Circle())
                        .overlay(Circle().stroke(canSend ? .clear : Theme.border, lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                    .disabled(!canSend)
                    .accessibilityIdentifier("composer-send")
                    .accessibilityLabel("Send message")
                }
            }
            .chatComposerSurface()
        }
        .padding(.horizontal, ChatChrome.horizontalInset)
        .padding(.vertical, 8)
        .toolbar {
            ToolbarItemGroup(placement: .keyboard) {
                Spacer()
                Button("Done") {
                    focused = false
                }
                .tint(.accentColor)
                .accessibilityIdentifier("composer-port-keyboard-done")
            }
        }
        .onChange(of: dictation.transcript) { _, transcript in
            draft = Dictation.draft(
                base: dictation.base,
                transcript: transcript
            )
        }
        .onChange(of: insertionRelay?.pendingInsertion) { _, pending in
            guard let text = pending else { return }
            appendToDraft(text)
            insertionRelay?.pendingInsertion = nil
        }
        .task(id: scope) {
            draftLoaded = false
            draft = draftStore.load(scope: scope)
            draftLoaded = true
        }
        // A task keyed by both scope and text is a cancellable debounce and
        // avoids writing every keystroke while still preserving relaunch text.
        .task(id: DraftSaveKey(scope: scope, text: draft)) {
            guard draftLoaded else { return }
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            draftStore.save(draft, scope: scope)
        }
        .onDisappear {
            if draftLoaded { draftStore.save(draft, scope: scope) }
            dictation.stop()
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { dictation.stop() }
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)) { note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey]
            let value = (raw as? NSNumber)?.uintValue ?? (raw as? UInt)
            if value == AVAudioSession.InterruptionType.began.rawValue {
                dictation.stop()
            }
        }
    }

    private var canSend: Bool {
        OpenMausComposerSubmission.canSend(draft, isBusy: submissionInFlight)
    }

    /// Appends `text` into the live draft, matching how a user would type after existing
    /// content. Used by both the insertion relay (palette/prompt picker) and, indirectly, by
    /// dictation's own base/transcript composition.
    private func appendToDraft(_ text: String) {
        if draft.isEmpty {
            draft = text
        } else if draft.hasSuffix(" ") || draft.hasSuffix("\n") {
            draft += text
        } else {
            draft += " " + text
        }
    }

    private func submit() {
        guard canSend else { return }
        // Cancel an authorization prompt or active audio capture before the
        // send task starts, matching donor ChatView's ordering.
        dictation.stop()
        var text = OpenMausComposerSubmission.trimmedText(draft)
        guard !text.isEmpty else { return }
        // product atom 20: a typed "/command" expands to its saved template body before
        // send rather than being sent to the daemon literally. Unrecognized commands are
        // sent through unchanged — this is expansion, not a hard gate on the "/" prefix.
        if let resolver = slashCommandResolver, let expanded = resolver(text) {
            text = expanded
        }
        focused = false
        submissionError = nil
        submissionInFlight = true
        // Donor fires sent sound/haptic at this point. Keep feedback as a
        // seam so CodeRight can map its own feedback implementation without
        // coupling this port to donor-only `SoundEffects` or `Haptics` types.
        onSendAttempt()

        Task { @MainActor in
            let result = await sender(text)
            switch result {
            case .sent:
                draft = ""
                draftStore.remove(scope: scope)
            case let .failed(message):
                // Preserve failed text for retry; only a confirmed send clears
                // both field and persisted draft.
                submissionError = message
            }
            submissionInFlight = false
        }
    }

    private func toggleDictation() {
        guard !submissionInFlight else { return }
        if !dictation.isListening && !dictation.isStarting {
            focused = false
        }
        dictation.toggle(capturing: draft)
    }

    private struct DraftSaveKey: Hashable, Sendable {
        let scope: ComposerDraftScope
        let text: String
    }
}
