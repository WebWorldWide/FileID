import SwiftUI

// Prototype v3 primitives: hairline panels, mono section labels, metric
// cells, square controls and the grained progress bar.

// MARK: - Labels

struct SectionLabel: View {
    let text: String
    var color: Color = Theme.label
    init(_ text: String, color: Color = Theme.label) { self.text = text; self.color = color }
    var body: some View {
        Text(text)
            .font(Theme.Font.sectionLabel)
            .tracking(1.5)
            .textCase(.uppercase)
            .foregroundStyle(color)
            .lineLimit(1)
    }
}

struct MetaText: View {
    let text: String
    var color: Color = Theme.text3
    var size: CGFloat = 11.5
    init(_ text: String, color: Color = Theme.text3, size: CGFloat = 11.5) { self.text = text; self.color = color; self.size = size }
    var body: some View {
        Text(text).font(Theme.Font.mono(size)).foregroundStyle(color)
    }
}

// MARK: - Panels

struct Panel<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: 0) { content }
            .background(Theme.panel, ignoresSafeAreaEdges: [])
            .overlay(Rectangle().stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
            .clipped()
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
    var keyWidth: CGFloat = 80
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(key).font(Theme.Font.meta).tracking(0.9).textCase(.uppercase).foregroundStyle(Theme.label).frame(width: keyWidth, alignment: .leading)
            Text(value).font(Theme.Font.body(13)).foregroundStyle(Theme.textOnPanel).textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14).padding(.vertical, 7)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
    }
}

// MARK: - Buttons

struct DSPrimaryButtonStyle: ButtonStyle {
    var height: CGFloat? = nil
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.controlSize) private var controlSize
    private var resolvedHeight: CGFloat {
        if let height { return height }
        switch controlSize {
        case .mini, .small: return Theme.Metrics.controlSmall
        case .large, .extraLarge: return 32
        default: return Theme.Metrics.controlLarge
        }
    }
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Font.mono(controlSize == .small || controlSize == .mini ? 11 : 12, .semibold))
            .foregroundStyle(Theme.onGold)
            .padding(.horizontal, controlSize == .small || controlSize == .mini ? 10 : 14)
            .frame(height: resolvedHeight)
            .background(configuration.isPressed ? Theme.goldHover : Theme.gold, ignoresSafeAreaEdges: [])
            .opacity(isEnabled ? 1 : 0.38)
            .contentShape(Rectangle())
    }
}

struct DSSecondaryButtonStyle: ButtonStyle {
    var height: CGFloat? = nil
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.controlSize) private var controlSize
    private var resolvedHeight: CGFloat {
        if let height { return height }
        switch controlSize {
        case .mini, .small: return Theme.Metrics.controlSmall
        case .large, .extraLarge: return 32
        default: return Theme.Metrics.control
        }
    }
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Font.mono(controlSize == .small || controlSize == .mini ? 11 : 12))
            .foregroundStyle(Theme.textOnPanel)
            .padding(.horizontal, controlSize == .small || controlSize == .mini ? 9 : 12)
            .frame(height: resolvedHeight)
            .background(configuration.isPressed ? Color.white.opacity(0.1) : Color.white.opacity(0.04), ignoresSafeAreaEdges: [])
            .overlay(Rectangle().stroke(Theme.controlBorder, lineWidth: 1))
            .opacity(isEnabled ? 1 : 0.38)
            .contentShape(Rectangle())
    }
}

struct DSGhostButtonStyle: ButtonStyle {
    var color: Color = Theme.text3
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Font.mono(11.5))
            .foregroundStyle(configuration.isPressed ? Theme.text1 : color)
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
    }
}

struct DSAccentOutlineButtonStyle: ButtonStyle {
    var color: Color = Theme.gold
    var height: CGFloat = Theme.Metrics.controlSmall
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Font.mono(11))
            .foregroundStyle(color)
            .padding(.horizontal, 9)
            .frame(height: height)
            .background(color.opacity(configuration.isPressed ? 0.2 : 0.1), ignoresSafeAreaEdges: [])
            .overlay(Rectangle().stroke(color.opacity(0.6), lineWidth: 1))
            .contentShape(Rectangle())
    }
}

struct DSDangerButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Font.mono(12, .semibold))
            .foregroundStyle(configuration.isPressed ? .white : Theme.danger)
            .padding(.horizontal, 12)
            .frame(height: Theme.Metrics.controlLarge)
            .background(configuration.isPressed ? Color(hex: 0xE8433A) : Color(hex: 0xE8433A).opacity(0.1), ignoresSafeAreaEdges: [])
            .overlay(Rectangle().stroke(Color(hex: 0xE8433A).opacity(0.5), lineWidth: 1))
            .contentShape(Rectangle())
    }
}

