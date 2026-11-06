import SwiftUI
import AppKit
import AVFoundation
import FileIDShared

/// The Video sub-picker's choices and the recipe each one sends. Kept apart
/// from the view so the request-building rules are testable.
struct MediaToolChoice: Equatable {
    static let tools: [(id: String, label: String)] = [
        ("export", "Export"), ("trim", "Trim"), ("remux", "Remux"), ("audio", "Audio"), ("proxy", "Proxy"), ("rotate", "Rotate"),
    ]
    static let rotations: [(Int, String)] = [(90, "90° right"), (180, "180°"), (270, "90° left")]

    var tool = "export"
    var trimStart = 0.0
    var trimEnd = 0.0
    var trimMode = "keyframe"
    var container = "mp4"
    var proxySide = 640
    var rotation = 90

    var isExport: Bool { tool == "export" }

    /// Export is the existing video recipe; the others appear only when the engine reports them available.
    static func available(_ capabilities: [ToolCapability]) -> [(String, String)] {
        tools.filter { tool in tool.id == "export" || capabilities.contains { $0.id == tool.id && $0.available } }
            .map { ($0.id, $0.label) }
    }

    static func container(ofPath path: String) -> String? {
        switch URL(fileURLWithPath: path).pathExtension.lowercased() {
        case "mp4", "m4v": "mp4"
        case "mov": "mov"
        default: nil
        }
    }

    var formatOptions: [(String, String)] {
        switch tool {
        case "audio": [("m4a", "M4A · AAC")]
        case "proxy": [("mp4", "MP4 · H.264")]
        default: [("mp4", "MP4"), ("mov", "MOV")]
        }
    }

    /// Remuxing into the container a file already has only copies it, and an exact trim is MP4 only.
    func disabledContainers(sources: [String]) -> Set<String> {
        switch tool {
        case "remux": Set(sources.compactMap(Self.container(ofPath:)))
        case "trim" where trimMode == "exact": ["mov"]
        default: []
        }
    }

    var format: String {
        switch tool {
        case "audio": "m4a"
        case "proxy": "mp4"
        case "trim" where trimMode == "exact": "mp4"
        default: container
        }
    }

    mutating func adopt(tool next: String, sources: [String]) {
        tool = next
        if ["trim", "rotate"].contains(next), let first = sources.first.flatMap(Self.container(ofPath:)) { container = first }
        settleContainer(sources: sources)
    }

    mutating func settleContainer(sources: [String]) {
        let disabled = disabledContainers(sources: sources)
        guard disabled.contains(container), let allowed = ["mp4", "mov"].first(where: { !disabled.contains($0) }) else { return }
        container = allowed
    }

    /// Nil for Export, which keeps the video export recipe. Fields a recipe
    /// does not take stay nil; the engine refuses them otherwise.
    var recipe: ToolRecipe? {
        switch tool {
        case "trim": ToolRecipe(kind: "trim", format: format, maxDimension: 1920, trimStartSeconds: trimStart, trimEndSeconds: trimEnd, trimMode: trimMode)
        case "remux", "audio": ToolRecipe(kind: tool, format: format, maxDimension: 1920)
        case "proxy": ToolRecipe(kind: "proxy", format: "mp4", maxDimension: proxySide)
        case "rotate": ToolRecipe(kind: "rotate", format: format, maxDimension: 1920, rotationDegrees: rotation)
        default: nil
        }
    }

    var label: String { Self.tools.first { $0.id == tool }?.label ?? tool }

    /// Displayed turn for the preview; 90° left turns back rather than three quarters forward.
    var previewAngle: Double {
        guard tool == "rotate" else { return 0 }
        return rotation == 270 ? -90 : Double(rotation)
    }
}

