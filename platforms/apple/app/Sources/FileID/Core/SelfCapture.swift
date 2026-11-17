#if DEBUG
// Debug-only self-capture: the app renders its own window to PNG files and quits,
// so a page can be inspected without any screen-recording permission. Mirrors
// `self_capture` in platforms/linux/src/app/src/window.rs. Not compiled into release builds.
//
//   FILEID_SELF_SHOT_DIR=<dir>     turns the mode on; <dir>/<page>.png per page
//   FILEID_SELF_SHOT_PAGES=a,b     page names or slugs (default: every page)
//   FILEID_SELF_SHOT_SETTLE=2      seconds to wait after each navigation
//   FILEID_SELF_SHOT_FOLDER=<dir>  library folder to open (and scan when the catalog is empty)
//   FILEID_SELF_SHOT_QUERY=<text>  searches Search & Moments (the Chat hand-off path) before its capture
//   FILEID_SELF_SHOT_VIEWS=<name>=<page>[?kind=doc&query=x&scroll=bottom|<points>&preview=<file name>&focus=<n key-view steps>];...   extra captures after the pages
//   FILEID_SELF_SHOT_CLUSTER=1     after the scan, run People's "Group faces" (the button's own call) and wait for it, so faces are embedded and grouped before the captures
//   FILEID_SELF_SHOT_ADVANCED=1    Settings opens with its Advanced card expanded (the Engine choice and the engine that is running)
//   FILEID_SELF_SHOT_DESCRIBE=<n>  after the pages, describe the first n photos with the active model (the Library selection button's call) and capture the Describe Files page and the status pill while it runs and when it ends
//   FILEID_SELF_SHOT_MODEL=<kind>  makes this the active description model before the Describe, Chat and Moments steps (for example qwen3_vl_2b)
//   FILEID_SELF_SHOT_CHAT=<q>|<q>  after that, ask each question in Chat (the composer's send) and capture the answer
//   FILEID_SELF_SHOT_CHAT_SCOPED=<q>  one more question attached to the first FILEID_SELF_SHOT_CHAT_SCOPE_N (default 6) photos, the selection check
//   FILEID_SELF_SHOT_MOMENTS_VIDEO=<name>  in Search & Moments, search for the name, open its first video and press Find moments (Quick); capture when the job ends
//   FILEID_SELF_SHOT_TOOLS=<query>  in File Tools, search, select the first FILEID_SELF_SHOT_TOOLS_N (default 2) results and press Preview; FILEID_SELF_SHOT_TOOLS_KIND=photo|video, FILEID_SELF_SHOT_TOOLS_DEST=<scratch folder>
//   FILEID_WIN_SIZE=WxH            content size of the window
import AppKit
import SwiftUI
import FileIDShared

/// One extra capture: a page plus optional Library kind filter, Library search text and a scroll to the bottom.
struct SelfShotView: Equatable {
    var name: String
    var page: String
    var kind: String?
    var query: String?
    var scrollToBottom = false
    var scrollOffset: Double?
    var preview: String?
    var focusSteps = 0

    /// "docs=Library?kind=doc;found=Library?query=invoice%20total;lower=Settings?scroll=bottom"
    static func parse(_ raw: String?, available: [String]) -> [SelfShotView] {
        (raw ?? "").split(separator: ";").compactMap { entry in
            guard let eq = entry.firstIndex(of: "=") else { return nil }
            let name = SelfShotPlan.slug(String(entry[..<eq]))
            let rest = entry[entry.index(after: eq)...]
            let pageText = rest.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
            guard !name.isEmpty, let page = available.first(where: { SelfShotPlan.slug($0) == SelfShotPlan.slug(String(pageText[0])) }) else { return nil }
            var view = SelfShotView(name: name, page: page)
            if pageText.count == 2 {
                for pair in pageText[1].split(separator: "&") {
                    let kv = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
                    guard kv.count == 2 else { continue }
                    let value = String(kv[1]).removingPercentEncoding ?? String(kv[1])
                    switch kv[0] {
                    case "kind": view.kind = value
                    case "query": view.query = value.isEmpty ? nil : value
                    case "focus": view.focusSteps = max(0, min(20, Int(value) ?? 0))
                    case "preview": view.preview = value.isEmpty ? nil : value
                    case "scroll": view.scrollToBottom = value == "bottom"; view.scrollOffset = Double(value)
                    default: break
                    }
                }
            }
            return view
        }
    }
}

