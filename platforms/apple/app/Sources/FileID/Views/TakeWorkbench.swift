import AppKit
import AVKit
import FileIDShared
import SwiftUI

struct TakeWorkbench: View {
    @Bindable var engine: EngineClient
    let candidates: [CatalogHit]
    var inline = false

    @Environment(\.dismiss) private var dismiss
    @State private var events: [CatalogEvent] = []
    @State private var selectedEvent: CatalogEvent?
    @State private var takes: [CatalogTake] = []
    @State private var suggestedGroups: [CatalogTakeGroupSuggestion] = []
    @State private var recommendation: CatalogTakeRecommendation?
    @State private var title = ""
    @State private var goal = ""
    @State private var eventQuery = ""
    @State private var selectedIDs: Set<Int64> = []
    @State private var selectedTakeID: Int64?
    @State private var lastDeletedEventID: String?
    @State private var player: AVPlayer?
    @State private var pendingID = ""
    @State private var pendingAction = ""
    @State private var message = ""
    @State private var goalJobID: String?
    @State private var goalEventID: String?
    @State private var goalRequestedIDs: Set<Int64> = []
    @State private var previousJobIDs: Set<String> = []

    private var goalJob: CatalogJob? {
        engine.catalogJobs.first { $0.id == goalJobID && selectedEvent?.id == goalEventID }
    }

    private func canAnalyzeGoal(_ event: CatalogEvent) -> Bool {
        let videoExtensions: Set<String> = ["mov", "mp4", "m4v", "mkv", "avi", "webm", "3gp", "mpeg", "mpg", "mts", "m2ts"]
        return pendingID.isEmpty && goal.trimmingCharacters(in: .whitespacesAndNewlines) == event.goal
            && selectedIDs == Set(event.fileIDs) && takes.count == event.fileIDs.count
            && takes.allSatisfy { videoExtensions.contains(URL(fileURLWithPath: $0.path).pathExtension.lowercased()) }
            && !["queued", "running", "paused"].contains(goalJob?.state ?? "")
    }

