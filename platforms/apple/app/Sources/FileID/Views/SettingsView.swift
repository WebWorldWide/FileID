// Settings tab. (Review tab folded into Settings → Advanced.)
import SwiftUI
import AppKit
import UniformTypeIdentifiers
import FileIDShared

// MARK: - Settings

struct SettingsTab: View {
    let engine: EngineClient
    let store: ReadStore
    @AppStorage(AppSettings.cleanupAutoTagKey) private var cleanupAutoTag: Bool = AppSettings.cleanupAutoTagDefault
    @AppStorage(AppSettings.detailedScanTagsKey) private var detailedScanTags: Bool = AppSettings.detailedScanTagsDefault
    @AppStorage(AppSettings.restructureGranularityKey) private var restructureGranularity: String = AppSettings.restructureGranularityDefault
    @State private var showAdvanced = false
    @State private var sessions: [ReadStore.ScanSessionRow] = []
    @State private var confirmFactoryReset = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Settings").font(.largeTitle.bold())

                // ─── User-facing settings (always visible) ───────────────

                GlassCard(fillsWidth: true) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Scan performance").font(.headline)
                        Toggle(isOn: $detailedScanTags) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("Detailed RAM++ tags during scans")
                                    .font(.callout)
                                Text("Off keeps scans fast with Apple's built-in on-device classifier. Turn on for the richer 4,585-label RAM++ model; it uses substantially more memory and can make large photo scans much slower. Takes effect after restarting the engine.")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .toggleStyle(.switch)
                    }
                }

                GlassCard(fillsWidth: true) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Cleanup").font(.headline)
                        Toggle(isOn: $cleanupAutoTag) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("Tag kept files after Cleanup")
                                    .font(.callout)
                                Text("When ON, after you trash duplicates the surviving keepers get a Finder tag (\"\(AppSettings.cleanupAutoTagName)\"). Useful for finding files you've already deduped via a Finder Smart Folder.")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .toggleStyle(.switch)
                    }
                }

                GlassCard(fillsWidth: true) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Restructure").font(.headline)
                        Picker("Folder granularity", selection: $restructureGranularity) {
                            Text("Looser").tag("loose")
                            Text("Balanced").tag("normal")
                            Text("Tighter").tag("tight")
                        }
                        .pickerStyle(.segmented)
                        Text("How finely Restructure splits your files into folders — Looser groups broadly into fewer folders, Tighter makes more, smaller ones. Takes effect the next time the engine starts (relaunch FileID, or Settings ▸ Advanced ▸ Restart Engine).")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }

                DeepAnalyzeExclusionsCard()

                // AI Models — visible because users genuinely care about
                // which models are installed and download status.
                CLIPSemanticSearchCard()
                RamPlusTaggerCard()
                BGEDocCard()

                DeepAnalyzeModelPickerCard(engine: engine)
                FaceEmbedderCard(engine: engine, store: store)

                // ─── Advanced (collapsed by default) ─────────────────────
                // Engine PIDs, DB paths, log files. Power-user info that
                // doesn't help a casual user choose anything; hiding it
                // declutters the page.

                GlassCard(fillsWidth: true) {
                    DisclosureGroup(isExpanded: $showAdvanced) {
                        VStack(alignment: .leading, spacing: 16) {
                            Divider().opacity(0.3)

                            // Engine
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Engine").font(.subheadline.bold())
                                infoRow("Status", connectionLabel)
                                if case .ready(let info) = engine.state {
                                    infoRow("Version", info.version)
                                    infoRow("PID",     "\(info.pid)")
                                    infoRow("Workers", "\(info.workerCap)")
                                    infoRow("Memory",  "\(Int(info.physicalMemoryGB)) GB")
                                }
                                HStack(spacing: 8) {
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

                            Divider().opacity(0.3)

                            // Storage
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Storage").font(.subheadline.bold())
                                infoRow("Total files",   "\(store.totalFiles)")
                                infoRow("Images tagged", "\(store.totalImages)")
                                infoRow("Stored duplicate hints", "\(store.totalDuplicateGroups)")
                                infoRow("Hint reclaimable", String(format: "%.1f MB", store.totalReclaimableMB))
                                infoRow("Database", ReadStore.defaultDBURL.path)
                                Button("Show database in Finder") {
                                    NSWorkspace.shared.activateFileViewerSelecting([ReadStore.defaultDBURL])
                                }
                                .buttonStyle(.bordered)
                            }

                            Divider().opacity(0.3)

                            // Recent scans (folded in from former Review tab)
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Recent scans").font(.subheadline.bold())
                                if sessions.isEmpty {
                                    Text("No scans recorded yet.")
                                        .font(.caption).foregroundStyle(.secondary)
                                } else {
                                    ForEach(sessions) { s in
                                        sessionRow(s)
                                    }
                                }
                            }

                            Divider().opacity(0.3)

                            // Logs
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Logs").font(.subheadline.bold())
                                Text("Detailed scan + app logs for troubleshooting.")
                                    .font(.caption).foregroundStyle(.secondary)
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

                            Divider().opacity(0.3)

                            // Danger Zone
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Danger Zone").font(.subheadline.bold()).foregroundStyle(.red)
                                Text("Permanently erase FileID's library, local models, settings, and caches.")
                                    .font(.caption).foregroundStyle(.secondary)
                                Button(role: .destructive) {
                                    confirmFactoryReset = true
                                } label: {
                                    Text("Factory Reset & Quit")
                                }
                                .buttonStyle(.borderedProminent)
                                .tint(.red)
                                .confirmationDialog(
                                    "Are you sure you want to completely erase FileID?",
                                    isPresented: $confirmFactoryReset,
                                    titleVisibility: .visible
                                ) {
                                    Button("Erase Everything and Quit", role: .destructive) {
                                        engine.factoryResetAndQuit()
                                    }
                                    Button("Cancel", role: .cancel) { }
                                } message: {
                                    Text("This will permanently delete the database, all tags, faces, settings, FileID-managed models, and caches. Shared Deep Analyze model downloads are kept. This action cannot be undone.")
                                }
                            }
                        }
                        .padding(.top, 8)
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "wrench.and.screwdriver")
                                .foregroundStyle(.secondary)
                            Text("Advanced").font(.headline)
                            Text("(engine status, database, scan history, logs)")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                        }
                    }
                }
            }
            .padding(24)
        }
        .onAppear {
            Task { sessions = store.recentSessions() }
        }
        .onChange(of: showAdvanced) { _, expanded in
            if expanded { Task { sessions = store.recentSessions() } }
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
                Text(s.rootPath).font(.caption.monospaced()).lineLimit(1).truncationMode(.middle)
                HStack(spacing: 12) {
                    Text(s.startedAt.formatted(date: .abbreviated, time: .shortened))
                    Text(s.status)
                    if let n = s.lastFileIndex { Text("\(n) files") }
                }
                .font(.caption2.monospaced()).foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.vertical, 1)
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

    static var modelsFolderURL: URL { AppSupportPath.models }
    static var scanLogURL: URL {
        AppSupportPath.fileID.appendingPathComponent("logs/scan.jsonl")
    }
    static var appLogURL: URL {
        AppSupportPath.fileID.appendingPathComponent("logs/app.log")
    }
}

