import SwiftUI
import FileIDShared

struct ModelCatalogBrowser: View {
    let engine: EngineClient
    var visionOnly = false
    var initialSelection: AIModelKind?
    let onSelect: (AIModelKind) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var role = "all"
    @State private var fitsOnly = true
    @State private var installedOnly = false
    @State private var selection: String?
    @State private var statusMessage = ""
    private var ramGB: Double { Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824 }
    private var selected: AIModelKind? { selection.flatMap(AIModelKind.init(rawValue:)) }
    private var filtered: [AIModelKind] {
        AIModelKind.allCases.filter { kind in
            (!visionOnly || kind.supportsVision) &&
            (role == "all" || (role == "vision") == kind.supportsVision) &&
            (!fitsOnly || kind.fits(ramGB: ramGB)) &&
            (!installedOnly || ModelInstallStatus.isInstalled(kind: kind)) &&
            (query.isEmpty || (kind.displayName + " " + kind.sourceRepo + " " + kind.licenseName).localizedCaseInsensitiveContains(query))
        }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Find a local model").font(Theme.Font.body(20, .semibold))
                    Text("\(MLXModelCatalog.entries.count) audited catalog entries · \(Int(ramGB)) GB Mac")
                        .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
                }
                Spacer()
                Button { dismiss() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.plain).help("Close model browser").accessibilityLabel("Close model browser")
            }
            HStack {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search model, publisher or license", text: $query).textFieldStyle(.plain)
                    .accessibilityLabel("Search models")
            }.padding(10).background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
            HStack {
                if !visionOnly {
                    SegmentedBar(selection: $role, options: [("all", "All"), ("vision", "Vision"), ("text", "Text")], height: 26)
                }
                Spacer()
                Toggle("Fits this Mac", isOn: $fitsOnly)
                Toggle("Downloaded", isOn: $installedOnly)
            }.font(Theme.Font.body(12))
            List(selection: $selection) {
                ForEach(filtered, id: \.rawValue) { kind in
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: kind.supportsVision ? "photo.on.rectangle" : "text.bubble")
                            .foregroundStyle(kind.supportsVision ? Theme.info : Theme.ai).frame(width: 22)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(kind.displayName).font(Theme.Font.body(13.5, .semibold)).lineLimit(1)
                            Text(kind.sourceRepo).font(Theme.Font.body(12)).foregroundStyle(.secondary).lineLimit(1)
                            Text("\(kind.supportsVision ? "Vision + chat" : "Text + chat") · \(kind.licenseName)")
                                .font(Theme.Font.body(11)).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer(minLength: 8)
                        VStack(alignment: .trailing, spacing: 4) {
                            Text(ByteCountFormatter.string(fromByteCount: kind.approxBytes, countStyle: .file)).monospacedDigit()
                            Text(ModelInstallStatus.isInstalled(kind: kind) ? "Downloaded" : "Not downloaded")
                                .foregroundStyle(ModelInstallStatus.isInstalled(kind: kind) ? Theme.info : .secondary)
                            if !kind.fits(ramGB: ramGB) { Text("Needs more memory").foregroundStyle(Theme.warning) }
                            if let decode = engine.benchmarkResults[kind.rawValue]?.generatedTokensPerSecond {
                                Text("\(Int(decode.rounded())) tok/s here").foregroundStyle(Theme.gold).monospacedDigit()
                            }
                        }.font(Theme.Font.body(11))
                    }.padding(.vertical, 6).tag(kind.rawValue)
                }
            }.dsList().listStyle(.inset).overlay {
                if filtered.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: "magnifyingglass").font(Theme.Font.display(28)).foregroundStyle(.secondary)
                        Text("No models match these filters").font(Theme.Font.body(14, .semibold))
                        Button("Reset filters") { query = ""; fitsOnly = false; installedOnly = false; role = "all" }
                    }
                }
            }
            if let selected {
                VStack(alignment: .leading, spacing: 5) {
                    Text(selected.displayName).font(Theme.Font.body(14, .semibold)).lineLimit(2)
                    Text("Estimated memory: \(String(format: "%.1f", selected.ramBudgetGB)) GB. Runtime compatibility is registered; quality and speed vary by model.")
                        .font(Theme.Font.body(12)).foregroundStyle(.secondary)
                    if let entry = selected.catalogEntry {
                        Text("\(entry.quantization.bits)-bit · \(entry.bases.count) audited upstream license\(entry.bases.count == 1 ? "" : "s")")
                            .font(Theme.Font.body(11)).foregroundStyle(.secondary)
                    }
                    ModelBenchmarkPanel(engine: engine, kind: selected)
                }
            }
            if let progress = engine.modelDownloadProgress {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(progress.message).font(Theme.Font.body(12)).lineLimit(2)
                        ProgressView(value: progress.fraction).tint(Theme.gold)
                    }
                    Button("Cancel") { engine.cancelPrewarm() }
                }
            }
            if !statusMessage.isEmpty { Text(statusMessage).font(Theme.Font.body(12)).foregroundStyle(.secondary).textSelection(.enabled) }
            HStack {
                Text("Downloads require your action. Inference stays on this Mac.")
                    .font(Theme.Font.body(11)).foregroundStyle(.secondary)
                Spacer()
                Button(selected.map { ModelInstallStatus.isInstalled(kind: $0) ? "Load model" : "Download & load" } ?? "Download & load") {
                    if let selected {
                        statusMessage = engine.prewarmModel(selected.rawValue) ? "Requested \(selected.displayName). Progress appears here when processing starts." : (ModelLicenseAcceptance.isAccepted(for: selected) ? "The engine is unavailable. Relaunch FileID and try again." : "This model requires accepted source terms before downloading.")
                    }
                }.disabled(selected == nil || selected?.fits(ramGB: ramGB) != true || engine.modelDownloadProgress != nil)
                Button("Use selection") {
                    if let selected { onSelect(selected); dismiss() }
                }.buttonStyle(.dsPrimary).tint(Theme.gold).disabled(selected == nil || selected?.fits(ramGB: ramGB) != true)
            }
        }.padding(24).frame(minWidth: 600, idealWidth: 760, minHeight: 520, idealHeight: 650)
            .onAppear { selection = initialSelection?.rawValue; engine.refreshModelCatalog() }
            .onChange(of: engine.lastErrorSignal) { _, _ in
                if let error = engine.lastError, let selected, error.modelKind == selected.rawValue || error.kind.hasPrefix("prewarm") { statusMessage = error.message }
            }
            .onChange(of: filtered.map(\.rawValue)) { _, visible in
                if let selection, !visible.contains(selection) { self.selection = nil }
            }
            .onExitCommand { dismiss() }
    }
}

