// First-launch onboarding. Surfaces install controls for CLIP, ArcFace,
// and the recommended VLM in one place. Every model fetches from its
// canonical upstream HuggingFace repo at runtime — no redistribution.
import SwiftUI
import FileIDShared

struct WelcomeSheet: View {
    let engine: EngineClient
    @Environment(\.dismiss) private var dismiss

    @State private var clip = CLIPModelInstaller.shared
    @State private var arcface = ArcFaceModelInstaller.shared

    private let recommendedFace: FaceEmbedderKind
    private let recommendedVLM: AIModelKind

    @State private var vlmRequested = false
    @State private var vlmRequestedAt: Date?
    @State private var installAllRequested = false
    @State private var vlmLockedTotalBytes: Int64?
    @State private var vlmLastError: String?

    @State private var vlmRateSampleAt: TimeInterval = 0
    @State private var vlmRateSampleFrac: Double = 0
    @State private var vlmSmoothedBytesPerSec: Double = 0
    @State private var vlmLastFraction: Double = 0

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
                title: "Semantic search (CLIP ViT-B/32)",
                detail: "Type queries like \"sunset at the beach\" — FileID ranks every photo by visual relevance.",
                size: "~210 MB",
                installed: clipInstalled,
                inProgress: clipInProgress,
                progressLabel: clipProgressLabel,
                progressFrac: clipProgressFrac,
                rateETA: clipRateETA,
                action: { clip.install() },
                cancel: { clip.cancel() }
            )
            modelRow(
                title: "Face recognition (\(recommendedFace.displayName))",
                detail: recommendedFace.subtitle,
                size: "~\(recommendedFace.approxBytes / 1_048_576) MB",
                installed: arcfaceInstalled,
                inProgress: arcfaceInProgress,
                progressLabel: arcfaceProgressLabel,
                progressFrac: arcfaceProgressFrac,
                rateETA: arcfaceRateETA,
                action: { arcface.install(recommendedFace) },
                cancel: { arcface.cancel(recommendedFace) }
            )
            modelRow(
                title: "Deep Analyze (\(recommendedVLM.displayName))",
                detail: "On-device vision model that captions photos, PDFs, video keyframes, and writes smart filenames. Recommended pick for this Mac.",
                size: vlmSizeLabel,
                installed: vlmInstalled,
                inProgress: vlmInProgress,
                progressLabel: vlmProgressLabel,
                progressFrac: vlmProgressFrac,
                rateETA: vlmRateETA,
                action: { triggerVLMInstall() },
                cancel: {
                    // Hide the row before sending the IPC — the cancel
                    // takes ~1 s to land in swift-transformers' fetch
                    // loop, during which stale progress events would
                    // otherwise re-show the spinner.
                    vlmRequested = false
                    resetVLMTracking()
                    engine.cancelPrewarm()
                }
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
        .onDisappear { installAllRequested = false }
        .onChange(of: anyInProgress) { _, inProgress in
            // Once everything "Install all" kicked off has settled without
            // completing the full set — a cancel or a failure leaves models
            // missing and idle — re-enable the button instead of latching it
            // disabled for the rest of the session. (F-C4-017)
            if !inProgress && !allInstalled { installAllRequested = false }
        }
        .onChange(of: vlmInstalled) { _, nowInstalled in
            if nowInstalled {
                vlmRequested = false
                vlmLastError = nil
                resetVLMTracking()
            }
        }
        .onChange(of: allInstalled) { _, nowInstalled in
            guard nowInstalled else { return }
            Task { @MainActor in
                try? await Task.sleep(for: .milliseconds(800))
                if allInstalled { dismiss() }
            }
        }
        .onChange(of: engine.modelDownloadProgress?.fraction ?? -1) { _, _ in
            guard vlmRequested,
                  let p = engine.modelDownloadProgress,
                  p.modelKind == recommendedVLM.rawValue else { return }
            updateVLMRate(progress: p)
        }
        .onChange(of: engine.lastError?.message ?? "") { _, msg in
            // prewarm_cancelled is the engine echoing a user Cancel —
            // local state is already cleared, no error UI needed.
            guard vlmRequested, let err = engine.lastError else { return }
            if err.kind == "prewarm_cancelled" { return }
            // Files already on disk → error is post-download MLX load,
            // not an install failure. Real load issues resurface on
            // first VLM use, where the banner has actual context.
            if ModelInstallStatus.isInstalled(kind: recommendedVLM) { return }
            // `unknown_model` is the canonical unrecognized-model-kind error
            // (renamed from the macOS-only `prewarm_invalid_kind` for cross-
            // platform parity, audit F-C2-003); route it like the prewarm_*
            // family so the row flips to Failed instead of spinning.
            if err.kind.hasPrefix("prewarm_") || err.kind == "unknown_model"
                || msg.contains(recommendedVLM.displayName) {
                vlmLastError = msg
                vlmRequested = false
            }
        }
    }

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
                installAllRequested = true
                if !clipInstalled, !clipInProgress { clip.install() }
                if !arcfaceInstalled, !arcfaceInProgress { arcface.install(recommendedFace) }
                if !vlmInstalled, !vlmInProgress { triggerVLMInstall() }
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.gold)
            .disabled(allInstalled || installAllRequested)

            Button(allInstalled ? "Done" : "Skip for now") { dismiss() }
                .buttonStyle(.bordered)
                .keyboardShortcut(.defaultAction)
        }
    }

    @ViewBuilder
    private func modelRow(title: String, detail: String, size: String,
                          installed: Bool, inProgress: Bool,
                          progressLabel: String?, progressFrac: Double?,
                          rateETA: String?,
                          action: @escaping () -> Void,
                          cancel: @escaping () -> Void) -> some View {
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
                    if let rateETA, !rateETA.isEmpty {
                        Text(rateETA).font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                }
            }
            if installed {
                Text("Installed").font(.caption).foregroundStyle(.green)
            } else if inProgress {
                Button("Cancel", action: cancel)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            } else {
                Button("Install", action: action)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }

    private func triggerVLMInstall() {
        guard !vlmInProgress else { return }
        resetVLMTracking()
        vlmRequested = true
        let started = Date()
        vlmRequestedAt = started
        engine.prewarmModel(recommendedVLM.rawValue)
        // If the engine never reports progress, surface a clear error
        // after 30 s rather than spinning forever. A real download
        // sends a fraction event well within that window.
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(30))
            guard vlmRequested,
                  vlmRequestedAt == started,
                  engine.modelDownloadProgress?.modelKind != recommendedVLM.rawValue else { return }
            vlmLastError = "No response from engine — try again."
            vlmRequested = false
        }
    }

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
        if case .downloading(let frac, _, _, _) = clip.status { return frac }
        return nil
    }
    private var clipProgressLabel: String? {
        switch clip.status {
        case .downloading(_, let msg, _, _): return msg
        case .extracting:                    return "Extracting…"
        case .installFailed(let why):        return "Failed: \(why)"
        default:                             return nil
        }
    }
    private var clipRateETA: String? {
        if case .downloading(_, _, let bps, let eta) = clip.status {
            return DownloadFormat.rateAndETA(DownloadTick(written: 0, total: 0,
                                                           bytesPerSecond: bps,
                                                           etaSeconds: eta))
        }
        return nil
    }

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
        if case .downloading(let frac, _, _, _) = arcface.status[recommendedFace] { return frac }
        return nil
    }
    private var arcfaceProgressLabel: String? {
        switch arcface.status[recommendedFace] {
        case .downloading(_, let msg, _, _): return msg
        case .installFailed(let why):        return "Failed: \(why)"
        default:                             return nil
        }
    }
    private var arcfaceRateETA: String? {
        if case .downloading(_, _, let bps, let eta) = arcface.status[recommendedFace] {
            return DownloadFormat.rateAndETA(DownloadTick(written: 0, total: 0,
                                                           bytesPerSecond: bps,
                                                           etaSeconds: eta))
        }
        return nil
    }

    private var vlmInstalled: Bool {
        ModelInstallStatus.isInstalled(kind: recommendedVLM)
    }
    private var vlmInProgress: Bool {
        guard vlmRequested else { return false }
        if vlmInstalled { return false }
        if let p = engine.modelDownloadProgress, p.modelKind == recommendedVLM.rawValue {
            return p.fraction < 1.0
        }
        return true
    }
    private var vlmProgressFrac: Double? {
        guard vlmRequested else { return nil }
        if let p = engine.modelDownloadProgress, p.modelKind == recommendedVLM.rawValue {
            return p.fraction
        }
        return nil
    }
    private var vlmProgressLabel: String? {
        if let err = vlmLastError { return "Failed: \(err)" }
        guard vlmRequested else { return nil }
        if let p = engine.modelDownloadProgress, p.modelKind == recommendedVLM.rawValue {
            return p.message
        }
        return "Starting…"
    }

    /// Trust engine totalBytes only once meaningful download progress
    /// has accumulated (>5 %) AND the reported total is in the same
    /// ballpark as our estimate (≥ 90 %). swift-transformers' Progress
    /// is per-file, so an early per-file total can be misleading.
    /// Locked once chosen so the size badge can't flicker.
    private var resolvedVLMTotalBytes: Int64 {
        if let locked = vlmLockedTotalBytes { return locked }
        if let p = engine.modelDownloadProgress,
           p.modelKind == recommendedVLM.rawValue,
           p.fraction > 0.05,
           let t = p.totalBytes,
           t >= Int64(Double(recommendedVLM.approxBytes) * 0.9) {
            return t
        }
        return recommendedVLM.approxBytes
    }

    private var vlmSizeLabel: String {
        let gb = Double(resolvedVLMTotalBytes) / 1_073_741_824.0
        return String(format: "~%.1f GB", gb)
    }

    private var vlmRateETA: String? {
        guard let p = engine.modelDownloadProgress,
              p.modelKind == recommendedVLM.rawValue,
              p.fraction > 0, p.fraction < 1.0 else { return nil }
        let total = resolvedVLMTotalBytes
        let written = Int64(Double(total) * p.fraction)
        let tick = DownloadTick(written: written, total: total,
                                 bytesPerSecond: vlmSmoothedBytesPerSec,
                                 etaSeconds: vlmSmoothedBytesPerSec > 0
                                     ? Double(max(0, total - written)) / vlmSmoothedBytesPerSec
                                     : 0)
        return DownloadFormat.rateAndETA(tick)
    }

    /// EMA bandwidth derived from `fraction × resolvedTotal`. Fraction
    /// is aggregate across files; raw byte fields are per-file and
    /// would jump to a tiny total every time a new file starts.
    private func updateVLMRate(progress p: ModelDownloadProgress) {
        if vlmLockedTotalBytes == nil,
           let t = p.totalBytes, t > recommendedVLM.approxBytes / 2 {
            vlmLockedTotalBytes = t
        }
        let now = Date().timeIntervalSinceReferenceDate
        let total = Double(resolvedVLMTotalBytes)
        let frac = p.fraction
        let bytesNow = total * frac
        if vlmRateSampleAt == 0 || frac < vlmLastFraction {
            vlmRateSampleAt = now
            vlmRateSampleFrac = frac
            vlmSmoothedBytesPerSec = 0
            vlmLastFraction = frac
            return
        }
        let dt = now - vlmRateSampleAt
        // Sample at most every 500 ms — first chunks are TCP slow-start.
        if dt < 0.5 {
            vlmLastFraction = frac
            return
        }
        let bytesPrev = total * vlmRateSampleFrac
        let instant = (bytesNow - bytesPrev) / dt
        if vlmSmoothedBytesPerSec == 0 {
            vlmSmoothedBytesPerSec = instant
        } else {
            vlmSmoothedBytesPerSec = 0.7 * vlmSmoothedBytesPerSec + 0.3 * instant
        }
        vlmRateSampleAt = now
        vlmRateSampleFrac = frac
        vlmLastFraction = frac
    }

    private func resetVLMTracking() {
        vlmRateSampleAt = 0
        vlmRateSampleFrac = 0
        vlmSmoothedBytesPerSec = 0
        vlmLastFraction = 0
        vlmLockedTotalBytes = nil
        vlmLastError = nil
    }

    private var allInstalled: Bool {
        clipInstalled && arcfaceInstalled && vlmInstalled
    }

    /// True while any of the three onboarding downloads is still running.
    /// Drives "Install all" re-enablement once a cancel/failure settles
    /// everything back to idle. (F-C4-017)
    private var anyInProgress: Bool {
        clipInProgress || arcfaceInProgress || vlmInProgress
    }
}
