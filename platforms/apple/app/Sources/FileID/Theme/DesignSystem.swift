import SwiftUI

// FileID design primitives (Prototype v3, softened after cosmic-studio):
// frosted panels with subtle corners, Archivo for every label, Instrument
// Serif for titles, monospace only for numbers and shortcuts, one gold accent
// for selection and primary actions, and quick well-damped motion.

private func rounded(_ radius: CGFloat) -> RoundedRectangle {
    RoundedRectangle(cornerRadius: radius, style: .continuous)
}

// MARK: - Labels

struct SectionLabel: View {
    let text: String
    var color: Color = Theme.label
    init(_ text: String, color: Color = Theme.label) { self.text = text; self.color = color }
    var body: some View {
        Text(text)
            .font(Theme.Font.sectionLabel)
            .tracking(0.6)
            .textCase(.uppercase)
            .foregroundStyle(color)
            .lineLimit(1)
    }
}

struct MetaText: View {
    let text: String
    var color: Color = Theme.text3
    var size: CGFloat = 12
    init(_ text: String, color: Color = Theme.text3, size: CGFloat = 12) { self.text = text; self.color = color; self.size = max(size, 11.5) }
    var body: some View {
        Text(text).font(Theme.Font.body(size)).foregroundStyle(color)
    }
}

// MARK: - Panels

struct Panel<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: 0) { content }
            .background(Theme.panel, in: rounded(Theme.Radius.l))
            .clipShape(rounded(Theme.Radius.l))
            .overlay(rounded(Theme.Radius.l).stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
    }
}

struct PanelHeader<Trailing: View>: View {
    let title: String
    var trailing: Trailing
    init(_ title: String, @ViewBuilder trailing: () -> Trailing) { self.title = title; self.trailing = trailing() }
    var body: some View {
        HStack(spacing: 12) {
            SectionLabel(title)
            Spacer(minLength: 8)
            trailing
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

extension PanelHeader where Trailing == EmptyView {
    init(_ title: String) { self.init(title) { EmptyView() } }
}

struct PanelFooter<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        HStack(spacing: 8) { content }
            .padding(.horizontal, 14).padding(.vertical, 8)
            .background(Theme.panelHeader, ignoresSafeAreaEdges: [])
            .overlay(alignment: .top) { Hairline() }
    }
}

struct Hairline: View {
    var color: Color = Theme.hairline
    var body: some View { Rectangle().fill(color).frame(height: 1) }
}

struct VHairline: View {
    var color: Color = Theme.hairline
    var body: some View { Rectangle().fill(color).frame(width: 1) }
}

// MARK: - Metrics and key/value rows

struct MetricCell: View {
    let label: String
    let value: String
    var unit: String = ""
    var valueColor: Color = Theme.text1
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionLabel(label)
            Text(value).font(Theme.Font.mono(20, .semibold)).foregroundStyle(valueColor).lineLimit(1).minimumScaleFactor(0.7)
            if !unit.isEmpty { MetaText(unit) }
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .trailing) { VHairline() }
    }
}

struct MetricRow: View {
    let cells: [MetricCell]
    var body: some View {
        HStack(spacing: 0) { ForEach(Array(cells.enumerated()), id: \.offset) { $0.element } }
    }
}

struct KeyValueRow: View {
    let key: String
    let value: String
    var keyWidth: CGFloat = 84
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(key).font(Theme.Font.body(12, .medium)).foregroundStyle(Theme.label).frame(width: keyWidth, alignment: .leading)
            Text(value).font(Theme.Font.body(13)).foregroundStyle(Theme.textOnPanel).textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14).padding(.vertical, 7)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
    }
}

// MARK: - Focus

/// Full Keyboard Access ring: 2 pt gold stroke 3 pt outside the control,
/// following its corner radius. Every DS control draws its own ring and
/// turns the system one off so a control never shows two.
struct DSFocusRing: ViewModifier {
    var radius: CGFloat
    @Environment(\.isFocused) private var focused
    func body(content: Content) -> some View {
        content.overlay {
            if focused {
                rounded(radius + 3).stroke(Theme.focusRing, lineWidth: 2).padding(-3).allowsHitTesting(false)
            }
        }
    }
}

extension View {
    func dsFocusRing(radius: CGFloat = Theme.Radius.s) -> some View { modifier(DSFocusRing(radius: radius)) }
}

