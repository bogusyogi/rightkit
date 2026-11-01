import Foundation
import RightKitCredentials
import Security
import SwiftUI
import Observation

/// Named credentials stored in device-only Keychain items, isolated from
/// provider & connector credential services. Metadata contains names only.
///
/// Enabled MCP connectors may reference a value as ${NAME}; the connector
/// resolves it after the authentication gate & passes it only as transport
/// authentication. Values never enter tool arguments, chat context, journals
/// or exports. The environment-overlay API is retained for connected hosts.
public struct SessionSecretsStore: Sendable {
    public enum Failure: Error, LocalizedError, Equatable {
        case keychain(OSStatus)
        case invalidKey(String)

        public var errorDescription: String? {
            switch self {
            case let .keychain(status):
                return "Keychain error \(status)."
            case let .invalidKey(key):
                return "'\(key)' is not a valid session secret key."
            }
        }
    }

    private let service: String
    /// Keychain access is RightKitCredentials' `SecretsStore`.
    private let keychain: SecretsStore?
    nonisolated(unsafe) private let defaults: UserDefaults
    private static let keyNamesDefaultsKey = "app.coderight.session-secrets.key-names"

    public init(
        service: String = "app.coderight.CodeRight.session-secrets",
        defaults: UserDefaults = .standard
    ) {
        self.service = service
        self.defaults = defaults
        keychain = try? SecretsStore(namespace: service, accessibility: .whenUnlockedThisDeviceOnly)
    }

    private func backing() throws -> SecretsStore {
        guard let keychain else { throw Failure.keychain(errSecParam) }
        return keychain
    }

    private static func map(_ error: Error) -> Error {
        guard let error = error as? SecretsError else { return error }
        switch error {
        case let .keychain(status): return Failure.keychain(status)
        case .invalid: return Failure.keychain(errSecParam)
        case .guarded: return Failure.keychain(errSecNotAvailable)
        }
    }

    // MARK: - Validation

    /// Mirrors the daemon-side key validation in
    /// `coderight_config::session_secrets` so an invalid key is rejected in
    /// the same shape on both sides rather than silently failing to inject.
    public static func isValidKey(_ key: String) -> Bool {
        guard let first = key.unicodeScalars.first,
              CharacterSet.letters.contains(first) || first == "_"
        else { return false }
        return key.unicodeScalars.dropFirst().allSatisfy {
            CharacterSet.alphanumerics.contains($0) || $0 == "_"
        }
    }

    // MARK: - Write

    /// Stores a session secret value under `key`. Device-only, off backups.
    public func store(_ value: String, forKey key: String) throws {
        guard Self.isValidKey(key) else { throw Failure.invalidKey(key) }
        do {
            try backing().setString(key, value)
        } catch {
            throw Self.map(error)
        }

        var names = Set(storedKeyNames())
        names.insert(key)
        persistKeyNames(names)
    }

    // MARK: - Read

    /// Reads a session secret's value. Callers must only use this to build
    /// the run's environment block, never to render UI or to log/echo the
    /// value anywhere.
    public func read(forKey key: String) throws -> String? {
        do {
            return try backing().getString(key)
        } catch {
            throw Self.map(error)
        }
    }

    /// Key names only, sorted — safe to render in an editor UI or an audit
    /// trail. Never surfaces a value.
    public func keyNames() -> [String] {
        storedKeyNames().sorted()
    }

    /// Whether a value is currently saved for `key`, without reading it.
    public func hasValue(forKey key: String) -> Bool {
        (try? backing().exists(key)) ?? false
    }

    /// Builds the environment overlay for a run: every saved key mapped to
    /// its value. This is the one sanctioned exit point for values — the
    /// caller (session launch) must merge this into the process/daemon
    /// environment directly and must never pass it through a tool-call
    /// argument, transcript entry, journal write, or session-package export.
    public func environmentOverlay() throws -> [String: String] {
        var overlay: [String: String] = [:]
        for key in storedKeyNames() {
            if let value = try read(forKey: key) {
                overlay[key] = value
            }
        }
        return overlay
    }

    // MARK: - Delete

    public func remove(forKey key: String) throws {
        do {
            try backing().delete(key)
        } catch {
            throw Self.map(error)
        }
        var names = Set(storedKeyNames())
        names.remove(key)
        persistKeyNames(names)
    }

    // MARK: - Key-name index

