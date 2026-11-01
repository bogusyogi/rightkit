#if os(macOS)
import Foundation
import Observation
import RightKitHub
import SwiftUI

// The agent secure box (bogusyogi/pulse#345 logins, #346 virtual cards): the three owner-only
// switches, the saved items (labels, sites and limits, never a value) and the forms that add a login
// or a card. The engine methods are `remote.secure.put|list|remove` (core/src/remote/agent_secrets.rs).
//
// A secret is typed only into a `SecureField` of this process and goes straight into
// `remote.secure.put` with the Pulse PIN; it is never put in an observable property, a log line,
// an error text or a search string, and the field is cleared as soon as the call is sent. The
// Tauri hub's twin is hub/src/views/SecureBoxSettings.tsx, which has no secret field at all (the
// system dialog opens from its command).

@MainActor
@Observable
final class SecureBoxModel {
    struct Item: Decodable, Identifiable, Equatable, Sendable {
        let id: String
        let kind: String
        let label: String
        let host: String?
        let hosts: [String]?
        /// A login has a one-time code saved (the engine never lists the secret).
        let totp: Bool?
        let last4: String?
        let maxCents: Int?
        let autoCents: Int?
        let monthCents: Int?
        /// What this card used this month, when the engine lists it per card.
        let spentCents: Int?
        /// Purchases with this card that ended without Pulse knowing whether they went through.
        let held: Int?
    }

    struct Listing: Decodable, Sendable {
        let items: [Item]
        let spentCents: [String: Int]?
        /// The effective global caps (`remoteFormFillPaymentsAutoCapCents`, `remoteFormFillPaymentsMonthCapCents`).
        let capAutoCents: Int?
        let capMonthCents: Int?
        /// Per card: cents reserved by open purchases, and by purchases whose outcome is unknown.
        let heldCents: [String: Int]?
        let unknownCents: [String: Int]?
    }

    /// A protected switch waiting for the Pulse PIN (the engine asked for it).
    struct PinAsk: Equatable {
        let key: String
        let value: SettingsJSON
        var problem: String?
    }

    private(set) var items: [Item] = []
    private(set) var spent: [String: Int] = [:]
    private(set) var unknown: [String: Int] = [:]
    private(set) var capAuto: Int?
    private(set) var capMonth: Int?
    private(set) var problem: String?
    private(set) var status: String?
    private(set) var busy = false
    private(set) var pinAsk: PinAsk?
    private let engine: any EngineClient

    init(engine: any EngineClient) {
        self.engine = engine
    }

    // MARK: Reading

    func load() async {
        do {
            let listing = try await engine.call("remote.secure.list", EngineEmpty(), as: Listing.self, timeout: .seconds(15))
            items = listing.items
            spent = listing.spentCents ?? [:]
            unknown = listing.unknownCents ?? [:]
            capAuto = listing.capAutoCents
            capMonth = listing.capMonthCents
            problem = nil
        } catch {
            problem = SettingsFormat.describe(error)
        }
    }

    func remove(_ item: Item) async {
        busy = true
        defer { busy = false }
        do {
            _ = try await engine.call("remote.secure.remove", params: try JSONEncoder().encode(["id": item.id]), timeout: .seconds(15))
            status = "Removed \(item.label)."
            problem = nil
            await load()
        } catch {
            problem = SettingsFormat.describe(error)
        }
    }

    // MARK: Saving (the secrets pass through here once and are not kept)

    private struct LoginBody: Encodable {
        let kind = "login"
        let label: String
        let host: String
        let username: String
        let password: String
        let pin: String
        /// Omitted from the JSON when nil.
        var totp: String?
    }

    /// Adds or replaces only a saved login's one-time code (the engine keeps its username and password).
    private struct TotpBody: Encodable {
        let kind = "login"
        let id: String
        let totp: String
        let pin: String
    }

    private struct CardBody: Encodable {
        let kind = "card"
        let label: String
        let hosts: [String]
        let number: String
        let expMonth: Int
        let expYear: Int
        let cvv: String
        let name: String
        let zip: String
        let virtual = true
        let maxCents: Int
        let autoCents: Int
        let monthCents: Int
        let pin: String
    }

    struct CardEntry {
        var label = ""
        var shops = ""
        var zip = ""
        var maxDollars = ""
        var autoDollars = "0"
        var monthDollars = ""
        var virtual = false
        var name = ""
        var number = ""
        var expiry = ""
        var cvv = ""
    }