/// Hosts a visual ButtonStyle inside a real Button so the face can read
/// focus, and so keyboard shortcuts, roles and disabled state pass through.
private struct FocusHostedButton<Face: ButtonStyle>: View {
    let configuration: PrimitiveButtonStyleConfiguration
    let face: Face
    var body: some View {
        Button(configuration).buttonStyle(face).focusEffectDisabled()
    }
}

// MARK: - Buttons

/// Hover is tracked per button so every control shows it is clickable before
/// the click, and pressed state shows the click landed.
private struct HoverTracking: ViewModifier {
    @Binding var hovering: Bool
    func body(content: Content) -> some View {
        content.onHover { inside in withAnimation(Theme.quick) { hovering = inside } }
    }
}

private func resolvedHeight(_ explicit: CGFloat?, _ size: ControlSize, regular: CGFloat) -> CGFloat {
    if let explicit { return explicit }
    switch size {
    case .mini, .small: return Theme.Metrics.controlSmall
    case .large, .extraLarge: return 32
    default: return regular
    }
}

struct DSPrimaryButtonStyle: PrimitiveButtonStyle {
    var height: CGFloat? = nil
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(height: height)) }
    struct Face: ButtonStyle {
        let height: CGFloat?
        func makeBody(configuration: Configuration) -> some View { DSPrimaryFace(configuration: configuration, height: height) }
    }
    fileprivate struct DSPrimaryFace: View {
        let configuration: ButtonStyleConfiguration
        let height: CGFloat?
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.controlSize) private var controlSize
        @State private var hovering = false
        var body: some View {
            let small = controlSize == .small || controlSize == .mini
            configuration.label
                .font(Theme.Font.body(small ? 12 : 13, .semibold))
                .foregroundStyle(Theme.onGold)
                .padding(.horizontal, small ? 10 : 14)
                .frame(height: resolvedHeight(height, controlSize, regular: Theme.Metrics.controlLarge))
                .background(configuration.isPressed ? Theme.goldDim : (hovering && isEnabled ? Theme.goldHover : Theme.gold), in: rounded(Theme.Radius.s))
                .opacity(isEnabled ? 1 : 0.4)
                .scaleEffect(configuration.isPressed ? 0.98 : 1)
                .contentShape(rounded(Theme.Radius.s))
                .dsFocusRing()
                .modifier(HoverTracking(hovering: $hovering))
                .animation(Theme.quick, value: configuration.isPressed)
        }
    }
}

struct DSSecondaryButtonStyle: PrimitiveButtonStyle {
    var height: CGFloat? = nil
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(height: height)) }
    struct Face: ButtonStyle {
        let height: CGFloat?
        func makeBody(configuration: Configuration) -> some View { DSSecondaryFace(configuration: configuration, height: height) }
    }
    fileprivate struct DSSecondaryFace: View {
        let configuration: ButtonStyleConfiguration
        let height: CGFloat?
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.controlSize) private var controlSize
        @State private var hovering = false
        var body: some View {
            let small = controlSize == .small || controlSize == .mini
            configuration.label
                .font(Theme.Font.body(small ? 12 : 13, .medium))
                .foregroundStyle(Theme.text1)
                .padding(.horizontal, small ? 9 : 12)
                .frame(height: resolvedHeight(height, controlSize, regular: Theme.Metrics.control))
                .background(Color.white.opacity(configuration.isPressed ? 0.14 : (hovering && isEnabled ? 0.09 : 0.05)), in: rounded(Theme.Radius.s))
                .overlay(rounded(Theme.Radius.s).stroke(hovering && isEnabled ? Theme.controlBorderHover : Theme.hairlineStrong, lineWidth: 1))
                .opacity(isEnabled ? 1 : 0.4)
                .contentShape(rounded(Theme.Radius.s))
                .dsFocusRing()
                .modifier(HoverTracking(hovering: $hovering))
                .animation(Theme.quick, value: configuration.isPressed)
        }
    }
}