    private var uniqueCandidates: [CatalogHit] {
        var seen = Set<Int64>()
        return candidates.filter { seen.insert($0.fileID).inserted }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                MetaText("Group related attempts, state the goal, and let the evidence pick the best take.", color: Theme.text2)
                Spacer()
                Button("Find related takes") {
                    send("suggestTakeGroups", fileIDs: uniqueCandidates.map(\.fileID))
                }
                .disabled(uniqueCandidates.count < 2)
                Button("New group") { newGroup() }
                if let lastDeletedEventID {
                    Button("Undo delete") { send("undoEventEdit", eventID: lastDeletedEventID) }
                }
                if !inline { Button("Done") { dismiss() } }
            }
            Text("Group related clips, describe the result you want, and review each outcome. FileID only recommends a winner when the evidence supports it.")
                .font(.subheadline).foregroundStyle(.secondary)
            HSplitView {
                VStack {
                    HStack {
                        TextField("Find group", text: $eventQuery)
                            .onSubmit { send("listEvents", query: eventQuery) }
                        Button("Find") { send("listEvents", query: eventQuery) }
                    }
                    List(events, id: \.id) { event in
                        Button {
                            selectedEvent = event
                            lastDeletedEventID = nil
                            title = event.title
                            goal = event.goal
                            selectedIDs = Set(event.fileIDs)
                            load(event.id)
                        } label: {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(event.title).font(.headline)
                                Text("\(event.fileIDs.count) files · \(event.goal)").font(.caption).foregroundStyle(.secondary).lineLimit(2)
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
                .frame(minWidth: 220)

                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        TextField("Group name", text: $title)
                        TextField("Desired outcome, e.g. Alex gets a hit", text: $goal)
                        HStack {
                            Button("Save group") { saveGroup() }
                                .disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || selectedIDs.count < 2)
                            if let event = selectedEvent {
                                Button("Analyze goal") { analyzeGoal(event) }.disabled(!canAnalyzeGoal(event))
                                Button("Refresh scores") { load(event.id) }
                                Button("Undo group edit") { send("undoEventEdit", eventID: event.id) }
                                Button("Delete group") {
                                    lastDeletedEventID = event.id
                                    send("deleteEvent", eventID: event.id)
                                }
                            }
                        }
                        if let job = goalJob {
                            HStack {
                                ProgressView(value: job.progress).frame(width: 130)
                                Text("Goal analysis: " + job.state).font(.caption)
                                if ["running", "queued"].contains(job.state) {
                                    Button("Pause") { send("pauseJob", jobID: job.id) }
                                }
                                if job.state == "paused" { Button("Resume") { send("resumeJob", jobID: job.id) } }
                                if ["running", "queued", "paused"].contains(job.state) {
                                    Button("Cancel") { send("cancelJob", jobID: job.id) }
                                }
                            }
                        }
                        Text("Save the goal before analyzing a video group. Load a visual model in Deep Analyze first. Automatic scores are drafts from sampled frames; hidden outcomes and unknown identities require review.")
                            .font(.caption).foregroundStyle(.secondary)
                        Text("Select at least two files from the search results to make a group. Originals are never changed.")
                            .font(.caption).foregroundStyle(.secondary)
                        if !suggestedGroups.isEmpty {
                            Text("Possible groups from visual similarity and file dates. Check the files; this does not detect the desired outcome.")
                                .font(.caption).foregroundStyle(.secondary)
                            ForEach(suggestedGroups.indices, id: \.self) { index in
                                let suggestion = suggestedGroups[index]
                                RelatedTakeSuggestionRow(suggestion: suggestion) {
                                    newGroup()
                                    selectedIDs = Set(suggestion.members.map(\.fileID))
                                    message = "Name this group and describe the outcome before saving."
                                }
                            }
                        }
                        ForEach(uniqueCandidates, id: \.fileID) { hit in
                            Toggle(isOn: Binding(
                                get: { selectedIDs.contains(hit.fileID) },
                                set: { enabled in
                                    if enabled { selectedIDs.insert(hit.fileID) }
                                    else { selectedIDs.remove(hit.fileID) }
                                }
                            )) {
                                Text(URL(fileURLWithPath: hit.path).lastPathComponent).lineLimit(1)
                            }
                        }
                        Divider()
                        if let recommendation {
                            Text("Recommendation: \(recommendation.status.capitalized)").font(.headline)
                            Text(recommendation.reason).font(.subheadline).foregroundStyle(.secondary)
                        }
                        ForEach(takes, id: \.fileID) { take in
                            VStack(alignment: .leading, spacing: 6) {
                                HStack {
                                    Button(URL(fileURLWithPath: take.path).lastPathComponent) {
                                        selectedTakeID = take.fileID
                                        player = AVPlayer(url: URL(fileURLWithPath: take.path))
                                    }
                                    .buttonStyle(.plain).font(.headline)
                                    Spacer()
                                    if recommendation?.fileIDs.contains(take.fileID) == true {
                                        Image(systemName: "star.fill").foregroundStyle(Theme.gold)
                                    }
                                    Button("Open") { NSWorkspace.shared.open(URL(fileURLWithPath: take.path)) }
                                }
                                Text(take.stale ? "Needs review after source changed" : outcomeText(take))
                                    .font(.caption).foregroundStyle(.secondary)
                                if let event = selectedEvent {
                                    HStack {
                                        Button("Goal met") { feedback(event.id, take, outcome: 1) }
                                        Button("Not met") { feedback(event.id, take, outcome: 0) }
                                        Button("Unclear") { feedback(event.id, take, outcome: nil) }
                                        Button(take.preferred ? "Unmark preferred" : "Prefer") {
                                            send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: take.fileID, outcomeScore: take.outcomeScore, preferred: !take.preferred))
                                        }
                                        Button("Undo") { send("undoTakeFeedback", fileID: take.fileID, eventID: event.id) }
                                    }.buttonStyle(.bordered)
                                }
                                if selectedTakeID == take.fileID, let player {
                                    NativeVideoPlayer(player: player).frame(height: 180)
                                }
                            }
                            .padding(10)
                            .background(.quaternary.opacity(0.25), in: Rectangle())
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(16)
                }
                .frame(minWidth: 560)
            }
            if !message.isEmpty { Text(message).font(.caption).foregroundStyle(.secondary) }
        }
        .padding(20)
        .frame(minWidth: 900, minHeight: 650)
        .tint(Theme.gold)
        .task { send("listEvents") }
        .onChange(of: engine.catalogResponses) { _, _ in consume() }
        .onChange(of: engine.catalogJobs) { _, _ in refreshGoalJob() }
    }

    private func outcomeText(_ take: CatalogTake) -> String {
        if take.preferred { return "Preferred by you" }
        guard let score = take.outcomeScore else { return take.modelVersion?.hasPrefix("take-outcome-sequence-v1/") == true ? "Insufficient sampled evidence" : "Outcome not reviewed" }
        let draft = take.modelVersion?.hasPrefix("take-outcome-sequence-v1/") == true ? "Draft · " : ""
        if score >= 0.5 { return draft + "Goal met · \(take.explanation ?? "")" }
        return draft + "Goal not met · \(take.explanation ?? "")"
    }

    private func newGroup() {
        selectedEvent = nil
        takes = []
        recommendation = nil
        title = ""
        goal = ""
        selectedIDs = []
        selectedTakeID = nil
        player = nil
    }

    private func saveGroup() {
        lastDeletedEventID = nil
        let event = CatalogEvent(id: selectedEvent?.id ?? UUID().uuidString, title: title, goal: goal,
                                 fileIDs: Array(selectedIDs).sorted())
        send("saveEvent", event: event)
    }

    private func analyzeGoal(_ event: CatalogEvent) {
        goalEventID = event.id
        goalRequestedIDs = Set(event.fileIDs)
        previousJobIDs = Set(engine.catalogJobs.map(\.id))
        send("enqueueTimeline", fileIDs: event.fileIDs, timelineMode: "moments")
    }

    private func refreshGoalJob() {
        guard let job = goalJob else { return }
        if job.state == "completed", pendingID.isEmpty, let eventID = goalEventID {
            goalJobID = nil
            load(eventID)
        } else if job.state == "failed" {
            message = job.error ?? "Goal analysis failed. Existing reviewed scores are preserved."
        }
    }

    private func feedback(_ eventID: String, _ take: CatalogTake, outcome: Double?) {
        send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: eventID, fileID: take.fileID,
                                                            outcomeScore: outcome, preferred: take.preferred))
    }

    private func load(_ eventID: String) { send("takeGroup", eventID: eventID) }

    private func send(_ action: String, query: String? = nil, fileID: Int64? = nil, fileIDs: [Int64]? = nil, event: CatalogEvent? = nil,
                      eventID: String? = nil, feedback: CatalogTakeFeedback? = nil, timelineMode: String? = nil, jobID: String? = nil) {
        let request = CatalogRequest(requestID: UUID().uuidString, action: action, query: query, fileID: fileID,
                                     jobID: jobID, fileIDs: fileIDs, timelineMode: timelineMode, event: event, eventID: eventID, takeFeedback: feedback)
        pendingID = request.requestID
        pendingAction = action
        message = "Working…"
        if !engine.send(.catalogRequest(request: request)) { message = "The engine is unavailable." }
    }

    private func consume() {
        guard !pendingID.isEmpty, let response = engine.catalogResponses[pendingID] else { return }
        let action = pendingAction
        pendingID = ""
        pendingAction = ""
        message = response.message ?? (response.status == "ok" ? "" : "The request failed.")
        guard response.status == "ok" else {
            if action == "deleteEvent" { lastDeletedEventID = nil }
            return
        }
        if action == "suggestTakeGroups" {
            suggestedGroups = response.suggestedTakeGroups ?? []
        }
        if action == "enqueueTimeline" {
            goalJobID = response.jobs.first { $0.kind == "timelineSample" && Set($0.fileIDs) == goalRequestedIDs && !previousJobIDs.contains($0.id) }?.id
            refreshGoalJob()
        }
        if let events = response.events {
            if action == "takeGroup" || action == "saveEvent" || action == "setTakeFeedback" || action == "undoTakeFeedback" {
                if let event = events.first {
                    selectedEvent = event
                    title = event.title
                    goal = event.goal
                    selectedIDs = Set(event.fileIDs)
                }
            } else {
                self.events = events
                if action == "deleteEvent" { newGroup() }
            }
        }
        if let takes = response.takes {
            self.takes = takes
            recommendation = response.recommendation
        }
        if action == "saveEvent" || action == "deleteEvent" || action == "undoEventEdit" {
            if action == "undoEventEdit" { lastDeletedEventID = nil }
            send("listEvents", query: eventQuery)
        }
    }
}

private struct RelatedTakeSuggestionRow: View {
    let suggestion: CatalogTakeGroupSuggestion
    let onReview: () -> Void

    var body: some View {
        let names = suggestion.members.map { URL(fileURLWithPath: $0.path).lastPathComponent }.joined(separator: ", ")
        let similarity = String(format: "%.2f", suggestion.similarity)
        return HStack {
            VStack(alignment: .leading, spacing: 3) {
                Text(names).lineLimit(2)
                Text("Visual similarity \(similarity) · \(suggestion.reason)")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Review group", action: onReview)
        }
        .padding(8)
        .background(.quaternary.opacity(0.25), in: Rectangle())
    }
}
