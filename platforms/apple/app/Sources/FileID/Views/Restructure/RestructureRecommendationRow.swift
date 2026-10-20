import SwiftUI

/// One row in the Recommendations panel. The panel owns the background, so
/// rows only draw the hover tint and the outcome rail.
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
    /// Read here rather than in the parent so a hover only re-renders this
    /// row, not the page (which would re-pass the proposal array).
    var hoverBus: RestructureHoverBus? = nil

    var onToggleApproval: () -> Void = {}
    var onToggleExpand: () -> Void = {}
    var onHover: (Bool) -> Void = { _ in }
    @ViewBuilder var expandedContent: () -> ExpandedContent

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                ZStack {
                    RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                        .fill(outcome.tint.opacity(0.16))
                    Image(systemName: outcome.icon)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(outcome.tint)
                }
                .frame(width: 30, height: 30)
                .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(headline).font(Theme.Font.body(13.5, .semibold)).foregroundStyle(Theme.text1)
                    Text(bodyText)
                        .font(Theme.Font.body(12.5))
                        .foregroundStyle(Theme.text2)
                        .fixedSize(horizontal: false, vertical: true)
                    if !isInformational {
                        HStack(spacing: 8) {
                            Button(action: onToggleExpand) {
                                Label(isExpanded ? "Hide files" : "Review files", systemImage: isExpanded ? "chevron.down" : "chevron.right")
                            }
                            .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                            .accessibilityValue(isExpanded ? "Expanded" : "Collapsed")
                            .help(isExpanded ? "Collapse the file list" : "Show every file this group moves")
                            if isApproved {
                                Button("Skip these", action: onToggleApproval)
                                    .buttonStyle(.dsGhost)
                                    .help("Leave these files where they are")
                            } else {
                                Button { onToggleApproval() } label: { Label("Include", systemImage: "checkmark") }
                                    .buttonStyle(DSAccentOutlineButtonStyle(color: outcome.tint, height: Theme.Metrics.hitTarget))
                                    .help("Include these files in the move")
                            }
                        }
                        .padding(.top, 4)
                    }
                }
                Spacer(minLength: 0)
                VStack(alignment: .trailing, spacing: 2) {
                    Text(fileCount.formatted())
                        .font(Theme.Font.mono(20, .semibold))
                        .foregroundStyle(isInformational ? Theme.text2 : outcome.tint)
                    MetaText(folderCount == 1 ? "1 folder" : "\(folderCount.formatted()) folders", size: 11.5)
                }
                .accessibilityElement(children: .combine)
            }
            .padding(.horizontal, 14).padding(.vertical, 12)

            if isExpanded && !isInformational {
                Hairline(color: Theme.hairlineSoft)
                expandedContent()
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .background(Color.white.opacity(0.02), ignoresSafeAreaEdges: [])
                    .transition(.opacity)
            }
        }
        .opacity(isApproved || isInformational ? 1.0 : 0.55)
        .background(highlighted ? outcome.tint.opacity(0.06) : Color.clear, ignoresSafeAreaEdges: [])
        .overlay(alignment: .leading) {
            Rectangle().fill(outcome.tint).frame(width: 3).opacity(highlighted ? 1 : 0)
        }
        .contentShape(Rectangle())
        .onHover(perform: onHover)
        .animation(Theme.springStandard, value: isExpanded)
        .animation(Theme.spring, value: isApproved)
        .animation(Theme.spring, value: highlighted)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(headline + (isApproved || isInformational ? "" : ", skipped"))
    }

    private var highlighted: Bool {
        isHighlighted || (hoverBus?.touchesOutcome(outcome) ?? false)
    }
}