struct SelfShotPlan: Equatable {
    var dir: URL
    var pages: [String]
    var folder: URL?
    var settle: Double
    var windowSize: CGSize?
    var query: String?
    var clusterFaces = false
    var views: [SelfShotView] = []

    static let dirKey = "FILEID_SELF_SHOT_DIR"

    static func from(environment: [String: String], available: [String]) -> SelfShotPlan? {
        guard let dir = environment[dirKey], !dir.isEmpty else { return nil }
        let settle = environment["FILEID_SELF_SHOT_SETTLE"].flatMap(Double.init).map { max(0, $0) } ?? 2
        let folder = environment["FILEID_SELF_SHOT_FOLDER"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0, isDirectory: true) }
        return SelfShotPlan(dir: URL(fileURLWithPath: dir, isDirectory: true),
                            pages: parsePages(environment["FILEID_SELF_SHOT_PAGES"], available: available),
                            folder: folder, settle: settle,
                            windowSize: parseSize(environment["FILEID_WIN_SIZE"]),
                            query: environment["FILEID_SELF_SHOT_QUERY"].flatMap { $0.isEmpty ? nil : $0 },
                            clusterFaces: environment["FILEID_SELF_SHOT_CLUSTER"] == "1",
                            views: SelfShotView.parse(environment["FILEID_SELF_SHOT_VIEWS"], available: available))
    }

    /// "Search & Moments" -> "search-moments".
    static func slug(_ name: String) -> String {
        var out = ""
        for ch in name.lowercased() {
            if ch.isLetter || ch.isNumber { out.append(ch) } else if !out.hasSuffix("-") { out.append("-") }
        }
        return out.trimmingCharacters(in: CharacterSet(charactersIn: "-"))
    }

    /// Empty or missing means every page. Unknown names are dropped; duplicates keep the first.
    static func parsePages(_ raw: String?, available: [String]) -> [String] {
        let wanted = (raw ?? "").split(separator: ",").map { slug(String($0)) }.filter { !$0.isEmpty }
        guard !wanted.isEmpty else { return available }
        var seen = Set<String>()
        return wanted.compactMap { want in
            guard let match = available.first(where: { slug($0) == want }), seen.insert(match).inserted else { return nil }
            return match
        }
    }

    static func parseSize(_ raw: String?) -> CGSize? {
        guard let raw else { return nil }
        let parts = raw.lowercased().split(separator: "x")
        guard parts.count == 2, let w = Double(parts[0]), let h = Double(parts[1]), w >= 200, h >= 200 else { return nil }
        return CGSize(width: w, height: h)
    }
}

extension Notification.Name {
    static let selfShotOpenPreview = Notification.Name("FileID.selfShotOpenPreview")
    static let selfShotSendChat = Notification.Name("FileID.selfShotSendChat")
    static let selfShotFindMoments = Notification.Name("FileID.selfShotFindMoments")
}

@MainActor
enum SelfShot {
    static let plan: SelfShotPlan? = SelfShotPlan.from(
        environment: ProcessInfo.processInfo.environment,
        available: MainWindow.Tab.allCases.map(\.rawValue))
    static var isActive: Bool { plan != nil }
    static let expandAdvanced = ProcessInfo.processInfo.environment["FILEID_SELF_SHOT_ADVANCED"] == "1" && plan != nil
    private static weak var engineRef: EngineClient?
    private static var started = false

