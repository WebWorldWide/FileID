import SwiftUI
import AppKit
import FileIDShared

struct ToolsWorkbench: View {
    @Bindable var engine: EngineClient
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var hits: [CatalogHit] = []
    @State private var selection = Set<Int64>()
    @State private var kind = "photo"
    @State private var format = "png"
    @State private var maxDimension = 4096
    @State private var destination = ""
    @State private var message = "Choose files and an internal output folder. Exports create new versions."
    @State private var pending = ""
    @State private var pendingAction = ""
    @State private var searchID = ""
    @State private var operationID: String?
    @State private var outputs: [ToolOutput] = []
    @State private var executed = false
    @State private var capabilities: [ToolCapability] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack { Text("File Tools").font(.title2.bold()); Spacer(); Button("Done") { dismiss() } }
            HStack {
                TextField("Find catalog files by name or description", text: $query).textFieldStyle(.roundedBorder).onSubmit { search() }
                Button("Find", action: search).disabled(query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            List(hits, id: \.fileID) { hit in
                Toggle(isOn: Binding(get: { selection.contains(hit.fileID) }, set: { value in
                    if value { selection.insert(hit.fileID) } else { selection.remove(hit.fileID) }; invalidate()
                })) { Text(hit.path).lineLimit(1).truncationMode(.middle) }
            }.frame(minHeight: 150)
            HStack {
                Picker("Tool", selection: $kind) { Text("Photo conversion").tag("photo"); Text("Chapter export").tag("chapters") }
                Picker("Output", selection: $format) {
                    if kind == "photo" { Text("PNG").tag("png"); Text("JPEG").tag("jpeg"); Text("TIFF").tag("tiff") }
                    else { Text("JSON markers").tag("json"); Text("WebVTT chapters").tag("vtt") }
                }
                if kind == "photo" { TextField("Maximum pixels", value: $maxDimension, format: .number).frame(width: 100) }
            }
            HStack {
                Text(destination.isEmpty ? "No output folder selected" : destination).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button("Choose output folder") {
                    let panel = NSOpenPanel(); panel.canChooseFiles = false; panel.canChooseDirectories = true; panel.canCreateDirectories = false
                    if panel.runModal() == .OK, let url = panel.url { destination = url.path; invalidate() }
                }
            }
            Text(capabilities.first(where: { $0.id == kind })?.detail ?? "Loading local capabilities…").font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Last export") { send(ToolRequest(requestID: UUID().uuidString, action: "history")) }.disabled(!pending.isEmpty)
                Button("Preview export") { send(ToolRequest(requestID: UUID().uuidString, action: "preview", fileIDs: selection.sorted(), destination: destination, recipe: ToolRecipe(kind: kind, format: format, maxDimension: maxDimension))) }
                    .disabled(selection.isEmpty || destination.isEmpty || !pending.isEmpty || !(1...8192).contains(maxDimension))
                Button("Export new versions") { send(ToolRequest(requestID: UUID().uuidString, action: "execute", operationID: operationID)) }
                    .disabled(operationID == nil || executed || !pending.isEmpty)
                Button("Undo export") { send(ToolRequest(requestID: UUID().uuidString, action: "undo", operationID: operationID)) }
                    .disabled(operationID == nil || !executed || !pending.isEmpty)
                if !pending.isEmpty { ProgressView().controlSize(.small) }
                if pendingAction == "execute" {
                    Button("Cancel") { _ = engine.send(.toolRequest(request: ToolRequest(requestID: UUID().uuidString, action: "cancel", operationID: operationID))); message = "Stopping export…" }
                }
            }
            Text(message).font(.callout).textSelection(.enabled)
            List(Array(outputs.enumerated()), id: \.offset) { _, output in
                VStack(alignment: .leading) {
                    Text(output.outputPath).lineLimit(1).truncationMode(.middle)
                    Text("\(output.state) · \(output.message)").font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                }
            }.frame(minHeight: 130)
            Text("Stabilization, AI upscaling, and tracked video reframing are still under development.").font(.caption).foregroundStyle(.secondary)
        }
        .padding(20).frame(minWidth: 850, minHeight: 650).tint(Theme.gold)
        .onAppear { send(ToolRequest(requestID: UUID().uuidString, action: "capabilities")) }
        .onChange(of: kind) { _, new in format = new == "photo" ? "png" : "json"; invalidate() }
        .onChange(of: format) { _, _ in invalidate() }
        .onChange(of: maxDimension) { _, _ in invalidate() }
        .onChange(of: engine.toolResponses) { _, _ in consume() }
        .onChange(of: engine.catalogResponses) { _, _ in
            guard !searchID.isEmpty, let result = engine.catalogResponses[searchID] else { return }
            searchID = ""
            var seen = Set<Int64>(); hits = result.hits.filter { seen.insert($0.fileID).inserted }; selection.removeAll(); invalidate()
            if result.status != "ok" { message = result.message ?? "Search failed." }
        }

    }

    private func invalidate() { if pending.isEmpty { operationID = nil; outputs = []; executed = false } }
    private func search() {
        searchID = UUID().uuidString
        if !engine.send(.catalogRequest(request: CatalogRequest(requestID: searchID, action: "search", query: query))) { searchID = ""; message = "The engine is unavailable." }
    }
    private func send(_ request: ToolRequest) {
        pending = request.requestID; pendingAction = request.action
        searchID = ""
        if !engine.send(.toolRequest(request: request)) { pending = ""; pendingAction = ""; message = "The engine is unavailable." }
    }
    private func consume() {
        guard !pending.isEmpty, let response = engine.toolResponses[pending] else { return }
        let action = pendingAction; pending = ""; pendingAction = ""
        message = response.message
        if action == "capabilities" { capabilities = response.capabilities; return }
        if action == "history", response.status == "ok" { operationID = response.operationID; executed = response.outputs.contains { $0.state == "completed" } }
        if action == "preview", response.status == "ok" { operationID = response.operationID; executed = false }
        if action == "execute" { executed = response.outputs.contains { $0.state == "completed" } }
        if action == "undo", response.status == "ok" { operationID = nil; executed = false }
        outputs = response.outputs
    }
}
