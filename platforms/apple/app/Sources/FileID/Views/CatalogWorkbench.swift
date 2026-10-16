import SwiftUI
import AVKit
import AppKit
import FileIDShared

struct CatalogWorkbench: View {
    @Bindable var engine: EngineClient
    var store: ReadStore? = nil
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var hits: [CatalogHit] = []
    @State private var chapters: [CatalogChapter] = []
    @State private var cast: [CatalogHit] = []
    @State private var selected: CatalogHit?
    @State private var selectedIndex: Int?
    @State private var pendingID = ""
    @State private var pendingAction = ""
    @State private var message = ""
    @State private var title = ""
    @State private var summary = ""
    @State private var start = 0.0
    @State private var end = 0.0
    @State private var player: AVPlayer?
    @State private var clock = PlayheadClock()
    @State private var duration = 0.0
    @State private var tracks = ReadStore.TimelineTracks.empty
    @State private var editingID: String?
    @State private var editorOpen = false
    @State private var refreshedTimelineJobs: Set<String> = []
    @State private var timelineMode = "sampled"
    @State private var showingTakes = false
    var inline = false

    var body: some View {
        VStack(spacing: 0) {
            if !inline {
                HStack {
                    Text("Search & Moments").font(Theme.Font.display(24)).foregroundStyle(Theme.text1)
                    Spacer()
                    Button("Done") { dismiss() }.buttonStyle(.dsSecondary).keyboardShortcut(.cancelAction)
                }
                .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, 14)
                .overlay(alignment: .bottom) { Hairline() }
            }
            HStack(spacing: 0) {
                resultsPane
                    .frame(width: Theme.Metrics.sidePane)
                    .focusSection()
                VHairline()
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        if let selected {
                            playerCard(selected)
                            chaptersPanel(selected)
                            if !cast.isEmpty { peoplePanel }
                        } else {
                            DashedNotice(title: "Find a moment", subtitle: "Search on the left, then pick a result to play it, see what was examined and edit its chapters.")
                        }
                        queuePanel
                    }
                    .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .focusSection()
            }
        }
        .frame(minWidth: inline ? nil : 900, minHeight: inline ? nil : 680)
        .tint(Theme.gold)
        .sheet(isPresented: $showingTakes) {
            TakeWorkbench(engine: engine, candidates: hits)
        }
        .task {
            while !Task.isCancelled {
                _ = engine.send(.catalogRequest(request: CatalogRequest(requestID: UUID().uuidString, action: "jobs")))
                do { try await Task.sleep(for: .seconds(1)) } catch { break }
            }
        }
        .task(id: selected?.fileID) { await loadTimeline() }
        .onChange(of: engine.catalogResponses) { _, _ in consume() }
        .onChange(of: engine.catalogJobs) { _, jobs in
            guard let selected,
                  let completed = jobs.first(where: {
                      $0.kind == "timelineSample" && $0.state == "completed"
                          && $0.fileIDs.contains(selected.fileID) && !refreshedTimelineJobs.contains($0.id)
                  }) else { return }
            refreshedTimelineJobs.insert(completed.id)
            send(CatalogRequest(requestID: UUID().uuidString, action: "detail", fileID: selected.fileID))
            Task { await loadTimeline() }
        }
        .onDisappear { player?.pause(); clock.attach(nil) }
    }

    // MARK: - Results pane

    private var resultsPane: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                DSTextField(placeholder: "Search names, captions, chapters", text: $query, icon: "magnifyingglass", onSubmit: search)
                HStack(spacing: 8) {
                    if pendingAction == "search" {
                        IndeterminateBar().frame(width: 80)
                    } else {
                        MetaText(resultsMeta, size: 12)
                    }
                    Spacer(minLength: 4)
                    Button("Best takes") { showingTakes = true }
                        .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                        .disabled(Set(hits.map(\.fileID)).count < 2)
                        .help("Group these results as attempts and pick the best take")
                }
            }
            .padding(14)
            Hairline()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(hits.enumerated()), id: \.offset) { index, hit in
                        RailRow(selected: selectedIndex == index, action: { select(hit, at: index) }) {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(URL(fileURLWithPath: hit.path).lastPathComponent)
                                    .font(Theme.Font.mono(12.5, .medium)).foregroundStyle(Theme.text1)
                                    .lineLimit(1).truncationMode(.middle)
                                Text(hitMeta(hit)).font(Theme.Font.body(11.5)).foregroundStyle(hit.kind == "sampledFrame" ? Theme.ai : Theme.text3).lineLimit(1)
                                if !hit.text.isEmpty {
                                    Text(hit.text).font(Theme.Font.body(12)).foregroundStyle(Theme.text2).lineLimit(2)
                                }
                            }
                            .padding(.vertical, 7)
                        }
                        .accessibilityLabel("\(URL(fileURLWithPath: hit.path).lastPathComponent), \(hitMeta(hit))")
                        .accessibilityHint(hit.text)
                    }
                }
                .padding(.vertical, 8)
            }
            .overlay {
                if hits.isEmpty && pendingAction != "search" {
                    MetaText(message.isEmpty ? "Results show here: file names, captions, chapters and sampled video frames." : message)
                        .multilineTextAlignment(.center).padding(24).allowsHitTesting(false)
                }
            }
        }
        .background(Theme.pane, ignoresSafeAreaEdges: [])
    }

    private var resultsMeta: String {
        if hits.isEmpty { return query.isEmpty ? "Type and press Return" : "No results" }
        return "\(hits.count.formatted()) result\(hits.count == 1 ? "" : "s")"
    }

    private func hitMeta(_ hit: CatalogHit) -> String {
        var parts: [String] = []
        if let time = hit.startSeconds { parts.append(Timecode.string(time)) }
        if let page = hit.page { parts.append("Page \(page)") }
        switch hit.kind {
        case "sampledFrame": parts.append("Sampled frame, unverified")
        case "chapter": parts.append("Chapter")
        case "passage": parts.append("Passage")
        case "person": parts.append("Person")
        case "documentText": parts.append("Text in document")
        case "ocrText": parts.append("Text in image")
        default: parts.append(hit.kind.capitalized)
        }
        return parts.joined(separator: " · ")
    }

    // MARK: - Player card

    private func playerCard(_ hit: CatalogHit) -> some View {
        let url = URL(fileURLWithPath: hit.path)
        let video = isVideo(hit.path)
        return Panel {
            PanelHeader("Player") {
                MetaText(video && duration > 0 ? Timecode.string(clock.seconds) + " / " + Timecode.string(duration) : hitMeta(hit), size: 12)
                    .monospacedDigit()
            }
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(url.lastPathComponent).font(Theme.Font.mono(13, .medium)).foregroundStyle(Theme.text1)
                        .lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                    Text(url.deletingLastPathComponent().path).font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                        .lineLimit(1).truncationMode(.head)
                }
                if let player {
                    NativeVideoPlayer(player: player)
                        .aspectRatio(16 / 9, contentMode: .fit)
                        .frame(maxHeight: 340)
                        .frame(maxWidth: .infinity)
                        .background(Color.black, in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
                        .accessibilityLabel("Video player")
                }
                if video {
                    MomentTimeline(duration: timelineDuration, examined: tracks.examined, chapters: chapters,
                                   speech: tracks.speech, playhead: clock.seconds, onSeek: seek)
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            PanelFooter {
                if video {
                    SegmentedBar(selection: $timelineMode, options: [("sampled", "Quick samples"), ("moments", "Full moments")])
                        .help("Quick samples looks at a frame every few seconds. Full moments reads frame sequences and speech, and takes longer.")
                    Button("Analyze") {
                        send(CatalogRequest(requestID: UUID().uuidString, action: "enqueueTimeline", fileIDs: [hit.fileID], timelineMode: timelineMode))
                    }
                    .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget))
                    .help("Queue this video for timeline analysis. Load a model in Deep Analyze first.")
                }
                Spacer(minLength: 8)
                if video {
                    Button("Play from start") { seek(0); player?.play() }
                        .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                }
                Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([url]) }
                    .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
            }
        }
    }

    private var timelineDuration: Double {
        if duration > 0 { return duration }
        let ends = chapters.map(\.endSeconds) + tracks.examined.map(\.end) + tracks.speech.map(\.end)
        return max(ends.max() ?? 0, 1)
    }

    // MARK: - Chapters

    private func chaptersPanel(_ hit: CatalogHit) -> some View {
        Panel {
            PanelHeader("Chapters · \(chapters.count)") {
                Button("Undo last edit") {
                    send(CatalogRequest(requestID: UUID().uuidString, action: "undoChapterEdit", fileID: hit.fileID))
                }
                .buttonStyle(.dsGhost)
                Button { openEditor(nil) } label: { Label("Add chapter", systemImage: "plus") }
                    .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
            }
            if let note = draftNote {
                Text(note).font(Theme.Font.body(12)).foregroundStyle(Theme.ai)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 14).padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.ai.opacity(0.06), ignoresSafeAreaEdges: [])
                    .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
            }
            if chapters.isEmpty && !editorOpen {
                MetaText("No chapters yet. Analyze the video for drafts, or add one at the playhead.")
                    .padding(.horizontal, 14).padding(.vertical, 12)
            }
            ForEach(chapters, id: \.id) { chapter in
                chapterRow(chapter, fileID: hit.fileID)
            }
            if editorOpen { chapterEditor(fileID: hit.fileID) }
            PanelFooter {
                MetaText("Chapters live in FileID's catalog. The video file is never changed.", size: 11.5)
                Spacer(minLength: 0)
            }
        }
    }

    private var draftNote: String? {
        let drafts = chapters.filter { !$0.userEdited && !$0.stale }
        if drafts.contains(where: { $0.modelVersion.hasPrefix("timeline-moment-sequence-v1/") }) {
            return "Drafts describe sampled frame sequences. Check the action, outcome and timing before accepting."
        }
        if drafts.contains(where: { $0.modelVersion.hasPrefix("timeline-chapter-suggestion-v1/") }) {
            return "Drafts come from sparse frame samples, so brief events can be missed. Review before relying on them."
        }
        return nil
    }

    private func chapterStatus(_ chapter: CatalogChapter) -> (String, Color) {
        if chapter.stale { return ("Stale", Theme.warning) }
        if chapter.userEdited { return ("Accepted", Theme.delight) }
        return ("Draft", Theme.ai)
    }

    private func chapterRow(_ chapter: CatalogChapter, fileID: Int64) -> some View {
        let status = chapterStatus(chapter)
        let current = duration > 0 && clock.seconds >= chapter.startSeconds && clock.seconds < max(chapter.endSeconds, chapter.startSeconds + 0.5)
        let draft = !chapter.userEdited && !chapter.stale
        return HStack(alignment: .center, spacing: 12) {
            Button { seek(chapter.startSeconds) } label: {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Text(Timecode.string(chapter.startSeconds))
                        .font(Theme.Font.mono(12, .medium)).foregroundStyle(current ? Theme.gold : Theme.text2)
                        .frame(width: 56, alignment: .leading)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(chapter.title).font(Theme.Font.body(13, .medium)).foregroundStyle(Theme.text1).lineLimit(1)
                        if !chapter.summary.isEmpty {
                            Text(chapter.summary).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).lineLimit(2)
                        }
                    }
                    Spacer(minLength: 0)
                }
                .padding(.vertical, 4)
                .frame(minHeight: Theme.Metrics.hitTarget)
            }
            .buttonStyle(DSRowButtonStyle(radius: Theme.Radius.s))
            .accessibilityLabel("\(chapter.title), at \(Timecode.spoken(chapter.startSeconds))")
            .accessibilityHint("Plays from this chapter")
            StatusPill(text: status.0, color: status.1)
            if draft {
                Text("\(Int((chapter.confidence * 100).rounded()))%")
                    .font(Theme.Font.mono(11)).foregroundStyle(Theme.text3).frame(width: 34, alignment: .trailing)
                    .accessibilityLabel("Confidence \(Int((chapter.confidence * 100).rounded())) percent")
                Button("Accept") { accept(chapter) }
                    .buttonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.hitTarget))
            }
            Button("Rename") { openEditor(chapter) }
                .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
            DSIconButton("Delete chapter", systemImage: "trash", tint: Theme.text3) {
                send(CatalogRequest(requestID: UUID().uuidString, action: "deleteChapter", fileID: fileID, chapterID: chapter.id))
            }
        }
        .padding(.leading, 14).padding(.trailing, 10).padding(.vertical, 6)
        .background(current ? Theme.goldWash : Color.clear, ignoresSafeAreaEdges: [])
        .overlay(alignment: .leading) { if current { Rectangle().fill(Theme.gold).frame(width: 3) } }
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
        .animation(Theme.spring, value: current)
    }

    private func chapterEditor(fileID: Int64) -> some View {
        let valid = !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && start.isFinite && end.isFinite && start >= 0 && end >= start
        return VStack(alignment: .leading, spacing: 10) {
            SectionLabel(editingID == nil ? "New chapter" : "Edit chapter")
            DSTextField(placeholder: "Chapter title", text: $title, height: Theme.Metrics.control)
            DSTextField(placeholder: "Description", text: $summary, height: Theme.Metrics.control)
            HStack(spacing: 14) {
                LabeledSegment("Start") {
                    DSNumberField(placeholder: "Start seconds", value: $start, format: .number)
                    Button("Playhead") { start = clock.seconds }.buttonStyle(.dsGhost).disabled(player == nil)
                }
                LabeledSegment("End") {
                    DSNumberField(placeholder: "End seconds", value: $end, format: .number)
                    Button("Playhead") { end = clock.seconds }.buttonStyle(.dsGhost).disabled(player == nil)
                }
                Spacer(minLength: 8)
                Button("Cancel") { closeEditor() }
                    .buttonStyle(.dsGhost)
                    .keyboardShortcut(.cancelAction)
                Button(editingID == nil ? "Add chapter" : "Save chapter") {
                    let chapter = CatalogChapter(id: editingID ?? UUID().uuidString, fileID: fileID, startSeconds: start, endSeconds: end,
                                                 title: title, summary: summary, sourceRevision: "", modelVersion: "user",
                                                 confidence: 1, userEdited: true, stale: false)
                    send(CatalogRequest(requestID: UUID().uuidString, action: "saveChapter", chapter: chapter))
                }
                .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget))
                .keyboardShortcut(.defaultAction)
                .disabled(!valid)
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .background(Color.white.opacity(0.02), ignoresSafeAreaEdges: [])
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
        .transition(.opacity)
    }

    // MARK: - People

    private var peoplePanel: some View {
        Panel {
            PanelHeader("People in this file · \(cast.count)")
            ForEach(cast, id: \.evidenceID) { appearance in
                HStack(spacing: 12) {
                    Circle().fill(appearance.text.hasSuffix("Reviewed") ? Theme.gold : Theme.ai).frame(width: 7, height: 7)
                        .accessibilityHidden(true)
                    Text(appearance.text).font(Theme.Font.body(13)).foregroundStyle(Theme.text1).lineLimit(1)
                    Spacer(minLength: 0)
                    if let time = appearance.startSeconds {
                        Button(Timecode.string(time)) { seek(time) }
                            .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                            .font(Theme.Font.mono(12))
                            .disabled(player == nil)
                            .accessibilityLabel("Play from \(Timecode.spoken(time))")
                    } else {
                        MetaText("Sampled image", size: 11.5)
                    }
                }
                .padding(.horizontal, 14).padding(.vertical, 6)
                .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
            }
            PanelFooter {
                MetaText("Detected names need review in People. Untimed appearances come from sampled images.", size: 11.5)
                Spacer(minLength: 0)
            }
        }
    }

    // MARK: - Analysis queue

    private var queuePanel: some View {
        Panel {
            PanelHeader("Analysis queue") {
                MetaText(engine.catalogJobs.isEmpty ? "Idle" : "\(engine.catalogJobs.count) job\(engine.catalogJobs.count == 1 ? "" : "s")", size: 12)
            }
            if engine.catalogJobs.isEmpty {
                MetaText("Nothing queued. Pick a video and choose Analyze to sample its frames.")
                    .padding(.horizontal, 14).padding(.vertical, 12)
            }
            ForEach(engine.catalogJobs, id: \.id) { job in
                CatalogJobRow(job: job) { action in
                    switch action {
                    case "retryTimeline": send(CatalogRequest(requestID: UUID().uuidString, action: "enqueueTimeline", fileIDs: job.fileIDs))
                    default: send(CatalogRequest(requestID: UUID().uuidString, action: action, jobID: job.id))
                    }
                }
            }
            PanelFooter {
                MetaText("Visual sampling can miss events between frames. Load a model in Deep Analyze before starting.", size: 11.5)
                Spacer(minLength: 0)
            }
        }
    }

    // MARK: - Actions

    private func select(_ hit: CatalogHit, at index: Int) {
        selected = hit
        selectedIndex = index
        closeEditor()
        cast = []
        chapters = []
        start = hit.startSeconds ?? 0
        end = start
        send(CatalogRequest(requestID: UUID().uuidString, action: "detail", fileID: hit.fileID))
        player?.pause()
        if isVideo(hit.path) {
            let next = AVPlayer(url: URL(fileURLWithPath: hit.path))
            next.seek(to: CMTime(seconds: hit.startSeconds ?? 0, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
            player = next
        } else {
            player = nil
        }
        clock.attach(player)
    }

    private func loadTimeline() async {
        duration = 0
        tracks = .empty
        guard let hit = selected, isVideo(hit.path) else { return }
        let id = hit.fileID
        if let store {
            let loaded = await Task.detached { store.timelineTracks(forFileID: id) }.value
            guard !Task.isCancelled, selected?.fileID == id else { return }
            tracks = loaded
        }
        let seconds = (try? await AVURLAsset(url: URL(fileURLWithPath: hit.path)).load(.duration))?.seconds ?? 0
        guard !Task.isCancelled, selected?.fileID == id else { return }
        duration = seconds.isFinite ? seconds : 0
    }

    private func seek(_ seconds: Double) {
        guard let player else { return }
        player.seek(to: CMTime(seconds: max(0, seconds), preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
        clock.seconds = max(0, seconds)
    }

    private func accept(_ chapter: CatalogChapter) {
        var accepted = chapter
        accepted.userEdited = true
        accepted.modelVersion = "user"
        accepted.confidence = 1
        send(CatalogRequest(requestID: UUID().uuidString, action: "saveChapter", chapter: accepted))
    }

    private func openEditor(_ chapter: CatalogChapter?) {
        withAnimation(Theme.springStandard) {
            editingID = chapter?.id
            title = chapter?.title ?? ""
            summary = chapter?.summary ?? ""
            start = chapter?.startSeconds ?? clock.seconds
            end = chapter?.endSeconds ?? clock.seconds
            editorOpen = true
        }
        if let chapter { seek(chapter.startSeconds) }
    }

    private func closeEditor() {
        withAnimation(Theme.springStandard) {
            editorOpen = false
            editingID = nil
            title = ""
            summary = ""
        }
    }

    private func search() {
        guard !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        send(CatalogRequest(requestID: UUID().uuidString, action: "search", query: query))
    }

    private func send(_ request: CatalogRequest) {
        pendingID = request.requestID
        pendingAction = request.action
        message = ""
        if !engine.send(.catalogRequest(request: request)) {
            pendingID = ""
            pendingAction = ""
            message = "The engine is unavailable."
        }
    }

    private func consume() {
        guard !pendingID.isEmpty, let response = engine.catalogResponses[pendingID] else { return }
        let action = pendingAction
        pendingID = ""
        pendingAction = ""
        message = response.message ?? (response.status == "ok" ? "" : "The request failed.")
        if !message.isEmpty, action != "search" { ToastCenter.shared.show(message) }
        guard response.status == "ok" else { return }
        switch action {
        case "search":
            hits = response.hits
            selectedIndex = nil
            if hits.isEmpty { message = "No matches in names, captions, chapters or sampled frames." }
        case "jobs":
            engine.applyCatalogJobsSnapshot(response.jobs)
        case "detail", "saveChapter", "undoChapterEdit":
            withAnimation(Theme.springStandard) { chapters = response.chapters }
            if action == "detail" { cast = response.hits.filter { $0.kind == "person" } } else { closeEditor() }
        case "deleteChapter":
            if let selected { send(CatalogRequest(requestID: UUID().uuidString, action: "detail", fileID: selected.fileID)) }
        case "enqueueTimeline":
            ToastCenter.shared.show("Queued for analysis")
        default:
            break
        }
    }

    private func isVideo(_ path: String) -> Bool { ["mov","mp4","m4v","mkv","avi","mpg","mpeg","mts","m2ts","webm"].contains(URL(fileURLWithPath: path).pathExtension.lowercased()) }
}

// MARK: - Playhead

@MainActor @Observable
final class PlayheadClock {
    var seconds: Double = 0
    private var token: Any?
    private weak var player: AVPlayer?

    func attach(_ next: AVPlayer?) {
        if let token, let player { player.removeTimeObserver(token) }
        token = nil
        player = next
        seconds = 0
        guard let next else { return }
        token = next.addPeriodicTimeObserver(forInterval: CMTime(seconds: 0.25, preferredTimescale: 600), queue: .main) { [weak self] time in
            MainActor.assumeIsolated { self?.seconds = time.seconds.isFinite ? time.seconds : 0 }
        }
    }
}

// MARK: - Timeline tracks

/// Examined / Chapters / Speech lanes over a shared time ruler. Clicking a
/// lane seeks; the gold playhead crosses all three.
struct MomentTimeline: View {
    let duration: Double
    let examined: [ReadStore.TimelineSpan]
    let chapters: [CatalogChapter]
    let speech: [ReadStore.TimelineSpan]
    let playhead: Double
    let onSeek: (Double) -> Void

    static let labelWidth: CGFloat = 70
    static let laneHeight: CGFloat = 16

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            lane("Examined", value: examined.isEmpty ? "Not analyzed yet" : "\(examined.count) sampled frame\(examined.count == 1 ? "" : "s")") { context, size in
                for span in examined {
                    let x0 = x(span.start, size.width), x1 = x(span.end, size.width)
                    if x1 - x0 < 1.5 {
                        context.fill(Path(CGRect(x: x0 - 0.5, y: 2, width: 1, height: size.height - 4)), with: .color(Theme.ai.opacity(0.9)))
                    } else {
                        context.fill(Path(roundedRect: CGRect(x: x0, y: 3, width: x1 - x0, height: size.height - 6), cornerRadius: 2), with: .color(Theme.ai.opacity(0.45)))
                    }
                }
            }
            lane("Chapters", value: chapters.isEmpty ? "None" : "\(chapters.count) chapter\(chapters.count == 1 ? "" : "s")") { context, size in
                for chapter in chapters {
                    let x0 = x(chapter.startSeconds, size.width)
                    let x1 = max(x0 + 3, x(max(chapter.endSeconds, chapter.startSeconds), size.width) - 1)
                    let color: Color = chapter.stale ? Theme.paused : (chapter.userEdited ? Theme.gold : Theme.ai)
                    let rect = CGRect(x: x0, y: 2, width: x1 - x0, height: size.height - 4)
                    context.fill(Path(roundedRect: rect, cornerRadius: 3), with: .color(color.opacity(chapter.userEdited ? 0.55 : 0.32)))
                    context.stroke(Path(roundedRect: rect, cornerRadius: 3), with: .color(color.opacity(0.85)), lineWidth: 1)
                }
            }
            lane("Speech", value: speech.isEmpty ? "None" : "\(speech.count) passage\(speech.count == 1 ? "" : "s")") { context, size in
                for span in speech {
                    let x0 = x(span.start, size.width), x1 = max(x0 + 2, x(span.end, size.width))
                    context.fill(Path(roundedRect: CGRect(x: x0, y: 4, width: x1 - x0, height: size.height - 8), cornerRadius: 2), with: .color(Theme.info.opacity(0.6)))
                }
            }
            HStack(spacing: 10) {
                Color.clear.frame(width: Self.labelWidth, height: 1)
                Canvas { context, size in
                    let step = Self.rulerStep(duration)
                    var t = 0.0
                    while t <= duration + 0.001 {
                        let px = x(t, size.width)
                        context.fill(Path(CGRect(x: px, y: 0, width: 1, height: 5)), with: .color(Theme.hairlineStrong))
                        let label = context.resolve(Text(Timecode.string(t)).font(Theme.Font.mono(10.5)).foregroundStyle(Theme.text3))
                        let anchor: UnitPoint = t == 0 ? .topLeading : (t + step > duration + 0.001 ? .topTrailing : .top)
                        context.draw(label, at: CGPoint(x: px, y: 7), anchor: anchor)
                        t += step
                    }
                }
                .frame(height: 20)
            }
            .accessibilityHidden(true)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Timeline")
    }

    private func x(_ seconds: Double, _ width: CGFloat) -> CGFloat {
        guard duration > 0 else { return 0 }
        return CGFloat(max(0, min(seconds, duration)) / duration) * width
    }

    private func lane(_ name: String, value: String, draw: @escaping (inout GraphicsContext, CGSize) -> Void) -> some View {
        HStack(spacing: 10) {
            Text(name).font(Theme.Font.body(11.5, .medium)).foregroundStyle(Theme.text3)
                .frame(width: Self.labelWidth, alignment: .leading)
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    RoundedRectangle(cornerRadius: Theme.Radius.xs, style: .continuous).fill(Color.white.opacity(0.045))
                    Canvas { context, size in draw(&context, size) }
                    Rectangle().fill(Theme.gold).frame(width: 1.5).offset(x: x(playhead, geo.size.width) - 0.75)
                }
                .contentShape(Rectangle())
                .onTapGesture { location in onSeek(Double(location.x / max(geo.size.width, 1)) * duration) }
            }
            .frame(height: Self.laneHeight)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(name)
        .accessibilityValue(value)
    }

    static func rulerStep(_ duration: Double) -> Double {
        let steps: [Double] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200]
        return steps.first { duration / $0 <= 8 } ?? 7200
    }
}

