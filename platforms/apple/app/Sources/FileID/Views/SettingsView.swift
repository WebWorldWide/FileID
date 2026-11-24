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
    @AppStorage(AppSettings.fastTextReadingKey) private var fastTextReading: Bool = AppSettings.fastTextReadingDefault
    @State private var showAdvanced = false
    @State private var sessions: [ReadStore.ScanSessionRow] = []
    @State private var confirmFactoryReset = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                AppearanceCard()
                PrivacyCard()

                LocationsCard(engine: engine)

                Panel(padding: 16) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Duplicates").font(Theme.Font.body(14, .semibold))
                        SettingToggleRow("Tag kept files after removing duplicates",
                                         subtitle: "After you move extra copies to Trash, the copies you keep get the Finder tag “\(AppSettings.cleanupAutoTagName)”, so a Smart Folder can find them.",
                                         isOn: $cleanupAutoTag)
                    }
                }

                Panel(padding: 16) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Scanning").font(Theme.Font.body(14, .semibold))
                        SettingToggleRow("Faster text reading (Latin scripts only)",
                                         subtitle: "Reads text in photos and screenshots as English and other Latin-alphabet languages (Cyrillic too), which makes scans of screenshots and signs about a third faster. Chinese, Japanese, Korean, Arabic and other scripts are not read. Takes effect after FileID’s helper restarts.",
                                         isOn: $fastTextReading)
                        restartHelperRow
                    }
                }

                Panel(padding: 16) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Organize Folders").font(Theme.Font.body(14, .semibold))
                        LabeledSegment("Folder granularity") {
                            SegmentedBar(selection: $restructureGranularity, options: [("loose", "Looser"), ("normal", "Balanced"), ("tight", "Tighter")])
                        }
                        MetaText("How finely Organize Folders splits your files. Looser makes fewer, broader folders; Tighter makes more, smaller ones. Takes effect after FileID’s helper restarts.")
                            .fixedSize(horizontal: false, vertical: true)
                        restartHelperRow
                    }
                }

                DeepAnalyzeExclusionsCard()

                EssentialModelsCard()
                ModelInstallCard.clip
                ModelInstallCard.ramPlus
                ModelInstallCard.bge

                DeepAnalyzeModelPickerCard(engine: engine)
                ModelInstallCard.face
                ModelInstallCard.faceMesh

                Panel(padding: 16) {
                    DisclosureGroup(isExpanded: $showAdvanced) {
                        VStack(alignment: .leading, spacing: 16) {
                            Hairline(color: Theme.hairlineSoft)

                            VStack(alignment: .leading, spacing: 8) {
                                Text("FileID’s helper").font(Theme.Font.body(12.5, .semibold))
                                infoRow("Status", connectionLabel)
                                if case .ready(let info) = engine.state {
                                    infoRow("Version", info.version)
                                    infoRow("Process number", "\(info.pid)")
                                    infoRow("Files worked on at once", "\(info.workerCap)")
                                    infoRow("This Mac’s memory",  "\(Int(info.physicalMemoryGB)) GB")
                                }
                                HStack(spacing: 8) {
                                    Button("Restart helper") { engine.restart() }
                                        .buttonStyle(.dsSecondary)
                                        .help("Restart FileID’s background helper. Stops any scan in progress.")
                                }
                            }

                            Hairline(color: Theme.hairlineSoft)

                            VStack(alignment: .leading, spacing: 8) {
                                Text("Storage").font(Theme.Font.body(12.5, .semibold))
                                infoRow("Total files",   store.totalFiles.formatted())
                                infoRow("Photos tagged", store.totalImages.formatted())
                                infoRow("Sets of duplicates", store.totalDuplicateGroups.formatted())
                                infoRow("Space duplicates use", Self.duplicateSpaceLabel(reclaimableBytes: store.totalReclaimableBytes))
                                infoRow("Library file", PathText.abbreviated(ReadStore.defaultDBURL.path), isPath: true)
                                    .help(ReadStore.defaultDBURL.path)
                                Button("Show library file in Finder") {
                                    NSWorkspace.shared.activateFileViewerSelecting([ReadStore.defaultDBURL])
                                }
                                .buttonStyle(.dsSecondary)
                            }

                            Hairline(color: Theme.hairlineSoft)

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

                            Hairline(color: Theme.hairlineSoft)

                            VStack(alignment: .leading, spacing: 8) {
                                Text("Acknowledgements").font(Theme.Font.body(12.5, .semibold))
                                Text("FileID is open source under the Apache License 2.0. These are the licenses of the open-source software and models it uses.")
                                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                                HStack(spacing: 8) {
                                    Button("Open-source acknowledgements") {
                                        if let url = LicenseNotices.noticesURL() { NSWorkspace.shared.open(url) }
                                    }
                                    .buttonStyle(.dsSecondary)
                                    .disabled(LicenseNotices.noticesURL() == nil)
                                    .help(LicenseNotices.noticesURL() == nil
                                          ? "The acknowledgements file is only in the packaged app."
                                          : "Opens the list of open-source licenses.")
                                    Button("FileID license") {
                                        if let url = LicenseNotices.licenseURL() { NSWorkspace.shared.open(url) }
                                    }
                                    .buttonStyle(.dsSecondary)
                                    .disabled(LicenseNotices.licenseURL() == nil)
                                }
                            }

                            Hairline(color: Theme.hairlineSoft)

                            VStack(alignment: .leading, spacing: 8) {
                                Text("Logs").font(Theme.Font.body(12.5, .semibold))
                                Text("Detailed scan and app logs for troubleshooting.")
                                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                                HStack(spacing: 8) {
                                    Button("Show scan log in Finder") {
                                        NSWorkspace.shared.activateFileViewerSelecting([SettingsTab.scanLogURL])
                                    }
                                    .buttonStyle(.dsSecondary)
                                    Button("Open app log") {
                                        NSWorkspace.shared.open(SettingsTab.appLogURL)
                                    }
                                    .buttonStyle(.dsSecondary)
                                }
                            }

                            Hairline(color: Theme.hairlineSoft)

                            VStack(alignment: .leading, spacing: 8) {
                                Text("Danger zone").font(Theme.Font.body(12.5, .semibold)).foregroundStyle(Theme.danger)
                                Text("Permanently erase FileID’s library, settings, caches and small helper models. The large description models are kept.")
                                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                                Button(role: .destructive) {
                                    confirmFactoryReset = true
                                } label: {
                                    Text("Factory reset and quit")
                                }
                                .buttonStyle(.dsDanger)
                                .confirmationDialog(
                                    "Are you sure you want to completely erase FileID?",
                                    isPresented: $confirmFactoryReset,
                                    titleVisibility: .visible
                                ) {
                                    Button("Erase everything and quit", role: .destructive) {
                                        engine.factoryResetAndQuit()
                                    }
                                    Button("Cancel", role: .cancel) { }
                                } message: {
                                    Text("This permanently deletes everything FileID learned (tags, faces, descriptions), its settings, caches and the small downloaded helper models. The large description models are kept; remove them under Settings ▸ AI models. Your own files are not touched. This can’t be undone.")
                                }
                            }
                        }
                        .padding(.top, 8)
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "wrench.and.screwdriver")
                                .foregroundStyle(Theme.text3)
                            Text("Advanced").font(Theme.Font.body(14, .semibold))
                            Text("Background helper, storage, scan history, acknowledgements and logs.")
                                .font(Theme.Font.body(12))
                                .foregroundStyle(Theme.text3)
                        }
                    }
                }
            }
            .frame(maxWidth: 1100, alignment: .leading)
            .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.top, Theme.Metrics.pageInsetY).padding(.bottom, 24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .environment(\.helperDown, HelperGate.isDown(engine))
        .onAppear {
            Task { sessions = store.recentSessions() }
            #if DEBUG
            if SelfShot.expandAdvanced { showAdvanced = true }
            #endif
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
                Text(PathText.abbreviated(s.rootPath)).font(Theme.Font.mono(11.5)).lineLimit(1).truncationMode(.middle).help(s.rootPath)
                HStack(spacing: 12) {
                    Text(s.startedAt.formatted(date: .abbreviated, time: .shortened))
                    Text(s.status.capitalized)
                    if let n = s.lastFileIndex { Text(n == 1 ? "1 file" : "\(n.formatted()) files") }
                }
                .font(Theme.Font.body(12)).monospacedDigit().foregroundStyle(Theme.text3)
            }
            Spacer()
        }
        .padding(.vertical, 1)
    }

    private var connectionLabel: String {
        switch engine.state {
        case .starting:           return "Starting…"
        case .ready:              return "Ready"
        case .crashed(let why):   return "Stopped unexpectedly: \(why)"
        }
    }

    private var restartHelperRow: some View {
        HStack(spacing: 8) {
            Button("Restart helper now") { engine.restart() }
                .buttonStyle(.dsSecondary)
            MetaText("Applies the change right away. Stops a scan that is running.")
        }
    }

    @ViewBuilder
    private func infoRow(_ k: String, _ v: String, isPath: Bool = false) -> some View {
        HStack(alignment: .top) {
            Text(k).foregroundStyle(Theme.text3).frame(width: Theme.Font.scaled(160), alignment: .leading)
            Text(v).font(isPath ? Theme.Font.mono(12.5) : Theme.Font.body(13.5)).monospacedDigit().textSelection(.enabled)
            Spacer()
        }
        .font(Theme.Font.body(13.5))
    }

    static func duplicateSpaceLabel(reclaimableBytes bytes: Int64) -> String {
        bytes <= 0 ? "None" : ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    static var scanLogURL: URL { EngineLogFiles.revealTarget(state: AppSupportPath.fileID) }
    static var appLogURL: URL {
        AppSupportPath.fileID.appendingPathComponent("logs/app.log")
    }
}

