import SwiftUI

/// One row inside the unified Restructure surface. Carries no
/// background of its own — the parent owns the material plane so
/// adjacent rows don't read as competing cards.
struct RestructureRecommendationRow<ExpandedContent: View>: View {
    let outcome: RestructureOutcome
    let headline: String
    let bodyText: String
    let fileCount: Int
    let folderCount: Int
    let isApproved: Bool
    var isInformational = false
    var isExpanded = false
    var isHighlighted = false
    /// When supplied, the row derives its hover-highlight from the shared
    /// bus inside its OWN body — so the read doesn't live in the parent's
    /// body and trigger a full re-render (re-passing the proposal array)
    /// on every mouse-move.
    var hoverBus: RestructureHoverBus? = nil

    var onToggleApproval: () -> Void = {}
    var onToggleExpand: () -> Void = {}
    var onHover: (Bool) -> Void = { _ in }
    @ViewBuilder var expandedContent: () -> ExpandedContent

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 14) {
                iconBadge
                VStack(alignment: .leading, spacing: 4) {
                    Text(headline).font(.callout.weight(.semibold))
                    Text(bodyText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    if !isInformational {
                        actionRow.padding(.top, 4)
                    }
                }
                Spacer(minLength: 0)
                if !isInformational {
                    countBadge
                }
            }
            .padding(.horizontal, 18).padding(.vertical, 14)

            if isExpanded && !isInformational {
                Divider().opacity(0.18).padding(.leading, 64)
                expandedContent()
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                    .background(Color.white.opacity(0.025))
                    .transition(
                        .asymmetric(
                            insertion: .opacity.combined(with: .move(edge: .top)),
                            removal: .opacity
                        )
                    )
            }
        }
        .opacity(isApproved || isInformational ? 1.0 : 0.55)
        .background(
            Rectangle().fill(
                highlighted ? outcome.tint.opacity(0.06) : Color.clear
            )
        )
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(outcome.tint)
                .frame(width: 2)
                .opacity(highlighted ? 1.0 : 0.0)
        }
        .contentShape(Rectangle())
        .onHover(perform: onHover)
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: isExpanded)
        .animation(.easeInOut(duration: 0.18), value: isApproved)
        .animation(.easeInOut(duration: 0.18), value: highlighted)
    }

    /// Parent-provided flag OR, when a hover bus is supplied, derived here
    /// so the observable read lives in this row's body.
    private var highlighted: Bool {
        isHighlighted || (hoverBus?.touchesOutcome(outcome) ?? false)
    }

    @ViewBuilder
    private var iconBadge: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 10)
                .fill(outcome.tint.opacity(0.18))
                .frame(width: 38, height: 38)
            Image(systemName: outcome.icon)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(outcome.tint)
        }
    }

    @ViewBuilder
    private var countBadge: some View {
        VStack(alignment: .trailing, spacing: 1) {
            Text("\(fileCount)")
                .font(.system(size: 22, weight: .bold, design: .rounded))
                .monospacedDigit()
                .foregroundStyle(outcome.tint)
            Text(folderCount == 1 ? "1 folder" : "\(folderCount) folders")
                .font(.caption2.monospaced())
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder
    private var actionRow: some View {
        HStack(spacing: 8) {
            reviewButton
            approveButton
        }
    }

    @ViewBuilder
    private var reviewButton: some View {
        Button(action: onToggleExpand) {
            HStack(spacing: 4) {
                Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                    .font(.caption.bold())
                Text(isExpanded ? "Hide files" : "Review files")
                    .font(.caption.weight(.semibold))
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(
                Capsule().stroke(
                    isExpanded ? outcome.tint.opacity(0.7) : Color.secondary.opacity(0.5),
                    lineWidth: 1
                )
            )
            .foregroundStyle(isExpanded ? outcome.tint : .secondary)
        }
        .buttonStyle(.plain)
        .help(isExpanded ? "Collapse the file list." : "Expand to see every file this card affects.")
    }

    @ViewBuilder
    private var approveButton: some View {
        let label = isApproved ? "Skip these" : "Approve"
        let icon  = isApproved ? "xmark.circle" : "checkmark.circle.fill"
        let bgFill: Color = isApproved
            ? Color.secondary.opacity(0.15)
            : outcome.tint.opacity(0.85)
        let fg: Color = isApproved ? .primary : .black
        Button(action: onToggleApproval) {
            HStack(spacing: 4) {
                Image(systemName: icon).font(.caption)
                Text(label).font(.caption.weight(.semibold))
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(Capsule().fill(bgFill))
            .foregroundStyle(fg)
        }
        .buttonStyle(.plain)
        .help(isApproved
              ? "Exclude this card's files from the next apply."
              : "Include this card's files when you apply.")
    }
}
