// Review + Settings tabs.
import SwiftUI
import AppKit
import FileIDShared

struct ReviewView: View {
    let engine: EngineClient
    let store: ReadStore
    @State private var sessions: [ReadStore.ScanSessionRow] = []
    @State private var lastSeenBatchIndex: Int = -1

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Review").font(.largeTitle.bold())

                if !engine.queueState.isIdle {
                    GlassCard {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Image(systemName: "list.bullet.rectangle")
                                    .foregroundStyle(Theme.gold)
                                Text("Queue").font(.headline)
                                Spacer()
                                Text("\(engine.queueState.depth) total")
                                    .font(.caption.monospaced()).foregroundStyle(.secondary)
                            }
                            if let r = engine.queueState.running {
                                HStack(spacing: 6) {
                                    Image(systemName: "play.circle.fill").foregroundStyle(.green)
                                    Text("Running:").font(.caption.bold()).foregroundStyle(.secondary)
                                    Text(r.title).font(.callout.monospaced())
                                }
                            }
                            if !engine.queueState.pending.isEmpty {
                                Divider().opacity(0.3)
                                ForEach(engine.queueState.pending) { j in
                                    HStack(spacing: 6) {
                                        Image(systemName: "clock").foregroundStyle(.tertiary)
                                        Text(j.title).font(.caption.monospaced())
                                        Spacer()
                                    }
                                }
                            }
                        }
                    }
                }

                // Live progress (mirrors what the sidebar shows, larger).
                if let p = engine.lastProgress {
                    GlassCard {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("Active scan").font(.headline)
                            Text("\(p.processed) / \(p.total) (\(p.discovered) discovered)")
                                .font(.title2.monospaced())
                            ProgressView(value: Double(p.processed),
                                         total: Double(max(p.total, 1)))
                                .tint(Theme.gold)
                            HStack(spacing: 24) {
                                stat("rate",   String(format: "%.1f files/s", p.filesPerSecond))
                                if let eta = p.etaSeconds, eta > 0 {
                                    stat("ETA",  formatETA(eta))
                                }
                                stat("RSS",    "\(p.residentMB) MB")
                                stat("avail",  "\(p.availableMB) MB")
                                if p.failed > 0 {
                                    stat("failed", "\(p.failed)").foregroundStyle(.red)
                                }
                            }
                        }
                    }
                }

                // Last batch summary (M2 telemetry surfaced).
                if let b = engine.lastBatch {
                    GlassCard {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Last batch").font(.headline)
                            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 4) {
                                GridRow {
                                    Text("Batch #").foregroundStyle(.secondary)
                                    Text("\(b.batchIndex)").font(.body.monospaced())
                                }
                                GridRow {
                                    Text("Files").foregroundStyle(.secondary)
                                    Text("\(b.filesInBatch)").font(.body.monospaced())
                                }
                                GridRow {
                                    Text("Wall").foregroundStyle(.secondary)
                                    Text(String(format: "%.2f s", b.wallSeconds)).font(.body.monospaced())
                                }
                                GridRow {
                                    Text("Insert p50/p95").foregroundStyle(.secondary)
                                    Text(String(format: "%.1f / %.1f ms", b.storeInsertP50Ms, b.storeInsertP95Ms))
                                        .font(.body.monospaced())
                                }
                                GridRow {
                                    Text("RSS / avail").foregroundStyle(.secondary)
                                    Text("\(b.residentMB) MB / \(b.availableMB) MB")
                                        .font(.body.monospaced())
                                }
                            }
                            .font(.callout)
                        }
                    }
                }

                // Recent scan history.
                GlassCard {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Recent scans").font(.headline)
                            Spacer()
                            Text("\(sessions.count)")
                                .font(.caption.monospaced()).foregroundStyle(.secondary)
                        }
                        if sessions.isEmpty {
                            Text("No scans recorded yet.")
                                .font(.callout).foregroundStyle(.secondary)
                        } else {
                            ForEach(sessions) { s in
                                sessionRow(s)
                            }
                        }
                    }
                }
            }
            .padding(24)
        }
        .onAppear {
            store.openIfPossible()
            sessions = store.recentSessions()
        }
        .onChange(of: engine.lastBatch?.batchIndex ?? -1) { _, new in
            if new != lastSeenBatchIndex {
                lastSeenBatchIndex = new
                store.notifyChanged()
                sessions = store.recentSessions()
            }
        }
    }

    @ViewBuilder
    private func sessionRow(_ s: ReadStore.ScanSessionRow) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: s.status == "completed" ? "checkmark.circle.fill"
                              : s.status == "running"  ? "circle.dotted"
                              : "xmark.circle")
                .foregroundStyle(s.status == "completed" ? .green
                                 : s.status == "running"  ? Theme.gold : .red)
            VStack(alignment: .leading, spacing: 2) {
                Text(s.rootPath).font(.callout.monospaced()).lineLimit(1).truncationMode(.middle)
                HStack(spacing: 12) {
                    Text(s.startedAt.formatted(date: .abbreviated, time: .shortened))
                    Text("status: \(s.status)")
                    if let n = s.lastFileIndex { Text("\(n) files") }
                }
                .font(.caption2.monospaced()).foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private func stat(_ k: String, _ v: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(k).font(.caption2).foregroundStyle(.secondary)
            Text(v).font(.callout.monospaced())
        }
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        let h = s / 3600; let m = (s % 3600) / 60; let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }
}

