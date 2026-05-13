// Deep Analyze UI: model picker card, dedicated tab, per-file button,
// status bar. Active model selection lives in `DeepAnalyzeSettings`
// (Services/), which the engine reads from UserDefaults at spawn.
import SwiftUI
import AppKit
import FileIDShared

// MARK: - Settings card

struct DeepAnalyzeModelPickerCard: View {
    let engine: EngineClient
    @State private var settings = DeepAnalyzeSettings.shared

    private var ramGB: Double {
        Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
    }
    private var top3: [AIModelKind] {
        AIModelKind.recommendedFor(ramGB: ramGB)
    }

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("AI Models — accuracy tier (Deep Analyze)").font(.headline)
                    Spacer()
                    Text("Recommended for this Mac (\(Int(ramGB)) GB RAM)")
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                }
                Text("On-device AI that reads images and writes captions + smart filenames. Nothing leaves your Mac.")
                    .font(.callout).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                ForEach(top3, id: \.rawValue) { kind in
                    modelOptionRow(kind)
                }
                if let progress = engine.modelDownloadProgress {
                    Divider().opacity(0.3)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(progress.message).font(.caption.bold())
                        ProgressView(value: progress.fraction).tint(Theme.gold)
                    }
                }
                Divider().opacity(0.3)
                DisclosureGroup("Show all available models") {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(AIModelKind.allCases.filter { !top3.contains($0) }, id: \.rawValue) { kind in
                            modelOptionRow(kind)
                        }
                    }
                    .padding(.top, 6)
                }
                .font(.caption)
            }
        }
    }

    @ViewBuilder
    private func modelOptionRow(_ kind: AIModelKind) -> some View {
        ModelOptionRow(
            kind: kind,
            isActive: settings.activeKind == kind,
            installed: ModelInstallStatus.isInstalled(kind: kind),
            fits: kind.fits(ramGB: ramGB),
            ramGB: ramGB,
            onPick: { settings.activeKind = kind }
        )
    }

}

// Extracted from DeepAnalyzeModelPickerCard to keep the type-checker happy.
private struct ModelOptionRow: View {
    let kind: AIModelKind
    let isActive: Bool
    let installed: Bool
    let fits: Bool
    let ramGB: Double
    let onPick: () -> Void

    var body: some View {
        Button {
            guard fits else { return }   // would OOM-kill the engine
            onPick()
        } label: {
            HStack(alignment: .top, spacing: 10) {
                indicatorIcon
                VStack(alignment: .leading, spacing: 2) {
                    titleRow
                    Text(kind.subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    statsLine
                    if !fits {
                        Text("Disabled — would OOM-kill the engine on a \(Int(ramGB)) GB Mac. Pick a smaller model.")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                    }
                }
                Spacer()
            }
            .padding(8)
            .background(rowBackground)
            .overlay(rowBorder)
            .opacity(fits ? 1.0 : 0.55)
        }
        .buttonStyle(.plain)
        .disabled(!fits)
        .help(fits ? "" : "This model needs \(String(format: "%.1f", kind.ramBudgetGB)) GB resident RAM. With your \(Int(ramGB)) GB Mac and the scan engine running, loading it would OOM-kill the engine. Pick a smaller model.")
    }

    @ViewBuilder
    private var indicatorIcon: some View {
        let name: String = {
            if isActive { return "largecircle.fill.circle" }
            return fits ? "circle" : "exclamationmark.triangle.fill"
        }()
        let color: Color = {
            if isActive { return Theme.gold }
            return fits ? .secondary : .orange
        }()
        Image(systemName: name)
            .font(.title3)
            .foregroundStyle(color)
    }

    @ViewBuilder
    private var titleRow: some View {
        HStack(spacing: 6) {
            Text(kind.displayName)
                .font(.callout.bold())
            badgeView
        }
    }

    @ViewBuilder
    private var badgeView: some View {
        if !fits {
            BadgePill(label: "Needs \(Int(kind.ramBudgetGB)) GB RAM (you have \(Int(ramGB)))",
                       color: .orange)
        } else if installed {
            BadgePill(label: "Downloaded", color: .green)
        } else {
            let gb = String(format: "%.1f GB", Double(kind.approxBytes) / 1_073_741_824)
            BadgePill(label: "Will download \(gb)", color: .secondary)
        }
    }

    @ViewBuilder
    private var statsLine: some View {
        Text("≈ \(String(format: "%.1f", kind.ramBudgetGB)) GB RAM · \(String(format: "%.1f", kind.secondsPerImage)) s/image · \(kind.licenseName)")
            .font(.caption2.monospaced())
            .foregroundStyle(.tertiary)
    }

    @ViewBuilder
    private var rowBackground: some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(isActive ? Theme.gold.opacity(0.10) : Color.clear)
    }

