#if os(iOS)
import SwiftUI
import AuthenticationServices

// Account section — HR-ACCOUNT-P1..P8 (docs/DOGFOOD.md). Passwordless
// HeardRight account: magic-link sign-in, Sign in with Apple, sign-out, and
// an account-deletion request. A standalone View (not an extension of
// ContentView) since it owns its own `AccountSessionStore` instance.
//
// `AccountSessionStore` (`AccountSessionStore.swift`, the parallel
// "account-session-and-signature" lane's file, NOT owned by this lane) did
// NOT exist when this lane started and landed mid-way through this file's
// first draft. This is the RECONCILED version, checked directly against
// that file's real shape as read:
//   - `AccountSessionStore` is a plain `struct` (not `ObservableObject`) —
//     its own header says so explicitly: "NOT itself an ObservableObject/
//     @MainActor view model by design ... the UI lane can wrap however it
//     wants". So this view owns a plain `private let accountSession =
//     AccountSessionStore()`, not an `@EnvironmentObject`/injected
//     dependency — there is no app-root wiring to add or wait on.
//   - Real method names/signatures: `status() async throws ->
//     AccountSessionStatus`, `beginMagicLink(email:) async throws`,
//     `complete(token:) async throws -> AccountSessionStatus` (not
//     "completeSession"), `signOut() async` (NOT throwing — no `try`/
//     `catch` around it), `devicesList() async throws -> [AccountDeviceRow]`
//     (not "listDevices"), `rename(deviceIdHash:label:)`,
//     `remove(deviceIdHash:)`, `reassign(deviceIdHash:)`.
//   - `AccountSessionStatus.expiresAt` is `Int64?` (Unix seconds), not a
//     string.
//   - `delete() async throws` (HR-ACCOUNT-P8) calls the real
//     `DELETE /v1/shop/account` endpoint directly — see `deleteAccountRow`
//     below for the confirm-then-delete flow and `.activeSubscription`'s
//     409 handling.
//
// HR-ACCOUNT-P1/P5: nothing in DictationController, recording, or the local
// model path references `accountSession` — an account is reachable only
// from this optional Settings surface, never required for Free/local use.
// HR-ACCOUNT-P4's "purchase is not held behind web signup" is guaranteed
// structurally on iOS by `storeKitPurchasePanel` (ContentViewSettings.swift):
// its Buy Pro button calls only `storeKit.purchase()` /
// `proResolver.resolve()`, neither of which touches `accountSession` — this
// view's "Pro across Apple devices" row is reachable only AFTER
// `proResolver.state == .pro`, as a dismissible follow-up, never a gate.
struct AccountSettingsView: View {
    private let accountSession = AccountSessionStore()

    @ObservedObject var license: LicenseController

    let ink: Color
    let muted: Color
    let ember: Color
    let ok: Color
    let line: Color
    let surface: Color
    let paper: Color

    /// Whether Pro is currently active on this device — passed in from
    /// `ContentViewSettings.swift` (`proResolver.state == .pro`) so the
    /// optional "Pro across Apple devices" row only ever appears
    /// after a purchase is already unlocked and usable.
    var hasPro: Bool = false

    @State private var status: AccountSessionStatus?
    @State private var statusLoadFailed = false
    @State private var email = ""
    @State private var linkSent = false
    @State private var token = ""
    @State private var busy = false
    @State private var message = ""
    @State private var linkPromptDismissed = false
    @State private var deleteConfirming = false
    @State private var windowsLinkCode: WindowsLinkCodeResponse?
    @State private var windowsLinkBusy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Account")
                .font(.headline.weight(.black))
                .foregroundStyle(ink)

            Text("Optional and passwordless. Needed only for cross-platform activation, lost-device removal, and web device management — never for Free use or local dictation.")
                .font(.caption)
                .foregroundStyle(muted)
                .fixedSize(horizontal: false, vertical: true)

            if statusLoadFailed {
                Text("Account status is unavailable right now. Free use and any Pro entitlement already active on this iPhone are unaffected.")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(muted)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let status {
                signedInOrOutContent(status)
            } else {
                HStack(spacing: 8) {
                    ProgressView().tint(muted)
                    Text("Checking account status…")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(muted)
                }
            }


            if hasPro {
                windowsLinkRow
            }

            if !message.isEmpty, status?.signedIn != false {
                Text(message)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .sheetSectionStyle(surface: surface, line: line)
        .task { await refresh() }
    }

    private var windowsLinkRow: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Link a Windows PC")
                .font(.caption.weight(.bold))
                .foregroundStyle(ink)
            Text("Create a one-time code to enter in HeardRight on Windows. No license key, App Store receipt, or iCloud secret is shared.")
                .font(.caption2)
                .foregroundStyle(muted)
                .fixedSize(horizontal: false, vertical: true)

            if let windowsLinkCode {
                Text(windowsLinkCode.code)
                    .font(.title3.monospaced().weight(.black))
                    .foregroundStyle(ink)
                    .textSelection(.enabled)
                Text("Single use · expires \(windowsLinkExpiry(windowsLinkCode.expiresAt))")
                    .font(.caption2)
                    .foregroundStyle(muted)
            }

            Button {
                Task { await requestWindowsLink() }
            } label: {
                Label(
                    windowsLinkBusy ? "Generating…" : (windowsLinkCode == nil ? "Generate Windows code" : "Generate a new code"),
                    systemImage: "desktopcomputer"
                )
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(HRSecondaryButtonStyle(paper: ink, coffee: surface, border: line))
            .disabled(windowsLinkBusy)
        }
        .padding(.top, 4)
    }

