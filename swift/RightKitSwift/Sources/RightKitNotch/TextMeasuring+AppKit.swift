#if os(macOS)
import RightKitNotchCore
import AppKit
import SwiftUI

/// Mac measurer: the same NSFont calls the layout made inline before.
struct AppKitTextMeasurer: TextMeasuring {
    private func nsWeight(_ weight: Font.Weight) -> NSFont.Weight {
        switch weight {
        case .ultraLight: return .ultraLight
        case .thin: return .thin
        case .light: return .light
        case .medium: return .medium
        case .semibold: return .semibold
        case .bold: return .bold
        case .heavy: return .heavy
        case .black: return .black
        default: return .regular
        }
    }

    private func font(_ size: CGFloat, _ weight: Font.Weight, monospacedDigits: Bool = false) -> NSFont {
        monospacedDigits
            ? NSFont.monospacedDigitSystemFont(ofSize: size, weight: nsWeight(weight))
            : NSFont.systemFont(ofSize: size, weight: nsWeight(weight))
    }

    func width(of text: String, size: CGFloat, weight: Font.Weight, monospacedDigits: Bool) -> CGFloat {
        (text as NSString).size(withAttributes: [.font: font(size, weight, monospacedDigits: monospacedDigits)]).width
    }

    func lineHeight(size: CGFloat, weight: Font.Weight) -> CGFloat {
        let f = font(size, weight)
        return ceil(f.ascender - f.descender + f.leading)
    }

    func wrappedHeight(of text: String, size: CGFloat, weight: Font.Weight, width: CGFloat) -> CGFloat {
        (text as NSString).boundingRect(
            with: CGSize(width: width, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading],
            attributes: [.font: font(size, weight)]
        ).height
    }
}
#endif