// MARK: - Deep Analyze exclusions card

/// Folders to skip during a whole-library Deep Analyze pass — separate from
/// the (currently unsurfaced) scan-exclusion list: a folder can be fine to
/// catalog/tag/search but too slow or private to run the VLM over. Nothing
/// is removed from the library, so unlike the model-install cards above
/// there's no purge-in-flight state to track — just persist the list; it
/// takes effect starting with the next whole-library Deep Analyze run.
struct DeepAnalyzeExclusionsCard: View {
    @State private var settings = DeepAnalyzeSettings.shared
    @State private var message: (text: String, isError: Bool)?

    var body: some View {
        GlassCard(fillsWidth: true) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Deep Analyze exclusions").font(.headline)
                Text("FileID skips these folders when running Deep Analyze over your whole library. Files stay in the library and search normally — only the VLM pass (captions, smart renames, tags) is skipped. Selecting specific files to analyze always ignores this list.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Divider().opacity(0.3)

                if let message {
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: message.isError ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                            .foregroundStyle(message.isError ? .red : .green)
                        Text(message.text)
                            .font(.caption)
                            .foregroundStyle(message.isError ? .red : .secondary)
                        Spacer()
                    }
                }

                if settings.excludedFolders.isEmpty {
                    Text("No folders are excluded from Deep Analyze.")
                        .font(.caption2).foregroundStyle(.secondary)
                } else {
                    ForEach(settings.excludedFolders, id: \.self) { folder in
                        HStack(spacing: 8) {
                            Text(folder)
                                .font(.caption.monospaced())
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            Button {
                                settings.removeExcludedFolder(folder)
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                            }
                            .buttonStyle(.borderless)
                            .foregroundStyle(.secondary)
                            .help("Stop excluding \(folder) from Deep Analyze")
                        }
                    }
                }

                Button("Add folder…") { pickExcludedFolder() }
                    .buttonStyle(.bordered)
            }
        }
    }

    private func pickExcludedFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Choose a folder to exclude from Deep Analyze"
        panel.prompt = "Exclude"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        switch settings.addExcludedFolder(url.path) {
        case .added:
            message = ("Excluded. Deep Analyze will skip this folder starting with the next whole-library run.", false)
        case .alreadyExcluded:
            message = ("That folder is already excluded from Deep Analyze.", false)
        case .invalid:
            message = ("Couldn't exclude that folder — pick a folder on this Mac.", true)
        }
    }
}