    static func run(engine: EngineClient, store: ReadStore, navigate: @escaping (String) -> Void,
                    openFolder: @escaping (URL, Bool) -> Void) {
        guard let plan, !started else { return }
        started = true
        engineRef = engine
        NSApp.setActivationPolicy(.accessory)
        Task { @MainActor in
            try? FileManager.default.createDirectory(at: plan.dir, withIntermediateDirectories: true)
            if let size = plan.windowSize { window()?.setContentSize(size) }
            await waitUntil(timeout: 90) { if case .ready = engine.state { return true } else { return false } }
            if let folder = plan.folder {
                await waitUntil(timeout: 3) { store.totalFiles > 0 }
                let needsScan = store.totalFiles == 0
                openFolder(folder, needsScan)
                if needsScan {
                    await waitUntil(timeout: 60) { engine.lastProgress.map { [.discovering, .tagging, .postScan].contains($0.phase) } ?? false }
                    navigate(plan.pages.first ?? MainWindow.Tab.library.rawValue)
                    try? await Task.sleep(for: .seconds(min(plan.settle, 1.0)))
                    let scanning = engine.lastProgress.map { [.discovering, .tagging, .postScan].contains($0.phase) } ?? false
                    write(name: "\(SelfShotPlan.slug(plan.pages.first ?? "library"))-scanning\(scanning ? "" : "-finished")", plan: plan)
                    await waitUntil(timeout: 600) { engine.lastProgress.map { [.completed, .failed, .cancelled].contains($0.phase) } ?? false }
                    let done = Date()
                    await waitUntil(timeout: 10) { store.totalFiles >= (engine.lastProgress?.processed ?? 0) }
                    FileHandle.standardError.write(Data("self-shot: files counter \(store.totalFiles) \(Int(Date().timeIntervalSince(done) * 1000)) ms after the scan completed\n".utf8))
                }
            }
            if plan.clusterFaces {
                await waitUntil(timeout: 30) { store.totalFiles > 0 }
                let before = engine.lastFaceClustering?.personCount
                engine.runFaceClustering()
                await waitUntil(timeout: 900) { !engine.faceClusteringInFlight }
                let people = engine.lastFaceClustering.map { String($0.personCount) } ?? "none"
                FileHandle.standardError.write(Data("self-shot: face clustering finished, persons \(people) (before \(before.map(String.init) ?? "none"))\n".utf8))
                store.notifyChanged()
                try? await Task.sleep(for: .seconds(1))
            }
            for page in plan.pages {
                if page == MainWindow.Tab.moments.rawValue, let query = plan.query { ChatHandoff.shared.momentsQuery = query }
                navigate(page)
                try? await Task.sleep(for: .seconds(plan.settle))
                write(name: SelfShotPlan.slug(page), plan: plan)
            }
            for view in plan.views {
                let defaults = UserDefaults.standard
                defaults.set(view.kind ?? "", forKey: "library.kindFilter")
                if let query = view.query { defaults.set(query, forKey: LibraryView.queryHandoffKey) }
                navigate(view.page)
                try? await Task.sleep(for: .seconds(plan.settle))
                if view.scrollToBottom || view.scrollOffset != nil { scrollContent(to: view.scrollOffset); try? await Task.sleep(for: .seconds(0.6)) }
                if let name = view.preview {
                    NotificationCenter.default.post(name: .selfShotOpenPreview, object: name)
                    try? await Task.sleep(for: .seconds(plan.settle))
                    if view.focusSteps > 0 { focusControl(steps: view.focusSteps, in: window()?.attachedSheet ?? window()); try? await Task.sleep(for: .seconds(0.6)) }
                    write(name: view.name, plan: plan, sheet: true)
                    if let main = window(), let sheet = main.attachedSheet { main.endSheet(sheet) }
                    try? await Task.sleep(for: .seconds(0.5))
                } else {
                    if view.focusSteps > 0 { focusControl(steps: view.focusSteps, in: window()); try? await Task.sleep(for: .seconds(0.6)) }
                    write(name: view.name, plan: plan)
                }
            }
            await runExtras(engine: engine, store: store, navigate: navigate, plan: plan)
            NSApp.terminate(nil)
            try? await Task.sleep(for: .seconds(3))
            exit(0)
        }
    }

    private static func env(_ key: String) -> String? {
        ProcessInfo.processInfo.environment[key].flatMap { $0.isEmpty ? nil : $0 }
    }

    private static func note(_ text: String) {
        FileHandle.standardError.write(Data("self-shot: \(text)\n".utf8))
    }