    /// "50" or "50.25" to whole cents; nil when it is not a plain amount.
    static func cents(_ text: String) -> Int? {
        var t = text.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("$") { t.removeFirst() }
        let parts = t.split(separator: ".", omittingEmptySubsequences: false)
        guard (1...2).contains(parts.count), parts[0].count >= 1, parts[0].count <= 7,
              parts[0].allSatisfy(\.isASCII), parts[0].allSatisfy(\.isNumber),
              let whole = Int(parts[0]) else { return nil }
        var fraction = 0
        if parts.count == 2 {
            guard parts[1].count >= 1, parts[1].count <= 2, parts[1].allSatisfy(\.isNumber),
                  let f = Int(parts[1]) else { return nil }
            fraction = parts[1].count == 1 ? f * 10 : f
        }
        return whole * 100 + fraction
    }

    /// `MM/YY` or `MM/YYYY`.
    static func expiry(_ text: String) -> (month: Int, year: Int)? {
        let pair = text.trimmingCharacters(in: .whitespaces).split(separator: "/", omittingEmptySubsequences: false)
        guard pair.count == 2,
              let month = Int(pair[0].trimmingCharacters(in: .whitespaces)), (1...12).contains(month) else { return nil }
        let year = pair[1].trimmingCharacters(in: .whitespaces)
        guard year.count == 2 || year.count == 4, let value = Int(year) else { return nil }
        return (month, value)
    }

    /// Saves a login. Returns true when it was saved. Neither secret is kept.
    func saveLogin(label: String, host: String, username: String, password: String, totp: String = "", pin: String) async -> Bool {
        guard !label.trimmingCharacters(in: .whitespaces).isEmpty, !host.trimmingCharacters(in: .whitespaces).isEmpty else {
            problem = "Give the login a label and the site it is for."
            return false
        }
        guard !username.isEmpty, !password.isEmpty else {
            problem = "Type the username and the password."
            return false
        }
        guard !pin.isEmpty else {
            problem = "Type your Pulse PIN to save."
            return false
        }
        var body = LoginBody(
            label: label.trimmingCharacters(in: .whitespaces), host: host.trimmingCharacters(in: .whitespaces),
            username: username, password: password, pin: pin)
        if !totp.isEmpty { body.totp = totp.trimmingCharacters(in: .whitespacesAndNewlines) }
        return await put(body, saved: "Login saved.")
    }

    /// Adds or replaces a saved login's one-time code. Returns true when it was saved. The secret is not kept.
    func setTotp(id: String, totp: String, pin: String) async -> Bool {
        let code = totp.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !code.isEmpty else {
            problem = "Type the one-time code secret (the setup key or the otpauth:// address)."
            return false
        }
        guard !pin.isEmpty else {
            problem = "Type your Pulse PIN to save."
            return false
        }
        return await put(TotpBody(id: id, totp: code, pin: pin), saved: "One-time code saved.")
    }

    /// Saves a virtual or tokenized card. Returns true when it was saved. No card field is kept.
    func saveCard(_ card: CardEntry, pin: String) async -> Bool {
        let shops = card.shops.split(whereSeparator: { $0 == "," || $0 == " " || $0 == "\n" }).map(String.init)
        guard !card.label.trimmingCharacters(in: .whitespaces).isEmpty, !shops.isEmpty else {
            problem = "Give the card a label and the shops it may be used on."
            return false
        }
        guard card.virtual else {
            problem = "Only a virtual or tokenized card can be saved. Make one at your card issuer, limited to the shops you use."
            return false
        }
        guard let max = Self.cents(card.maxDollars), max > 0, let auto = Self.cents(card.autoDollars),
              let month = Self.cents(card.monthDollars), month > 0 else {
            problem = "Limits are dollar amounts: the most for one purchase and for a month above $0, and the no-question amount."
            return false
        }
        guard auto <= max else {
            problem = "The no-question amount cannot be above the most for one purchase."
            return false
        }
        let zip = card.zip.trimmingCharacters(in: .whitespaces)
        guard zip.range(of: #"^\d{5}(-?\d{4})?$"#, options: .regularExpression) != nil else {
            problem = "The billing ZIP is a US ZIP code."
            return false
        }
        guard let exp = Self.expiry(card.expiry) else {
            problem = "The expiry is month and year, such as 08/29."
            return false
        }
        guard !card.name.isEmpty, !card.number.isEmpty, !card.cvv.isEmpty else {
            problem = "Type the name on the card, the number and the security code."
            return false
        }
        guard !pin.isEmpty else {
            problem = "Type your Pulse PIN to save."
            return false
        }
        let body = CardBody(
            label: card.label.trimmingCharacters(in: .whitespaces), hosts: shops, number: card.number,
            expMonth: exp.month, expYear: exp.year, cvv: card.cvv, name: card.name, zip: zip,
            maxCents: max, autoCents: auto, monthCents: month, pin: pin)
        return await put(body, saved: "Card saved.")
    }

