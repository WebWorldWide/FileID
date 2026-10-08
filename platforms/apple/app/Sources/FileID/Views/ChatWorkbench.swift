import SwiftUI
import AppKit
import AVKit
import FileIDShared

struct ChatWorkbench: View {
    @Bindable var engine: EngineClient
    @Environment(\.dismiss) private var dismiss
    @AppStorage("chatConversationID") private var conversationID = ""
    @State private var input = ""
    @State private var useModel = true
    @State private var messages: [ChatMessage] = []
    @State private var hits: [CatalogHit] = []
    @State private var message = "Search your local catalog. File changes are available in File Tools."
    @State private var busy = false
    @State private var ignored = Set<String>()
    @State private var activeRequest = ""
    @State private var historyRequest = ""
    @State private var player: AVPlayer?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("FileID Chat").font(.title2.bold())
                Spacer()
                Button("Delete conversation") { control("clear") }
                Button("Done") { dismiss() }
            }
            HSplitView {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        ForEach(messages) { entry in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(entry.role == "user" ? "You" : "FileID").font(.caption.bold()).foregroundStyle(entry.role == "user" ? Theme.gold : .secondary)
                                Text(entry.text).textSelection(.enabled)
                            }.frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }.padding(10)
                }.frame(minWidth: 330)
                List(Array(hits.enumerated()), id: \.offset) { index, hit in
                    VStack(alignment: .leading, spacing: 5) {
                        Text("[\(index + 1)] \(URL(fileURLWithPath: hit.path).lastPathComponent)").font(.headline).lineLimit(2)
                        Text(hit.text.isEmpty ? hit.path : hit.text).font(.caption).lineLimit(4)
                        if hit.kind == "sampledFrame" { Text("Unverified sampled frame").font(.caption).foregroundStyle(.secondary) }
                        HStack {
                            Button("Open") {
                                let url = URL(fileURLWithPath: hit.path)
                                if FileManager.default.fileExists(atPath: url.path) {
                                    if let seconds = hit.startSeconds, ["mov", "mp4", "m4v", "mkv", "avi"].contains(url.pathExtension.lowercased()) {
                                        let next = AVPlayer(url: url)
                                        next.seek(to: CMTime(seconds: seconds, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
                                        player?.pause(); player = next
                                    } else { NSWorkspace.shared.open(url) }
                                }
                                else { message = "This catalog file is offline or unavailable." }
                            }
                            if let seconds = hit.startSeconds { Text("\(seconds, specifier: "%.1f") s").font(.caption) }
                            if let page = hit.page { Text("Page \(page)").font(.caption) }
                        }
                    }
                }.frame(minWidth: 330)
            }.frame(minHeight: 360)
            if let player { NativeVideoPlayer(player: player).frame(height: 180) }
            Text(message).font(.callout).textSelection(.enabled)
            HStack {
                TextField("Find birthday gift opening…", text: $input).textFieldStyle(.roundedBorder).onSubmit(send)
                Button("Send", action: send).disabled(busy || input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || input.count > 2000)
                if busy { Button("Stop") { control("cancel") }; ProgressView().controlSize(.small) }
            }
            Toggle("Summarize evidence with the loaded local model", isOn: $useModel).font(.caption)
            Text("Keyword retrieval works without a model. Answers are suggestions grounded in catalog evidence; sparse analysis can miss events.").font(.caption).foregroundStyle(.secondary)
        }.padding(20).frame(minWidth: 800, minHeight: 620).tint(Theme.gold)
            .onAppear {
                if conversationID.isEmpty { conversationID = UUID().uuidString }
                if let response = engine.latestChatResponse, response.conversationID == conversationID,
                   ["retrieving", "queued", "streaming"].contains(response.status) {
                    activeRequest = response.requestID; busy = true
                    messages = response.messages; hits = response.hits; message = response.message
                }
                control("history")
            }
            .onDisappear { player?.pause() }
            .onChange(of: engine.latestChatResponse) { _, response in
                guard let response, response.conversationID == conversationID, !ignored.contains(response.requestID) else { return }
                messages = response.messages; message = response.message
                if response.status != "completed" || !response.hits.isEmpty || activeRequest == response.requestID { hits = response.hits }
                busy = ["retrieving", "queued", "streaming"].contains(response.status)
                if busy, response.requestID != historyRequest { activeRequest = response.requestID }
            }
    }

    private func send() {
        guard !busy, !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, input.count <= 2000 else { return }
        activeRequest = UUID().uuidString
        let request = ChatRequest(requestID: activeRequest, conversationID: conversationID, action: "send", text: input, useModel: useModel)
        if engine.send(.chatRequest(request: request)) { input = ""; busy = true }
        else { message = "The engine is unavailable." }
    }

    private func control(_ action: String) {
        if action != "history", !activeRequest.isEmpty { ignored.insert(activeRequest); activeRequest = ""; busy = false }
        if action == "clear" { messages = []; hits = [] }
        let request = ChatRequest(requestID: UUID().uuidString, conversationID: conversationID, action: action)
        if action == "history" { historyRequest = request.requestID }
        if !engine.send(.chatRequest(request: request)) { message = "The engine is unavailable." }
    }
}
