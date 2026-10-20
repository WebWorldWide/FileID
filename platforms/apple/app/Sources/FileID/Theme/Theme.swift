import SwiftUI
import AppKit
import CoreText

// MARK: - Theme (Prototype v3)
// Rounded continuous corners, 1 px hairlines, Archivo text, serif page titles.
// Gold is the only action/selection accent; lavender means a model is
// running, cyan is information, pink is "done". Text and hairline tokens
// strengthen automatically under Increase Contrast.

enum Theme {
    static let gold    = Color(red: 1.0, green: 0.8, blue: 0.0)          // #FFCC00
    static let goldDim = Color(red: 0.8, green: 0.64, blue: 0.0)
    static let goldHover = Color(hex: 0xFFD84D)
    static let onGold  = Color(hex: 0x141414)

    static let ai      = Color(hex: 0xB19BCE)
    static let info    = Color(hex: 0xA0E2EA)
    static let delight = Color(hex: 0xF2A6C0)
    static let danger  = Color(hex: 0xFF6A5E)
    static let warning = Color(hex: 0xE0A340)
    static let paused  = Color(red: 185/255, green: 190/255, blue: 196/255).opacity(0.5)

    static let bg1 = Color(hex: 0x0B0B0C)
    static let text1 = Color(hex: 0xF0F1F2)
    static let text2 = adaptive(normal: NSColor(hex: 0xCFD3D8), increased: NSColor(hex: 0xEBEDEF))
    static let text3 = adaptive(normal: NSColor(hex: 0xA6ACB3), increased: NSColor(hex: 0xD6D9DD))
    static let label = Color(hex: 0xAEB4BB)
    static let textOnPanel = Color(hex: 0xE2E5E8)

    static let panel          = Color(red: 24/255, green: 24/255, blue: 27/255).opacity(0.78)
    static let panelHeader    = Color.white.opacity(0.02)
    static let pane           = Color.white.opacity(0.015)
    static let well           = Color.black.opacity(0.3)
    static let hairline       = adaptive(normal: NSColor.white.withAlphaComponent(0.08), increased: NSColor.white.withAlphaComponent(0.34))
    static let hairlineSoft   = adaptive(normal: NSColor.white.withAlphaComponent(0.06), increased: NSColor.white.withAlphaComponent(0.26))
    static let hairlineStrong = adaptive(normal: NSColor.white.withAlphaComponent(0.14), increased: NSColor.white.withAlphaComponent(0.5))

    /// Resolves per appearance, so it follows System Settings ▸ Accessibility ▸ Increase Contrast live.
    static func adaptive(normal: NSColor, increased: NSColor) -> Color {
        Color(nsColor: nsAdaptive(normal: normal, increased: increased))
    }
    static func nsAdaptive(normal: NSColor, increased: NSColor) -> NSColor {
        NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.accessibilityHighContrastDarkAqua, .accessibilityHighContrastAqua, .darkAqua, .aqua])
                .map { $0.rawValue.contains("HighContrast") } == true ? increased : normal
        }
    }
    static let nsText2 = nsAdaptive(normal: NSColor(hex: 0xCFD3D8), increased: NSColor(hex: 0xEBEDEF))
    static let nsText3 = nsAdaptive(normal: NSColor(hex: 0xA6ACB3), increased: NSColor(hex: 0xD6D9DD))
    static let nsHairline = nsAdaptive(normal: NSColor.white.withAlphaComponent(0.08), increased: NSColor.white.withAlphaComponent(0.34))
    static let nsHairlineHover = nsAdaptive(normal: NSColor.white.withAlphaComponent(0.28), increased: NSColor.white.withAlphaComponent(0.6))
    static let controlBorderHover = adaptive(normal: NSColor.white.withAlphaComponent(0.24), increased: NSColor.white.withAlphaComponent(0.6))
    static let focusRing      = gold.opacity(0.9)
    static let hover          = Color.white.opacity(0.05)
    static let goldWash       = gold.opacity(0.07)

    /// Subtle corners (cosmic-studio): 6 controls, 8 cards and rows, 10 panels.
    enum Radius {
        static let xs: CGFloat = 4
        static let s:  CGFloat = 6
        static let m:  CGFloat = 8
        static let l:  CGFloat = 10
    }

    enum Metrics {
        static let titleBar: CGFloat = 48
        static let sidebar: CGFloat = 232
        static let navRow: CGFloat = 50
        static let control: CGFloat = 28
        static let controlSmall: CGFloat = 24
        static let controlLarge: CGFloat = 30
        /// Smallest click/tap target on any control (HIG, macOS).
        static let hitTarget: CGFloat = 28
        static let pageInsetX: CGFloat = 26
        static let pageInsetY: CGFloat = 20
        static let sidePane: CGFloat = 300
    }

    enum Font {
        /// Instrument Serif and Archivo ship in Resources/Fonts (SIL OFL) and are
        /// registered at launch by `BrandFonts`; SwiftUI falls back to the system
        /// faces if registration fails.
        static func display(_ size: CGFloat) -> SwiftUI.Font { BrandFonts.registered ? .custom("Instrument Serif", size: size) : .system(size: size, weight: .regular, design: .serif) }
        /// Numbers, shortcuts and paths only; labels and prose use Archivo.
        static func mono(_ size: CGFloat, _ weight: SwiftUI.Font.Weight = .regular) -> SwiftUI.Font { .system(size: size, weight: weight, design: .monospaced) }
        static func body(_ size: CGFloat = 14, _ weight: SwiftUI.Font.Weight = .regular) -> SwiftUI.Font { BrandFonts.registered ? .custom("Archivo", size: size).weight(weight) : .system(size: size, weight: weight) }
        static var sectionLabel: SwiftUI.Font { body(11, .semibold) }
        static var meta: SwiftUI.Font { body(12) }
        static var pageTitle: SwiftUI.Font { display(34) }
    }

    /// Tight spring (0.35 / 0.78) for control state: segments, toggles,
    /// chips, selection. Nil under Reduce Motion, so changes land instantly.
    @MainActor static var spring: Animation? { reduceMotion ? nil : .spring(response: 0.35, dampingFraction: 0.78) }
    /// Standard spring (0.40 / 0.80) for layout: panes, panels, rows
    /// appearing. Under Reduce Motion it becomes a short cross-fade.
    @MainActor static var springStandard: Animation { reduceMotion ? .easeOut(duration: 0.15) : .spring(response: 0.4, dampingFraction: 0.8) }
    static let quick = Animation.easeOut(duration: 0.14)
    @MainActor static var reduceMotion: Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }
}