// MARK: - Deep Analyze exclusions card

/// Folders skipped by a whole-library Deep Analyze pass. Nothing is removed from the library, so there is no purge state to track.
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

                Hairline(color: Theme.hairlineSoft)

                if let message {
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: message.isError ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                            .foregroundStyle(message.isError ? Theme.danger : Theme.delight)
                        Text(message.text)
                            .font(Theme.Font.body(12))
                            .foregroundStyle(message.isError ? Theme.danger : Theme.text3)
                        Spacer()
                    }
                }

                if settings.excludedFolders.isEmpty {
                    Text("No folders are skipped.")
                        .font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
                } else {
                    ForEach(settings.excludedFolders, id: \.self) { folder in
                        HStack(spacing: 8) {
                            Text(PathText.abbreviated(folder))
                                .help(folder)
                                .font(Theme.Font.mono(11.5))
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            DSIconButton("Stop skipping \(URL(fileURLWithPath: folder).lastPathComponent)", systemImage: "xmark", tint: Theme.text3) {
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
        case .listFull:
            message = ("Describe Files can skip at most \(DeepAnalyzeSettings.maxExcludedFolders) folders. Remove one first.", true)
        case .invalid:
            message = ("Couldn’t skip that folder. Choose a folder on this Mac.", true)
        }
    }
}

// MARK: - Model install card

struct ModelInstallCard: View {
    struct Row {
        let name: String
        var detail: String? = nil
        var technical: String? = nil
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
    @State private var confirmRemove = false
    @Environment(\.helperDown) private var helperDown

    private var sizeLabel: String { ModelSize.label(approxBytes) }

    var body: some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                Text(title).font(Theme.Font.body(14, .semibold))
                Text(blurb)
                    .font(Theme.Font.meta).foregroundStyle(Theme.text3)
                    .fixedSize(horizontal: false, vertical: true)
                Hairline(color: Theme.hairlineSoft)
                ForEach(rows, id: \.path) { rowView($0) }
                Hairline(color: Theme.hairlineSoft)
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
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.name).font(Theme.Font.body(13.5, .semibold))
                if let detail = row.detail {
                    Text(detail).font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                }
            }
            Spacer()
        }
        .help([row.technical, row.path.path].compactMap { $0 }.joined(separator: "\n"))
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
            MetaText("\(reason) About \(sizeLabel), downloaded once from the internet (huggingface.co).", size: 11.5)
            HStack(spacing: 8) {
                Button { installer.install() } label: {
                    Label("Download (~\(sizeLabel))", systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.dsPrimary)
                .disabled(helperDown)
                .help(helperDown ? HelperGate.downloadsUnavailableNote : "")
                .accessibilityLabel("Download \(title), about \(sizeLabel)")
                Spacer()
            }
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 6) {
                if fraction > 0 { ProgressView(value: fraction) } else { ProgressView() }
                HStack {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(message).font(Theme.Font.mono(11.5)).foregroundStyle(Theme.text3)
                        if let rate = installer.status.rateAndETA, !rate.isEmpty {
                            Text(rate).font(Theme.Font.mono(11.5)).foregroundStyle(Theme.text3)
                        }
                    }
                    Spacer()
                    Button("Cancel") { installer.cancel() }
                        .buttonStyle(.borderless).controlSize(.small)
                        .accessibilityLabel("Cancel downloading \(title)")
                }
            }
        case .extracting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                MetaText("Finishing…")
                Spacer()
            }
        case .installed(let bytes):
            HStack(spacing: 8) {
                MetaText("Ready · \(ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)) on this Mac", size: 11.5)
                Spacer()
                Button("Remove") { confirmRemove = true }
                    .buttonStyle(DSGhostButtonStyle(color: Theme.danger))
                    .padding(.trailing, -6)
                    .accessibilityLabel("Remove \(title)")
            }
        case .installFailed(let why):
            VStack(alignment: .leading, spacing: 6) {
                Text(why).font(Theme.Font.body(12)).foregroundStyle(Theme.danger)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button("Try again") { installer.install() }.buttonStyle(.dsSecondary)
                        .disabled(helperDown)
                        .help(helperDown ? HelperGate.downloadsUnavailableNote : "")
                        .accessibilityLabel("Try again to download \(title)")
                    Spacer()
                }
            }
        }
    }

}

