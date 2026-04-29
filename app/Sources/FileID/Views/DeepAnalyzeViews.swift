// Deep Analyze UI: model picker card, dedicated tab, per-file button,
// status bar. Active model persists in UserDefaults; the engine reads
// the same key.
import SwiftUI
import AppKit
import FileIDShared

// MARK: - Active-model singleton (UserDefaults-backed)

@Observable
final class DeepAnalyzeSettings: @unchecked Sendable {
    static let shared = DeepAnalyzeSettings()
    private let key = "deepAnalyzeActiveModel"

    var activeKind: AIModelKind {
        didSet {
            UserDefaults.standard.set(activeKind.rawValue, forKey: key)
        }
    }

    let systemRAMGB: Double

    private init() {
        let ram = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
        self.systemRAMGB = ram
        // Demote a persisted model that no longer fits the RAM tier,
        // and prefer a downloaded model when the persisted one isn't
        // local (MLX HubApi rejects offline fetches when NetworkMonitor
        // misreports connectivity).
        let persisted: AIModelKind? = UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel")
            .flatMap { AIModelKind(rawValue: $0) }
        if let p = persisted, p.fits(ramGB: ram) {
            if ModelInstallStatus.isInstalled(kind: p) {
                self.activeKind = p
            } else {
                let downloaded = AIModelKind.recommendedFor(ramGB: ram)
                    .first(where: { $0.fits(ramGB: ram) && ModelInstallStatus.isInstalled(kind: $0) })
                self.activeKind = downloaded ?? p
            }
        } else {
            self.activeKind = Self.preferredDefault(ramGB: ram)
        }
    }

    /// First downloaded recommendation, else the safest fits-this-Mac pick.
    static func preferredDefault(ramGB: Double) -> AIModelKind {
        for kind in AIModelKind.recommendedFor(ramGB: ramGB) where kind.fits(ramGB: ramGB) {
            if ModelInstallStatus.isInstalled(kind: kind) {
                return kind
            }
        }
        return AIModelKind.safeDefaultFor(ramGB: ramGB)
    }
}

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
                Text("On-demand local VLM. Generates human-readable captions + smart filenames. Privacy-first — nothing leaves the device.")
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

    private func formatBytes(_ b: Int64) -> String {
        let gb = Double(b) / 1_073_741_824
        return String(format: "%.1f GB", gb)
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

// Mirrors the engine's "config.json on disk?" install check.
enum ModelInstallStatus {
    static func isInstalled(kind: AIModelKind) -> Bool {
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
            .appendingPathComponent("huggingface/models", isDirectory: true)
            .appendingPathComponent(kind.sourceRepo, isDirectory: true)
            .appendingPathComponent("config.json")
        return FileManager.default.fileExists(atPath: url.path)
    }
}

// MARK: - Deep Analyze full-page view

struct DeepAnalyzeView: View {
    let engine: EngineClient
    let store: ReadStore
    @State private var settings = DeepAnalyzeSettings.shared
    @State private var skipExisting = true
    @State private var showUnnamedConfirm = false

    private var pendingTotals: (total: Int, pending: Int) {
        store.deepAnalyzePending(modelKey: settings.activeKind.rawValue)
    }

    /// Drives the "name people first" confirm dialog.
    private func hasUnnamedClusters() -> Bool {
        let rows = store.persons()
        guard !rows.isEmpty else { return false }
        return rows.contains { !$0.hasAnyName }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header
                statusCard
                actionsCard
                if engine.deepAnalyzeInFlight || engine.deepAnalyzeProgress != nil {
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
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: "sparkles")
                .font(.system(size: 30))
                .foregroundStyle(Theme.gold)
                .frame(width: 36, alignment: .center)
                .padding(.top, 4)
            VStack(alignment: .leading, spacing: 4) {
                Text("Deep Analyze").font(.title.bold())
                Text("Local VLM. Generates human-readable captions + filename suggestions for your images. Privacy-first — nothing leaves the device.")
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
                Text("Smart names + captions come from THIS step. The basic scan only produces tags + face detection — filenames stay as-is until you run Deep Analyze.")
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
        return GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("Run Deep Analyze").font(.headline)
                Toggle("Skip files already analyzed by \(settings.activeKind.displayName)",
                       isOn: $skipExisting)
                    .font(.callout)
                HStack(spacing: 10) {
                    Button {
                        // For best results, faces should be clustered AND
                        // named before Deep Analyze — the VLM uses names
                        // in captions ("the kid playing piano" → "Mia
                        // playing piano"). Warn the user if they're about
                        // to run with unnamed clusters so they can pause
                        // and name first.
                        if hasUnnamedClusters() {
                            showUnnamedConfirm = true
                        } else {
                            engine.deepAnalyzeAll(modelKind: settings.activeKind.rawValue,
                                                  skipExisting: skipExisting)
                        }
                    } label: {
                        Label("Analyze entire library", systemImage: "wand.and.stars")
                            .padding(.horizontal, 14).padding(.vertical, 8)
                            .background(RoundedRectangle(cornerRadius: 8).fill(activeFits ? Theme.gold : Color.gray))
                            .foregroundStyle(.black)
                            .font(.callout.bold())
                    }
                    .buttonStyle(.plain)
                    .disabled(engine.deepAnalyzeInFlight || !activeFits)
                    .alert("Name your people first?",
                           isPresented: $showUnnamedConfirm) {
                        Button("Cancel — let me name them", role: .cancel) {}
                        Button("Run anyway") {
                            engine.deepAnalyzeAll(modelKind: settings.activeKind.rawValue,
                                                  skipExisting: skipExisting)
                        }
                    } message: {
                        Text("Face clustering has run, but some people clusters don't have names yet. Captions will use generic descriptions like \"a person\" instead of real names. Naming a few of the most-photographed people in the People tab takes ~30 seconds and makes captions much more useful.")
                    }

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
                        Text("Suggested name: ")
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
