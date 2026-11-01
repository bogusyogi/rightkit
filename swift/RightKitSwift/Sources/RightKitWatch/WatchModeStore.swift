import Foundation

/// Dependency-free cache for the recording modes supplied by iPhone.
/// Cached values keep Watch recording useful while the phone is unavailable.
struct WatchMode: Codable, Identifiable, Equatable {
    let id: String
    let name: String
}

final class WatchModeStore {
    private static let modesKey = "watch.available-modes.v1"
    private static let selectedModeKey = "watch.selected-mode-id.v1"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    var modes: [WatchMode] {
        guard let data = defaults.data(forKey: Self.modesKey),
              let values = try? JSONDecoder().decode([WatchMode].self, from: data)
        else { return [] }
        return values
    }

    var selectedModeID: String? {
        get { defaults.string(forKey: Self.selectedModeKey) }
        set {
            if let newValue, modes.contains(where: { $0.id == newValue }) {
                defaults.set(newValue, forKey: Self.selectedModeKey)
            } else {
                defaults.removeObject(forKey: Self.selectedModeKey)
            }
        }
    }

    func replaceModes(_ values: [WatchMode]) {
        let unique = values.reduce(into: [WatchMode]()) { result, mode in
            guard !mode.id.isEmpty, !mode.name.isEmpty,
                  !result.contains(where: { $0.id == mode.id }) else { return }
            result.append(mode)
        }
        guard let data = try? JSONEncoder().encode(unique) else { return }
        defaults.set(data, forKey: Self.modesKey)

        if let selectedModeID, !unique.contains(where: { $0.id == selectedModeID }) {
            defaults.removeObject(forKey: Self.selectedModeKey)
        }
    }

    func replaceModes(from payload: [String: Any]) {
        guard let rawModes = payload["modes"] as? [[String: String]] else { return }
        replaceModes(rawModes.compactMap { raw in
            guard let id = raw["id"], let name = raw["name"] else { return nil }
            return WatchMode(id: id, name: name)
        })
    }
}