/// One button for every required model that is still missing.
struct EssentialModelsCard: View {
    @State private var clip = CLIPModelInstaller.shared
    @State private var ramPlus = ModelInstaller.ramPlus
    @State private var bge = ModelInstaller.bge
    @State private var face = ModelInstaller.face
    @Environment(\.helperDown) private var helperDown

    private var items: [(installer: any ModelInstalling, bytes: Int64)] {
        [(clip, CLIPModelInstaller.approxDownloadBytes), (ramPlus, ramPlus.spec.approxBytes),
         (bge, bge.spec.approxBytes), (face, face.spec.approxBytes)]
    }
    private var missing: [(installer: any ModelInstalling, bytes: Int64)] {
        items.filter { !$0.installer.status.isInstalled }
    }

    var body: some View {
        let missing = missing
        let busy = missing.contains { $0.installer.status.inProgress }
        let idle = missing.filter { !$0.installer.status.inProgress }
        Panel(padding: 16) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("AI models").font(Theme.Font.body(14, .semibold))
                    MetaText(missing.isEmpty
                             ? "Search, tags, faces and documents are all downloaded."
                             : "Search, tags, faces and documents each need a small model. Without them, FileID still finds files by name and by text inside them.")
                    if helperDown && !missing.isEmpty {
                        MetaText(HelperGate.downloadsUnavailableNote)
                    }
                }
                Spacer()
                if !missing.isEmpty {
                    Button {
                        for item in idle { item.installer.install() }
                    } label: {
                        Label(idle.isEmpty ? "Downloading…" : "Download the essentials · about \(ByteCountFormatter.string(fromByteCount: idle.reduce(0) { $0 + $1.bytes }, countStyle: .file))",
                              systemImage: "arrow.down.circle.fill")
                    }
                    .buttonStyle(.dsPrimary)
                    .disabled(idle.isEmpty || helperDown)
                    .help(busy ? "Each model’s progress is shown in its card below" : "Downloads every missing model below, once, from the internet (huggingface.co)")
                }
            }
        }
        .onAppear { for item in items { item.installer.refreshStatus() } }
    }
}