    private func put<Body: Encodable>(_ body: Body, saved: String) async -> Bool {
        busy = true
        status = nil
        problem = nil
        defer { busy = false }
        do {
            let data = try JSONEncoder().encode(body)
            _ = try await engine.call("remote.secure.put", params: data, timeout: .seconds(30))
            status = saved
            await load()
            return true
        } catch {
            // The engine's refusals never contain a value (agent_secrets.rs); decode failures are the same.
            problem = SettingsFormat.describe(error)
            return false
        }
    }

    // MARK: The owner-only switches (the notch applies a protected change only through the engine)

    func set(_ key: String, _ value: SettingsJSON, pin: String? = nil) async {
        var body: [String: SettingsJSON] = ["key": .string(key), "value": value]
        if let pin, !pin.isEmpty { body["pin"] = .string(pin) }
        do {
            _ = try await engine.call("remote.settings.set", params: try JSON.encoder.encode(body), timeout: .seconds(15))
            pinAsk = nil
            problem = nil
        } catch let error as EngineError {
            if ["unlock_needs_auth", "pin_not_set", "pin_wrong", "pin_locked", "pin_invalid"].contains(error.code) {
                pinAsk = PinAsk(key: key, value: value, problem: pin == nil && error.code == "unlock_needs_auth" ? nil : error.message)
            } else {
                problem = error.message
            }
        } catch {
            problem = SettingsFormat.describe(error)
        }
    }

    func cancelPin() { pinAsk = nil }

    /// Writes one of the two global caps (whole cents) through the owner-only settings door; the engine
    /// asks for the PIN when the change raises a cap. False when the text is not a dollar amount.
    func setCap(_ key: String, text: String) async -> Bool {
        guard let cents = Self.cents(text), cents > 0 else {
            problem = "A cap is a dollar amount above $0, such as 25 or 100.50."
            return false
        }
        await set(key, .number(Double(cents)))
        await load()
        return pinAsk == nil && problem == nil
    }

    /// An earlier purchase with this card ended without Pulse knowing the outcome.
    func isHeld(_ item: Item) -> Bool {
        (item.held ?? 0) > 0 || (unknown[item.id] ?? 0) > 0
    }

    /// What a card's own limits come to after the global caps (the engine clamps; this only says so).
    func effectiveAuto(_ item: Item) -> Int? {
        guard let own = item.autoCents else { return nil }
        return capAuto.map { min(own, $0) } ?? own
    }

    func effectiveMonth(_ item: Item) -> Int? {
        guard let own = item.monthCents else { return nil }
        return capMonth.map { min(own, $0) } ?? own
    }
}

enum SettingsSecureBox {
    static let autoCapKey = "remoteFormFillPaymentsAutoCapCents"
    static let monthCapKey = "remoteFormFillPaymentsMonthCapCents"
    static let spotKey = "remoteScreenSpotTakeover"
    static let keys = [
        "remoteFormFillLogins", "remoteFormFillPayments", "remoteFormFillLoginManagers",
        autoCapKey, monthCapKey, spotKey,
    ]