// MARK: - Shared rows

struct StatusPill: View {
    let text: String
    let color: Color
    var body: some View {
        Text(text).font(Theme.Font.body(10.5, .semibold)).foregroundStyle(color)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.xs, style: .continuous).stroke(color.opacity(0.6), lineWidth: 1))
            .fixedSize()
    }
}

struct CatalogJobRow: View {
    let job: CatalogJob
    let control: (String) -> Void

    private var title: String {
        if job.kind == "catalogIndex" { return "Search index" }
        let count = job.fileIDs.count
        return "\(count) video\(count == 1 ? "" : "s") · timeline"
    }

    private var tone: (Color, ProgressTone) {
        switch job.state {
        case "running": return (Theme.gold, .gold)
        case "paused": return (Theme.text3, .paused)
        case "completed": return (Theme.delight, .done)
        case "failed", "cancelled": return (Theme.danger, .neutral)
        default: return (Theme.text3, .neutral)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Text(title).font(Theme.Font.body(13, .medium)).foregroundStyle(Theme.text1)
                Text(job.state.capitalized).font(Theme.Font.body(12, .semibold)).foregroundStyle(tone.0)
                Spacer(minLength: 8)
                Text("\(Int((job.progress * 100).rounded()))%").font(Theme.Font.mono(11.5)).foregroundStyle(Theme.text3)
                if ["running", "queued"].contains(job.state) {
                    Button("Pause") { control("pauseJob") }.buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                }
                if job.state == "paused" {
                    Button("Resume") { control("resumeJob") }.buttonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.hitTarget))
                }
                if job.kind == "catalogIndex", ["failed", "cancelled"].contains(job.state) {
                    Button("Retry") { control("resumeJob") }.buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                } else if job.state == "failed" {
                    Button("Retry") { control("retryTimeline") }.buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                }
                if ["running", "queued", "paused"].contains(job.state) {
                    Button("Cancel") { control("cancelJob") }.buttonStyle(.dsGhost)
                }
            }
            GrainProgressBar(progress: job.progress, tone: tone.1, height: 4)
            if let error = job.error, !error.isEmpty {
                Text(error).font(Theme.Font.body(12)).foregroundStyle(Theme.danger).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title), \(job.state)")
    }
}

enum Timecode {
    static func string(_ seconds: Double) -> String {
        let total = Int(max(0, seconds.isFinite ? seconds : 0).rounded(.down))
        let h = total / 3600, m = (total % 3600) / 60, s = total % 60
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }

    static func spoken(_ seconds: Double) -> String {
        let total = Int(max(0, seconds.isFinite ? seconds : 0).rounded(.down))
        let h = total / 3600, m = (total % 3600) / 60, s = total % 60
        var parts: [String] = []
        if h > 0 { parts.append("\(h) hour\(h == 1 ? "" : "s")") }
        if m > 0 { parts.append("\(m) minute\(m == 1 ? "" : "s")") }
        parts.append("\(s) second\(s == 1 ? "" : "s")")
        return parts.joined(separator: " ")
    }
}