// MARK: - Settings

struct SettingsTab: View {
    let engine: EngineClient
    let store: ReadStore
    @AppStorage(AppSettings.useAIFaceClusteringKey) private var useAIFaceClustering: Bool = AppSettings.useAIFaceClusteringDefault

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Settings").font(.largeTitle.bold())

                GlassCard {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Face Clustering").font(.headline)
                        Toggle(isOn: $useAIFaceClustering) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("Use AI to cluster faces (more accurate)")
                                    .font(.callout)
                                Text("When ON, the People tab's primary action runs the local Vision-Language Model on face crops to merge clusters that look like the same person. When OFF, only the fast L2-distance pass runs — faster but less accurate.")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .toggleStyle(.switch)
                    }
                }

                GlassCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Engine").font(.headline)
                        infoRow("Status", connectionLabel)
                        if case .ready(let info) = engine.state {
                            infoRow("Version", info.version)
                            infoRow("PID",     "\(info.pid)")
                            infoRow("Workers", "\(info.workerCap)")
                            infoRow("Memory",  "\(Int(info.physicalMemoryGB)) GB")
                        }
                        HStack(spacing: 8) {
                            // Manual restart — useful when auto-respawn budget
                            // has been exhausted (state == .crashed) and the
                            // user wants to retry without relaunching the app.
                            Button("Restart Engine") { engine.start() }
                                .buttonStyle(.bordered)
                                .help("Spawn a fresh engine process. Cancels any in-flight scan.")
                            if case .ready = engine.state {
                                Button("Stop Engine") { engine.shutdown() }
                                    .buttonStyle(.bordered)
                                    .help("Cleanly shut down the engine process.")
                            }
                        }
                    }
                }

                GlassCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Storage").font(.headline)
                        infoRow("Total files",   "\(store.totalFiles)")
                        infoRow("Images tagged", "\(store.totalImages)")
                        infoRow("Duplicate groups", "\(store.totalDuplicateGroups)")
                        infoRow("Reclaimable",   String(format: "%.1f MB", store.totalReclaimableMB))
                        Divider().opacity(0.4)
                        infoRow("DB path", ReadStore.defaultDBURL.path)
                        Button("Show DB in Finder") {
                            NSWorkspace.shared.activateFileViewerSelecting([ReadStore.defaultDBURL])
                        }
                        .buttonStyle(.bordered)
                    }
                }

                GlassCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Diagnostics").font(.headline)
                        Text("Per-batch profiler events stream to scan.jsonl. `jq`-queryable. App-side debug events go to app.log.")
                            .font(.callout).foregroundStyle(.secondary)
                        HStack(spacing: 8) {
                            Button("Open scan log") {
                                NSWorkspace.shared.open(SettingsTab.scanLogURL)
                            }
                            .buttonStyle(.bordered)
                            Button("Open app log") {
                                NSWorkspace.shared.open(SettingsTab.appLogURL)
                            }
                            .buttonStyle(.bordered)
                            Button("Show logs in Finder") {
                                NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.scanLogURL])
                            }
                            .buttonStyle(.bordered)
                        }
                    }
                }

                GlassCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("AI Models — fast tier (per-file CLIP)").font(.headline)
                        modelStatusRow(
                            name: "MobileCLIP-S2 (image)",
                            url: SettingsTab.mobileCLIPImageURL
                        )
                        modelStatusRow(
                            name: "MobileCLIP-S2 (text)",
                            url: SettingsTab.mobileCLIPTextURL
                        )
                        Divider().opacity(0.3)
                        Button("Open Models folder") {
                            NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.modelsFolderURL])
                        }
                        .buttonStyle(.bordered)
                    }
                }

                DeepAnalyzeModelPickerCard(engine: engine)
                FaceEmbedderCard(engine: engine, store: store)
            }
            .padding(24)
        }
    }

    private var connectionLabel: String {
        switch engine.state {
        case .starting:           return "Starting…"
        case .ready:              return "Ready"
        case .crashed(let why):   return "Crashed — \(why)"
        }
    }

    @ViewBuilder
    private func infoRow(_ k: String, _ v: String) -> some View {
        HStack(alignment: .top) {
            Text(k).foregroundStyle(.secondary).frame(width: 130, alignment: .leading)
            Text(v).font(.callout.monospaced()).textSelection(.enabled)
            Spacer()
        }
        .font(.callout)
    }

    @ViewBuilder
    private func modelStatusRow(name: String, url: URL) -> some View {
        let installed = FileManager.default.fileExists(atPath: url.path)
        HStack(alignment: .center, spacing: 8) {
            Image(systemName: installed ? "checkmark.circle.fill" : "xmark.circle")
                .foregroundStyle(installed ? .green : .orange)
            VStack(alignment: .leading, spacing: 1) {
                Text(name).font(.callout)
                Text(installed ? "Installed" : "Not downloaded — embeddings disabled until you install via v1's AI Models settings (or drop a .mlpackage at the path below)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Text(url.path)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
        }
    }

    static var modelsFolderURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/Models", isDirectory: true)
    }
    static var mobileCLIPImageURL: URL {
        modelsFolderURL.appendingPathComponent("mobileclip_image/mobileclip_s2_image.mlpackage")
    }
    static var mobileCLIPTextURL: URL {
        modelsFolderURL.appendingPathComponent("mobileclip_text/mobileclip_s2_text.mlpackage")
    }
    static var scanLogURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/logs/scan.jsonl")
    }
    static var appLogURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/logs/app.log")
    }
}