    @ViewBuilder
    private var rowBorder: some View {
        let color: Color = {
            if isActive { return Theme.gold.opacity(0.6) }
            return fits ? Color.white.opacity(0.08) : Color.orange.opacity(0.4)
        }()
        RoundedRectangle(cornerRadius: 8)
            .stroke(color, lineWidth: 1)
    }
}

// Sentinel-based install check. The engine writes
// `.fileid-installed` only after ensureLoaded succeeded — i.e. every
// weight shard is on disk AND MLX successfully built a ModelContainer.
// Checking config.json (which Hub creates very early) flips green
// while gigabytes of safetensors are still streaming in.
enum ModelInstallStatus {
    static func isInstalled(kind: AIModelKind) -> Bool {
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
            .appendingPathComponent("huggingface/models", isDirectory: true)
            .appendingPathComponent(kind.sourceRepo, isDirectory: true)
            .appendingPathComponent(".fileid-installed")
        return FileManager.default.fileExists(atPath: url.path)
    }
}

// MARK: - Deep Analyze full-page view

struct DeepAnalyzeView: View {
    let engine: EngineClient
    let store: ReadStore
    var onSwitchTab: (MainWindow.Tab) -> Void = { _ in }
    @State private var settings = DeepAnalyzeSettings.shared
    @State private var skipExisting = true
    @State private var bulkRenameSheetOpen = false
    @State private var pendingRenameCount: Int = 0

    private var pendingTotals: (total: Int, pending: Int) {
        store.deepAnalyzePending(modelKey: settings.activeKind.rawValue)
    }

