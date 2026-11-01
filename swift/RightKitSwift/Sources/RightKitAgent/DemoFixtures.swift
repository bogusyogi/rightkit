import Foundation

/// Canned, offline data for the scripted demo walkthrough (APP-09). Nothing here makes a
/// network call or touches Keychain/persistent stores — every value is a literal.
public enum DemoFixtures {
    /// A fake paired computer, shown in the demo's pairing step. Never written to
    /// `DaemonStore`/Keychain — the demo renders it in its own throwaway view state.
    public static let daemon = DaemonConfig(
        base: "http://192.168.1.42:5099",
        token: "demo-token",
        label: "Adrian's MacBook Pro",
        targetKind: .computer
    )

    /// A short, scripted transcript: user request, assistant narration, a tool approval,
    /// and a final confirmation — enough to demonstrate pairing → session → approval without
    /// needing a live daemon or model.
    public static let transcript: [TranscriptMessage] = [
        TranscriptMessage(id: "demo-1", role: .user, text: "Add a loading spinner to the login button"),
        TranscriptMessage(
            id: "demo-2",
            role: .assistant,
            text: "I'll add a spinner state to LoginButton.swift and wire it to the auth call."
        ),
        TranscriptMessage(
            id: "demo-3",
            role: .approval,
            text: "Apply edit to LoginButton.swift?",
            diff: """
            --- a/LoginButton.swift
            +++ b/LoginButton.swift
            @@ -1,3 +1,6 @@
             struct LoginButton: View {
            +    @State private var isLoading = false
            +
                 var body: some View {
            """
        ),
        TranscriptMessage(id: "demo-4", role: .user, text: "Approved"),
        TranscriptMessage(
            id: "demo-5",
            role: .assistant,
            text: "Done — the button now shows a spinner while signing in."
        ),
    ]

    /// A finished demo session record, ready to display in a session-detail-shaped view.
    public static let session = LocalSessionRecord(
        id: "demo-session",
        title: "Add a loading spinner to the login button",
        workspaceId: "demo-workspace",
        providerAccountId: "demo-provider",
        model: "demo-model",
        createdAt: Date(timeIntervalSince1970: 1_700_000_000),
        updatedAt: Date(timeIntervalSince1970: 1_700_000_600),
        messages: transcript
    )
}