/// Text-only button. The frame keeps a 28 pt hit target even though only
/// the label is drawn.
struct DSGhostButtonStyle: PrimitiveButtonStyle {
    var color: Color = Theme.text3
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(color: color)) }
    struct Face: ButtonStyle {
        let color: Color
        func makeBody(configuration: Configuration) -> some View { DSGhostFace(configuration: configuration, color: color) }
    }
    fileprivate struct DSGhostFace: View {
        let configuration: ButtonStyleConfiguration
        let color: Color
        @Environment(\.isEnabled) private var isEnabled
        @State private var hovering = false
        var body: some View {
            configuration.label
                .font(Theme.Font.body(12.5, .medium))
                .foregroundStyle((configuration.isPressed || hovering) && isEnabled ? Theme.text1 : color)
                .padding(.horizontal, 6)
                .frame(minHeight: Theme.Metrics.hitTarget)
                .background(Color.white.opacity(hovering && isEnabled ? 0.06 : 0), in: rounded(Theme.Radius.xs))
                .opacity(isEnabled ? 1 : 0.4)
                .contentShape(rounded(Theme.Radius.xs))
                .dsFocusRing(radius: Theme.Radius.xs)
                .modifier(HoverTracking(hovering: $hovering))
        }
    }
}

struct DSAccentOutlineButtonStyle: PrimitiveButtonStyle {
    var color: Color = Theme.gold
    var height: CGFloat = Theme.Metrics.control
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(color: color, height: height)) }
    struct Face: ButtonStyle {
        let color: Color
        let height: CGFloat
        func makeBody(configuration: Configuration) -> some View { DSAccentOutlineFace(configuration: configuration, color: color, height: height) }
    }
    fileprivate struct DSAccentOutlineFace: View {
        let configuration: ButtonStyleConfiguration
        let color: Color
        let height: CGFloat
        @Environment(\.isEnabled) private var isEnabled
        @State private var hovering = false
        var body: some View {
            configuration.label
                .font(Theme.Font.body(12, .semibold))
                .foregroundStyle(color)
                .padding(.horizontal, 10)
                .frame(height: height)
                .background(color.opacity(configuration.isPressed ? 0.24 : (hovering && isEnabled ? 0.16 : 0.1)), in: rounded(Theme.Radius.s))
                .overlay(rounded(Theme.Radius.s).stroke(color.opacity(0.6), lineWidth: 1))
                .opacity(isEnabled ? 1 : 0.4)
                .contentShape(rounded(Theme.Radius.s))
                .dsFocusRing()
                .modifier(HoverTracking(hovering: $hovering))
        }
    }
}

struct DSDangerButtonStyle: PrimitiveButtonStyle {
    var height: CGFloat = Theme.Metrics.controlLarge
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(height: height)) }
    struct Face: ButtonStyle {
        let height: CGFloat
        func makeBody(configuration: Configuration) -> some View { DSDangerFace(configuration: configuration, height: height) }
    }
    fileprivate struct DSDangerFace: View {
        let configuration: ButtonStyleConfiguration
        let height: CGFloat
        @Environment(\.isEnabled) private var isEnabled
        @State private var hovering = false
        var body: some View {
            let red = Color(hex: 0xE8433A)
            let active = (configuration.isPressed || hovering) && isEnabled
            configuration.label
                .font(Theme.Font.body(13, .semibold))
                .foregroundStyle(active ? .white : Theme.danger)
                .padding(.horizontal, 12)
                .frame(height: height)
                .background(configuration.isPressed ? red : red.opacity(hovering && isEnabled ? 0.85 : 0.12), in: rounded(Theme.Radius.s))
                .overlay(rounded(Theme.Radius.s).stroke(red.opacity(0.5), lineWidth: 1))
                .opacity(isEnabled ? 1 : 0.4)
                .contentShape(rounded(Theme.Radius.s))
                .dsFocusRing()
                .modifier(HoverTracking(hovering: $hovering))
        }
    }
}

/// Plain clickable rows and tiles: whole frame is the hit area, with a soft
/// hover wash so it reads as clickable.
struct DSRowButtonStyle: PrimitiveButtonStyle {
    var radius: CGFloat = Theme.Radius.m
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(radius: radius)) }
    struct Face: ButtonStyle {
        let radius: CGFloat
        func makeBody(configuration: Configuration) -> some View { DSRowFace(configuration: configuration, radius: radius) }
    }
    fileprivate struct DSRowFace: View {
        let configuration: ButtonStyleConfiguration
        let radius: CGFloat
        @State private var hovering = false
        var body: some View {
            configuration.label
                .contentShape(rounded(radius))
                .background(Color.white.opacity(configuration.isPressed ? 0.08 : (hovering ? 0.045 : 0)), in: rounded(radius))
                .dsFocusRing(radius: radius)
                .modifier(HoverTracking(hovering: $hovering))
        }
    }
}