// MARK: - Face embedder card

/// Settings card for the face-recognition tier. Passive install-status
/// display only — the engine picks up whichever .mlpackage is present
/// the next time face clustering runs.
struct FaceEmbedderCard: View {
    let engine: EngineClient
    let store: ReadStore

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — face recognition").font(.headline)
                Text("On-device face embedder for clustering people. Convert from Buffalo (Immich) ONNX once via `scripts/convert_arcface.py`; the engine uses whichever variant is present.")
                    .font(.callout).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                ForEach(FaceEmbedderKind.allCases, id: \.rawValue) { kind in
                    embedderRow(kind)
                }
            }
        }
    }

    @ViewBuilder
    private func embedderRow(_ kind: FaceEmbedderKind) -> some View {
        let installed = kind.isInstalled()
        let path = FaceEmbedderKind.modelsDirectory.appendingPathComponent(kind.modelFileName).path
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: installed ? "checkmark.circle.fill" : "xmark.circle")
                .foregroundStyle(installed ? .green : .orange)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                Text(kind.displayName).font(.callout.bold())
                Text(kind.subtitle).font(.caption).foregroundStyle(.secondary)
                if !installed {
                    Text("Not installed. Run: python3 scripts/convert_arcface.py --variant \(kind == .arcfaceIResNet50 ? "iresnet50" : "mobileface")")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.orange)
                }
                Text(path)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
        }
    }
}