extension ButtonStyle where Self == DSPrimaryButtonStyle { static var dsPrimary: DSPrimaryButtonStyle { .init() } }
extension ButtonStyle where Self == DSSecondaryButtonStyle { static var dsSecondary: DSSecondaryButtonStyle { .init() } }
extension ButtonStyle where Self == DSGhostButtonStyle { static var dsGhost: DSGhostButtonStyle { .init() } }
extension ButtonStyle where Self == DSAccentOutlineButtonStyle { static var dsAccentOutline: DSAccentOutlineButtonStyle { .init() } }
extension ButtonStyle where Self == DSDangerButtonStyle { static var dsDanger: DSDangerButtonStyle { .init() } }

// MARK: - Toggle / radio / check

struct DSToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(spacing: 10) {
                ZStack(alignment: configuration.isOn ? .trailing : .leading) {
                    Rectangle().fill(configuration.isOn ? Theme.gold.opacity(0.16) : Color.clear)
                        .overlay(Rectangle().stroke(configuration.isOn ? Theme.gold : Color.white.opacity(0.25), lineWidth: 1))
                    Rectangle().fill(configuration.isOn ? Theme.gold : Theme.text3).frame(width: 12, height: 12).padding(1)
                }
                .frame(width: 30, height: 16)
                .animation(.easeOut(duration: 0.15), value: configuration.isOn)
                configuration.label
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

struct DSRadio: View {
    let on: Bool
    var body: some View {
        ZStack {
            Rectangle().stroke(on ? Theme.gold : Color.white.opacity(0.3), lineWidth: 1).frame(width: 12, height: 12)
            if on { Rectangle().fill(Theme.gold).frame(width: 6, height: 6) }
        }
        .frame(width: 14, height: 14)
    }
}

struct DSCheck: View {
    let on: Bool
    var body: some View {
        ZStack {
            Rectangle().fill(on ? Theme.gold : Color.clear).frame(width: 14, height: 14)
                .overlay(Rectangle().stroke(on ? Theme.gold : Color.white.opacity(0.3), lineWidth: 1))
            if on { Text("✓").font(.system(size: 11, weight: .bold)).foregroundStyle(Theme.onGold) }
        }
    }
}

// MARK: - Segmented bar (generic)

struct SegmentedBar<T: Hashable>: View {
    @Binding var selection: T
    let options: [(T, String)]
    var height: CGFloat = 26
    var body: some View {
        HStack(spacing: 0) {
            ForEach(options, id: \.0) { option in
                let on = selection == option.0
                Button { selection = option.0 } label: {
                    Text(option.1)
                        .font(Theme.Font.mono(11.5, on ? .semibold : .medium))
                        .foregroundStyle(on ? Theme.onGold : Theme.text2)
                        .lineLimit(1).fixedSize()
                        .padding(.horizontal, 10)
                        .frame(height: height)
                        .background(on ? Theme.gold : Color.clear, ignoresSafeAreaEdges: [])
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
        .overlay(Rectangle().stroke(Theme.hairlineStrong, lineWidth: 1))
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
                Rectangle().fill(Color.white.opacity(0.07))
                LinearGradient(colors: colors, startPoint: .leading, endPoint: .trailing)
                    .frame(width: geo.size.width)
                    .mask(alignment: .leading) { Rectangle().frame(width: geo.size.width * p) }
                GrainOverlay().opacity(0.4).blendMode(.overlay)
            }
        }
        .frame(height: height)
        .animation(.easeOut(duration: 0.45), value: progress)
        .accessibilityElement()
        .accessibilityValue(Text("\(Int((progress.isFinite ? progress : 0) * 100)) percent"))
    }
}

/// Fine procedural noise tiled from one cached 140 px texture, so progress
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

// MARK: - Rail row (selected list item with a 3 px gold rail)

struct RailRow<Content: View>: View {
    let selected: Bool
    var height: CGFloat? = nil
    var action: () -> Void
    @ViewBuilder var content: Content
    var body: some View {
        Button(action: action) {
            HStack(spacing: 11) {
                Rectangle().fill(selected ? Theme.gold : Color.clear).frame(width: 3)
                content
                Spacer(minLength: 0)
            }
            .padding(.trailing, 14)
            .frame(height: height)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? Theme.goldWash : Color.clear, ignoresSafeAreaEdges: [])
            .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Chips and tags

struct TagChip: View {
    let text: String
    var body: some View {
        Text(text).font(Theme.Font.meta).foregroundStyle(Theme.text2)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .overlay(Rectangle().stroke(Theme.hairlineStrong, lineWidth: 1))
    }
}

struct KindBadge: View {
    let text: String
    var color: Color = Theme.textOnPanel
    var background: Color = Color.black.opacity(0.55)
    var border: Color? = nil
    var body: some View {
        Text(text).font(Theme.Font.mono(10, .semibold)).tracking(1).textCase(.uppercase).foregroundStyle(color)
            .padding(.horizontal, 5).padding(.vertical, 2).background(background, ignoresSafeAreaEdges: [])
            .overlay { if let border { Rectangle().stroke(border, lineWidth: 1) } }
    }
}

// MARK: - Inputs

struct DSTextField: View {
    let placeholder: String
    @Binding var text: String
    var icon: String? = nil
    var height: CGFloat = Theme.Metrics.controlLarge
    var onSubmit: () -> Void = {}
    var body: some View {
        HStack(spacing: 8) {
            if let icon { Image(systemName: icon).font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.text3) }
            TextField(placeholder, text: $text).textFieldStyle(.plain).font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1).onSubmit(onSubmit)
        }
        .padding(.horizontal, 10)
        .frame(height: height)
        .background(Color.black.opacity(0.3), ignoresSafeAreaEdges: [])
        .overlay(Rectangle().stroke(Theme.hairlineStrong.opacity(1.15), lineWidth: 1))
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
        .overlay(Rectangle().stroke(style: StrokeStyle(lineWidth: 1, dash: [4, 3])).foregroundStyle(Color.white.opacity(0.16)))
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
        HStack(spacing: 14) {
            Rectangle().fill(Theme.delight).frame(width: 6, height: 6)
            Text(toast.text).font(Theme.Font.mono(12)).foregroundStyle(Theme.text1).lineLimit(1)
            if let label = toast.actionLabel {
                Button(label) { toast.action?(); ToastCenter.shared.dismiss() }.buttonStyle(.dsAccentOutline)
            }
        }
        .padding(.leading, 14).padding(.trailing, 12).padding(.vertical, 9)
        .background(Color(hex: 0x1A1A1D), ignoresSafeAreaEdges: [])
        .overlay(Rectangle().stroke(Color.white.opacity(0.16), lineWidth: 1))
        .shadow(color: .black.opacity(0.55), radius: 25, y: 20)
    }
}

/// Type-erased button style for call sites that choose a style at runtime.
struct AnyButtonStyle: ButtonStyle {
    private let make: (Configuration) -> AnyView
    init<S: ButtonStyle>(_ style: S) { make = { AnyView(style.makeBody(configuration: $0)) } }
    func makeBody(configuration: Configuration) -> some View { make(configuration) }
}

// MARK: - App-wide native control styles

/// Square checkbox: 14 pt box, gold fill with ✓ when on, label beside it.
struct DSCheckboxToggleStyle: ToggleStyle {
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                DSCheck(on: configuration.isOn).alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4 }
                configuration.label.font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(isEnabled ? 1 : 0.4)
        .accessibilityAddTraits(configuration.isOn ? .isSelected : [])
    }
}

