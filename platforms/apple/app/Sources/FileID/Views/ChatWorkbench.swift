import SwiftUI
import AppKit
import AVKit
import FileIDShared

struct ChatWorkbench: View {
    @Bindable var engine: EngineClient
    var onClose: (() -> Void)? = nil
    @FocusState private var inputFocused: Bool
    @AppStorage("chatConversationID") private var conversationID = ""
    @AppStorage("chatDraft") private var input = ""
    @AppStorage("chatUseModel") private var useModel = true
    @AppStorage("chatModelKind") private var chatModelKind = DeepAnalyzeSettings.shared.activeKind.rawValue
    @State private var modelsPresented = false
    @State private var messages: [ChatMessage] = []
    @State private var hits: [CatalogHit] = []
    @State private var actions: [ChatAction] = []
    @State private var handoff = ChatHandoff.shared
    @State private var message = ""
    @State private var busy = false
    @State private var ignored = Set<String>()
    @State private var activeRequest = ""
    @State private var historyRequest = ""
    @State private var player: AVPlayer?
    @State private var openedSource: Int?
    @State private var sourcesExpanded = false

    private var isPage: Bool { onClose == nil }
    private var modelName: String { AIModelKind(rawValue: chatModelKind)?.displayName ?? "Local model" }
    private static let lookingIn = ["Names", "Tags", "Descriptions", "Chapters", "Text in files"]