@MainActor
@Observable
final class ToolsSession {
    var query = ""
    var hits: [CatalogHit] = []
    var selection = Set<Int64>()
    var kind = "photo"
    var format = "png"
    var maxDimension = 4096
    var allowUpscale = false
    var videoAspectRatio = "source"
    var videoFraming = "fit"
    var videoStabilization = false
    var destination = ""
    var destinationBookmark: Data?
    var destinationAccessReady: Bool {
        #if FILEID_APP_STORE
        destinationBookmark != nil
        #else
        true
        #endif
    }
    var message = "Choose files and a folder to save to. Exports are new files; your originals stay as they are."
    var pending = ""
    var pendingAction = ""
    var searchID = ""
    var operationID: String?
    var outputs: [ToolOutput] = []
    var executed = false
    var capabilities: [ToolCapability] = []
    var pendingEngineGeneration = 0
    var media = MediaToolChoice()

    var selectedPaths: [String] { hits.filter { selection.contains($0.fileID) }.map(\.path) }

    var recipe: ToolRecipe {
        if kind == "video", let recipe = media.recipe { return recipe }
        return ToolRecipe(kind: kind, format: format, maxDimension: maxDimension,
                          allowUpscale: ["photo", "video"].contains(kind) ? allowUpscale : nil,
                          videoAspectRatio: kind == "video" ? videoAspectRatio : nil,
                          videoFraming: kind == "video" && videoAspectRatio != "source" ? videoFraming : nil,
                          videoStabilization: kind == "video" ? videoStabilization : nil)
    }

    var exportsVideo: Bool { kind == "video" && media.isExport }

    /// A request sent to an engine that has since exited never gets a reply;
    /// without this every control stays disabled for the rest of the session.
    func abandonPendingIfEngineRestarted(generation: Int) {
        guard !pending.isEmpty, generation != pendingEngineGeneration else { return }
        let action = pendingAction
        pending = ""
        pendingAction = ""
        message = action == "execute"
            ? "FileID's engine stopped during the export. Some files may already be saved; press Show last export to check."
            : "FileID's engine stopped before this finished. Try again."
    }
}

struct ToolsWorkbench: View {
    @Bindable var engine: EngineClient
    @Environment(\.dismiss) private var dismiss
    @Bindable var session: ToolsSession
    var inline = false
    @State private var previewImage: NSImage?
    @State private var player: AVPlayer?
    @State private var clock = PlayheadClock()

    private var busy: Bool { !session.pending.isEmpty }
    private var selectedHits: [CatalogHit] { session.hits.filter { session.selection.contains($0.fileID) } }
    private var canPreview: Bool {
        !session.selection.isEmpty && !session.destination.isEmpty && session.destinationAccessReady
            && !busy && (1...8192).contains(session.maxDimension)
    }

