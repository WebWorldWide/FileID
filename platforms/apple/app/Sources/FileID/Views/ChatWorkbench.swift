import SwiftUI
import AppKit
import AVKit
import FileIDShared

struct ChatWorkbench: View {
    @Bindable var engine: EngineClient
    var onClose: (() -> Void)? = nil
    @FocusState private var inputFocused: Bool
    @Environment(\.dismiss) private var dismiss
    @AppStorage("chatConversationID") private var conversationID = ""
    @AppStorage("chatDraft") private var input = ""
    @State private var useModel = true
    @AppStorage("chatModelKind") private var chatModelKind = DeepAnalyzeSettings.shared.activeKind.rawValue
    @State private var modelsPresented = false
    @State private var messages: [ChatMessage] = []
    @State private var hits: [CatalogHit] = []
    @State private var message = "Search your local catalog. File changes are available in File Tools."
    @State private var busy = false
    @State private var ignored = Set<String>()
    @State private var activeRequest = ""
    @State private var historyRequest = ""
    @State private var player: AVPlayer?

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    SectionLabel("Chat")
                    MetaText(useModel ? "Answers cite your files · on this Mac" : "Keyword search only", size: 11)
                }
                Spacer()
                Button("Clear") { control("clear") }
                    .buttonStyle(.dsGhost)
                    .help("Delete this conversation")
                if let onClose {
                    Button { onClose() } label: {
                        Image(systemName: "xmark").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.text2)
                            .frame(width: 24, height: 24).contentShape(Rectangle())
                    }.buttonStyle(.plain).keyboardShortcut(.cancelAction)
                        .help("Close chat").accessibilityLabel("Close chat")
                }
            }
            .padding(.horizontal, 16).padding(.vertical, 12)
            Hairline()
            ScrollViewReader { scroll in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 18) {
                        if messages.isEmpty {
                            VStack(alignment: .leading, spacing: 10) {
                                Text("Find the moment.").font(Theme.Font.body(16, .semibold))
                                Text("Ask about files, people, and moments in your local library.")
                                    .font(Theme.Font.body(13.5)).foregroundStyle(.secondary)
                                ForEach(["Find birthday gift opening", "Show videos with chapter markers"], id: \.self) { suggestion in
                                    Button { input = suggestion; inputFocused = true } label: {
                                        HStack { Text(suggestion); Spacer(); Image(systemName: "arrow.up.left") }
                                            .font(Theme.Font.body(12)).padding(10)
                                            .background(Color.white.opacity(0.05), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                                    }.buttonStyle(.plain)
                                }
                            }.padding(.vertical, 12)
                        }
                        ForEach(messages) { entry in
                            VStack(alignment: .leading, spacing: 6) {
                                Text(entry.role == "user" ? "You" : "FileID")
                                    .font(Theme.Font.body(12, .semibold))
                                    .foregroundStyle(entry.role == "user" ? Theme.gold : Theme.ai)
                                Text(entry.text).font(Theme.Font.body(13.5)).textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }.id(entry.id)
                        }
                        if busy { HStack(spacing: 8) { ProgressView().controlSize(.small); Text("Searching your library…").font(Theme.Font.body(12)).foregroundStyle(.secondary) } }
                    }.padding(16)
                }.onChange(of: messages.last?.id) { _, id in
                    if let id { withAnimation(.easeOut(duration: 0.18)) { scroll.scrollTo(id, anchor: .bottom) } }
                }
            }
            if let player { NativeVideoPlayer(player: player).frame(height: 170).padding(.horizontal, 12) }
            if !hits.isEmpty {
                DisclosureGroup("Evidence (\(hits.count))") {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(Array(hits.enumerated()), id: \.offset) { index, hit in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("[\(index + 1)] \(URL(fileURLWithPath: hit.path).lastPathComponent)")
                                        .font(Theme.Font.body(12, .semibold)).lineLimit(2)
                                    Text(hit.text).font(Theme.Font.body(12)).foregroundStyle(.secondary).lineLimit(3)
                                    HStack {
                                        Button("Open") { open(hit) }.buttonStyle(.borderless)
                                        if let seconds = hit.startSeconds { Text("\(seconds, specifier: "%.1f") s").font(Theme.Font.body(11)).monospacedDigit() }
                                        if let page = hit.page { Text("Page \(page)").font(Theme.Font.body(11)) }
                                        if hit.kind == "sampledFrame" { Text("Unverified frame").font(Theme.Font.body(11)).foregroundStyle(.secondary) }
                                    }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }.padding(.top, 8)
                    }.frame(maxHeight: 180)
                }.font(Theme.Font.body(12, .semibold)).padding(16)
            }
            Divider().overlay(Color.white.opacity(0.08))
            VStack(alignment: .leading, spacing: 10) {
                Text(message).font(Theme.Font.body(12)).foregroundStyle(.secondary).lineLimit(3).textSelection(.enabled)
                HStack(alignment: .center, spacing: 8) {
                    TextField("Ask about your files…", text: $input).textFieldStyle(.plain)
                        .focused($inputFocused).onSubmit(send)
                    if busy {
                        Button { control("cancel") } label: { Image(systemName: "stop.circle.fill") }
                            .buttonStyle(.plain).help("Stop response").accessibilityLabel("Stop response")
                    } else {
                        Button(action: send) { Image(systemName: "arrow.up.circle.fill").font(Theme.Font.body(20, .semibold)).foregroundStyle(Theme.gold) }
                            .buttonStyle(.plain).help("Send message").accessibilityLabel("Send message")
                            .disabled(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || input.count > 2000)
                    }
                }.padding(10).background(Color.white.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                Toggle("Summarize with local AI", isOn: $useModel).font(Theme.Font.body(12))
                Button { modelsPresented = true } label: {
                    Label(AIModelKind(rawValue: chatModelKind)?.displayName ?? "Choose a model", systemImage: "cpu").font(Theme.Font.body(12)).lineLimit(1)
                }.buttonStyle(.plain).foregroundStyle(Theme.ai).help("Choose, download and load a local chat model")
                Text("Keyword search works without a model.").font(Theme.Font.body(11)).foregroundStyle(.secondary)
            }.padding(16)
        }.frame(maxWidth: .infinity, maxHeight: .infinity).background(.ultraThinMaterial, ignoresSafeAreaEdges: []).tint(Theme.gold)
            .onAppear {
                inputFocused = true
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
                if response.requestID == historyRequest, busy { return }
                messages = response.messages; message = response.message
                if response.status != "completed" || !response.hits.isEmpty || activeRequest == response.requestID { hits = response.hits }
                busy = ["retrieving", "queued", "streaming"].contains(response.status)
                if busy, response.requestID != historyRequest { activeRequest = response.requestID }
            }
        .sheet(isPresented: $modelsPresented) {
            ModelCatalogBrowser(engine: engine, initialSelection: AIModelKind(rawValue: chatModelKind)) { chatModelKind = $0.rawValue }
        }
    }

    private func open(_ hit: CatalogHit) {
        let url = URL(fileURLWithPath: hit.path)
        guard FileManager.default.fileExists(atPath: url.path) else {
            message = "This catalog file is offline or unavailable."
            return
        }
        if ["mov", "mp4", "m4v", "mkv", "avi", "mp3", "m4a", "wav", "aiff", "aac"].contains(url.pathExtension.lowercased()) {
            let next = AVPlayer(url: url)
            if let seconds = hit.startSeconds {
                next.seek(to: CMTime(seconds: seconds, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
            }
            player?.pause(); player = next
        } else { NSWorkspace.shared.activateFileViewerSelecting([url]) }
    }

    private func send() {
        guard !busy, !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, input.count <= 2000 else { return }
        activeRequest = UUID().uuidString
        let request = ChatRequest(requestID: activeRequest, conversationID: conversationID, action: "send", text: input, useModel: useModel, modelKind: chatModelKind)
        if engine.send(.chatRequest(request: request)) { input = ""; busy = true }
        else { message = "The engine is unavailable." }
    }

    private func control(_ action: String) {
        if action != "history", !activeRequest.isEmpty { ignored.insert(activeRequest); activeRequest = ""; busy = false }
        let request = ChatRequest(requestID: UUID().uuidString, conversationID: conversationID, action: action)
        if action == "history" { historyRequest = request.requestID }
        if engine.send(.chatRequest(request: request)) {
            if action == "clear" { messages = []; hits = [] }
        } else { message = "The engine is unavailable." }
    }
}
