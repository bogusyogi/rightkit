import SwiftUI

/// One structured question surfaced by the `ask_user` tool (Claude-Code
/// `AskUserQuestion` parity): 1-4 questions per round, 2-4 options each, with
/// an optional `multiSelect` flag. Mirrors the engine-side schema in
/// `engine/crates/tools/src/ask_user.rs` without depending on the SSE
/// `ProtocolEvent` model, since the local FFI runtime delivers these through
/// its own `UserQuestioner` callback rather than the daemon event stream.
public struct LocalAskUserQuestion: Identifiable, Equatable, Sendable {
    public let id: String
    public let header: String?
    public let question: String
    /// When empty, the question is answered with free text instead of options.
    public let options: [String]
    public let multiSelect: Bool

    public init(
        id: String,
        header: String? = nil,
        question: String,
        options: [String] = [],
        multiSelect: Bool = false
    ) {
        self.id = id
        self.header = header
        self.question = question
        self.options = options
        self.multiSelect = multiSelect
    }
}

/// One answered question: selected option labels (possibly plural under
/// `multiSelect`), or free text when the question carried no options.
public struct LocalAskUserAnswer: Equatable, Sendable {
    public let questionId: String
    public let selectedOptions: [String]
    public let freeText: String?
}

/// Structured ask_user round-trip: 1-4 questions rendered as radio/checkbox
/// groups (or a text field when a question has no options), blocked until
/// every question has an answer. Matches the block-until-resolved pattern of
/// `ApprovalInteractionCard` — local, one-way submission state, no retry
/// racing the same round.
@MainActor
struct LocalAskUserView: View {
    let questions: [LocalAskUserQuestion]
    let onSubmit: ([LocalAskUserAnswer]) -> Void
    let onCancel: (() -> Void)?

    @State private var selections: [String: Set<String>] = [:]
    @State private var freeText: [String: String] = [:]
    @State private var isSubmitting = false

    init(
        questions: [LocalAskUserQuestion],
        onSubmit: @escaping ([LocalAskUserAnswer]) -> Void,
        onCancel: (() -> Void)? = nil
    ) {
        self.questions = questions
        self.onSubmit = onSubmit
        self.onCancel = onCancel
    }

    var body: some View {
        NavigationStack {
            Form {
                ForEach(questions) { question in
                    Section {
                        if let header = question.header, !header.isEmpty {
                            Text(header)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(Theme.muted)
                        }
                        Text(question.question)
                            .font(.subheadline)
                            .foregroundStyle(Theme.text)

                        if question.options.isEmpty {
                            TextField(
                                "Your answer",
                                text: textBinding(for: question.id),
                                axis: .vertical
                            )
                            .textFieldStyle(.roundedBorder)
                            .lineLimit(1 ... 4)
                            .disabled(isSubmitting)
                            .accessibilityLabel("Answer for \(question.question)")
                        } else {
                            ForEach(question.options, id: \.self) { option in
                                optionRow(question: question, option: option)
                            }
                        }
                    }
                }
            }
            .navigationTitle("Question\(questions.count > 1 ? "s" : "")")
            .toolbar {
                if let onCancel {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Skip", role: .cancel) { onCancel() }
                            .disabled(isSubmitting)
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") { submit() }
                        .disabled(isSubmitting || !allQuestionsAnswered)
                }
            }
        }
    }

    @ViewBuilder
    private func optionRow(question: LocalAskUserQuestion, option: String) -> some View {
        let isSelected = selections[question.id]?.contains(option) ?? false
        Button {
            toggle(option, for: question)
        } label: {
            HStack {
                Text(option)
                    .foregroundStyle(Theme.text)
                Spacer()
                if isSelected {
                    Image(systemName: question.multiSelect ? "checkmark.square.fill" : "checkmark.circle.fill")
                        .foregroundStyle(Theme.accent2)
                } else {
                    Image(systemName: question.multiSelect ? "square" : "circle")
                        .foregroundStyle(Theme.muted)
                }
            }
        }
        .disabled(isSubmitting)
        .accessibilityLabel(option)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }

    private func toggle(_ option: String, for question: LocalAskUserQuestion) {
        var current = selections[question.id] ?? []
        if question.multiSelect {
            if current.contains(option) { current.remove(option) } else { current.insert(option) }
        } else {
            current = current.contains(option) ? [] : [option]
        }
        selections[question.id] = current
    }

    private func textBinding(for questionId: String) -> Binding<String> {
        Binding(
            get: { freeText[questionId] ?? "" },
            set: { freeText[questionId] = $0 }
        )
    }

    private var allQuestionsAnswered: Bool {
        questions.allSatisfy { question in
            if question.options.isEmpty {
                let text = freeText[question.id] ?? ""
                return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }
            return !(selections[question.id]?.isEmpty ?? true)
        }
    }

    private func submit() {
        guard allQuestionsAnswered, !isSubmitting else { return }
        isSubmitting = true
        let answers = questions.map { question -> LocalAskUserAnswer in
            if question.options.isEmpty {
                return LocalAskUserAnswer(
                    questionId: question.id,
                    selectedOptions: [],
                    freeText: freeText[question.id]
                )
            }
            let selected = selections[question.id] ?? []
            // Preserve the question's own option order rather than Set order,
            // so a multi-select answer reads back deterministically.
            let ordered = question.options.filter { selected.contains($0) }
            return LocalAskUserAnswer(questionId: question.id, selectedOptions: ordered, freeText: nil)
        }
        onSubmit(answers)
    }
}