    var body: some View {
        HStack(spacing: 0) {
            conversation
                .focusSection()
            if isPage {
                VHairline()
                sourcesPane
                    .frame(width: Theme.Metrics.sidePane)
                    .background(Theme.pane, ignoresSafeAreaEdges: [])
                    .focusSection()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .tint(Theme.gold)
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
            if response.requestID == activeRequest || response.actions != nil { actions = (response.actions ?? []).filter(ChatActions.isRunnable) }
            busy = ["retrieving", "queued", "streaming"].contains(response.status)
            if busy, response.requestID != historyRequest { activeRequest = response.requestID }
        }
        .sheet(isPresented: $modelsPresented) {
            ModelCatalogBrowser(engine: engine, initialSelection: AIModelKind(rawValue: chatModelKind)) { chatModelKind = $0.rawValue }
        }
    }

    // MARK: - Conversation column

    private var conversation: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            ScrollViewReader { scroll in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 20) {
                        if messages.isEmpty { emptyState }
                        ForEach(messages) { entry in messageView(entry).id(entry.id) }
                        if !busy && !actions.isEmpty {
                            VStack(alignment: .leading, spacing: 10) {
                                ForEach(actions) { action in ChatActionCard(action: action, engine: engine) }
                            }
                            .padding(.leading, 12)
                            .transition(.opacity)
                        }
                        if busy {
                            HStack(spacing: 10) {
                                IndeterminateBar().frame(width: 60)
                                MetaText(message.isEmpty ? "Searching your library…" : message, color: Theme.ai)
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    .padding(.horizontal, isPage ? Theme.Metrics.pageInsetX : 16)
                    .padding(.vertical, isPage ? Theme.Metrics.pageInsetY : 16)
                    .frame(maxWidth: isPage ? 760 : .infinity, alignment: .leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .onChange(of: messages.last?.id) { _, id in
                    if let id { withAnimation(Theme.springStandard) { scroll.scrollTo(id, anchor: .bottom) } }
                }
            }
            if !isPage && !hits.isEmpty { inspectorSources }
            Hairline()
            composer
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 6) {
                SectionLabel("Looking in")
                HStack(spacing: 6) {
                    ForEach(isPage ? Self.lookingIn : Array(Self.lookingIn.prefix(3)), id: \.self) { TagChip(text: $0) }
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Looking in names, tags, descriptions, chapters and text in files")
            }
            Spacer(minLength: 8)
            Button("Clear") { control("clear") }
                .buttonStyle(.dsGhost)
                .help(messages.isEmpty && hits.isEmpty ? "Nothing to clear yet" : "Start a new conversation. This one is deleted.")
                .disabled(messages.isEmpty && hits.isEmpty)
            if let onClose {
                DSIconButton("Close chat", systemImage: "xmark") { onClose() }
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(.horizontal, isPage ? Theme.Metrics.pageInsetX : 16).padding(.vertical, 10)
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Find the moment.").font(Theme.Font.display(isPage ? 34 : 28)).foregroundStyle(Theme.text1)
            Text("Ask about files, people and moments in this library, or ask FileID to do something. Answers cite the files they come from, and anything that moves or deletes files waits for your OK.")
                .font(Theme.Font.body(13.5)).foregroundStyle(Theme.text2)
                .fixedSize(horizontal: false, vertical: true)
            VStack(spacing: 6) {
                ForEach(["Find birthday gift opening", "How many videos are from last month?", "How much space do duplicates use?", "Organize my Downloads"], id: \.self) { suggestion in
                    Button { input = suggestion; inputFocused = true } label: {
                        HStack {
                            Text(suggestion).font(Theme.Font.body(13)).foregroundStyle(Theme.text1)
                            Spacer()
                            Image(systemName: "arrow.up.left").font(.system(size: 10, weight: .semibold)).foregroundStyle(Theme.text3)
                                .accessibilityHidden(true)
                        }
                        .padding(.horizontal, 12)
                        .frame(height: 34)
                        .background(Color.white.opacity(0.04), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
                    }
                    .buttonStyle(DSRowButtonStyle(radius: Theme.Radius.s))
                    .accessibilityHint("Puts this question in the message field")
                }
            }
            .frame(maxWidth: 460)
        }
        .padding(.vertical, 8)
    }

    private func messageView(_ entry: ChatMessage) -> some View {
        let user = entry.role == "user"
        let isLastAnswer = !user && entry.id == messages.last(where: { $0.role != "user" })?.id
        return VStack(alignment: .leading, spacing: 6) {
            SectionLabel(user ? "You" : "FileID", color: user ? Theme.gold : Theme.ai)
            Text(entry.text).font(Theme.Font.body(13.5)).foregroundStyle(Theme.text1).textSelection(.enabled)
                .lineSpacing(2)
                .frame(maxWidth: .infinity, alignment: .leading)
            if isLastAnswer && !hits.isEmpty {
                FlowLayout(spacing: 6) {
                    ForEach(Array(hits.prefix(8).enumerated()), id: \.offset) { index, hit in
                        CitationChip(index: index + 1, hit: hit, active: openedSource == index) { open(hit, index: index) }
                    }
                }
                .padding(.top, 2)
            }
        }
        .padding(.leading, 12)
        .overlay(alignment: .leading) { Rectangle().fill((user ? Theme.gold : Theme.ai).opacity(0.5)).frame(width: 2) }
        .accessibilityElement(children: .contain)
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Ask about your files…", text: $input, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text1)
                    .lineLimit(1...5)
                    .focused($inputFocused)
                    .onSubmit(send)
                    .accessibilityLabel("Message")
                    .padding(.horizontal, 10).padding(.vertical, 7)
                    .frame(minHeight: Theme.Metrics.controlLarge)
                    .background(Color.black.opacity(0.3), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                    .overlay { if inputFocused { RoundedRectangle(cornerRadius: Theme.Radius.s + 3, style: .continuous).stroke(Theme.focusRing, lineWidth: 2).padding(-3).allowsHitTesting(false) } }
                if busy {
                    Button("Stop") { control("cancel") }
                        .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.controlLarge))
                        .keyboardShortcut(".", modifiers: .command)
                        .help("Stop the answer (⌘.)")
                } else {
                    Button("Send", action: send)
                        .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.controlLarge))
                        .disabled(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || input.count > 2000)
                        .help(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Type a question first" : input.count > 2000 ? "Shorten the question to 2,000 characters" : "Send (Return)")
                }
            }
            HStack(spacing: 10) {
                Toggle("Summarize with local AI", isOn: $useModel)
                    .toggleStyle(DSToggleStyle())
                    .font(Theme.Font.body(12.5))
                    .foregroundStyle(Theme.text2)
                Button { modelsPresented = true } label: {
                    Label(modelName, systemImage: "cpu").font(Theme.Font.body(12, .semibold)).lineLimit(1)
                }
                .buttonStyle(DSAccentOutlineButtonStyle(color: Theme.ai, height: Theme.Metrics.hitTarget))
                .disabled(!useModel)
                .help(useModel ? "Choose the local AI model that writes answers" : "Turn on Summarize with local AI to choose a model")
                .accessibilityLabel("Chat model: \(modelName)")
                Spacer(minLength: 0)
            }
            if !busy && !message.isEmpty && message != messages.last(where: { $0.role != "user" })?.text {
                Text(message).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).lineLimit(3).textSelection(.enabled)
            } else if !useModel {
                MetaText("Answers list matching files only. Turn on local AI for written answers.")
            }
        }
        .padding(.horizontal, isPage ? Theme.Metrics.pageInsetX : 16).padding(.vertical, 12)
        .frame(maxWidth: isPage ? 760 + Theme.Metrics.pageInsetX * 2 : .infinity, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Sources

    private var sourcesPane: some View {
        VStack(spacing: 0) {
            HStack {
                SectionLabel("Sources")
                Spacer()
                MetaText(hits.isEmpty ? "None yet" : "\(hits.count)", size: 12)
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            Hairline()
            if let player {
                NativeVideoPlayer(player: player)
                    .aspectRatio(16 / 9, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
                    .padding(12)
                    .accessibilityLabel("Source player")
                Hairline()
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(hits.enumerated()), id: \.offset) { index, hit in
                        SourceRow(index: index + 1, hit: hit, selected: openedSource == index) { open(hit, index: index) }
                    }
                }
                .padding(.vertical, 8)
            }
            .overlay {
                if hits.isEmpty {
                    MetaText("Files an answer draws on show here. Click one to open it.")
                        .multilineTextAlignment(.center).padding(24).allowsHitTesting(false)
                }
            }
        }
    }

    private var inspectorSources: some View {
        VStack(alignment: .leading, spacing: 0) {
            Hairline()
            Button { withAnimation(Theme.springStandard) { sourcesExpanded.toggle() } } label: {
                HStack(spacing: 8) {
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .bold)).foregroundStyle(Theme.text3)
                        .rotationEffect(.degrees(sourcesExpanded ? 90 : 0))
                    SectionLabel("Sources · \(hits.count)")
                    Spacer()
                }
                .padding(.horizontal, 16)
                .frame(height: 34)
            }
            .buttonStyle(DSRowButtonStyle(radius: 0))
            .accessibilityLabel("Sources, \(hits.count)")
            .accessibilityValue(sourcesExpanded ? "Expanded" : "Collapsed")
            if sourcesExpanded {
                if let player {
                    NativeVideoPlayer(player: player).frame(height: 170)
                        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                        .padding(.horizontal, 12).padding(.bottom, 8)
                        .accessibilityLabel("Source player")
                }
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(hits.enumerated()), id: \.offset) { index, hit in
                            SourceRow(index: index + 1, hit: hit, selected: openedSource == index) { open(hit, index: index) }
                        }
                    }
                    .padding(.bottom, 8)
                }
                .frame(maxHeight: 200)
                .transition(.opacity)
            }
        }
    }

    // MARK: - Actions

    private func open(_ hit: CatalogHit, index: Int) {
        let url = URL(fileURLWithPath: hit.path)
        guard FileManager.default.fileExists(atPath: url.path) else {
            message = "This file is offline or unavailable."
            return
        }
        openedSource = index
        guard MediaPlayback.extensions.contains(url.pathExtension.lowercased()) else {
            NSWorkspace.shared.open(url)
            return
        }
        Task {
            guard let asset = await MediaPlayback.playableAsset(url) else {
                NSWorkspace.shared.open(url)
                return
            }
            let next = AVPlayer(playerItem: AVPlayerItem(asset: asset))
            if let seconds = hit.startSeconds {
                await next.seek(to: CMTime(seconds: seconds, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
            }
            guard openedSource == index else { return }
            player?.pause()
            withAnimation(Theme.springStandard) { player = next; sourcesExpanded = true }
        }
    }

    private func send() {
        guard !busy, !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, input.count <= 2000 else { return }
        let text = input
        let requestID = UUID().uuidString
        activeRequest = requestID
        actions = []
        busy = true
        Task {
            // The engine fuses CLIP picture matches with keyword evidence when the
            // app's text encoder is loaded; it never loads or downloads one here.
            let vector = CLIPTextEncoder.shared.isReady
                ? await Task.detached(priority: .userInitiated) { CLIPTextEncoder.shared.embedText(text) }.value : nil
            guard activeRequest == requestID else { return }
            let request = ChatRequest(requestID: requestID, conversationID: conversationID, action: "send", text: text, useModel: useModel,
                                      modelKind: chatModelKind, context: handoff.context, queryVector: vector,
                                      embeddingModel: vector == nil ? nil : CLIPEmbeddingSpace.modelID)
            if engine.send(.chatRequest(request: request)) { input = ""; openedSource = nil }
            else { busy = false; message = "FileID is still getting ready. Try again in a moment." }
        }
    }

    private func control(_ action: String) {
        if action != "history", !activeRequest.isEmpty { ignored.insert(activeRequest); activeRequest = ""; busy = false }
        let request = ChatRequest(requestID: UUID().uuidString, conversationID: conversationID, action: action)
        if action == "history" { historyRequest = request.requestID }
        if engine.send(.chatRequest(request: request)) {
            if action == "clear" { messages = []; hits = []; actions = []; player?.pause(); player = nil; openedSource = nil }
        } else { message = "FileID is still getting ready. Try again in a moment." }
    }
}

// MARK: - Citations

private func sourceMeta(_ hit: CatalogHit) -> String {
    var parts: [String] = []
    if let seconds = hit.startSeconds { parts.append(DurationText.clock(seconds)) }
    if let page = hit.page { parts.append("Page \(page)") }
    if hit.kind == "sampledFrame" { parts.append("Video frame, not checked") }
    return parts.joined(separator: " · ")
}

private struct CitationChip: View {
    let index: Int
    let hit: CatalogHit
    let active: Bool
    let action: () -> Void
    var body: some View {
        let name = URL(fileURLWithPath: hit.path).lastPathComponent
        let meta = sourceMeta(hit)
        Button(action: action) {
            HStack(spacing: 5) {
                Text("[\(index)]").font(Theme.Font.mono(11, .semibold)).foregroundStyle(Theme.gold)
                Text(name).font(Theme.Font.mono(11)).foregroundStyle(Theme.text2).lineLimit(1).truncationMode(.middle)
                if !meta.isEmpty { Text("· " + meta).font(Theme.Font.mono(11)).foregroundStyle(Theme.text3).lineLimit(1) }
            }
            .padding(.horizontal, 8)
            .frame(maxWidth: 280, minHeight: Theme.Metrics.hitTarget - 4)
            .background(active ? Theme.gold.opacity(0.12) : Color.white.opacity(0.04), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(active ? Theme.gold.opacity(0.5) : Theme.hairlineStrong, lineWidth: 1))
            .fixedSize()
        }
        .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.s))
        .help(hit.path)
        .contextMenu { Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: hit.path)]) } }
        .accessibilityLabel("Source \(index): \(name)\(meta.isEmpty ? "" : ", " + meta)")
        .accessibilityHint("Opens the file")
    }
}