/// No chrome, only the focus ring: for controls that draw their own face
/// (toggles, checkboxes, segments, tiles).
struct DSBareButtonStyle: PrimitiveButtonStyle {
    var radius: CGFloat = Theme.Radius.s
    func makeBody(configuration: Configuration) -> some View { FocusHostedButton(configuration: configuration, face: Face(radius: radius)) }
    struct Face: ButtonStyle {
        let radius: CGFloat
        func makeBody(configuration: Configuration) -> some View {
            configuration.label.contentShape(rounded(radius)).dsFocusRing(radius: radius)
        }
    }
}

/// Square icon-only button with a 28 pt hit target. The label is both the
/// tooltip and the VoiceOver name.
struct DSIconButton: View {
    let label: String
    let systemImage: String
    var size: CGFloat
    var tint: Color
    let action: () -> Void
    init(_ label: String, systemImage: String, size: CGFloat = Theme.Metrics.hitTarget, tint: Color = Theme.text2, action: @escaping () -> Void) {
        self.label = label; self.systemImage = systemImage; self.size = size; self.tint = tint; self.action = action
    }
    var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 11.5, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: size, height: size)
        }
        .buttonStyle(DSRowButtonStyle(radius: Theme.Radius.s))
        .help(label)
        .accessibilityLabel(label)
    }
}

extension PrimitiveButtonStyle where Self == DSPrimaryButtonStyle { static var dsPrimary: DSPrimaryButtonStyle { .init() } }
extension PrimitiveButtonStyle where Self == DSSecondaryButtonStyle { static var dsSecondary: DSSecondaryButtonStyle { .init() } }
extension PrimitiveButtonStyle where Self == DSGhostButtonStyle { static var dsGhost: DSGhostButtonStyle { .init() } }
extension PrimitiveButtonStyle where Self == DSAccentOutlineButtonStyle { static var dsAccentOutline: DSAccentOutlineButtonStyle { .init() } }
extension PrimitiveButtonStyle where Self == DSDangerButtonStyle { static var dsDanger: DSDangerButtonStyle { .init() } }
extension PrimitiveButtonStyle where Self == DSRowButtonStyle { static var dsRow: DSRowButtonStyle { .init() } }
extension PrimitiveButtonStyle where Self == DSBareButtonStyle { static var dsBare: DSBareButtonStyle { .init() } }

// MARK: - Toggle / radio / check

struct DSToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(spacing: 10) {
                ZStack(alignment: configuration.isOn ? .trailing : .leading) {
                    Capsule().fill(configuration.isOn ? Theme.gold : Color.white.opacity(0.14))
                    Circle().fill(configuration.isOn ? Theme.onGold : Theme.text2).frame(width: 12, height: 12).padding(2)
                }
                .frame(width: 30, height: 16)
                .animation(Theme.spring, value: configuration.isOn)
                configuration.label
            }
            .frame(minHeight: Theme.Metrics.hitTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.s))
        .accessibilityRepresentation { Toggle(isOn: configuration.$isOn) { configuration.label } }
    }
}

struct DSRadio: View {
    let on: Bool
    var body: some View {
        ZStack {
            Circle().stroke(on ? Theme.gold : Color.white.opacity(0.3), lineWidth: 1.2).frame(width: 13, height: 13)
            if on { Circle().fill(Theme.gold).frame(width: 7, height: 7) }
        }
        .frame(width: 15, height: 15)
        .accessibilityHidden(true)
    }
}

struct DSCheck: View {
    let on: Bool
    var body: some View {
        ZStack {
            rounded(Theme.Radius.xs).fill(on ? Theme.gold : Color.white.opacity(0.04)).frame(width: 15, height: 15)
                .overlay(rounded(Theme.Radius.xs).stroke(on ? Theme.gold : Color.white.opacity(0.3), lineWidth: 1))
            if on { Image(systemName: "checkmark").font(.system(size: 9, weight: .heavy)).foregroundStyle(Theme.onGold) }
        }
        .animation(Theme.quick, value: on)
        .accessibilityHidden(true)
    }
}

// MARK: - Segmented bar (generic)

