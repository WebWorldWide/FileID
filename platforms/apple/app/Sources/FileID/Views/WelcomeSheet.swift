// First-launch onboarding. Surfaces install controls for CLIP, ArcFace,
// and the recommended VLM in one place. Every model fetches from its
// canonical upstream HuggingFace repo at runtime — no redistribution.
import SwiftUI
import FileIDShared

struct WelcomeSheet: View {
    let engine: EngineClient
    @Environment(\.dismiss) private var dismiss

    @State private var clip = CLIPModelInstaller.shared
    @State private var ramplus = ModelInstaller.ramPlus
    @State private var arcface = ModelInstaller.face
    @State private var bge = ModelInstaller.bge

    private let recommendedFace: FaceEmbedderKind
    private let hardwareRecommendedVLM: AIModelKind
    private let availableDiskBytes: Int64?
    @State private var selectedVLM: AIModelKind

    @State private var vlmRequested = false
    @State private var vlmRequestedAt: Date?
    @State private var vlmLastProgressAt: Date?   // progress-stall watchdog
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
        let freeDisk = (try? FileManager.default.homeDirectoryForCurrentUser
            .resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]))?
            .volumeAvailableCapacityForImportantUsage
        self.recommendedFace = FaceEmbedderKind.defaultFor(ramGB: ram)
        self.availableDiskBytes = freeDisk
        let recommended = AIModelKind.safeDefaultFor(ramGB: ram,
                                                      freeDiskBytes: freeDisk)
        self.hardwareRecommendedVLM = recommended
        self._selectedVLM = State(initialValue: AIModelKind.onboardingSelection(
            persistedRawValue: UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel"),
            ramGB: ram,
            freeDiskBytes: freeDisk))
    }

    private var systemRAMGB: Double {
        Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            SectionLabel("1 · Download the AI models").padding(.horizontal, 22).padding(.top, 12).padding(.bottom, 4)

            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
            modelRow(
                title: "Search by description",
                detail: "Find photos by typing what's in them",
                technical: "CLIP ViT-B/32",
                size: "~\(ByteCountFormatter.string(fromByteCount: Int64(CLIPModelInstaller.approxDownloadBytes), countStyle: .file))",
                status: clip.status,
                action: { clip.install() },
                cancel: { clip.cancel() }
            )
            modelRow(
                title: "Photo tags",
                detail: "Labels what's in each photo",
                technical: "RAM++",
                size: "~\(ByteCountFormatter.string(fromByteCount: ramplus.spec.approxBytes, countStyle: .file))",
                status: ramplus.status,
                action: { ramplus.install() },
                cancel: { ramplus.cancel() }
            )
            modelRow(
                title: "Faces",
                detail: "Groups photos by person",
                technical: recommendedFace.displayName,
                size: "~\(ByteCountFormatter.string(fromByteCount: Int64(recommendedFace.approxBytes), countStyle: .file))",
                status: arcface.status,
                action: { arcface.install() },
                cancel: { arcface.cancel() }
            )
            modelRow(
                title: "Documents",
                detail: "Understands what documents are about",
                technical: "BGE-small",
                size: "~\(ByteCountFormatter.string(fromByteCount: bge.spec.approxBytes, countStyle: .file))",
                status: bge.status,
                action: { bge.install() },
                cancel: { bge.cancel() }
            )
            Menu {
                ForEach(AIModelKind.favorites, id: \.rawValue) { kind in
                    Button {
                        selectedVLM = kind
                    } label: {
                        HStack {
                            Text("\(kind.displayName) · \(kind.licenseName)")
                            if kind == selectedVLM {
                                Image(systemName: "checkmark").accessibilityHidden(true)
                            }
                        }
                    }
                    .accessibilityAddTraits(kind == selectedVLM ? .isSelected : [])
                    .disabled(!vlmChoiceAvailable(kind))
                }
            } label: {
                HStack {
                    Text("Description model")
                    Spacer()
                    Text(selectedVLM.displayName)
                    Image(systemName: "chevron.up.chevron.down")
                }
            }
            .disabled(vlmInProgress)
            .menuStyle(.borderlessButton)
            .font(Theme.Font.body(13.5))
            .padding(.horizontal, 22).padding(.top, 12)
            .accessibilityLabel("Description model")
            .accessibilityValue(selectedVLM.displayName)
            Text(selectedVLM == hardwareRecommendedVLM
                 ? "Recommended for this Mac (\(systemRAMGB.formatted(.number.precision(.fractionLength(0...1)))) GB memory)."
                 : "Your choice. FileID uses it to describe files and answer Chat.")
                .font(Theme.Font.meta)
                .foregroundStyle(Theme.text3)
                .padding(.horizontal, 22).padding(.bottom, 4)
            modelRow(
                title: "Descriptions and Chat",
                detail: "Writes descriptions and answers questions · Optional",
                technical: selectedVLM.displayName,
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
                    // Drop the engine's last mid-download fraction for this model
                    // too — it isn't cleared by a Cancel and would otherwise fool a
                    // fresh retry's watchdogs into thinking a download is underway.
                    engine.clearModelDownloadProgress(forModelKind: selectedVLM.rawValue)
                    engine.cancelPrewarm()
                }
            )
            .disabled(!vlmInstalled && !vlmInProgress && !selectedVLMCanInstall)
            if let selectedVLMInstallBlocker {
                MetaText(selectedVLMInstallBlocker, color: Theme.danger, size: 11).padding(.horizontal, 22).padding(.vertical, 6)
            }
                }
            }
            .frame(maxHeight: 520)

            footer
        }
        .frame(width: 600)
        .background(Color(red: 22/255, green: 22/255, blue: 24/255).opacity(0.94), ignoresSafeAreaEdges: [])
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
        .preferredColorScheme(.dark)
        .onAppear {
            clip.refreshStatus()
            ramplus.refreshStatus()
            arcface.refreshStatus()
            bge.refreshStatus()
        }
        .onDisappear { installAllRequested = false }
        .onChange(of: anyInProgress) { _, inProgress in
            // Once everything "Install all" kicked off has settled without
            // completing the full set — a cancel or a failure leaves models
            // missing and idle — re-enable the button instead of latching it
            // disabled for the rest of the session.
            if !inProgress && !allInstalled { installAllRequested = false }
        }
        .onChange(of: selectedVLM) { _, kind in
            DeepAnalyzeSettings.shared.activeKind = kind
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
                  p.modelKind == selectedVLM.rawValue else { return }
            vlmLastProgressAt = Date()   // Feed the stall watchdog
            updateVLMRate(progress: p)
        }
        // Key on `lastErrorSignal` (bumps on EVERY error write) not the
        // message string, so a retry that fails with the identical message still
        // flips the row to Failed instead of spinning forever.
        .onChange(of: engine.lastErrorSignal) { _, _ in
            // prewarm_cancelled is the engine echoing a user Cancel —
            // local state is already cleared, no error UI needed.
            guard vlmRequested, let err = engine.lastError else { return }
            if err.kind == "prewarm_cancelled" { return }
            // Files already on disk → error is post-download MLX load,
            // not an install failure. Real load issues resurface on
            // first VLM use, where the banner has actual context.
            if ModelInstallStatus.isInstalled(kind: selectedVLM) { return }
            // `unknown_model` is the canonical unrecognized-model-kind error
            // (renamed from the macOS-only `prewarm_invalid_kind` for cross-
            // platform parity,); route it like the prewarm_*
            // family so the row flips to Failed instead of spinning.
            if err.kind.hasPrefix("prewarm_") || err.kind == "unknown_model"
                || err.message.contains(selectedVLM.displayName) {
                vlmLastError = err.message
                vlmRequested = false
            }
        }
    }

    private var header: some View {
        HStack(spacing: 16) {
            Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 52, height: 52)
            VStack(alignment: .leading, spacing: 6) {
                Text("Welcome to FileID").font(Theme.Font.display(34)).foregroundStyle(Theme.text1)
                MetaText("FileID sorts your files with AI that runs on this Mac. The models download once from Hugging Face; after that, nothing leaves your Mac.", color: Theme.text2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 22).padding(.top, 22).padding(.bottom, 18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            MetaText(allInstalled ? "Next: choose a folder to scan." : "You can also download them later in Settings.")
            Spacer()
            Button(allInstalled ? "Continue" : "Later") { dismiss() }
                .buttonStyle(DSSecondaryButtonStyle(height: 32))
                .keyboardShortcut(.cancelAction)
            Button(allInstalled ? "All downloaded" : installAllRequested ? "Downloading…" : "Download all · \(totalDownloadLabel)") {
                installAllRequested = true
                if !clipInstalled, !clipInProgress { clip.install() }
                if !ramplusInstalled, !ramplusInProgress { ramplus.install() }
                if !arcfaceInstalled, !arcfaceInProgress { arcface.install() }
                if !bgeInstalled, !bgeInProgress { bge.install() }
                if !vlmInstalled, !vlmInProgress { triggerVLMInstall() }
            }
            .buttonStyle(DSPrimaryButtonStyle(height: 32))
            .keyboardShortcut(.defaultAction)
            .disabled(allInstalled || installAllRequested
                      || (!vlmInstalled && !selectedVLMCanInstall))
        }
        .padding(.horizontal, 22).padding(.vertical, 16)
        .overlay(alignment: .top) { Hairline() }
    }

    private func modelRow(title: String, detail: String, technical: String, size: String,
                          status: ModelInstaller.Status,
                          action: @escaping () -> Void,
                          cancel: @escaping () -> Void) -> some View {
        modelRow(title: title, detail: detail, technical: technical, size: size,
                 installed: status.isInstalled, inProgress: status.inProgress,
                 progressLabel: status.progressLabel, progressFrac: status.fraction,
                 rateETA: status.rateAndETA, action: action, cancel: cancel)
    }

    @ViewBuilder
    private func modelRow(title: String, detail: String, technical: String, size: String,
                          installed: Bool, inProgress: Bool,
                          progressLabel: String?, progressFrac: Double?,
                          rateETA: String?,
                          action: @escaping () -> Void,
                          cancel: @escaping () -> Void) -> some View {
        let frac = installed ? 1.0 : (progressFrac ?? 0)
        let failed = !inProgress && !installed && progressLabel != nil
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(title).font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1).lineLimit(1)
                    .help(technical)
                Text(detail).font(Theme.Font.body(13)).foregroundStyle(Theme.text2).lineLimit(1).truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if installed {
                    MetaText("Ready", color: Theme.delight, size: 11)
                } else if inProgress {
                    MetaText(progressLabel ?? "\(Int(frac * 100))%", color: Theme.gold, size: 11).lineLimit(1)
                    Button("Cancel", action: cancel).buttonStyle(.dsGhost)
                } else {
                    MetaText(size, size: 11)
                    Button("Download", action: action).buttonStyle(DSSecondaryButtonStyle(height: 24))
                        .help("Download \(technical) (\(size))")
                }
            }
            GrainProgressBar(progress: inProgress && frac == 0 ? 0.03 : frac, tone: installed ? .done : .gold)
            if inProgress, let rateETA, !rateETA.isEmpty { MetaText(rateETA, size: 11) }
            if failed, let progressLabel { MetaText(progressLabel, color: Theme.danger, size: 11) }
        }
        .padding(.horizontal, 22).padding(.vertical, 10)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
    }

    private func triggerVLMInstall() {
        guard !vlmInProgress else { return }
        guard selectedVLMCanInstall else {
            vlmLastError = selectedVLMInstallBlocker ?? "This model cannot be installed safely on this Mac."
            return
        }
        guard ModelLicenseGate.ensureAccepted(for: selectedVLM) else { return }
        resetVLMTracking()
        // Clear any residual progress from a just-cancelled attempt at the same
        // model so both watchdogs below arm against a clean slate — otherwise a
        // lingering fraction makes the 30 s "no response" guard see the old
        // modelKind and bail, and a genuinely-hung retry spins forever.
        engine.clearModelDownloadProgress(forModelKind: selectedVLM.rawValue)
        vlmRequested = true
        let started = Date()
        vlmRequestedAt = started
        DeepAnalyzeSettings.shared.activeKind = selectedVLM
        engine.prewarmModel(selectedVLM.rawValue)
        // If the engine never reports progress, surface a clear error
        // after 30 s rather than spinning forever. A real download
        // sends a fraction event well within that window.
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(30))
            guard vlmRequested,
                  vlmRequestedAt == started,
                  engine.modelDownloadProgress?.modelKind != selectedVLM.rawValue else { return }
            vlmLastError = "No response from engine — try again."
            vlmRequested = false
        }
        // started-then-stalled watchdog. A download that emits ≥1 progress
        // event then goes silent (server hang / dropped link without RST) defeats
        // the 30 s "never started" timer above. Poll for progress staleness.
        Task { @MainActor in
            while vlmRequested, vlmRequestedAt == started {
                try? await Task.sleep(for: .seconds(5))
                guard vlmRequested, vlmRequestedAt == started, !vlmInstalled else { return }
                // Only treat silence as a stall while the DOWNLOAD is
                // still in flight. Once it hits 100%, the post-download MLX cold-load
                // (~10s+, no progress events, sentinel not yet written) is silent by
                // design — without this gate the watchdog false-fired "Download
                // stalled" on a legitimately-loading model. The vlmInstalled sentinel
                // path owns the cold-load phase.
                if vlmLastFraction < 0.999,
                   let last = vlmLastProgressAt,
                   Date().timeIntervalSince(last) > 45 {
                    vlmLastError = "Download stalled — check your connection and try again."
                    vlmRequested = false
                    return
                }
            }
        }
    }

    private var clipInstalled: Bool { clip.status.isInstalled }
    private var clipInProgress: Bool { clip.status.inProgress }
    private var ramplusInstalled: Bool { ramplus.status.isInstalled }
    private var ramplusInProgress: Bool { ramplus.status.inProgress }
    private var bgeInstalled: Bool { bge.status.isInstalled }
    private var bgeInProgress: Bool { bge.status.inProgress }
    private var arcfaceInstalled: Bool { arcface.status.isInstalled }
    private var arcfaceInProgress: Bool { arcface.status.inProgress }

    private var vlmInstalled: Bool {
        ModelInstallStatus.isInstalled(kind: selectedVLM)
    }
    private func vlmChoiceAvailable(_ kind: AIModelKind) -> Bool {
        kind.fits(ramGB: systemRAMGB)
            && (ModelInstallStatus.isInstalled(kind: kind)
                || kind.fits(freeDiskBytes: availableDiskBytes))
    }
    private var selectedVLMCanInstall: Bool {
        vlmChoiceAvailable(selectedVLM)
    }
    private var selectedVLMInstallBlocker: String? {
        guard !vlmInstalled else { return nil }
        if !selectedVLM.fits(ramGB: systemRAMGB) {
            return "\(selectedVLM.displayName) needs more unified memory than this Mac can safely provide. Choose a lighter model."
        }
        if !selectedVLM.fits(freeDiskBytes: availableDiskBytes) {
            let gib = Double(selectedVLM.requiredFreeBytes) / 1_073_741_824.0
            return "Free space on the models drive before installing (about \(gib.formatted(.number.precision(.fractionLength(1)))) GB required while files are verified)."
        }
        return nil
    }
    private var vlmInProgress: Bool {
        guard vlmRequested else { return false }
        if vlmInstalled { return false }
        if let p = engine.modelDownloadProgress, p.modelKind == selectedVLM.rawValue {
            return p.fraction < 1.0
        }
        return true
    }
    private var vlmProgressFrac: Double? {
        guard vlmRequested else { return nil }
        if let p = engine.modelDownloadProgress, p.modelKind == selectedVLM.rawValue {
            return p.fraction
        }
        return nil
    }
    private var vlmProgressLabel: String? {
        if let err = vlmLastError { return "Failed: \(err)" }
        guard vlmRequested else { return nil }
        if let p = engine.modelDownloadProgress, p.modelKind == selectedVLM.rawValue {
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
           p.modelKind == selectedVLM.rawValue,
           p.fraction > 0.05,
           let t = p.totalBytes,
           t >= Int64(Double(selectedVLM.approxBytes) * 0.9) {
            return t
        }
        return selectedVLM.approxBytes
    }

    private var vlmSizeLabel: String {
        let gb = Double(resolvedVLMTotalBytes) / 1_073_741_824.0
        return String(format: "~%.1f GB", gb)
    }

    private var vlmRateETA: String? {
        guard let p = engine.modelDownloadProgress,
              p.modelKind == selectedVLM.rawValue,
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
           let t = p.totalBytes, t > selectedVLM.approxBytes / 2 {
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
        vlmLastProgressAt = nil
        vlmLastError = nil
    }

    private var totalDownloadLabel: String {
        var bytes: Int64 = 0
        if !clipInstalled { bytes += Int64(CLIPModelInstaller.approxDownloadBytes) }
        if !ramplusInstalled { bytes += ramplus.spec.approxBytes }
        if !arcfaceInstalled { bytes += Int64(recommendedFace.approxBytes) }
        if !bgeInstalled { bytes += bge.spec.approxBytes }
        if !vlmInstalled { bytes += resolvedVLMTotalBytes }
        return "~" + ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private var allInstalled: Bool {
        clipInstalled && ramplusInstalled && arcfaceInstalled && bgeInstalled && vlmInstalled
    }

    /// True while any of the three onboarding downloads is still running.
    /// Drives "Install all" re-enablement once a cancel/failure settles
    /// everything back to idle.
    private var anyInProgress: Bool {
        clipInProgress || ramplusInProgress || arcfaceInProgress || bgeInProgress || vlmInProgress
    }
}