    @MainActor
    static func rows(model: SettingsPageModel) -> [SettingsRowSpec] {
        guard keys.contains(where: { model.has($0) }) else { return [] }
        var list = SettingsRowList()
        if model.has("remoteFormFillLogins") {
            list.add(
                id: "remoteFormFillLogins", text: "\(loginsTitle) \(loginsNote)",
                SecureSwitchRow(key: "remoteFormFillLogins", title: loginsTitle, note: loginsNote, model: model))
        }
        if model.has("remoteFormFillLoginManagers") {
            list.add(
                id: "remoteFormFillLoginManagers", text: "\(managersTitle) \(managersNote)",
                SecureManagersRow(model: model))
        }
        if model.has("remoteFormFillPayments") {
            list.add(
                id: "remoteFormFillPayments", text: "\(paymentsTitle) \(paymentsNote)",
                SecureSwitchRow(key: "remoteFormFillPayments", title: paymentsTitle, note: paymentsNote, model: model))
        }
        if model.has(autoCapKey) {
            list.add(
                id: autoCapKey, text: "\(autoCapTitle) \(autoCapNote)",
                SecureCapRow(key: autoCapKey, title: autoCapTitle, note: autoCapNote, model: model))
        }
        if model.has(monthCapKey) {
            list.add(
                id: monthCapKey, text: "\(monthCapTitle) \(monthCapNote)",
                SecureCapRow(key: monthCapKey, title: monthCapTitle, note: monthCapNote, model: model))
        }
        if model.has(spotKey) {
            list.add(
                id: spotKey, text: "\(spotTitle) \(spotNote)",
                SecureSwitchRow(key: spotKey, title: spotTitle, note: spotNote, model: model))
        }
        list.add(
            id: "secureBoxItems", text: "Saved logins and cards virtual card login password secure box",
            SecureBoxItemsRow(model: model))
        return list.rows
    }

    static let loginsTitle = "Let agents sign in with logins saved in Pulse"
    static let loginsNote =
        "Lets an agent ask Pulse to type one saved field (username or password) into the page open in your browser. Pulse reads the page's real address, requires it to match the site you saved, and asks you with Touch ID or your PIN each time. The agent never sees the value. Needs Remote tools on. Off by default."
    static let managersTitle = "Password managers Pulse may start for a login"
    static let managersNote =
        "One per line as name=chord, for example 1Password=cmd+shift+\\ . Pulse sends that manager's own autofill shortcut to your browser and sees nothing of what it fills; the manager's own Touch ID runs in the browser. Pulse never reads a manager's vault. Empty by default: no manager is used."
    static let paymentsTitle = "Let agents pay with a virtual card saved in Pulse"
    static let paymentsNote =
        "Lets an agent ask Pulse to type one field of a saved virtual or tokenized US card into a verified checkout page, inside the limits you set on the card. Up to the no-question amount (never above the cap below) it pays without asking; above it you confirm each purchase with Touch ID or your PIN, every time. Pressing the pay button still asks. The amount and the shop's name come from the agent and Pulse cannot check them, so the caps are the real limit. The agent never sees the card. Needs Remote tools on. Off by default."

    static let autoCapTitle = "Largest purchase Pulse approves without asking"
    static let autoCapNote =
        "Across all cards, whatever a card's own no-question amount says. Above it, Touch ID or your PIN confirms each purchase. Default $25. Raising it asks for your PIN."
    static let monthCapTitle = "Most agents may spend in a calendar month"
    static let monthCapNote =
        "Across all cards, whatever a card's own monthly limit says. Default $100. Raising it asks for your PIN."
    static let spotTitle = "Let an agent hand you one part of the screen"
    static let spotNote =
        "Lets an agent ask you to do one step yourself (a login, a payment confirmation, a CAPTCHA). Pulse dims everything except that region, shows what the agent is about to fill and where, and has a Stop button. The agent never sees the region. Needs Remote tools on. Off by default."

    static func dollars(_ cents: Int?) -> String {
        guard let cents else { return "none" }
        return "$\(cents / 100).\(cents % 100 < 10 ? "0" : "")\(cents % 100)"
    }
}

/// The Pulse PIN field a protected switch asks for when the engine says the change loosens a gate.
private struct SecurePinPrompt: View {
    let ask: SecureBoxModel.PinAsk
    let box: SecureBoxModel
    @State private var pin = ""
    @Environment(\.hubTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: ask.problem ?? "This change makes a gate less safe, so Pulse asks for your PIN. Set a PIN under Remote access first if you have none.")
                .font(theme.type.caption.font)
            HStack(spacing: theme.spacing.sm) {
                // Secure entry is an AppKit-backed control that SwiftRight does not prove. It stays in the
                // macOS-only branch, which the controls lint skips. The other branch never shows a plain field.
                #if os(macOS)
                SecureField("Pulse PIN", text: $pin)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Pulse PIN")
                    .onSubmit { confirm() }
                #else
                Text(verbatim: "PIN entry is available on the Mac only.")
                #endif
                SettingsButton("Confirm", enabled: !pin.isEmpty) { confirm() }
                SettingsButton("Cancel", kind: .ghost) { pin = ""; box.cancelPin() }
            }
        }
    }

    private func confirm() {
        let typed = pin
        pin = ""
        guard !typed.isEmpty else { return }
        let ask = self.ask
        let box = self.box
        Task { await box.set(ask.key, ask.value, pin: typed) }
    }
}

