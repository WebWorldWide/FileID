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
//   FILEID_SELF_SHOT_VIEWS=<name>=<page>[?kind=doc&query=x&scroll=bottom|<points>&preview=<file name>];...   extra captures after the pages
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
}

@MainActor
enum SelfShot {
    static let plan: SelfShotPlan? = SelfShotPlan.from(
        environment: ProcessInfo.processInfo.environment,
        available: MainWindow.Tab.allCases.map(\.rawValue))
    static var isActive: Bool { plan != nil }
    private static var started = false

    static func run(engine: EngineClient, store: ReadStore, navigate: @escaping (String) -> Void,
                    openFolder: @escaping (URL, Bool) -> Void) {
        guard let plan, !started else { return }
        started = true
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
                    write(name: view.name, plan: plan, sheet: true)
                    if let main = window(), let sheet = main.attachedSheet { main.endSheet(sheet) }
                    try? await Task.sleep(for: .seconds(0.5))
                } else {
                    write(name: view.name, plan: plan)
                }
            }
            NSApp.terminate(nil)
            try? await Task.sleep(for: .seconds(3))
            exit(0)
        }
    }

    /// Scrolls the largest visible scroll view in the content area (right of the sidebar) to `offset` points, or to its end.
    private static func scrollContent(to offset: Double?) {
        guard let root = window()?.contentView else { return }
        var best: NSScrollView?
        func visit(_ view: NSView) {
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

    private static func waitUntil(timeout: Double, _ condition: @MainActor () -> Bool) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(250)) }
    }

    private static func window() -> NSWindow? {
        NSApp.windows.filter { $0.contentView != nil && !($0 is NSPanel) && $0.sheetParent == nil }
            .max { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }
    }

    private static func write(name: String, plan: SelfShotPlan, sheet: Bool = false) {
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
