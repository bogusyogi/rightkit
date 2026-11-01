import SwiftUI

/// Searchable provider-catalog picker. Gateways such as OpenRouter expose
/// hundreds of models, so a menu or wheel is unusable.
struct LocalModelPickerView: View {
    let models: [String]
    @Binding var selection: String?

    /// Optional provider effort/thinking control, shown as a footer section
    /// under the search list when the caller wires it up. Optional with a
    /// `nil` default so the existing call site in `LocalSessionView`
    /// (`LocalModelPickerView(models:selection:)`) keeps compiling
    /// unchanged — that call site is outside this lane's file allowlist, so
    /// this stays additive rather than switching it to a required binding.
    var effortSelection: Binding<String>? = nil
    var onSelect: (String) -> Void = { _ in }

    @Environment(\.dismiss) private var dismiss
    @State private var query = ""

    private var filteredModels: [String] {
        // Retired local-model identifiers never enter cloud requests.
        Self.filter(models: models, query: query).filter {
            $0 != ProviderAccount.onDeviceModelID
        }
    }

    static func filter(models: [String], query: String) -> [String] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return models }
        return models.filter { $0.localizedCaseInsensitiveContains(needle) }
    }

    var body: some View {
        NavigationStack {
            List {
                ForEach(filteredModels, id: \.self) { model in
                    Button {
                        selection = model
                        onSelect(model)
                        dismiss()
                    } label: {
                        HStack(spacing: 10) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(model)
                                    .foregroundStyle(Theme.text)
                                    .textSelection(.enabled)
                                if model.localizedCaseInsensitiveContains("free") {
                                    Text("Free")
                                        .font(.caption)
                                        .foregroundStyle(Theme.accent)
                                }
                            }
                            Spacer()
                            if model == selection {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(Theme.accent)
                            }
                        }
                    }.accessibilityIdentifier("model-option-\(model)")
                }
            }
            .scrollContentBackground(.hidden).background(Theme.background)
            .overlay {
                if filteredModels.isEmpty {
                    ContentUnavailableView.search(text: query)
                }
            }
            .safeAreaInset(edge: .bottom) {
                if let effortSelection {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Reasoning effort").font(.caption).foregroundStyle(Theme.muted)
                        Picker("Reasoning effort", selection: effortSelection) {
                            ForEach(ModelCatalog.EffortLevel.allCases) { level in
                                Text(level.title).tag(level.rawValue)
                            }
                        }
                        .pickerStyle(.segmented)
                    }
                    .padding(12)
                    .background(Theme.surface)
                }
            }
            .searchable(text: $query, prompt: "Search models")
            .autocorrectionDisabled()
            .textInputAutocapitalization(.never)
            .navigationTitle("Choose Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}