/// Determinate progress draws the grained gold bar; indeterminate progress
/// draws a short gold segment sweeping across the same square track.
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
                Rectangle().fill(Color.white.opacity(0.07))
                Rectangle()
                    .fill(LinearGradient(colors: [Theme.gold.opacity(0), Theme.gold, Theme.gold.opacity(0)], startPoint: .leading, endPoint: .trailing))
                    .frame(width: geo.size.width * 0.35)
                    .offset(x: geo.size.width * (reduceMotion ? 0.32 : phase))
            }
            .clipped()
        }
        .frame(height: 4)
        .frame(minWidth: 40)
        .onAppear {
            guard !reduceMotion else { return }
            withAnimation(.easeInOut(duration: 1.1).repeatForever(autoreverses: false)) { phase = 1.0 }
        }
        .accessibilityLabel("In progress")
    }
}

/// Mono label beside a control, the prototype's "RESOLUTION [720p|1080p]" row.
struct LabeledSegment<Content: View>: View {
    let label: String
    @ViewBuilder var content: Content
    init(_ label: String, @ViewBuilder content: () -> Content) { self.label = label; self.content = content() }
    var body: some View {
        HStack(spacing: 10) {
            SectionLabel(label)
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
                Button { selection = option.id } label: {
                    VStack(spacing: 8) {
                        Rectangle().stroke(on ? Theme.gold : Theme.text2, lineWidth: 1.5)
                            .frame(width: option.ratio >= 1 ? 30 : 30 * option.ratio, height: option.ratio >= 1 ? 30 / option.ratio : 30)
                            .frame(height: 32)
                        Text(option.label).font(Theme.Font.body(13, .semibold)).foregroundStyle(on ? Theme.gold : Theme.text1)
                        Text(option.id == "source" ? "Source" : option.id).font(Theme.Font.mono(11)).foregroundStyle(on ? Theme.gold : Theme.text3)
                    }
                    .frame(width: 96)
                    .padding(.vertical, 10)
                    .background(on ? Theme.gold.opacity(0.08) : Color.clear, ignoresSafeAreaEdges: [])
                    .overlay(Rectangle().stroke(on ? Theme.gold : Theme.hairlineStrong, lineWidth: 1))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(option.label)
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
    }
}

extension View {
    /// v3 list chrome: transparent rows on the panel fill, hairline frame, and an
    /// optional mono empty-state line instead of a blank grey box.
    func dsList(empty: String? = nil) -> some View {
        self
            .scrollContentBackground(.hidden)
            .background(Theme.panel, ignoresSafeAreaEdges: [])
            .overlay(Rectangle().stroke(Theme.hairlineStrong, lineWidth: 1))
            .overlay {
                if let empty {
                    MetaText(empty).multilineTextAlignment(.center).padding(20).allowsHitTesting(false)
                }
            }
    }
}
