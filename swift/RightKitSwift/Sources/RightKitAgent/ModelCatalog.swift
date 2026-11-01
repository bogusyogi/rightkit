import Foundation

/// Fetch the list of models a provider actually offers, so a user picks from
/// reality instead of typing an id and finding out at session-start whether
/// it exists. Mirrors the desktop app's catalog-then-pick flow.
///
/// A plain HTTPS GET with the API key attached — the same request any client
/// of that provider would make. No engine/Rust involvement: this is read-only
/// metadata, not agent work.
enum ModelCatalog {
    /// A stable catalog item that can be shown alongside provider-discovered
    /// models without changing the existing `[String]` fetch API.
    struct Entry: Codable, Hashable, Identifiable, Sendable {
        /// Stable model identifier used by the picker and persistence layer.
        let id: String
        /// Human-readable model name.
        let name: String
        /// Whether this item can actually be selected on this build/device.
        let isAvailable: Bool
        /// Why the item is unavailable, when it cannot be selected.
        let availabilityNote: String?
    }

    /// Catalog metadata for local models planned by the Rust cookbook.
    ///
    /// These entries are intentionally unavailable: no model weights are
    /// bundled in this workspace, and catalog presence is not readiness.
    static var onDeviceEntries: [Entry] {
        [
            Entry(
                id: "qwen2.5-coder-7b",
                name: "Qwen2.5 Coder 7B Instruct",
                isAvailable: false,
                availabilityNote: "No on-device model weights are bundled."
            ),
            Entry(
                id: "qwen2.5-coder-14b",
                name: "Qwen2.5 Coder 14B Instruct",
                isAvailable: false,
                availabilityNote: "No on-device model weights are bundled."
            ),
            Entry(
                id: "deepseek-coder-v2-lite",
                name: "DeepSeek Coder V2 Lite",
                isAvailable: false,
                availabilityNote: "No on-device model weights are bundled."
            ),
            Entry(
                id: "llama-cpp-coder-7b",
                name: "Open coding 7B GGUF via llama.cpp",
                isAvailable: false,
                availabilityNote: "No on-device model weights are bundled."
            ),
        ]
    }

    /// Combine local catalog metadata with provider-discovered model ids.
    /// Existing callers can continue using `fetch`, while newer callers get
    /// additive entries and retain the source ordering they expect.
    static func entries(withProviderModelIDs modelIDs: [String]) -> [Entry] {
        onDeviceEntries + modelIDs.sorted().map {
            Entry(id: $0, name: $0, isAvailable: true, availabilityNote: nil)
        }
    }

    struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// Provider "effort" / "thinking budget" control. Raw values match what
    /// `LocalSessionView.start()` already sends through unchanged (it maps
    /// `"auto"` to `nil`, everything else passes through as
    /// `reasoningEffort` to the launch request) — this only replaces the
    /// hardcoded list that used to live inline in `LocalSessionLauncher`
    /// with one shared, documented source, and gives the model picker a
    /// way to surface the same set of choices next to a chosen model.
    enum EffortLevel: String, CaseIterable, Identifiable {
        case auto
        case none
        case low
        case medium
        case high
        case xhigh

        var id: String { rawValue }

        var title: String {
            switch self {
            case .auto: return "Auto"
            case .none: return "Off"
            case .low: return "Low"
            case .medium: return "Medium"
            case .high: return "High"
            case .xhigh: return "Extra high"
            }
        }

        var detail: String {
            switch self {
            case .auto:
                return "Provider default — no explicit effort/thinking budget requested."
            case .none:
                return "Disables extended reasoning where the provider supports turning it off."
            case .low:
                return "Minimal extra reasoning — fastest, cheapest of the explicit levels."
            case .medium:
                return "Balanced reasoning depth."
            case .high:
                return "More reasoning tokens before responding — slower and costs more."
            case .xhigh:
                return "Maximum reasoning depth some providers expose — slowest and most expensive."
            }
        }
    }

    /// `baseURL` is used as given (already includes `/v1` for OpenAI-style
    /// gateways per the presets in `AddProviderView`). Returns model ids
    /// sorted for a stable picker order.
    static func fetch(
        wireProtocol: ProviderAccount.WireProtocol,
        baseURL: String,
        apiKey: String
    ) async throws -> [String] {
        var trimmed = baseURL.trimmingCharacters(in: .whitespaces)
        while trimmed.hasSuffix("/") { trimmed.removeLast() }

        var request: URLRequest
        switch wireProtocol {
        case .openai:
            guard let url = URL(string: "\(trimmed)/models") else {
                throw Failure(message: "Base URL is not valid.")
            }
            request = URLRequest(url: url)
            request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        case .anthropic:
            // Anthropic's base URL (default `https://api.anthropic.com`) has
            // no `/v1` suffix in the preset, unlike the OpenAI-style ones.
            guard let url = URL(string: "\(trimmed)/v1/models") else {
                throw Failure(message: "Base URL is not valid.")
            }
            request = URLRequest(url: url)
            request.setValue(apiKey, forHTTPHeaderField: "x-api-key")
            request.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")
        }
        request.timeoutInterval = 15

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw Failure(message: "No response from the provider.")
        }
        guard (200..<300).contains(http.statusCode) else {
            let message: String
            switch http.statusCode {
            case 401: message = "Provider rejected this key (401). Check your key & selected service."
            case 403: message = "This key cannot access these models (403). Check your selected service & plan."
            case 404: message = "Model catalog was not found (404). Check the provider endpoint."
            case 429: message = "Provider limit reached (429). Check your selected plan or try again shortly."
            default:
                let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
                let detail = (body?["error"] as? [String: Any])?["message"] as? String
                    ?? body?["message"] as? String
                message = "Could not load models (\(http.statusCode)). "
                    + (detail.map { String($0.prefix(200)) } ?? "Try again shortly.")
            }
            throw Failure(message: message)
        }

        // Both OpenAI-compatible and Anthropic's `/v1/models` return
        // `{"data": [{"id": "..."}, ...]}` — one decode path covers both.
        struct Entry: Decodable { let id: String }
        struct Envelope: Decodable { let data: [Entry] }
        guard let envelope = try? JSONDecoder().decode(Envelope.self, from: data) else {
            throw Failure(message: "Could not read the model list this provider returned.")
        }
        return envelope.data.map(\.id).sorted()
    }
}
