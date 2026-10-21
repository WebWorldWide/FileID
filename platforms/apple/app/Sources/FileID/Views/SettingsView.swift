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

                Panel(padding: 16) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Duplicates").font(Theme.Font.body(14, .semibold))
                        SettingToggleRow("Tag kept files after removing duplicates",
                                         subtitle: "After you move extra copies to Trash, the copies you keep get the Finder tag “\(AppSettings.cleanupAutoTagName)”, so a Smart Folder can find them.",
                                         isOn: $cleanupAutoTag)
                    }
                }

                Panel(padding: 16) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Organize Folders").font(Theme.Font.body(14, .semibold))
                        LabeledSegment("Folder granularity") {
                            SegmentedBar(selection: $restructureGranularity, options: [("loose", "Looser"), ("normal", "Balanced"), ("tight", "Tighter")])
                        }
                        Text("How finely Organize Folders splits your files. Looser makes fewer, broader folders; Tighter makes more, smaller ones. Takes effect after you quit and reopen FileID.")
                            .font(Theme.Font.body(11))
                            .foregroundStyle(Theme.text3)
                    }
                }

                DeepAnalyzeExclusionsCard()

                // AI Models — visible because users genuinely care about
                // which models are installed and download status.
                ModelInstallCard.clip
                ModelInstallCard.ramPlus
                ModelInstallCard.bge

                DeepAnalyzeModelPickerCard(engine: engine)
                ModelInstallCard.face

                // ─── Advanced (collapsed by default) ─────────────────────
                // Engine PIDs, DB paths, log files. Power-user info that
                // doesn't help a casual user choose anything; hiding it
                // declutters the page.

                Panel(padding: 16) {
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
                                        .help("Restart FileID's background helper. Stops any scan in progress.")
                                    if case .ready = engine.state {
                                        Button("Stop Engine") { engine.shutdown() }
                                            .buttonStyle(.dsSecondary)
                                            .help("Stop FileID's background helper.")
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
                                        .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
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
                                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
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
                                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
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
                                    Text("This permanently deletes everything FileID learned (tags, faces, descriptions), its settings, downloaded models and caches. The large description models are kept. Your own files are not touched. This can't be undone.")
                                }
                            }
                        }
                        .padding(.top, 8)
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "wrench.and.screwdriver")
                                .foregroundStyle(Theme.text3)
                            Text("Advanced").font(Theme.Font.body(14, .semibold))
                            Text("(troubleshooting: background helper, storage, scan history, logs)")
                                .font(Theme.Font.body(12))
                                .foregroundStyle(Theme.text3)
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
                .font(Theme.Font.mono(10.5)).foregroundStyle(Theme.text3)
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
            Text(k).foregroundStyle(Theme.text3).frame(width: 130, alignment: .leading)
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
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Folders not to describe").font(Theme.Font.body(14, .semibold))
                Text("Describe Files skips these folders when it describes the whole library. Their files still show up and can be searched. Describing a single file ignores this list.")
                    .font(Theme.Font.meta).foregroundStyle(Theme.text3)
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
                    Text("No folders are skipped.")
                        .font(Theme.Font.body(11)).foregroundStyle(Theme.text3)
                } else {
                    ForEach(settings.excludedFolders, id: \.self) { folder in
                        HStack(spacing: 8) {
                            Text(folder)
                                .font(Theme.Font.mono(11))
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            DSIconButton("Describe \(folder) again", systemImage: "xmark", tint: Theme.text3) {
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
        panel.message = "Choose a folder for Describe Files to skip"
        panel.prompt = "Exclude"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        switch settings.addExcludedFolder(url.path) {
        case .added:
            message = ("Done. Describe Files will skip this folder from the next run.", false)
        case .alreadyExcluded:
            message = ("That folder is already skipped.", false)
        case .invalid:
            message = ("Couldn't skip that folder. Choose a folder on this Mac.", true)
        }
    }
}

// MARK: - Model install card

struct ModelInstallCard: View {
    struct Row {
        let name: String
        var detail: String? = nil
        let path: URL
    }

    let installer: any ModelInstalling
    let title: String
    let blurb: String
    let rows: [Row]
    let approxBytes: Int64
    let removeTitle: String
    let fallback: String
    var onRemoved: () -> Void = {}
    var localZip: ((URL) -> Void)? = nil
    @State private var confirmRemove = false

    private var sizeLabel: String { ByteCountFormatter.string(fromByteCount: approxBytes, countStyle: .file) }

    var body: some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                Text(title).font(Theme.Font.body(14, .semibold))
                Text(blurb)
                    .font(Theme.Font.meta).foregroundStyle(Theme.text3)
                    .fixedSize(horizontal: false, vertical: true)
                Divider().opacity(0.3)
                ForEach(rows, id: \.path) { rowView($0) }
                Divider().opacity(0.3)
                footer
            }
        }
        .onAppear { installer.refreshStatus() }
        .confirmationDialog(removeTitle, isPresented: $confirmRemove, titleVisibility: .visible) {
            Button("Remove", role: .destructive) {
                Task {
                    await installer.uninstall()
                    onRemoved()
                }
            }
            Button("Keep", role: .cancel) {}
        } message: {
            Text("Frees about \(sizeLabel). \(fallback)")
        }
    }

    private func rowView(_ row: Row) -> some View {
        HStack(alignment: .top, spacing: 8) {
            rowIcon(present: installer.presentFilePaths.contains(row.path.path)).padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.name).font(Theme.Font.body(13.5, .semibold))
                if let detail = row.detail {
                    Text(detail).font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                }
                Text(row.path.path)
                    .font(Theme.Font.mono(10.5)).foregroundStyle(Theme.text3)
                    .lineLimit(1).truncationMode(.middle)
            }
            Spacer()
        }
    }

    @ViewBuilder
    private func rowIcon(present: Bool) -> some View {
        switch installer.status {
        case _ where present: Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.delight)
        case .downloading, .extracting: Image(systemName: "arrow.down.circle.fill").foregroundStyle(Theme.gold)
        case .installFailed: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
        default: Image(systemName: "circle.dashed").foregroundStyle(Theme.text3)
        }
    }

    @ViewBuilder
    private var footer: some View {
        switch installer.status {
        case .unknown:
            ProgressView().controlSize(.small)
        case .missing(let reason):
            MetaText("\(reason) About \(sizeLabel) from huggingface.co.", size: 11)
            HStack(spacing: 8) {
                Button { installer.install() } label: {
                    Label("Download (~\(sizeLabel))", systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.dsPrimary)
                localZipButton
                openFolderButton
                Spacer()
            }
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 6) {
                if fraction > 0 { ProgressView(value: fraction) } else { ProgressView() }
                HStack {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(message).font(Theme.Font.mono(10.5)).foregroundStyle(Theme.text3)
                        if let rate = installer.status.rateAndETA, !rate.isEmpty {
                            Text(rate).font(Theme.Font.mono(10.5)).foregroundStyle(Theme.text3)
                        }
                    }
                    Spacer()
                    Button("Cancel") { installer.cancel() }
                        .buttonStyle(.borderless).controlSize(.small)
                }
            }
        case .extracting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                MetaText("Extracting…")
                Spacer()
            }
        case .installed(let bytes):
            HStack(spacing: 8) {
                MetaText("Ready · \(ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)) on disk", size: 11)
                Spacer()
                openFolderButton
                Button("Remove") { confirmRemove = true }
                    .buttonStyle(.borderless).controlSize(.small)
                    .foregroundStyle(Theme.danger)
            }
        case .installFailed(let why):
            VStack(alignment: .leading, spacing: 6) {
                Text(why).font(Theme.Font.body(12)).foregroundStyle(Theme.danger)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button("Retry") { installer.install() }.buttonStyle(.dsSecondary)
                    localZipButton
                    openFolderButton
                    Spacer()
                }
            }
        }
    }

    @ViewBuilder
    private var localZipButton: some View {
        if let localZip {
            Button("Install from local zip…") {
                let panel = NSOpenPanel()
                panel.allowedContentTypes = [.zip]
                panel.allowsMultipleSelection = false
                panel.message = "Choose the clip-models.zip file."
                panel.prompt = "Install"
                if panel.runModal() == .OK, let url = panel.url { localZip(url) }
            }
            .buttonStyle(.dsSecondary)
        }
    }

    private var openFolderButton: some View {
        Button("Open Models folder") {
            NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.modelsFolderURL])
        }
        .buttonStyle(.borderless).controlSize(.small)
        .foregroundStyle(Theme.text3)
    }
}

