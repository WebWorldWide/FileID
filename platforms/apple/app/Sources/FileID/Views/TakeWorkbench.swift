import AppKit
import AVKit
import FileIDShared
import SwiftUI

struct TakeWorkbench: View {
    @Bindable var engine: EngineClient
    let candidates: [CatalogHit]

    @Environment(\.dismiss) private var dismiss
    @State private var events: [CatalogEvent] = []
    @State private var selectedEvent: CatalogEvent?
    @State private var takes: [CatalogTake] = []
    @State private var recommendation: CatalogTakeRecommendation?
    @State private var title = ""
    @State private var goal = ""
    @State private var eventQuery = ""
    @State private var selectedIDs: Set<Int64> = []
    @State private var selectedTakeID: Int64?
    @State private var player: AVPlayer?
    @State private var pendingID = ""
    @State private var pendingAction = ""
    @State private var message = ""

    private var uniqueCandidates: [CatalogHit] {
        var seen = Set<Int64>()
        return candidates.filter { seen.insert($0.fileID).inserted }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Best Takes").font(.title2.bold())
                Spacer()
                Button("New group") { newGroup() }
                Button("Done") { dismiss() }
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
                                Button("Undo group edit") { send("undoEventEdit", eventID: event.id) }
                                Button("Delete group") { send("deleteEvent", eventID: event.id) }
                            }
                        }
                        Text("Select at least two files from the search results to make a group. Originals are never changed.")
                            .font(.caption).foregroundStyle(.secondary)
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
                            .background(.quaternary.opacity(0.25), in: RoundedRectangle(cornerRadius: 12))
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
    }

    private func outcomeText(_ take: CatalogTake) -> String {
        if take.preferred { return "Preferred by you" }
        guard let score = take.outcomeScore else { return "Outcome not reviewed" }
        if score >= 0.5 { return "Goal met · \(take.explanation ?? "")" }
        return "Goal not met · \(take.explanation ?? "")"
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
        let event = CatalogEvent(id: selectedEvent?.id ?? UUID().uuidString, title: title, goal: goal,
                                 fileIDs: Array(selectedIDs).sorted())
        send("saveEvent", event: event)
    }

    private func feedback(_ eventID: String, _ take: CatalogTake, outcome: Double?) {
        send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: eventID, fileID: take.fileID,
                                                            outcomeScore: outcome, preferred: take.preferred))
    }

    private func load(_ eventID: String) { send("takeGroup", eventID: eventID) }

    private func send(_ action: String, query: String? = nil, fileID: Int64? = nil, event: CatalogEvent? = nil,
                      eventID: String? = nil, feedback: CatalogTakeFeedback? = nil) {
        let request = CatalogRequest(requestID: UUID().uuidString, action: action, query: query, fileID: fileID,
                                     event: event, eventID: eventID, takeFeedback: feedback)
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
        guard response.status == "ok" else { return }
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
            send("listEvents", query: eventQuery)
        }
    }
}