// MARK: - CLIP semantic-search card

/// Settings card for the CLIP semantic-search tier. State-driven —
/// shows install status, download/extract progress, and the manual
/// "install from local zip" fallback.
struct CLIPSemanticSearchCard: View {
    @State private var installer = CLIPModelInstaller.shared
    @State private var confirmUninstall = false

    private var downloadSizeLabel: String {
        ByteCountFormatter.string(
            fromByteCount: CLIPModelInstaller.approxDownloadBytes,
            countStyle: .file)
    }

    var body: some View {
        GlassCard(fillsWidth: true) {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — semantic search (CLIP)").font(.headline)
                Text("Type natural-language searches like \"sunset at the beach\" and FileID ranks every photo by visual relevance. Uses OpenCLIP ViT-B/32 — runs entirely on your Mac.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Divider().opacity(0.3)

                // Per-file install state.
                fileStatusRow(
                    name: "CLIP ViT-B/32 (image)",
                    url: CLIPModelInstaller.modelsRoot
                        .appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx")
                )
                fileStatusRow(
                    name: "CLIP ViT-B/32 (text)",
                    url: CLIPTextEncoder.defaultModelURL
                )
                fileStatusRow(
                    name: "BPE vocabulary (vocab.json + merges.txt)",
                    url: CLIPTextEncoder.defaultDirectory
                        .appendingPathComponent("vocab.json")
                )

                Divider().opacity(0.3)

                // State-aware footer.
                statusFooter
            }
        }
        .onAppear { installer.refreshStatus() }
        .confirmationDialog(
            "Remove CLIP models?",
            isPresented: $confirmUninstall,
            titleVisibility: .visible
        ) {
            Button("Remove", role: .destructive) {
                Task {
                    await installer.uninstall()
                    // Drop the in-memory text encoder too — otherwise semantic
                    // search keeps running against the model we just deleted until
                    // the next app launch, with no fallback. (F-C4-017)
                    CLIPTextEncoder.shared.unload()
                }
            }
            Button("Keep", role: .cancel) {}
        } message: {
            Text("Frees approximately \(downloadSizeLabel). Semantic search will revert to keyword search until you reinstall.")
        }
    }

