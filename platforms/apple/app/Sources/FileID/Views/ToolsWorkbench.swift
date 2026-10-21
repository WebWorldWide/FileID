import SwiftUI
import AppKit
import FileIDShared

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

}

struct ToolsWorkbench: View {
    @Bindable var engine: EngineClient
    @Environment(\.dismiss) private var dismiss
    @Bindable var session: ToolsSession
    var inline = false
    @State private var previewImage: NSImage?

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
                optionRow("Format") {
                    if session.kind == "photo" {
                        SegmentedBar(selection: $session.format, options: [("png", "PNG"), ("jpeg", "JPEG"), ("tiff", "TIFF")])
                    } else if session.kind == "video" {
                        SegmentedBar(selection: $session.format, options: [("mp4", "MP4 · H.264")])
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
                if session.kind == "video" {
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
                if ["photo", "video"].contains(session.kind) {
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
                        } else if let previewImage {
                            Image(nsImage: previewImage).resizable().aspectRatio(contentMode: session.kind == "video" && session.videoFraming == "auto" ? .fill : .fit)
                        } else {
                            Image(systemName: session.kind == "video" ? "film" : "photo").font(.system(size: 28, weight: .light)).foregroundStyle(Theme.text3)
                        }
                    }
                    .aspectRatio(frameRatio, contentMode: .fit)
                    .frame(maxWidth: .infinity, maxHeight: 300)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.gold, lineWidth: 1.5))
                    .frame(maxWidth: .infinity)
                    .animation(Theme.springStandard, value: frameRatio)
                    .accessibilityLabel(selectedHits.first.map { "Preview of \(URL(fileURLWithPath: $0.path).lastPathComponent)" } ?? "No file selected")

                    VStack(alignment: .leading, spacing: 4) {
                        Text(summaryLine).font(Theme.Font.body(13, .medium)).foregroundStyle(Theme.text1)
                        Text(plainKindDetail)
                            .font(Theme.Font.body(12)).foregroundStyle(Theme.text3).fixedSize(horizontal: false, vertical: true)
                            .help(session.capabilities.first(where: { $0.id == session.kind })?.detail ?? "")
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
                                         recipe: ToolRecipe(kind: session.kind, format: session.format, maxDimension: session.maxDimension, allowUpscale: ["photo", "video"].contains(session.kind) ? session.allowUpscale : nil, videoAspectRatio: session.kind == "video" ? session.videoAspectRatio : nil, videoFraming: session.kind == "video" && session.videoAspectRatio != "source" ? session.videoFraming : nil, videoStabilization: session.kind == "video" ? session.videoStabilization : nil),
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
                HStack {
                    Button("Show last export") { send(ToolRequest(requestID: UUID().uuidString, action: "history")) }
                        .buttonStyle(.dsGhost)
                        .disabled(busy)
                    Spacer()
                    MetaText("Check reframed videos before sharing them.", size: 11.5)
                }
            }
            .padding(14)
        }
    }

    private var summaryLine: String {
        let count = session.selection.count
        guard count > 0 else { return "No files selected" }
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
        panel.canCreateDirectories = false
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
        session.searchID = ""
        if !engine.send(.toolRequest(request: request)) { session.pending = ""; session.pendingAction = ""; session.message = "FileID is still getting ready. Try again in a moment." }
    }
    private func consume() {
        guard !session.pending.isEmpty, let response = engine.toolResponses[session.pending] else { return }
        let action = session.pendingAction; session.pending = ""; session.pendingAction = ""
        session.message = response.message
        if action == "capabilities" { session.capabilities = response.capabilities; return }
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
                Text(output.message).font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3).lineLimit(2).textSelection(.enabled)
            }
        }
        .padding(.vertical, 8)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
        .help(output.outputPath)
        .accessibilityElement(children: .combine)
    }
}