/// Magnitude-style score for one model on this Mac.
struct ModelBenchmarkPanel: View {
    let engine: EngineClient
    let kind: AIModelKind

    private var running: Bool { engine.benchmarkModelKind == kind.rawValue }
    private var installed: Bool { ModelInstallStatus.isInstalled(kind: kind) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text("Speed on this Mac").font(Theme.Font.body(12.5, .semibold))
                Spacer()
                if running {
                    Button("Cancel") { engine.cancelBenchmark() }.buttonStyle(DSGhostButtonStyle(color: Theme.text3))
                } else {
                    Button { engine.benchmarkModel(kind) } label: {
                        Label(engine.benchmarkResults[kind.rawValue] == nil ? "Benchmark" : "Run again", systemImage: "speedometer")
                    }
                    .buttonStyle(DSSecondaryButtonStyle(height: 26))
                    .disabled(!installed || engine.benchmarkModelKind != nil || engine.modelDownloadProgress != nil)
                    .help(installed ? "Runs fixed prompts twice on this Mac and keeps the medians. Nothing is downloaded." : "Download the model first. Benchmarks never download.")
                }
            }
            if running, let progress = engine.benchmarkProgress {
                VStack(alignment: .leading, spacing: 4) {
                    GrainProgressBar(progress: progress.total > 0 ? Double(progress.completed) / Double(progress.total) : 0, tone: .ai, height: 5)
                    Text(Self.phaseLabel(progress.phase)).font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
                }
            } else if let result = engine.benchmarkResults[kind.rawValue] {
                Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 4) {
                    GridRow {
                        metric("Cold load", String(format: "%.1f s", result.loadSeconds))
                        metric("Reading", result.promptTokensPerSecond.map { "\(Int($0.rounded())) tok/s" })
                        metric("Writing", result.generatedTokensPerSecond.map { "\(Int($0.rounded())) tok/s" })
                    }
                    GridRow {
                        metric("Per image", result.secondsPerImage.map { String(format: "%.1f s", $0) })
                        metric("Peak memory", result.peakResidentGB.map { String(format: "%.1f GB", $0) })
                        metric("Valid JSON", result.jsonValidRate.map { "\(Int(($0 * 100).rounded()))%" })
                    }
                }
                Text(Self.footnote(result)).font(Theme.Font.body(11)).foregroundStyle(Theme.text3).lineLimit(2)
            } else if let message = engine.benchmarkMessage, message.modelKind == kind.rawValue {
                Text(message.text).font(Theme.Font.body(11.5)).foregroundStyle(Theme.warning).fixedSize(horizontal: false, vertical: true)
            } else {
                Text(installed ? "Not measured yet. A run takes about a minute and loads the model once."
                               : "Download this model to measure it on this Mac.")
                    .font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
            }
        }
        .padding(12)
        .background(Color.white.opacity(0.035), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
        .animation(Theme.quick, value: running)
    }

    @ViewBuilder
    private func metric(_ label: String, _ value: String?) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label).font(Theme.Font.body(11)).foregroundStyle(Theme.text3)
            Text(value ?? "—").font(Theme.Font.body(13, .semibold)).monospacedDigit().foregroundStyle(Theme.text1)
        }
    }

    static func phaseLabel(_ phase: String) -> String {
        switch phase {
        case "load": return "Loading the model from disk"
        case "text": return "Writing structured tags"
        case "rename": return "Suggesting filenames"
        case "vision": return "Describing a test image"
        case "probe": return "Checking what it sees"
        default: return "Finishing"
        }
    }

    static func footnote(_ result: ModelBenchmark) -> String {
        var parts: [String] = []
        if let date = ISO8601DateFormatter().date(from: result.measuredAt) {
            parts.append("Measured " + date.formatted(.relative(presentation: .named)))
        }
        if let probe = result.probeAccuracy { parts.append(probe >= 1 ? "saw the test image correctly" : "misread the test image") }
        parts.append(result.hardware)
        return parts.joined(separator: " · ")
    }
}
