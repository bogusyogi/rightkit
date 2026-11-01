import SwiftUI

/// User-facing fleet configuration & peer-contact approval surface.
///
/// Team identity and shared context live in app-owned preferences; only this
/// view creates a bridge carrying them. Agents receive projected read-only
/// prompt text through Rust and have no path back to these fields.
@MainActor
struct LocalTeamFleetView: View {
    @AppStorage("app.coderight.teamFleet.manifest") private var manifestJson = ""
    @AppStorage("app.coderight.teamFleet.sharedBrief") private var sharedBrief = ""
    @State private var fleet: LocalTeamFleetBridge.Summary?
    @State private var inbox: [LocalTeamFleetBridge.ReceivedTask] = []
    @State private var requesterPeerId = ""
    @State private var targetPeerId = ""
    @State private var task = ""
    @State private var depth = 1
    @State private var pendingApproval = false
    @State private var error: String?
    @State private var receipt: LocalTeamFleetBridge.DispatchReceipt?
    @State private var execution: LocalTeamFleetBridge.ExecutionResult?

    var body: some View {
        Form {
            Section {
                TextEditor(text: $manifestJson)
                    .font(.footnote.monospaced())
                    .frame(minHeight: 120)
                    .accessibilityIdentifier("team-fleet-manifest")
                Button("Import fleet") {
                    Task { await importFleet() }
                }
                .disabled(manifestJson.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                if let fleet {
                    LabeledContent("Team", value: fleet.teamId)
                    Text(fleet.peerIds.joined(separator: " · "))
                        .font(.footnote)
                        .foregroundStyle(Theme.muted)
                }
            } header: {
                Text("Named fleet")
            } footer: {
                Text(
                    "Paste a versioned team manifest. Unknown fields, duplicate peers, unsafe ids, and unbounded depth fail closed."
                )
            }

            Section("Shared team brief") {
                TextEditor(text: $sharedBrief)
                    .frame(minHeight: 100)
                    .accessibilityIdentifier("team-fleet-shared-brief")
                Text("Only you can edit this brief. Peers receive it as read-only prompt context.")
                    .font(.footnote)
                    .foregroundStyle(Theme.muted)
                if let prompt = fleet?.sharedContextPrompt {
                    Text(prompt)
                        .font(.footnote.monospaced())
                        .textSelection(.enabled)
                }
            }

            if fleet != nil {
                Section {
                    TextField("Requester peer id", text: $requesterPeerId)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Target peer id", text: $targetPeerId)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Task", text: $task, axis: .vertical)
                        .lineLimit(2...5)
                    Stepper("Delegation depth: \(depth)", value: $depth, in: 1...32)
                    Button("Request peer contact") {
                        pendingApproval = true
                    }
                    .disabled(requesterPeerId.isEmpty || targetPeerId.isEmpty || task.isEmpty)
                } header: {
                    Text("Delegate work")
                } footer: {
                    Text(
                        "Peer contact is never implicit. Confirmation below is required for each requester → target dispatch."
                    )
                }

                Section("Peer inbox") {
                    if inbox.isEmpty {
                        Text("Choose a peer then refresh its direct inbox.")
                            .foregroundStyle(Theme.muted)
                    } else {
                        ForEach(inbox, id: \.messageId) { message in
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(message.requesterPeerId) → \(message.targetPeerId)")
                                    .font(.caption.weight(.semibold))
                                Text(message.task)
                                if let brief = message.sharedContextPrompt {
                                    Text(brief)
                                        .font(.caption.monospaced())
                                        .foregroundStyle(Theme.muted)
                                        .lineLimit(3)
                                }
                            }
                        }
                    }
                    TextField("Peer id", text: $targetPeerId)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Refresh inbox") {
                        Task { await refreshInbox() }
                    }
                    .disabled(targetPeerId.isEmpty)
                    Button("Preview next peer prompt") {
                        Task { await executeNextPeerTask() }
                    }
                    .disabled(targetPeerId.isEmpty)
                }
            }

            if let receipt {
                Section("Last dispatch") {
                    Label("Queued without waiting on peer", systemImage: "tray.and.arrow.up")
                    Text(receipt.messageId)
                        .font(.footnote.monospaced())
                        .textSelection(.enabled)
                }
            }

            if let execution {
                Section("Prepared peer prompt") {
                    Label(
                        execution.prepared ? "Pending local peer execution" : "Peer prompt unavailable",
                        systemImage: execution.prepared ? "text.badge.checkmark" : "xmark.octagon"
                    )
                    .foregroundStyle(execution.prepared ? Theme.accent : Theme.danger)
                    Text(execution.prompt)
                        .font(.footnote.monospaced())
                        .textSelection(.enabled)
                }
            }

            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(Theme.danger)
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Peer Fleet")
        .confirmationDialog(
            "Approve peer contact?",
            isPresented: $pendingApproval,
            titleVisibility: .visible
        ) {
            Button("Approve \(requesterPeerId) → \(targetPeerId)") {
                Task { await dispatchAfterUserApproval() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This sends one task to selected peer. It does not grant future peer contact.")
        }
    }

    private var bridge: LocalTeamFleetBridge {
        LocalTeamFleetBridge(
            configuration: .init(
                storageDir: fleetStorageDirectory.path,
                manifestJson: manifestJson,
                sharedContext: sharedBrief.isEmpty ? [:] : ["brief": sharedBrief]
            ))
    }

    private var fleetStorageDirectory: URL {
        let root = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return root.appendingPathComponent("PeerFleet", isDirectory: true)
    }

    private func importFleet() async {
        do {
            try FileManager.default.createDirectory(
                at: fleetStorageDirectory,
                withIntermediateDirectories: true
            )
            fleet = try await bridge.importFleet()
            inbox = []
            error = nil
            if requesterPeerId.isEmpty { requesterPeerId = fleet?.peerIds.first ?? "" }
            if targetPeerId.isEmpty { targetPeerId = fleet?.peerIds.dropFirst().first ?? "" }
        } catch {
            fleet = nil
            self.error = error.localizedDescription
        }
    }

    private func dispatchAfterUserApproval() async {
        do {
            receipt = try await bridge.dispatchAfterUserApproval(
                requesterPeerId: requesterPeerId,
                targetPeerId: targetPeerId,
                task: task,
                depth: UInt8(depth)
            )
            task = ""
            execution = nil
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func refreshInbox() async {
        do {
            inbox = try await bridge.receive(for: targetPeerId)
            error = nil
        } catch {
            inbox = []
            self.error = error.localizedDescription
        }
    }

    private func executeNextPeerTask() async {
        do {
            execution = try await bridge.executeNext(for: targetPeerId)
            inbox = try await bridge.receive(for: targetPeerId)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}