/// An owner-only switch: written through the engine's `remote.settings.set` (the notch applies it).
struct SecureSwitchRow: View {
    let key: String
    let title: String
    let note: String
    let model: SettingsPageModel

    var body: some View {
        let box = model.secureBox
        let current = model
        let name = key
        let binding = Binding<Bool>(
            get: { MainActor.assumeIsolated { current.bool(name) } },
            set: { on in Task { await box.set(name, .bool(on)) } })
        VStack(alignment: .leading, spacing: 6) {
            SettingsRowFrame(title: title, note: note) {
                Toggle("", isOn: binding)
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .disabled(!model.bool("remoteTools"))
                    .accessibilityLabel(title)
            }
            if let ask = box.pinAsk, ask.key == key {
                SecurePinPrompt(ask: ask, box: box)
            }
        }
    }
}

/// A global payment cap, typed in dollars, saved with Save (the engine stores whole cents).
struct SecureCapRow: View {
    let key: String
    let title: String
    let note: String
    let model: SettingsPageModel
    @State private var draft: String?

    var body: some View {
        let box = model.secureBox
        let shown = model.number(key).map { SettingsSecureBox.dollars(Int($0)) } ?? ""
        VStack(alignment: .leading, spacing: 6) {
            SettingsStackedFrame(title: title, note: note) {
                HStack(spacing: 8) {
                    TextField("Dollars", text: Binding(get: { draft ?? shown }, set: { draft = $0 }))
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 140)
                        .disabled(!model.bool("remoteTools"))
                        .accessibilityLabel(title)
                        .onSubmit { save(box) }
                    SettingsButton("Save", enabled: draft != nil && draft != shown && !box.busy) { save(box) }
                }
            }
            if let ask = box.pinAsk, ask.key == key {
                SecurePinPrompt(ask: ask, box: box)
            }
        }
    }

    private func save(_ box: SecureBoxModel) {
        guard let typed = draft else { return }
        let name = key
        Task {
            if await box.setCap(name, text: typed) { draft = nil }
        }
    }
}

/// The password-manager list (`name=chord`, one per line), saved when Return is pressed or the field is left.
struct SecureManagersRow: View {
    let model: SettingsPageModel
    @State private var draft: String?
    private static let key = "remoteFormFillLoginManagers"

    var body: some View {
        let box = model.secureBox
        let published = model.settings[Self.key]
        let items = published?.stringList ?? []
        let asText = published?.stringValue != nil
        VStack(alignment: .leading, spacing: 6) {
            SettingsStackedFrame(title: SettingsSecureBox.managersTitle, note: SettingsSecureBox.managersNote) {
                // Multi-line entry is an AppKit-backed control that SwiftRight does not prove; it lives in the
                // macOS-only branch. The other branch shows the saved list read-only.
                #if os(macOS)
                TextEditor(text: Binding(get: { draft ?? items.joined(separator: "\n") }, set: { draft = $0 }))
                    .font(.body)
                    .frame(minHeight: 56, maxHeight: 120)
                    .disabled(!model.bool("remoteFormFillLogins"))
                    .accessibilityLabel(SettingsSecureBox.managersTitle)
                #else
                Text(verbatim: items.joined(separator: "\n"))
                #endif
                SettingsButton("Save managers", enabled: draft != nil && draft != items.joined(separator: "\n")) {
                    let next = (draft ?? "").split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                    draft = nil
                    Task { await box.set(Self.key, SettingsListRow.encode(next, asText: asText)) }
                }
            }
            if let ask = box.pinAsk, ask.key == Self.key {
                SecurePinPrompt(ask: ask, box: box)
            }
        }
    }
}

