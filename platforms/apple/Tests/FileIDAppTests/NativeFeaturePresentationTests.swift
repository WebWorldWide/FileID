import AppKit
import SwiftUI
import Testing
import FileIDShared
@testable import FileID

@Suite("Native feature presentation", .serialized)
struct NativeFeaturePresentationTests {
    @MainActor
    private func capture<V: View>(_ view: V, name: String, size: NSSize, root: URL) async throws {
        let host = NSHostingView(rootView: view.preferredColorScheme(.dark).background(Color(nsColor: .windowBackgroundColor)))
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.isReleasedWhenClosed = false
        window.contentView = host
        host.frame = NSRect(origin: .zero, size: size)
        window.orderFront(nil)
        defer { window.orderOut(nil); window.close() }
        try await Task.sleep(for: .milliseconds(350))
        host.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
        let bitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        let png = try #require(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: root.appendingPathComponent(name + ".png"))
        #expect(bitmap.pixelsWide >= Int(size.width))
        #expect(png.count > 5_000)
    }
    @MainActor
    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_UI_CAPTURE_ROOT"] != nil))
    func inspectorCatalogAndVideoTools() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["FILEID_UI_CAPTURE_ROOT"]), isDirectory: true)
        try #require(root.path.hasPrefix("/tmp/"))
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let engine = EngineClient()
        try await capture(ChatWorkbench(engine: engine, onClose: {}), name: "chat-inspector", size: NSSize(width: 380,height: 760),root: root)
        try await capture(ModelCatalogBrowser(engine: engine, initialSelection: .qwen35_2B, onSelect: { _ in }),name:"model-browser",size:NSSize(width:760,height:650),root:root)
        let settings = DeepAnalyzeSettings.shared
        let previousModel = settings.activeKind
        defer { settings.activeKind = previousModel }
        if let candidate = AIModelKind.visionModels.first(where: { !AIModelKind.favorites.contains($0) && $0.fits(ramGB:16) }) {
            settings.activeKind = candidate
            try await capture(DeepAnalyzeModelPickerCard(engine: engine), name:"selected-catalog-model",size:NSSize(width:760,height:650),root:root)
        }
        let session = ToolsSession()
        session.kind = "video"; session.format = "mp4"; session.maxDimension = 1920
        try await capture(ToolsWorkbench(engine: engine,session:session),name:"video-tools",size:NSSize(width:840,height:760),root:root)
    }
}