/// Each segment's hit area is the full bar height (28 pt by default); the
/// gold pill is drawn 2 pt inside it.
struct SegmentedBar<T: Hashable>: View {
    @Binding var selection: T
    let options: [(T, String)]
    var height: CGFloat = Theme.Metrics.hitTarget
    @Namespace private var pill
    var body: some View {
        HStack(spacing: 0) {
            ForEach(options, id: \.0) { option in
                let on = selection == option.0
                Button { withAnimation(Theme.spring) { selection = option.0 } } label: {
                    Text(option.1)
                        .font(Theme.Font.body(12.5, on ? .semibold : .medium))
                        .foregroundStyle(on ? Theme.onGold : Theme.text2)
                        .lineLimit(1).fixedSize()
                        .padding(.horizontal, 12)
                        .frame(height: height)
                        .background {
                            if on { rounded(Theme.Radius.xs).fill(Theme.gold).padding(2).matchedGeometryEffect(id: "pill", in: pill) }
                        }
                }
                .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.xs))
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
        .background(Color.black.opacity(0.28), in: rounded(Theme.Radius.s))
        .overlay(rounded(Theme.Radius.s).stroke(Theme.hairlineStrong, lineWidth: 1))
        .accessibilityElement(children: .contain)
    }
}

// MARK: - Progress

enum ProgressTone { case gold, ai, done, paused, neutral, info }

struct GrainProgressBar: View {
    var progress: Double
    var tone: ProgressTone = .gold
    var height: CGFloat = 6
    private var colors: [Color] {
        switch tone {
        case .gold: return [Color(hex: 0xFF8A1A), Color(hex: 0xFFB000), Theme.gold, Color(hex: 0xFFE9A3)]
        case .ai: return [Color(hex: 0x7462A6), Theme.ai, Color(hex: 0xDED2F2)]
        case .done: return [Color(hex: 0xFFB000), Theme.gold, Theme.delight]
        case .paused: return [Color(red: 185/255, green: 190/255, blue: 196/255).opacity(0.28), Color(red: 185/255, green: 190/255, blue: 196/255).opacity(0.55)]
        case .neutral: return [Color(hex: 0xD6DADE).opacity(0.35), Color(hex: 0xD6DADE).opacity(0.85)]
        case .info: return [Theme.info.opacity(0.2), Theme.info.opacity(0.55)]
        }
    }
    var body: some View {
        GeometryReader { geo in
            let p = max(0, min(1, progress.isFinite ? progress : 0))
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.07))
                LinearGradient(colors: colors, startPoint: .leading, endPoint: .trailing)
                    .frame(width: geo.size.width)
                    .mask(alignment: .leading) { Capsule().frame(width: max(p > 0 ? height : 0, geo.size.width * p)) }
                GrainOverlay().opacity(0.3).blendMode(.overlay).clipShape(Capsule())
            }
        }
        .frame(height: height)
        .animation(.easeOut(duration: 0.3), value: progress)
        .accessibilityElement()
        .accessibilityValue(Text("\(Int((progress.isFinite ? progress : 0) * 100)) percent"))
    }
}

