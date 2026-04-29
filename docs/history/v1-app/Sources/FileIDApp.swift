import SwiftUI
import AppKit
import SwiftData

@main
struct FileIDApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    // Build the container once at launch. If the on-disk store can't migrate to the
    // current schema (e.g. new fields on FileRecord, new ScanSession model), delete it
    // and start fresh rather than crashing.
    let modelContainer: ModelContainer = {
        let schema = Schema([FileRecord.self, PersonRecord.self, ScanSession.self])
        do {
            return try ModelContainer(for: schema)
        } catch {
            // Migration failed — wipe the old store and recreate it clean.
            if let support = FileManager.default.urls(
                for: .applicationSupportDirectory, in: .userDomainMask
            ).first, let enumerator = FileManager.default.enumerator(
                at: support, includingPropertiesForKeys: nil
            ) {
                for case let url as URL in enumerator {
                    let name = url.lastPathComponent
                    if name == "default.store"
                        || name == "default.store-wal"
                        || name == "default.store-shm" {
                        try? FileManager.default.removeItem(at: url)
                    }
                }
            }
            do {
                return try ModelContainer(for: schema)
            } catch let retryError {
                fatalError("FileID: SwiftData store unrecoverable after wipe: \(retryError)")
            }
        }
    }()

    var body: some Scene {
        WindowGroup {
            MainWindowView()
                .frame(minWidth: 1200, minHeight: 800)
                // `.underWindowBackground` fills the full window (including the
                // split view's toolbar strip) with a single opaque dark surface.
                // `.hudWindow` left a white gap above the content in full-screen
                // mode because the split view's internal toolbar region rendered
                // with the system default on top of the HUD material.
                .background(VisualEffectView(material: .underWindowBackground, blendingMode: .behindWindow))
                .ignoresSafeArea()
                .modelContainer(modelContainer)
        }
        // NOTE: deliberately NOT `.windowStyle(.hiddenTitleBar)`. That style
        // removes the entire titlebar, which takes the standard close /
        // minimize / zoom traffic-light buttons with it. We want a *transparent*
        // titlebar (so the LavaLamp / underWindowBackground material extends
        // to the top edge) while keeping the three buttons visible — handled
        // in AppDelegate.applicationDidFinishLaunching via
        // `titlebarAppearsTransparent = true` + `titleVisibility = .hidden`
        // + `.fullSizeContentView`.
        .commands {
            SidebarCommands()
            CommandGroup(after: .appInfo) {
                Button("About FileID Professional") {
                    let info = Bundle.main.infoDictionary
                    let version = info?["CFBundleShortVersionString"] as? String ?? "dev"
                    let build = info?["CFBundleVersion"] as? String ?? "local"
                    let alert = NSAlert()
                    alert.messageText = "FileID Professional"
                    alert.informativeText = "Version \(version) (build \(build))\nOn-device AI file organization for macOS."
                    alert.alertStyle = .informational
                    alert.addButton(withTitle: "OK")
                    alert.runModal()
                }
            }
            CommandGroup(replacing: .newItem) { }
            CommandMenu("File") {
                Button("Open Folder…") {
                    NotificationCenter.default.post(name: .fileIDOpenFolder, object: nil)
                }
                .keyboardShortcut("o", modifiers: .command)
                Button("Rescan Current Folder") {
                    NotificationCenter.default.post(name: .fileIDRescan, object: nil)
                }
                .keyboardShortcut("r", modifiers: .command)
            }
        }
    }
}

// Visual Effect View wrapper to bring AppKit's native vibrant materials to SwiftUI
struct VisualEffectView: NSViewRepresentable {
    var material: NSVisualEffectView.Material
    var blendingMode: NSVisualEffectView.BlendingMode
    
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        return view
    }
    
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
    }
}