private struct SourceRow: View {
    let index: Int
    let hit: CatalogHit
    let selected: Bool
    let action: () -> Void
    var body: some View {
        let name = URL(fileURLWithPath: hit.path).lastPathComponent
        let meta = sourceMeta(hit)
        RailRow(selected: selected, action: action) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("[\(index)]").font(Theme.Font.mono(11.5, .semibold)).foregroundStyle(Theme.gold).frame(width: 30, alignment: .leading)
                VStack(alignment: .leading, spacing: 3) {
                    Text(name).font(Theme.Font.mono(12, .medium)).foregroundStyle(Theme.text1).lineLimit(1).truncationMode(.middle)
                    if !meta.isEmpty {
                        Text(meta).font(Theme.Font.body(11.5)).foregroundStyle(hit.kind == "sampledFrame" ? Theme.ai : Theme.text3)
                    }
                    if !hit.text.isEmpty {
                        Text(hit.text).font(Theme.Font.body(12)).foregroundStyle(Theme.text2).lineLimit(3)
                    }
                }
            }
            .padding(.vertical, 7)
        }
        .help(hit.path)
        .contextMenu { Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: hit.path)]) } }
        .accessibilityLabel("Source \(index): \(name)\(meta.isEmpty ? "" : ", " + meta)")
        .accessibilityHint("Opens the file. \(hit.text)")
    }
}