/// Fine procedural noise tiled from one cached 140 pt texture, so progress
/// bars and the window grain share a single bitmap instead of a per-frame filter.
struct GrainOverlay: View {
    var body: some View {
        Image(nsImage: GrainTexture.shared)
            .resizable(resizingMode: .tile)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

enum GrainTexture {
    /// 140 pt tile backed by a 2× bitmap so the grain stays sub-point on
    /// Retina. Three octaves of smoothed noise summed around mid-grey give the
    /// soft film look of the prototype's fractalNoise filter instead of
    /// harsh per-pixel static.
    static let shared: NSImage = {
        let points = 140, scale = 2, size = points * scale
        var rng = SystemRandomNumberGenerator()
        func layer(cells: Int) -> [Float] {
            var grid = [Float](repeating: 0, count: (cells + 1) * (cells + 1))
            for i in grid.indices { grid[i] = Float.random(in: -1...1, using: &rng) }
            var out = [Float](repeating: 0, count: size * size)
            let step = Float(size) / Float(cells)
            for y in 0..<size {
                let fy = Float(y) / step, y0 = Int(fy), ty = fy - Float(y0)
                for x in 0..<size {
                    let fx = Float(x) / step, x0 = Int(fx), tx = fx - Float(x0)
                    let a = grid[y0 * (cells + 1) + x0], b = grid[y0 * (cells + 1) + min(x0 + 1, cells)]
                    let c = grid[min(y0 + 1, cells) * (cells + 1) + x0], d = grid[min(y0 + 1, cells) * (cells + 1) + min(x0 + 1, cells)]
                    let sx = tx * tx * (3 - 2 * tx), sy = ty * ty * (3 - 2 * ty)
                    out[y * size + x] = (a + (b - a) * sx) + ((c + (d - c) * sx) - (a + (b - a) * sx)) * sy
                }
            }
            return out
        }
        let fine = layer(cells: size), mid = layer(cells: size / 2), coarse = layer(cells: size / 5)
        var pixels = [UInt8](repeating: 128, count: size * size)
        for i in pixels.indices {
            let n = fine[i] * 0.55 + mid[i] * 0.3 + coarse[i] * 0.15
            pixels[i] = UInt8(max(0, min(255, 128 + n * 70)))
        }
        let provider = CGDataProvider(data: Data(pixels) as CFData)!
        let cg = CGImage(width: size, height: size, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: size,
                         space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.none.rawValue),
                         provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)!
        return NSImage(cgImage: cg, size: NSSize(width: points, height: points))
    }()
}

// MARK: - Selectable row

struct RailRow<Content: View>: View {
    let selected: Bool
    var height: CGFloat? = nil
    var action: () -> Void
    @ViewBuilder var content: Content
    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                content
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .frame(height: height)
            .frame(minHeight: Theme.Metrics.hitTarget)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? Theme.gold.opacity(0.12) : Color.clear, in: rounded(Theme.Radius.m))
            .overlay(rounded(Theme.Radius.m).stroke(selected ? Theme.gold.opacity(0.45) : Color.clear, lineWidth: 1))
        }
        .buttonStyle(.dsRow)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .padding(.horizontal, 6)
    }
}

// MARK: - Chips and tags

struct TagChip: View {
    let text: String
    var body: some View {
        Text(text).font(Theme.Font.body(11.5, .medium)).foregroundStyle(Theme.text2)
            .padding(.horizontal, 7).padding(.vertical, 2)
            .background(Color.white.opacity(0.07), in: rounded(Theme.Radius.xs))
    }
}

struct KindBadge: View {
    let text: String
    var color: Color = Theme.textOnPanel
    var background: Color = Color.black.opacity(0.55)
    var border: Color? = nil
    var body: some View {
        Text(text.capitalized).font(Theme.Font.body(10.5, .semibold)).foregroundStyle(color)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(background, in: rounded(Theme.Radius.xs))
            .overlay { if let border { rounded(Theme.Radius.xs).stroke(border, lineWidth: 1) } }
    }
}

// MARK: - Inputs

/// Text field on a dark well. Keyboard focus draws the same gold ring as
/// the buttons; `mono` is for paths, numbers and timecodes.
struct DSTextField: View {
    let placeholder: String
    @Binding var text: String
    var icon: String? = nil
    var height: CGFloat = Theme.Metrics.controlLarge
    var mono = false
    var onSubmit: () -> Void = {}
    @FocusState private var focused: Bool
    var body: some View {
        HStack(spacing: 8) {
            if let icon { Image(systemName: icon).font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.text3).accessibilityHidden(true) }
            TextField(placeholder, text: $text)
                .textFieldStyle(.plain)
                .font(mono ? Theme.Font.mono(12.5) : Theme.Font.body(13.5))
                .foregroundStyle(Theme.text1)
                .focused($focused)
                .onSubmit(onSubmit)
                .accessibilityLabel(placeholder)
            if !text.isEmpty && icon == "magnifyingglass" {
                DSIconButton("Clear", systemImage: "xmark", size: 20, tint: Theme.text3) { text = "" }
            }
        }
        .padding(.horizontal, 10)
        .frame(height: height)
        .background(Color.black.opacity(0.3), in: rounded(Theme.Radius.s))
        .overlay(rounded(Theme.Radius.s).stroke(focused ? Theme.gold.opacity(0.7) : Theme.hairlineStrong, lineWidth: 1))
        .overlay { if focused { rounded(Theme.Radius.s + 3).stroke(Theme.focusRing, lineWidth: 2).padding(-3).allowsHitTesting(false) } }
        .contentShape(rounded(Theme.Radius.s))
        .onTapGesture { focused = true }
        .animation(Theme.quick, value: focused)
    }
}