// AppDelegate is MainActor-isolated — every method an NSApplicationDelegate
// implements is called by AppKit on the main thread, and we touch NSWindow /
// NSApplication APIs that are themselves @MainActor. Marking the class
// @MainActor lets us call our private helpers (configureMainWindow,
// reportOrphanCrash, tailScanLog) without per-call await ceremony.
@MainActor
class AppDelegate: NSObject, NSApplicationDelegate {
    var appViewModel: AppViewModel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Window setup runs in two passes:
        //   1. Synchronous: most apps work here, fast path.
        //   2. Async retry: belt + suspenders for the case where SwiftUI
        //      hasn't fully realized the WindowGroup's NSWindow yet at the
        //      moment AppDelegate fires. On macOS 26 the WindowGroup can
        //      be slow to attach a window, so the sync pass occasionally
        //      operates on `windows.first` = nil or an auxiliary panel.
        configureMainWindow()
        Task { @MainActor [weak self] in
            // ~50 ms gives SwiftUI a chance to realize the window without
            // perceptible launch lag.
            try? await Task.sleep(for: .milliseconds(50))
            self?.configureMainWindow()
        }

        // Seed deepAnalyzeEnabled the first time this user launches. Low-RAM
        // Macs default to OFF so an untouched install can't Jetsam itself by
        // auto-firing a 2–3 GB VLM load immediately after face clustering.
        // Users can still flip the toggle in Settings; we only seed when the
        // key is absent.
        if UserDefaults.standard.object(forKey: "deepAnalyzeEnabled") == nil {
            UserDefaults.standard.set(
                Hardware.deepAnalyzeAutoDefaultOn,
                forKey: "deepAnalyzeEnabled"
            )
        }

        // BEFORE anything else: check for an orphan CrashSentinel marker.
        // If the previous run died hard (SIGABRT / SIGKILL / runtime abort),
        // applicationWillTerminate never fired, and the marker file still
        // sits on disk with the phase + subject that was in flight. Read it,
        // compose a crash report, and clear the marker BEFORE we write a new
        // one for this launch.
        if let orphan = CrashSentinel.readOrphan() {
            reportOrphanCrash(orphan)
        }
        CrashSentinel.set(phase: "launch")

        // Launch-time readout of the RAM/core-scaled caps. Lets us confirm in
        // one glance that the scan engine is using the hardware it was handed.
        NSLog(
            "FileID hardware: RAM=\(Int(Hardware.physicalMemoryGB))GB "
            + "cores=\(Hardware.coreCount) Pcores=\(Hardware.performanceCoreCount) "
            + "workers=\(Hardware.workerCap) visionCeiling=\(Hardware.visionCeilingMB)MB "
            + "thumbCache=\(Hardware.thumbnailCacheMB)MB saveEvery=\(Hardware.saveEvery)"
        )

        // Face-clustering crash recovery. If an in-flight marker survived a
        // prior crash AND its attempt count hit the threshold, this call
        // permanently skips that fileID and deletes its FacePrintCache
        // entries — breaking the stale-print feedback loop that caused the
        // user's three consecutive clustering crashes.
        Task.detached(priority: .userInitiated) {
            if let escalated = await ClusterCircuitBreaker.shared.recoverFromCrash() {
                NSLog("FileID: face-clustering circuit breaker escalated fileID \(escalated) to the permanent skip-list")
            }
        }