    /// True when at least one person cluster has been given a name.
    /// Hard requirement before Deep Analyze can run — the VLM uses the
    /// names in captions, and "a person doing X" captions are nearly
    /// useless. User must visit People and name at least one cluster.
    private var hasNamedAnyone: Bool {
        store.namedPersonCount() > 0
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header
                if !engine.deepAnalyzeAvailable {
                    unavailableCard
                }
                statusCard
                actionsCard
                // Smart names produced by Deep Analyze stack up here for
                // bulk apply. Lives in this tab (not Library) because
                // smart names ARE Deep Analyze's output — putting the
                // bulk-rename trigger anywhere else split the workflow.
                if pendingRenameCount > 0 {
                    smartNamesCard
                }
                // Show a "Starting…" card the instant Deep Analyze is
                // requested, even before the first progress event lands.
                // Without this, hitting Skip from the People tab feels
                // like a 10s freeze (the wait for the VLM model to load).
                if engine.deepAnalyzeInFlight, engine.deepAnalyzeProgress == nil {
                    startingCard
                        .animation(.spring(response: 0.35, dampingFraction: 0.78),
                                   value: engine.deepAnalyzeInFlight)
                }
                if engine.deepAnalyzeProgress != nil {
                    progressCard
                }
                if let lastDone = engine.deepAnalyzeComplete {
                    completionCard(lastDone)
                }
                if let lastFile = engine.deepAnalyzeLast {
                    lastFileCard(lastFile)
                }
                Spacer(minLength: 40)
            }
            .padding(24)
        }
        .background(Color.clear)
        .onAppear { refreshPendingRenameCount() }
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            refreshPendingRenameCount()
        }
        .onChange(of: store.version) { _, _ in
            refreshPendingRenameCount()
        }
        .sheet(isPresented: $bulkRenameSheetOpen, onDismiss: refreshPendingRenameCount) {
            BulkRenameSheet(store: store)
        }
    }

    private func refreshPendingRenameCount() {
        pendingRenameCount = store.filesWithProposedNames(limit: 5000).count
    }

    private var smartNamesCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    Image(systemName: "wand.and.rays").foregroundStyle(Theme.gold)
                    Text("Smart names ready").font(.headline)
                    Spacer()
                    Text("\(pendingRenameCount) file\(pendingRenameCount == 1 ? "" : "s")")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                Text("Deep Analyze suggested new filenames for these images. Review and apply them in one batch — original names remain in Finder's metadata until you Apply.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button {
                    bulkRenameSheetOpen = true
                } label: {
                    Label("Review and apply…", systemImage: "wand.and.rays")
                        .font(.callout.bold())
                        .padding(.horizontal, 14).padding(.vertical, 8)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.gold))
                        .foregroundStyle(.black)
                }
                .buttonStyle(.plain)
            }
        }
    }

    private var unavailableCard: some View {
        GlassCard {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.title2)
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Deep Analyze isn't available on this build")
                        .font(.headline)
                    Text(engine.deepAnalyzeUnavailableReason ??
                         "mlx.metallib was not compiled. Run ./run.sh — it will fail with install instructions if cmake or the Metal Toolchain is missing.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
            }
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: "text.below.photo")
                .font(.system(size: 30))
                .foregroundStyle(Theme.gold)
                .frame(width: 36, alignment: .center)
                .padding(.top, 4)
            VStack(alignment: .leading, spacing: 4) {
                Text("Deep Analyze").font(.largeTitle.bold())
                Text("Reads your images with an on-device AI and writes a sentence about each one plus a smart filename. Nothing leaves your Mac.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
    }

    private var statusCard: some View {
        let totals = pendingTotals
        let pendingMins = Double(totals.pending) * settings.activeKind.secondsPerImage / 60.0
        return GlassCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("Library status").font(.headline)
                    Spacer()
                    BadgePill(label: "\(Int(settings.systemRAMGB)) GB Mac",
                               color: .secondary)
                }
                Text("Run a scan first (in the Sidebar). Then come back here — Deep Analyze adds human-readable captions and suggests smart filenames for every image. Without it, files keep their original names.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.bottom, 2)
                Divider().opacity(0.3)
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 4) {
                    GridRow {
                        Text("Active model").foregroundStyle(.secondary)
                        Text(settings.activeKind.displayName).font(.callout.monospaced())
                    }
                    GridRow {
                        Text("Total images").foregroundStyle(.secondary)
                        Text("\(totals.total)").font(.callout.monospaced())
                    }
                    GridRow {
                        Text("Not yet analyzed").foregroundStyle(.secondary)
                        Text("\(totals.pending)").font(.callout.monospaced())
                    }
                    GridRow {
                        Text("Estimated batch time").foregroundStyle(.secondary)
                        Text(formatDuration(seconds: pendingMins * 60))
                            .font(.callout.monospaced())
                    }
                }
            }
        }
    }

    private var actionsCard: some View {
        let activeFits = settings.activeKind.fits(ramGB: settings.systemRAMGB)
        let canRun = engine.deepAnalyzeAvailable && activeFits && hasNamedAnyone
        return GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("Run Deep Analyze").font(.headline)
                // Banner only when there's nothing in flight — once a
                // run is going (e.g. user hit Skip from People), the
                // banner is stale noise; the progress card + Cancel
                // button below are what they need to see.
                if !hasNamedAnyone, !engine.deepAnalyzeInFlight {
                    namingRequiredBanner
                }
                Toggle("Skip files already analyzed by \(settings.activeKind.displayName)",
                       isOn: $skipExisting)
                    .font(.callout)
                HStack(spacing: 10) {
                    Button {
                        engine.deepAnalyzeAll(modelKind: settings.activeKind.rawValue,
                                              skipExisting: skipExisting)
                    } label: {
                        Label("Analyze entire library", systemImage: "wand.and.stars")
                            .padding(.horizontal, 14).padding(.vertical, 8)
                            .background(RoundedRectangle(cornerRadius: 8).fill(canRun ? Theme.gold : Color.gray))
                            .foregroundStyle(.black)
                            .font(.callout.bold())
                    }
                    .buttonStyle(.plain)
                    .disabled(engine.deepAnalyzeInFlight || !canRun)
                    .help(hasNamedAnyone
                          ? "Run the on-device VLM on every image and write captions + smart filenames."
                          : "Name at least one person in the People tab first — captions need real names to be useful.")

                    if engine.deepAnalyzeInFlight {
                        Button("Cancel", role: .destructive) {
                            engine.deepAnalyzeCancel()
                        }
                        .buttonStyle(.bordered)
                    }
                    Spacer()
                    Text("Runs serially on the GPU. Safe to leave overnight — system stays awake.")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                if !activeFits {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                        Text("Active model (\(settings.activeKind.displayName)) needs more RAM than this Mac has. Pick a smaller model in Settings before running.")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
                Divider().opacity(0.3)
                Text("Per-file Deep Analyze: open a file in Library and click the **Deep Analyze** button on the preview toolbar.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    /// Soft-block banner shown when no person has been named yet.
    /// Two paths out: the recommended one (name people, get good
    /// captions) and the escape hatch (skip and run with generic
    /// captions). The escape hatch lives RIGHT NEXT to the recommended
    /// path so the user knows it's an option they're explicitly
    /// choosing, not a hidden default.
    @ViewBuilder
    private var namingRequiredBanner: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "person.crop.circle.badge.exclamationmark.fill")
                .font(.title3)
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 6) {
                Text("Name your people first (recommended)").font(.callout.bold())
                Text("Deep Analyze writes captions like \"Mia playing piano\" — that needs at least one named person. Without names, captions fall back to generic descriptions like \"a person playing piano.\"")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button {
                        onSwitchTab(.people)
                    } label: {
                        Label("Go to People", systemImage: "arrow.right.circle.fill")
                            .font(.caption.bold())
                            .padding(.horizontal, 10).padding(.vertical, 5)
                            .background(Capsule().fill(Theme.gold))
                            .foregroundStyle(.black)
                    }
                    .buttonStyle(.plain)
                    .help("Open the People tab, name the most-photographed faces (~30 seconds), then come back.")

                    Text("or")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)

                    Button {
                        engine.deepAnalyzeAll(modelKind: settings.activeKind.rawValue,
                                              skipExisting: skipExisting)
                    } label: {
                        Label("Skip — run without names", systemImage: "forward.fill")
                            .font(.caption.bold())
                            .padding(.horizontal, 10).padding(.vertical, 5)
                            .background(Capsule().stroke(Color.secondary.opacity(0.6), lineWidth: 1))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .disabled(engine.deepAnalyzeInFlight
                              || !engine.deepAnalyzeAvailable
                              || !settings.activeKind.fits(ramGB: settings.systemRAMGB))
                    .help("Run Deep Analyze right now without naming people. Captions will use generic descriptions (\"a person\", \"two people\") instead of real names. You can run again later after naming.")
                }
                .padding(.top, 2)
            }
            Spacer()
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.orange.opacity(0.12)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.orange.opacity(0.4), lineWidth: 1))
    }

    /// Shown the moment Deep Analyze is kicked off but before the
    /// first per-file progress event arrives. The Qwen / Gemma VLM
    /// container takes ~10s to load on first call; without this card
    /// the user sees an empty page after hitting Skip and assumes
    /// nothing happened. The subtitle is driven by the engine's
    /// `deepAnalyzeStarting` event so it advances "Queued" → "Loading
    /// <model>…" → "Finding files to analyze…" as the runner moves
    /// through each phase.
    @ViewBuilder
    private var startingCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 12) {
                    ProgressView()
                        .controlSize(.regular)
                        .tint(Theme.gold)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Starting Deep Analyze…").font(.headline)
                        Text(startingSubtitle)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .contentTransition(.opacity)
                            .animation(.easeInOut(duration: 0.18),
                                       value: engine.deepAnalyzeStarting?.message)
                    }
                    Spacer()
                    Button("Cancel", role: .destructive) {
                        engine.deepAnalyzeCancel()
                    }
                    .buttonStyle(.bordered)
                }
                ShimmerView(cornerRadius: 3)
                    .frame(height: 4)
            }
        }
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    /// Human-readable subtitle for `startingCard`. Falls back to the
    /// generic "Loading…" copy when the engine hasn't emitted a phase
    /// label yet (e.g. older engine binary).
    private var startingSubtitle: String {
        engine.deepAnalyzeStarting?.message
            ?? "Loading the on-device model. First file usually appears in 5–15 seconds."
    }

    @ViewBuilder
    private var progressCard: some View {
        if let p = engine.deepAnalyzeProgress {
            GlassCard {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Working…").font(.headline)
                        Spacer()
                        if let eta = p.etaSeconds {
                            Text("ETA \(formatDuration(seconds: eta))")
                                .font(.callout.monospaced()).foregroundStyle(Theme.gold)
                        }
                    }
                    ProgressView(value: Double(p.processed),
                                  total: Double(max(p.total, 1)))
                        .tint(Theme.gold)
                    Text("\(p.processed) / \(p.total)")
                        .font(.callout.monospaced())
                    if let path = p.currentPath {
                        Text(URL(fileURLWithPath: path).lastPathComponent)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    // V14.9-L1: live caption stream — engine emits the
                    // partial caption text at 4 Hz as the VLM generates.
                    // Empty/nil while pre-inference; fills word-by-word
                    // once tokens start flowing.
                    if let cap = p.currentCaption, !cap.isEmpty {
                        Text(cap)
                            .font(.callout)
                            .foregroundStyle(.primary)
                            .lineLimit(3)
                            .multilineTextAlignment(.leading)
                            .padding(.top, 4)
                            .animation(.easeInOut(duration: 0.15), value: cap)
                    }
                }
            }
        }
    }

    private func completionCard(_ c: DeepAnalyzeComplete) -> some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Image(systemName: c.cancelled ? "xmark.octagon" : "checkmark.seal.fill")
                        .foregroundStyle(c.cancelled ? .red : .green)
                    Text(c.cancelled ? "Cancelled" : "Last run complete").font(.headline)
                    Spacer()
                }
                Text("\(c.processed) processed · \(c.failed) failed · \(formatDuration(seconds: c.totalSeconds)) wall time")
                    .font(.callout.monospaced()).foregroundStyle(.secondary)
            }
        }
    }

    private func lastFileCard(_ d: DeepAnalyzeFileDone) -> some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 6) {
                Text("Most recent caption").font(.headline)
                Text(d.description).font(.callout)
                if let n = d.proposedName {
                    HStack {
                        Image(systemName: "wand.and.rays").foregroundStyle(Theme.gold)
                        Text("Smart name: ")
                            .font(.caption.bold())
                            .foregroundStyle(.secondary)
                        Text(n).font(.caption.monospaced()).foregroundStyle(Theme.gold)
                    }
                }
            }
        }
    }

    private func formatDuration(seconds: Double) -> String {
        let s = max(0, Int(seconds.rounded()))
        let h = s / 3600
        let m = (s % 3600) / 60
        let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }
}

