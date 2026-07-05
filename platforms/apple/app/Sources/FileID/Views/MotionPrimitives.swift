// Shared motion primitives:
//   ShimmerView         — gold/lavender sweep over a loading placeholder.
// All respect reduceMotion.
import SwiftUI

// MARK: - Shimmer

/// A loading-state placeholder. Renders a soft rounded rectangle with a
/// gold/lavender highlight sweeping diagonally across it. Use as a
/// stand-in for content that's about to arrive (a thumbnail, a caption,
/// a face crop). Subtle; not a literal "loading" word.
struct ShimmerView: View {
    var cornerRadius: CGFloat = 8
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: CGFloat = -1.2

    var body: some View {
        RoundedRectangle(cornerRadius: cornerRadius)
            .fill(Color.white.opacity(0.05))
            .overlay(highlight)
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius))
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(
                    .linear(duration: 1.6).repeatForever(autoreverses: false)
                ) {
                    phase = 1.2
                }
            }
    }

    @ViewBuilder
    private var highlight: some View {
        if reduceMotion {
            EmptyView()
        } else {
            GeometryReader { geo in
                let w = geo.size.width
                LinearGradient(
                    colors: [
                        Color.clear,
                        Theme.gold.opacity(0.18),
                        Theme.ai.opacity(0.18),
                        Color.clear
                    ],
                    startPoint: .leading, endPoint: .trailing
                )
                .frame(width: w * 0.45)
                .offset(x: w * phase)
            }
        }
    }
}
