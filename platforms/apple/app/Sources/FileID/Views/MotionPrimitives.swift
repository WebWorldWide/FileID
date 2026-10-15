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

    var body: some View {
        AtmosphereTimeline { elapsed in
            let phase = CGFloat(-1.2 + 2.4 * elapsed.truncatingRemainder(dividingBy: 1.6) / 1.6)
            Rectangle()
                .fill(Color.white.opacity(0.05))
                .overlay {
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
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}
