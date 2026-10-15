import SwiftUI
import AVKit
import AppKit
import FileIDShared

struct CatalogWorkbench: View {
    @Bindable var engine: EngineClient
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var hits: [CatalogHit] = []
    @State private var chapters: [CatalogChapter] = []
    @State private var cast: [CatalogHit] = []
    @State private var selected: CatalogHit?
    @State private var pendingID = ""
    @State private var pendingAction = ""
    @State private var message = ""
    @State private var title = ""
    @State private var summary = ""
    @State private var start = 0.0
    @State private var end = 0.0
    @State private var player: AVPlayer?
    @State private var editingID: String?
    @State private var refreshedTimelineJobs: Set<String> = []
    @State private var timelineMode = "sampled"
    @State private var showingTakes = false
    var inline = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if !inline {
                HStack {
                    Text("Search & Moments").font(Theme.Font.display(24))
                    Spacer()
                    Button("Done") { dismiss() }.buttonStyle(.dsSecondary)
                }
            }
            HStack {
                TextField("Search names, descriptions, chapters, or sampled video frames", text: $query)
                    .textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), ignoresSafeAreaEdges: []).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1)).onSubmit { search() }
                Button("Search", action: search).disabled(query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Button("Best Takes") { showingTakes = true }
                Button("Refresh jobs") { send(CatalogRequest(requestID: UUID().uuidString, action: "jobs")) }
            }
            if !message.isEmpty { Text(message).font(Theme.Font.body(12)).foregroundStyle(.secondary) }
            HSplitView {
                List(Array(hits.enumerated()), id: \.offset) { _, hit in
                    Button {
                        selected = hit
                        editingID = nil
                        title = ""
                        summary = ""
                        cast = []
                        start = hit.startSeconds ?? 0
                        end = start
                        send(CatalogRequest(requestID: UUID().uuidString, action: "detail", fileID: hit.fileID))
                        if isVideo(hit.path) {
                            let next = AVPlayer(url: URL(fileURLWithPath: hit.path))
                            next.seek(to: CMTime(seconds: hit.startSeconds ?? 0, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
                            player = next
                        } else { player = nil }
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(URL(fileURLWithPath: hit.path).lastPathComponent).font(Theme.Font.body(14, .semibold)).lineLimit(1)
                            if let time = hit.startSeconds { Text("At \(time, specifier: "%.1f") seconds · \(hit.kind == "sampledFrame" ? "sampled frame, unverified" : hit.kind)").font(Theme.Font.body(12)).foregroundStyle(.secondary) }
                            if !hit.text.isEmpty { Text(hit.text).font(Theme.Font.body(12)).lineLimit(3) }
                    }.frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                    }.buttonStyle(.plain)
                }.dsList(empty: hits.isEmpty ? "Search names, descriptions, chapters or sampled frames." : nil).frame(minWidth: 260)
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if let selected {
                            Text(URL(fileURLWithPath: selected.path).lastPathComponent).font(Theme.Font.body(14, .semibold))
                            if let player { NativeVideoPlayer(player: player).frame(height: 220) }
                            if !cast.isEmpty {
                                VStack(alignment: .leading, spacing: 8) {
                                    Label("People in this file", systemImage: "person.2").font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.ai)
                                    ForEach(cast, id: \.evidenceID) { appearance in
                                        Button {
                                            if let time = appearance.startSeconds { player?.seek(to: CMTime(seconds: time, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero) }
                                        } label: {
                                            HStack {
                                                Text(appearance.text).font(Theme.Font.body(13.5)).lineLimit(2)
                                                Spacer()
                                                if let time = appearance.startSeconds { Text("\(time, specifier: "%.1f")s").font(Theme.Font.body(12).monospacedDigit()) }
                                            }
                                        }.buttonStyle(.plain).disabled(appearance.startSeconds == nil || player == nil)
                                    }
                                    Text("Detected identities need review in People. Untimed appearances come from sampled images.")
                                        .font(Theme.Font.body(11)).foregroundStyle(.secondary)
                                }.padding(.vertical, 8)
                            }
                            HStack {
                                Button(isVideo(selected.path) ? "Play from start" : "Show in Finder") {
                                    let url = URL(fileURLWithPath: selected.path)
                                    if isVideo(selected.path) { player?.pause(); player = AVPlayer(url: url) }
                                    else { NSWorkspace.shared.activateFileViewerSelecting([url]) }
                                }
                if isVideo(selected.path) {
                    SegmentedBar(selection: $timelineMode, options: [("sampled", "Quick samples"), ("moments", "Full moments")], height: 26)
                                    Button("Analyze") {
                        send(CatalogRequest(requestID: UUID().uuidString, action: "enqueueTimeline", fileIDs: [selected.fileID], timelineMode: timelineMode))
                                    }
                                }
                            }
                Text("Chapters").font(Theme.Font.body(14, .semibold))
                if chapters.contains(where: { $0.modelVersion.hasPrefix("timeline-moment-sequence-v1/") && !$0.userEdited && !$0.stale }) {
                    Text("Moment drafts describe sampled frame sequences. Review actions, outcomes, and timing before accepting.")
                        .font(Theme.Font.body(12)).foregroundStyle(.secondary)
                }
                            if chapters.contains(where: { $0.modelVersion.hasPrefix("timeline-chapter-suggestion-v1/") && !$0.userEdited && !$0.stale }) {
                                Text("Draft suggestions use sparse frame samples. Review before relying on them; brief events can be missed.")
                                    .font(Theme.Font.body(12)).foregroundStyle(.secondary)
                            }
                            Button("Undo last chapter edit") {
                                send(CatalogRequest(requestID: UUID().uuidString, action: "undoChapterEdit", fileID: selected.fileID))
                            }
                            ForEach(chapters, id: \.id) { chapter in
                                HStack {
                                    Button {
                                        editingID = chapter.id
                                        title = chapter.title
                                        summary = chapter.summary
                                        start = chapter.startSeconds
                                        end = chapter.endSeconds
                                        player?.seek(to: CMTime(seconds: chapter.startSeconds, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
                                    } label: {
                                        Text("\(chapter.startSeconds, specifier: "%.1f")s · \(chapter.title)\(chapter.stale ? " · stale" : "")")
                                    }.buttonStyle(.plain)
                                    if chapter.modelVersion.hasPrefix("timeline-chapter-suggestion-v1/") && !chapter.userEdited {
                                        Text("Draft").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    Button(role: .destructive) {
                                        send(CatalogRequest(requestID: UUID().uuidString, action: "deleteChapter", fileID: selected.fileID, chapterID: chapter.id))
                                    } label: { Image(systemName: "minus.circle") }
                                }
                            }
                            TextField("Chapter title", text: $title).textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), ignoresSafeAreaEdges: []).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                            TextField("Description", text: $summary).textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), ignoresSafeAreaEdges: []).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                            HStack {
                                TextField("Start seconds", value: $start, format: .number).textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), ignoresSafeAreaEdges: []).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                                TextField("End seconds", value: $end, format: .number).textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), ignoresSafeAreaEdges: []).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                            }
                            HStack {
                                Button(editingID == nil ? "Add chapter" : "Save chapter") {
                                    let chapter = CatalogChapter(id: editingID ?? UUID().uuidString, fileID: selected.fileID, startSeconds: start, endSeconds: end, title: title, summary: summary, sourceRevision: "", modelVersion: "user", confidence: 1, userEdited: true, stale: false)
                                    send(CatalogRequest(requestID: UUID().uuidString, action: "saveChapter", chapter: chapter))
                                }.disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !start.isFinite || !end.isFinite || start < 0 || end < start)
                                if editingID != nil { Button("New chapter") { editingID = nil; title = ""; summary = "" } }
                            }
                            Text("Markers are stored in the internal catalog. Original files are never changed.").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                        } else { Text("Select a result to play a matching moment or edit chapters.").foregroundStyle(.secondary) }
                        Divider()
                Text("Analysis jobs").font(Theme.Font.body(14, .semibold))
                        Text("Visual sampling can miss events between frames. Load a model in Deep Analyze before starting.").font(Theme.Font.body(12)).foregroundStyle(.secondary)
                        ForEach(engine.catalogJobs, id: \.id) { job in
                            VStack(alignment: .leading, spacing: 4) {
                        Text(job.kind == "catalogIndex" ? "Search index · \(job.state)" : "\(job.fileIDs.count) video(s) · \(job.state)").font(Theme.Font.body(12.5))
                                ProgressView(value: job.progress)
                                if let error = job.error { Text(error).font(Theme.Font.body(12)).foregroundStyle(.secondary) }
                                HStack {
                                    if ["running", "queued"].contains(job.state) { Button("Pause") { control(job, action: "pauseJob") } }
                                if job.state == "paused" { Button("Resume") { control(job, action: "resumeJob") } }
                            if job.kind == "catalogIndex", ["failed", "cancelled"].contains(job.state) { Button("Retry") { control(job, action: "resumeJob") } }
                            else if job.state == "failed" { Button("Retry") { send(CatalogRequest(requestID: UUID().uuidString, action: "enqueueTimeline", fileIDs: job.fileIDs)) } }
                                    if ["running", "queued", "paused"].contains(job.state) { Button("Cancel") { control(job, action: "cancelJob") } }
                                }
                            }
                        }
                    }.padding(12)
                }.frame(minWidth: 380)
            }
        }
        .padding(20).frame(minWidth: 900, minHeight: 680)
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
        .onChange(of: engine.catalogResponses) { _, _ in consume() }
        .onChange(of: engine.catalogJobs) { _, jobs in
            guard let selected,
                  let completed = jobs.first(where: {
                      $0.kind == "timelineSample" && $0.state == "completed"
                          && $0.fileIDs.contains(selected.fileID) && !refreshedTimelineJobs.contains($0.id)
                  }) else { return }
            refreshedTimelineJobs.insert(completed.id)
            send(CatalogRequest(requestID: UUID().uuidString, action: "detail", fileID: selected.fileID))
        }
        .onDisappear { player?.pause() }
    }

    private func search() { send(CatalogRequest(requestID: UUID().uuidString, action: "search", query: query)) }
    private func control(_ job: CatalogJob, action: String) { send(CatalogRequest(requestID: UUID().uuidString, action: action, jobID: job.id)) }
    private func send(_ request: CatalogRequest) {
        pendingID = request.requestID
        pendingAction = request.action
        message = "Working…"
        if !engine.send(.catalogRequest(request: request)) { message = "The engine is unavailable." }
    }
    private func consume() {
        guard !pendingID.isEmpty, let response = engine.catalogResponses[pendingID] else { return }
        let action = pendingAction
        pendingID = ""
        pendingAction = ""
        message = response.message ?? (response.status == "ok" ? "" : "The request failed.")
        if response.status == "ok" {
            if action == "search" {
                hits = response.hits
                if hits.isEmpty { message = "No matches in indexed names, descriptions, chapters, or sampled frames." }
            }
            if action == "jobs" { engine.applyCatalogJobsSnapshot(response.jobs) }
            if ["detail", "saveChapter", "undoChapterEdit"].contains(action) {
                chapters = response.chapters
                if action == "detail" { cast = response.hits.filter { $0.kind == "person" } }
                if action != "detail" { editingID = nil; title = ""; summary = "" }
            }
            if action == "deleteChapter", let selected {
                send(CatalogRequest(requestID: UUID().uuidString, action: "detail", fileID: selected.fileID))
            }
        }
    }
    private func isVideo(_ path: String) -> Bool { ["mov","mp4","m4v","mkv","avi","mpg","mpeg","mts","m2ts","webm"].contains(URL(fileURLWithPath: path).pathExtension.lowercased()) }
}