        // Removed Batch 17's eager CLIP preload — it raced with scan workers
        // when a user picked a folder immediately after launch, causing 14×
        // concurrent MLModel(contentsOf:) calls and a throughput collapse
        // (21 files/s → 0.2 files/s). MobileCLIPService now serializes the
        // load properly so the lazy first-call path is safe — pay the 1-2 s
        // cost once on first use, but no eager preload contention.
    }

    // Window setup, factored so we can call it twice (sync + async) to
    // catch both the fast-path case and the case where SwiftUI's WindowGroup
    // hasn't realized its NSWindow yet at AppDelegate-launch time.
    private func configureMainWindow() {
        // Pick the largest visible content-view window — auxiliary panels
        // (NSPanel for color picker, etc.) shouldn't be touched.
        let candidate = NSApplication.shared.windows.first { window in
            window.styleMask.contains(.titled)
                && window.isVisible
                && !(window is NSPanel)
        } ?? NSApplication.shared.windows.first
        guard let window = candidate else { return }

        window.isOpaque = false
        window.backgroundColor = .clear
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.styleMask.insert(.fullSizeContentView)

        // Standard close / minimize / zoom buttons must remain visible. Some
        // SwiftUI modifiers (`.toolbar(.hidden, for: .windowToolbar)` in
        // particular) can sneakily hide the entire titlebar layer that
        // contains them. Forcing isHidden = false here guarantees they're
        // always visible regardless of what the SwiftUI side did.
        window.standardWindowButton(.closeButton)?.isHidden = false
        window.standardWindowButton(.miniaturizeButton)?.isHidden = false
        window.standardWindowButton(.zoomButton)?.isHidden = false
    }

    // Fires on ⌘Q / menu Quit / graceful logout. Does NOT fire on abort,
    // SIGKILL, or Swift-runtime aborts — which is exactly the behaviour we
    // want. A missing marker next launch means "clean exit"; a present
    // marker means "previous run died."
    func applicationWillTerminate(_ notification: Notification) {
        CrashSentinel.clear()
    }

    // Compose a single stanza in ~/Library/Logs/FileID/crash.log with the
    // marker contents + the tail of scan.log. Meant to be the one file the
    // user pastes to us after a crash — enough context to pinpoint the
    // in-flight operation without having to parse the whole scan log.
    private func reportOrphanCrash(_ m: CrashSentinel.Marker) {
        guard let dir = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)
            .first?.appendingPathComponent("Logs/FileID", isDirectory: true) else { return }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let crashURL = dir.appendingPathComponent("crash.log")
        let tail = tailScanLog(lines: 200)
        let now = ISO8601DateFormatter().string(from: Date())
        let header = """

        ======================================================================
        CRASH DETECTED at \(now)
        Previous run:
          started: \(m.startedAt)
          pid:     \(m.pid)
          phase:   \(m.phase)
          subject: \(m.subject ?? "<none>")
          batch:   \(m.lastBatch.map(String.init) ?? "<none>")
        Tail of scan.log (last 200 lines):
        ----------------------------------------------------------------------
        """
        let footer = "\n======================================================================\n"
        let blob = Data((header + "\n" + tail + footer).utf8)
        if let h = try? FileHandle(forWritingTo: crashURL) {
            defer { try? h.close() }
            _ = try? h.seekToEnd()
            try? h.write(contentsOf: blob)
            try? h.synchronize()
        } else {
            try? blob.write(to: crashURL, options: .atomic)
        }
        // Also mirror a short line to scan.log so the durable transcript
        // reflects that a crash recovery happened.
        MediaProcessor.appendScanLogExternal(
            "CrashSentinel: previous run died in phase=\(m.phase) subject=\(m.subject ?? "<none>") (pid=\(m.pid))"
        )
    }

    // Reads the tail of scan.log. Caps at ~256 KB to keep crash.log bounded
    // even if someone leaves a very long scan running. Splits on newlines
    // and returns the last `lines` entries.
    private func tailScanLog(lines: Int) -> String {
        guard let dir = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)
            .first?.appendingPathComponent("Logs/FileID", isDirectory: true) else { return "<scan.log missing>" }
        let url = dir.appendingPathComponent("scan.log")
        guard let handle = try? FileHandle(forReadingFrom: url) else { return "<scan.log missing>" }
        defer { try? handle.close() }
        let size = (try? handle.seekToEnd()) ?? 0
        let readBytes: UInt64 = 256 * 1024
        let offset = size > readBytes ? size - readBytes : 0
        try? handle.seek(toOffset: offset)
        let data = (try? handle.readToEnd()) ?? Data()
        let text = String(data: data, encoding: .utf8) ?? ""
        let split = text.split(separator: "\n", omittingEmptySubsequences: false)
        return split.suffix(lines).joined(separator: "\n")
    }
}

// MARK: - Notification Names

extension Notification.Name {
    static let fileIDOpenFolder         = Notification.Name("fileIDOpenFolder")
    static let fileIDRescan             = Notification.Name("fileIDRescan")
    static let fileIDOpenAIModelSettings = Notification.Name("fileIDOpenAIModelSettings")
}
