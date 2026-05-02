// Shared motion primitives:
//   ShimmerView         — gold/lavender sweep over a loading placeholder.
//   CompletionRipple    — gold ring pulse on successful completion.
//   .iridescentBorder() — thin animated gradient border for hero cards.
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

// MARK: - Completion ripple

/// Gold ring that briefly expands + fades out. Drop on a parent view
/// and trigger by toggling a Bool. Use sparingly — ADA-quality apps
/// don't celebrate every interaction, just the meaningful ones (a scan
/// finishing, a smart-name batch applying, etc.).
struct CompletionRipple: ViewModifier {
    let trigger: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var animate: Bool = false

    func body(content: Content) -> some View {
        content
            .overlay(rippleLayer)
            .onChange(of: trigger) { _, _ in
                // Fire on ANY change — caller can toggle a Bool, increment
                // an Int, or flip any other value to request a ripple.
                // The previous `guard new` filter only allowed false→true
                // transitions, which silently swallowed half the toggles.
                guard !reduceMotion else { return }
                animate = false
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.01) {
                    withAnimation(.easeOut(duration: 0.9)) { animate = true }
                }
            }
    }

    @ViewBuilder
    private var rippleLayer: some View {
        if animate && !reduceMotion {
            GeometryReader { geo in
                Circle()
                    .stroke(Theme.gold, lineWidth: 2)
                    .scaleEffect(animate ? 2.6 : 0.4)
                    .opacity(animate ? 0 : 0.85)
                    .frame(width: geo.size.width, height: geo.size.height)
                    .allowsHitTesting(false)
            }
            .transition(.opacity)
        }
    }
}

extension View {
    func completionRipple(_ trigger: Bool) -> some View {
        modifier(CompletionRipple(trigger: trigger))
    }
}

// MARK: - Iridescent border

/// Animated multi-color border for hero cards. Gold → ai → info → gold
/// drift in a slow cycle. Static gold when reduceMotion is on.
struct IridescentBorder: ViewModifier {
    var cornerRadius: CGFloat = 16
    var lineWidth: CGFloat = 1.5
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var t: CGFloat = 0

    func body(content: Content) -> some View {
        content
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius)
                    .stroke(
                        AngularGradient(
                            colors: [
                                Theme.gold,
                                Theme.delight,
                                Theme.ai,
                                Theme.info,
                                Theme.gold
                            ],
                            center: .center,
                            angle: .degrees(reduceMotion ? 0 : Double(t) * 360)
                        ),
                        lineWidth: lineWidth
                    )
            )
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(
                    .linear(duration: 14).repeatForever(autoreverses: false)
                ) {
                    t = 1
                }
            }
    }
}

extension View {
    func iridescentBorder(cornerRadius: CGFloat = 16,
                           lineWidth: CGFloat = 1.5) -> some View {
        modifier(IridescentBorder(cornerRadius: cornerRadius, lineWidth: lineWidth))
    }
}