    @ViewBuilder
    private var statusFooter: some View {
        switch installer.status {
        case .unknown:
            ProgressView().controlSize(.small)
        case .missing(let reason):
            HStack(spacing: 8) {
                Image(systemName: "arrow.down.circle")
                    .foregroundStyle(Theme.gold)
                VStack(alignment: .leading, spacing: 2) {
                    Text(reason).font(.caption2).foregroundStyle(.secondary)
                    Text("Approximately \(downloadSizeLabel) from huggingface.co (Xenova CLIP ViT-B/32 + OpenAI BPE vocabulary).")
                        .font(.caption2).foregroundStyle(.tertiary)
                }
                Spacer()
            }
            HStack(spacing: 8) {
                Button {
                    installer.install()
                } label: {
                    Label("Download", systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)

                Button("Install from local zip…") { pickLocalZip() }
                    .buttonStyle(.bordered)

                Button("Open Models folder") {
                    NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.modelsFolderURL])
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                Spacer()
            }

        case .downloading(let frac, let msg, let bps, let eta):
            VStack(alignment: .leading, spacing: 6) {
                if frac > 0 {
                    ProgressView(value: frac)
                } else {
                    ProgressView()
                }
                HStack {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(msg).font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                        let rateETA = DownloadFormat.rateAndETA(
                            DownloadTick(written: 0, total: 0,
                                          bytesPerSecond: bps, etaSeconds: eta))
                        if !rateETA.isEmpty {
                            Text(rateETA).font(.caption2.monospaced())
                                .foregroundStyle(.tertiary)
                        }
                    }
                    Spacer()
                    Button("Cancel") { installer.cancel() }
                        .buttonStyle(.borderless)
                        .controlSize(.small)
                }
            }

        case .extracting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Extracting…").font(.caption).foregroundStyle(.secondary)
                Spacer()
            }

        case .installed(let bytes):
            HStack(spacing: 8) {
                Image(systemName: "checkmark.seal.fill")
                    .foregroundStyle(.green)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Installed").font(.callout.bold())
                    Text("\(bytes / 1_048_576) MB on disk · semantic search active.")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Open Models folder") {
                    NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.modelsFolderURL])
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                Button("Uninstall") { confirmUninstall = true }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .foregroundStyle(.red)
            }

        case .installFailed(let why):
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                    Text(why).font(.caption)
                        .foregroundStyle(.red)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer()
                }
                HStack(spacing: 8) {
                    Button("Retry") { installer.install() }
                        .buttonStyle(.bordered)
                    Button("Install from local zip…") { pickLocalZip() }
                        .buttonStyle(.bordered)
                    Button("Open Models folder") {
                        NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.modelsFolderURL])
                    }
                    .buttonStyle(.borderless)
                    Spacer()
                }
            }
        }
    }

    @ViewBuilder
    private func fileStatusRow(name: String, url: URL) -> some View {
        let installed = installer.presentFilePaths.contains(url.path)
        HStack(alignment: .center, spacing: 8) {
            Image(systemName: installed ? "checkmark.circle.fill" : "circle.dashed")
                .foregroundStyle(installed ? .green : .secondary)
            VStack(alignment: .leading, spacing: 1) {
                Text(name).font(.callout)
                Text(url.path)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
        }
    }

    private func pickLocalZip() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.zip]
        panel.allowsMultipleSelection = false
        panel.message = "Choose the clip-models.zip file."
        panel.prompt = "Install"
        if panel.runModal() == .OK, let url = panel.url {
            installer.installFromLocalZip(url)
        }
    }
}

// MARK: - Face embedder card

/// Settings card for the face-recognition tier. Per-variant install
/// state with Download/Uninstall buttons. The engine picks up whichever
/// .mlpackage is on disk the next time face clustering runs.
// AI Models — RAM++ tagger (macOS lockstep). Single-model card; the engine's
// RamPlusService reads whatever this installs when detailed scan tags are on
// and otherwise uses the lighter Vision classifier.
struct RamPlusTaggerCard: View {
    @State private var installer = RamPlusModelInstaller.shared
    @State private var confirmUninstall = false

    private var modelPath: String {
        RamPlusModelInstaller.modelsRoot.appendingPathComponent("ram_plus/ram_plus.onnx").path
    }

    var body: some View {
        GlassCard(fillsWidth: true) {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — image tagging").font(.headline)
                Text("RAM++ recognizes 4,585 everyday tags on-device (richer than the built-in classifier). Apache-2.0; install with one click, no Python required. Enable Detailed RAM++ tags under Scan performance to use it during scans.")
                    .font(.callout).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                HStack(alignment: .top, spacing: 8) {
                    statusIcon.padding(.top, 2)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("RAM++ (Recognize Anything Plus)").font(.callout.bold())
                        Text("Swin-Large @384 · 4585-tag English vocabulary").font(.caption).foregroundStyle(.secondary)
                        Text(modelPath)
                            .font(.caption2.monospaced()).foregroundStyle(.tertiary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    Spacer()
                }
                footer.padding(.leading, 24)
            }
        }
        .onAppear { installer.refreshStatus() }
        .confirmationDialog(
            "Remove RAM++ tagger?",
            isPresented: $confirmUninstall,
            titleVisibility: .visible
        ) {
            Button("Remove", role: .destructive) { installer.uninstall(); confirmUninstall = false }
            Button("Keep", role: .cancel) { confirmUninstall = false }
        } message: {
            Text("Frees ~450 MB. Tagging falls back to the lighter built-in classifier.")
        }
    }

