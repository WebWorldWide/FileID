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
    @State private var showAdvanced = false
    @State private var sessions: [ReadStore.ScanSessionRow] = []

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Settings").font(.largeTitle.bold())

                // ─── User-facing settings (always visible) ───────────────

                GlassCard {
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

                // AI Models — visible because users genuinely care about
                // which models are installed and download status.
                CLIPSemanticSearchCard()

                DeepAnalyzeModelPickerCard(engine: engine)
                FaceEmbedderCard(engine: engine, store: store)
                privacyCard

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
                                infoRow("Duplicate groups", "\(store.totalDuplicateGroups)")
                                infoRow("Reclaimable",   String(format: "%.1f MB", store.totalReclaimableMB))
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
            sessions = store.recentSessions()
        }
        .onChange(of: showAdvanced) { _, expanded in
            if expanded { sessions = store.recentSessions() }
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

    /// Privacy disclosure card. Explicit about what's stored where —
    /// matches Apple's HIG guidance and supports an ADA "Inclusivity"
    /// case (transparent local-first behavior).
    private var privacyCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 6) {
                    Image(systemName: "lock.shield")
                        .foregroundStyle(.green)
                    Text("Privacy").font(.headline)
                    Spacer()
                    Text("100% on-device")
                        .font(.caption.bold())
                        .foregroundStyle(.green)
                        .padding(.horizontal, 8).padding(.vertical, 3)
                        .background(Capsule().fill(Color.green.opacity(0.15)))
                }
                Text("FileID never sends your photos, captions, names, or any data to any server. Everything runs on your Mac. No cloud, no analytics, no telemetry.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Divider().opacity(0.3)
                privacyRow(icon: "photo",
                            title: "Your photos",
                            detail: "Stay where you put them. FileID reads them; nothing is uploaded.")
                privacyRow(icon: "person.2.crop.square.stack",
                            title: "Faces + names",
                            detail: "Stored only in FileID's local database. Names you type are never transmitted.")
                privacyRow(icon: "text.below.photo",
                            title: "Captions + smart names",
                            detail: "Generated by an on-device Vision-Language Model. Apple Neural Engine + Metal. No internet round-trip.")
                privacyRow(icon: "internaldrive",
                            title: "Where it lives",
                            detail: "~/Library/Application Support/FileID/. Open the database in the Advanced section above. Delete the folder to remove all FileID data.")
                privacyRow(icon: "antenna.radiowaves.left.and.right.slash",
                            title: "Network use",
                            detail: "FileID only contacts the network to download AI models you opt-in to (Hugging Face / Apple). Once installed, models never re-contact the source.")
            }
        }
    }

    @ViewBuilder
    private func privacyRow(icon: String, title: String, detail: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: icon)
                .foregroundStyle(.green)
                .font(.callout)
                .frame(width: 18, alignment: .center)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.callout.bold())
                Text(detail).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
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

    static var modelsFolderURL: URL { AppSupportPath.models }
    static var scanLogURL: URL {
        AppSupportPath.fileID.appendingPathComponent("logs/scan.jsonl")
    }
    static var appLogURL: URL {
        AppSupportPath.fileID.appendingPathComponent("logs/app.log")
    }
}

// MARK: - CLIP semantic-search card

/// Settings card for the CLIP semantic-search tier. State-driven —
/// shows install status, download/extract progress, and the manual
/// "install from local zip" fallback.
struct CLIPSemanticSearchCard: View {
    @State private var installer = CLIPModelInstaller.shared
    @State private var confirmUninstall = false

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — semantic search (CLIP)").font(.headline)
                Text("Type natural-language searches like \"sunset at the beach\" and FileID ranks every photo by visual relevance. Uses MobileCLIP-S2 — runs entirely on your Mac.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Divider().opacity(0.3)

                // Per-file install state.
                fileStatusRow(
                    name: "MobileCLIP-S2 (image)",
                    url: CLIPModelInstaller.modelsRoot
                        .appendingPathComponent("mobileclip_image/mobileclip_s2_image.mlpackage")
                )
                fileStatusRow(
                    name: "MobileCLIP-S2 (text)",
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
            Button("Remove", role: .destructive) { installer.uninstall() }
            Button("Keep", role: .cancel) {}
        } message: {
            Text("Frees ~350 MB. Semantic search will revert to keyword search until you reinstall.")
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
                    Text("~210 MB download from huggingface.co (Apple's MobileCLIP repo + OpenAI's BPE vocabulary).")
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

        case .downloading(let frac, let msg):
            VStack(alignment: .leading, spacing: 6) {
                if frac > 0 {
                    ProgressView(value: frac)
                } else {
                    ProgressView()
                }
                HStack {
                    Text(msg).font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
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
        let installed = FileManager.default.fileExists(atPath: url.path)
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
struct FaceEmbedderCard: View {
    let engine: EngineClient
    let store: ReadStore
    @State private var installer = ArcFaceModelInstaller.shared
    @State private var confirmUninstall: FaceEmbedderKind?

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("AI Models — face recognition").font(.headline)
                Text("On-device face embedder for clustering people. Pre-converted from Buffalo (Immich) ONNX — install with one click, no Python required.")
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

        case .downloading(let frac, let msg):
            VStack(alignment: .leading, spacing: 4) {
                if frac > 0 {
                    ProgressView(value: frac)
                } else {
                    ProgressView()
                }
                HStack {
                    Text(msg).font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
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