// MARK: - Per-file button (used in MediaPreviewSheet)

struct DeepAnalyzeButton: View {
    let engine: EngineClient
    let file: FileRow
    @State private var settings = DeepAnalyzeSettings.shared

    var body: some View {
        let alreadyDone = file.vlmDescription != nil
            && file.vlmModel == settings.activeKind.rawValue
        let fits = settings.activeKind.fits(ramGB: settings.systemRAMGB)
        Button {
            guard fits else { return }
            engine.deepAnalyzeFile(fileID: file.id, modelKind: settings.activeKind.rawValue)
        } label: {
            Label(
                alreadyDone ? "Re-analyze" : "Deep Analyze",
                systemImage: engine.deepAnalyzeInFlight ? "hourglass" : "wand.and.stars"
            )
            .font(.system(size: 12, weight: .semibold))
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(RoundedRectangle(cornerRadius: 6).fill(
                fits ? Theme.gold.opacity(alreadyDone ? 0.18 : 0.85) : Color.gray.opacity(0.4)
            ))
            .foregroundStyle(alreadyDone ? Theme.gold : .black)
        }
        .buttonStyle(.plain)
        .disabled(engine.deepAnalyzeInFlight || !fits)
        .help(fits
              ? "Run \(settings.activeKind.displayName) on this image. Caption + suggested filename get added to the metadata panel."
              : "\(settings.activeKind.displayName) needs more RAM than this Mac has. Pick a smaller model in Settings → AI Models.")
    }
}
