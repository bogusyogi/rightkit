#if os(iOS)
import SwiftUI

/// Port of `../coderight-web/src/components/ModelSelector.tsx`: a menu over `vm.models`
/// (falling back to a static list before the daemon's orchestration settings load), current
/// selection highlighted, `setModel` fired on change.
struct ModelSelectorView: View {
    private static let fallbackOptions = [
        "openrouter/auto",
        "anthropic/claude-sonnet-4.5",
        "openai/gpt-5-codex",
        "x-ai/grok-build-0.1",
        "deepseek/deepseek-v4-pro",
        "qwen/qwen3-coder"
    ]

    let vm: SessionViewModel

    private var options: [String] {
        vm.models.isEmpty ? Self.fallbackOptions : vm.models
    }

    var body: some View {
        Menu {
            ForEach(options, id: \.self) { option in
                Button {
                    Task { await vm.setModel(option) }
                } label: {
                    if option == vm.model {
                        Label(option, systemImage: "checkmark")
                    } else {
                        Text(option)
                    }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Text(vm.model.isEmpty ? "Model" : vm.model)
                    .font(.caption)
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.caption2)
            }
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Theme.surface, in: Capsule())
            .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
        }
    }
}
#endif