extension ModelInstallCard {
    static var clip: ModelInstallCard {
        let installer = CLIPModelInstaller.shared
        return ModelInstallCard(
            installer: installer,
            title: "Search by description",
            blurb: "Lets you search photos by what’s in them, like “sunset at the beach”. Runs on this Mac.",
            rows: [
                Row(name: "Understands photos", detail: "Looks at each photo during a scan.", technical: "OpenCLIP ViT-B/32 image model (MIT)", path: CLIPModelInstaller.modelsRoot.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx")),
                Row(name: "Understands your search words", detail: "Matches what you type to what photos show.", technical: "OpenCLIP ViT-B/32 text model (MIT)", path: CLIPEmbeddingSpace.textModelURL),
                Row(name: "Word list", detail: "The words the search model knows.", technical: "BPE vocabulary (vocab.json + merges.txt)", path: CLIPEmbeddingSpace.textModelDirectory.appendingPathComponent("vocab.json")),
            ],
            approxBytes: CLIPModelInstaller.approxDownloadBytes,
            removeTitle: "Remove the Search by description model?",
            fallback: "Search goes back to matching names and tags until you download it again.")
    }

    static var ramPlus: ModelInstallCard {
        let installer = ModelInstaller.ramPlus
        return ModelInstallCard(
            installer: installer,
            title: "Photo tags",
            blurb: "Labels what’s in each photo (thousands of everyday things) so you can search and sort by it. Without it, FileID uses a simpler built-in tagger.",
            rows: [Row(name: "Photo tagger", detail: "Knows about 4,500 everyday things, in English.", technical: "RAM++ (Recognize Anything Plus), Swin-Large @384, Apache-2.0", path: installer.spec.primaryFile)],
            approxBytes: installer.spec.approxBytes,
            removeTitle: "Remove the Photo tags model?",
            fallback: "Tagging falls back to the simpler built-in tagger.")
    }

    static var bge: ModelInstallCard {
        let installer = ModelInstaller.bge
        return ModelInstallCard(
            installer: installer,
            title: "Documents",
            blurb: "Reads what documents are about, so Organize Folders puts a physics paper with your physics files. Without it, documents are grouped by file name.",
            rows: [Row(name: "Document reader", detail: "Works out what a document is about from its text.", technical: "BGE-small-en-v1.5 (MIT), 384-d text embedder, runs on the CPU", path: installer.spec.primaryFile)],
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
            blurb: "Recognizes the same person across your photos so People can group them. Runs on this Mac.",
            rows: [Row(name: "Face matcher", detail: "Tells which faces belong to the same person.", technical: "\(kind.displayName) · \(kind.subtitle) · \(kind.licenseName)", path: installer.spec.primaryFile)],
            approxBytes: installer.spec.approxBytes,
            removeTitle: "Remove face model?",
            fallback: "Grouping faces stops until you download it again.")
    }

    static var faceMesh: ModelInstallCard {
        let installer = ModelInstaller.faceMesh
        return ModelInstallCard(
            installer: installer,
            title: "Closed eyes in Best Takes",
            blurb: "Spots closed eyes more reliably when Best Takes picks the best photo of a moment. Runs on this Mac. Without it, FileID uses Apple’s built-in eye check.",
            rows: [Row(name: "Eye checker", detail: "Measures how open each eye is.", technical: "MediaPipe Face Mesh V2 (Apache-2.0), 478 face landmarks", path: installer.spec.primaryFile)],
            approxBytes: installer.spec.approxBytes,
            removeTitle: "Remove the closed-eyes model?",
            fallback: "Best Takes goes back to Apple’s built-in eye check.")
    }
}


/// What leaves FileID's own library: Spotlight gets names, tags and descriptions.
struct PrivacyCard: View {
    @AppStorage(AppSettings.spotlightKey) private var spotlight: Bool = AppSettings.spotlightDefault
    var body: some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Privacy").font(Theme.Font.body(14, .semibold))
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Show results in Spotlight").font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1)
                        MetaText("File names, tags and descriptions FileID wrote become searchable with ⌘Space by anyone using this Mac. Turning this off removes them from Spotlight.")
                    }
                    Spacer()
                    Toggle("", isOn: $spotlight).labelsHidden().toggleStyle(DSToggleStyle())
                        .accessibilityLabel("Show results in Spotlight")
                }
                MetaText("Nothing is sent anywhere. The only network use is a model download you start yourself.")
            }
        }
    }
}

