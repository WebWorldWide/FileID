import SwiftUI

/// Frosted action bar for the Restructure tab. `RestructureView` pins it near
/// the TOP of the tab (persistent, above the plan content) whenever a plan is
/// active or pending, so the primary action stays visible without scrolling.
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
    /// Cooperatively stops the in-flight apply/undo. Files already moved stay
    /// moved (each is durable before the engine polls for cancel) and remain
    /// undoable — nil hides the Cancel affordance entirely.
    var onCancel: (() -> Void)? = nil

    @State private var primaryHovered = false
    @State private var cancelRequested = false

    var body: some View {
        HStack(alignment: .center, spacing: 18) {
            selectionSummary
            Spacer(minLength: 16)
            if isApplying, onCancel != nil {
                cancelButton
            }
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
        // Reset for the NEXT apply — otherwise a view-identity-preserving
        // re-render across two separate applies would show a stuck
        // "Stopping…" label from the previous run.
        .onChange(of: isApplying) { _, applying in
            if !applying { cancelRequested = false }
        }
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
    private var cancelButton: some View {
        Button {
            guard !cancelRequested else { return }
            cancelRequested = true
            onCancel?()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "xmark.circle.fill").font(.callout.bold())
                Text(cancelRequested ? "Stopping…" : "Cancel")
                    .font(.callout.weight(.semibold))
            }
            .padding(.horizontal, 14).padding(.vertical, 9)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .stroke(Color.red.opacity(0.55), lineWidth: 1)
            )
            .foregroundStyle(.red)
            .opacity(cancelRequested ? 0.55 : 1.0)
        }
        .buttonStyle(.plain)
        .disabled(cancelRequested)
        .help("Stops after the file currently being moved finishes. Files already moved stay moved and remain undoable.")
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