    /// Describe, Chat, Moments and File Tools runs, each through the call its button makes.
    private static func runExtras(engine: EngineClient, store: ReadStore, navigate: @escaping (String) -> Void, plan: SelfShotPlan) async {
        if let raw = env("FILEID_SELF_SHOT_MODEL"), let kind = AIModelKind(rawValue: raw) { DeepAnalyzeSettings.shared.activeKind = kind }
        let photos = store.files(limit: 2000).filter { FileTypes.images.contains(URL(fileURLWithPath: $0.pathText).pathExtension.lowercased()) }
        if let n = env("FILEID_SELF_SHOT_DESCRIBE").flatMap(Int.init), n > 0 {
            navigate(MainWindow.Tab.deep.rawValue)
            try? await Task.sleep(for: .seconds(plan.settle))
            write(name: "describe-before", plan: plan)
            let kind = DeepAnalyzeSettings.shared.activeKind.rawValue
            let ids = photos.prefix(n).map(\.id)
            let started = engine.deepAnalyzeSelection(fileIDs: Array(ids), modelKind: kind)
            note("describe started \(started) for \(ids.count) files with \(kind)")
            await waitUntil(timeout: 300) { engine.deepAnalyzeProgress != nil || !engine.deepAnalyzeInFlight }
            try? await Task.sleep(for: .seconds(1))
            write(name: "describe-running", plan: plan)
            var shots = 1
            let deadline = Date().addingTimeInterval(900)
            while engine.deepAnalyzeInFlight, Date() < deadline {
                try? await Task.sleep(for: .seconds(1))
                if engine.deepAnalyzeInFlight, let d = engine.deepAnalyzeProgress, d.processed >= shots, shots < 3 { shots += 1; write(name: "describe-running-\(shots)", plan: plan) }
            }
            try? await Task.sleep(for: .seconds(1.5))
            note("describe ended; complete \(engine.deepAnalyzeComplete.map { "\($0.processed)" } ?? "none")")
            write(name: "describe-done", plan: plan)
            navigate(MainWindow.Tab.library.rawValue)
            try? await Task.sleep(for: .seconds(plan.settle))
            write(name: "describe-library", plan: plan)
        }
        if let raw = env("FILEID_SELF_SHOT_CHAT") {
            navigate(MainWindow.Tab.chat.rawValue)
            try? await Task.sleep(for: .seconds(plan.settle))
            write(name: "chat-empty", plan: plan)
            for (i, question) in raw.split(separator: "|").enumerated() {
                await askChat(String(question), name: "chat-answer-\(i + 1)", engine: engine, plan: plan)
            }
        }
        if let question = env("FILEID_SELF_SHOT_CHAT_SCOPED") {
            let count = env("FILEID_SELF_SHOT_CHAT_SCOPE_N").flatMap(Int.init) ?? 6
            let ids = Array(photos.prefix(count).map(\.id))
            ChatHandoff.shared.chatScope = ChatScope.selected(ids, kinds: ids.map { _ in "image" })
            navigate(MainWindow.Tab.chat.rawValue)
            try? await Task.sleep(for: .seconds(plan.settle))
            await askChat(question, name: "chat-selection", engine: engine, plan: plan)
        }
        if let name = env("FILEID_SELF_SHOT_MOMENTS_VIDEO") {
            ChatHandoff.shared.momentsQuery = name
            navigate(MainWindow.Tab.moments.rawValue)
            try? await Task.sleep(for: .seconds(plan.settle))
            NotificationCenter.default.post(name: .selfShotFindMoments, object: nil)
            try? await Task.sleep(for: .seconds(2))
            write(name: "moments-queued", plan: plan)
            await waitUntil(timeout: 420) { engine.catalogJobs.contains { $0.kind == "timelineSample" && ["completed", "failed", "cancelled"].contains($0.state) } }
            note("timeline jobs: \(engine.catalogJobs.map { "\($0.kind) \($0.state)" }.joined(separator: ", "))")
            try? await Task.sleep(for: .seconds(3))
            write(name: "moments-timeline", plan: plan)
        }
        if let query = env("FILEID_SELF_SHOT_TOOLS"), let session = ChatHandoff.shared.toolsSession {
            navigate(MainWindow.Tab.tools.rawValue)
            try? await Task.sleep(for: .seconds(plan.settle))
            write(name: "tools-empty", plan: plan)
            session.kind = env("FILEID_SELF_SHOT_TOOLS_KIND") ?? "photo"
            session.query = query
            session.searchID = UUID().uuidString
            _ = engine.send(.catalogRequest(request: CatalogRequest(requestID: session.searchID, action: "search", query: query)))
            await waitUntil(timeout: 30) { !session.allHits.isEmpty }
            try? await Task.sleep(for: .seconds(1))
            let n = env("FILEID_SELF_SHOT_TOOLS_N").flatMap(Int.init) ?? 2
            session.selection = Set(session.hits.prefix(n).map(\.fileID))
            session.destination = env("FILEID_SELF_SHOT_TOOLS_DEST") ?? ""
            try? await Task.sleep(for: .seconds(1))
            write(name: "tools-selected", plan: plan)
            let request = ToolRequest(requestID: UUID().uuidString, action: "preview", fileIDs: session.selection.sorted(),
                                      destination: session.destination, recipe: session.recipe, destinationBookmark: nil)
            session.pending = request.requestID; session.pendingAction = request.action
            session.pendingEngineGeneration = engine.engineResetSignal
            _ = engine.send(.toolRequest(request: request))
            await waitUntil(timeout: 60) { session.pending.isEmpty }
            try? await Task.sleep(for: .seconds(2))
            note("tools preview: \(session.message) outputs \(session.outputs.count)")
            write(name: "tools-preview", plan: plan)
        }
        await waitUntil(timeout: 120) {
            !engine.deepAnalyzeInFlight && !engine.faceClusteringInFlight && !engine.catalogJobs.contains { $0.state == "running" || $0.state == "queued" }
        }
        navigate(MainWindow.Tab.library.rawValue)
        try? await Task.sleep(for: .seconds(plan.settle + 3))
        write(name: "final-idle", plan: plan)
    }

