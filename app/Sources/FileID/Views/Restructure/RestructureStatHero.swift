import SwiftUI

/// Three big-number tiles above the Sankey: Staying / Tidying /
/// Reorganizing. Hovering a tile cross-highlights the matching ribbons
/// + recommendation cards via the shared hover bus.
struct RestructureStatHero: View {
    let summary: RestructureView.AssistantSummary
    var hoverBus: RestructureHoverBus

    var body: some View {
        HStack(spacing: 12) {
            StatTile(
                outcome: .keep,
                value: summary.staysPutFiles,
                label: "Staying put",
                hint: "\(summary.anchorFolders) anchored \(summary.anchorFolders == 1 ? "folder" : "folders")",
                hoverBus: hoverBus
            )
            StatTile(
                outcome: .tidy,
                value: summary.movedOutFiles,
                label: "Tidying",
                hint: "\(summary.mixedFolders) mixed \(summary.mixedFolders == 1 ? "folder" : "folders")",
                hoverBus: hoverBus
            )
            StatTile(
                outcome: .reorganize,
                value: summary.dissolvedFiles,
                label: "Reorganizing",
                hint: "\(summary.junkFolders) generic \(summary.junkFolders == 1 ? "folder" : "folders")",
                hoverBus: hoverBus
            )
        }
        .frame(maxWidth: .infinity)
    }
}

private struct StatTile: View {
    let outcome: RestructureOutcome
    let value: Int
    let label: String
    let hint: String
    var hoverBus: RestructureHoverBus

    @State private var isHovered = false

    var body: some View {
        let isActive = hoverBus.touchesOutcome(outcome)
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: outcome.icon)
                    .font(.caption)
                    .foregroundStyle(outcome.tint)
                Text(label.uppercased())
                    .font(.system(size: 10, weight: .semibold).monospaced())
                    .tracking(0.6)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            Text("\(value)")
                .font(.system(size: 36, weight: .bold, design: .rounded))
                .monospacedDigit()
                .foregroundStyle(isActive ? outcome.tint : Color.primary)
            Text(hint).font(.caption2).foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12).fill(.ultraThinMaterial))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(
                    isActive ? outcome.tint.opacity(0.7) : Color.white.opacity(0.06),
                    lineWidth: isActive ? 1.5 : 1
                )
        )
        .shadow(
            color: isActive ? outcome.tint.opacity(0.30) : .black.opacity(0.16),
            radius: isActive ? 16 : 5,
            y: isActive ? 5 : 2
        )
        .scaleEffect(isActive ? 1.012 : 1.0)
        .contentShape(RoundedRectangle(cornerRadius: 12))
        .onHover { hovering in
            // Edge-only forwarding to the bus — AppKit can fire
            // duplicate hover events during resize / focus change.
            if hovering != isHovered {
                isHovered = hovering
                hoverBus.set(hovering ? .outcome(outcome) : nil)
            }
        }
        .animation(.easeInOut(duration: 0.18), value: isActive)
    }
}
