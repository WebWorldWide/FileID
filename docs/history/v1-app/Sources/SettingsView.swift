import SwiftUI

struct SettingsView: View {
    @ObservedObject var viewModel: AppViewModel
    @AppStorage("classificationConfidence") private var confidence: Double = 0.30
    // 0.55 is the new default (down from 0.80) — 0.80 was aggressively
    // over-merging Vision's L2-normalized face prints into only a handful
    // of "mega-identities" covering thousands of unrelated faces. Existing
    // installs with a stored value > 0.75 get reset to 0.55 in
    // FaceClusteringService.loadSettings so the fix rolls out universally.
    @AppStorage("faceClusterThreshold") private var faceThreshold: Double = 0.55
    @AppStorage("batchSize") private var batchSize: Int = 25
    @State private var showUninstallConfirm = false
    
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                HStack {
                    Image(systemName: "gearshape.2.fill")
                        .font(.title)
                        .foregroundStyle(Theme.gold)
                    Text("Settings")
                        .font(.title.bold())
                    Spacer()
                }
                
                SettingsSection(title: "Performance", icon: "bolt.fill") {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Batch Save Interval")
                                .font(.caption.bold())
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text("\(batchSize) files")
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(Theme.gold)
                        }
                        Slider(value: Binding(get: { Double(batchSize) }, set: { batchSize = Int($0) }), in: 10...100, step: 5)
                            .tint(Theme.gold)
                            .help("Files saved per batch — lower = snappier UI, higher = faster throughput")
                        Text("Lower values = more responsive UI, higher values = faster processing")
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                }
                
                SettingsSection(title: "AI Classification", icon: "brain") {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Classification Confidence")
                                .font(.caption.bold())
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text(String(format: "%.0f%%", confidence * 100))
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(Theme.gold)
                        }
                        Slider(value: $confidence, in: 0.1...0.9, step: 0.05)
                            .tint(Theme.gold)
                            .help("Minimum Vision confidence before a label becomes a tag")
                        Text("Lower = more tags (may include false positives). Higher = fewer, more accurate tags.")
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                    
                    Divider()
                    
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Face Clustering Sensitivity")
                                .font(.caption.bold())
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text(String(format: "%.2f", faceThreshold))
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(Theme.gold)
                        }
                        Slider(value: $faceThreshold, in: 0.3...1.0, step: 0.05)
                            .tint(Theme.gold)
                            .help("Face-embedding distance threshold for merging two faces into the same person")
                            .task { await FaceClusteringService.shared.setThreshold(Float(faceThreshold)) }
                            .onChange(of: faceThreshold) { _, v in
                                Task { await FaceClusteringService.shared.setThreshold(Float(v)) }
                            }
                        Text("Lower = stricter (more identities). Higher = looser (fewer identities, over-merges unrelated faces). Recommended: 0.55. Re-run clustering after changing.")
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                }

                SettingsSection(title: "AI Models", icon: "brain.head.profile.fill") {
                    AIModelSetupView(showsHeader: false, compact: false)
                }

                SettingsSection(title: "Deep Analyze", icon: "sparkles.rectangle.stack") {
                    DeepAnalyzeSettingsPanel(viewModel: viewModel)
                }

                SettingsSection(title: "Data", icon: "square.and.arrow.up") {
                    HStack {
                        Button {
                            exportLogs()
                        } label: {
                            Label("Export Logs to File", systemImage: "doc.text.fill")
                        }
                        .buttonStyle(.bordered)
                        .help("Save the in-memory log buffer to a .txt file")

                        Spacer()

                        Text("\(viewModel.logs.count) log entries")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                
                SettingsSection(title: "System", icon: "cpu") {
                    HStack {
                        VStack(alignment: .leading, spacing: Theme.Space.s) {
                            Text("CPU Cores: \(ProcessInfo.processInfo.activeProcessorCount)")
                            Text("RAM: \(ProcessInfo.processInfo.physicalMemory / (1024 * 1024 * 1024)) GB")
                            Text("macOS \(ProcessInfo.processInfo.operatingSystemVersionString)")
                        }
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                        Spacer()
                    }
                }

                SettingsSection(title: "Uninstall", icon: "trash.fill") {
                    UninstallPanel(showConfirm: $showUninstallConfirm)
                }

                SettingsSection(title: "Credits & Licenses", icon: "doc.badge.plus") {
                    VStack(alignment: .leading, spacing: 8) {
                        AttributionRow(
                            who: "MobileCLIP S2",
                            by: "Apple Machine Learning Research",
                            licenseName: "Apple Sample Code License",
                            licenseURL: URL(string: "https://github.com/apple/ml-mobileclip/blob/main/LICENSE_weights_data")!,
                            repoURL: URL(string: "https://huggingface.co/apple/coreml-mobileclip")
                        )
                        AttributionRow(
                            who: "Qwen2.5-VL 3B (4-bit)",
                            by: "Alibaba Cloud / MLX community",
                            licenseName: "Apache License 2.0",
                            licenseURL: URL(string: "https://www.apache.org/licenses/LICENSE-2.0")!,
                            repoURL: URL(string: "https://huggingface.co/mlx-community/Qwen2.5-VL-3B-Instruct-4bit")
                        )
                        AttributionRow(
                            who: "MLX Swift",
                            by: "Apple",
                            licenseName: "MIT License",
                            licenseURL: URL(string: "https://github.com/ml-explore/mlx-swift/blob/main/LICENSE")!,
                            repoURL: URL(string: "https://github.com/ml-explore/mlx-swift")
                        )
                        AttributionRow(
                            who: "Swift Transformers / Jinja",
                            by: "Hugging Face",
                            licenseName: "Apache License 2.0",
                            licenseURL: URL(string: "https://www.apache.org/licenses/LICENSE-2.0")!,
                            repoURL: URL(string: "https://github.com/huggingface/swift-transformers")
                        )
                        Divider()
                        Text("Model weights are downloaded on demand from their official repositories and never bundled with FileID. Downloading a model is treated as acceptance of its license.")
                            .font(.system(size: 10))
                            .foregroundStyle(.tertiary)
                    }
                }
            }
            .padding(Theme.Space.l)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
    }
    
    func exportLogs() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "FileID_Logs_\(Date().formatted(date: .numeric, time: .omitted)).txt"
        panel.allowedContentTypes = [.plainText]
        if panel.runModal() == .OK, let url = panel.url {
            let content = viewModel.logs.joined(separator: "\n")
            try? content.write(to: url, atomically: true, encoding: .utf8)
        }
    }
}