/// Registers the bundled brand fonts once per process. The resource bundle
/// sits beside the executable in assembled apps and under the build products
/// in `swift build` runs, so both locations are tried before `Bundle.module`.
enum BrandFonts {
    nonisolated(unsafe) private(set) static var registered = false

    static func register() {
        guard !registered else { return }
        let bundleName = "FileID_FileID.bundle"
        let executableDirectory = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
        let candidates = [Bundle.main.resourceURL?.appendingPathComponent(bundleName),
                          Bundle.main.bundleURL.appendingPathComponent(bundleName),
                          executableDirectory.appendingPathComponent(bundleName)]
        let bundle = candidates.compactMap { $0.flatMap(Bundle.init(url:)) }.first ?? Bundle.module
        guard let urls = bundle.urls(forResourcesWithExtension: "ttf", subdirectory: "Fonts"), !urls.isEmpty else { return }
        var ok = false
        for url in urls {
            if CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil) { ok = true }
            else if let font = CGFont(CGDataProvider(url: url as CFURL)!), CTFontManagerRegisterGraphicsFont(font, nil) { ok = true }
        }
        registered = ok
    }
}

extension NSColor {
    convenience init(hex: UInt32) {
        self.init(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                  blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}

extension Color {
    init(hex: UInt32, opacity: Double = 1) {
        self.init(red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255, opacity: opacity)
    }
}

// MARK: - BadgePill → square mono chip

struct BadgePill: View {
    let label:  String
    var color:  Color = Theme.gold
    /// Over photos and video frames the tint wash alone disappears on bright
    /// skies, so the pill sits on a dark backing like the Library kind badge.
    var onImage = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.xs, style: .continuous)
        Text(label)
            .font(Theme.Font.body(11, .semibold))
            .foregroundStyle(color)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.12), in: shape)
            .background(onImage ? Color.black.opacity(0.62) : Color.clear, in: shape)
            .overlay { if onImage { shape.stroke(color.opacity(0.6), lineWidth: 1) } }
    }
}

// MARK: - SettingToggleRow

struct SettingToggleRow: View {
    let title: String
    let subtitle: String?
    @Binding var isOn: Bool

    init(_ title: String, subtitle: String? = nil, isOn: Binding<Bool>) {
        self.title = title
        self.subtitle = subtitle
        self._isOn = isOn
    }

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(Theme.Font.body(13.5))
                    .fixedSize(horizontal: false, vertical: true)
                if let subtitle {
                    Text(subtitle)
                        .font(Theme.Font.meta)
                        .foregroundStyle(Theme.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .accessibilityHidden(true)
            Spacer(minLength: 12)
            Toggle(title, isOn: $isOn)
                .toggleStyle(DSToggleStyle())
                .labelsHidden()
                .accessibilityHint(Text(subtitle ?? ""))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
