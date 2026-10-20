// Shared empty / awaiting-data state. Every primary tab gets the same
// visual rhythm: large icon, title2 bold heading, callout body, optional
// primary action below. Single source of truth for empty-state styling
// matches Apple's HIG (consistency across primary destinations).
import SwiftUI

struct EmptyStateView: View {
    let icon: String
    let title: String
    let message: String
    var primaryAction: (label: String, run: () -> Void)? = nil
    /// Optional second line of body text, rendered slightly muted.
    var secondaryMessage: String? = nil

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
                .frame(maxWidth: 480)
                .fixedSize(horizontal: false, vertical: true)
            if let secondary = secondaryMessage {
                Text(secondary)
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text3)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 480)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let action = primaryAction {
                Button(action.label, action: action.run)
                    .buttonStyle(DSPrimaryButtonStyle(height: 32))
                    .padding(.top, 6)
            }
        }
        .padding(.horizontal, 40)
        .padding(.top, 48)
        .frame(maxWidth: .infinity, alignment: .top)
    }
}