    @ViewBuilder private var statusIcon: some View {
        switch installer.status {
        case .installed: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .downloading: Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
        default: Image(systemName: "xmark.circle").foregroundStyle(.orange)
        }
    }

    @ViewBuilder private var footer: some View {
        switch installer.status {
        case .unknown:
            EmptyView()
        case .missing:
            Button { installer.install() } label: {
                Label("Install (~450 MB)", systemImage: "arrow.down.circle.fill")
            }
            .buttonStyle(.borderedProminent)
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 4) {
                ProgressView(value: fraction).frame(maxWidth: 280)
                HStack {
                    Text(message).font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel") { installer.cancel() }.font(.caption)
                }
            }
        case .installed(let sizeBytes):
            HStack(spacing: 8) {
                Text("Installed · \(sizeBytes / 1_048_576) MB").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Remove", role: .destructive) { confirmUninstall = true }.font(.caption)
            }
        case .installFailed(let msg):
            VStack(alignment: .leading, spacing: 4) {
                Text(msg).font(.caption).foregroundStyle(.red)
                Button("Retry") { installer.install() }.buttonStyle(.bordered)
            }
        }
    }
}

// AI Models — BGE document embedder. The engine's restructure clusters documents by
// content when this is installed, else by filename — so installing is purely an upgrade.
struct BGEDocCard: View {
    @State private var installer = BGEModelInstaller.shared
    @State private var confirmUninstall = false

    private var modelPath: String {
        BGEModelInstaller.modelsRoot.appendingPathComponent("bge_text/bge_small.onnx").path
    }

    var body: some View {
        GlassCard(fillsWidth: true) {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — document understanding").font(.headline)
                Text("BGE-small reads a document's content so Restructure groups files by what they say, not their filename (a physics paper joins your physics folder). MIT; one-click, no Python. Without it, documents group by filename.")
                    .font(.callout).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                HStack(alignment: .top, spacing: 8) {
                    statusIcon.padding(.top, 2)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("BGE-small-en-v1.5").font(.callout.bold())
                        Text("384-d BERT text embedder · runs on the Neural Engine").font(.caption).foregroundStyle(.secondary)
                        Text(modelPath)
                            .font(.caption2.monospaced()).foregroundStyle(.tertiary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    Spacer()
                }
                footer.padding(.leading, 24)
            }
        }
        .onAppear { installer.refreshStatus() }
        .confirmationDialog(
            "Remove document understanding?",
            isPresented: $confirmUninstall,
            titleVisibility: .visible
        ) {
            Button("Remove", role: .destructive) { installer.uninstall(); confirmUninstall = false }
            Button("Keep", role: .cancel) { confirmUninstall = false }
        } message: {
            Text("Frees ~135 MB. Documents fall back to filename-based grouping in Restructure.")
        }
    }

    @ViewBuilder private var statusIcon: some View {
        switch installer.status {
        case .installed: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .downloading: Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
        default: Image(systemName: "xmark.circle").foregroundStyle(.orange)
        }
    }

    @ViewBuilder private var footer: some View {
        switch installer.status {
        case .unknown:
            EmptyView()
        case .missing:
            Button { installer.install() } label: {
                Label("Install (~135 MB)", systemImage: "arrow.down.circle.fill")
            }
            .buttonStyle(.borderedProminent)
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 4) {
                ProgressView(value: fraction).frame(maxWidth: 280)
                HStack {
                    Text(message).font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel") { installer.cancel() }.font(.caption)
                }
            }
        case .installed(let sizeBytes):
            HStack(spacing: 8) {
                Text("Installed · \(sizeBytes / 1_048_576) MB").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Remove", role: .destructive) { confirmUninstall = true }.font(.caption)
            }
        case .installFailed(let msg):
            VStack(alignment: .leading, spacing: 4) {
                Text(msg).font(.caption).foregroundStyle(.red)
                Button("Retry") { installer.install() }.buttonStyle(.bordered)
            }
        }
    }
}