// MARK: - Deep Analyze settings panel

private struct DeepAnalyzeSettingsPanel: View {
    @ObservedObject var viewModel: AppViewModel
    @AppStorage("deepAnalyzeEnabled") private var enabled: Bool = true
    @AppStorage("deepAnalyzeFullSweep") private var fullSweep: Bool = false
    @AppStorage("deepAnalyzeActiveModel") private var activeModelRaw: String = AIModelKind.qwen2VL2B.rawValue
    @AppStorage("autoClusterAfterScan") private var autoCluster: Bool = true
    // "performance" | "balanced" | "gentle" — MediaProcessor reads this to
    // size Deep Analyze chunks and inter-chunk sleeps. See
    // runDeepAnalyzePassIfEnabled for the concrete mappings.
    @AppStorage("deepAnalyzeThrottle") private var throttle: String = "balanced"
    @State private var skippedCount: Int = 0

    // Only VLM kinds installed on disk are eligible. The legacy Qwen kind
    // always shows so the user has at least one fallback even before any
    // model is downloaded.
    private var installedVLMs: [AIModelKind] {
        let installed = AIModelKind.allCases.filter { $0.isVLM && $0.descriptor.isInstalled }
        return installed.isEmpty ? [.qwen2VL2B] : installed
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            // Active model picker. Only installed VLMs appear; if the user
            // hasn't downloaded the new options yet, the picker stays on the
            // default Qwen and points them at AI Models to grab more.
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Deep Analyze model")
                        .font(.system(size: 12, weight: .semibold))
                    Spacer()
                    Picker("", selection: $activeModelRaw) {
                        ForEach(installedVLMs, id: \.rawValue) { k in
                            Text(k.descriptor.displayName).tag(k.rawValue)
                        }
                    }
                    .pickerStyle(.menu)
                    .frame(minWidth: 220, maxWidth: .infinity, alignment: .trailing)
                    .help("Switch the active vision model. Re-loading takes ~10 s the first time after a switch.")
                }
                if installedVLMs.count == 1 {
                    Text("Download Gemma 3, Qwen3-VL, SmolVLM, or PaliGemma in AI Models to expand this list.")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                }
            }

            Divider()

            SettingToggleRow(
                "Auto-analyze documents after scan",
                subtitle: "Runs the active model on PDFs, Office files, and screenshots. Invoices, receipts, and forms get rich captions FileID can use for categorization.",
                isOn: $enabled
            )

            Divider()

            SettingToggleRow(
                "Full Sweep: also analyze photos and videos",
                subtitle: "Adds every photo/video to the Deep Analyze queue after scan. ~1 s/file with the default model — on a 50 K library that's ~14 hours. SmolVLM is ~2× faster, Gemma 3 12B ~3× slower. You can walk away; progress shows in the sidebar. Off by default.",
                isOn: $fullSweep
            )
            .disabled(!enabled)

            Divider()

            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Deep Analyze intensity")
                        .font(.system(size: 12, weight: .semibold))
                    Spacer()
                    Picker("", selection: $throttle) {
                        Text("Gentle").tag("gentle")
                        Text("Balanced").tag("balanced")
                        Text("Performance").tag("performance")
                    }
                    .pickerStyle(.segmented)
                    .frame(maxWidth: 260)
                    .help("Gentle keeps the rest of your Mac responsive. Performance finishes faster but may slow everything else down.")
                }
                Text("Balanced runs 32 files per chunk with a 250 ms pause between chunks. Gentle drops to 16/1 s and skips chunks under memory pressure. Performance is 64/50 ms.")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
            .disabled(!enabled)

            Divider()

            // Face-clustering auto-run toggle + circuit-breaker reset.
            // The toggle is an escape hatch if clustering ever misbehaves;
            // the reset clears the permanent skip-list that accumulates when
            // specific files crash clustering 3 times in a row.
            SettingToggleRow(
                "Auto-cluster faces after scan",
                subtitle: "When a scan completes, automatically group faces into People identities. If you hit a clustering crash that keeps recurring, turn this off and re-enable after updating the app.",
                isOn: $autoCluster
            )

            HStack {
                Text(skippedCount > 0
                     ? "Permanently skipped: \(skippedCount) face print\(skippedCount == 1 ? "" : "s")"
                     : "No crashed face prints on record")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    Task {
                        await ClusterCircuitBreaker.shared.resetSkipList()
                        skippedCount = await ClusterCircuitBreaker.shared.skippedCount()
                    }
                } label: {
                    Label("Reset skip-list", systemImage: "arrow.counterclockwise")
                        .font(.caption)
                }
                .buttonStyle(.bordered)
                .disabled(skippedCount == 0)
                .help("Clear the list of face prints that have been permanently skipped. Use this after fixing the underlying issue so these files are retried on the next scan.")
            }
            .task {
                skippedCount = await ClusterCircuitBreaker.shared.skippedCount()
            }

            // Re-clusters every face print already stored on PersonRecord
            // rows against the current threshold. Used after the 0.80 → 0.55
            // threshold retune to split over-merged identities without a full
            // library rescan. Runs entirely on stored blobs — no Vision,
            // no MLX — so it's Jetsam-safe on 16 GB Macs.
            HStack {
                Text("Recluster faces at current threshold without rescanning the library.")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    Task {
                        viewModel.log("Rebuilding People from stored face prints…")
                        await FaceClusteringService.shared.rebuildPeopleFromStoredPrints()
                        viewModel.log("Rebuild People complete.")
                    }
                } label: {
                    Label("Rebuild People", systemImage: "person.2.crop.square.stack")
                        .font(.caption)
                }
                .buttonStyle(.bordered)
                .disabled(viewModel.isProcessing)
                .help("Re-groups the faces already on disk using the current clustering threshold. Use this if the People tab looks over-merged after an app update. No rescan needed.")
            }

            Divider()

            HStack {
                if viewModel.deepAnalyzeRunning {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Deep Analyze running…")
                            .font(.caption.bold())
                    }
                    Spacer()
                    Button {
                        viewModel.cancelDeepAnalyze()
                    } label: {
                        Label("Cancel", systemImage: "xmark.circle")
                            .font(.caption.bold())
                    }
                    .buttonStyle(.bordered)
                    .help("Cancel the in-flight Deep Analyze pass")
                } else {
                    Button {
                        viewModel.runDeepAnalyzeNow()
                    } label: {
                        Label("Run Deep Analyze on current library",
                              systemImage: "play.fill")
                            .font(.caption.bold())
                    }
                    .buttonStyle(.bordered)
                    .disabled(viewModel.isProcessing || !enabled)
                    .help(viewModel.isProcessing
                          ? "Finish the current scan before running Deep Analyze"
                          : "Re-run Deep Analyze across every file in the library")
                    Spacer()
                    if !DeepAnalyzeService.activeKind.descriptor.isInstalled {
                        Label("\(DeepAnalyzeService.activeKind.descriptor.displayName) not installed",
                              systemImage: "exclamationmark.triangle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
            }
        }
    }
}

