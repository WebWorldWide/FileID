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
    @AppStorage(AppSettings.restructureGranularityKey) private var restructureGranularity: String = AppSettings.restructureGranularityDefault
    @State private var showAdvanced = false
    @State private var sessions: [ReadStore.ScanSessionRow] = []
    @State private var confirmFactoryReset = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                AppearanceCard()

                // ─── User-facing settings (always visible) ───────────────

                GlassCard {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Cleanup").font(Theme.Font.body(14, .semibold))
                        Toggle(isOn: $cleanupAutoTag) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("Tag kept files after Cleanup")
                                    .font(Theme.Font.body(13.5))
                                Text("When ON, after you trash duplicates the surviving keepers get a Finder tag (\"\(AppSettings.cleanupAutoTagName)\"). Useful for finding files you've already deduped via a Finder Smart Folder.")
                                    .font(Theme.Font.body(11))
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .toggleStyle(DSToggleStyle())
                    }
                }

                GlassCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Restructure").font(Theme.Font.body(14, .semibold))
                        LabeledSegment("Folder granularity") {
                            SegmentedBar(selection: $restructureGranularity, options: [("loose", "Looser"), ("normal", "Balanced"), ("tight", "Tighter")])
                        }
                        Text("How finely Restructure splits your files into folders — Looser groups broadly into fewer folders, Tighter makes more, smaller ones. Takes effect the next time the engine starts (relaunch FileID, or Settings ▸ Advanced ▸ Restart Engine).")
                            .font(Theme.Font.body(11))
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

                GlassCard {
                    DisclosureGroup(isExpanded: $showAdvanced) {
                        VStack(alignment: .leading, spacing: 16) {
                            Divider().opacity(0.3)

                            // Engine
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Engine").font(Theme.Font.body(12.5, .semibold))
                                infoRow("Status", connectionLabel)
                                if case .ready(let info) = engine.state {
                                    infoRow("Version", info.version)
                                    infoRow("PID",     "\(info.pid)")
                                    infoRow("Workers", "\(info.workerCap)")
                                    infoRow("Memory",  "\(Int(info.physicalMemoryGB)) GB")
                                }
                                HStack(spacing: 8) {
                                    Button("Restart Engine") { engine.start() }
                                        .buttonStyle(.dsSecondary)
                                        .help("Spawn a fresh engine process. Cancels any in-flight scan.")
                                    if case .ready = engine.state {
                                        Button("Stop Engine") { engine.shutdown() }
                                            .buttonStyle(.dsSecondary)
                                            .help("Cleanly shut down the engine process.")
                                    }
                                }
                            }

                            Divider().opacity(0.3)

                            // Storage
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Storage").font(Theme.Font.body(12.5, .semibold))
                                infoRow("Total files",   "\(store.totalFiles)")
                                infoRow("Images tagged", "\(store.totalImages)")
                                infoRow("Stored duplicate hints", "\(store.totalDuplicateGroups)")
                                infoRow("Hint reclaimable", String(format: "%.1f MB", store.totalReclaimableMB))
                                infoRow("Database", ReadStore.defaultDBURL.path)
                                Button("Show database in Finder") {
                                    NSWorkspace.shared.activateFileViewerSelecting([ReadStore.defaultDBURL])
                                }
                                .buttonStyle(.dsSecondary)
                            }

                            Divider().opacity(0.3)

                            // Recent scans (folded in from former Review tab)
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Recent scans").font(Theme.Font.body(12.5, .semibold))
                                if sessions.isEmpty {
                                    Text("No scans recorded yet.")
                                        .font(Theme.Font.body(12)).foregroundStyle(.secondary)
                                } else {
                                    ForEach(sessions) { s in
                                        sessionRow(s)
                                    }
                                }
                            }

                            Divider().opacity(0.3)

                            // Logs
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Logs").font(Theme.Font.body(12.5, .semibold))
                                Text("Detailed scan + app logs for troubleshooting.")
                                    .font(Theme.Font.body(12)).foregroundStyle(.secondary)
                                HStack(spacing: 8) {
                                    Button("Open scan log") {
                                        NSWorkspace.shared.open(SettingsTab.scanLogURL)
                                    }
                                    .buttonStyle(.dsSecondary)
                                    Button("Open app log") {
                                        NSWorkspace.shared.open(SettingsTab.appLogURL)
                                    }
                                    .buttonStyle(.dsSecondary)
                                    Button("Show logs in Finder") {
                                        NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.scanLogURL])
                                    }
                                    .buttonStyle(.dsSecondary)
                                }
                            }

                            Divider().opacity(0.3)

                            // Danger Zone
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Danger Zone").font(Theme.Font.body(12.5, .semibold)).foregroundStyle(Theme.danger)
                                Text("Permanently erase FileID's library, local models, settings, and caches.")
                                    .font(Theme.Font.body(12)).foregroundStyle(.secondary)
                                Button(role: .destructive) {
                                    confirmFactoryReset = true
                                } label: {
                                    Text("Factory Reset & Quit")
                                }
                                .buttonStyle(.dsPrimary)
                                .tint(Theme.danger)
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
                            Text("Advanced").font(Theme.Font.body(14, .semibold))
                            Text("(engine status, database, scan history, logs)")
                                .font(Theme.Font.body(12))
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
                .foregroundStyle(s.status == "completed" ? Theme.delight
                                 : s.status == "running"  ? Theme.gold : Theme.danger)
            VStack(alignment: .leading, spacing: 2) {
                Text(s.rootPath).font(Theme.Font.mono(11)).lineLimit(1).truncationMode(.middle)
                HStack(spacing: 12) {
                    Text(s.startedAt.formatted(date: .abbreviated, time: .shortened))
                    Text(s.status)
                    if let n = s.lastFileIndex { Text("\(n) files") }
                }
                .font(Theme.Font.mono(10.5)).foregroundStyle(.secondary)
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
            Text(v).font(Theme.Font.mono(12.5)).textSelection(.enabled)
            Spacer()
        }
        .font(Theme.Font.body(13.5))
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
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("Deep Analyze exclusions").font(Theme.Font.body(14, .semibold))
                Text("FileID skips these folders when running Deep Analyze over your whole library. Files stay in the library and search normally — only the VLM pass (captions, smart renames, tags) is skipped. Selecting specific files to analyze always ignores this list.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Divider().opacity(0.3)

                if let message {
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: message.isError ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                            .foregroundStyle(message.isError ? Theme.danger : Theme.delight)
                        Text(message.text)
                            .font(Theme.Font.body(12))
                            .foregroundStyle(message.isError ? Theme.danger : .secondary)
                        Spacer()
                    }
                }

                if settings.excludedFolders.isEmpty {
                    Text("No folders are excluded from Deep Analyze.")
                        .font(Theme.Font.body(11)).foregroundStyle(.secondary)
                } else {
                    ForEach(settings.excludedFolders, id: \.self) { folder in
                        HStack(spacing: 8) {
                            Text(folder)
                                .font(Theme.Font.mono(11))
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            DSIconButton("Stop excluding \(folder) from Deep Analyze", systemImage: "xmark", tint: Theme.text3) {
                                settings.removeExcludedFolder(folder)
                            }
                        }
                    }
                }

                Button("Add folder…") { pickExcludedFolder() }
                    .buttonStyle(.dsSecondary)
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
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — semantic search (CLIP)").font(Theme.Font.body(14, .semibold))
                Text("Type natural-language searches like \"sunset at the beach\" and FileID ranks every photo by visual relevance. Uses OpenCLIP ViT-B/32 — runs entirely on your Mac.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
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
                    // the next app launch, with no fallback.
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
                    Text(reason).font(Theme.Font.body(11)).foregroundStyle(.secondary)
                    Text("Approximately \(downloadSizeLabel) from huggingface.co (Xenova CLIP ViT-B/32 + OpenAI BPE vocabulary).")
                        .font(Theme.Font.body(11)).foregroundStyle(.tertiary)
                }
                Spacer()
            }
            HStack(spacing: 8) {
                Button {
                    installer.install()
                } label: {
                    Label("Download", systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.dsPrimary)
                .tint(Theme.gold)

                Button("Install from local zip…") { pickLocalZip() }
                    .buttonStyle(.dsSecondary)

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
                        Text(msg).font(Theme.Font.mono(10.5))
                            .foregroundStyle(.secondary)
                        let rateETA = DownloadFormat.rateAndETA(
                            DownloadTick(written: 0, total: 0,
                                          bytesPerSecond: bps, etaSeconds: eta))
                        if !rateETA.isEmpty {
                            Text(rateETA).font(Theme.Font.mono(10.5))
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
                Text("Extracting…").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                Spacer()
            }

        case .installed(let bytes):
            HStack(spacing: 8) {
                Image(systemName: "checkmark.seal.fill")
                    .foregroundStyle(Theme.delight)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Installed").font(Theme.Font.body(13.5, .semibold))
                    Text("\(ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)) on disk · semantic search active.")
                        .font(Theme.Font.body(11)).foregroundStyle(.secondary)
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
                    .foregroundStyle(Theme.danger)
            }

        case .installFailed(let why):
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(Theme.danger)
                    Text(why).font(Theme.Font.body(12))
                        .foregroundStyle(Theme.danger)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer()
                }
                HStack(spacing: 8) {
                    Button("Retry") { installer.install() }
                        .buttonStyle(.dsSecondary)
                    Button("Install from local zip…") { pickLocalZip() }
                        .buttonStyle(.dsSecondary)
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
                .foregroundStyle(installed ? Theme.delight : .secondary)
            VStack(alignment: .leading, spacing: 1) {
                Text(name).font(Theme.Font.body(13.5))
                Text(url.path)
                    .font(Theme.Font.mono(10.5))
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
// RamPlusService reads whatever this installs and falls back to Vision tags if
// absent, so installing is purely an upgrade.
struct RamPlusTaggerCard: View {
    @State private var installer = RamPlusModelInstaller.shared
    @State private var confirmUninstall = false

    private var modelPath: String {
        RamPlusModelInstaller.modelsRoot.appendingPathComponent("ram_plus/ram_plus.onnx").path
    }

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — image tagging").font(Theme.Font.body(14, .semibold))
                Text("RAM++ recognizes 4585 everyday tags on-device (richer than the built-in classifier). Apache-2.0; install with one click, no Python required. Without it, tagging uses the lighter built-in classifier.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                HStack(alignment: .top, spacing: 8) {
                    statusIcon.padding(.top, 2)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("RAM++ (Recognize Anything Plus)").font(Theme.Font.body(13.5, .semibold))
                        Text("Swin-Large @384 · 4585-tag English vocabulary").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                        Text(modelPath)
                            .font(Theme.Font.mono(10.5)).foregroundStyle(.tertiary)
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
        case .installed: Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.delight)
        case .downloading: Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
        default: Image(systemName: "xmark.circle").foregroundStyle(Theme.warning)
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
            .buttonStyle(.dsPrimary)
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 4) {
                ProgressView(value: fraction).frame(maxWidth: 280)
                HStack {
                    Text(message).font(Theme.Font.body(12)).foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel") { installer.cancel() }.font(Theme.Font.body(12))
                }
            }
        case .installed(let sizeBytes):
            HStack(spacing: 8) {
                Text("Installed · \(ByteCountFormatter.string(fromByteCount: Int64(sizeBytes), countStyle: .file))").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                Spacer()
                Button("Remove", role: .destructive) { confirmUninstall = true }.font(Theme.Font.body(12))
            }
        case .installFailed(let msg):
            VStack(alignment: .leading, spacing: 4) {
                Text(msg).font(Theme.Font.body(12)).foregroundStyle(Theme.danger)
                Button("Retry") { installer.install() }.buttonStyle(.dsSecondary)
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
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — document understanding").font(Theme.Font.body(14, .semibold))
                Text("BGE-small reads a document's content so Restructure groups files by what they say, not their filename (a physics paper joins your physics folder). MIT; one-click, no Python. Without it, documents group by filename.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
                Divider().opacity(0.3)
                HStack(alignment: .top, spacing: 8) {
                    statusIcon.padding(.top, 2)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("BGE-small-en-v1.5").font(Theme.Font.body(13.5, .semibold))
                        Text("384-d BERT text embedder · runs on the Neural Engine").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                        Text(modelPath)
                            .font(Theme.Font.mono(10.5)).foregroundStyle(.tertiary)
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
        case .installed: Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.delight)
        case .downloading: Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
        default: Image(systemName: "xmark.circle").foregroundStyle(Theme.warning)
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
            .buttonStyle(.dsPrimary)
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 4) {
                ProgressView(value: fraction).frame(maxWidth: 280)
                HStack {
                    Text(message).font(Theme.Font.body(12)).foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel") { installer.cancel() }.font(Theme.Font.body(12))
                }
            }
        case .installed(let sizeBytes):
            HStack(spacing: 8) {
                Text("Installed · \(ByteCountFormatter.string(fromByteCount: Int64(sizeBytes), countStyle: .file))").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                Spacer()
                Button("Remove", role: .destructive) { confirmUninstall = true }.font(Theme.Font.body(12))
            }
        case .installFailed(let msg):
            VStack(alignment: .leading, spacing: 4) {
                Text(msg).font(Theme.Font.body(12)).foregroundStyle(Theme.danger)
                Button("Retry") { installer.install() }.buttonStyle(.dsSecondary)
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
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — face recognition").font(Theme.Font.body(14, .semibold))
                Text("SFace (Apache-2.0) produces a 128-d face embedding per detected face, used to cluster people across your library — install with one click, no Python required.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
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
            let size = ByteCountFormatter.string(fromByteCount: Int64(kind.approxBytes), countStyle: .file)
            Text("Frees about \(size). Face clustering will fall back to whichever other variant is installed, or pause if none are.")
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
                    Text(kind.displayName).font(Theme.Font.body(13.5, .semibold))
                    Text(kind.subtitle).font(Theme.Font.body(12)).foregroundStyle(.secondary)
                    Text(path)
                        .font(Theme.Font.mono(10.5))
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
            Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.delight)
        case .downloading:
            Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
        default:
            Image(systemName: "xmark.circle").foregroundStyle(Theme.warning)
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
                    Label("Install (~\(ByteCountFormatter.string(fromByteCount: Int64(kind.approxBytes), countStyle: .file)))",
                          systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.dsPrimary)
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
                        Text(msg).font(Theme.Font.mono(10.5))
                            .foregroundStyle(.secondary)
                        let rateETA = DownloadFormat.rateAndETA(
                            DownloadTick(written: 0, total: 0,
                                          bytesPerSecond: bps, etaSeconds: eta))
                        if !rateETA.isEmpty {
                            Text(rateETA).font(Theme.Font.mono(10.5))
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
                Text("\(ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)) installed")
                    .font(Theme.Font.body(11)).foregroundStyle(.secondary)
                Spacer()
                Button("Uninstall") { confirmUninstall = kind }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .foregroundStyle(Theme.danger)
            }

        case .installFailed(let why):
            VStack(alignment: .leading, spacing: 4) {
                Text(why).font(Theme.Font.body(11))
                    .foregroundStyle(Theme.danger)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button("Retry") { installer.install(kind) }
                        .buttonStyle(.dsSecondary)
                        .controlSize(.small)
                    Spacer()
                }
            }
        }
    }
}


/// Prototype v3 appearance: lava lamp Full / Calm / Off.
struct AppearanceCard: View {
    @AppStorage("lavaMode") private var lavaMode: String = LavaMode.full.rawValue
    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("Appearance").font(Theme.Font.body(14, .semibold))
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Lava lamp").font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1)
                        MetaText("Wax glows rise behind the panes; the light turns lavender while a model runs. Reduce Motion pauses it.")
                    }
                    Spacer()
                    SegmentedBar(selection: $lavaMode, options: LavaMode.allCases.map { ($0.rawValue, $0.label) }, height: 26)
                }
            }
        }
    }
}