extension ModelInstallCard {
    static var clip: ModelInstallCard {
        let installer = CLIPModelInstaller.shared
        return ModelInstallCard(
            installer: installer,
            title: "Search by description",
            blurb: "Lets you search photos by what's in them, like \"sunset at the beach\". Model: OpenCLIP ViT-B/32 (MIT), runs on this Mac.",
            rows: [
                Row(name: "CLIP ViT-B/32 (image)", path: CLIPModelInstaller.modelsRoot.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx")),
                Row(name: "CLIP ViT-B/32 (text)", path: CLIPTextEncoder.defaultModelURL),
                Row(name: "BPE vocabulary (vocab.json + merges.txt)", path: CLIPTextEncoder.defaultDirectory.appendingPathComponent("vocab.json")),
            ],
            approxBytes: CLIPModelInstaller.approxDownloadBytes,
            removeTitle: "Remove the Search by description model?",
            fallback: "Search goes back to matching names and tags until you download it again.",
            onRemoved: { CLIPTextEncoder.shared.unload() },
            localZip: { installer.installFromLocalZip($0) })
    }

    static var ramPlus: ModelInstallCard {
        let installer = ModelInstaller.ramPlus
        return ModelInstallCard(
            installer: installer,
            title: "Photo tags",
            blurb: "Labels what's in each photo (thousands of everyday things) so you can search and sort by it. Model: RAM++ (Apache-2.0). Without it, FileID uses a simpler built-in tagger.",
            rows: [Row(name: "RAM++ (Recognize Anything Plus)", detail: "Swin-Large @384 · 4585-tag English vocabulary", path: installer.spec.primaryFile)],
            approxBytes: installer.spec.approxBytes,
            removeTitle: "Remove the Photo tags model?",
            fallback: "Tagging falls back to the simpler built-in tagger.")
    }

    static var bge: ModelInstallCard {
        let installer = ModelInstaller.bge
        return ModelInstallCard(
            installer: installer,
            title: "Documents",
            blurb: "Reads what documents are about, so Organize Folders puts a physics paper with your physics files. Model: BGE-small (MIT). Without it, documents are grouped by file name.",
            rows: [Row(name: "BGE-small-en-v1.5", detail: "384-d BERT text embedder · runs on the Neural Engine", path: installer.spec.primaryFile)],
            approxBytes: installer.spec.approxBytes,
            removeTitle: "Remove the Documents model?",
            fallback: "Organize Folders groups documents by file name instead.")
    }

    static var face: ModelInstallCard {
        let installer = ModelInstaller.face
        let kind = FaceEmbedderKind.sface
        return ModelInstallCard(
            installer: installer,
            title: "Faces",
            blurb: "Recognizes the same person across your photos so People can group them. Model: SFace (Apache-2.0), runs on this Mac.",
            rows: [Row(name: kind.displayName, detail: kind.subtitle, path: installer.spec.primaryFile)],
            approxBytes: installer.spec.approxBytes,
            removeTitle: "Remove face model?",
            fallback: "Grouping faces stops until you download it again.")
    }
}


/// Prototype v3 appearance: lava lamp Full / Calm / Off.
struct AppearanceCard: View {
    @AppStorage("lavaMode") private var lavaMode: String = LavaMode.full.rawValue
    var body: some View {
        Panel(padding: 16) {
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
