// First-launch welcome sheet. Shown once per install (until the user
// dismisses) and again whenever any of the recommended on-device models
// are missing. Lets the user kick off CLIP + ArcFace downloads with one
// click each — the rest of the app works without them, but People and
// semantic search require these models, so we surface the install as
// part of onboarding instead of hiding it in Settings.
import SwiftUI
import FileIDShared

struct WelcomeSheet: View {
    @Environment(\.dismiss) private var dismiss

    @State private var clip = CLIPModelInstaller.shared
    @State private var arcface = ArcFaceModelInstaller.shared

    /// Default ArcFace variant to install — picks based on the user's
    /// RAM (iresnet50 above 16 GB, mobileface below).
    private let recommendedFace: FaceEmbedderKind

    init() {
        let ram = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0
        self.recommendedFace = FaceEmbedderKind.defaultFor(ramGB: ram)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            header
            Divider().opacity(0.3)
            modelRow(
                title: "Semantic search (MobileCLIP-S2)",
                detail: "Type queries like \"sunset at the beach\" — FileID ranks every photo by visual relevance.",
                size: "~210 MB",
                installed: clipInstalled,
                inProgress: clipInProgress,
                progressLabel: clipProgressLabel,
                action: { clip.install() }
            )
            modelRow(
                title: "Face recognition (\(recommendedFace.displayName))",
                detail: recommendedFace.subtitle,
                size: "~\(recommendedFace.approxBytes / 1_048_576) MB",
                installed: arcfaceInstalled,
                inProgress: arcfaceInProgress,
                progressLabel: arcfaceProgressLabel,
                action: { arcface.install(recommendedFace) }
            )

            Spacer(minLength: 4)

            HStack {
                Text("You can install these later from Settings → AI Models.")
                    .font(.caption2).foregroundStyle(.tertiary)
                Spacer()
                Button("Install both") {
                    if !clipInstalled, !clipInProgress { clip.install() }
                    if !arcfaceInstalled, !arcfaceInProgress { arcface.install(recommendedFace) }
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)
                .disabled(clipInstalled && arcfaceInstalled)

                Button(everythingInstalled ? "Done" : "Skip for now") { dismiss() }
                    .buttonStyle(.bordered)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(28)
        .frame(width: 560)
        .onAppear {
            clip.refreshStatus()
            arcface.refreshStatus()
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Welcome to FileID").font(.largeTitle.bold())
            Text("FileID runs entirely on your Mac. To enable semantic search and face clustering, install these on-device models. Both are optional — the rest of the app works without them.")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private func modelRow(title: String, detail: String, size: String,
                          installed: Bool, inProgress: Bool,
                          progressLabel: String?,
                          action: @escaping () -> Void) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: installed ? "checkmark.seal.fill" : "square.and.arrow.down.on.square")
                .font(.title2)
                .foregroundStyle(installed ? .green : Theme.gold)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(title).font(.callout.bold())
                    Spacer()
                    Text(size).font(.caption2).foregroundStyle(.secondary)
                }
                Text(detail).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let label = progressLabel {
                    Text(label).font(.caption2.monospaced())
                        .foregroundStyle(.tertiary)
                        .padding(.top, 2)
                }
            }
            if installed {
                Text("Installed").font(.caption).foregroundStyle(.green)
            } else if inProgress {
                ProgressView().controlSize(.small)
            } else {
                Button("Install", action: action)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - Status helpers

    private var clipInstalled: Bool {
        if case .installed = clip.status { return true }
        return false
    }
    private var clipInProgress: Bool {
        switch clip.status {
        case .downloading, .extracting: return true
        default: return false
        }
    }
    private var clipProgressLabel: String? {
        switch clip.status {
        case .downloading(_, let msg): return msg
        case .extracting:              return "Extracting…"
        case .installFailed(let why):  return "Failed: \(why)"
        default:                       return nil
        }
    }

    private var arcfaceInstalled: Bool {
        if case .installed = arcface.status[recommendedFace] { return true }
        return false
    }
    private var arcfaceInProgress: Bool {
        switch arcface.status[recommendedFace] {
        case .downloading, .extracting: return true
        default: return false
        }
    }
    private var arcfaceProgressLabel: String? {
        switch arcface.status[recommendedFace] {
        case .downloading(_, let msg): return msg
        case .extracting:              return "Extracting…"
        case .installFailed(let why):  return "Failed: \(why)"
        default:                       return nil
        }
    }

    private var everythingInstalled: Bool { clipInstalled && arcfaceInstalled }
}
