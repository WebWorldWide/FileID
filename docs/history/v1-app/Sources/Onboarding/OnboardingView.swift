import SwiftUI

struct OnboardingView: View {
    @AppStorage("hasOnboarded") private var hasOnboarded = false
    @State private var page = 0

    private let copyCards: [OnboardingCard] = [
        OnboardingCard(
            icon:     "folder.badge.magnifyingglass",
            title:    "Pick a Folder",
            body:     "Drag any folder onto the window — or use File → Open Folder. FileID will stream through every image, video, and document without loading them all into memory at once.",
            accent:   Color(red: 1.0, green: 0.8, blue: 0.0)
        ),
        OnboardingCard(
            icon:     "brain",
            title:    "AI Tags Everything Locally",
            body:     "Apple's Neural Engine classifies scenes, detects faces, reads text, and identifies animals — entirely on-device. Your files never leave your Mac.",
            accent:   .cyan
        ),
        OnboardingCard(
            icon:     "checkmark.seal.fill",
            title:    "Review & Apply",
            body:     "FileID proposes renames and a folder structure. You approve each change before anything moves. Undo is always one tap away.",
            accent:   .green
        )
    ]

    private var cardCount: Int { copyCards.count + 1 }

    var body: some View {
        ZStack {
            LavaLampBackground().ignoresSafeArea()

            VStack(spacing: 0) {
                Spacer(minLength: 8)

                ZStack {
                    AIModelsOnboardingCard()
                        .opacity(page == 0 ? 1 : 0)
                        .offset(x: CGFloat(0 - page) * 560)
                        .animation(.spring(response: 0.4, dampingFraction: 0.85), value: page)

                    ForEach(Array(copyCards.enumerated()), id: \.offset) { idx, card in
                        OnboardingCardView(card: card)
                            .opacity(idx + 1 == page ? 1 : 0)
                            .offset(x: CGFloat((idx + 1) - page) * 560)
                            .animation(.spring(response: 0.4, dampingFraction: 0.85), value: page)
                    }
                }
                .frame(height: 440)
                .clipped()

                HStack(spacing: 8) {
                    ForEach(0..<cardCount, id: \.self) { i in
                        Circle()
                            .fill(i == page ? Color(red: 1.0, green: 0.8, blue: 0.0) : Color.white.opacity(0.3))
                            .frame(width: i == page ? 10 : 6, height: i == page ? 10 : 6)
                            .animation(.spring(response: 0.3), value: page)
                    }
                }
                .padding(.top, 16)
                .accessibilityLabel("Page \(page + 1) of \(cardCount)")

                Spacer(minLength: 8)

                HStack(spacing: 16) {
                    if page > 0 {
                        Button("Back") { withAnimation { page -= 1 } }
                            .buttonStyle(.bordered)
                            .accessibilityLabel("Go to previous page")
                            .help("Go to the previous page")
                    } else {
                        Button {
                            withAnimation(.spring()) { hasOnboarded = true }
                        } label: {
                            Text("Skip for now").foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Skip onboarding and open the app now")
                        .help("Skip onboarding and open the app")
                    }

                    Spacer()

                    if page < cardCount - 1 {
                        Button {
                            withAnimation { page += 1 }
                        } label: {
                            Label(page == 0 ? "Continue" : "Next", systemImage: "arrow.right")
                                .fontWeight(.semibold).foregroundStyle(.black)
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        .accessibilityLabel("Go to next page")
                        .help("Continue to the next page")
                    } else {
                        Button {
                            withAnimation(.spring()) { hasOnboarded = true }
                        } label: {
                            Label("Get Started", systemImage: "checkmark")
                                .fontWeight(.bold).foregroundStyle(.black)
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        .accessibilityLabel("Finish onboarding and open the app")
                        .help("Finish onboarding and open the app")
                    }
                }
                .padding(.horizontal, 40)
                .padding(.bottom, 32)
            }
        }
        .frame(width: 620, height: 720)
    }
}

private struct AIModelsOnboardingCard: View {
    var body: some View {
        VStack(spacing: 14) {
            HStack(spacing: 12) {
                ZStack {
                    Circle()
                        .fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.15))
                        .frame(width: 64, height: 64)
                    Image(systemName: "brain.head.profile.fill")
                        .font(.system(size: 28))
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        .symbolRenderingMode(.hierarchical)
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text("Optional AI Models")
                        .font(.system(size: 22, weight: .bold, design: .rounded))
                    Text("Tap Download on any you want now — they run locally, no cloud. Skip anything; FileID works without them.")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.horizontal, 24)

            ScrollView {
                AIModelSetupView(showsHeader: false, compact: false)
                    .padding(.horizontal, 24)
            }
            .frame(maxHeight: 320)
        }
        .padding(.top, 12)
    }
}

private struct OnboardingCard {
    let icon:   String
    let title:  String
    let body:   String
    let accent: Color
}

private struct OnboardingCardView: View {
    let card: OnboardingCard

    var body: some View {
        VStack(spacing: 24) {
            ZStack {
                Circle()
                    .fill(card.accent.opacity(0.15))
                    .frame(width: 100, height: 100)
                Image(systemName: card.icon)
                    .font(.system(size: 44))
                    .foregroundStyle(card.accent)
                    .symbolRenderingMode(.hierarchical)
            }
            .accessibilityHidden(true)

            VStack(spacing: 12) {
                Text(card.title)
                    .font(.system(size: 26, weight: .bold, design: .rounded))
                    .multilineTextAlignment(.center)

                Text(card.body)
                    .font(.system(size: 14))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .lineSpacing(4)
                    .frame(maxWidth: 420)
            }
        }
        .padding(.horizontal, 40)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(card.title). \(card.body)")
    }
}
