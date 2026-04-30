// V7 — Recommendation cards for the Restructure tab.
//
// Mirrors the System Settings → Storage → Optimize pattern: one card
// per outcome class (kept as anchors / tidied / reorganized), each
// with primary "Approve" + secondary "Review files" + tertiary "Skip"
// buttons. Approval state is local to the parent RestructureView so
// "Apply approved" only acts on cards the user hasn't skipped.
import SwiftUI

/// One outcome class. Drives the card icon, copy, tint, and which
/// proposals it represents.
enum RestructureOutcome: String, CaseIterable, Identifiable {
    /// Anchors — folders staying put. No proposals; this card is
    /// informational ("X folders will be left alone").
    case keep
    /// Mixed folders — outliers moving out, rest staying.
    case tidy
    /// Junk folders — everything moving to better buckets.
    case reorganize

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .keep:       return "lock.fill"
        case .tidy:       return "tray.and.arrow.up.fill"
        case .reorganize: return "arrow.triangle.branch"
        }
    }

    var tint: Color {
        switch self {
        case .keep:       return .green
        case .tidy:       return .orange
        case .reorganize: return Theme.gold
        }
    }
}

/// Single recommendation row. Read-only props + callbacks; parent
/// owns the proposals + approval state.
struct RecommendationCard<ExpandedContent: View>: View {
    let outcome: RestructureOutcome
    let headline: String
    let bodyText: String
    /// Number of files this card represents (for the destructive-
    /// action confirmation dialog). 0 means informational only.
    let fileCount: Int
    /// Number of source folders affected (for sub-stat).
    let folderCount: Int
    /// Whether the user has approved this card. Cards default to
    /// approved; skipping excludes them from the apply pass.
    let isApproved: Bool
    /// True for the .keep card — informational, no action buttons.
    var isInformational: Bool = false
    /// True when the card is expanded inline showing its file list.
    /// Driven by the parent so multiple cards can coordinate (e.g.
    /// only one expanded at a time, or all expandable independently).
    var isExpanded: Bool = false
    /// Driven by the shared hover bus. When true the card glows in
    /// its outcome tint — used to mirror Sankey hover. The hover-
    /// driven outline + shadow live alongside the existing approval
    /// state styling so they compose without flicker.
    var isHighlighted: Bool = false

    var onToggleApproval: () -> Void = {}
    var onToggleExpand: () -> Void = {}
    /// Hover callback. Wired by the parent into the hover bus so a
    /// pointer over this card cross-highlights the matching ribbons
    /// in the Sankey + folder rows in the staysPut disclosure.
    var onHover: (Bool) -> Void = { _ in }
    /// View builder for the expanded content (the per-file list).
    /// The card renders this with a smooth height animation when
    /// `isExpanded` toggles. Pass `EmptyView()` for cards that
    /// shouldn't expand (informational cards).
    @ViewBuilder var expandedContent: () -> ExpandedContent

    @State private var isHovered: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                ZStack {
                    RoundedRectangle(cornerRadius: 8)
                        .fill(outcome.tint.opacity(0.18))
                        .frame(width: 36, height: 36)
                    Image(systemName: outcome.icon)
                        .font(.callout.bold())
                        .foregroundStyle(outcome.tint)
                }
                .padding(.top, 2)

                VStack(alignment: .leading, spacing: 4) {
                    Text(headline).font(.callout.bold())
                    Text(bodyText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    if !isInformational {
                        actionRow
                    }
                }
                Spacer(minLength: 0)

                // Big-number badge — file count + folder count.
                if !isInformational {
                    VStack(alignment: .trailing, spacing: 1) {
                        Text("\(fileCount)")
                            .font(.title3.bold().monospacedDigit())
                            .foregroundStyle(outcome.tint)
                        Text(folderCount == 1 ? "1 folder" : "\(folderCount) folders")
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                    .padding(.top, 4)
                }
            }
            .padding(14)

            // Inline-expanded content — slides into place beneath the
            // card header with a smooth spring animation. Replaces the
            // previous drill-down sheet so the user never loses context.
            if isExpanded && !isInformational {
                Divider().opacity(0.25).padding(.horizontal, 14)
                expandedContent()
                    .padding(.horizontal, 14)
                    .padding(.bottom, 14)
                    .transition(
                        .asymmetric(
                            insertion: .opacity.combined(with: .move(edge: .top)),
                            removal: .opacity
                        )
                    )
            }
        }
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(.ultraThinMaterial)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(strokeColor, lineWidth: strokeWidth)
        )
        .opacity(isApproved || isInformational ? 1.0 : 0.55)
        // Outer shadow matches LibraryView hover convention — radius
        // 5→14 on hover so each card reads as its own surface above
        // the .ultraThinMaterial of its neighbors. Tint shifts to the
        // outcome color when the hover bus is highlighting this card,
        // making the cross-highlight unmistakable.
        .shadow(
            color: shadowColor,
            radius: shadowRadius,
            y: shadowY
        )
        .scaleEffect(isHighlighted ? 1.005 : 1.0)
        .onHover { hovering in
            isHovered = hovering
            onHover(hovering)
        }
        .animation(.spring(response: 0.35, dampingFraction: 0.85), value: isExpanded)
        .animation(.easeInOut(duration: 0.18), value: isApproved)
        .animation(.easeInOut(duration: 0.18), value: isHovered)
        .animation(.easeInOut(duration: 0.18), value: isHighlighted)
    }

    private var strokeColor: Color {
        if isHighlighted {
            return outcome.tint.opacity(0.85)
        } else if isExpanded {
            return outcome.tint.opacity(0.55)
        } else if isApproved {
            return outcome.tint.opacity(0.30)
        } else {
            return Color.secondary.opacity(0.20)
        }
    }

    private var strokeWidth: CGFloat {
        isHighlighted || isExpanded ? 1.5 : 1
    }

    private var shadowColor: Color {
        if isHighlighted {
            return outcome.tint.opacity(0.45)
        }
        return Color.black.opacity(isHovered ? 0.45 : 0.18)
    }

    private var shadowRadius: CGFloat {
        if isHighlighted { return 16 }
        return isHovered ? 14 : 5
    }

    private var shadowY: CGFloat {
        isHovered ? 6 : 2
    }

    @ViewBuilder
    private var actionRow: some View {
        HStack(spacing: 8) {
            reviewButton
            approveButton
        }
        .padding(.top, 2)
    }

    @ViewBuilder
    private var reviewButton: some View {
        Button(action: onToggleExpand) {
            HStack(spacing: 4) {
                Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                    .font(.caption.bold())
                    .rotationEffect(.degrees(0))
                Text(isExpanded ? "Hide files" : "Review files")
                    .font(.caption.bold())
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(
                Capsule()
                    .stroke(
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
        let icon = isApproved ? "xmark.circle" : "checkmark.circle.fill"
        let bgFill: Color = isApproved
            ? Color.secondary.opacity(0.15)
            : outcome.tint.opacity(0.85)
        let fg: Color = isApproved ? .primary : .black
        Button(action: onToggleApproval) {
            HStack(spacing: 4) {
                Image(systemName: icon).font(.caption)
                Text(label).font(.caption.bold())
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