/// The saved items and the forms that add a login or a card.
struct SecureBoxItemsRow: View {
    let model: SettingsPageModel
    private enum Mode { case none, login, card, totp }
    @State private var mode: Mode = .none
    @State private var label = ""
    @State private var host = ""
    @State private var username = ""
    @State private var password = ""
    @State private var totp = ""
    @State private var totpTarget: SecureBoxModel.Item?
    @State private var card = SecureBoxModel.CardEntry()
    @State private var pin = ""
    @Environment(\.hubTheme) private var theme

    private func summary(_ item: SecureBoxModel.Item, spent: Int?, box: SecureBoxModel) -> String {
        if item.kind == "login" {
            return "\(item.label): login for \(item.host ?? "an unknown site")" + (item.totp == true ? " · one-time code saved" : "")
        }
        var parts = [
            "\(item.label): card ending \(item.last4 ?? "????")",
            "shops \((item.hosts ?? []).joined(separator: ", "))",
            "up to \(SettingsSecureBox.dollars(item.maxCents)) a purchase",
            "no question at or under \(SettingsSecureBox.dollars(box.effectiveAuto(item)))" + (box.effectiveAuto(item) != item.autoCents ? " (held to the cap)" : ""),
            "\(SettingsSecureBox.dollars(box.effectiveMonth(item))) a month" + (box.effectiveMonth(item) != item.monthCents ? " (held to the cap)" : ""),
        ]
        if let spent {
            let left = max(0, (box.effectiveMonth(item) ?? 0) - spent)
            parts.append("\(SettingsSecureBox.dollars(spent)) used this month, \(SettingsSecureBox.dollars(left)) left")
        }
        return parts.joined(separator: " · ")
    }

    private func clear() {
        mode = .none
        label = ""; host = ""; username = ""; password = ""; totp = ""; pin = ""
        totpTarget = nil
        card = SecureBoxModel.CardEntry()
    }

    private func field(_ name: String, _ text: Binding<String>) -> some View {
        TextField(name, text: text)
            .textFieldStyle(.roundedBorder)
            .autocorrectionDisabled()
            .accessibilityLabel(name)
    }

    private func secret(_ name: String, _ text: Binding<String>) -> AnyView {
        // Secure entry is AppKit-backed and not proven by SwiftRight: macOS-only branch, see SecurePinPrompt.
        #if os(macOS)
        return AnyView(
            SecureField(name, text: text)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .accessibilityLabel(name)
        )
        #else
        return AnyView(Text(verbatim: "Secure entry is available on the Mac only."))
        #endif
    }

