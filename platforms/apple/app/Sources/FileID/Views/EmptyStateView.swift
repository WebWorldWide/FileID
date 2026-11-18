// Shared empty / awaiting-data state used by every primary tab.
import SwiftUI

struct EmptyStateView: View {
    let icon: String
    let title: String
    let message: String
    var primaryAction: (label: String, run: () -> Void)? = nil
    /// Optional second line of body text, rendered slightly muted.
    var secondaryMessage: String? = nil
    /// Shown with a spinner where the action would be, while the work it would start runs.
    var progressLabel: String? = nil

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(Theme.gold.opacity(0.55))
                .accessibilityHidden(true)
            Text(title)
                .font(Theme.Font.body(20, .semibold))
            Text(message)
                .font(Theme.Font.body(13.5))
                .foregroundStyle(Theme.text3)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
                .fixedSize(horizontal: false, vertical: true)
            if let secondary = secondaryMessage {
                Text(secondary)
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text3)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let progressLabel {
                ProgressView(progressLabel)
                    .padding(.top, 6)
            }
            if let action = primaryAction {
                Button(action.label, action: action.run)
                    .buttonStyle(DSPrimaryButtonStyle(height: 32))
                    .padding(.top, 6)
            }
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 24)
        .padding(.bottom, 56)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
