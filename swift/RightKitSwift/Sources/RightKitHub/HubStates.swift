import SwiftUI

/// Shared layout for the empty, loading and error views: an icon, a title, a message and an optional button.
struct HubStateLayout: View {
    let symbol: String
    let title: String
    let message: String
    let tint: HubTone
    let actionTitle: String?
    let action: (@MainActor () -> Void)?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(spacing: theme.spacing.sm) {
            Image(systemName: symbol)
                .foregroundStyle(tint == .neutral ? palette.ink3.color : tint.color(in: palette).color)
                .font(Font.system(size: 28, weight: .regular))
            Text(verbatim: title)
                .font(theme.type.title.font)
                .foregroundStyle(palette.ink.color)
            if !message.isEmpty {
                Text(verbatim: message)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink2.color)
                    .frame(maxWidth: 360)
            }
            if let actionTitle, let action {
                Button(actionTitle) { action() }
            }
        }
        .padding(theme.spacing.xxl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Nothing to show yet, for example an empty list or no search match.
public struct HubEmptyState: View {
    private let symbol: String
    private let title: String
    private let message: String
    private let actionTitle: String?
    private let action: (@MainActor () -> Void)?

    public init(
        symbol: String = "tray",
        title: String,
        message: String = "",
        actionTitle: String? = nil,
        action: (@MainActor () -> Void)? = nil
    ) {
        self.symbol = symbol
        self.title = title
        self.message = message
        self.actionTitle = actionTitle
        self.action = action
    }

    public var body: some View {
        HubStateLayout(
            symbol: symbol, title: title, message: message, tint: .neutral,
            actionTitle: actionTitle, action: action
        )
    }
}

/// Waiting for something. There is no spinner (ProgressView is not covered by SwiftRight); the
/// message says what is being waited for.
public struct HubLoadingState: View {
    private let message: String

    public init(message: String = "Loading") {
        self.message = message
    }

    public var body: some View {
        HubStateLayout(
            symbol: "hourglass", title: message, message: "", tint: .neutral,
            actionTitle: nil, action: nil
        )
    }
}

/// Something failed. Give the user the reason and, when it can help, a retry.
public struct HubErrorState: View {
    private let title: String
    private let message: String
    private let retryTitle: String
    private let retry: (@MainActor () -> Void)?

    public init(
        title: String = "Something went wrong",
        message: String = "",
        retryTitle: String = "Try again",
        retry: (@MainActor () -> Void)? = nil
    ) {
        self.title = title
        self.message = message
        self.retryTitle = retryTitle
        self.retry = retry
    }

    /// An error state from an engine or platform error.
    public init(error: any Error, retry: (@MainActor () -> Void)? = nil) {
        if let engine = error as? EngineError {
            self.init(title: engine.message, message: engine.code, retry: retry)
        } else {
            self.init(title: "Something went wrong", message: String(describing: error), retry: retry)
        }
    }

    public var body: some View {
        HubStateLayout(
            symbol: "exclamationmark.triangle", title: title, message: message, tint: .bad,
            actionTitle: retry == nil ? nil : retryTitle, action: retry
        )
    }
}

/// A one-line notice above a section's content, for a condition that does not stop the section.
public struct HubBanner: View {
    private let message: String
    private let tone: HubTone
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(_ message: String, tone: HubTone = .warn) {
        self.message = message
        self.tone = tone
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private var fill: HubColor {
        switch tone {
        case .ok: return palette.okSoft
        case .warn: return palette.warnSoft
        case .bad: return palette.badSoft
        case .accent: return palette.accentSoft
        case .neutral: return palette.fill2
        }
    }

    public var body: some View {
        HubLayered {
            Rectangle().fill(fill.color)
            HStack {
                Text(verbatim: message)
                    .font(theme.type.caption.font)
                    .foregroundStyle(tone.color(in: palette).color)
                Spacer()
            }
            .padding(.horizontal, theme.spacing.xl)
            .padding(.vertical, theme.spacing.sm)
        }
    }
}
