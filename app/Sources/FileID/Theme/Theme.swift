import SwiftUI

// MARK: - Theme

enum Theme {
    static let gold    = Color(red: 1.0, green: 0.8, blue: 0.0)
    static let goldDim = Color(red: 0.8, green: 0.64, blue: 0.0)

    static let surfaceBase     = Color.black.opacity(0.3)
    static let surfaceCard     = Color.white.opacity(0.06)
    static let surfaceBorder   = Color.white.opacity(0.08)

    enum Space {
        static let xs: CGFloat = 4
        static let s:  CGFloat = 8
        static let m:  CGFloat = 16
        static let l:  CGFloat = 24
        static let xl: CGFloat = 40
    }

    enum Radius {
        static let s:  CGFloat = 8
        static let m:  CGFloat = 12
        static let l:  CGFloat = 16
    }
}

// MARK: - GlassCard

struct GlassCard<Content: View>: View {
    @ViewBuilder var content: Content
    var padding: CGFloat = Theme.Space.m

    var body: some View {
        content
            .padding(padding)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.m)
                    .fill(.ultraThinMaterial)
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.Radius.m)
                            .stroke(Theme.surfaceBorder, lineWidth: 1)
                    )
            )
    }
}

// MARK: - BadgePill

struct BadgePill: View {
    let label:  String
    var color:  Color = Theme.gold

    var body: some View {
        Text(label)
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Capsule().fill(color.opacity(0.15)))
    }
}

// MARK: - SettingToggleRow
// Gold, right-aligned toggle. Used everywhere to keep switches consistent.

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
                Text(title).font(.caption.bold())
                if let subtitle {
                    Text(subtitle)
                        .font(.system(size: 10))
                        .foregroundStyle(.tertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 12)
            Toggle("", isOn: $isOn)
                .toggleStyle(.switch)
                .labelsHidden()
                .tint(Theme.gold)
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
                .fontWeight(.semibold)
                .foregroundStyle(Color.black)
        }
        .buttonStyle(.borderedProminent)
        .tint(Theme.gold)
    }
}

// MARK: - ThemedSegmentedControl
// .pickerStyle(.segmented) renders light and clashes with gold-on-dark.

struct ThemedSegmentedControl: View {
    @Binding var selection: String
    let options: [(tag: String, label: String)]

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options, id: \.tag) { opt in
                let selected = selection == opt.tag
                Button {
                    withAnimation(.easeInOut(duration: 0.15)) { selection = opt.tag }
                } label: {
                    Text(opt.label)
                        .font(.system(size: 12, weight: selected ? .bold : .medium))
                        .foregroundStyle(selected ? Color.black : Color.primary.opacity(0.7))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .background(
                            Capsule()
                                .fill(selected ? Theme.gold : Color.white.opacity(0.08))
                        )
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(3)
        .background(
            RoundedRectangle(cornerRadius: 20)
                .fill(Color.white.opacity(0.05))
                .overlay(
                    RoundedRectangle(cornerRadius: 20)
                        .stroke(Color.white.opacity(0.1), lineWidth: 1)
                )
        )
    }
}

struct ThemedTogglePicker: View {
    @Binding var selection: Bool
    let falseLabel: String
    let trueLabel:  String

    var body: some View {
        HStack(spacing: 2) {
            pillButton(label: falseLabel, active: !selection) { selection = false }
            pillButton(label: trueLabel,  active:  selection) { selection = true  }
        }
        .padding(3)
        .background(
            RoundedRectangle(cornerRadius: 20)
                .fill(Color.white.opacity(0.05))
                .overlay(
                    RoundedRectangle(cornerRadius: 20)
                        .stroke(Color.white.opacity(0.1), lineWidth: 1)
                )
        )
    }

    private func pillButton(label: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button {
            withAnimation(.easeInOut(duration: 0.15)) { action() }
        } label: {
            Text(label)
                .font(.system(size: 12, weight: active ? .bold : .medium))
                .foregroundStyle(active ? Color.black : Color.primary.opacity(0.7))
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(Capsule().fill(active ? Theme.gold : Color.white.opacity(0.08)))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
    }
}