    // Key *names* (never values) are tracked in `UserDefaults` so the editor
    // can list what's saved without a Keychain enumeration query. This index
    // can only ever fall out of sync in the safe direction (a stale name
    // whose Keychain item is gone — `hasValue`/`read` both treat that as
    // "not saved"), never leak a value.
    private func storedKeyNames() -> [String] {
        defaults.stringArray(forKey: Self.keyNamesDefaultsKey) ?? []
    }

    private func persistKeyNames(_ names: Set<String>) {
        defaults.set(Array(names), forKey: Self.keyNamesDefaultsKey)
    }
}

/// Backing store for the session secrets editor screen. Holds only key
/// *names* in memory (`keyNames`) — never a value — so there is no in-memory
/// state here that a debug dump or view snapshot could leak. Every value
/// read/write goes straight to `SessionSecretsStore`'s Keychain-backed calls.
@MainActor
@Observable
public final class SessionSecretsEditorStore {
    public private(set) var keyNames: [String] = []

    private let secrets: SessionSecretsStore

    public init(secrets: SessionSecretsStore = SessionSecretsStore()) {
        self.secrets = secrets
        refresh()
    }

    public func refresh() {
        keyNames = secrets.keyNames()
    }

    /// Saves `value` under `key`. The value parameter is write-only here:
    /// nothing on this store retains it after the call returns.
    public func save(key: String, value: String) throws {
        try secrets.store(value, forKey: key)
        refresh()
    }

    public func remove(key: String) {
        try? secrets.remove(forKey: key)
        refresh()
    }

    public func hasValue(key: String) -> Bool {
        secrets.hasValue(forKey: key)
    }
}

/// Editor for session-scoped secrets/env values (ENG-106). Shows only key
/// names and a "saved" indicator — a value is entered write-only through a
/// `SecureField` and is never read back into this view, so there is nothing
/// here that could accidentally render a secret on screen, in a transcript,
/// or in any export.
@MainActor
public struct SessionSecretsEditorView: View {
    @State private var store: SessionSecretsEditorStore
    @State private var isPresentingAdd = false

    public init(store: SessionSecretsEditorStore = SessionSecretsEditorStore()) {
        _store = State(initialValue: store)
    }

    public var body: some View {
        List {
            if store.keyNames.isEmpty {
                ContentUnavailableView(
                    "No session secrets",
                    systemImage: "key",
                    description: Text("Save a named credential for connector authentication. Use ${NAME} in a connector’s credential field. Values stay out of chat context.")
                )
            } else {
                ForEach(store.keyNames, id: \.self) { key in
                    SessionSecretRow(key: key, store: store)
                }
            }
        }
        .scrollContentBackground(.hidden).background(Theme.background)
        .navigationTitle("Session secrets")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    isPresentingAdd = true
                } label: {
                    Label("Add Secret", systemImage: "plus")
                }
            }
        }
        .sheet(isPresented: $isPresentingAdd) {
            SessionSecretAddView(store: store)
        }
    }
}

@MainActor
private struct SessionSecretRow: View {
    let key: String
    let store: SessionSecretsEditorStore

    var body: some View {
        HStack {
            Text(key)
                .font(.body.weight(.medium))
                .textSelection(.enabled)
            Spacer()
            if store.hasValue(key: key) {
                Label("Saved", systemImage: "checkmark.shield")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .labelStyle(.titleAndIcon)
            }
        }
        .swipeActions {
            Button(role: .destructive) {
                store.remove(key: key)
            } label: {
                Label("Remove", systemImage: "trash")
            }
        }
    }
}

@MainActor
private struct SessionSecretAddView: View {
    let store: SessionSecretsEditorStore

    @Environment(\.dismiss) private var dismiss
    @State private var key = ""
    @State private var value = ""
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Key") {
                    TextField("KEY_NAME", text: $key)
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                }
                Section("Value") {
                    // Write-only: the value never leaves this form except as
                    // the argument to `store.save`.
                    SecureField("Value", text: $value)
                        .textContentType(.password)
                }
                if let errorMessage {
                    Text(errorMessage).foregroundStyle(.red)
                }
            }
            .navigationTitle("Add Secret")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { save() }
                }
            }
        }
    }

    private func save() {
        do {
            try store.save(key: key, value: value)
            dismiss()
        } catch {
            errorMessage = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}
