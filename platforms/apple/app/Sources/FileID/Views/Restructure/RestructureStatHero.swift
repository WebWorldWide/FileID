import SwiftUI

/// Metric row above the map: Staying put / Tidying / Reorganizing. Hovering
/// a cell cross-highlights the matching ribbons and recommendation rows.
struct RestructureStatHero: View {
    let summary: RestructureView.AssistantSummary
    var hoverBus: RestructureHoverBus

    var body: some View {
        HStack(spacing: 0) {
            StatCell(outcome: .keep, value: summary.staysPutFiles, label: "Staying put",
                     hint: "\(summary.anchorFolders) kept \(summary.anchorFolders == 1 ? "folder" : "folders")", hoverBus: hoverBus)
            VHairline()
            StatCell(outcome: .tidy, value: summary.movedOutFiles, label: "Tidying",
                     hint: "\(summary.mixedFolders) mixed \(summary.mixedFolders == 1 ? "folder" : "folders")", hoverBus: hoverBus)
            VHairline()
            StatCell(outcome: .reorganize, value: summary.dissolvedFiles, label: "Reorganizing",
                     hint: "\(summary.junkFolders) generic \(summary.junkFolders == 1 ? "folder" : "folders")", hoverBus: hoverBus)
        }
        .fixedSize(horizontal: false, vertical: true)
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous).stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
    }
}

private struct StatCell: View {
    let outcome: RestructureOutcome
    let value: Int
    let label: String
    let hint: String
    var hoverBus: RestructureHoverBus

    @State private var isHovered = false

    var body: some View {
        let isActive = hoverBus.touchesOutcome(outcome)
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Circle().fill(outcome.tint).frame(width: 7, height: 7)
                SectionLabel(label)
            }
            Text(value.formatted())
                .font(Theme.Font.mono(20, .semibold))
                .foregroundStyle(isActive ? outcome.tint : Theme.text1)
                .contentTransition(.numericText())
            MetaText(hint, size: 11.5)
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(outcome.tint.opacity(isActive ? 0.07 : 0), ignoresSafeAreaEdges: [])
        .contentShape(Rectangle())
        .onHover { hovering in
            // Edge-only forwarding: AppKit can repeat hover events on resize.
            if hovering != isHovered {
                isHovered = hovering
                hoverBus.set(hovering ? .outcome(outcome) : nil)
            }
        }
        .animation(Theme.spring, value: isActive)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue("\(value) files, \(hint)")
    }
}
