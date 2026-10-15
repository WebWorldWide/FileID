import SwiftUI

// MARK: - Theme (Prototype v3)
// Square corners, 1 px hairlines, mono section labels, serif page titles.
// Gold is the only action/selection accent; lavender means a model is
// running, cyan is information, pink is "done".

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

    static let bg0 = Color(hex: 0x070708)
    static let bg1 = Color(hex: 0x0B0B0C)
    static let text1 = Color(hex: 0xF0F1F2)
    static let text2 = Color(hex: 0xCFD3D8)
    static let text3 = Color(hex: 0xA6ACB3)
    static let label = Color(hex: 0xAEB4BB)
    static let textOnPanel = Color(hex: 0xE2E5E8)

    static let panel          = Color(red: 24/255, green: 24/255, blue: 27/255).opacity(0.78)
    static let panelHeader    = Color.white.opacity(0.02)
    static let pane           = Color.white.opacity(0.015)
    static let well           = Color.black.opacity(0.3)
    static let hairline       = Color.white.opacity(0.08)
    static let hairlineSoft   = Color.white.opacity(0.06)
    static let hairlineStrong = Color.white.opacity(0.14)
    static let controlBorder  = Color.white.opacity(0.18)
    static let hover          = Color.white.opacity(0.05)
    static let goldWash       = gold.opacity(0.07)

    static let surfaceBase     = Color.black.opacity(0.3)
    static let surfaceCard     = panel
    static let surfaceBorder   = hairline

    enum Space {
        static let xs: CGFloat = 4
        static let s:  CGFloat = 8
        static let m:  CGFloat = 16
        static let l:  CGFloat = 24
        static let xl: CGFloat = 40
    }

    /// Prototype v3 is square everywhere; kept so call sites compile.
    enum Radius {
        static let s:  CGFloat = 0
        static let m:  CGFloat = 0
        static let l:  CGFloat = 0
    }

    enum Metrics {
        static let titleBar: CGFloat = 48
        static let sidebar: CGFloat = 232
        static let navRow: CGFloat = 50
        static let control: CGFloat = 28
        static let controlSmall: CGFloat = 24
        static let controlLarge: CGFloat = 30
        static let pageInsetX: CGFloat = 26
        static let pageInsetY: CGFloat = 20
        static let sidePane: CGFloat = 300
    }

    enum Font {
        static func display(_ size: CGFloat) -> SwiftUI.Font { .system(size: size, weight: .regular, design: .serif) }
        static func mono(_ size: CGFloat, _ weight: SwiftUI.Font.Weight = .regular) -> SwiftUI.Font { .system(size: size, weight: weight, design: .monospaced) }
        static func body(_ size: CGFloat = 14, _ weight: SwiftUI.Font.Weight = .regular) -> SwiftUI.Font { .system(size: size, weight: weight) }
        static let sectionLabel = mono(11, .semibold)
        static let meta = mono(11)
        static let metaStrong = mono(11.5, .semibold)
        static let control = mono(11.5)
        static let pageTitle = display(34)
    }

    static let spring = Animation.spring(response: 0.35, dampingFraction: 0.8)
}

extension Color {
    init(hex: UInt32, opacity: Double = 1) {
        self.init(red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255, opacity: opacity)
    }
}

// MARK: - GlassCard → square hairline panel

struct GlassCard<Content: View>: View {
    @ViewBuilder var content: Content
    var padding: CGFloat = Theme.Space.m

    var body: some View {
        content
            .padding(padding)
            .background(Theme.panel)
            .overlay(Rectangle().stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
    }
}

// MARK: - BadgePill → square mono chip

struct BadgePill: View {
    let label:  String
    var color:  Color = Theme.gold

    var body: some View {
        Text(label)
            .font(Theme.Font.mono(10.5, .semibold))
            .tracking(0.8)
            .textCase(.uppercase)
            .foregroundStyle(color)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .overlay(Rectangle().stroke(color.opacity(0.5), lineWidth: 1))
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

// MARK: - GoldButton

struct GoldButton: View {
    let title:  String
    let icon:   String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: icon)
        }
        .buttonStyle(DSPrimaryButtonStyle())
    }
}

// MARK: - ThemedSegmentedControl → square segmented bar

struct ThemedSegmentedControl: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding var selection: String
    let options: [(tag: String, label: String)]

    var body: some View {
        HStack(spacing: 0) {
            ForEach(options, id: \.tag) { opt in
                let selected = selection == opt.tag
                Button {
                    selection = opt.tag
                } label: {
                    Text(opt.label)
                        .font(Theme.Font.mono(11.5, selected ? .semibold : .medium))
                        .foregroundStyle(selected ? Theme.onGold : Theme.text2)
                        .lineLimit(1).fixedSize()
                        .padding(.horizontal, 12)
                        .frame(height: Theme.Metrics.control)
                        .background(selected ? Theme.gold : Color.clear)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text(opt.label))
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .animation(reduceMotion ? nil : Theme.spring, value: selection)
        .overlay(Rectangle().stroke(Theme.hairlineStrong, lineWidth: 1))
    }
}