struct FaceEmbedderCard: View {
    let engine: EngineClient
    let store: ReadStore
    @State private var installer = ArcFaceModelInstaller.shared
    @State private var confirmUninstall: FaceEmbedderKind?

    var body: some View {
        GlassCard(fillsWidth: true) {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — face recognition").font(.headline)
                Text("SFace (Apache-2.0) produces a 128-d face embedding per detected face, used to cluster people across your library — install with one click, no Python required.")
                    .font(.callout).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                ForEach(FaceEmbedderKind.allCases, id: \.rawValue) { kind in
                    embedderRow(kind)
                    if kind != FaceEmbedderKind.allCases.last {
                        Divider().opacity(0.2)
                    }
                }
            }
        }
        .onAppear { installer.refreshStatus() }
        .confirmationDialog(
            "Remove face model?",
            isPresented: Binding(
                get: { confirmUninstall != nil },
                set: { if !$0 { confirmUninstall = nil } }
            ),
            titleVisibility: .visible,
            presenting: confirmUninstall
        ) { kind in
            Button("Remove", role: .destructive) {
                installer.uninstall(kind)
                confirmUninstall = nil
            }
            Button("Keep", role: .cancel) { confirmUninstall = nil }
        } message: { kind in
            let mb = kind.approxBytes / 1_048_576
            Text("Frees ~\(mb) MB. Face clustering will fall back to whichever other variant is installed, or pause if none are.")
        }
    }

    @ViewBuilder
    private func embedderRow(_ kind: FaceEmbedderKind) -> some View {
        let path = FaceEmbedderKind.modelsDirectory.appendingPathComponent(kind.modelFileName).path
        let status = installer.status[kind] ?? .unknown
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                statusIcon(status)
                    .padding(.top, 2)
                VStack(alignment: .leading, spacing: 2) {
                    Text(kind.displayName).font(.callout.bold())
                    Text(kind.subtitle).font(.caption).foregroundStyle(.secondary)
                    Text(path)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
            }
            statusFooter(for: kind, status: status)
                .padding(.leading, 24)
        }
    }

    @ViewBuilder
    private func statusIcon(_ status: ArcFaceModelInstaller.Status) -> some View {
        switch status {
        case .installed:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .downloading:
            Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
        default:
            Image(systemName: "xmark.circle").foregroundStyle(.orange)
        }
    }

    @ViewBuilder
    private func statusFooter(for kind: FaceEmbedderKind,
                              status: ArcFaceModelInstaller.Status) -> some View {
        switch status {
        case .unknown:
            EmptyView()

        case .missing:
            HStack(spacing: 8) {
                Button {
                    installer.install(kind)
                } label: {
                    Label("Install (~\(kind.approxBytes / 1_048_576) MB)",
                          systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)
                .controlSize(.small)
                Spacer()
            }

        case .downloading(let frac, let msg, let bps, let eta):
            VStack(alignment: .leading, spacing: 4) {
                if frac > 0 {
                    ProgressView(value: frac)
                } else {
                    ProgressView()
                }
                HStack {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(msg).font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                        let rateETA = DownloadFormat.rateAndETA(
                            DownloadTick(written: 0, total: 0,
                                          bytesPerSecond: bps, etaSeconds: eta))
                        if !rateETA.isEmpty {
                            Text(rateETA).font(.caption2.monospaced())
                                .foregroundStyle(.tertiary)
                        }
                    }
                    Spacer()
                    Button("Cancel") { installer.cancel(kind) }
                        .buttonStyle(.borderless)
                        .controlSize(.small)
                }
            }

        case .installed(let bytes):
            HStack(spacing: 8) {
                Text("\(bytes / 1_048_576) MB installed")
                    .font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Uninstall") { confirmUninstall = kind }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .foregroundStyle(.red)
            }

        case .installFailed(let why):
            VStack(alignment: .leading, spacing: 4) {
                Text(why).font(.caption2)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button("Retry") { installer.install(kind) }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    Spacer()
                }
            }
        }
    }
}
