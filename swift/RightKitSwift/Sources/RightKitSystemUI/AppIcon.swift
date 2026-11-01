import Foundation
import RightKitHub
import SwiftUI

/// One app's icon: the PNG bytes the engine returns as a `data:image/png;base64,...` URL.
///
/// The bytes are decoded by the platform services (`HubImageView`), so this file has no ImageIO,
/// CoreGraphics or AppKit and the same source compiles on every platform.
@MainActor
final class AppIcon {
    /// The icon's size on screen, in points.
    static let points: CGFloat = 28
    /// The longest side the engine's PNG is decoded to: twice the size on screen, for a 2x display.
    static let maxPixel = 64

    let data: Data

    private init(data: Data) {
        self.data = data
    }

    /// The icon for a data URL, or nil when it carries no bytes. Only unwraps the URL: whether the
    /// bytes are an image the platform can draw is decided when the icon is shown, and an icon that
    /// cannot be drawn shows the neutral glyph.
    static func decode(dataURL: String) -> AppIcon? {
        guard let comma = dataURL.firstIndex(of: ",") else { return nil }
        let encoded = String(dataURL[dataURL.index(after: comma)...])
        guard let data = Data(base64Encoded: encoded), !data.isEmpty else { return nil }
        return AppIcon(data: data)
    }

    /// Decodes the icon ahead of drawing it, so the picture or row that shows it does not wait.
    func preload(services: any HubPlatformServices) async {
        _ = await HubImages.preload(data, maxPixel: Self.maxPixel, services: services)
    }
}

/// An app's icon, or a neutral glyph while it has none (never arrived, or the app has no usable icon).
struct AppIconView: View {
    private let icon: AppIcon?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ icon: AppIcon?) {
        self.icon = icon
    }

    var body: some View {
        let palette = theme.palette(scheme)
        if let icon {
            HubImageView(
                data: icon.data,
                maxPixel: AppIcon.maxPixel,
                fitting: CGSize(width: AppIcon.points, height: AppIcon.points)
            ) {
                fallback(palette)
            }
            .frame(width: AppIcon.points, height: AppIcon.points)
        } else {
            fallback(palette)
        }
    }

    private func fallback(_ palette: HubPalette) -> some View {
        HubLayered {
            RoundedRectangle(cornerRadius: 6).fill(palette.plane.color)
            Image(systemName: "app.dashed")
                .foregroundStyle(palette.ink3.color)
                .frame(width: AppIcon.points, height: AppIcon.points)
        }
        .frame(width: AppIcon.points, height: AppIcon.points)
    }
}