    var body: some View {
        let box = model.secureBox
        SettingsStackedFrame(
            title: "Saved logins and cards",
            note: box.items.isEmpty
                ? "Nothing saved yet. Only labels, sites and limits are ever listed here, never a username, password or card number."
                : "Only labels, sites and limits are listed, never a value. Saving needs your Pulse PIN."
        ) {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(box.items) { item in
                    SettingsPathLine(
                        text: summary(item, spent: item.kind == "card" ? (item.spentCents ?? box.spent[item.id]) : nil, box: box),
                        removeLabel: "Remove \(item.kind) \(item.label)"
                    ) { Task { await box.remove(item) } }
                    if item.kind == "card", box.isHeld(item) {
                        Text(verbatim: "A purchase with this card ended without Pulse knowing whether it went through. Its amount stays counted, and Pulse asks you before it pays on that site again. Check the card's own statement.")
                            .font(.caption).foregroundStyle(.orange)
                    }
                    if item.kind == "login" {
                        SettingsButton(item.totp == true ? "Replace one-time code…" : "Add one-time code…", kind: .ghost, enabled: !box.busy) {
                            clear(); totpTarget = item; mode = .totp
                        }
                    }
                }
                HStack(spacing: theme.spacing.sm) {
                    SettingsButton("Add login…", enabled: !box.busy) { clear(); mode = .login }
                    SettingsButton("Add virtual card…", enabled: !box.busy) { clear(); mode = .card }
                }
                if mode == .login { loginForm(box) }
                if mode == .card { cardForm(box) }
                if mode == .totp, let target = totpTarget { totpForm(box, target) }
                if let problem = box.problem {
                    Text(verbatim: problem).font(.caption).foregroundStyle(.red)
                }
                if let status = box.status {
                    Text(verbatim: status).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .task { await box.load() }
    }

    @ViewBuilder
    private func loginForm(_ box: SecureBoxModel) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: "The login can be used only on this exact site.").font(.caption).foregroundStyle(.secondary)
            field("Label", $label)
            field("Site, for example example.com", $host)
            field("Username", $username)
            secret("Password", $password)
            secret("One-time code secret (optional): setup key or otpauth:// address", $totp)
            secret("Pulse PIN", $pin)
            HStack(spacing: theme.spacing.sm) {
                SettingsButton("Save login", enabled: !box.busy) {
                    let (l, h, u, p, t, n) = (label, host, username, password, totp, pin)
                    // Cleared at once: the secrets are in the call only.
                    password = ""; username = ""; totp = ""; pin = ""
                    Task { if await box.saveLogin(label: l, host: h, username: u, password: p, totp: t, pin: n) { clear() } }
                }
                SettingsButton("Cancel", kind: .ghost) { clear() }
            }
        }
    }

    @ViewBuilder
    private func totpForm(_ box: SecureBoxModel, _ target: SecureBoxModel.Item) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: "One-time code for \(target.label): type the setup key or the otpauth:// address. The saved username and password are kept.")
                .font(.caption).foregroundStyle(.secondary)
            secret("One-time code secret", $totp)
            secret("Pulse PIN", $pin)
            HStack(spacing: theme.spacing.sm) {
                SettingsButton("Save one-time code", enabled: !box.busy) {
                    let (id, t, n) = (target.id, totp, pin)
                    totp = ""; pin = ""
                    Task { if await box.setTotp(id: id, totp: t, pin: n) { clear() } }
                }
                SettingsButton("Cancel", kind: .ghost) { clear() }
            }
        }
    }

    private func capsLine(_ box: SecureBoxModel) -> String {
        guard let auto = box.capAuto, let month = box.capMonth else {
            return "Pulse also holds every card to two global caps, set above."
        }
        return "Global caps: \(SettingsSecureBox.dollars(auto)) a purchase without asking and \(SettingsSecureBox.dollars(month)) a month across all cards. A higher limit here is held to them."
    }

    /// Says when what is typed will be held to a global cap.
    private func limitNote(_ box: SecureBoxModel) -> String? {
        var notes: [String] = []
        if let cap = box.capAuto, let auto = SecureBoxModel.cents(card.autoDollars), auto > cap {
            notes.append("The no-question amount will be held to \(SettingsSecureBox.dollars(cap)).")
        }
        if let cap = box.capMonth, let month = SecureBoxModel.cents(card.monthDollars), month > cap {
            notes.append("The month limit will be held to \(SettingsSecureBox.dollars(cap)).")
        }
        return notes.isEmpty ? nil : notes.joined(separator: " ")
    }

    @ViewBuilder
    private func cardForm(_ box: SecureBoxModel) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: "US virtual or tokenized cards only. Make one at your card issuer, limited to the shops you use.")
                .font(.caption).foregroundStyle(.secondary)
            Text(verbatim: capsLine(box))
                .font(.caption).foregroundStyle(.secondary)
            field("Label", $card.label)
            field("Shops, separated by commas", $card.shops)
            field("Billing ZIP", $card.zip)
            field("Most for one purchase, dollars", $card.maxDollars)
            field("No question at or under, dollars", $card.autoDollars)
            field("Most for a month, dollars", $card.monthDollars)
            Toggle("This is a virtual or tokenized card from my issuer, limited to these shops", isOn: $card.virtual)
                .accessibilityLabel("This is a virtual or tokenized card")
            if !card.virtual {
                Text(verbatim: "Pulse refuses a card that is not virtual or tokenized: a real card number is never saved. Make a virtual card at your issuer first.")
                    .font(.caption).foregroundStyle(.red)
            }
            if let note = limitNote(box) {
                Text(verbatim: note).font(.caption).foregroundStyle(.orange)
            }
            secret("Name on the card", $card.name)
            secret("Card number", $card.number)
            field("Expiry, MM/YY", $card.expiry)
            secret("Security code", $card.cvv)
            secret("Pulse PIN", $pin)
            HStack(spacing: theme.spacing.sm) {
                SettingsButton("Save card", enabled: !box.busy) {
                    let sent = card
                    let typedPin = pin
                    card.name = ""; card.number = ""; card.cvv = ""; pin = ""
                    Task { if await box.saveCard(sent, pin: typedPin) { clear() } }
                }
                SettingsButton("Cancel", kind: .ghost) { clear() }
            }
        }
    }
}
#endif
