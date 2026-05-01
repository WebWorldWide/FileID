// First-launch welcome sheet. Shown once per install (until the user
// dismisses) and again whenever any of the recommended on-device models
// are missing. Lets the user kick off CLIP + ArcFace + VLM downloads
// with one click each — every model FileID needs is installed from this
// one onboarding surface, not scattered across tabs.
//
// All three categories pull from their canonical upstream HuggingFace
// repos at runtime; FileID never redistributes weights.
import SwiftUI
import FileIDShared

struct WelcomeSheet: View {
    let engine: EngineClient
    @Environment(\.dismiss) private var dismiss

    @State private var clip = CLIPModelInstaller.shared
    @State private var arcface = ArcFaceModelInstaller.shared

    /// Default ArcFace variant — picks based on the user's RAM
    /// (iresnet50 above 16 GB, mobileface below).
    private let recommendedFace: FaceEmbedderKind
    /// Default VLM — picks the largest model that fits in RAM.
    private let recommendedVLM: AIModelKind
    /// Tracks whether the user pressed "Install" for the VLM (so the row
    /// flips to a progress state even before the engine reports its
    /// first download fraction).
    @State private var vlmRequested = false

    init(engine: EngineClient) {
        self.engine = engine
        let ram = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0
        self.recommendedFace = FaceEmbedderKind.defaultFor(ramGB: ram)
        self.recommendedVLM = AIModelKind.safeDefaultFor(ramGB: ram)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            Divider().opacity(0.3)

            modelRow(
                title: "Semantic search (MobileCLIP-S2)",
                detail: "Type queries like \"sunset at the beach\" — FileID ranks every photo by visual relevance.",
                size: "~210 MB",
                installed: clipInstalled,
                inProgress: clipInProgress,
                progressLabel: clipProgressLabel,
                progressFrac: clipProgressFrac,
                action: { clip.install() }
            )
            modelRow(
                title: "Face recognition (\(recommendedFace.displayName))",
                detail: recommendedFace.subtitle,
                size: "~\(recommendedFace.approxBytes / 1_048_576) MB",
                installed: arcfaceInstalled,
                inProgress: arcfaceInProgress,
                progressLabel: arcfaceProgressLabel,
                progressFrac: arcfaceProgressFrac,
                action: { arcface.install(recommendedFace) }
            )
            modelRow(
                title: "Deep Analyze (\(recommendedVLM.displayName))",
                detail: "On-device vision model that captions photos, PDFs, video keyframes, and writes smart filenames. Recommended pick for this Mac.",
                size: "~\(Int(recommendedVLM.ramBudgetGB)) GB",
                installed: vlmInstalled,
                inProgress: vlmInProgress,
                progressLabel: vlmProgressLabel,
                progressFrac: vlmProgressFrac,
                action: { triggerVLMInstall() }
            )

            Spacer(minLength: 4)

            footer
        }
        .padding(28)
        .frame(width: 600)
        .onAppear {
            clip.refreshStatus()
            arcface.refreshStatus()
        }
        .onChange(of: vlmInstalled) { _, nowInstalled in
            if nowInstalled { vlmRequested = false }
        }
    }

    // MARK: - Header / footer

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Welcome to FileID").font(.largeTitle.bold())
            Text("FileID runs entirely on your Mac. Install the on-device models below to enable semantic search, face clustering, and Deep Analyze. Every model downloads from its canonical upstream repository on HuggingFace — FileID never redistributes weights.")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var footer: some View {
        HStack {
            Text("Skip and install later from Settings → AI Models.")
                .font(.caption2).foregroundStyle(.tertiary)
            Spacer()
            Button("Install all") {
                if !clipInstalled, !clipInProgress { clip.install() }
                if !arcfaceInstalled, !arcfaceInProgress { arcface.install(recommendedFace) }
                if !vlmInstalled, !vlmInProgress { triggerVLMInstall() }
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.gold)
            .disabled(allInstalled)

            Button(allInstalled ? "Done" : "Skip for now") { dismiss() }
                .buttonStyle(.bordered)
                .keyboardShortcut(.defaultAction)
        }
    }

    // MARK: - Row builder

    @ViewBuilder
    private func modelRow(title: String, detail: String, size: String,
                          installed: Bool, inProgress: Bool,
                          progressLabel: String?, progressFrac: Double?,
                          action: @escaping () -> Void) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: installed
                  ? "checkmark.seal.fill"
                  : (inProgress ? "arrow.down.circle.fill" : "square.and.arrow.down.on.square"))
                .font(.title2)
                .foregroundStyle(installed ? .green : Theme.gold)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(title).font(.callout.bold())
                    Spacer()
                    Text(size).font(.caption2).foregroundStyle(.secondary)
                }
                Text(detail).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if inProgress {
                    if let frac = progressFrac, frac > 0 {
                        ProgressView(value: frac).tint(Theme.gold)
                    } else {
                        ProgressView().controlSize(.small)
                    }
                    if let label = progressLabel {
                        Text(label).font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                }
            }
            if installed {
                Text("Installed").font(.caption).foregroundStyle(.green)
            } else if !inProgress {
                Button("Install", action: action)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - VLM trigger

    private func triggerVLMInstall() {
        vlmRequested = true
        engine.prewarmModel(recommendedVLM.rawValue)
    }

    // MARK: - CLIP status helpers

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
    private var clipProgressFrac: Double? {
        if case .downloading(let frac, _) = clip.status { return frac }
        return nil
    }
    private var clipProgressLabel: String? {
        switch clip.status {
        case .downloading(_, let msg): return msg
        case .extracting:              return "Extracting…"
        case .installFailed(let why):  return "Failed: \(why)"
        default:                       return nil
        }
    }

    // MARK: - ArcFace status helpers

    private var arcfaceInstalled: Bool {
        if case .installed = arcface.status[recommendedFace] { return true }
        return false
    }
    private var arcfaceInProgress: Bool {
        switch arcface.status[recommendedFace] {
        case .downloading: return true
        default:           return false
        }
    }
    private var arcfaceProgressFrac: Double? {
        if case .downloading(let frac, _) = arcface.status[recommendedFace] { return frac }
        return nil
    }
    private var arcfaceProgressLabel: String? {
        switch arcface.status[recommendedFace] {
        case .downloading(_, let msg): return msg
        case .installFailed(let why):  return "Failed: \(why)"
        default:                       return nil
        }
    }

    // MARK: - VLM status helpers

    private var vlmInstalled: Bool {
        ModelInstallStatus.isInstalled(kind: recommendedVLM)
    }
    private var vlmInProgress: Bool {
        if vlmInstalled { return false }
        if let p = engine.modelDownloadProgress, p.modelKind == recommendedVLM.rawValue {
            return p.fraction < 1.0
        }
        return vlmRequested
    }
    private var vlmProgressFrac: Double? {
        if let p = engine.modelDownloadProgress, p.modelKind == recommendedVLM.rawValue {
            return p.fraction
        }
        return nil
    }
    private var vlmProgressLabel: String? {
        if let p = engine.modelDownloadProgress, p.modelKind == recommendedVLM.rawValue {
            return p.message
        }
        if vlmRequested { return "Starting…" }
        return nil
    }

    private var allInstalled: Bool {
        clipInstalled && arcfaceInstalled && vlmInstalled
    }
}