/// Numeric twin of `DSTextField` for seconds, pixels and similar values.
struct DSNumberField<F: ParseableFormatStyle>: View where F.FormatOutput == String {
    let placeholder: String
    @Binding var value: F.FormatInput
    let format: F
    var width: CGFloat = 96
    var height: CGFloat = Theme.Metrics.control
    @FocusState private var focused: Bool
    var body: some View {
        TextField(placeholder, value: $value, format: format)
            .textFieldStyle(.plain)
            .font(Theme.Font.mono(12.5))
            .foregroundStyle(Theme.text1)
            .focused($focused)
            .accessibilityLabel(placeholder)
            .padding(.horizontal, 8)
            .frame(width: width, height: height)
            .background(Color.black.opacity(0.3), in: rounded(Theme.Radius.s))
            .overlay(rounded(Theme.Radius.s).stroke(focused ? Theme.gold.opacity(0.7) : Theme.hairlineStrong, lineWidth: 1))
            .overlay { if focused { rounded(Theme.Radius.s + 3).stroke(Theme.focusRing, lineWidth: 2).padding(-3).allowsHitTesting(false) } }
    }
}

// MARK: - Empty / dashed notice

struct DashedNotice: View {
    let title: String
    var subtitle: String? = nil
    var body: some View {
        VStack(spacing: 6) {
            Text(title).font(Theme.Font.display(28)).foregroundStyle(Theme.text1)
            if let subtitle { MetaText(subtitle) }
        }
        .frame(maxWidth: .infinity)
        .padding(40)
        .overlay(rounded(Theme.Radius.l).stroke(style: StrokeStyle(lineWidth: 1, dash: [5, 4])).foregroundStyle(Color.white.opacity(0.16)))
    }
}

// MARK: - Toast

@MainActor @Observable
final class ToastCenter {
    static let shared = ToastCenter()
    struct Toast: Identifiable { let id = UUID(); let text: String; let actionLabel: String?; let action: (() -> Void)? }
    private(set) var current: Toast?
    private var dismissTask: Task<Void, Never>?

    func show(_ text: String, actionLabel: String? = nil, duration: Double = 5, action: (() -> Void)? = nil) {
        dismissTask?.cancel()
        current = Toast(text: text, actionLabel: actionLabel, action: action)
        dismissTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(duration))
            guard !Task.isCancelled else { return }
            self?.current = nil
        }
    }
    func dismiss() { dismissTask?.cancel(); current = nil }
}

struct ToastView: View {
    let toast: ToastCenter.Toast
    var body: some View {
        HStack(spacing: 12) {
            Circle().fill(Theme.delight).frame(width: 7, height: 7)
            Text(toast.text).font(Theme.Font.body(13, .medium)).foregroundStyle(Theme.text1).lineLimit(1)
            if let label = toast.actionLabel {
                Button(label) { toast.action?(); ToastCenter.shared.dismiss() }.buttonStyle(.dsAccentOutline)
            }
        }
        .padding(.leading, 14).padding(.trailing, 10).padding(.vertical, 8)
        .background(.ultraThinMaterial, in: rounded(Theme.Radius.l))
        .background(Color(hex: 0x1A1A1D).opacity(0.85), in: rounded(Theme.Radius.l))
        .overlay(rounded(Theme.Radius.l).stroke(Color.white.opacity(0.14), lineWidth: 1))
        .shadow(color: .black.opacity(0.45), radius: 20, y: 12)
    }
}

/// Type-erased button style for call sites that choose a style at runtime.
struct AnyButtonStyle: PrimitiveButtonStyle {
    private let make: (Configuration) -> AnyView
    init<S: PrimitiveButtonStyle>(_ style: S) { make = { AnyView(style.makeBody(configuration: $0)) } }
    func makeBody(configuration: Configuration) -> some View { make(configuration) }
}

// MARK: - App-wide native control styles

/// Checkbox: 15 pt rounded box, gold fill with a check when on, label beside it.
struct DSCheckboxToggleStyle: ToggleStyle {
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(alignment: .center, spacing: 8) {
                DSCheck(on: configuration.isOn)
                configuration.label.font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1)
            }
            .frame(minHeight: Theme.Metrics.hitTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.xs))
        .opacity(isEnabled ? 1 : 0.4)
        .accessibilityRepresentation { Toggle(isOn: configuration.$isOn) { configuration.label } }
    }
}