private struct UninstallPanel: View {
    @Binding var showConfirm: Bool
    @State private var previewPaths: [URL] = []
    @State private var totalBytes: Int64 = 0
    @State private var lastReport: UninstallService.Report?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Remove FileID data, downloaded AI models, logs, and preferences. The app bundle itself stays on disk — drag it to Trash afterwards to complete removal.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if !previewPaths.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(previewPaths, id: \.self) { url in
                        HStack(spacing: 6) {
                            Image(systemName: "folder.fill")
                                .foregroundStyle(.tertiary)
                                .font(.system(size: 9))
                            Text(url.path.replacingOccurrences(of: NSHomeDirectory(), with: "~"))
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                    }
                }
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(Color.black.opacity(0.25)))
            }

            HStack {
                Button(role: .destructive) {
                    showConfirm = true
                } label: {
                    Label("Uninstall FileID\u{2026}", systemImage: "trash.fill")
                        .font(.caption.bold())
                        .foregroundStyle(.white)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(RoundedRectangle(cornerRadius: 6).fill(Color.red.opacity(0.85)))
                }
                .buttonStyle(.plain)
                .contentShape(Rectangle())
                .disabled(previewPaths.isEmpty)
                .help("Permanently delete FileID data and downloaded models")

                Spacer()

                if totalBytes > 0 {
                    Text(ByteCountFormatter.string(fromByteCount: totalBytes, countStyle: .file))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }

            if previewPaths.isEmpty {
                Text("Nothing to remove — FileID has not stored any data yet.")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }
        }
        .task { refresh() }
        .confirmationDialog(
            "Uninstall FileID?",
            isPresented: $showConfirm,
            titleVisibility: .visible
        ) {
            Button("Uninstall and Quit", role: .destructive) {
                let report = UninstallService.perform()
                lastReport = report
                NSApp.terminate(nil)
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This deletes the SwiftData library, MobileCLIP and Qwen2.5-VL weights (~3+ GB), logs, and saved preferences. FileID will quit. This cannot be undone.")
        }
    }

    private func refresh() {
        previewPaths = UninstallService.preview()
        totalBytes = UninstallService.totalBytes()
    }
}