    private static func askChat(_ question: String, name: String, engine: EngineClient, plan: SelfShotPlan) async {
        UserDefaults.standard.set(question, forKey: "chatDraft")
        try? await Task.sleep(for: .seconds(0.5))
        NotificationCenter.default.post(name: .selfShotSendChat, object: nil)
        try? await Task.sleep(for: .seconds(1))
        await waitUntil(timeout: 600) {
            guard let r = engine.latestChatResponse else { return false }
            return !["retrieving", "queued", "streaming"].contains(r.status)
        }
        try? await Task.sleep(for: .seconds(2))
        note("chat answer status \(engine.latestChatResponse?.status ?? "none")")
        write(name: name, plan: plan)
    }

    /// Scrolls the largest visible scroll view in the content area (right of the sidebar) to `offset` points, or to its end.
    private static func scrollContent(to offset: Double?) {
        guard let root = window()?.contentView else { return }
        var best: NSScrollView?
        func visit(_ view: NSView) {
            if view.isHidden || view.alphaValue == 0 || view.layer?.opacity == 0 { return }
            if let scroll = view as? NSScrollView, let document = scroll.documentView, !scroll.isHiddenOrHasHiddenAncestor,
               document.frame.height > scroll.contentView.bounds.height + 1, scroll.convert(scroll.bounds, to: nil).minX > 200,
               scroll.bounds.width * scroll.bounds.height > (best.map { $0.bounds.width * $0.bounds.height } ?? 0) { best = scroll }
            view.subviews.forEach(visit)
        }
        visit(root)
        guard let scroll = best, let document = scroll.documentView else {
            FileHandle.standardError.write(Data("self-shot: no scroll view to scroll\n".utf8)); return
        }
        FileHandle.standardError.write(Data("self-shot: scrolling \(Int(document.frame.height)) tall document in \(Int(scroll.bounds.height)) pt view\n".utf8))
        let end = max(0, document.frame.height - scroll.contentView.bounds.height)
        let y = document.isFlipped ? min(end, offset ?? end) : max(0, end - (offset ?? end))
        scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    /// Moves AppKit's key view loop forward `steps` times, the way Tab would, without sending an event.
    private static func focusControl(steps: Int, in window: NSWindow?) {
        guard let window else { return }
        window.makeFirstResponder(nil)
        for _ in 0..<steps { window.selectNextKeyView(nil) }
        let name = window.firstResponder.map { String(describing: type(of: $0)) } ?? "nil"
        FileHandle.standardError.write(Data("self-shot: first responder after focus steps: \(name)\n".utf8))
    }

    private static func waitUntil(timeout: Double, _ condition: @MainActor () -> Bool) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(250)) }
    }

    private static func window() -> NSWindow? {
        NSApp.windows.filter { $0.contentView != nil && !($0 is NSPanel) && $0.sheetParent == nil }
            .max { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }
    }

    private static func write(name: String, plan: SelfShotPlan, sheet: Bool = false) {
        if let p = engineRef?.lastProgress {
            FileHandle.standardError.write(Data("self-shot: progress at \(name): \(p.phase) \(p.processed)/\(p.total) discovered \(p.discovered)\n".utf8))
        }
        let url = plan.dir.appendingPathComponent("\(name).png")
        guard let window = (sheet ? window()?.attachedSheet : nil) ?? window(), let view = window.contentView?.superview ?? window.contentView else {
            FileHandle.standardError.write(Data("self-shot: no window for \(name)\n".utf8)); return
        }
        view.layoutSubtreeIfNeeded()
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: rep)
        if let png = rep.representation(using: .png, properties: [:]), (try? png.write(to: url)) != nil {
            FileHandle.standardError.write(Data("self-shot: wrote \(url.lastPathComponent) \(rep.pixelsWide)x\(rep.pixelsHigh)\n".utf8))
        }
    }
}
#endif
