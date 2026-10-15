import SwiftUI

/// Prototype v3 atmosphere: warm wax glows rise and sink behind the frosted
/// panes, lit from below. The light turns lavender while a model is running.
/// Modes: full, calm (slower, dimmer wax) and off (static glow only).
enum LavaMode: String, CaseIterable {
    case full, calm, off
    var label: String {
        switch self { case .full: return "Full"; case .calm: return "Calm"; case .off: return "Off" }
    }
}

struct LavaLampBackground: View {
    var paused: Bool = false
    var aiActive: Bool = false
    @AppStorage("lavaMode") private var modeRaw: String = LavaMode.full.rawValue
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var mode: LavaMode { LavaMode(rawValue: modeRaw) ?? .full }

    private struct Blob {
        let x: Double, size: Double, rise: Double, sway: Double, delay: Double
        let color: Color, opacity: Double
    }

    private static let blobs: [Blob] = [
        Blob(x: -0.08, size: 560, rise: 64, sway: 19, delay: -10, color: Color(red: 1, green: 166/255, blue: 36/255), opacity: 0.85),
        Blob(x: 0.16, size: 420, rise: 52, sway: 15, delay: -33, color: Color(red: 1, green: 112/255, blue: 24/255), opacity: 0.8),
        Blob(x: 0.36, size: 620, rise: 72, sway: 21, delay: -48, color: Color(red: 1, green: 196/255, blue: 40/255), opacity: 0.7),
        Blob(x: 0.56, size: 380, rise: 46, sway: 13, delay: -18, color: Color(red: 232/255, green: 72/255, blue: 14/255), opacity: 0.75),
        Blob(x: 0.68, size: 520, rise: 60, sway: 17, delay: -55, color: Color(red: 1, green: 140/255, blue: 28/255), opacity: 0.8),
        Blob(x: 0.84, size: 440, rise: 56, sway: 16, delay: -27, color: Color(red: 1, green: 186/255, blue: 52/255), opacity: 0.7)
    ]

    var body: some View {
        ZStack {
            LinearGradient(stops: [
                .init(color: Theme.bg1, location: 0),
                .init(color: Color(hex: 0x0F0A08), location: 0.42),
                .init(color: Color(hex: 0x1B0D06), location: 0.78),
                .init(color: Color(hex: 0x2A1206), location: 1)
            ], startPoint: .top, endPoint: .bottom)

            GeometryReader { geo in
                let w = geo.size.width, h = geo.size.height
                ZStack {
                    RadialGradient(stops: [
                        .init(color: Color(red: 1, green: 128/255, blue: 32/255).opacity(0.55), location: 0),
                        .init(color: Color(red: 214/255, green: 74/255, blue: 14/255).opacity(0.32), location: 0.38),
                        .init(color: Color(red: 120/255, green: 34/255, blue: 8/255).opacity(0.14), location: 0.68),
                        .init(color: .clear, location: 1)
                    ], center: .center, startRadius: 0, endRadius: max(w, h) * 0.7)
                    .frame(width: w * 1.4, height: h * 0.9)
                    .position(x: w / 2, y: h * 1.0)
                    .opacity(mode == .off ? 0.4 : 1)

                    RadialGradient(stops: [
                        .init(color: Theme.ai.opacity(0.42), location: 0),
                        .init(color: Color(red: 130/255, green: 110/255, blue: 175/255).opacity(0.18), location: 0.5),
                        .init(color: .clear, location: 1)
                    ], center: .center, startRadius: 0, endRadius: max(w, h) * 0.6)
                    .frame(width: w, height: h * 0.8)
                    .position(x: w / 2, y: h * 1.0)
                    .opacity(aiActive && mode != .off ? 1 : 0)
                    .animation(.easeInOut(duration: 2.4), value: aiActive)

                    if mode != .off {
                        AtmosphereTimeline(paused: paused || reduceMotion) { time in
                            Canvas { ctx, size in
                                let slow = mode == .calm ? 1.7 : 1.0
                                ctx.addFilter(.blur(radius: 28))
                                ctx.blendMode = .screen
                                for blob in Self.blobs {
                                    let t = time + blob.delay
                                    let risePhase = sin((t / (blob.rise * slow)) * 2 * .pi) * 0.5 + 0.5
                                    let swayPhase = sin((t / (blob.sway * slow)) * 2 * .pi)
                                    let cx = blob.x * size.width + blob.size / 2 + swayPhase * 16
                                    let cy = size.height + blob.size * 0.1 - risePhase * size.height * 0.8
                                    let r = blob.size / 2
                                    let rect = CGRect(x: cx - r, y: cy - r * 1.1, width: r * 2, height: r * 2.2)
                                    ctx.fill(Path(ellipseIn: rect), with: .radialGradient(
                                        Gradient(stops: [
                                            .init(color: blob.color.opacity(0.9 * blob.opacity), location: 0),
                                            .init(color: blob.color.opacity(0.62 * blob.opacity), location: 0.22),
                                            .init(color: blob.color.opacity(0.32 * blob.opacity), location: 0.48),
                                            .init(color: blob.color.opacity(0.1 * blob.opacity), location: 0.74),
                                            .init(color: blob.color.opacity(0), location: 1)
                                        ]), center: CGPoint(x: cx, y: cy), startRadius: 0, endRadius: r * 1.1))
                                }
                            }
                            .drawingGroup(opaque: false, colorMode: .extendedLinear)
                        }
                        .opacity(mode == .calm ? 0.55 : 1)
                    }
                }
            }

            LinearGradient(stops: [
                .init(color: Theme.bg1.opacity(0.7), location: 0),
                .init(color: Theme.bg1.opacity(0.38), location: 0.3),
                .init(color: Theme.bg1.opacity(0.08), location: 0.58),
                .init(color: .clear, location: 0.8)
            ], startPoint: .top, endPoint: .bottom)

            GrainOverlay().opacity(0.22).blendMode(.overlay)
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}