    var body: some View {
        VStack(spacing: 0) {
            if !inline {
                HStack {
                    Text("File Tools").font(Theme.Font.display(24)).foregroundStyle(Theme.text1)
                    Spacer()
                    Button("Done") { dismiss() }.buttonStyle(.dsSecondary).keyboardShortcut(.cancelAction)
                }
                .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, 14)
                .overlay(alignment: .bottom) { Hairline() }
            }
            HStack(spacing: 0) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        recipePanel
                        outputPanel
                    }
                    .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .focusSection()
                VHairline()
                previewPane
                    .frame(width: 380)
                    .background(Theme.pane, ignoresSafeAreaEdges: [])
                    .focusSection()
            }
        }
        .frame(minWidth: inline ? nil : 900, minHeight: inline ? nil : 650)
        .tint(Theme.gold)
        .onAppear {
            consume(); consumeSearch()
            session.abandonPendingIfEngineRestarted(generation: engine.engineResetSignal)
            if session.pending.isEmpty && session.capabilities.isEmpty { send(ToolRequest(requestID: UUID().uuidString, action: "capabilities")) }
        }
        .task(id: selectedHits.first?.path) {
            previewImage = nil
            guard let path = selectedHits.first?.path else { return }
            previewImage = await ThumbnailService.shared.thumbnail(for: URL(fileURLWithPath: path), size: 640)
        }
        .onChange(of: session.kind) { _, new in session.format = new == "photo" ? "png" : (new == "video" ? "mp4" : "json"); session.maxDimension = new == "video" ? 1920 : 4096; invalidate() }
        .onChange(of: session.format) { _, _ in invalidate() }
        .onChange(of: session.maxDimension) { _, _ in invalidate() }
        .onChange(of: session.allowUpscale) { _, _ in invalidate() }
        .onChange(of: session.videoAspectRatio) { _, _ in invalidate() }
        .onChange(of: session.videoFraming) { _, _ in invalidate() }
        .onChange(of: session.videoStabilization) { _, _ in invalidate() }
        .onChange(of: session.media) { _, _ in session.media.settleContainer(sources: session.selectedPaths); invalidate() }
        .onChange(of: session.selection) { _, _ in session.media.settleContainer(sources: session.selectedPaths) }
        .task(id: trimSource) { await loadTrimPlayer() }
        .onDisappear { player?.pause() }
        .onChange(of: engine.engineResetSignal) { _, new in session.abandonPendingIfEngineRestarted(generation: new) }
        .onChange(of: engine.toolResponses) { _, _ in consume() }
        .onChange(of: engine.catalogResponses) { _, _ in consumeSearch() }
    }

    // MARK: - 01 Recipe

    private var recipePanel: some View {
        Panel {
            PanelHeader("01 · Pick files") {
                SegmentedBar(selection: $session.kind, options: [("photo", "Photo"), ("video", "Video"), ("chapters", "Chapters")])
                    .disabled(busy)
            }
            HStack(spacing: 8) {
                DSTextField(placeholder: "Find files by name or description", text: $session.query, icon: "magnifyingglass", onSubmit: search)
                Button("Find", action: search)
                    .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.controlLarge))
                    .disabled(busy || session.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .help(session.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Type part of a file name or what's in it first" : "Search the library")
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            .disabled(busy)
            if session.hits.isEmpty {
                MetaText("Search for files by name or by what's in them, then tick the ones to export. Exports are new files; your originals stay as they are.")
                    .padding(.horizontal, 14).padding(.bottom, 12)
            } else {
                Hairline()
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(session.hits, id: \.fileID) { hit in
                        let url = URL(fileURLWithPath: hit.path)
                        Toggle(isOn: Binding(get: { session.selection.contains(hit.fileID) }, set: { value in
                            if value { session.selection.insert(hit.fileID) } else { session.selection.remove(hit.fileID) }
                            invalidate()
                        })) {
                            HStack(spacing: 10) {
                                Text(url.lastPathComponent).font(Theme.Font.mono(12)).foregroundStyle(Theme.text1)
                                    .lineLimit(1).truncationMode(.middle)
                                Spacer(minLength: 8)
                                Text(url.deletingLastPathComponent().lastPathComponent).font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                                    .lineLimit(1).truncationMode(.head)
                            }
                        }
                        .toggleStyle(DSCheckboxToggleStyle())
                        .padding(.horizontal, 14)
                        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
                        .help(hit.path)
                    }
                }
                .disabled(busy)
                PanelFooter {
                    MetaText("\(session.selection.count) of \(session.hits.count) chosen", size: 12)
                    Spacer()
                    Button(session.selection.count == session.hits.count ? "Select none" : "Select all") {
                        session.selection = session.selection.count == session.hits.count ? [] : Set(session.hits.map(\.fileID))
                        invalidate()
                    }
                    .buttonStyle(.dsGhost)
                    .disabled(busy)
                }
            }
        }
    }

    // MARK: - 02 Output

    private var outputPanel: some View {
        Panel {
            PanelHeader("02 · Output")
            VStack(alignment: .leading, spacing: 12) {
                if session.kind == "video" {
                    optionRow("Tool") {
                        SegmentedBar(selection: Binding(get: { session.media.tool }, set: { session.media.adopt(tool: $0, sources: session.selectedPaths) }),
                                     options: MediaToolChoice.available(session.capabilities))
                    }
                }
                optionRow("Format") {
                    if session.kind == "photo" {
                        SegmentedBar(selection: $session.format, options: [("png", "PNG"), ("jpeg", "JPEG"), ("tiff", "TIFF")])
                    } else if session.exportsVideo {
                        SegmentedBar(selection: $session.format, options: [("mp4", "MP4 · H.264")])
                    } else if session.kind == "video" {
                        SegmentedBar(selection: Binding(get: { session.media.format }, set: { if ["mp4", "mov"].contains($0) { session.media.container = $0 } }),
                                     options: session.media.formatOptions,
                                     disabled: session.media.disabledContainers(sources: session.selectedPaths))
                    } else {
                        SegmentedBar(selection: $session.format, options: [("vtt", "WebVTT"), ("json", "JSON")])
                    }
                }
                if session.kind == "photo" {
                    optionRow("Longest side") {
                        DSNumberField(placeholder: "Longest side in pixels", value: $session.maxDimension, format: .number.grouping(.never), width: 90)
                        MetaText("px, up to 8192", size: 12)
                    }
                }
                if session.kind == "video" && !session.media.isExport {
                    mediaRows
                }
                if session.exportsVideo {
                    optionRow("Resolution") {
                        SegmentedBar(selection: $session.maxDimension, options: [(1280, "720p"), (1920, "1080p"), (3840, "4K")])
                    }
                    optionRow("Frame", alignment: .top) {
                        AspectTiles(selection: $session.videoAspectRatio)
                    }
                    if session.videoAspectRatio != "source" {
                        optionRow("Framing") {
                            SegmentedBar(selection: $session.videoFraming, options: [("fit", "Fit full picture"), ("auto", "Follow people")])
                        }
                    }
                    note(session.videoFraming == "auto" && session.videoAspectRatio != "source"
                         ? "Follows detected people with smooth crops and fits the full picture when people are lost or a group can't fit. Clips up to ten minutes. Review the exported framing."
                         : "Fits the full picture inside the frame and pads the empty space.")
                    optionRow("") {
                        Toggle("Stabilize shaky video", isOn: $session.videoStabilization).toggleStyle(DSToggleStyle())
                            .font(Theme.Font.body(13)).foregroundStyle(Theme.text1)
                    }
                    if session.videoStabilization {
                        note("Corrects estimated shake with up to 12% zoom to cover the edges. Clips up to ten minutes and 60 fps. Review the result.")
                    }
                }
                if session.kind == "photo" || session.exportsVideo {
                    optionRow("") {
                        Toggle("Enlarge smaller media to this size", isOn: $session.allowUpscale).toggleStyle(DSToggleStyle())
                            .font(Theme.Font.body(13)).foregroundStyle(Theme.text1)
                    }
                    note("Enlarging adds pixels, not detail.")
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            .disabled(busy)
            PanelFooter {
                SectionLabel("Save to")
                Text(session.destination.isEmpty ? "No folder chosen" : session.destination)
                    .font(session.destination.isEmpty ? Theme.Font.body(12.5) : Theme.Font.mono(12)).foregroundStyle(session.destination.isEmpty ? Theme.text3 : Theme.text1)
                    .lineLimit(1).truncationMode(.head)
                    .help(session.destination)
                Spacer(minLength: 8)
                Button("Choose…", action: chooseDestination)
                    .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                    .disabled(busy)
                    .accessibilityLabel("Choose output folder")
            }
        }
    }

    @ViewBuilder private var mediaRows: some View {
        switch session.media.tool {
        case "trim":
            trimRow("Start", value: $session.media.trimStart)
            trimRow("End", value: $session.media.trimEnd)
            optionRow("Cut") {
                SegmentedBar(selection: $session.media.trimMode, options: [("keyframe", "Keyframe"), ("exact", "Exact")])
            }
            note(session.media.trimMode == "exact"
                 ? "Re-encodes to an H.264 MP4 so the cut lands on the exact frame. HDR videos are refused."
                 : "Copies the video without re-encoding. The new file may start a little early, at the keyframe before your start time.")
        case "remux":
            note("Moves the video and sound into the other container without re-encoding. The container a file already has is greyed out.")
        case "audio":
            note("Saves the soundtrack as AAC in an M4A file. Videos without sound are refused.")
        case "proxy":
            optionRow("Size") {
                SegmentedBar(selection: $session.media.proxySide, options: [(640, "640 px"), (960, "960 px")])
            }
            note("A small H.264 copy for quick previews, linked to its original. Smaller videos aren't enlarged.")
        default:
            optionRow("Turn") {
                SegmentedBar(selection: $session.media.rotation, options: MediaToolChoice.rotations)
            }
            note("Changes which way up the picture is shown, without re-encoding it.")
        }
    }

    private func trimRow(_ label: String, value: Binding<Double>) -> some View {
        optionRow(label) {
            DSNumberField(placeholder: "\(label) in seconds", value: value, format: .number.precision(.fractionLength(0...2)), width: 90)
                .accessibilityLabel("\(label) time in seconds")
            MetaText("s", size: 12)
            Button("Set from playhead") { value.wrappedValue = (clock.seconds * 100).rounded() / 100 }
                .buttonStyle(.dsGhost)
                .disabled(player == nil)
                .help(player == nil ? "Tick a video that can play here first" : "Use the player's current time")
        }
    }

    private var trimSource: String? {
        session.kind == "video" && session.media.tool == "trim" ? selectedHits.first?.path : nil
    }

    private func loadTrimPlayer() async {
        player?.pause()
        player = nil
        clock.attach(nil)
        guard let path = trimSource, let asset = await MediaPlayback.playableAsset(URL(fileURLWithPath: path)), !Task.isCancelled else { return }
        let seconds = (try? await asset.load(.duration))?.seconds ?? 0
        guard !Task.isCancelled, trimSource == path else { return }
        let next = AVPlayer(playerItem: AVPlayerItem(asset: asset))
        player = next
        clock.attach(next)
        if seconds.isFinite, seconds > 0, session.media.trimEnd <= session.media.trimStart {
            session.media.trimEnd = (seconds * 100).rounded(.down) / 100
        }
    }

    private func optionRow<Content: View>(_ label: String, alignment: VerticalAlignment = .center, @ViewBuilder content: () -> Content) -> some View {
        HStack(alignment: alignment, spacing: 12) {
            Text(label).font(Theme.Font.body(12.5, .medium)).foregroundStyle(Theme.text2)
                .frame(width: 92, alignment: .leading)
                .padding(.top, alignment == .top ? 6 : 0)
                .accessibilityHidden(label.isEmpty)
            content()
            Spacer(minLength: 0)
        }
    }

    private func note(_ text: String) -> some View {
        HStack(spacing: 12) {
            Color.clear.frame(width: 92, height: 1)
            Text(text).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - 03 Preview

    private var frameRatio: CGFloat {
        guard session.kind == "video" else {
            if let size = previewImage?.size, size.height > 0 { return size.width / size.height }
            return 4 / 3
        }
        if !session.media.isExport {
            let size = previewImage?.size ?? CGSize(width: 16, height: 9)
            let ratio = size.height > 0 ? size.width / size.height : 16 / 9
            return session.media.previewAngle.truncatingRemainder(dividingBy: 180) == 0 ? ratio : 1 / ratio
        }
        switch session.videoAspectRatio {
        case "9:16": return 9 / 16
        case "1:1": return 1
        case "4:5": return 4 / 5
        default: return 16 / 9
        }
    }

    private var previewPane: some View {
        VStack(spacing: 0) {
            HStack {
                SectionLabel("03 · Preview")
                Spacer()
                if busy { IndeterminateBar().frame(width: 60) }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    ZStack {
                        Color.black.opacity(0.35)
                        if session.kind == "chapters" {
                            VStack(spacing: 8) {
                                Image(systemName: "list.bullet.rectangle").font(.system(size: 28, weight: .light)).foregroundStyle(Theme.text3)
                                MetaText(session.format == "vtt" ? "WebVTT chapter track" : "Chapter list as JSON")
                            }
                        } else if trimSource != nil, let player {
                            NativeVideoPlayer(player: player).accessibilityLabel("Video player")
                        } else if let previewImage {
                            turnedPreview(previewImage)
                        } else {
                            Image(systemName: session.kind == "video" ? "film" : "photo").font(.system(size: 28, weight: .light)).foregroundStyle(Theme.text3)
                        }
                    }
                    .aspectRatio(frameRatio, contentMode: .fit)
                    .frame(maxWidth: .infinity, maxHeight: 300)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                    // Gold marks a real preview; an empty frame keeps the neutral hairline.
                    .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous)
                        .stroke(previewImage == nil ? Theme.hairlineStrong : Theme.gold, lineWidth: previewImage == nil ? 1 : 1.5))
                    .frame(maxWidth: .infinity)
                    .animation(Theme.springStandard, value: frameRatio)
                    .accessibilityLabel(selectedHits.first.map { "Preview of \(URL(fileURLWithPath: $0.path).lastPathComponent)" } ?? "No file selected")

                    VStack(alignment: .leading, spacing: 4) {
                        Text(summaryLine).font(Theme.Font.body(13, .medium)).foregroundStyle(Theme.text1)
                        Text(plainKindDetail)
                            .font(Theme.Font.body(12)).foregroundStyle(Theme.text3).fixedSize(horizontal: false, vertical: true)
                            .help(session.capabilities.first(where: { $0.id == (session.kind == "video" && !session.media.isExport ? session.media.tool : session.kind) })?.detail ?? "")
                    }

                    if !session.outputs.isEmpty {
                        VStack(alignment: .leading, spacing: 0) {
                            SectionLabel("Outputs · \(session.outputs.count)").padding(.bottom, 6)
                            ForEach(Array(session.outputs.enumerated()), id: \.offset) { _, output in
                                ToolOutputRow(output: output)
                            }
                        }
                    }

                    if !session.message.isEmpty {
                        Text(session.message).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2)
                            .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(14)
            }
            Hairline()
            VStack(spacing: 8) {
                HStack(spacing: 8) {
                    Button("Preview") {
                        send(ToolRequest(requestID: UUID().uuidString, action: "preview",
                                         fileIDs: session.selection.sorted(), destination: session.destination,
                                         recipe: session.recipe,
                                         destinationBookmark: session.destinationBookmark))
                    }
                    .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.controlLarge))
                    .disabled(!canPreview)
                    .help(canPreview ? "Show the files this export will create, before writing anything" : session.selection.isEmpty ? "Tick at least one file first" : "Choose a folder to save to first")
                    Button("Export") {
                        send(ToolRequest(requestID: UUID().uuidString, action: "execute", destination: session.destination,
                                         operationID: session.operationID, destinationBookmark: session.destinationBookmark))
                    }
                    .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.controlLarge))
                    .disabled(session.operationID == nil || session.executed || !session.destinationAccessReady || busy)
                    .keyboardShortcut(.defaultAction)
                    .help(session.operationID == nil ? "Press Preview first to check what will be created" : session.executed ? "Already exported. Change something and preview again to export more." : "Create the new files in the chosen folder")
                    Spacer(minLength: 0)
                    if session.pendingAction == "execute" {
                        Button("Cancel") {
                            _ = engine.send(.toolRequest(request: ToolRequest(requestID: UUID().uuidString, action: "cancel", operationID: session.operationID)))
                            session.message = "Stopping export…"
                        }
                        .buttonStyle(.dsGhost)
                    } else {
                        Button("Undo") {
                            send(ToolRequest(requestID: UUID().uuidString, action: "undo", destination: session.destination,
                                             operationID: session.operationID, destinationBookmark: session.destinationBookmark))
                        }
                        .buttonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.controlLarge))
                        .disabled(session.operationID == nil || !session.outputs.contains(where: { $0.state == "completed" }) || !session.destinationAccessReady || busy)
                        .help(session.outputs.contains(where: { $0.state == "completed" }) ? "Move the files this export created to Trash" : "Nothing exported yet")
                    }
                }
                VStack(alignment: .leading, spacing: 6) {
                    Button("Show last export") { send(ToolRequest(requestID: UUID().uuidString, action: "history")) }
                        .buttonStyle(.dsGhost)
                        .disabled(busy)
                        .padding(.leading, -6)
                    if session.exportsVideo {
                        MetaText("Check reframed videos before sharing them.", size: 11.5)
                    }
                }
            }
            .padding(14)
        }
    }

    private func turnedPreview(_ image: NSImage) -> some View {
        let angle = session.media.previewAngle
        let quarter = angle.truncatingRemainder(dividingBy: 180) != 0
        return GeometryReader { geo in
            Image(nsImage: image).resizable()
                .aspectRatio(contentMode: session.exportsVideo && session.videoFraming == "auto" ? .fill : .fit)
                .frame(width: quarter ? geo.size.height : geo.size.width, height: quarter ? geo.size.width : geo.size.height)
                .rotationEffect(.degrees(angle))
                .frame(width: geo.size.width, height: geo.size.height)
        }
        .animation(Theme.reduceMotion ? nil : .spring(response: 0.35, dampingFraction: 0.8), value: angle)
    }

    private var summaryLine: String {
        let count = session.selection.count
        guard count > 0 else { return "No files selected" }
        if session.kind == "video" && !session.media.isExport {
            var parts = ["\(count) file\(count == 1 ? "" : "s")", session.media.label, session.media.format.uppercased()]
            switch session.media.tool {
            case "trim":
                parts.append(String(format: "%.2f–%.2f s", session.media.trimStart, session.media.trimEnd))
                parts.append(session.media.trimMode == "exact" ? "Exact" : "Keyframe")
            case "proxy": parts.append("\(session.media.proxySide) px")
            case "rotate": parts.append(MediaToolChoice.rotations.first { $0.0 == session.media.rotation }?.1 ?? "")
            default: break
            }
            return parts.joined(separator: " · ")
        }
        var parts = ["\(count) file\(count == 1 ? "" : "s")", session.format.uppercased()]
        if session.kind == "photo" { parts.append("≤ \(session.maxDimension) px") }
        if session.kind == "video" {
            parts.append(session.maxDimension >= 3840 ? "4K" : (session.maxDimension >= 1920 ? "1080p" : "720p"))
            if session.videoAspectRatio != "source" { parts.append(session.videoAspectRatio) }
        }
        return parts.joined(separator: " · ")
    }

    private func consumeSearch() {
        guard !session.searchID.isEmpty, let result = engine.catalogResponses[session.searchID] else { return }
        session.searchID = ""
        var seen = Set<Int64>()
        session.hits = result.hits.filter { seen.insert($0.fileID).inserted }
        session.selection.removeAll(); invalidate()
        if result.status != "ok" { session.message = result.message ?? "Search failed." }
    }

    private func invalidate() { if session.pending.isEmpty { session.operationID = nil; session.outputs = []; session.executed = false } }
    private func chooseDestination() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Choose where to save the new copies. Your originals stay where they are."
        panel.prompt = "Save Here"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        session.destination = url.path
        session.destinationBookmark = nil
        invalidate()
        let selectedURL = url
        Task.detached(priority: .utility) {
            let authorizedBookmark = try? SecurityScopedBookmark.makeIPCBookmark(for: selectedURL)
            await MainActor.run {
                guard session.destination == selectedURL.path else { return }
                session.destinationBookmark = authorizedBookmark
                #if FILEID_APP_STORE
                if authorizedBookmark == nil {
                    session.message = "FileID couldn't get permission to save in that folder. Choose it again."
                }
                #endif
                invalidate()
            }
        }
    }

    private var plainKindDetail: String {
        if session.kind == "video" && !session.media.isExport {
            switch session.media.tool {
            case "trim": return "Keeps the part between the start and end times as a new file. Your original video stays as it is."
            case "remux": return "Puts the same video and sound in a different container. Your original video stays as it is."
            case "audio": return "Saves the soundtrack as a new audio file. Your original video stays as it is."
            case "proxy": return "Makes a small preview copy linked to the original. Your original video stays as it is."
            default: return "Saves a copy that plays the other way up. Your original video stays as it is."
            }
        }
        switch session.kind {
        case "video": return "Makes a new MP4 at the size and shape you choose. Your original video stays as it is."
        case "chapters": return "Saves a video's chapters as a file other apps can read."
        default: return "Makes a new copy at the format and size you choose. Location and camera details are removed, and your original stays as it is."
        }
    }

    private func search() {
        guard session.pending.isEmpty else { return }
        session.searchID = UUID().uuidString
        if !engine.send(.catalogRequest(request: CatalogRequest(requestID: session.searchID, action: "search", query: session.query))) { session.searchID = ""; session.message = "FileID is still getting ready. Try again in a moment." }
    }
    private func send(_ request: ToolRequest) {
        session.pending = request.requestID; session.pendingAction = request.action
        session.pendingEngineGeneration = engine.engineResetSignal
        session.searchID = ""
        if !engine.send(.toolRequest(request: request)) { session.pending = ""; session.pendingAction = ""; session.message = "FileID is still getting ready. Try again in a moment." }
    }
    private func consume() {
        guard !session.pending.isEmpty, let response = engine.toolResponses[session.pending] else { return }
        let action = session.pendingAction; session.pending = ""; session.pendingAction = ""
        session.message = response.message
        if action == "capabilities" {
            session.capabilities = response.capabilities
            if !MediaToolChoice.available(response.capabilities).contains(where: { $0.0 == session.media.tool }) { session.media.tool = "export" }
            return
        }
        if action == "history", response.status == "ok" { session.operationID = response.operationID; session.executed = response.operationID != nil }
        if action == "preview", response.status == "ok" { session.operationID = response.operationID; session.executed = false }
        if action == "execute" { session.executed = true }
        if action == "undo", response.status == "ok" { session.operationID = nil; session.executed = false }
        withAnimation(Theme.springStandard) { session.outputs = response.outputs }
    }
}

private struct ToolOutputRow: View {
    let output: ToolOutput

    private var tone: (Color, ProgressTone, Double) {
        switch output.state {
        case "completed": return (Theme.delight, .done, 1)
        case "failed": return (Theme.danger, .neutral, 1)
        case "undone": return (Theme.text3, .paused, 1)
        default: return (Theme.text3, .paused, 0)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                Text(URL(fileURLWithPath: output.outputPath).lastPathComponent).font(Theme.Font.mono(12)).foregroundStyle(Theme.text1)
                    .lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 6)
                Text(output.state == "pending" ? "Planned" : output.state.capitalized).font(Theme.Font.body(11.5, .semibold))
                    .foregroundStyle(output.state == "running" ? Theme.gold : tone.0)
            }
            if output.state == "running" {
                IndeterminateBar()
            } else {
                GrainProgressBar(progress: tone.2, tone: tone.1, height: 4)
            }
            if !output.message.isEmpty {
                Text(output.message).font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
                    .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
            }
        }
        .padding(.vertical, 8)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
        .help(output.outputPath)
        .accessibilityElement(children: .combine)
    }
}