private struct AttributionRow: View {
    let who: String
    let by: String
    let licenseName: String
    let licenseURL: URL
    let repoURL: URL?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "book.closed.fill")
                .foregroundStyle(Theme.gold)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(who).font(.caption.bold())
                    if who.hasPrefix("Qwen") {
                        Image(systemName: "info.circle")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .help(Self.qwenJustification)
                    }
                }
                Text("by \(by)")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    Link(licenseName, destination: licenseURL)
                        .font(.system(size: 10))
                    if let repoURL {
                        Link("Source", destination: repoURL)
                            .font(.system(size: 10))
                    }
                }
                .foregroundStyle(.tertiary)
            }
            Spacer()
        }
    }

    fileprivate static let qwenJustification = """
Qwen2.5-VL is Alibaba's open-weight vision-language model (Apache 2.0). FileID runs it 100% locally via Apple's MLX framework — no network calls, no telemetry, weights on disk. We chose Qwen2.5-VL 3B because it outperforms same-size alternatives (LLaVA 1.6, Moondream, Phi-3.5-Vision) on document and scene understanding benchmarks (DocVQA, ChartQA, OCRBench). Since inference is fully offline, model country-of-origin does not affect data privacy.
"""
}

struct SettingsSection<Content: View>: View {
    let title: String
    let icon: String
    @ViewBuilder let content: Content
    
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .foregroundStyle(Theme.gold)
                Text(title)
                    .font(.headline)
            }
            
            VStack(alignment: .leading, spacing: 12) {
                content
            }
            .padding(16)
            .background(RoundedRectangle(cornerRadius: 12).fill(.ultraThinMaterial))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(Color.white.opacity(0.08), lineWidth: 1))
        }
    }
}
