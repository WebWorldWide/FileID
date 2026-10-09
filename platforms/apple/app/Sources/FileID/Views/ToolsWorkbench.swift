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
    var destination = ""
    var destinationBookmark: Data?
    var destinationAccessReady: Bool {
        #if FILEID_APP_STORE
        destinationBookmark != nil
        #else
        true
        #endif
    }
    var message = "Choose files and an internal output folder. Exports create new versions."
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

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack { Text("File Tools").font(.title2.bold()); Spacer(); Button("Done") { dismiss() } }
            HStack {
                TextField("Find catalog files by name or description", text: $session.query).textFieldStyle(.roundedBorder).onSubmit { search() }
                Button("Find", action: search).disabled(!session.pending.isEmpty || session.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }.disabled(!session.pending.isEmpty)
            List(session.hits, id: \.fileID) { hit in
                Toggle(isOn: Binding(get: { session.selection.contains(hit.fileID) }, set: { value in
                    if value { session.selection.insert(hit.fileID) } else { session.selection.remove(hit.fileID) }; invalidate()
                })) { Text(hit.path).lineLimit(1).truncationMode(.middle) }
            }.frame(minHeight: 150).disabled(!session.pending.isEmpty)
            HStack {
                Picker("Tool", selection: $session.kind) { Text("Photo conversion").tag("photo"); Text("Chapter export").tag("chapters"); Text("Video conversion").tag("video") }
                Picker("Output", selection: $session.format) {
                    if session.kind == "photo" { Text("PNG").tag("png"); Text("JPEG").tag("jpeg"); Text("TIFF").tag("tiff") }
                    else if session.kind == "video" { Text("MP4 · H.264").tag("mp4") }
                    else { Text("JSON markers").tag("json"); Text("WebVTT chapters").tag("vtt") }
                }
                if session.kind == "photo" { TextField("Maximum pixels", value: $session.maxDimension, format: .number).frame(width: 100) }
                if session.kind == "video" { Picker("Resolution", selection: $session.maxDimension) { Text("720p · 1280 pixels").tag(1280); Text("1080p · 1920 pixels").tag(1920) } }
            }.disabled(!session.pending.isEmpty)
            if session.kind == "photo" {
                Toggle("Enlarge smaller photos to the maximum size", isOn: $session.allowUpscale)
                    .disabled(!session.pending.isEmpty)
                Text("Conventional resizing increases pixel dimensions; it does not recover missing detail.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if session.kind == "video" {
                Picker("Frame", selection: $session.videoAspectRatio) {
                    Text("Original").tag("source")
                    Text("Vertical · 9:16").tag("9:16")
                    Text("Horizontal · 16:9").tag("16:9")
                    Text("Square · 1:1").tag("1:1")
                    Text("Portrait · 4:5").tag("4:5")
                }.disabled(!session.pending.isEmpty)
                Text("Fits the full picture inside the selected frame; empty space is padded. Subject-tracked cropping is still in development.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Text(session.destination.isEmpty ? "No output folder selected" : session.destination).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button("Choose output folder", action: chooseDestination)
            }.disabled(!session.pending.isEmpty)
            Text(session.capabilities.first(where: { $0.id == session.kind })?.detail ?? "Loading local capabilities…").font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Last export") { send(ToolRequest(requestID: UUID().uuidString, action: "history")) }.disabled(!session.pending.isEmpty)
                Button("Preview export") {
                    send(ToolRequest(requestID: UUID().uuidString, action: "preview",
                                     fileIDs: session.selection.sorted(), destination: session.destination,
                                     recipe: ToolRecipe(kind: session.kind, format: session.format, maxDimension: session.maxDimension, allowUpscale: session.kind == "photo" ? session.allowUpscale : nil, videoAspectRatio: session.kind == "video" ? session.videoAspectRatio : nil),
                                     destinationBookmark: session.destinationBookmark))
                }
                    .disabled(session.selection.isEmpty || session.destination.isEmpty || !session.destinationAccessReady
                              || !session.pending.isEmpty || !(1...8192).contains(session.maxDimension))
                Button("Export new versions") {
                    send(ToolRequest(requestID: UUID().uuidString, action: "execute", destination: session.destination,
                                     operationID: session.operationID, destinationBookmark: session.destinationBookmark))
                }
                    .disabled(session.operationID == nil || session.executed || !session.destinationAccessReady || !session.pending.isEmpty)
                Button("Undo export") {
                    send(ToolRequest(requestID: UUID().uuidString, action: "undo", destination: session.destination,
                                     operationID: session.operationID, destinationBookmark: session.destinationBookmark))
                }
                    .disabled(session.operationID == nil || !session.outputs.contains(where: { $0.state == "completed" }) || !session.destinationAccessReady || !session.pending.isEmpty)
                if !session.pending.isEmpty { ProgressView().controlSize(.small) }
                if session.pendingAction == "execute" {
                    Button("Cancel") { _ = engine.send(.toolRequest(request: ToolRequest(requestID: UUID().uuidString, action: "cancel", operationID: session.operationID))); session.message = "Stopping export…" }
                }
            }
            Text(session.message).font(.callout).textSelection(.enabled)
            List(Array(session.outputs.enumerated()), id: \.offset) { _, output in
                VStack(alignment: .leading) {
                    Text(output.outputPath).lineLimit(1).truncationMode(.middle)
                    Text("\(output.state) · \(output.message)").font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                }
            }.frame(minHeight: 130)
            Text("Stabilization, AI upscaling, and tracked video reframing are still under development.").font(.caption).foregroundStyle(.secondary)
        }
        .padding(20).frame(minWidth: 850, minHeight: 650).tint(Theme.gold)
        .onAppear {
            consume(); consumeSearch()
            if session.pending.isEmpty && session.capabilities.isEmpty { send(ToolRequest(requestID: UUID().uuidString, action: "capabilities")) }
        }
        .onChange(of: session.kind) { _, new in session.format = new == "photo" ? "png" : (new == "video" ? "mp4" : "json"); session.maxDimension = new == "video" ? 1920 : 4096; invalidate() }
        .onChange(of: session.format) { _, _ in invalidate() }
        .onChange(of: session.maxDimension) { _, _ in invalidate() }
        .onChange(of: session.allowUpscale) { _, _ in invalidate() }
        .onChange(of: session.videoAspectRatio) { _, _ in invalidate() }
        .onChange(of: engine.toolResponses) { _, _ in consume() }
        .onChange(of: engine.catalogResponses) { _, _ in consumeSearch() }

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
                    session.message = "FileID couldn't grant the output folder to its local engine. Choose it again."
                }
                #endif
                invalidate()
            }
        }
    }

    private func search() {
        guard session.pending.isEmpty else { return }
        session.searchID = UUID().uuidString
        if !engine.send(.catalogRequest(request: CatalogRequest(requestID: session.searchID, action: "search", query: session.query))) { session.searchID = ""; session.message = "The engine is unavailable." }
    }
    private func send(_ request: ToolRequest) {
        session.pending = request.requestID; session.pendingAction = request.action
        session.searchID = ""
        if !engine.send(.toolRequest(request: request)) { session.pending = ""; session.pendingAction = ""; session.message = "The engine is unavailable." }
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
        session.outputs = response.outputs
    }
}