/// Appearance: lava lamp Full / Calm / Off, and the page text size.
struct AppearanceCard: View {
    @AppStorage("lavaMode") private var lavaMode: String = LavaMode.full.rawValue
    @AppStorage(TextSize.storageKey) private var textSize: String = TextSize.standard.rawValue
    var body: some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Appearance").font(Theme.Font.body(14, .semibold))
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Lava lamp").font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1)
                        MetaText("Wax glows rise behind the panes; the light turns lavender while a model runs. Reduce Motion pauses it.")
                    }
                    .accessibilityHidden(true)
                    Spacer()
                    SegmentedBar(selection: $lavaMode, options: LavaMode.allCases.map { ($0.rawValue, $0.label) })
                        .accessibilityLabel("Lava lamp")
                        .accessibilityHint("Wax glows rise behind the panes; the light turns lavender while a model runs. Reduce Motion pauses it.")
                }
                Hairline(color: Theme.hairlineSoft).padding(.vertical, 4)
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Text size").font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1)
                        MetaText("File names, details, descriptions and chat. The title bar and sidebar stay the same size.")
                    }
                    .accessibilityHidden(true)
                    Spacer()
                    SegmentedBar(selection: $textSize, options: TextSize.allCases.map { ($0.rawValue, $0.title) })
                        .accessibilityLabel("Text size")
                        .accessibilityHint("Changes the size of text on every page. The title bar and sidebar stay the same size.")
                }
            }
        }
    }
}
