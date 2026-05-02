import SwiftUI

/// Floating frosted bar pinned to the bottom of the Restructure tab.
struct RestructureApplyBar: View {
    let selectedCount: Int
    let totalCount: Int
    let canApply: Bool
    var onApplyShortcuts: () -> Void
    var onConvertToMoves: () -> Void

    @State private var primaryHovered = false

    var body: some View {
        HStack(alignment: .center, spacing: 18) {
            selectionSummary
            Divider().frame(height: 32).opacity(0.25)
            stepChips
            Spacer(minLength: 16)
            secondaryButton
            primaryButton
        }
        .padding(.horizontal, 18).padding(.vertical, 14)
        .background(
            RoundedRectangle(cornerRadius: 16)
                .fill(.regularMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 16)
                        .stroke(Color.white.opacity(0.10), lineWidth: 1)
                )
                .shadow(color: .black.opacity(0.45), radius: 22, y: 10)
                .shadow(color: Theme.gold.opacity(canApply ? 0.18 : 0),
                          radius: 14, y: 0)
        )
        .animation(.easeInOut(duration: 0.25), value: canApply)
    }

    @ViewBuilder
    private var selectionSummary: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text("\(selectedCount)")
                    .font(.system(size: 18, weight: .bold, design: .rounded))
                    .monospacedDigit()
                    .foregroundStyle(canApply ? Theme.gold : Color.primary)
                Text("of").font(.caption).foregroundStyle(.secondary)
                Text("\(totalCount)")
                    .font(.system(size: 14, weight: .semibold, design: .rounded))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                Text("selected").font(.caption).foregroundStyle(.secondary)
            }
            Text(selectedCount == 0
                  ? "Approve a recommendation above to enable Apply."
                  : "Originals stay put — applying creates shortcuts you can review.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder
    private var stepChips: some View {
        HStack(spacing: 8) {
            stepChip(number: "1", label: "Apply as shortcuts",
                      hint: "Safe preview", filled: true)
            Image(systemName: "arrow.right")
                .font(.caption2)
                .foregroundStyle(.tertiary)
            stepChip(number: "2", label: "Convert to real moves",
                      hint: "When ready", filled: false)
        }
    }

    @ViewBuilder
    private func stepChip(number: String, label: String,
                            hint: String, filled: Bool) -> some View {
        HStack(spacing: 6) {
            Text(number)
                .font(.caption2.bold().monospacedDigit())
                .foregroundStyle(filled ? .black : Theme.gold)
                .frame(width: 16, height: 16)
                .background(Circle().fill(filled ? Theme.gold : Theme.gold.opacity(0.15)))
            VStack(alignment: .leading, spacing: 0) {
                Text(label).font(.caption.weight(.semibold))
                Text(hint).font(.system(size: 9)).foregroundStyle(.tertiary)
            }
        }
    }

    @ViewBuilder
    private var primaryButton: some View {
        Button(action: onApplyShortcuts) {
            HStack(spacing: 6) {
                Image(systemName: "link").font(.callout.bold())
                Text(selectedCount > 0
                      ? "Apply as shortcuts (\(selectedCount))"
                      : "Apply as shortcuts")
                    .font(.callout.weight(.semibold))
            }
            .padding(.horizontal, 16).padding(.vertical, 9)
            .background(
                RoundedRectangle(cornerRadius: 10).fill(
                    LinearGradient(
                        colors: [Theme.gold, Theme.goldDim],
                        startPoint: .top, endPoint: .bottom
                    )
                )
            )
            .foregroundStyle(.black)
            .opacity(canApply ? 1.0 : 0.45)
        }
        .buttonStyle(.plain)
        .disabled(!canApply)
        .help("Creates shortcuts at the new paths pointing back to the original files. Originals stay put — fully reversible.")
        .scaleEffect(primaryHovered && canApply ? 1.02 : 1.0)
        .animation(.spring(response: 0.28, dampingFraction: 0.7),
                     value: primaryHovered)
        .onHover { primaryHovered = $0 }
    }

    @ViewBuilder
    private var secondaryButton: some View {
        Button(action: onConvertToMoves) {
            HStack(spacing: 6) {
                Image(systemName: "arrow.triangle.swap").font(.callout)
                Text("Convert to real moves").font(.callout)
            }
            .padding(.horizontal, 14).padding(.vertical, 8)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .stroke(Theme.gold.opacity(0.55), lineWidth: 1)
            )
            .foregroundStyle(Theme.gold)
        }
        .buttonStyle(.plain)
        .help("Once the structure looks right, replace every shortcut with a real on-disk move. Not reversible inside the app.")
    }
}
