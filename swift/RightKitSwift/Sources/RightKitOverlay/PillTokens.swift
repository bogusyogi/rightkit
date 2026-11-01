#if os(macOS)
import CoreText
import AppKit
import Foundation
import SwiftUI

struct ActiveHoverRegion: NSViewRepresentable {
    let tooltip: String
    let onChange: (Bool) -> Void

    func makeNSView(context: Context) -> ActiveHoverView {
        ActiveHoverView(onChange: onChange, tooltip: tooltip)
    }

    func updateNSView(_ view: ActiveHoverView, context: Context) {
        view.onChange = onChange
        view.toolTip = tooltip
        view.reconcilePointer()
    }
}

final class ActiveHoverView: NSView {
    var onChange: (Bool) -> Void
    private var tracker: NSTrackingArea?

    init(onChange: @escaping (Bool) -> Void, tooltip: String) {
        self.onChange = onChange
        super.init(frame: .zero)
        self.toolTip = tooltip
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracker { removeTrackingArea(tracker) }
        let tracker = NSTrackingArea(
            rect: bounds,
            options: [.activeAlways, .mouseEnteredAndExited, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(tracker)
        self.tracker = tracker
        DispatchQueue.main.async { [weak self] in self?.reconcilePointer() }
    }

    override func mouseEntered(with event: NSEvent) { onChange(true) }
    override func mouseExited(with event: NSEvent) { onChange(false) }

    func reconcilePointer() {
        guard let window else { return }
        let inWindow = window.convertPoint(fromScreen: NSEvent.mouseLocation)
        onChange(bounds.contains(convert(inWindow, from: nil)))
    }
}

enum HRTokens {
    static let accent = Color(red: 90 / 255, green: 160 / 255, blue: 1)
    static let accentText = Color(red: 148 / 255, green: 190 / 255, blue: 1)
    static let ember = Color(hex: 0xFF5630)
    static let coffee = Color(red: 8 / 255, green: 8 / 255, blue: 8 / 255)
    static let raised = Color(red: 18 / 255, green: 18 / 255, blue: 18 / 255)
    static let foreground = Color(red: 245 / 255, green: 242 / 255, blue: 238 / 255)
    static let muted = Color(red: 157 / 255, green: 151 / 255, blue: 145 / 255)
    static let error = Color(red: 229 / 255, green: 103 / 255, blue: 91 / 255)
    static let success = Color(red: 63 / 255, green: 208 / 255, blue: 106 / 255)

    private static var fontsRegistered = false

    static func registerFonts() {
        guard !fontsRegistered else { return }
        fontsRegistered = true
        let names = [
            "HankenGrotesk-400.ttf", "HankenGrotesk-500.ttf",
            "HankenGrotesk-600.ttf", "HankenGrotesk-700.ttf",
            "SplineSansMono-400.ttf", "SplineSansMono-500.ttf", "Tanker-400.ttf"
        ]
        let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
        let roots = [
            Bundle.main.resourceURL?.appendingPathComponent("fonts", isDirectory: true),
            cwd.appendingPathComponent("src-tauri/fonts", isDirectory: true),
            cwd.appendingPathComponent("../../src-tauri/fonts", isDirectory: true)
        ].compactMap { $0 }
        for name in names {
            guard let url = roots.lazy.map({ $0.appendingPathComponent(name) }).first(where: { FileManager.default.fileExists(atPath: $0.path) }) else { continue }
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
    }

    static func body(_ size: CGFloat, _ weight: Font.Weight = .medium) -> Font {
        let face: String
        if weight == .bold {
            face = "HankenGrotesk-Bold"
        } else if weight == .semibold {
            face = "HankenGrotesk-SemiBold"
        } else if weight == .regular {
            face = "HankenGrotesk-Regular"
        } else {
            face = "HankenGrotesk-Medium"
        }
        return .custom(face, size: size)
    }

    static func mono(_ size: CGFloat) -> Font {
        .custom("SplineSansMono-Regular", size: size)
    }

    static func display(_ size: CGFloat) -> Font {
        // The bundled Tanker file's embedded PostScript name is literally
        // "false"; CoreText must use that name even though the asset filename
        // is Tanker-400.ttf.
        .custom("false", size: size)
    }
}

enum HRAuxiliaryPalette {
    static let surface: Color = .black
    static let raised = Color(hex: 0x171717)
    static let foreground = Color(hex: 0xF5F2EE)
    static let muted = Color(hex: 0x999999)
    static let line = Color(hex: 0x333333)
    static let accent: Color = .white
    static let hover = Color.white.opacity(0.14)
}

extension Color {
    init(hex: UInt32, opacity: Double = 1) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xff) / 255,
            green: Double((hex >> 8) & 0xff) / 255,
            blue: Double(hex & 0xff) / 255,
            opacity: opacity
        )
    }
}
#endif