/// Determinate progress draws the grained gold bar; indeterminate progress
/// draws a short gold segment sweeping across the same track.
struct DSProgressViewStyle: ProgressViewStyle {
    func makeBody(configuration: Configuration) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let fraction = configuration.fractionCompleted {
                GrainProgressBar(progress: fraction, tone: .gold)
            } else {
                IndeterminateBar()
            }
            configuration.label.font(Theme.Font.meta).foregroundStyle(Theme.text3)
            configuration.currentValueLabel.font(Theme.Font.meta).foregroundStyle(Theme.text3)
        }
    }
}

struct IndeterminateBar: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: CGFloat = -0.35
    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.07))
                Capsule()
                    .fill(LinearGradient(colors: [Theme.gold.opacity(0), Theme.gold, Theme.gold.opacity(0)], startPoint: .leading, endPoint: .trailing))
                    .frame(width: geo.size.width * 0.35)
                    .offset(x: geo.size.width * (reduceMotion ? 0.32 : phase))
            }
            .clipShape(Capsule())
        }
        .frame(height: 4)
        .frame(minWidth: 40)
        .onAppear {
            guard !reduceMotion else { return }
            withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: false)) { phase = 1.0 }
        }
        .accessibilityLabel("In progress")
    }
}

/// Label beside a control, the prototype's "Resolution [720p|1080p]" row.
struct LabeledSegment<Content: View>: View {
    let label: String
    @ViewBuilder var content: Content
    init(_ label: String, @ViewBuilder content: () -> Content) { self.label = label; self.content = content() }
    var body: some View {
        HStack(spacing: 10) {
            Text(label).font(Theme.Font.body(12.5, .medium)).foregroundStyle(Theme.text2)
            content
        }
    }
}

/// Aspect-ratio picker drawn as frame tiles (Original, Vertical, Horizontal, Square, Portrait).
struct AspectTiles: View {
    @Binding var selection: String
    private let options: [(id: String, label: String, ratio: CGFloat)] = [
        ("source", "Original", 16 / 9), ("9:16", "Vertical", 9 / 16), ("16:9", "Horizontal", 16 / 9), ("1:1", "Square", 1), ("4:5", "Portrait", 4 / 5)
    ]
    var body: some View {
        HStack(spacing: 8) {
            ForEach(options, id: \.id) { option in
                let on = selection == option.id
                Button { withAnimation(Theme.spring) { selection = option.id } } label: {
                    VStack(spacing: 7) {
                        rounded(3).stroke(on ? Theme.gold : Theme.text2, lineWidth: 1.5)
                            .frame(width: option.ratio >= 1 ? 30 : 30 * option.ratio, height: option.ratio >= 1 ? 30 / option.ratio : 30)
                            .frame(height: 32)
                        Text(option.label).font(Theme.Font.body(13, .semibold)).foregroundStyle(on ? Theme.gold : Theme.text1)
                        Text(option.id == "source" ? "Source" : option.id).font(Theme.Font.mono(11)).foregroundStyle(on ? Theme.gold : Theme.text3)
                    }
                    .frame(width: 96)
                    .padding(.vertical, 10)
                    .background(on ? Theme.gold.opacity(0.1) : Color.white.opacity(0.02), in: rounded(Theme.Radius.m))
                    .overlay(rounded(Theme.Radius.m).stroke(on ? Theme.gold : Theme.hairlineStrong, lineWidth: 1))
                }
                .buttonStyle(.dsRow)
                .accessibilityLabel(option.label)
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
    }
}

extension View {
    /// List chrome: transparent rows on the panel fill, rounded frame, and an
    /// optional empty-state line instead of a blank grey box.
    func dsList(empty: String? = nil) -> some View {
        self
            .scrollContentBackground(.hidden)
            .background(Theme.panel, in: rounded(Theme.Radius.l))
            .clipShape(rounded(Theme.Radius.l))
            .overlay(rounded(Theme.Radius.l).stroke(Theme.hairlineStrong, lineWidth: 1))
            .overlay {
                if let empty {
                    MetaText(empty).multilineTextAlignment(.center).padding(20).allowsHitTesting(false)
                }
            }
    }
}
