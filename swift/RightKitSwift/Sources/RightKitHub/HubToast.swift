import Observation
import SwiftUI

/// How a toast looks.
public enum HubToastKind: String, Sendable, Equatable {
    case info
    case success
    case warning
    case error
}

/// One transient message.
public struct HubToast: Sendable, Equatable, Identifiable {
    public let id: UUID
    public var kind: HubToastKind
    public var message: String
    /// A second line, for example the engine error code.
    public var detail: String?
    /// How long it stays before it dismisses itself. Nil keeps it until dismissed.
    public var duration: Duration?

    public init(
        id: UUID = UUID(),
        kind: HubToastKind = .info,
        message: String,
        detail: String? = nil,
        duration: Duration? = .seconds(4)
    ) {
        self.id = id
        self.kind = kind
        self.message = message
        self.detail = detail
        self.duration = duration
    }
}

/// Where sections post toasts. One sink lives in the `HubContext`; `HubShell` draws it.
@MainActor
@Observable
public final class ToastSink {
    /// The toasts on screen, oldest first.
    public private(set) var toasts: [HubToast] = []

    /// The most toasts shown at once; older ones drop.
    public let limit: Int

    public init(limit: Int = 4) {
        self.limit = max(1, limit)
    }

    /// Shows a toast and schedules its dismissal.
    public func post(_ toast: HubToast) {
        toasts.append(toast)
        if toasts.count > limit {
            toasts.removeFirst(toasts.count - limit)
        }
        if let duration = toast.duration {
            let id = toast.id
            Task { [weak self] in
                try? await Task.sleep(for: duration)
                self?.dismiss(id)
            }
        }
    }

    /// Shows a message.
    public func show(_ message: String, kind: HubToastKind = .info, detail: String? = nil) {
        post(HubToast(kind: kind, message: message, detail: detail))
    }

    /// Shows an engine or platform failure.
    public func show(error: any Error) {
        if let engine = error as? EngineError {
            post(HubToast(kind: .error, message: engine.message, detail: engine.code))
        } else {
            post(HubToast(kind: .error, message: String(describing: error)))
        }
    }

    /// Removes one toast.
    public func dismiss(_ id: UUID) {
        toasts.removeAll { $0.id == id }
    }
}

/// One toast row.
public struct HubToastView: View {
    private let toast: HubToast
    private let onDismiss: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(_ toast: HubToast, onDismiss: @escaping @MainActor () -> Void) {
        self.toast = toast
        self.onDismiss = onDismiss
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private var symbol: String {
        switch toast.kind {
        case .info: return "info.circle.fill"
        case .success: return "checkmark.circle.fill"
        case .warning: return "exclamationmark.triangle.fill"
        case .error: return "xmark.octagon.fill"
        }
    }

    private var tint: HubColor {
        switch toast.kind {
        case .info: return palette.accent
        case .success: return palette.ok
        case .warning: return palette.warn
        case .error: return palette.bad
        }
    }

    public var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.elevated.color)
            RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.lineStrong.color, lineWidth: 1)
            HStack(alignment: .top, spacing: theme.spacing.sm) {
                Image(systemName: symbol)
                    .foregroundStyle(tint.color)
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: toast.message)
                        .font(theme.type.body.font)
                        .foregroundStyle(palette.ink.color)
                    if let detail = toast.detail {
                        Text(verbatim: detail)
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                    }
                }
                Spacer()
                Button {
                    onDismiss()
                } label: {
                    Image(systemName: "xmark")
                        .foregroundStyle(palette.ink3.color)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss")
            }
            .padding(theme.spacing.md)
            .frame(maxWidth: 360, alignment: .leading)
        }
        .frame(maxWidth: 360)
    }
}

/// Draws a `ToastSink`: a ZStack aligned to the bottom trailing corner. Place it as an overlay on the
/// whole hub. Its empty area does not take clicks.
public struct HubToastLayer: View {
    private let sink: ToastSink
    @Environment(\.hubTheme) private var theme

    public init(sink: ToastSink) {
        self.sink = sink
    }

    public var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Color.clear
            VStack(alignment: .trailing, spacing: theme.spacing.sm) {
                ForEach(sink.toasts) { toast in
                    HubToastView(toast) { sink.dismiss(toast.id) }
                        .transition(.opacity)
                }
            }
            .padding(theme.spacing.lg)
            .animation(.easeInOut(duration: theme.motion.standard), value: sink.toasts)
        }
    }
}
