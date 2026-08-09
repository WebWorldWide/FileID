import SwiftUI

/// Floating frosted bar pinned to the bottom of the Restructure tab.
///
/// One real-move action: the engine butler performs direct on-disk moves (there
/// is no macOS symlink-preview mode), so the prior two-step "apply as shortcuts →
/// convert to real moves" UI was vestigial — both buttons routed to the same
/// real-move confirmation — and its "originals stay put / reversible" copy
/// misrepresented an irreversible operation. Collapsed to a single Apply action
/// with honest, irreversible messaging; the caller still gates it behind a
/// confirmation dialog.
struct RestructureApplyBar: View {
    let selectedCount: Int
    let totalCount: Int
    let canApply: Bool
    /// True while an apply is in flight — disables the button so the
    /// irreversible path can't be double-fired.
    var isApplying: Bool = false
    var onApply: () -> Void

    @State private var primaryHovered = false

    var body: some View {
        HStack(alignment: .center, spacing: 18) {
            selectionSummary
            Spacer(minLength: 16)
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
                  : "Selected files are moved on disk when you apply — review first.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder
    private var primaryButton: some View {
        Button(action: onApply) {
            HStack(spacing: 6) {
                Image(systemName: "folder.fill.badge.gearshape").font(.callout.bold())
                Text(selectedCount > 0
                      ? "Apply moves (\(selectedCount))"
                      : "Apply moves")
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
            .opacity((canApply && !isApplying) ? 1.0 : 0.45)
        }
        .buttonStyle(.plain)
        .disabled(!canApply || isApplying)
        .help("Moves the selected files into the new structure on disk and updates the library. Runs through the engine and is not reversible inside the app — review the structure first.")
        .scaleEffect(primaryHovered && canApply && !isApplying ? 1.02 : 1.0)
        .animation(.spring(response: 0.28, dampingFraction: 0.7),
                     value: primaryHovered)
        .onHover { primaryHovered = $0 }
    }
}