    @MainActor
    private func requestWindowsLink() async {
        guard !windowsLinkBusy else { return }
        windowsLinkBusy = true
        defer { windowsLinkBusy = false }
        windowsLinkCode = await license.requestWindowsLinkCode()
    }

    private func windowsLinkExpiry(_ raw: String) -> String {
        guard let date = ISO8601DateFormatter().date(from: raw) else { return raw }
        return date.formatted(date: .omitted, time: .shortened)
    }

    @ViewBuilder
    private func signedInOrOutContent(_ status: AccountSessionStatus) -> some View {
        if status.signedIn {
            VStack(alignment: .leading, spacing: 10) {
                Label("Signed in · \(status.email ?? "")", systemImage: "checkmark.seal")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(ok)
                if let expiresAt = status.expiresAt {
                    // Unix seconds -> Date.
                    Text("Session valid through \(Date(timeIntervalSince1970: TimeInterval(expiresAt)).formatted(date: .abbreviated, time: .omitted))")
                        .font(.caption2)
                        .foregroundStyle(muted)
                }

                Button {
                    Task { await signOut() }
                } label: {
                    Label("Sign out", systemImage: "rectangle.portrait.and.arrow.right")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(HRSecondaryButtonStyle(paper: ink, coffee: surface, border: line))
                .disabled(busy)

                deleteAccountRow(email: status.email)
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                Text("Sign in to link verified purchases to your HeardRight account.")
                    .font(.caption)
                    .foregroundStyle(muted)
                    .fixedSize(horizontal: false, vertical: true)

                signInWithAppleButton
                if busy {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(message.isEmpty ? "Working…" : message).font(.caption)
                    }
                    .accessibilityElement(children: .combine)
                }

                if !message.isEmpty, !busy {
                    Text(message)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(muted)
                        .fixedSize(horizontal: false, vertical: true)
                }

                Text("or email sign-in")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(muted)

                if !linkSent {
                    TextField(
                        "Email",
                        text: $email,
                        prompt: Text("you@example.com").foregroundStyle(muted)
                    )
                        .keyboardType(.emailAddress)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .textFieldStyle(HRTextFieldStyle(paper: paper, line: line, ink: ink))

                    Button {
                        Task { await sendMagicLink() }
                    } label: {
                        Label(busy ? "Sending…" : "Send sign-in link", systemImage: "envelope")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(HRPrimaryButtonStyle(disabled: busy || email.trimmingCharacters(in: .whitespaces).isEmpty, ink: ink, ember: ember))
                    .disabled(busy || email.trimmingCharacters(in: .whitespaces).isEmpty)
                } else {
                    Text("Check your email at \(email) for a sign-in link.")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(muted)
                        .fixedSize(horizontal: false, vertical: true)

                    TextField(
                        "Sign-in token",
                        text: $token,
                        prompt: Text("Paste token from the email").foregroundStyle(muted)
                    )
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .textFieldStyle(HRTextFieldStyle(paper: paper, line: line, ink: ink))

                    Button {
                        Task { await completeSignIn() }
                    } label: {
                        Label(busy ? "Signing in…" : "Complete sign-in", systemImage: "key")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(HRSecondaryButtonStyle(paper: ink, coffee: surface, border: line))
                    .disabled(busy || token.trimmingCharacters(in: .whitespaces).isEmpty)
                }

                // HR-ACCOUNT-P4: the ONLY nudge toward linking on this
                // screen, shown only once Pro is already active and usable,
                // and dismissible without doing anything to that Pro state.
                if hasPro, !linkPromptDismissed {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Pro across Apple devices")
                            .font(.caption.weight(.bold))
                            .foregroundStyle(ink)
                        Text("Enable Sync across Apple devices on both devices using the same iCloud account. Account sign-in does not transfer Pro.")
                            .font(.caption2)
                            .foregroundStyle(muted)
                            .fixedSize(horizontal: false, vertical: true)
                        Button("Not now") { linkPromptDismissed = true }
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(muted)
                    }
                    .padding(.top, 4)
                }
            }
        }
    }

    private var signInWithAppleButton: some View {
        SignInWithAppleButton(.continue) { request in
            request.requestedScopes = [.email]
        } onCompletion: { result in
            Task { await handleAppleSignIn(result) }
        }
        .signInWithAppleButtonStyle(.white)
        .frame(height: 44)
        .disabled(busy)
    }

    /// HR-ACCOUNT-P8: `DELETE /v1/shop/account` is real (`routes/
    /// account.ts`) and `AccountSessionStore.delete()` calls it directly —
    /// this is a real destructive action, not a support request. A rejected
    /// call must never render as success: on failure `deleteAccount()`
    /// leaves `deleteConfirming` true so the confirmation panel stays up
    /// with the error message shown, matching the same never-render-
    /// failure-as-success standard `remove()` already follows for devices.
    @ViewBuilder
    private func deleteAccountRow(email: String?) -> some View {
        if !deleteConfirming {
            Button("Delete account") { deleteConfirming = true }
                .font(.caption.weight(.semibold))
                .foregroundStyle(.red)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                Text("Delete account — are you sure?")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(ink)
                Text("This deletes: your account profile, email, and every device linked to this account. This keeps: your order and purchase history, anonymized (customer link removed) so receipts, refunds, and tax/legal records survive. This cannot be undone.")
                    .font(.caption2)
                    .foregroundStyle(muted)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    Button(busy ? "Deleting…" : "Confirm delete") {
                        Task { await deleteAccount() }
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.red)
                    .disabled(busy)

                    Button("Cancel") { deleteConfirming = false }
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(muted)
                        .disabled(busy)
                }
            }
        }
    }

    @MainActor
    private func deleteAccount() async {
        guard !busy else { return }
        busy = true
        message = ""
        defer { busy = false }
        do {
            try await accountSession.delete()
            deleteConfirming = false
            status = AccountSessionStatus(signedIn: false, email: nil, expiresAt: nil)
            message = "Account deleted. Your HeardRight product profile and devices are gone; anonymized purchase records are retained for accounting and legal requirements."
        } catch {
            // Left in the confirming state so the error is visible next to
            // the still-live Confirm/Cancel controls — never shown as
            // deleted on a rejected call.
            message = "Couldn't delete your account: \(error.localizedDescription)"
        }
    }

    @MainActor
    private func refresh() async {
        do {
            status = try await accountSession.status()
            statusLoadFailed = false
            if status?.signedIn == true {
                do {
                    status = try await accountSession.renewedStatus()
                } catch AccountSessionError.sessionExpired {
                    status = try await accountSession.status()
                } catch {
                    // Keep locally known signed-in state during transient
                    // network failures; renewal retries next time this screen opens.
                }
            }
        } catch {
            // Unknown must never render as "signed out" — that would show a
            // sign-in form to someone who is actually signed in and hide a
            // Pro entitlement that has nothing to do with this failure.
            statusLoadFailed = true
        }
    }

    @MainActor
    private func sendMagicLink() async {
        let trimmed = email.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, !busy else { return }
        busy = true
        message = ""
        defer { busy = false }
        do {
            try await accountSession.beginMagicLink(email: trimmed)
            linkSent = true
            message = "Link sent."
        } catch {
            message = "Couldn't send the link: \(error.localizedDescription)"
        }
    }

    @MainActor
    private func completeSignIn() async {
        let trimmed = token.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, !busy else { return }
        busy = true
        message = ""
        defer { busy = false }
        do {
            status = try await accountSession.complete(token: trimmed)
            token = ""
            linkSent = false
            email = ""
            message = "Signed in."
        } catch {
            // A failed completion keeps the sign-in form up — never shown as
            // signed in on a rejected call.
            message = "Couldn't complete sign-in: \(error.localizedDescription)"
        }
    }

    @MainActor
    private func handleAppleSignIn(_ result: Result<ASAuthorization, Error>) async {
        guard !busy else { return }
        busy = true
        message = "Finishing Apple sign-in…"
        defer { busy = false }
        switch result {
        case .failure(let error):
            message = "Sign in with Apple failed: \(error.localizedDescription)"
        case .success(let authorization):
            guard
                let credential = authorization.credential as? ASAuthorizationAppleIDCredential,
                let identityTokenData = credential.identityToken,
                let identityToken = String(data: identityTokenData, encoding: .utf8)
            else {
                message = "Sign in with Apple did not return a usable credential."
                return
            }
            do {
                status = try await accountSession.signInWithApple(identityToken: identityToken)
                statusLoadFailed = false
                message = "Signed in with Apple."
            } catch {
                message = "Couldn't finish Sign in with Apple: \(error.localizedDescription)"
            }
        }
    }

    @MainActor
    private func signOut() async {
        guard !busy else { return }
        busy = true
        message = ""
        defer { busy = false }
        // signOut() is NOT throwing — it is local-first and clears the
        // on-device session record unconditionally; a best-effort server
        // logout failure inside it must not (and structurally cannot) block
        // that local clear. See AccountSessionStore.swift's own doc comment
        // on this method for why.
        await accountSession.signOut()
        await refresh()
        message = "Signed out. Pro on this device is unaffected."
    }
}
#endif
